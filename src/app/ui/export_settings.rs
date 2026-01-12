use crate::app::types::{ConflictPolicy, SaveMode, ThemeMode};
use egui::RichText;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_export_settings_window(&mut self, ctx: &egui::Context) {
        if self.show_export_settings {
            egui::Window::new("Settings")
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label("Default Save Mode:");
                    ui.horizontal(|ui| {
                        let m = self.export_cfg.save_mode;
                        if ui
                            .selectable_label(m == SaveMode::Overwrite, "Overwrite")
                            .clicked()
                        {
                            self.export_cfg.save_mode = SaveMode::Overwrite;
                        }
                        if ui
                            .selectable_label(m == SaveMode::NewFile, "New File")
                            .clicked()
                        {
                            self.export_cfg.save_mode = SaveMode::NewFile;
                        }
                    });
                    if self.export_cfg.save_mode == SaveMode::NewFile {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label("Destination Folder:");
                            let folder = self
                                .export_cfg
                                .dest_folder
                                .as_ref()
                                .and_then(|p| p.to_str())
                                .unwrap_or("(source folder)");
                            ui.label(RichText::new(folder).monospace());
                            if ui.button("Choose...").clicked() {
                                if let Some(d) = self.pick_folder_dialog() {
                                    self.export_cfg.dest_folder = Some(d);
                                }
                            }
                            if ui.button("Clear").clicked() {
                                self.export_cfg.dest_folder = None;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Name Template:");
                            ui.text_edit_singleline(&mut self.export_cfg.name_template);
                        });
                        ui.horizontal(|ui| {
                            ui.label("On Conflict:");
                            let c = self.export_cfg.conflict;
                            if ui
                                .selectable_label(c == ConflictPolicy::Rename, "Rename")
                                .clicked()
                            {
                                self.export_cfg.conflict = ConflictPolicy::Rename;
                            }
                            if ui
                                .selectable_label(c == ConflictPolicy::Overwrite, "Overwrite")
                                .clicked()
                            {
                                self.export_cfg.conflict = ConflictPolicy::Overwrite;
                            }
                            if ui
                                .selectable_label(c == ConflictPolicy::Skip, "Skip")
                                .clicked()
                            {
                                self.export_cfg.conflict = ConflictPolicy::Skip;
                            }
                        });
                    } else {
                        ui.separator();
                        ui.checkbox(
                            &mut self.export_cfg.backup_bak,
                            ".bak backup on overwrite",
                        );
                    }
                    ui.separator();
                    ui.label("Appearance:");
                    let mut next_theme = self.theme_mode;
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(self.theme_mode == ThemeMode::Dark, "Dark")
                            .clicked()
                        {
                            next_theme = ThemeMode::Dark;
                        }
                        if ui
                            .selectable_label(self.theme_mode == ThemeMode::Light, "Light")
                            .clicked()
                        {
                            next_theme = ThemeMode::Light;
                        }
                    });
                    if next_theme != self.theme_mode {
                        self.set_theme(ctx, next_theme);
                    }
                    ui.separator();
                    ui.label("List:");
                    let mut next_skip = self.skip_dotfiles;
                    if ui
                        .checkbox(&mut next_skip, "Skip dotfiles (.*)")
                        .changed()
                    {
                        self.skip_dotfiles = next_skip;
                        self.save_prefs();
                        if let Some(root) = self.root.clone() {
                            self.start_scan_folder(root);
                        } else if self.skip_dotfiles {
                            self.items
                                .retain(|item| !Self::is_dotfile_path(&item.path));
                            self.rebuild_item_indexes();
                            self.apply_filter_from_search();
                            self.apply_sort();
                        }
                    }
                    ui.separator();
                    ui.label("List Columns:");
                    let mut next_cols = self.list_columns;
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut next_cols.file, "File");
                        ui.checkbox(&mut next_cols.folder, "Folder");
                        ui.checkbox(&mut next_cols.transcript, "Transcript");
                        if self.external_visible_columns.is_empty() {
                            ui.add_enabled(false, egui::Checkbox::new(&mut next_cols.external, "External"));
                        } else {
                            ui.checkbox(&mut next_cols.external, "External");
                        }
                        ui.checkbox(&mut next_cols.length, "Length");
                        ui.checkbox(&mut next_cols.channels, "Ch");
                        ui.checkbox(&mut next_cols.sample_rate, "SR");
                        ui.checkbox(&mut next_cols.bits, "Bits");
                        ui.checkbox(&mut next_cols.peak, "Peak");
                        ui.checkbox(&mut next_cols.lufs, "LUFS");
                        ui.checkbox(&mut next_cols.gain, "Gain");
                        ui.checkbox(&mut next_cols.wave, "Wave");
                    });
                    let external_available = !self.external_visible_columns.is_empty();
                    let any_visible = next_cols.file
                        || next_cols.folder
                        || next_cols.transcript
                        || (next_cols.external && external_available)
                        || next_cols.length
                        || next_cols.channels
                        || next_cols.sample_rate
                        || next_cols.bits
                        || next_cols.peak
                        || next_cols.lufs
                        || next_cols.gain
                        || next_cols.wave;
                    if !any_visible {
                        next_cols.file = true;
                    }
                    if next_cols != self.list_columns {
                        self.list_columns = next_cols;
                        self.ensure_sort_key_visible();
                        self.apply_sort();
                    }
                    ui.separator();
                    if ui.button("Close").clicked() {
                        self.show_export_settings = false;
                    }
                });
        }
    }
}
