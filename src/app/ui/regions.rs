use egui::RichText;

use crate::markers::RegionEntry;

impl crate::app::WavesPreviewer {
    /// Region list for the active editor tab: labeled [start, end) ranges
    /// that survive undo and destructive-edit remapping like markers do.
    pub(crate) fn ui_regions_window(&mut self, ctx: &egui::Context) {
        if !self.show_regions_window {
            return;
        }
        let mut open = true;
        let mut add_from_selection = false;
        let mut delete_at: Option<usize> = None;
        let mut select_range: Option<(usize, usize)> = None;
        let mut save_sidecar = false;
        let mut export_csv = false;
        egui::Window::new("Regions")
            .open(&mut open)
            .default_width(380.0)
            .default_height(420.0)
            .vscroll(true)
            .show(ctx, |ui| {
                let tab_idx = self
                    .active_tab
                    .filter(|_| self.is_editor_workspace_active());
                let Some(tab_idx) = tab_idx else {
                    ui.label(RichText::new("Open an editor tab to manage its regions.").weak());
                    return;
                };
                let sr = self
                    .tabs
                    .get(tab_idx)
                    .map(|t| t.buffer_sample_rate.max(1))
                    .unwrap_or(1);
                let has_selection = self
                    .tabs
                    .get(tab_idx)
                    .and_then(|t| t.selection)
                    .map(|(s, e)| e > s)
                    .unwrap_or(false);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(has_selection, egui::Button::new("Add from selection"))
                        .clicked()
                    {
                        add_from_selection = true;
                    }
                    if ui
                        .button("Save Sidecar")
                        .on_hover_text(
                            "Write <file>.regions.json next to the audio (empty list removes it)",
                        )
                        .clicked()
                    {
                        save_sidecar = true;
                    }
                    if ui.button("Export CSV...").clicked() {
                        export_csv = true;
                    }
                });
                ui.separator();
                let Some(tab) = self.tabs.get_mut(tab_idx) else {
                    return;
                };
                if tab.regions.is_empty() {
                    ui.label(RichText::new("No regions. Select a range and add one.").weak());
                    return;
                }
                egui::Grid::new("regions_grid")
                    .num_columns(4)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Label").weak());
                        ui.label(RichText::new("Start").weak());
                        ui.label(RichText::new("End").weak());
                        ui.label("");
                        ui.end_row();
                        for (i, region) in tab.regions.iter_mut().enumerate() {
                            ui.add(
                                egui::TextEdit::singleline(&mut region.label)
                                    .desired_width(120.0),
                            );
                            let fmt = |sample: usize| {
                                format!("{:.3}s", sample as f64 / sr as f64)
                            };
                            if ui
                                .link(fmt(region.start))
                                .on_hover_text("Select this region in the editor")
                                .clicked()
                            {
                                select_range = Some((region.start, region.end));
                            }
                            ui.label(fmt(region.end));
                            if ui.small_button("Delete").clicked() {
                                delete_at = Some(i);
                            }
                            ui.end_row();
                        }
                    });
            });
        if add_from_selection {
            self.editor_add_region_from_selection();
        }
        if let Some(i) = delete_at {
            self.editor_delete_region(i);
        }
        if let Some((s, e)) = select_range {
            if let Some(tab) = self.active_tab.and_then(|i| self.tabs.get_mut(i)) {
                tab.selection = Some((s, e));
                tab.selection_anchor_sample = Some(s);
            }
        }
        if save_sidecar {
            self.save_regions_sidecar_for_active_tab();
        }
        if export_csv {
            self.export_regions_csv_for_active_tab();
        }
        self.show_regions_window = open;
    }

    pub(in crate::app) fn editor_add_region_from_selection(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some((s, e)) = tab.selection.filter(|(s, e)| e > s) else {
            return false;
        };
        let undo = Self::capture_undo_state_labeled(tab, "Add Region");
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            let label = format!("R{:02}", tab.regions.len() + 1);
            tab.regions.push(RegionEntry {
                start: s,
                end: e,
                label,
            });
            tab.regions.sort_by_key(|r| (r.start, r.end));
        }
        self.push_editor_undo_state(tab_idx, undo, true);
        true
    }

    pub(in crate::app) fn editor_delete_region(&mut self, index: usize) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        if index >= tab.regions.len() {
            return false;
        }
        let undo = Self::capture_undo_state_labeled(tab, "Delete Region");
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.regions.remove(index);
        }
        self.push_editor_undo_state(tab_idx, undo, true);
        true
    }

    fn save_regions_sidecar_for_active_tab(&mut self) {
        let Some(tab) = self.active_tab.and_then(|i| self.tabs.get(i)) else {
            return;
        };
        let path = tab.path.clone();
        let sr = tab.buffer_sample_rate.max(1);
        let regions = tab.regions.clone();
        match crate::markers::write_regions(&path, sr, sr, &regions) {
            Ok(()) => self.push_toast(
                crate::app::types::ToastSeverity::Info,
                format!("Saved {} region(s) to sidecar", regions.len()),
            ),
            Err(err) => self.push_toast(
                crate::app::types::ToastSeverity::Error,
                format!("Region sidecar save failed: {err}"),
            ),
        }
    }

    fn export_regions_csv_for_active_tab(&mut self) {
        let Some(tab) = self.active_tab.and_then(|i| self.tabs.get(i)) else {
            return;
        };
        let sr = tab.buffer_sample_rate.max(1);
        let regions = tab.regions.clone();
        let default_name = tab
            .path
            .file_stem()
            .map(|s| format!("{}_regions.csv", s.to_string_lossy()))
            .unwrap_or_else(|| "regions.csv".to_string());
        let Some(dst) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            return;
        };
        let mut out = String::from("label,start_sample,end_sample,start_sec,end_sec\n");
        for r in &regions {
            out.push_str(&format!(
                "\"{}\",{},{},{:.6},{:.6}\n",
                r.label.replace('"', "\"\""),
                r.start,
                r.end,
                r.start as f64 / sr as f64,
                r.end as f64 / sr as f64,
            ));
        }
        match std::fs::write(&dst, out) {
            Ok(()) => self.push_toast(
                crate::app::types::ToastSeverity::Info,
                format!("Exported {} region(s)", regions.len()),
            ),
            Err(err) => self.push_toast(
                crate::app::types::ToastSeverity::Error,
                format!("Region CSV export failed: {err}"),
            ),
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_add_region_from_selection(&mut self) -> bool {
        self.editor_add_region_from_selection()
    }

    #[cfg(feature = "kittest")]
    pub fn test_regions(&self) -> Vec<(usize, usize, String)> {
        self.active_tab
            .and_then(|i| self.tabs.get(i))
            .map(|t| {
                t.regions
                    .iter()
                    .map(|r| (r.start, r.end, r.label.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}
