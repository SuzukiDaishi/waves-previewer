use std::path::PathBuf;

impl super::WavesPreviewer {
    pub(super) fn queue_external_load_with_current_settings(
        &mut self,
        path: PathBuf,
        target: super::external_ops::ExternalLoadTarget,
    ) {
        self.external_load_queue
            .push_back(super::ExternalLoadQueueItem {
                path,
                sheet_name: self.external_sheet_selected.clone(),
                has_header: self.external_has_header,
                header_row: self.external_header_row,
                data_row: self.external_data_row,
                target,
            });
    }

    pub(super) fn queue_external_load_with_settings(
        &mut self,
        path: PathBuf,
        sheet_name: Option<String>,
        has_header: bool,
        header_row: Option<usize>,
        data_row: Option<usize>,
        target: super::external_ops::ExternalLoadTarget,
    ) {
        self.external_load_queue
            .push_back(super::ExternalLoadQueueItem {
                path,
                sheet_name,
                has_header,
                header_row,
                data_row,
                target,
            });
    }

    pub(super) fn start_next_external_load_from_queue(&mut self) -> bool {
        let Some(next) = self.external_load_queue.pop_front() else {
            return false;
        };
        self.external_sheet_selected = next.sheet_name.clone();
        self.external_has_header = next.has_header;
        self.external_header_row = next.header_row;
        self.external_data_row = next.data_row;
        self.external_load_target = Some(next.target);
        self.external_settings_dirty = false;
        self.begin_external_load(next.path);
        true
    }

    pub(super) fn begin_external_load(&mut self, path: PathBuf) {
        if self.external_load_inflight {
            return;
        }
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
