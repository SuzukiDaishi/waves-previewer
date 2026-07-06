use std::path::Path;

use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn pending_gain_db_for_path(&self, path: &Path) -> f32 {
        self.item_for_path(path)
            .map(|i| i.pending_gain_db)
            .unwrap_or(0.0)
    }

    pub(super) fn set_pending_gain_db_for_path(&mut self, path: &Path, db: f32) {
        if let Some(item) = self.item_for_path_mut(path) {
            item.pending_gain_db = db;
        }
        self.pending_gain_count_cache = None;
    }

    pub(super) fn has_pending_gain(&self, path: &Path) -> bool {
        self.pending_gain_db_for_path(path).abs() > 0.0001
    }

    pub(super) fn pending_gain_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.pending_gain_db.abs() > 0.0001)
            .count()
    }

    /// Cached [`Self::pending_gain_count`] for per-frame consumers (topbar,
    /// list header). Recomputes at most every 250 ms; gain edits through
    /// [`Self::set_pending_gain_db_for_path`] invalidate it immediately, and
    /// other mutation paths tolerate the sub-second staleness.
    pub(super) fn pending_gain_count_throttled(&mut self) -> usize {
        const TTL: std::time::Duration = std::time::Duration::from_millis(250);
        if let Some((computed_at, count)) = self.pending_gain_count_cache {
            if computed_at.elapsed() < TTL {
                return count;
            }
        }
        let count = self.pending_gain_count();
        self.pending_gain_count_cache = Some((std::time::Instant::now(), count));
        count
    }
}
