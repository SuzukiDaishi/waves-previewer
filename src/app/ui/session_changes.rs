//! Two windows about the past of a session: what changed in the files it
//! points at since this user last opened it, and the local history of the
//! session document itself.
//!
//! Both read state the workers produced; neither touches the filesystem.

use egui::{Align2, Color32, RichText};

use crate::app::session_baseline::{format_stamp, summarize, ChangeKind};
use crate::app::types::{SessionHistoryIntent, ToastSeverity};

fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

impl crate::app::WavesPreviewer {
    /// The files this session points at that are not what they were when
    /// this user last opened it.
    pub(in crate::app) fn ui_session_changes_window(&mut self, ctx: &egui::Context) {
        if !self.show_session_changes_window {
            return;
        }
        if self.session_file_changes.is_none() {
            self.show_session_changes_window = false;
            return;
        }
        // Taken rather than cloned: a colleague re-rendering a folder can
        // put thousands of rows in here, and this runs every frame the
        // window is open. Put back before returning.
        let report = self
            .session_file_changes
            .take()
            .expect("checked just above");
        let mut open = true;
        let scroll_target = self.begin_floating_scroll_surface("session_changes_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let mut select: Option<std::path::PathBuf> = None;
        let mut dismiss = false;
        let changed_color = Color32::from_rgb(240, 190, 90);
        let added_color = Color32::from_rgb(150, 200, 150);
        let removed_color = Color32::from_rgb(230, 140, 140);
        let unreadable_color = Color32::from_rgb(170, 170, 175);
        let shown = egui::Window::new("Changed Since Last Open")
            .open(&mut open)
            .collapsible(false)
            .default_width(680.0)
            .default_height(420.0)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} — you last opened this session on {}",
                    summarize(&report.changes),
                    format_stamp(report.since)
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Dismiss")
                        .on_hover_text("Clear the warning. The files are not touched.")
                        .clicked()
                    {
                        dismiss = true;
                    }
                    ui.label(
                        RichText::new("Click a row to select it in the list.").weak(),
                    );
                });
                ui.add_space(6.0);
                ui.separator();
                // Only the visible rows are built. A `Grid` here would build
                // a widget per cell for every change, every frame.
                let row_height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
                    ui,
                    row_height,
                    report.changes.len(),
                    |ui, range| {
                        for change in &report.changes[range] {
                            ui.horizontal(|ui| {
                                let (label, color) = match change.kind {
                                    ChangeKind::Changed => (change.kind.label(), changed_color),
                                    ChangeKind::Added => (change.kind.label(), added_color),
                                    ChangeKind::Removed => (change.kind.label(), removed_color),
                                    ChangeKind::Unreadable => {
                                        (change.kind.label(), unreadable_color)
                                    }
                                };
                                ui.add_sized(
                                    [70.0, row_height],
                                    egui::Label::new(RichText::new(label).color(color)),
                                )
                                .on_hover_text(change.tracked.label());
                                ui.add_sized(
                                    [80.0, row_height],
                                    egui::Label::new(
                                        if matches!(
                                            change.kind,
                                            ChangeKind::Removed | ChangeKind::Unreadable
                                        ) {
                                            "—".to_string()
                                        } else {
                                            human_bytes(change.size)
                                        },
                                    ),
                                );
                                ui.add_sized(
                                    [120.0, row_height],
                                    egui::Label::new(format_stamp(change.detected_at)),
                                )
                                .on_hover_text("When this was detected");
                                let name = change
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| {
                                        change.path.to_string_lossy().to_string()
                                    });
                                if ui
                                    .add(
                                        egui::Label::new(name)
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text(change.path.display().to_string())
                                    .clicked()
                                {
                                    select = Some(change.path.clone());
                                }
                            });
                        }
                    },
                );
            });
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        if !open {
            self.show_session_changes_window = false;
        }
        if dismiss {
            self.show_session_changes_window = false;
        } else {
            // Nothing else writes this field while the window is drawing, so
            // putting it back cannot clobber a newer report.
            self.session_file_changes = Some(report);
        }
        if let Some(path) = select {
            self.select_list_path(&path);
        }
    }

    /// Move the list selection to a path, if the list still has it.
    fn select_list_path(&mut self, path: &std::path::Path) {
        let Some(row) = self.row_for_path(path) else {
            self.push_toast(
                ToastSeverity::Info,
                "That file is no longer in the list",
            );
            return;
        };
        self.selected = Some(row);
        self.scroll_to_selected = true;
        self.selected_multi.clear();
        self.selected_multi.insert(row);
        self.select_anchor = Some(row);
    }

    /// Versions of the session document this user has stored locally.
    pub(in crate::app) fn ui_session_history_window(&mut self, ctx: &egui::Context) {
        if !self.show_session_history_window {
            return;
        }
        let loading = self.session_history_request.is_some();
        let entries = self.session_history_entries.clone();
        let busy = self.session_save_state.is_some() || self.session_open_in_progress();
        let mut open = true;
        let scroll_target = self.begin_floating_scroll_surface("session_history_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let mut action: Option<(i64, SessionHistoryIntent)> = None;
        let mut save_as_id: Option<i64> = None;
        let shown = egui::Window::new("Session History")
            .open(&mut open)
            .collapsible(false)
            .default_width(660.0)
            .default_height(420.0)
            .vscroll(true)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Versions saved from this machine. Stored locally, so a colleague's \
                         saves are not here.",
                    )
                    .weak(),
                );
                ui.add_space(6.0);
                if loading {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label("Reading local history...");
                    });
                    return;
                }
                if entries.is_empty() {
                    ui.label("No earlier versions stored yet.");
                    ui.label(
                        RichText::new(
                            "A version is kept each time a save replaces an existing document.",
                        )
                        .weak(),
                    );
                    return;
                }
                egui::Grid::new("session_history_grid")
                    .striped(true)
                    .num_columns(5)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Revision").strong());
                        ui.label(RichText::new("Saved by").strong());
                        ui.label(RichText::new("Saved at").strong());
                        ui.label(RichText::new("Size").strong());
                        ui.label(RichText::new("").strong());
                        ui.end_row();
                        for entry in &entries {
                            ui.label(match entry.revision {
                                Some(rev) => rev.to_string(),
                                None => "—".to_string(),
                            });
                            ui.label(entry.saved_by.clone().unwrap_or_else(|| "—".to_string()));
                            ui.label(
                                entry
                                    .saved_at
                                    .clone()
                                    .unwrap_or_else(|| format_stamp(entry.captured_at)),
                            )
                            .on_hover_text(format!("Stored {}", format_stamp(entry.captured_at)));
                            ui.label(human_bytes(entry.byte_len))
                                .on_hover_text(format!("Content {}", entry.fingerprint));
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(!busy, egui::Button::new("Restore"))
                                    .on_hover_text(
                                        "Write this version over the session. The version it \
                                         replaces is kept here too.",
                                    )
                                    .clicked()
                                {
                                    action = Some((entry.id, SessionHistoryIntent::Restore));
                                }
                                if ui
                                    .button("Save As...")
                                    .on_hover_text("Write this version to another file")
                                    .clicked()
                                {
                                    save_as_id = Some(entry.id);
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        if !open {
            self.show_session_history_window = false;
        }
        // The file dialog blocks, so it runs after the window has drawn.
        if let Some(id) = save_as_id {
            if let Some(mut picked) = self.pick_project_save_dialog() {
                let needs_ext = picked
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| !s.eq_ignore_ascii_case("nwsess"))
                    .unwrap_or(true);
                if needs_ext {
                    picked.set_extension("nwsess");
                }
                action = Some((id, SessionHistoryIntent::SaveAs(picked)));
            }
        }
        if let Some((id, intent)) = action {
            if matches!(intent, SessionHistoryIntent::Restore) && self.has_unsaved_work() {
                self.push_toast(
                    ToastSeverity::Warning,
                    "Save or discard your in-memory edits before restoring an earlier version",
                );
                return;
            }
            self.request_session_history(id, intent);
        }
    }
}
