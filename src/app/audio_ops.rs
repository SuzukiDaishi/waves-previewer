use super::helpers::db_to_amp;
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn apply_effective_volume(&self) {
        // Global output volume (0..1)
        let base = db_to_amp(self.volume_db);
        self.audio.set_volume(base);
        // Per-file gain (can be >1)
        let path_opt = self
            .playing_path
            .as_ref()
            .or_else(|| self.current_active_path());
        let gain_db = if let Some(p) = path_opt {
            self.pending_gain_db_for_path(p)
        } else {
            0.0
        };
        let fg = db_to_amp(gain_db);
        self.audio.set_file_gain(fg);
    }
}
