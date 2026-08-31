//! The two prompts a shared session needs: "somebody else already saved
//! this" and "somebody else saved this while you were working".
//!
//! Both exist because the session lives on a file server with more than one
//! writer and no lock. Nothing here writes on its own -- every path out of
//! these windows is a choice the user made explicitly, because each one
//! either discards their edits or replaces a colleague's.

use egui::{Align2, Color32, RichText};

impl crate::app::WavesPreviewer {
    /// A save was refused: the document on disk is no longer the one this
    /// session was read from. The local edits are intact and unwritten.
    pub(in crate::app) fn run_frame_session_conflict_prompt(&mut self, ctx: &egui::Context) {
        let Some(conflict) = self.session_conflict.clone() else {
            return;
        };
        // A second save cannot start while one is in flight, and the
        // resolutions below all start one.
        let busy = self.session_save_state.is_some() || self.session_open_in_progress();
        let mut open = true;
        let scroll_target = self.begin_floating_scroll_surface("session_conflict_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let mut action: Option<ConflictAction> = None;
        let shown = egui::Window::new("Session changed by someone else")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.label(
                    RichText::new("Nothing was written. Your edits are still here.")
                        .color(Color32::from_rgb(240, 200, 120)),
                );
                ui.add_space(6.0);
                ui.label(format!("File: {}", conflict.path.display()));
                ui.label(format!("On disk: {}", conflict.on_disk));
                ui.label(match conflict.based_on_revision {
                    Some(rev) => format!("Yours: based on revision {rev}"),
                    None => "Yours: based on the version you opened".to_string(),
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Save As..."))
                        .on_hover_text("Keep both: write your version to a new file")
                        .clicked()
                    {
                        action = Some(ConflictAction::SaveAs);
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Overwrite"))
                        .on_hover_text(
                            "Replace their version with yours. The version on disk is kept as a .bak file.",
                        )
                        .clicked()
                    {
                        action = Some(ConflictAction::Overwrite);
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Reload (discard my changes)"))
                        .on_hover_text("Throw away your in-memory edits and open their version")
                        .clicked()
                    {
                        action = Some(ConflictAction::Reload);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(ConflictAction::Cancel);
                    }
                });
            });
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        if !open {
            action = Some(ConflictAction::Cancel);
        }
        let Some(action) = action else {
            return;
        };
        self.session_conflict = None;
        match action {
            ConflictAction::Cancel => {
                // The close this conflict interrupted is abandoned too: a
                // session that could not be written must not be torn down.
                self.close_after_session_save = false;
            }
            ConflictAction::SaveAs => {
                let Some(mut picked) = self.pick_project_save_dialog() else {
                    return;
                };
                let needs_ext = picked
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| !s.eq_ignore_ascii_case("nwsess"))
                    .unwrap_or(true);
                if needs_ext {
                    picked.set_extension("nwsess");
                }
                self.close_after_session_save = conflict.close_when_resolved;
                if let Err(err) = self.save_project_as(picked) {
                    self.close_after_session_save = false;
                    self.push_toast(
                        crate::app::types::ToastSeverity::Error,
                        format!("Session save failed: {err}"),
                    );
                }
            }
            ConflictAction::Overwrite => {
                self.close_after_session_save = conflict.close_when_resolved;
                if let Err(err) = self.save_project_as_forced(conflict.path.clone(), true) {
                    self.close_after_session_save = false;
                    self.push_toast(
                        crate::app::types::ToastSeverity::Error,
                        format!("Session save failed: {err}"),
                    );
                }
            }
            ConflictAction::Reload => {
                self.close_after_session_save = false;
                self.session_changed_on_disk = None;
                self.queue_project_open(conflict.path.clone());
            }
        }
    }

    /// Somebody else saved the open session. Offer a reload, and make the
    /// cost of taking it explicit when there are unsaved edits.
    pub(in crate::app) fn run_frame_session_reload_prompt(&mut self, ctx: &egui::Context) {
        if !self.session_reload_prompt {
            return;
        }
        let Some(changed) = self.session_changed_on_disk.clone() else {
            self.session_reload_prompt = false;
            return;
        };
        if changed.removed {
            // Nothing to reload from.
            self.session_reload_prompt = false;
            return;
        }
        let unsaved = self.has_unsaved_work();
        let mut open = true;
        let scroll_target = self.begin_floating_scroll_surface("session_reload_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let mut reload = false;
        let mut dismiss = false;
        let shown = egui::Window::new("Reload session?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.label(format!("File: {}", changed.path.display()));
                ui.label(format!("On disk: {}", changed.on_disk));
                ui.add_space(6.0);
                if unsaved {
                    ui.label(
                        RichText::new(
                            "You have unsaved edits (modified tabs, cached edits or pending gains).\n\
                             Reloading discards them.",
                        )
                        .color(Color32::from_rgb(240, 160, 160)),
                    );
                } else {
                    ui.label("Reloading replaces the open session with the version on disk.");
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let label = if unsaved {
                        "Reload (discard my changes)"
                    } else {
                        "Reload"
                    };
                    if ui.button(label).clicked() {
                        reload = true;
                    }
                    if ui.button("Keep working").clicked() {
                        dismiss = true;
                    }
                });
            });
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        if !open {
            dismiss = true;
        }
        if reload {
            self.session_reload_prompt = false;
            self.session_changed_on_disk = None;
            self.queue_project_open(changed.path.clone());
        } else if dismiss {
            // The banner stays: dismissing the dialog is not the same as
            // deciding the other person's save does not matter.
            self.session_reload_prompt = false;
        }
    }

    /// Ask to reload, from the topbar indicator or a menu entry.
    pub(in crate::app) fn request_session_reload_prompt(&mut self) {
        if self.session_changed_on_disk.is_some() {
            self.session_reload_prompt = true;
        }
    }
}

enum ConflictAction {
    SaveAs,
    Overwrite,
    Reload,
    Cancel,
}
