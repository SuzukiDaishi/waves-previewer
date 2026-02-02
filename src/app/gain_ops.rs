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
}
