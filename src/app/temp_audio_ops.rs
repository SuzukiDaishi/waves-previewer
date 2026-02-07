use std::path::{Path, PathBuf};

use super::types::MediaSource;
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn clear_clipboard_temp_files(&mut self) {
        for path in self.clipboard_temp_files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }

    pub(super) fn export_audio_to_temp_wav(
        &mut self,
        display_name: &str,
        audio: &crate::audio::AudioBuffer,
        sample_rate: u32,
    ) -> Option<PathBuf> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let dir = std::env::temp_dir().join("NeoWaves").join("clipboard");
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let safe = crate::app::helpers::sanitize_filename_component(display_name);
        let base = std::path::Path::new(&safe)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("clip");
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let filename = format!("{base}_{ts}.wav");
        let path = dir.join(filename);
        let range = (0, audio.len());
        if crate::wave::export_selection_wav(&audio.channels, sample_rate, range, &path).is_err() {
            return None;
        }
        self.clipboard_temp_files.push(path.clone());
        Some(path)
    }

    pub(super) fn edited_audio_for_path(
        &self,
        path: &Path,
    ) -> Option<std::sync::Arc<crate::audio::AudioBuffer>> {
        if let Some(tab) = self.tabs.iter().find(|t| {
            (t.dirty || t.loop_markers_dirty || t.markers_dirty) && t.path.as_path() == path
        }) {
            return Some(std::sync::Arc::new(
                crate::audio::AudioBuffer::from_channels(tab.ch_samples.clone()),
            ));
        }
        if let Some(cached) = self.edited_cache.get(path) {
            return Some(std::sync::Arc::new(
                crate::audio::AudioBuffer::from_channels(cached.ch_samples.clone()),
            ));
        }
        if let Some(item) = self.item_for_path(path) {
            if item.source == MediaSource::Virtual {
                return item.virtual_audio.clone();
            }
        }
        None
    }

    pub(super) fn decode_audio_for_virtual(
        &self,
        path: &Path,
    ) -> Option<(std::sync::Arc<crate::audio::AudioBuffer>, u32, u16)> {
        let (chans, in_sr) = crate::audio_io::decode_audio_multi(path).ok()?;
        let bits = crate::audio_io::read_audio_info(path)
            .map(|info| info.bits_per_sample)
            .unwrap_or(32);
        let audio = std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(chans));
        Some((audio, in_sr.max(1), bits))
    }
}
