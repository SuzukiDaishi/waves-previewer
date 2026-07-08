use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::types::MediaSource;
use super::WavesPreviewer;

const NEOWAVES_TEMP_DIR_NAME: &str = "NeoWaves";
const NEOWAVES_CACHE_PREFIX: &str = "nwcache_";
const TEMP_CACHE_RETENTION: Duration = Duration::from_secs(10 * 60);
const RECORDING_CACHE_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

pub(super) fn neowaves_temp_root() -> PathBuf {
    std::env::temp_dir().join(NEOWAVES_TEMP_DIR_NAME)
}

pub(super) fn neowaves_temp_cache_dir(kind: &str) -> PathBuf {
    neowaves_temp_root().join(kind)
}

pub(super) fn allocate_neowaves_temp_cache_path(kind: &str, extension: &str) -> Option<PathBuf> {
    let dir = neowaves_temp_cache_dir(kind);
    std::fs::create_dir_all(&dir).ok()?;
    let ext = extension.trim_start_matches('.').trim();
    let ext = if ext.is_empty() { "tmp" } else { ext };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    for attempt in 0..1000usize {
        let filename = format!(
            "{NEOWAVES_CACHE_PREFIX}{}_{}_{}.{}",
            std::process::id(),
            ts,
            attempt,
            ext
        );
        let path = dir.join(filename);
        if !path.exists() {
            return Some(path);
        }
    }
    None
}

pub(super) fn is_neowaves_internal_temp_path(path: &Path) -> bool {
    let temp = std::env::temp_dir();
    let neowaves_root = temp.join(NEOWAVES_TEMP_DIR_NAME);
    if path.starts_with(&neowaves_root) {
        return true;
    }
    if let Ok(rel) = path.strip_prefix(&temp) {
        let mut components = rel.components();
        let first = components
            .next()
            .and_then(|c| c.as_os_str().to_str())
            .unwrap_or_default();
        if matches!(
            first,
            "neowaves_pluginfx" | "neowaves_effect_graph_pluginfx"
        ) {
            return true;
        }
        if components.next().is_none() {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            return name.starts_with("neowaves_rec_")
                || name.starts_with("neowaves_audio_stream_test_");
        }
    }
    false
}

impl WavesPreviewer {
    pub(super) fn cleanup_neowaves_temp_cache_files(&mut self) {
        let mut keep: HashSet<PathBuf> = HashSet::new();
        keep.extend(self.clipboard_temp_files.iter().cloned());
        keep.extend(self.recording_temp_files.iter().cloned());
        keep.extend(
            self.external_drag_temp_files
                .iter()
                .map(|entry| entry.path.clone()),
        );
        cleanup_cache_dir(
            &neowaves_temp_cache_dir("clipboard"),
            &keep,
            true,
            TEMP_CACHE_RETENTION,
        );
        cleanup_cache_dir(
            &neowaves_temp_cache_dir("drag"),
            &keep,
            true,
            TEMP_CACHE_RETENTION,
        );
        cleanup_cache_dir(
            &neowaves_temp_cache_dir("recording"),
            &keep,
            false,
            RECORDING_CACHE_RETENTION,
        );
    }

    pub(super) fn clear_clipboard_temp_files(&mut self) {
        for path in self.clipboard_temp_files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Removes the temp WAV files backing recorded `(virtual)` items once their
    /// audio has been persisted as project sidecar audio (i.e. after a save).
    pub(super) fn clear_recording_temp_files(&mut self) {
        for path in self.recording_temp_files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
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

    /// Applies a pending list-level gain override and/or sample-rate override
    /// to already-decoded channels. Shared by the clipboard-copy and
    /// native-drag export paths so both apply identical overrides instead of
    /// each hand-rolling the gain/resample math.
    pub(super) fn apply_gain_and_resample(
        channels: Vec<Vec<f32>>,
        sample_rate: u32,
        gain_db: f32,
        target_sample_rate: u32,
        quality: crate::wave::ResampleQuality,
    ) -> (Vec<Vec<f32>>, u32) {
        if gain_db.abs() <= 0.0001 && target_sample_rate == sample_rate {
            return (channels, sample_rate.max(1));
        }
        let mut channels = channels;
        if gain_db.abs() > 0.0001 {
            let gain = crate::app::helpers::db_to_amp(gain_db);
            for channel in &mut channels {
                for sample in channel {
                    *sample = (*sample * gain).clamp(-1.0, 1.0);
                }
            }
        }
        if target_sample_rate != sample_rate {
            channels = crate::wave::resample_channels_quality(
                &channels,
                sample_rate,
                target_sample_rate,
                quality,
            );
        }
        (channels, target_sample_rate.max(1))
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

fn cleanup_cache_dir(
    dir: &Path,
    keep: &HashSet<PathBuf>,
    remove_unknown_immediately: bool,
    retention: Duration,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.contains(&path) {
            continue;
        }
        if path.is_dir() {
            let _ = std::fs::remove_dir(&path);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let known_cache = name.starts_with(NEOWAVES_CACHE_PREFIX);
        let expired = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .map(|age| age >= retention)
            .unwrap_or(true);
        if (!known_cache && remove_unknown_immediately) || expired {
            let _ = std::fs::remove_file(path);
        }
    }
}
