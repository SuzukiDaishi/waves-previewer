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
                let source_count = self.external_sources.len();
                if source_count > 0 {
                    let mut active = self
                        .external_active_source
                        .unwrap_or(0)
                        .min(source_count.saturating_sub(1));
                    let active_label = self
                        .external_sources
                        .get(active)
                        .map(|s| s.path.display().to_string())
                        .unwrap_or_else(|| "External Source".to_string());
                    egui::ComboBox::from_label("Source")
                        .selected_text(active_label)
                        .show_ui(ui, |ui| {
                            for (idx, src) in self.external_sources.iter().enumerate() {
                                let label = src.path.display().to_string();
                                ui.selectable_value(&mut active, idx, label);
                            }
                        });
                    if Some(active) != self.external_active_source {
                        self.external_active_source = Some(active);
                        self.sync_active_external_source();
                        self.external_settings_dirty = false;
                    }
                } else {
                    ui.label("No data source loaded.");
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.external_load_inflight, egui::Button::new("Load CSV/Excel..."))
                        .clicked()
                    {
                        if let Some(path) = self.pick_external_file_dialog() {
                            self.external_load_queue.clear();
                            self.external_load_target =
                                Some(crate::app::external_ops::ExternalLoadTarget::New);
                            self.external_sheet_selected = None;
                            self.external_sheet_names.clear();
                            self.external_settings_dirty = false;
                            self.begin_external_load(path);
                        }
                    }
                    if ui
                        .add_enabled(
                            !self.external_load_inflight && self.external_active_source.is_some(),
                            egui::Button::new("Reload"),
                        )
                        .clicked()
                    {
                        if let Some(idx) = self.external_active_source {
                            self.external_load_target =
                                Some(crate::app::external_ops::ExternalLoadTarget::Reload(idx));
                        }
                        if let Some(path) = self.external_source.clone() {
                            self.begin_external_load(path);
                        }
                    }
                    if ui
                        .add_enabled(self.external_active_source.is_some(), egui::Button::new("Remove"))
                        .clicked()
                    {
                        if let Some(idx) = self.external_active_source {
                            if idx < self.external_sources.len() {
                                self.external_sources.remove(idx);
                                if self.external_sources.is_empty() {
                                    self.external_active_source = None;
                                } else {
                                    self.external_active_source =
                                        Some(idx.min(self.external_sources.len() - 1));
                                }
                                self.sync_active_external_source();
                                self.rebuild_external_merged();
                                self.apply_external_mapping();
                                self.apply_filter_from_search();
                                self.apply_sort();
                            }
                        }
                    }
                    if ui
                        .add_enabled(!self.external_sources.is_empty(), egui::Button::new("Clear"))
                        .clicked()
                    {
                        self.clear_external_data();
                    }
                    if ui.button("Close").clicked() {
                        self.show_external_dialog = false;
                    }
                });
                if self.external_load_inflight {
                    let elapsed = self
                        .external_load_started_at
                        .map(|t| t.elapsed().as_secs_f32())
                        .unwrap_or(0.0);
                    ui.label(format!(
                        "Loading external data... rows: {}  ({:.1}s)",
                        self.external_load_rows, elapsed
                    ));
                }
                if let Some(err) = self.external_load_error.as_ref() {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }
                if !self.external_sources.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("Import Settings").strong());
                    if !self.external_sheet_names.is_empty() {
                        let mut selected = self
                            .external_sheet_selected
                            .clone()
                            .unwrap_or_else(|| self.external_sheet_names[0].clone());
                        egui::ComboBox::from_label("Sheet")
                            .selected_text(&selected)
                            .show_ui(ui, |ui| {
                                for name in &self.external_sheet_names {
                                    ui.selectable_value(&mut selected, name.clone(), name);
                                }
                            });
                        if Some(selected.clone()) != self.external_sheet_selected {
                            self.external_sheet_selected = Some(selected);
                            if let Some(idx) = self.external_active_source {
                                if let Some(src) = self.external_sources.get_mut(idx) {
                                    src.sheet_name = self.external_sheet_selected.clone();
                                }
                            }
                            self.external_settings_dirty = true;
                        }
                    }
                    let mut has_header = self.external_has_header;
                    if ui.checkbox(&mut has_header, "Header row").changed() {
                        self.external_has_header = has_header;
                        if !has_header {
                            self.external_header_row = None;
                        }
                        if let Some(idx) = self.external_active_source {
                            if let Some(src) = self.external_sources.get_mut(idx) {
                                src.has_header = self.external_has_header;
                                src.header_row = self.external_header_row;
                            }
                        }
                        self.external_settings_dirty = true;
                    }
                    ui.horizontal(|ui| {
                        ui.label("Header row (1-based, 0=auto)");
                        let mut header_row = self
                            .external_header_row
                            .map(|v| v as i32 + 1)
                            .unwrap_or(0);
                        if ui
                            .add_enabled(
                                self.external_has_header,
                                egui::DragValue::new(&mut header_row).range(0..=1_000_000),
                            )
                            .changed()
                        {
                            self.external_header_row = if header_row <= 0 {
                                None
                            } else {
                                Some((header_row - 1) as usize)
                            };
                            if let Some(idx) = self.external_active_source {
                                if let Some(src) = self.external_sources.get_mut(idx) {
                                    src.header_row = self.external_header_row;
                                }
                            }
                            self.external_settings_dirty = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Data row (1-based, 0=auto)");
                        let mut data_row = self
                            .external_data_row
                            .map(|v| v as i32 + 1)
                            .unwrap_or(0);
                        if ui
                            .add(egui::DragValue::new(&mut data_row).range(0..=1_000_000))
                            .changed()
                        {
                            self.external_data_row = if data_row <= 0 {
                                None
                            } else {
                                Some((data_row - 1) as usize)
                            };
                            if let Some(idx) = self.external_active_source {
                                if let Some(src) = self.external_sources.get_mut(idx) {
                                    src.data_row = self.external_data_row;
                                }
                            }
                            self.external_settings_dirty = true;
                        }
                    });
                    if self.external_settings_dirty {
                        ui.horizontal(|ui| {
                            ui.label("Settings changed.");
                            if ui
                                .add_enabled(!self.external_load_inflight, egui::Button::new("Reload with settings"))
                                .clicked()
                            {
                                if let Some(idx) = self.external_active_source {
                                    self.external_load_target =
                                        Some(crate::app::external_ops::ExternalLoadTarget::Reload(idx));
                                }
                                if let Some(path) = self.external_source.clone() {
                                    self.begin_external_load(path);
                                }
                            }
                        });
                    }
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
                    self.rebuild_external_merged();
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
                        crate::app::types::ExternalKeyRule::Regex => "Regex",
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
                            "Regex",
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
                        ui.label("Input");
                        let mut input = self.external_match_input;
                        egui::ComboBox::from_id_salt("external_regex_input")
                            .selected_text(match input {
                                crate::app::types::ExternalRegexInput::FileName => "File Name",
                                crate::app::types::ExternalRegexInput::Stem => "File Stem",
                                crate::app::types::ExternalRegexInput::Path => "Full Path",
                                crate::app::types::ExternalRegexInput::Dir => "Directory",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut input,
                                    crate::app::types::ExternalRegexInput::FileName,
                                    "File Name",
                                );
                                ui.selectable_value(
                                    &mut input,
                                    crate::app::types::ExternalRegexInput::Stem,
                                    "File Stem",
                                );
                                ui.selectable_value(
                                    &mut input,
                                    crate::app::types::ExternalRegexInput::Path,
                                    "Full Path",
                                );
                                ui.selectable_value(
                                    &mut input,
                                    crate::app::types::ExternalRegexInput::Dir,
                                    "Directory",
                                );
                            });
                        if input != self.external_match_input {
                            self.external_match_input = input;
                            regex_changed = true;
                        }
                    });
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
                ui.label(RichText::new("Scope (optional)").strong());
                let mut scope_changed = false;
                ui.horizontal(|ui| {
                    ui.label("Path regex");
                    if ui
                        .text_edit_singleline(&mut self.external_scope_regex)
                        .changed()
                    {
                        scope_changed = true;
                    }
                });
                if scope_changed {
                    self.apply_external_mapping();
                    self.apply_filter_from_search();
                    self.apply_sort();
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
                let mut show_unmatched = self.external_show_unmatched;
                if ui.checkbox(&mut show_unmatched, "Show unmatched rows in list").changed() {
                    self.external_show_unmatched = show_unmatched;
                    self.refresh_external_unmatched_items();
                }
                ui.label(format!(
                    "Matched: {}  Unmatched: {}",
                    self.external_match_count, self.external_unmatched_count
                ));
            });
    }
}
