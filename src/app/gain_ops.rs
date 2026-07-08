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

    /// Index of an open, fully loaded editor tab for `path`, if any — the
    /// condition under which list gain changes route through the editor's
    /// destructive edit pipeline instead of the pending list gain.
    fn gain_target_tab_idx(&self, path: &Path) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.path == path && !tab.loading && tab.samples_len > 0)
    }

    /// Apply a per-file gain DELTA through the unified edit framework:
    /// with an open editor tab the delta is baked into the tab's buffer via
    /// the editor's destructive pipeline (waveform, dirty flag, and undo all
    /// reflect it — a list volume change IS an editor edit); without a tab
    /// it adjusts the pending list gain as before. Returns true when the
    /// change was routed to an editor tab.
    pub(super) fn apply_file_gain_delta_unified(&mut self, path: &Path, delta_db: f32) -> bool {
        if delta_db.abs() < 1e-4 || !delta_db.is_finite() {
            return false;
        }
        if let Some(tab_idx) = self.gain_target_tab_idx(path) {
            let len = self
                .tabs
                .get(tab_idx)
                .map(|tab| tab.samples_len)
                .unwrap_or(0);
            if len > 0 {
                self.editor_apply_gain_range(tab_idx, (0, len), delta_db.clamp(-24.0, 24.0));
                self.schedule_lufs_for_path(path.to_path_buf());
                return true;
            }
        }
        let current = self.pending_gain_db_for_path(path);
        self.set_pending_gain_db_for_path(path, Self::clamp_gain_db(current + delta_db));
        false
    }

    /// Fold any pending list gain into an editor tab's buffer as a regular
    /// destructive edit (with undo) the moment the tab has real audio.
    /// Keeps the invariant "open tab => pending gain is zero", so playback,
    /// export and save never apply the gain twice, and the editor's
    /// waveform/undo history owns the change from here on.
    pub(super) fn editor_bake_pending_gain_into_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.loading || tab.samples_len == 0 {
            return;
        }
        let path = tab.path.clone();
        let gain_db = self.pending_gain_db_for_path(&path);
        if gain_db.abs() <= 1e-4 {
            return;
        }
        let undo_state = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let undo_state = Self::capture_undo_state(tab);
            let gain = crate::app::helpers::db_to_amp(gain_db);
            for ch in tab.ch_samples.iter_mut() {
                for v in ch.iter_mut() {
                    *v = (*v * gain).clamp(-1.0, 1.0);
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            undo_state
        };
        self.set_pending_gain_db_for_path(&path, 0.0);
        // Keep playback running: the buffer swap below preserves the
        // position, and the gain layer previously applied at playback time
        // is now part of the samples themselves.
        self.editor_finish_destructive_apply(tab_idx, undo_state, false);
        self.apply_effective_volume();
        self.schedule_lufs_for_path(path);
    }
}
