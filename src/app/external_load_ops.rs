use super::external_ops;
use super::external;

impl super::WavesPreviewer {
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
                                    self.external_load_error = Some(err);
                                } else {
                                    self.external_load_error = None;
                                    if let Some(next_path) = self.external_load_queue.pop_front() {
                                        self.external_load_target =
                                            Some(external_ops::ExternalLoadTarget::New);
                                        self.begin_external_load(next_path);
                                    }
                                }
                            } else {
                                self.external_load_error =
                                    Some("External load path missing.".to_string());
                            }
                        }
                        Err(err) => {
                            self.external_load_error = Some(err);
                        }
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
