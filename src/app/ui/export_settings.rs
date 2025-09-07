use egui::RichText;
use crate::app::types::{SaveMode, ConflictPolicy};

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_export_settings_window(&mut self, ctx: &egui::Context) {
        if self.show_export_settings {
            egui::Window::new("Export Settings").resizable(true).show(ctx, |ui| {
                ui.label("Default Save Mode:");
                ui.horizontal(|ui| {
                    let m = self.export_cfg.save_mode;
                    if ui.selectable_label(m == SaveMode::Overwrite, "Overwrite").clicked() { self.export_cfg.save_mode = SaveMode::Overwrite; }
                    if ui.selectable_label(m == SaveMode::NewFile, "New File").clicked() { self.export_cfg.save_mode = SaveMode::NewFile; }
                });
                if self.export_cfg.save_mode == SaveMode::NewFile {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Destination Folder:");
                        let folder = self.export_cfg.dest_folder.as_ref().and_then(|p| p.to_str()).unwrap_or("(source folder)");
                        ui.label(RichText::new(folder).monospace());
                        if ui.button("Choose...").clicked() {
                            if let Some(d) = rfd::FileDialog::new().pick_folder() { self.export_cfg.dest_folder = Some(d); }
                        }
                        if ui.button("Clear").clicked() { self.export_cfg.dest_folder = None; }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Name Template:");
                        ui.text_edit_singleline(&mut self.export_cfg.name_template);
                    });
                    ui.horizontal(|ui| {
                        ui.label("On Conflict:");
                        let c = self.export_cfg.conflict;
                        if ui.selectable_label(c == ConflictPolicy::Rename, "Rename").clicked() { self.export_cfg.conflict = ConflictPolicy::Rename; }
                        if ui.selectable_label(c == ConflictPolicy::Overwrite, "Overwrite").clicked() { self.export_cfg.conflict = ConflictPolicy::Overwrite; }
                        if ui.selectable_label(c == ConflictPolicy::Skip, "Skip").clicked() { self.export_cfg.conflict = ConflictPolicy::Skip; }
                    });
                } else {
                    ui.separator();
                    ui.checkbox(&mut self.export_cfg.backup_bak, ".wav.bak backup on overwrite");
                }
                ui.separator();
                if ui.button("Close").clicked() { self.show_export_settings = false; }
            });
        }
    }
}
