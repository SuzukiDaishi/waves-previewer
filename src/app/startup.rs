use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn apply_startup_paths(&mut self) {
        let cfg = self.startup.cfg.clone();
        if let Some(count) = cfg.dummy_list_count {
            self.populate_dummy_list(count);
            self.startup.open_first_pending = false;
            return;
        }
        if let Some(project) = cfg.open_project {
            self.queue_project_open(project);
            self.startup.open_first_pending = false;
            return;
        }
        if !cfg.open_files.is_empty() {
            self.replace_with_files(&cfg.open_files);
            self.after_add_refresh();
            return;
        }
        if let Some(dir) = cfg.open_folder {
            self.root = Some(dir);
            self.rescan();
        }
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
    }
}
