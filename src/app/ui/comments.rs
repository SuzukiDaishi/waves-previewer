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

use crate::app::comments::{self, CommentAnchor, CommentNode, CommentRef};
use crate::app::ui::comment_markdown::{self, Block, Span};
use crate::app::project::ProjectComment;

/// How much of the conversation the window is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommentFilter {
    #[default]
    All,
    /// Only threads that reference whatever the user is looking at, so the
    /// window can be left open beside the editor and follow along.
    ThisFile,
    Unresolved,
    Mine,
}

impl CommentFilter {
    pub fn label(self) -> &'static str {
        match self {
            CommentFilter::All => "All",
            CommentFilter::ThisFile => "This file",
            CommentFilter::Unresolved => "Unresolved",
            CommentFilter::Mine => "Mine",
        }
    }
}

/// A list row held inside the app rather than handed to the shell.
///
/// A plain drag from the list is already the OS file drag that puts a wav
/// into a DAW, so this rides on Alt instead of taking that away.
pub struct CommentRefDrag(pub std::path::PathBuf);

/// Something a button asked for, applied once the tree walk is over.
enum CommentAction {
    Reply(String),
    Jump(CommentRef),
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
            self.comments_window_rect = None;
            return;
        }
        if self.comments_detached {
            self.comments_window_rect = None;
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
        self.comments_window_rect = shown.as_ref().map(|shown| shown.response.rect);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        if !open {
            // Closing clears the highlights: next time it opens, "new" means
            // new since now.
            self.comment_unread_shown.clear();
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
        // Showing them is what reading them means.
        self.mark_comments_read();
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
                CommentFilter::ThisFile,
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
            CommentFilter::ThisFile => match self.current_active_path() {
                Some(path) => format!(
                    "Nothing said about {} yet.",
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                ),
                None => "Select a file to see what was said about it.".to_string(),
            },
            CommentFilter::Unresolved => "Every thread is resolved.".to_string(),
            CommentFilter::Mine => "You have not commented here yet.".to_string(),
            CommentFilter::All => "No comments yet.".to_string(),
        }
    }

    /// A thread is shown when the thread as a whole matches: a reply that
    /// mentions the search term keeps its root visible, because reading a
    /// reply without what it answers is not reading it.
    pub(in crate::app) fn comment_thread_matches_filter(&self, node: &CommentNode) -> bool {
        match self.comment_filter {
            CommentFilter::Unresolved if node.comment.resolved_at.is_some() => return false,
            CommentFilter::Mine if !self.comment_subtree_has_mine(node) => return false,
            CommentFilter::ThisFile => {
                let Some(path) = self.current_active_path().cloned() else {
                    return false;
                };
                if !self.comment_subtree_mentions(node, &path) {
                    return false;
                }
            }
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

    fn comment_subtree_mentions(&self, node: &CommentNode, path: &std::path::Path) -> bool {
        self.comment_mentions_path(&node.comment, path)
            || node
                .replies
                .iter()
                .any(|reply| self.comment_subtree_mentions(reply, path))
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
                if let Some(reference) = self.ui_comment_body(ui, &node.comment.body) {
                    actions.push(CommentAction::Jump(reference));
                }
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
            if self.comment_is_unread(&node.comment) {
                ui.label(RichText::new("●").small().color(Color32::from_rgb(140, 190, 240)))
                    .on_hover_text("New since you last looked");
            }
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
            "Write a comment for the team...  (@ to point at a file)"
        };
        // The picker's keys have to be taken before the text field is drawn,
        // or the caret moves instead of the highlight. Whether it is open is
        // therefore last frame's answer, which is a frame nobody can see.
        let mut accept_mention = false;
        if self.comment_mention_open {
            ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    self.comment_mention_index = self.comment_mention_index.saturating_add(1);
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    self.comment_mention_index = self.comment_mention_index.saturating_sub(1);
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || input.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                {
                    accept_mention = true;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    self.comment_mention_open = false;
                }
            });
        }
        let composer_id = egui::Id::new("comment_composer");
        let mut output = None;
        // Alt-dragging a row from the list drops a reference in here.
        let (_, dropped) = ui.dnd_drop_zone::<CommentRefDrag, _>(
            egui::Frame::new().inner_margin(2.0),
            |ui| {
                output = Some(
                    egui::TextEdit::multiline(&mut self.comment_draft)
                        .id(composer_id)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text(hint)
                        .show(ui),
                );
            },
        );
        if let Some(payload) = dropped {
            let reference = self.comment_ref_for_path(&payload.0, None);
            self.insert_comment_reference(&reference);
        }
        if let Some(output) = output {
            self.ui_comment_mention_picker(ui, &output, composer_id, accept_mention);
        }
        ui.horizontal(|ui| {
            self.ui_comment_reference_menu(ui);
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

    /// The `@` file picker under the composer's caret.
    ///
    /// Only opens on an `@` that starts a word, so an address somebody pastes
    /// stays an address, and never on `@[`, which is a reference already.
    fn ui_comment_mention_picker(
        &mut self,
        ui: &mut egui::Ui,
        output: &egui::text_edit::TextEditOutput,
        composer_id: egui::Id,
        accept: bool,
    ) {
        let caret = output
            .cursor_range
            .as_ref()
            .map(|range| range.primary.index)
            .filter(|_| output.response.has_focus());
        let Some((span, query)) = caret.and_then(|caret| mention_query(&self.comment_draft, caret))
        else {
            self.comment_mention_open = false;
            self.comment_mention_index = 0;
            return;
        };

        let matches = self.comment_mention_matches(&query);
        if matches.is_empty() {
            self.comment_mention_open = false;
            return;
        }
        self.comment_mention_open = true;
        self.comment_mention_index = self.comment_mention_index.min(matches.len() - 1);

        let mut chosen: Option<usize> = accept.then_some(self.comment_mention_index);
        egui::Area::new(ui.make_persistent_id("comment_mention_picker"))
            .order(egui::Order::Foreground)
            .fixed_pos(output.response.rect.left_bottom() + egui::vec2(0.0, 2.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(output.response.rect.width().max(240.0));
                    for (index, row) in matches.iter().enumerate() {
                        let selected = index == self.comment_mention_index;
                        if ui
                            .selectable_label(selected, &self.items[*row].display_name)
                            .clicked()
                        {
                            chosen = Some(index);
                        }
                    }
                    ui.label(
                        RichText::new("↑↓ to choose · Enter to insert · Esc to dismiss")
                            .weak()
                            .small(),
                    );
                });
            });

        let Some(index) = chosen.and_then(|index| matches.get(index).copied()) else {
            return;
        };
        let path = self.items[index].path.clone();
        let token = comments::format_ref(&self.comment_ref_for_path(&path, None));
        self.comment_draft
            .replace_range(span.clone(), &format!("{token} "));
        // Put the caret after what was just inserted, so typing continues
        // where the reader is looking rather than back inside the token.
        let caret = self.comment_draft[..span.start + token.len() + 1]
            .chars()
            .count();
        let mut state = egui::text_edit::TextEditState::load(ui.ctx(), composer_id)
            .unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(caret),
        )));
        state.store(ui.ctx(), composer_id);
        self.comment_mention_open = false;
        self.comment_mention_index = 0;
    }

    /// Rows whose name contains the query, best-anchored first. Capped,
    /// because this list routinely holds a hundred thousand files and a
    /// popup that long is not a picker.
    fn comment_mention_matches(&self, query: &str) -> Vec<usize> {
        const MAX_SUGGESTIONS: usize = 8;
        let needle = query.to_lowercase();
        let mut starts_with = Vec::new();
        let mut contains = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            let name = item.display_name.to_lowercase();
            if needle.is_empty() || name.starts_with(&needle) {
                starts_with.push(index);
            } else if name.contains(&needle) {
                contains.push(index);
            }
            if starts_with.len() >= MAX_SUGGESTIONS {
                break;
            }
        }
        starts_with.extend(contains);
        starts_with.truncate(MAX_SUGGESTIONS);
        starts_with
    }

    /// The four references worth one click, in the order they come up.
    ///
    /// Typing `@` reaches every file; this is for the ones a person is
    /// already looking at, where naming them again by hand is busywork.
    fn ui_comment_reference_menu(&mut self, ui: &mut egui::Ui) {
        let active = self.current_active_path().cloned();
        let mut insert: Option<CommentRef> = None;
        ui.menu_button("🔗 Reference", |ui| {
            let Some(path) = active.as_deref() else {
                ui.label(RichText::new("Select a file first.").weak());
                return;
            };
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            if ui.button(format!("File — {name}")).clicked() {
                insert = Some(self.comment_ref_for_path(path, None));
                ui.close();
            }

            let playhead = self.playback_current_source_time_sec();
            if ui
                .add_enabled(
                    playhead.is_some(),
                    egui::Button::new(match playhead {
                        Some(secs) => format!(
                            "Playhead — {}",
                            crate::app::helpers::format_time_s(secs as f32)
                        ),
                        None => "Playhead".to_string(),
                    }),
                )
                .on_disabled_hover_text("Nothing is loaded on the transport")
                .clicked()
            {
                if let Some(secs) = playhead {
                    insert = Some(self.comment_ref_for_path(
                        path,
                        Some(CommentAnchor {
                            start_sec: secs,
                            end_sec: None,
                            freq_hz: None,
                        }),
                    ));
                }
                ui.close();
            }

            let selection = self.active_tab_selection_anchor();
            if ui
                .add_enabled(
                    selection.is_some(),
                    egui::Button::new(match selection {
                        Some(anchor) => format!("Selection — {}", format_anchor(anchor)),
                        None => "Selection".to_string(),
                    }),
                )
                .on_disabled_hover_text("Select a range in the editor first")
                .clicked()
            {
                if let Some(anchor) = selection {
                    insert = Some(self.comment_ref_for_path(path, Some(anchor)));
                }
                ui.close();
            }
        })
        .response
        .on_hover_text("Point at a file, a moment, or a range. Typing @ reaches any file.");
        if let Some(reference) = insert {
            self.insert_comment_reference(&reference);
        }
    }

    /// The active editor tab's selection as a source-time anchor, carrying
    /// the spectral band when the selection was drawn on a spectrogram.
    fn active_tab_selection_anchor(&self) -> Option<CommentAnchor> {
        let tab = self.active_tab.and_then(|idx| self.tabs.get(idx))?;
        let (start, end) = tab.selection?;
        if start == end {
            return None;
        }
        let rate = tab.buffer_sample_rate.max(1) as f64;
        Some(CommentAnchor {
            start_sec: start.min(end) as f64 / rate,
            end_sec: Some(start.max(end) as f64 / rate),
            freq_hz: tab.freq_selection,
        })
    }

    /// Append a reference token, keeping exactly one space in front of it.
    pub(in crate::app) fn insert_comment_reference(&mut self, reference: &CommentRef) {
        let token = comments::format_ref(reference);
        if !self.comment_draft.is_empty() && !self.comment_draft.ends_with(char::is_whitespace) {
            self.comment_draft.push(' ');
        }
        self.comment_draft.push_str(&token);
        self.comment_draft.push(' ');
    }

    /// A comment body, drawn as its small Markdown with `@[...]` references
    /// as chips you can press. Returns the one that was pressed, if any.
    ///
    /// The chips cannot be part of the surrounding text run: a `LayoutJob`
    /// paints, it does not take clicks. So a paragraph is laid out as wrapped
    /// labels and buttons side by side rather than as one job.
    fn ui_comment_body(&mut self, ui: &mut egui::Ui, body: &str) -> Option<CommentRef> {
        let blocks = comment_markdown::parse_comment_body(body);
        let mut clicked = None;
        for block in &blocks {
            match block {
                Block::Heading { level, spans } => {
                    let size = match level {
                        1 => 17.0,
                        2 => 15.0,
                        _ => 14.0,
                    };
                    self.ui_comment_spans(ui, spans, Some(size), None, &mut clicked);
                }
                Block::Paragraph(spans) => {
                    self.ui_comment_spans(ui, spans, None, None, &mut clicked)
                }
                Block::Item { ordinal, spans } => {
                    let bullet = match ordinal {
                        Some(n) => format!("{n}."),
                        None => "•".to_string(),
                    };
                    ui.horizontal_top(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(bullet).weak());
                        self.ui_comment_spans(ui, spans, None, None, &mut clicked);
                    });
                }
                Block::Quote(spans) => {
                    ui.horizontal_top(|ui| {
                        ui.add_space(4.0);
                        ui.label(RichText::new("▏").weak());
                        let quote = ui.visuals().weak_text_color();
                        self.ui_comment_spans(ui, spans, None, Some(quote), &mut clicked);
                    });
                }
                Block::Code(text) => {
                    egui::Frame::group(ui.style())
                        .inner_margin(4.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(text).monospace());
                        });
                }
            }
        }
        clicked
    }

    fn ui_comment_spans(
        &self,
        ui: &mut egui::Ui,
        spans: &[Span],
        size: Option<f32>,
        color: Option<Color32>,
        clicked: &mut Option<CommentRef>,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for span in spans {
                match span {
                    Span::Text { text, style } => {
                        let mut rich = RichText::new(text);
                        if style.bold {
                            rich = rich.strong();
                        }
                        if style.italic {
                            rich = rich.italics();
                        }
                        if style.strike {
                            rich = rich.strikethrough();
                        }
                        if style.code {
                            rich = rich.monospace().background_color(
                                ui.visuals().extreme_bg_color,
                            );
                        }
                        if let Some(size) = size {
                            rich = rich.size(size);
                        }
                        if let Some(color) = color {
                            rich = rich.color(color);
                        }
                        ui.label(rich);
                    }
                    Span::Link(url) => {
                        ui.hyperlink(url);
                    }
                    Span::Reference(reference) => {
                        if self.ui_comment_ref_chip(ui, reference) {
                            *clicked = Some(reference.clone());
                        }
                    }
                }
            }
        });
    }

    fn ui_comment_ref_chip(&self, ui: &mut egui::Ui, reference: &CommentRef) -> bool {
        let resolved = self.resolve_comment_ref_path(reference);
        let name = resolved
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| reference.path.clone());
        let label = match reference.anchor {
            Some(anchor) => format!("🔗 {name} {}", format_anchor(anchor)),
            None => format!("🔗 {name}"),
        };
        let known = self.row_for_path(&resolved).is_some();
        let color = if known {
            ui.visuals().hyperlink_color
        } else {
            // Still pressable -- the file may be somewhere this list has not
            // been pointed at -- but not dressed up as a link that works.
            ui.visuals().weak_text_color()
        };
        let hover = if known {
            format!("Go to {}", resolved.display())
        } else {
            format!(
                "{} is not in this session's list. Opening it may fail.",
                resolved.display()
            )
        };
        ui.add(egui::Button::new(RichText::new(label).color(color)).small())
            .on_hover_text(hover)
            .clicked()
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
            CommentAction::Jump(reference) => {
                self.request_comment_ref_jump(&reference);
            }
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

    /// Turn files dropped onto the window into references in the composer.
    ///
    /// A drop anywhere else in the app loads the files into the list, which
    /// is the right answer everywhere except here, where the reader is
    /// plainly pointing at something rather than opening it.
    pub(in crate::app) fn comments_window_absorbs_drop(&mut self, ctx: &egui::Context) -> bool {
        let Some(rect) = self.comments_window_rect else {
            return false;
        };
        let over = ctx.input(|input| {
            input
                .pointer
                .interact_pos()
                .or_else(|| input.pointer.latest_pos())
        });
        if !over.is_some_and(|pos| rect.contains(pos)) {
            return false;
        }
        let paths: Vec<std::path::PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if paths.is_empty() {
            return false;
        }
        for path in paths {
            let reference = self.comment_ref_for_path(&path, None);
            self.insert_comment_reference(&reference);
        }
        true
    }

    /// Open the window, and read the document while it is coming up so a
    /// colleague's last few minutes are already there.
    pub(in crate::app) fn open_comments_window(&mut self) {
        self.show_comments_window = true;
        self.request_comment_pull();
    }
}

/// The `@word` being typed at `caret`, as a byte range over the whole token
/// and the word itself.
///
/// `caret` is a character index, which is what egui reports and not what a
/// `String` slices by.
fn mention_query(text: &str, caret: usize) -> Option<(std::ops::Range<usize>, String)> {
    let caret_byte = text
        .char_indices()
        .nth(caret)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len());
    let before = &text[..caret_byte];
    let at = before.rfind('@')?;
    // `@[` is a finished reference, not a query.
    if before[at + 1..].starts_with('[') {
        return None;
    }
    // Only an `@` that begins a word opens the picker.
    if let Some(previous) = before[..at].chars().next_back() {
        if !previous.is_whitespace() {
            return None;
        }
    }
    let word = &before[at + 1..];
    if word.chars().any(|ch| ch.is_whitespace() || ch == ']') {
        return None;
    }
    Some((at..caret_byte, word.to_string()))
}

/// A reference's position, as a reader wants to see it: a point, a span, and
/// the spectral band when the author drew one on a spectrogram.
fn format_anchor(anchor: CommentAnchor) -> String {
    let time = match anchor.normalized_range() {
        Some((start, end)) => format!(
            "{}–{}",
            crate::app::helpers::format_time_s(start as f32),
            crate::app::helpers::format_time_s(end as f32)
        ),
        None => crate::app::helpers::format_time_s(anchor.start_sec as f32),
    };
    match anchor.freq_hz {
        Some((low, high)) => format!("{time} · {low:.0}–{high:.0} Hz"),
        None => time,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> Option<(std::ops::Range<usize>, String)> {
        mention_query(text, text.chars().count())
    }

    #[test]
    fn an_at_that_starts_a_word_opens_the_picker() {
        let (span, word) = query("look at @line").expect("a query");
        assert_eq!(span, 8..13);
        assert_eq!(word, "line");
        // The bare `@` matches everything, which is the right first screen.
        assert_eq!(query("@").expect("a query").1, "");
    }

    #[test]
    fn an_address_stays_an_address() {
        assert!(query("mail me at name@example.com").is_none());
    }

    #[test]
    fn a_finished_reference_is_not_a_query() {
        assert!(query("see @[voice/line_001.wav]").is_none());
        assert!(query("see @[voice/line").is_none());
    }

    #[test]
    fn the_picker_closes_once_the_word_ends() {
        assert!(query("@line_001.wav and then").is_none());
    }

    #[test]
    fn a_caret_before_the_at_finds_nothing() {
        // The user moved back; there is no word being typed here.
        assert!(mention_query("hello @line", 3).is_none());
    }

    #[test]
    fn a_multibyte_body_slices_on_character_boundaries() {
        let text = "これを見て @line";
        let (span, word) = query(text).expect("a query");
        assert_eq!(&text[span], "@line");
        assert_eq!(word, "line");
    }

    #[test]
    fn an_anchor_reads_as_a_point_a_span_or_a_band() {
        let point = CommentAnchor {
            start_sec: 12.5,
            end_sec: None,
            freq_hz: None,
        };
        assert!(!format_anchor(point).contains('–'));
        let span = CommentAnchor {
            end_sec: Some(14.25),
            ..point
        };
        assert!(format_anchor(span).contains('–'));
        let band = CommentAnchor {
            freq_hz: Some((220.0, 880.0)),
            ..span
        };
        assert!(format_anchor(band).contains("220–880 Hz"));
    }
}
