use std::path::PathBuf;

use egui::{Color32, RichText};

use crate::app::types::WorkspaceView;
use crate::app::WavesPreviewer;

fn ensure_extension(mut path: PathBuf, ext: &str) -> PathBuf {
    let needs_ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| !s.eq_ignore_ascii_case(ext))
        .unwrap_or(true);
    if needs_ext {
        path.set_extension(ext);
    }
    path
}

impl WavesPreviewer {
    pub(super) fn ui_topbar_menu_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            self.ui_topbar_file_menu(ui);
            self.ui_topbar_edit_menu(ui);
            self.ui_topbar_export_menu(ui);
            self.ui_topbar_list_menu(ui);
            self.ui_topbar_tools_menu(ui, ctx);
            self.ui_topbar_help_menu(ui);
        });
    }

    fn ui_topbar_edit_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Edit", |ui| {
            let can_undo = self.undo_redo_available(false);
            let can_redo = self.undo_redo_available(true);
            if ui
                .add_enabled(can_undo, egui::Button::new("Undo"))
                .on_hover_text("Ctrl+Z")
                .clicked()
            {
                self.trigger_undo_redo(false);
                ui.close();
            }
            if ui
                .add_enabled(can_redo, egui::Button::new("Redo"))
                .on_hover_text("Ctrl+Y / Ctrl+Shift+Z")
                .clicked()
            {
                self.trigger_undo_redo(true);
                ui.close();
            }
            ui.separator();
            if ui
                .button("History...")
                .on_hover_text("Edit history of the active editor tab")
                .clicked()
            {
                self.show_undo_history_window = true;
                ui.close();
            }
        });
    }

    fn ui_topbar_help_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Help", |ui| {
            if ui.button("Keyboard Shortcuts...").clicked() {
                self.show_shortcuts_window = true;
                ui.close();
            }
            if ui.button("Customize Shortcuts...").clicked() {
                self.show_keymap_window = true;
                ui.close();
            }
        });
    }

    fn ui_topbar_file_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("File", |ui| {
            if ui.button("New Window (Ctrl+Shift+N)").clicked() {
                self.open_new_window();
                ui.close();
            }
            if ui.button("Session Open...").clicked() {
                if let Some(path) = self.pick_project_open_dialog() {
                    self.queue_project_open(path);
                }
                ui.close();
            }
            let recent_sessions = self.recent_session_paths_for_menu();
            if !recent_sessions.is_empty() {
                ui.menu_button("Recent Sessions", |ui| {
                    for (idx, path) in recent_sessions.iter().enumerate() {
                        let name = path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("session.nwsess");
                        let label = format!("{}  {}", idx + 1, name);
                        if ui
                            .button(label)
                            .on_hover_text(path.display().to_string())
                            .clicked()
                        {
                            self.queue_project_open(path.clone());
                            ui.close();
                        }
                    }
                });
            }
            if ui.button("Session Save (Ctrl+S)").clicked() {
                if let Err(err) = self.save_project() {
                    self.debug_log(format!("session save error: {err}"));
                    self.push_toast(
                        crate::app::types::ToastSeverity::Error,
                        format!("Session save failed: {err}"),
                    );
                }
                ui.close();
            }
            if ui.button("Session Save As...").clicked() {
                if let Some(path) = self.pick_project_save_dialog() {
                    let path = ensure_extension(path, "nwsess");
                    if let Err(err) = self.save_project_as(path) {
                        self.debug_log(format!("session save error: {err}"));
                        self.push_toast(
                            crate::app::types::ToastSeverity::Error,
                            format!("Session save-as failed: {err}"),
                        );
                    }
                }
                ui.close();
            }
            if ui.button("Session Close").clicked() {
                if let Err(err) = self.close_project_with_autosave() {
                    self.debug_log(format!("session close save error: {err}"));
                    self.push_toast(
                        crate::app::types::ToastSeverity::Error,
                        format!("Session close autosave failed: {err}"),
                    );
                }
                ui.close();
            }
            ui.separator();
            if ui.button("Folder...").clicked() {
                if let Some(dir) = self.pick_folder_dialog() {
                    self.root = Some(dir);
                    self.rescan();
                }
                ui.close();
            }
            if ui.button("Files...").clicked() {
                if let Some(files) = self.pick_files_dialog() {
                    self.start_explicit_file_load(
                        files,
                        true,
                        Some(crate::app::types::PendingListLoadTargetKind::Select),
                        true,
                    );
                }
                ui.close();
            }
        });
    }

    fn ui_topbar_export_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Export", |ui| {
            if ui.button("Apply Gains (new files)").clicked() {
                self.spawn_export_gains(false);
                ui.close();
            }
            if ui.button("Clear All Gains").clicked() {
                self.clear_all_pending_gains_with_undo();
                ui.close();
            }
            ui.separator();
            if ui.button("Export Selected (Ctrl+E)").clicked() {
                self.trigger_save_selected();
                ui.close();
            }
            ui.separator();
            if ui.button("Export List CSV...").clicked() {
                if let Some(path) = self.pick_list_csv_save_dialog() {
                    self.begin_export_list_csv(ensure_extension(path, "csv"));
                }
                ui.close();
            }
        });
    }

    fn ui_topbar_list_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("List", |ui| {
            if ui.button("Open First in Editor").clicked() {
                self.open_first_in_list();
                ui.close();
            }
            ui.separator();
            let selected = self.selected_paths();
            let real_selected = self.selected_real_paths();
            let renameable_selected = self.selected_renameable_paths();
            let has_selection = !selected.is_empty();
            let can_rename_selected = renameable_selected.len() == 1 || real_selected.len() > 1;
            if ui
                .add_enabled(
                    has_selection,
                    egui::Button::new("Copy Selected to Clipboard"),
                )
                .clicked()
            {
                self.copy_selected_to_clipboard();
                ui.close();
            }
            let can_paste = self
                .clipboard_payload
                .as_ref()
                .map(|p| !p.items.is_empty())
                .unwrap_or(false)
                || !self.get_clipboard_files().is_empty();
            if ui
                .add_enabled(can_paste, egui::Button::new("Paste"))
                .clicked()
            {
                self.paste_clipboard_to_list();
                ui.close();
            }
            if ui
                .add_enabled(can_rename_selected, egui::Button::new("Rename Selected..."))
                .clicked()
            {
                if renameable_selected.len() == 1 {
                    self.open_rename_dialog(renameable_selected[0].clone());
                } else {
                    self.open_batch_rename_dialog(real_selected.clone());
                }
                ui.close();
            }
            if ui
                .add_enabled(
                    has_selection,
                    egui::Button::new("Remove Selected from List"),
                )
                .clicked()
            {
                self.remove_paths_from_list_with_undo(&selected);
                ui.close();
            }
            let has_edits = self.has_edits_for_paths(&selected);
            if ui
                .add_enabled(has_edits, egui::Button::new("Clear Edits for Selected"))
                .clicked()
            {
                self.clear_edits_for_paths(&selected);
                ui.close();
            }
            ui.separator();
            if ui.button("Inspect Files (QA)...").clicked() {
                self.open_inspection_dialog();
                ui.close();
            }
            if ui.button("Normalize Loudness...").clicked() {
                self.open_loudnorm_dialog();
                ui.close();
            }
            if ui
                .button("Find Duplicates...")
                .on_hover_text(
                    "Scan the selection (or the whole list) for exact duplicates and perceptually similar files",
                )
                .clicked()
            {
                self.start_duplicate_scan();
                ui.close();
            }
            if ui
                .button("Export Engine Metadata...")
                .on_hover_text(
                    "Write a Wwise/FMOD/Unity metadata table (loops, rates, lengths, LUFS) for the selection or list",
                )
                .clicked()
            {
                self.show_engine_export_dialog = true;
                ui.close();
            }
            if ui
                .button("Edit BWF Metadata...")
                .on_hover_text(
                    "Write bext (Broadcast WAV) description/originator into the selected WAV files",
                )
                .clicked()
            {
                self.open_bwf_dialog();
                ui.close();
            }
            ui.separator();
            let multi = self.selected_paths().len() >= 2;
            if ui
                .add_enabled(
                    multi,
                    egui::Button::new("Audition Selection (Round-robin)"),
                )
                .on_hover_text(
                    "Play the selected files one after another in order; stop playback to end",
                )
                .clicked()
            {
                self.start_variation_audition(
                    crate::app::types::VariationAuditionMode::RoundRobin,
                );
                ui.close();
            }
            if ui
                .add_enabled(multi, egui::Button::new("Audition Selection (Random)"))
                .on_hover_text(
                    "Play the selected files in random order (never the same file twice in a row)",
                )
                .clicked()
            {
                self.start_variation_audition(
                    crate::app::types::VariationAuditionMode::Random,
                );
                ui.close();
            }
        });
    }

    fn ui_topbar_tools_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.menu_button("Tools", |ui| {
            if ui.button("Effect Graph...").clicked() {
                self.open_effect_graph_workspace();
                ui.close();
            }
            if ui.button("Recording...").clicked() {
                self.workspace_view = WorkspaceView::Recording;
                self.recording_tab.tab_open = true;
                if self.recording_tab.input_devices.is_empty() {
                    self.recording_refresh_devices();
                }
                ui.close();
            }
            if ui.button("Plugin Manager...").clicked() {
                self.show_plugin_manager = true;
                ui.close();
            }
            ui.separator();
            if ui.button("Settings...").clicked() {
                self.show_export_settings = true;
                ui.close();
            }
            ui.separator();
            self.ui_topbar_ai_menu(ui);
            ui.separator();
            self.ui_zoo_menu(ui, ctx);
            ui.separator();
            if ui.button("External Data...").clicked() {
                self.show_external_dialog = true;
                ui.close();
            }
            if ui.button("Transcript Window...").clicked() {
                self.show_transcript_window = true;
                ui.close();
            }
            if ui.button("Screenshot (F9)").clicked() {
                let path = self.default_screenshot_path();
                self.request_screenshot(ctx, path, false);
                ui.close();
            }
            if ui.button("Crash Reports...").clicked() {
                self.open_crash_report_window();
                ui.close();
            }
            ui.separator();
            if ui.button("Debug Window (F12)").clicked() {
                self.debug.cfg.enabled = true;
                self.debug.show_window = !self.debug.show_window;
                ui.close();
            }
            if ui.button("Run Checks").clicked() {
                self.debug.cfg.enabled = true;
                self.debug_check_invariants();
                ui.close();
            }
        });
    }

    fn ui_topbar_ai_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("AI", |ui| {
            if self.transcript_ai_has_model() {
                ui.label(
                    RichText::new("Transcript model: ready")
                        .color(Color32::from_rgb(120, 220, 140)),
                );
            } else {
                ui.label(RichText::new("Transcript model: not installed").weak());
            }
            if ui.button("Transcription...").clicked() {
                self.show_transcription_settings = true;
                ui.close();
            }
            if self.transcript_ai_has_model() {
                if ui
                    .add_enabled(
                        self.transcript_ai_can_uninstall(),
                        egui::Button::new("Uninstall Transcript Model..."),
                    )
                    .clicked()
                {
                    self.uninstall_transcript_model_cache();
                    ui.close();
                }
            } else if ui.button("Download Transcript Model...").clicked() {
                self.queue_transcript_model_download();
                ui.close();
            }
            ui.separator();
            if self.music_ai_has_model() {
                ui.label(
                    RichText::new("Music Analyze model: ready")
                        .color(Color32::from_rgb(120, 220, 140)),
                );
                if ui
                    .add_enabled(
                        self.music_ai_can_uninstall(),
                        egui::Button::new("Uninstall Music Analyze Model..."),
                    )
                    .clicked()
                {
                    self.uninstall_music_model_cache();
                    ui.close();
                }
            } else if ui.button("Download Music Analyze Model...").clicked() {
                self.queue_music_model_download();
                ui.close();
            }
        });
    }
}
