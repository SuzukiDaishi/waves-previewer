use std::path::PathBuf;

use super::WavesPreviewer;

pub struct ProjectOpenState {
    pub started_at: std::time::Instant,
    pub shown: bool,
}

impl WavesPreviewer {
    pub(super) fn queue_project_open(&mut self, path: PathBuf) {
        self.project_open_pending = Some(path);
        self.project_open_state = Some(ProjectOpenState {
            started_at: std::time::Instant::now(),
            shown: false,
        });
    }

    pub(super) fn tick_project_open(&mut self) {
        let Some(state) = self.project_open_state.as_mut() else {
            return;
        };
        if !state.shown {
            state.shown = true;
            return;
        }
        let Some(path) = self.project_open_pending.take() else {
            self.project_open_state = None;
            return;
        };
        if let Err(err) = self.open_project_file(path) {
            self.debug_log(format!("project open error: {err}"));
        }
        self.project_open_state = None;
    }
}
