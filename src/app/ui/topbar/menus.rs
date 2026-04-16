use std::path::PathBuf;

use egui::{Color32, RichText};

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
            self.ui_topbar_export_menu(ui);
            self.ui_topbar_list_menu(ui);
            self.ui_topbar_tools_menu(ui, ctx);
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
            if ui.button("Session Save (Ctrl+S)").clicked() {
                if let Err(err) = self.save_project() {
                    self.debug_log(format!("session save error: {err}"));
                }
                ui.close();
            }
            if ui.button("Session Save As...").clicked() {
                if let Some(path) = self.pick_project_save_dialog() {
                    let path = ensure_extension(path, "nwsess");
                    if let Err(err) = self.save_project_as(path) {
                        self.debug_log(format!("session save error: {err}"));
                    }
                }
                ui.close();
            }
            if ui.button("Session Close").clicked() {
                self.close_project();
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
                    self.replace_with_files(&files);
                    self.after_add_refresh();
                    self.select_open_target_path(&files, true);
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
        });
    }

    fn ui_topbar_tools_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.menu_button("Tools", |ui| {
            if ui.button("Effect Graph...").clicked() {
                self.open_effect_graph_workspace();
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
