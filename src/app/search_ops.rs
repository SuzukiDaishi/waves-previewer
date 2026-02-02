use std::time::{Duration, Instant};

use super::types::SortDir;
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn schedule_search_refresh(&mut self) {
        self.search_dirty = true;
        self.search_deadline = Some(Instant::now() + Duration::from_millis(300));
    }

    pub(super) fn apply_search_if_due(&mut self) {
        let Some(deadline) = self.search_deadline else {
            return;
        };
        if !self.search_dirty {
            return;
        }
        if Instant::now() >= deadline {
            self.apply_filter_from_search();
            if self.sort_dir != SortDir::None {
                self.apply_sort();
            }
            self.search_dirty = false;
            self.search_deadline = None;
        }
    }
}
