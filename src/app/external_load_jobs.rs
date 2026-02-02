use std::path::PathBuf;

impl super::WavesPreviewer {
    pub(super) fn begin_external_load(&mut self, path: PathBuf) {
        if self.external_load_inflight {
            return;
        }
        self.external_load_error = None;
        self.external_load_rows = 0;
        self.external_load_started_at = Some(std::time::Instant::now());
        self.external_load_path = Some(path.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        self.external_load_rx = Some(rx);
        self.external_load_inflight = true;
        let cfg = super::external::ExternalLoadConfig {
            path,
            sheet_name: self.external_sheet_selected.clone(),
            has_header: self.external_has_header,
            header_row: self.external_header_row,
            data_row: self.external_data_row,
        };
        super::external::spawn_load_table(cfg, tx);
    }
}
