//! The window a shared session is discussed in.
//!
//! One body, two frames around it. Docked it is an `egui::Window` like every
//! other panel here; detached it is a real second OS window through
//! `show_viewport_immediate`, the way `ui/video_viewport.rs` already does it.
//! That second form is the one this feature is for: reviewing somebody's
//! notes means reading them *while* scrubbing the audio they are about, and a
//! floating window over the editor makes you choose.
//!
//! Rendering never mutates the conversation directly. The tree is built from
//! a snapshot and each button records a [`CommentAction`], applied after the
//! walk is finished -- otherwise every reply button would need a mutable
//! borrow of the list it is being drawn from.

use egui::{Align, Color32, RichText};

use crate::app::comments::{self, CommentNode};
use crate::app::project::ProjectComment;

/// How much of the conversation the window is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommentFilter {
    #[default]
    All,
    Unresolved,
    Mine,
}

impl CommentFilter {
    pub fn label(self) -> &'static str {
        match self {
            CommentFilter::All => "All",
            CommentFilter::Unresolved => "Unresolved",
            CommentFilter::Mine => "Mine",
        }
    }
}

/// Something a button asked for, applied once the tree walk is over.
enum CommentAction {
    Reply(String),
    StartEdit(String, String),
    SubmitEdit(String),
    CancelEdit,
    Delete(String),
    SetResolved(String, bool),
    ToggleCollapsed(String),
}

/// How deep replies keep stepping right before they stop. Past this the
/// indent costs more width than the nesting is worth reading.
const MAX_INDENT_DEPTH: usize = 5;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_comments_window(&mut self, ctx: &egui::Context) {
        if !self.show_comments_window {
            return;
        }
        if self.comments_detached {
            self.ui_comments_viewport(ctx);
            return;
        }
        let mut open = true;
        let scroll_target = self.begin_floating_scroll_surface("comments_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let shown = egui::Window::new("Comments")
            .open(&mut open)
            .default_width(420.0)
            .default_height(520.0)
            .min_width(320.0)
            .resizable(true)
            .show(ctx, |ui| self.ui_comments_body(ui));
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        self.show_comments_window = open;
    }

    /// The detached form: an actual OS window, so the conversation can sit on
    /// a second monitor while the editor keeps the first.
    fn ui_comments_viewport(&mut self, ctx: &egui::Context) {
        let viewport_id = egui::ViewportId::from_hash_of("comments_viewport");
        let title = match self.project_path.as_ref().and_then(|p| p.file_stem()) {
            Some(name) => format!("NeoWaves Comments — {}", name.to_string_lossy()),
            None => "NeoWaves Comments".to_string(),
        };
        let builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([440.0, 640.0])
            .with_min_inner_size([320.0, 320.0])
            .with_resizable(true);
        let mut close_requested = false;
        ctx.show_viewport_immediate(viewport_id, builder, |ui, _class| {
            if ui.ctx().input(|input| input.viewport().close_requested()) {
                close_requested = true;
                return;
            }
            egui::Frame::new()
                .inner_margin(8.0)
                .fill(ui.visuals().panel_fill)
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    self.ui_comments_body(ui);
                });
        });
        if close_requested {
            // Closing the detached window puts the panel back where it came
            // from rather than losing it: the user closed a window, not a
            // feature.
            self.comments_detached = false;
        }
    }

    fn ui_comments_body(&mut self, ui: &mut egui::Ui) {
        let mut actions: Vec<CommentAction> = Vec::new();
        self.ui_comments_header(ui);
        ui.separator();

        let threads = comments::build_threads(&self.comments);
        let visible: Vec<&CommentNode> = threads
            .iter()
            .filter(|node| self.comment_thread_matches_filter(node))
            .collect();

        let composer_height = 116.0;
        let list_height = (ui.available_height() - composer_height).max(96.0);
        egui::ScrollArea::vertical()
            .id_salt("comments_threads")
            .max_height(list_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if visible.is_empty() {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(self.comments_empty_hint()).weak());
                    });
                    return;
                }
                for node in visible {
                    self.ui_comment_node(ui, node, 0, &mut actions);
                    ui.add_space(6.0);
                }
            });

        ui.separator();
        self.ui_comment_composer(ui, &mut actions);

        for action in actions {
            self.apply_comment_action(action);
        }
    }

    fn ui_comments_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for filter in [
                CommentFilter::All,
                CommentFilter::Unresolved,
                CommentFilter::Mine,
            ] {
                if ui
                    .selectable_label(self.comment_filter == filter, filter.label())
                    .clicked()
                {
                    self.comment_filter = filter;
                }
            }
            ui.separator();
            if ui
                .button("⟳")
                .on_hover_text(
                    "Read the session on disk again. Comments also arrive on their own, \
                     but a shared drive is only polled every few seconds.",
                )
                .clicked()
            {
                self.request_comment_pull();
            }
            let detach_label = if self.comments_detached { "⧉ Dock" } else { "⧉" };
            if ui
                .button(detach_label)
                .on_hover_text(if self.comments_detached {
                    "Put the conversation back in this window"
                } else {
                    "Open the conversation in its own window, so it can sit beside the editor"
                })
                .clicked()
            {
                self.comments_detached = !self.comments_detached;
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.comment_search)
                    .desired_width(f32::INFINITY)
                    .hint_text("Search comments"),
            );
        });
        let pending = self.comments_pending();
        if pending > 0 {
            ui.label(
                RichText::new(format!(
                    "{pending} comment(s) not shared yet — retrying"
                ))
                .small()
                .color(Color32::from_rgb(240, 190, 90)),
            );
        }
    }

    /// What to say when the list is empty, which is different every time and
    /// worth being specific about.
    fn comments_empty_hint(&self) -> String {
        if self.project_path.is_none() {
            return "Save this session first — comments live in the .nwsess, \
                    so there is nowhere to share them yet."
                .to_string();
        }
        if self.comments.is_empty() {
            return "No comments yet.".to_string();
        }
        if !self.comment_search.trim().is_empty() {
            return "No comment matches that search.".to_string();
        }
        match self.comment_filter {
            CommentFilter::Unresolved => "Every thread is resolved.".to_string(),
            CommentFilter::Mine => "You have not commented here yet.".to_string(),
            CommentFilter::All => "No comments yet.".to_string(),
        }
    }

    /// A thread is shown when the thread as a whole matches: a reply that
    /// mentions the search term keeps its root visible, because reading a
    /// reply without what it answers is not reading it.
    fn comment_thread_matches_filter(&self, node: &CommentNode) -> bool {
        match self.comment_filter {
            CommentFilter::Unresolved if node.comment.resolved_at.is_some() => return false,
            CommentFilter::Mine if !self.comment_subtree_has_mine(node) => return false,
            _ => {}
        }
        let needle = self.comment_search.trim().to_lowercase();
        needle.is_empty() || self.comment_subtree_matches_text(node, &needle)
    }

    fn comment_subtree_has_mine(&self, node: &CommentNode) -> bool {
        self.comment_is_mine(&node.comment)
            || node
                .replies
                .iter()
                .any(|reply| self.comment_subtree_has_mine(reply))
    }

    fn comment_subtree_matches_text(&self, node: &CommentNode, needle: &str) -> bool {
        node.comment.body.to_lowercase().contains(needle)
            || node
                .comment
                .author_name
                .as_deref()
                .unwrap_or(&node.comment.author_id)
                .to_lowercase()
                .contains(needle)
            || node
                .replies
                .iter()
                .any(|reply| self.comment_subtree_matches_text(reply, needle))
    }

    fn ui_comment_node(
        &mut self,
        ui: &mut egui::Ui,
        node: &CommentNode,
        depth: usize,
        actions: &mut Vec<CommentAction>,
    ) {
        let id = node.comment.id.clone();
        let collapsed = self.comment_collapsed.contains(&id);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.ui_comment_header_row(ui, node, depth, collapsed, actions);
            if node.comment.deleted {
                ui.label(RichText::new("(withdrawn)").weak().italics());
            } else if self.comment_editing_id.as_deref() == Some(id.as_str()) {
                ui.add(
                    egui::TextEdit::multiline(&mut self.comment_edit_draft)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.comment_edit_draft.trim().is_empty(),
                            egui::Button::new("Save"),
                        )
                        .clicked()
                    {
                        actions.push(CommentAction::SubmitEdit(id.clone()));
                    }
                    if ui.button("Cancel").clicked() {
                        actions.push(CommentAction::CancelEdit);
                    }
                });
            } else {
                ui.label(&node.comment.body);
                self.ui_comment_action_row(ui, node, actions);
            }
        });
        if collapsed || node.replies.is_empty() {
            return;
        }
        // Past the cap the replies stop stepping right. A deep thread in a
        // narrow window would otherwise end up a column one word wide.
        if depth < MAX_INDENT_DEPTH {
            ui.indent(("comment_replies", id.as_str()), |ui| {
                for reply in &node.replies {
                    self.ui_comment_node(ui, reply, depth + 1, actions);
                }
            });
        } else {
            for reply in &node.replies {
                self.ui_comment_node(ui, reply, depth + 1, actions);
            }
        }
    }

    fn ui_comment_header_row(
        &mut self,
        ui: &mut egui::Ui,
        node: &CommentNode,
        depth: usize,
        collapsed: bool,
        actions: &mut Vec<CommentAction>,
    ) {
        ui.horizontal(|ui| {
            let replies = node.len() - 1;
            if depth == 0 && replies > 0 {
                let arrow = if collapsed { "▶" } else { "▼" };
                if ui
                    .add(egui::Button::new(format!("{arrow} {replies}")).frame(false))
                    .on_hover_text(if collapsed {
                        "Show the replies"
                    } else {
                        "Hide the replies"
                    })
                    .clicked()
                {
                    actions.push(CommentAction::ToggleCollapsed(node.comment.id.clone()));
                }
            }
            ui.label(
                RichText::new(self.comment_author_label(&node.comment))
                    .strong()
                    .small(),
            );
            ui.label(
                RichText::new(format_stamp(&node.comment))
                    .weak()
                    .small(),
            );
            if self.comment_is_unsent(&node.comment.id) {
                ui.label(
                    RichText::new("· not shared yet")
                        .small()
                        .color(Color32::from_rgb(240, 190, 90)),
                )
                .on_hover_text("Still on its way to the session file. It will be retried.");
            }
            if node.comment.resolved_at.is_some() {
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new("resolved")
                            .small()
                            .color(Color32::from_rgb(150, 200, 150)),
                    );
                });
            }
        });
    }

    fn ui_comment_action_row(
        &mut self,
        ui: &mut egui::Ui,
        node: &CommentNode,
        actions: &mut Vec<CommentAction>,
    ) {
        let id = &node.comment.id;
        ui.horizontal(|ui| {
            if ui.small_button("Reply").clicked() {
                actions.push(CommentAction::Reply(id.clone()));
            }
            // Only the author may rewrite or withdraw what they wrote. Anyone
            // may settle a thread, because a thread belongs to the team.
            if self.comment_is_mine(&node.comment) {
                if ui.small_button("Edit").clicked() {
                    actions.push(CommentAction::StartEdit(
                        id.clone(),
                        node.comment.body.clone(),
                    ));
                }
                if ui.small_button("Delete").clicked() {
                    actions.push(CommentAction::Delete(id.clone()));
                }
            }
            if node.comment.parent.is_none() {
                let resolved = node.comment.resolved_at.is_some();
                let label = if resolved { "Reopen" } else { "Resolve" };
                if ui.small_button(label).clicked() {
                    actions.push(CommentAction::SetResolved(id.clone(), !resolved));
                }
            }
        });
    }

    fn ui_comment_composer(&mut self, ui: &mut egui::Ui, actions: &mut Vec<CommentAction>) {
        let replying_to = self.comment_reply_to.clone();
        if let Some(target) = replying_to.as_deref() {
            ui.horizontal(|ui| {
                let who = self
                    .comment_by_id(target)
                    .map(|comment| self.comment_author_label(comment))
                    .unwrap_or_else(|| "a comment".to_string());
                ui.label(RichText::new(format!("Replying to {who}")).small().weak());
                if ui.small_button("✕").on_hover_text("Post as a new thread instead").clicked() {
                    actions.push(CommentAction::CancelEdit);
                }
            });
        }
        let hint = if replying_to.is_some() {
            "Write a reply..."
        } else {
            "Write a comment for the team..."
        };
        ui.add(
            egui::TextEdit::multiline(&mut self.comment_draft)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text(hint),
        );
        ui.horizontal(|ui| {
            let can_post = !self.comment_draft.trim().is_empty();
            if ui
                .add_enabled(can_post, egui::Button::new("Post"))
                .on_disabled_hover_text("Write something first")
                .clicked()
            {
                let body = std::mem::take(&mut self.comment_draft);
                self.post_comment(replying_to, &body);
                self.comment_reply_to = None;
            }
            if self.project_path.is_none() {
                ui.label(
                    RichText::new("Not shared until the session is saved")
                        .small()
                        .weak(),
                );
            }
        });
    }

    /// `name`, with `@host` appended only when somebody else in this
    /// conversation posts under the same account name. Two people called
    /// `user` on two machines is a real shape on a shared drive, and it is
    /// the only time the machine name earns its space.
    fn comment_author_label(&self, comment: &ProjectComment) -> String {
        let label = comment
            .author_name
            .as_deref()
            .unwrap_or(&comment.author_id)
            .to_string();
        let ambiguous = self.comments.iter().any(|other| {
            other.author_id == comment.author_id && other.author_host != comment.author_host
        });
        match (ambiguous, comment.author_host.as_deref()) {
            (true, Some(host)) => format!("{label}@{host}"),
            _ => label,
        }
    }

    fn apply_comment_action(&mut self, action: CommentAction) {
        match action {
            CommentAction::Reply(id) => {
                self.comment_reply_to = Some(id);
                self.comment_editing_id = None;
                self.comment_edit_draft.clear();
            }
            CommentAction::StartEdit(id, body) => {
                self.comment_editing_id = Some(id);
                self.comment_edit_draft = body;
            }
            CommentAction::SubmitEdit(id) => {
                let body = std::mem::take(&mut self.comment_edit_draft);
                self.edit_comment(&id, &body);
                self.comment_editing_id = None;
            }
            CommentAction::CancelEdit => {
                self.comment_editing_id = None;
                self.comment_edit_draft.clear();
                self.comment_reply_to = None;
            }
            CommentAction::Delete(id) => {
                self.delete_comment(&id);
                if self.comment_editing_id.as_deref() == Some(id.as_str()) {
                    self.comment_editing_id = None;
                    self.comment_edit_draft.clear();
                }
            }
            CommentAction::SetResolved(id, resolved) => {
                self.set_thread_resolved(&id, resolved);
            }
            CommentAction::ToggleCollapsed(id) => {
                if !self.comment_collapsed.remove(&id) {
                    self.comment_collapsed.insert(id);
                }
            }
        }
    }

    /// Open the window, and read the document while it is coming up so a
    /// colleague's last few minutes are already there.
    pub(in crate::app) fn open_comments_window(&mut self) {
        self.show_comments_window = true;
        self.request_comment_pull();
    }
}

/// When it was written, in the reader's own zone. Stored UTC, because two
/// machines in different zones have to sort together; shown local, because
/// nobody reads their colleagues' notes in UTC.
fn format_stamp(comment: &ProjectComment) -> String {
    let shown = comment
        .edited_at
        .as_deref()
        .unwrap_or(comment.created_at.as_str());
    let text = chrono::DateTime::parse_from_rfc3339(shown)
        .map(|stamp| {
            stamp
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| shown.to_string());
    if comment.edited_at.is_some() && !comment.deleted {
        format!("{text} (edited)")
    } else {
        text
    }
}
