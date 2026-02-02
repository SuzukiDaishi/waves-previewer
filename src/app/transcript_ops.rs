use std::path::Path;

use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn request_transcript_seek(&mut self, path: &Path, start_ms: u64) {
        self.pending_transcript_seek = Some((path.to_path_buf(), start_ms));
        if self.playing_path.as_deref() == Some(path) {
            return;
        }
        if let Some(row) = self.row_for_path(path) {
            self.select_and_load(row, true);
            return;
        }
        if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
            self.active_tab = Some(idx);
            self.rebuild_current_buffer_with_mode();
        }
    }

    pub(super) fn apply_pending_transcript_seek(&mut self) {
        let Some((path, start_ms)) = self.pending_transcript_seek.clone() else {
            return;
        };
        if self.playing_path.as_ref() != Some(&path) {
            return;
        }
        let sr = self.audio.shared.out_sample_rate.max(1) as u64;
        let mut samples = ((start_ms * sr) / 1000) as usize;
        if let Some(tab) = self.tabs.iter().find(|t| t.path == path) {
            samples = self.map_display_to_audio_sample(tab, samples);
        }
        self.audio.seek_to_sample(samples);
        self.pending_transcript_seek = None;
    }
}
