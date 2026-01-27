use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn apply_startup_paths(&mut self) {
        let cfg = self.startup.cfg.clone();
        if let Some(count) = cfg.dummy_list_count {
            self.populate_dummy_list(count);
            self.startup.open_first_pending = false;
            self.apply_startup_external(&cfg);
            return;
        }
        if let Some(ref project) = cfg.open_project {
            self.queue_project_open(project.clone());
            self.startup.open_first_pending = false;
            self.apply_startup_external(&cfg);
            return;
        }
        if !cfg.open_files.is_empty() {
            self.replace_with_files(&cfg.open_files);
            self.after_add_refresh();
            self.apply_startup_external(&cfg);
            return;
        }
        if let Some(ref dir) = cfg.open_folder {
            self.root = Some(dir.clone());
            self.rescan();
        }
        self.apply_startup_external(&cfg);
    }

    fn apply_startup_external(&mut self, cfg: &super::StartupConfig) {
        let mut source_path = cfg.external_path.clone();
        if let Some(rows) = cfg.external_dummy_rows {
            let path = cfg
                .external_dummy_path
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from("debug").join("external_dummy.csv"));
            let has_header = cfg.external_has_header.unwrap_or(true);
            let cols = cfg.external_dummy_cols.max(1);
            if let Err(err) = write_external_dummy_csv(&path, rows, cols, has_header) {
                self.external_load_error = Some(err);
            } else {
                source_path = Some(path);
            }
        }
        let Some(path) = source_path else {
            return;
        };
        if let Some(rule) = cfg.external_key_rule {
            self.external_key_rule = rule;
        }
        if let Some(input) = cfg.external_key_input {
            self.external_match_input = input;
        }
        if let Some(regex) = cfg.external_key_regex.clone() {
            self.external_match_regex = regex;
        }
        if let Some(replace) = cfg.external_key_replace.clone() {
            self.external_match_replace = replace;
        }
        if let Some(scope) = cfg.external_scope_regex.clone() {
            self.external_scope_regex = scope;
        }
        if let Some(has_header) = cfg.external_has_header {
            self.external_has_header = has_header;
            if !has_header {
                self.external_header_row = None;
            }
        }
        if let Some(header_row) = cfg.external_header_row {
            self.external_header_row = Some(header_row);
        }
        if let Some(data_row) = cfg.external_data_row {
            self.external_data_row = Some(data_row);
        }
        if let Some(sheet) = cfg.external_sheet.clone() {
            self.external_sheet_selected = Some(sheet);
        }
        if cfg.external_show_unmatched {
            self.external_show_unmatched = true;
        }
        if cfg.external_show_dialog {
            self.show_external_dialog = true;
        }
        self.external_settings_dirty = false;
        self.begin_external_load(path);
    }

    pub(super) fn open_first_in_list(&mut self) {
        let Some(id) = self.files.first().copied() else {
            return;
        };
        let Some(item) = self.item_for_id(id) else {
            return;
        };
        let path = item.path.clone();
        self.selected = Some(0);
        self.selected_multi.clear();
        self.selected_multi.insert(0);
        self.select_anchor = Some(0);
        self.open_or_activate_tab(&path);
    }

    pub(super) fn run_startup_actions(&mut self, ctx: &egui::Context) {
        if self.startup.open_first_pending && !self.files.is_empty() {
            self.open_first_in_list();
            self.startup.open_first_pending = false;
        }

        if self.startup.screenshot_pending {
            let wait_for_tab = self.startup.cfg.open_first;
            let ready = if wait_for_tab {
                self.active_tab.is_some()
            } else {
                true
            };
            if ready {
                if !self.startup.view_mode_applied {
                    if let Some(mode) = self.startup.cfg.open_view_mode {
                        if let Some(idx) = self.active_tab {
                            if let Some(tab) = self.tabs.get_mut(idx) {
                                tab.view_mode = mode;
                            }
                            self.startup.view_mode_applied = true;
                        }
                    }
                }
                if !self.startup.waveform_overlay_applied {
                    if let Some(flag) = self.startup.cfg.open_waveform_overlay {
                        if let Some(idx) = self.active_tab {
                            if let Some(tab) = self.tabs.get_mut(idx) {
                                tab.show_waveform_overlay = flag;
                            }
                            self.startup.waveform_overlay_applied = true;
                        }
                    }
                }
                if self.startup.screenshot_frames_left > 0 {
                    self.startup.screenshot_frames_left =
                        self.startup.screenshot_frames_left.saturating_sub(1);
                } else if let Some(path) = self.startup.cfg.screenshot_path.clone() {
                    self.request_screenshot(ctx, path, self.startup.cfg.exit_after_screenshot);
                    self.startup.screenshot_pending = false;
                }
            }
        }
        if self.startup.debug_summary_pending {
            if self.external_load_inflight {
                return;
            }
            if self.startup.debug_summary_frames_left > 0 {
                self.startup.debug_summary_frames_left =
                    self.startup.debug_summary_frames_left.saturating_sub(1);
            } else if let Some(path) = self.startup.cfg.debug_summary_path.clone() {
                self.save_debug_summary(path);
                self.startup.debug_summary_pending = false;
            }
        }
    }
}

fn write_external_dummy_csv(
    path: &std::path::Path,
    rows: usize,
    cols: usize,
    has_header: bool,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir failed: {e}"))?;
    }
    let mut out = String::new();
    if has_header {
        let mut headers = Vec::with_capacity(cols);
        headers.push("Key".to_string());
        for i in 1..cols {
            headers.push(format!("Col{}", i + 1));
        }
        out.push_str(&headers.join(","));
        out.push('\n');
    }
    for i in 0..rows {
        let mut row = Vec::with_capacity(cols);
        row.push(format!("dummy_{:05}.wav", i + 1));
        for c in 1..cols {
            row.push(format!("Value{}_{}", c + 1, i + 1));
        }
        out.push_str(&row.join(","));
        out.push('\n');
    }
    std::fs::write(path, out).map_err(|e| format!("write dummy csv failed: {e}"))?;
    Ok(())
}
