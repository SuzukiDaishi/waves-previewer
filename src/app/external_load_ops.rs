use super::external;

impl super::WavesPreviewer {
    pub(super) fn append_external_load_error(&mut self, message: String) {
        let next = match self.external_load_error.take() {
            Some(prev) if !prev.trim().is_empty() => format!("{prev}\n{message}"),
            _ => message,
        };
        self.external_load_error = Some(next);
    }

    pub(super) fn finalize_pending_external_restore(&mut self) {
        let Some(restore) = self.pending_external_restore.take() else {
            return;
        };
        if self.external_sources.is_empty() {
            self.external_show_unmatched = restore.show_unmatched;
            return;
        }
        if let Some(active) = restore.active_source {
            if active < self.external_sources.len() {
                self.external_active_source = Some(active);
            }
        }
        self.external_show_unmatched = restore.show_unmatched;
        self.sync_active_external_source();
        self.rebuild_external_merged_with_preferences(
            None,
            restore.key_column,
            restore.visible_columns,
        );
        // A small/headless restore installs synchronously; a large GUI
        // restore is completed by drain_external_merge_results.
        if self.external_merge_rx.is_none() {
            self.apply_external_mapping();
            self.refresh_filter_then_sort();
        }
    }

    pub(super) fn drain_external_load_results(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.external_load_rx.take() else {
            return;
        };
        let mut done = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                external::ExternalLoadMsg::Progress { rows } => {
                    self.external_load_rows = rows;
                    ctx.request_repaint();
                }
                external::ExternalLoadMsg::Done(res) => {
                    done = true;
                    self.external_load_inflight = false;
                    self.external_load_started_at = None;
                    let path = self.external_load_path.take();
                    match res {
                        Ok(table) => {
                            if let Some(p) = path {
                                if let Err(err) = self.apply_external_table(p, table) {
                                    self.append_external_load_error(err);
                                }
                            } else {
                                self.append_external_load_error(
                                    "External load path missing.".to_string(),
                                );
                            }
                        }
                        Err(err) => {
                            self.append_external_load_error(err);
                        }
                    }
                    if !self.start_next_external_load_from_queue() {
                        self.finalize_pending_external_restore();
                    }
                    ctx.request_repaint();
                }
            }
        }
        if !done {
            self.external_load_rx = Some(rx);
        }
    }
}
