use std::collections::HashSet;

use egui::RichText;

fn compact_external_source_label(value: &str, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars {
        return value.to_owned();
    }
    let tail_chars = max_chars.saturating_sub(1);
    let tail: String = value
        .chars()
        .skip(char_count.saturating_sub(tail_chars))
        .collect();
    format!("…{tail}")
}

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_external_data_window(&mut self, ctx: &egui::Context) {
        if !self.show_external_dialog {
            return;
        }
        let viewport = ctx.content_rect();
        let window_max = egui::vec2(
            (viewport.width() - 32.0).max(1.0),
            (viewport.height() - 32.0).max(1.0),
        );
        let window_min = egui::vec2(480.0_f32.min(window_max.x), 320.0_f32.min(window_max.y));
        let window_default = egui::vec2(720.0_f32.min(window_max.x), 680.0_f32.min(window_max.y));
        let content_max_width = (window_max.x - 24.0).max(1.0);
        let content_max_height = (window_max.y - 44.0).max(120.0);
        let mut open = self.show_external_dialog;
        egui::Window::new("External Data")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(window_default)
            .min_size(window_min)
            .max_size(window_max)
            .constrain_to(viewport)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_max_width(content_max_width);
                egui::ScrollArea::vertical()
                    .id_salt("external_data_window_scroll")
                    .auto_shrink([false, false])
                    .max_width(content_max_width)
                    .max_height(content_max_height)
                    .show(ui, |ui| {
                        let source_count = self.external_sources.len();
                        if source_count > 0 {
                            let mut active = self
                                .external_active_source
                                .unwrap_or(0)
                                .min(source_count.saturating_sub(1));
                            let active_full_label = self
                                .external_sources
                                .get(active)
                                .map(|s| s.path.display().to_string())
                                .unwrap_or_else(|| "External Source".to_string());
                            let active_label =
                                compact_external_source_label(&active_full_label, 72);
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Source");
                                let combo_width = ui.available_width().clamp(180.0, 560.0);
                                let combo = egui::ComboBox::from_id_salt(
                                    "external_data_source_selector",
                                )
                                .width(combo_width)
                                .selected_text(active_label)
                                .show_ui(ui, |ui| {
                                    for (idx, src) in self.external_sources.iter().enumerate() {
                                        let full_label = src.path.display().to_string();
                                        let label =
                                            compact_external_source_label(&full_label, 88);
                                        ui.selectable_value(&mut active, idx, label)
                                            .on_hover_text(full_label);
                                    }
                                });
                                combo.response.on_hover_text(active_full_label);
                            });
                            if Some(active) != self.external_active_source {
                                self.external_active_source = Some(active);
                                self.sync_active_external_source();
                                self.external_settings_dirty = false;
                            }
                        } else {
                            ui.label("No data source loaded.");
                        }
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled(
                                    !self.external_load_inflight,
                                    egui::Button::new("Load CSV/Excel..."),
                                )
                                .clicked()
                            {
                                if let Some(path) = self.pick_external_file_dialog() {
                                    self.external_load_queue.clear();
                                    self.pending_external_restore = None;
                                    self.external_load_error = None;
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
                                    !self.external_load_inflight
                                        && self.external_active_source.is_some(),
                                    egui::Button::new("Reload"),
                                )
                                .clicked()
                            {
                                if let Some(idx) = self.external_active_source {
                                    self.external_load_target = Some(
                                        crate::app::external_ops::ExternalLoadTarget::Reload(idx),
                                    );
                                }
                                if let Some(path) = self.external_source.clone() {
                                    self.pending_external_restore = None;
                                    self.external_load_error = None;
                                    self.begin_external_load(path);
                                }
                            }
                            if ui
                                .add_enabled(
                                    self.external_active_source.is_some(),
                                    egui::Button::new("Remove"),
                                )
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
                                        self.refresh_filter_then_sort();
                                    }
                                }
                            }
                            if ui
                                .add_enabled(
                                    !self.external_sources.is_empty(),
                                    egui::Button::new("Clear"),
                                )
                                .clicked()
                            {
                                self.clear_external_data();
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
                                let mut selected =
                                    self.external_sheet_selected.clone().unwrap_or_else(|| {
                                        self.external_sheet_names[0].clone()
                                    });
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
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Header row (1-based, 0=auto)");
                                let mut header_row =
                                    self.external_header_row.map(|v| v as i32 + 1).unwrap_or(0);
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
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Data row (1-based, 0=auto)");
                                let mut data_row =
                                    self.external_data_row.map(|v| v as i32 + 1).unwrap_or(0);
                                if ui
                                    .add(
                                        egui::DragValue::new(&mut data_row).range(0..=1_000_000),
                                    )
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
                                ui.horizontal_wrapped(|ui| {
                                    ui.label("Settings changed.");
                                    if ui
                                        .add_enabled(
                                            !self.external_load_inflight,
                                            egui::Button::new("Reload with settings"),
                                        )
                                        .clicked()
                                    {
                                        if let Some(idx) = self.external_active_source {
                                            self.external_load_target = Some(
                                                crate::app::external_ops::ExternalLoadTarget::Reload(
                                                    idx,
                                                ),
                                            );
                                        }
                                        if let Some(path) = self.external_source.clone() {
                                            self.pending_external_restore = None;
                                            self.external_load_error = None;
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
                            self.refresh_filter_then_sort();
                            let key_name = self.external_headers[key_idx].clone();
                            self.external_visible_columns.retain(|c| c != &key_name);
                            if self.external_visible_columns.is_empty() {
                                self.external_visible_columns = Self::default_external_columns(
                                    &self.external_headers,
                                    key_idx,
                                );
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
                            self.refresh_filter_then_sort();
                        }
                        if self.external_key_rule == crate::app::types::ExternalKeyRule::Regex {
                            ui.separator();
                            let mut regex_changed = false;
                            ui.label(RichText::new("Match Rule").strong());
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Input");
                                let mut input = self.external_match_input;
                                egui::ComboBox::from_id_salt("external_regex_input")
                                    .selected_text(match input {
                                        crate::app::types::ExternalRegexInput::FileName => {
                                            "File Name"
                                        }
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
                            ui.horizontal_wrapped(|ui| {
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
                                self.refresh_filter_then_sort();
                            }
                        }
                        ui.separator();
                        ui.label(RichText::new("Scope (optional)").strong());
                        let mut scope_changed = false;
                        ui.horizontal_wrapped(|ui| {
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
                            self.refresh_filter_then_sort();
                        }
                        ui.separator();
                        let key_name = self.external_headers.get(key_idx).cloned();
                        let available_columns = self
                            .external_headers
                            .iter()
                            .filter(|name| Some(*name) != key_name.as_ref())
                            .count();
                        let mut select_all = false;
                        let mut select_none = false;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("Visible Columns").strong());
                            ui.weak(format!(
                                "{} / {} selected",
                                self.external_visible_columns.len().min(available_columns),
                                available_columns
                            ));
                            if ui.small_button("All").clicked() {
                                select_all = true;
                            }
                            if ui.small_button("None").clicked() {
                                select_none = true;
                            }
                        });
                        let current_visible: HashSet<&str> = self
                            .external_visible_columns
                            .iter()
                            .map(String::as_str)
                            .collect();
                        let mut changed_columns = Vec::new();
                        let row_height = ui.spacing().interact_size.y;
                        let column_list_height =
                            (viewport.height() * 0.30).clamp(120.0, 320.0);
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            egui::ScrollArea::vertical()
                                .id_salt("external_visible_columns_scroll")
                                .auto_shrink([false, false])
                                .max_height(column_list_height)
                                .show_rows(
                                    ui,
                                    row_height,
                                    self.external_headers.len(),
                                    |ui, row_range| {
                                        for idx in row_range {
                                            let Some(name) = self.external_headers.get(idx) else {
                                                continue;
                                            };
                                            if Some(name) == key_name.as_ref() {
                                                ui.allocate_space(egui::vec2(0.0, row_height));
                                                continue;
                                            }
                                            let mut enabled =
                                                current_visible.contains(name.as_str());
                                            if ui.checkbox(&mut enabled, name).changed() {
                                                changed_columns.push((name.clone(), enabled));
                                            }
                                        }
                                    },
                                );
                        });

                        if select_all || select_none || !changed_columns.is_empty() {
                            let mut selected: HashSet<String> = if select_none {
                                HashSet::new()
                            } else if select_all {
                                self.external_headers
                                    .iter()
                                    .filter(|name| Some(*name) != key_name.as_ref())
                                    .cloned()
                                    .collect()
                            } else {
                                self.external_visible_columns.iter().cloned().collect()
                            };
                            for (name, enabled) in changed_columns {
                                if enabled {
                                    selected.insert(name);
                                } else {
                                    selected.remove(&name);
                                }
                            }
                            let next_visible: Vec<String> = self
                                .external_headers
                                .iter()
                                .filter(|name| {
                                    Some(*name) != key_name.as_ref() && selected.contains(*name)
                                })
                                .cloned()
                                .collect();
                            if next_visible != self.external_visible_columns {
                                self.external_visible_columns = next_visible;
                                if let crate::app::types::SortKey::External(idx) = self.sort_key {
                                    if idx >= self.external_visible_columns.len() {
                                        self.sort_key = crate::app::types::SortKey::File;
                                        self.sort_dir = crate::app::types::SortDir::None;
                                    }
                                }
                                self.request_sort();
                            }
                        }
                        ui.separator();
                        let mut show_unmatched = self.external_show_unmatched;
                        if ui
                            .checkbox(&mut show_unmatched, "Show unmatched rows in list")
                            .changed()
                        {
                            self.external_show_unmatched = show_unmatched;
                            self.refresh_external_unmatched_items();
                        }
                        ui.label(format!(
                            "Matched: {}  Unmatched: {}",
                            self.external_match_count, self.external_unmatched_count
                        ));
                    });
            });
        self.show_external_dialog = open;
    }
}

#[cfg(test)]
mod tests {
    use super::compact_external_source_label;

    #[test]
    fn external_source_label_compacts_long_unicode_paths_without_breaking_chars() {
        let short = r"C:\Audio\表.xlsx";
        assert_eq!(compact_external_source_label(short, 32), short);

        let long = r"C:\Users\テストユーザー\非常に長いフォルダー名\さらに長いフォルダー名\音声管理表.xlsx";
        let compact = compact_external_source_label(long, 24);
        assert!(compact.starts_with('…'));
        assert!(compact.ends_with("音声管理表.xlsx"));
        assert_eq!(compact.chars().count(), 24);
    }
}
