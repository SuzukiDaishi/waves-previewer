use egui::RichText;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_external_data_window(&mut self, ctx: &egui::Context) {
        if !self.show_external_dialog {
            return;
        }
        egui::Window::new("External Data")
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if let Some(path) = self.external_source.as_ref() {
                    ui.label(path.display().to_string());
                } else {
                    ui.label("No data source loaded.");
                }
                ui.horizontal(|ui| {
                    if ui.button("Load CSV/Excel...").clicked() {
                        if let Some(path) = self.pick_external_file_dialog() {
                            match self.load_external_source(path) {
                                Ok(()) => self.external_load_error = None,
                                Err(err) => self.external_load_error = Some(err),
                            }
                        }
                    }
                    if ui
                        .add_enabled(self.external_source.is_some(), egui::Button::new("Clear"))
                        .clicked()
                    {
                        self.clear_external_data();
                    }
                    if ui.button("Close").clicked() {
                        self.show_external_dialog = false;
                    }
                });
                if let Some(err) = self.external_load_error.as_ref() {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }
                if self.external_headers.is_empty() {
                    return;
                }
                ui.separator();
                let mut key_idx = self.external_key_index.unwrap_or(0);
                let key_label = self
                    .external_headers
                    .get(key_idx)
                    .map(|s| s.as_str())
                    .unwrap_or("Key");
                egui::ComboBox::from_label("Key Column")
                    .selected_text(key_label)
                    .show_ui(ui, |ui| {
                        for (idx, name) in self.external_headers.iter().enumerate() {
                            ui.selectable_value(&mut key_idx, idx, name);
                        }
                    });
                if Some(key_idx) != self.external_key_index {
                    self.external_key_index = Some(key_idx);
                    self.rebuild_external_lookup();
                    self.apply_external_mapping();
                    self.apply_filter_from_search();
                    self.apply_sort();
                    let key_name = self.external_headers[key_idx].clone();
                    self.external_visible_columns.retain(|c| c != &key_name);
                    if self.external_visible_columns.is_empty() {
                        self.external_visible_columns =
                            Self::default_external_columns(&self.external_headers, key_idx);
                    }
                }
                ui.separator();
                let mut rule = self.external_key_rule;
                egui::ComboBox::from_label("Key Rule")
                    .selected_text(match rule {
                        crate::app::types::ExternalKeyRule::FileName => "File Name",
                        crate::app::types::ExternalKeyRule::Stem => "File Stem",
                        crate::app::types::ExternalKeyRule::Regex => "Regex (Stem)",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut rule,
                            crate::app::types::ExternalKeyRule::FileName,
                            "File Name",
                        );
                        ui.selectable_value(
                            &mut rule,
                            crate::app::types::ExternalKeyRule::Stem,
                            "File Stem",
                        );
                        ui.selectable_value(
                            &mut rule,
                            crate::app::types::ExternalKeyRule::Regex,
                            "Regex (Stem)",
                        );
                    });
                if rule != self.external_key_rule {
                    self.external_key_rule = rule;
                    self.apply_external_mapping();
                    self.apply_filter_from_search();
                    self.apply_sort();
                }
                if self.external_key_rule == crate::app::types::ExternalKeyRule::Regex {
                    ui.separator();
                    let mut regex_changed = false;
                    ui.label(RichText::new("Match Rule").strong());
                    ui.horizontal(|ui| {
                        ui.label("Regex");
                        if ui
                            .text_edit_singleline(&mut self.external_match_regex)
                            .changed()
                        {
                            regex_changed = true;
                        }
                        ui.label("Replace");
                        if ui
                            .text_edit_singleline(&mut self.external_match_replace)
                            .changed()
                        {
                            regex_changed = true;
                        }
                    });
                    if regex_changed {
                        self.apply_external_mapping();
                        self.apply_filter_from_search();
                        self.apply_sort();
                    }
                }
                ui.separator();
                ui.label(RichText::new("Visible Columns").strong());
                let mut next_visible: Vec<String> = Vec::new();
                let key_name = self.external_headers.get(key_idx).cloned();
                for name in &self.external_headers {
                    if Some(name) == key_name.as_ref() {
                        continue;
                    }
                    let mut enabled = self.external_visible_columns.contains(name);
                    if ui.checkbox(&mut enabled, name).changed() {}
                    if enabled {
                        next_visible.push(name.clone());
                    }
                }
                if next_visible != self.external_visible_columns {
                    self.external_visible_columns = next_visible;
                    if let crate::app::types::SortKey::External(idx) = self.sort_key {
                        if idx >= self.external_visible_columns.len() {
                            self.sort_key = crate::app::types::SortKey::File;
                            self.sort_dir = crate::app::types::SortDir::None;
                        }
                    }
                    self.apply_sort();
                }
                ui.separator();
                ui.label(format!(
                    "Matched: {}  Unmatched: {}",
                    self.external_match_count, self.external_unmatched_count
                ));
            });
    }
}
