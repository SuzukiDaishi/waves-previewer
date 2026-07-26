use std::path::PathBuf;

use egui::{Color32, RichText};

impl crate::app::WavesPreviewer {
    /// Standalone plugin management window (Tools > Plugin Manager...):
    /// catalog overview, rescan, scan status/errors, and search-path
    /// editing (persisted to prefs like the in-editor Search Paths box).
    pub(crate) fn ui_plugin_manager_window(&mut self, ctx: &egui::Context) {
        if !self.show_plugin_manager {
            return;
        }
        let mut open = true;
        let mut do_scan = false;
        let mut pick_folder = false;
        let mut add_path: Option<PathBuf> = None;
        let mut remove_index: Option<usize> = None;
        let mut reset_paths = false;
        egui::Window::new("Plugin Manager")
            .open(&mut open)
            .default_width(600.0)
            .default_height(520.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.plugin_scan_state.is_none(),
                            egui::Button::new("Rescan"),
                        )
                        .clicked()
                    {
                        do_scan = true;
                    }
                    if self.plugin_scan_state.is_some() {
                        ui.add(egui::Spinner::new());
                        ui.label(RichText::new("Scanning plugins...").weak());
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "{} plugin(s) in catalog",
                                self.plugin_catalog.len()
                            ))
                            .weak(),
                        );
                    }
                });
                if let Some(err) = self.plugin_scan_error.as_ref() {
                    ui.label(
                        RichText::new(format!("Scan failed: {err}"))
                            .color(Color32::LIGHT_RED),
                    );
                }
                ui.separator();
                ui.label(RichText::new("Search Paths").strong());
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Add Folder...").clicked() {
                        pick_folder = true;
                    }
                    if ui.button("Reset Defaults").clicked() {
                        reset_paths = true;
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.plugin_search_path_input)
                            .hint_text("Add path manually")
                            .desired_width(380.0),
                    );
                    let submit = (edit.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                        || ui.button("Add Path").clicked();
                    if submit {
                        let raw = self.plugin_search_path_input.trim();
                        if !raw.is_empty() {
                            add_path = Some(PathBuf::from(raw));
                            self.plugin_search_path_input.clear();
                        }
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("plugin_manager_paths")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        if self.plugin_search_paths.is_empty() {
                            ui.label(RichText::new("(No search paths)").weak());
                        } else {
                            for (idx, path) in self.plugin_search_paths.iter().enumerate() {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        RichText::new(path.display().to_string())
                                            .small()
                                            .monospace(),
                                    );
                                    if ui.small_button("Remove").clicked() {
                                        remove_index = Some(idx);
                                    }
                                });
                            }
                        }
                    });
                ui.separator();
                ui.label(RichText::new("Catalog").strong());
                egui::ScrollArea::vertical()
                    .id_salt("plugin_manager_catalog")
                    .show(ui, |ui| {
                        if self.plugin_catalog.is_empty() {
                            ui.label(
                                RichText::new(
                                    "No plugins scanned yet — press Rescan to sweep the search paths",
                                )
                                .weak(),
                            );
                        } else {
                            egui::Grid::new("plugin_manager_catalog_grid")
                                .num_columns(3)
                                .striped(true)
                                .min_col_width(90.0)
                                .show(ui, |ui| {
                                    ui.label(RichText::new("Name").strong().small());
                                    ui.label(RichText::new("Format").strong().small());
                                    ui.label(RichText::new("Path").strong().small());
                                    ui.end_row();
                                    for entry in self.plugin_catalog.iter() {
                                        ui.label(RichText::new(&entry.name).small());
                                        ui.label(
                                            RichText::new(format!("{:?}", entry.format)).small(),
                                        );
                                        ui.label(
                                            RichText::new(entry.path.display().to_string())
                                                .small()
                                                .monospace(),
                                        )
                                        .on_hover_text(entry.path.display().to_string());
                                        ui.end_row();
                                    }
                                });
                        }
                    });
            });
        if pick_folder {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                add_path = Some(folder);
            }
        }
        // Same follow-up as the in-editor Search Paths box: persist and
        // rescan so the catalog always reflects the path set.
        let mut paths_changed = false;
        if let Some(path) = add_path {
            paths_changed |= self.add_plugin_search_path(path);
        }
        if let Some(idx) = remove_index {
            paths_changed |= self.remove_plugin_search_path_at(idx);
        }
        if reset_paths {
            self.reset_plugin_search_paths_to_default();
            paths_changed = true;
        }
        if paths_changed {
            self.save_prefs();
            self.plugin_catalog.clear();
            do_scan = true;
        }
        if do_scan {
            self.spawn_plugin_scan();
        }
        self.show_plugin_manager = open;
    }
}
