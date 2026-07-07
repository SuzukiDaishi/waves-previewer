use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::types::{MediaId, MediaSource};
use super::{ExternalDragTempFile, PendingExternalDrag, WavesPreviewer};

const DRAG_TEMP_RETENTION: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug)]
pub(super) struct PreparedExternalDrag {
    pub(super) paths: Vec<PathBuf>,
    pub(super) temp_paths: Vec<PathBuf>,
}

// Constructed only by the Windows drag backend; other platforms just match.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NativeDragOutcome {
    Dropped,
    Cancel,
    Started,
}

impl WavesPreviewer {
    pub(super) fn queue_external_drag_for_row(&mut self, row_idx: usize) -> bool {
        if row_idx >= self.files.len() {
            return false;
        }
        let item_ids = if self.selected_multi.len() > 1 && self.selected_multi.contains(&row_idx) {
            self.selected_item_ids()
        } else {
            let Some(id) = self.files.get(row_idx).copied() else {
                return false;
            };
            self.selected = Some(row_idx);
            self.scroll_to_selected = false;
            self.selected_multi.clear();
            self.selected_multi.insert(row_idx);
            self.select_anchor = Some(row_idx);
            vec![id]
        };
        if item_ids.is_empty() {
            return false;
        }
        self.pending_external_drag = Some(PendingExternalDrag { item_ids });
        true
    }

    pub(super) fn flush_pending_external_drag(&mut self, frame: &mut eframe::Frame) {
        self.cleanup_neowaves_temp_cache_files();
        self.cleanup_external_drag_temp_files();
        let Some(pending) = self.pending_external_drag.take() else {
            return;
        };
        let prepared = match self.prepare_external_drag_paths_for_ids(&pending.item_ids) {
            Ok(prepared) => prepared,
            Err(err) => {
                self.set_external_drag_status(format!("Drag failed: {err}"));
                return;
            }
        };
        if prepared.paths.is_empty() {
            self.set_external_drag_status("Drag failed: no files prepared");
            return;
        }
        let paths = match canonicalize_drag_payload_paths(&prepared.paths) {
            Ok(paths) => paths,
            Err(err) => {
                self.set_external_drag_status(format!("Drag failed: {err}"));
                return;
            }
        };
        let now = Instant::now();
        for path in prepared.temp_paths {
            self.external_drag_temp_files
                .push_back(ExternalDragTempFile {
                    path,
                    created_at: now,
                });
        }
        let count = paths.len();
        let result = start_native_file_drag_guarded(|| start_native_file_drag(frame, &paths));
        self.finish_external_drag_result(count, result);
    }

    fn finish_external_drag_result(
        &mut self,
        count: usize,
        result: Result<NativeDragOutcome, String>,
    ) {
        match result {
            Ok(NativeDragOutcome::Dropped) => {
                self.set_external_drag_status(format!("Dragged {count} file(s)"));
            }
            Ok(NativeDragOutcome::Cancel) => {
                self.set_external_drag_status(format!("Drag canceled ({count} file(s))"));
            }
            Ok(NativeDragOutcome::Started) => {
                self.set_external_drag_status(format!("Started drag for {count} file(s)"));
            }
            Err(err) => {
                self.set_external_drag_status(format!("Drag failed: {err}"));
            }
        }
    }

    pub(super) fn prepare_external_drag_paths_for_ids(
        &mut self,
        ids: &[MediaId],
    ) -> Result<PreparedExternalDrag, String> {
        let mut paths = Vec::new();
        let mut temp_paths = Vec::new();
        let mut seen = HashSet::new();
        for id in ids {
            let item = self
                .item_for_id(*id)
                .cloned()
                .ok_or_else(|| format!("item not found: {id}"))?;
            let path = if self.external_drag_should_materialize(&item.path, item.source) {
                let (audio, sample_rate) = self
                    .external_drag_audio_for_item(&item.path, item.source)
                    .map_err(|err| format!("{}: {err}", item.display_name))?;
                let path =
                    self.export_audio_to_drag_wav(&item.display_name, &audio, sample_rate)?;
                temp_paths.push(path.clone());
                path
            } else {
                canonical_file_path(&item.path)
                    .map_err(|err| format!("{}: {err}", item.display_name))?
            };
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
        Ok(PreparedExternalDrag { paths, temp_paths })
    }

    fn external_drag_should_materialize(&self, path: &Path, source: MediaSource) -> bool {
        source == MediaSource::Virtual
            || self.has_edits_for_path(path)
            || self.pending_gain_db_for_path(path).abs() > 0.0001
            || self.sample_rate_override.contains_key(path)
            || self.bit_depth_override.contains_key(path)
            || self.format_override.contains_key(path)
    }

    fn external_drag_audio_for_item(
        &self,
        path: &Path,
        source: MediaSource,
    ) -> Result<(Arc<crate::audio::AudioBuffer>, u32), String> {
        if let Some(tab) = self.tabs.iter().find(|tab| {
            (tab.dirty || tab.loop_markers_dirty || tab.markers_dirty) && tab.path.as_path() == path
        }) {
            return self.external_drag_postprocess_audio(
                path,
                Arc::new(crate::audio::AudioBuffer::from_channels(
                    tab.ch_samples.clone(),
                )),
                tab.buffer_sample_rate.max(1),
            );
        }
        if let Some(cached) = self.edited_cache.get(path) {
            return self.external_drag_postprocess_audio(
                path,
                Arc::new(crate::audio::AudioBuffer::from_channels(
                    cached.ch_samples.clone(),
                )),
                cached.buffer_sample_rate.max(1),
            );
        }
        if source == MediaSource::Virtual {
            let item = self
                .item_for_path(path)
                .ok_or_else(|| "virtual item not found".to_string())?;
            let audio = item
                .virtual_audio
                .clone()
                .ok_or_else(|| "virtual audio is not available".to_string())?;
            let sample_rate = item
                .virtual_state
                .as_ref()
                .map(|state| state.sample_rate)
                .or_else(|| item.meta.as_ref().map(|meta| meta.sample_rate))
                .unwrap_or(self.audio.shared.out_sample_rate)
                .max(1);
            return self.external_drag_postprocess_audio(path, audio, sample_rate);
        }
        let (channels, sample_rate) = crate::audio_io::decode_audio_multi(path)
            .map_err(|err| format!("decode failed: {err}"))?;
        self.external_drag_postprocess_audio(
            path,
            Arc::new(crate::audio::AudioBuffer::from_channels(channels)),
            sample_rate.max(1),
        )
    }

    fn external_drag_postprocess_audio(
        &self,
        path: &Path,
        audio: Arc<crate::audio::AudioBuffer>,
        sample_rate: u32,
    ) -> Result<(Arc<crate::audio::AudioBuffer>, u32), String> {
        let gain_db = self.pending_gain_db_for_path(path);
        let target_sr = self
            .sample_rate_override
            .get(path)
            .copied()
            .filter(|sr| *sr > 0)
            .unwrap_or(sample_rate);
        if gain_db.abs() <= 0.0001 && target_sr == sample_rate {
            return Ok((audio, sample_rate.max(1)));
        }
        let gain = crate::app::helpers::db_to_amp(gain_db);
        let mut channels = audio.channels.clone();
        if gain_db.abs() > 0.0001 {
            for channel in &mut channels {
                for sample in channel {
                    *sample = (*sample * gain).clamp(-1.0, 1.0);
                }
            }
        }
        if target_sr != sample_rate {
            channels = crate::wave::resample_channels_quality(
                &channels,
                sample_rate,
                target_sr,
                Self::to_wave_resample_quality(self.src_quality),
            );
        }
        Ok((
            Arc::new(crate::audio::AudioBuffer::from_channels(channels)),
            target_sr.max(1),
        ))
    }

    fn export_audio_to_drag_wav(
        &mut self,
        display_name: &str,
        audio: &crate::audio::AudioBuffer,
        sample_rate: u32,
    ) -> Result<PathBuf, String> {
        if audio.is_empty() {
            return Err(format!("{display_name}: audio is empty"));
        }
        let _ = display_name;
        let path = super::temp_audio_ops::allocate_neowaves_temp_cache_path("drag", "wav")
            .ok_or_else(|| "could not allocate unique drag temp path".to_string())?;
        crate::wave::export_selection_wav(
            &audio.channels,
            sample_rate.max(1),
            (0, audio.len()),
            &path,
        )
        .map_err(|err| format!("export drag wav failed: {err}"))?;
        Ok(path)
    }

    fn cleanup_external_drag_temp_files(&mut self) {
        let now = Instant::now();
        while self
            .external_drag_temp_files
            .front()
            .map(|entry| now.duration_since(entry.created_at) >= DRAG_TEMP_RETENTION)
            .unwrap_or(false)
        {
            if let Some(entry) = self.external_drag_temp_files.pop_front() {
                let _ = std::fs::remove_file(entry.path);
            }
        }
    }

    fn set_external_drag_status(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.external_drag_last_status = Some(message.clone());
        self.debug_log(format!("external drag: {message}"));
    }
}

fn canonical_file_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!("not a file: {}", path.display()));
    }
    std::fs::canonicalize(path).map_err(|err| format!("canonicalize failed: {err}"))
}

fn canonicalize_drag_payload_paths(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::with_capacity(paths.len());
    let mut seen = HashSet::new();
    for path in paths {
        let canonical = canonical_file_path(path)?;
        if seen.insert(canonical.clone()) {
            out.push(canonical);
        }
    }
    Ok(out)
}

fn start_native_file_drag_guarded<F>(start: F) -> Result<NativeDragOutcome, String>
where
    F: FnOnce() -> Result<NativeDragOutcome, String>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(start)) {
        Ok(result) => result,
        Err(_) => Err("native drag panicked".to_string()),
    }
}

#[cfg(target_os = "windows")]
pub(super) fn start_native_file_drag(
    frame: &mut eframe::Frame,
    paths: &[PathBuf],
) -> Result<NativeDragOutcome, String> {
    if paths.is_empty() {
        return Err("no files to drag".to_string());
    }
    let result = std::sync::Arc::new(std::sync::Mutex::new(None));
    let result_for_callback = result.clone();
    drag::start_drag(
        frame,
        drag::DragItem::Files(paths.to_vec()),
        drag::Image::Raw(Vec::new()),
        move |drag_result, _cursor| {
            if let Ok(mut slot) = result_for_callback.lock() {
                *slot = Some(match drag_result {
                    drag::DragResult::Dropped => NativeDragOutcome::Dropped,
                    drag::DragResult::Cancel => NativeDragOutcome::Cancel,
                });
            }
        },
        drag::Options {
            mode: drag::DragMode::Copy,
            ..Default::default()
        },
    )
    .map_err(|err| err.to_string())?;
    Ok(result
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or(NativeDragOutcome::Started))
}

#[cfg(not(target_os = "windows"))]
pub(super) fn start_native_file_drag(
    _frame: &mut eframe::Frame,
    _paths: &[PathBuf],
) -> Result<NativeDragOutcome, String> {
    Err("external file drag is supported on Windows only in this build".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::types::{MediaItem, MediaStatus};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_external_drag_test_{tag}_{}_{}",
            std::process::id(),
            ts
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn add_item(app: &mut WavesPreviewer, path: PathBuf, source: MediaSource) -> MediaId {
        let id = app.next_media_id;
        app.next_media_id += 1;
        let display_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("item.wav")
            .to_string();
        let display_folder: std::sync::Arc<str> = std::sync::Arc::from(
            path.parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        let item = MediaItem {
            id,
            path: path.clone(),
            display_name,
            display_folder,
            source,
            meta: None,
            pending_gain_db: 0.0,
            status: MediaStatus::Ok,
            transcript: None,
            transcript_language: None,
            external: Default::default(),
            virtual_audio: None,
            virtual_state: None,
        };
        app.items.push(item);
        app.files.push(id);
        app.rebuild_item_indexes();
        id
    }

    #[test]
    fn external_drag_real_file_uses_canonical_path_without_temp() {
        let dir = temp_dir("real");
        let wav = dir.join("source.wav");
        crate::wave::export_channels_audio(&[vec![0.0, 0.1, -0.1]], 48_000, &wav)
            .expect("write wav");
        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        let id = add_item(&mut app, wav.clone(), MediaSource::File);

        let prepared = app
            .prepare_external_drag_paths_for_ids(&[id])
            .expect("prepare");

        assert_eq!(prepared.paths, vec![std::fs::canonicalize(&wav).unwrap()]);
        assert!(prepared.temp_paths.is_empty());
    }

    #[test]
    fn external_drag_virtual_item_materializes_temp_wav() {
        let dir = temp_dir("virtual");
        let virtual_path = dir.join("virtual.wav");
        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        let id = add_item(&mut app, virtual_path.clone(), MediaSource::Virtual);
        let item = app.item_for_id_mut(id).expect("item");
        item.virtual_audio = Some(Arc::new(crate::audio::AudioBuffer::from_channels(vec![
            vec![0.0, 0.25, -0.25, 0.0],
        ])));

        let prepared = app
            .prepare_external_drag_paths_for_ids(&[id])
            .expect("prepare");

        assert_eq!(prepared.paths.len(), 1);
        assert_eq!(prepared.temp_paths.len(), 1);
        assert!(prepared.paths[0].is_file());
        assert_eq!(
            prepared.paths[0].extension().and_then(|s| s.to_str()),
            Some("wav")
        );
    }

    #[test]
    fn external_drag_pending_gain_materializes_real_file() {
        let dir = temp_dir("gain");
        let wav = dir.join("source.wav");
        crate::wave::export_channels_audio(&[vec![0.2, 0.2, 0.2]], 48_000, &wav)
            .expect("write wav");
        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        let id = add_item(&mut app, wav.clone(), MediaSource::File);
        app.set_pending_gain_db_for_path(&wav, -6.0);

        let prepared = app
            .prepare_external_drag_paths_for_ids(&[id])
            .expect("prepare");

        assert_eq!(prepared.paths.len(), 1);
        assert_eq!(prepared.temp_paths.len(), 1);
        assert_ne!(prepared.paths[0], std::fs::canonicalize(&wav).unwrap());
        assert!(prepared.paths[0].is_file());
        assert!(
            !prepared.paths[0]
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .contains("source"),
            "drag cache file name should not expose source file name"
        );
    }

    #[test]
    fn external_drag_fails_all_when_selected_item_cannot_prepare() {
        let dir = temp_dir("unsupported");
        let missing = dir.join("missing.wav");
        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        let id = add_item(&mut app, missing, MediaSource::File);

        let err = app
            .prepare_external_drag_paths_for_ids(&[id])
            .expect_err("missing file should fail");

        assert!(err.contains("not a file") || err.contains("canonicalize"));
    }

    #[test]
    fn external_drag_dedupes_prepared_paths_in_order() {
        let dir = temp_dir("dedupe");
        let wav = dir.join("source.wav");
        crate::wave::export_channels_audio(&[vec![0.0, 0.1, 0.0]], 48_000, &wav)
            .expect("write wav");
        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        let first = add_item(&mut app, wav.clone(), MediaSource::File);
        let second = add_item(&mut app, wav, MediaSource::File);

        let prepared = app
            .prepare_external_drag_paths_for_ids(&[first, second])
            .expect("prepare");

        assert_eq!(prepared.paths.len(), 1);
    }

    #[test]
    fn external_drag_same_name_virtual_items_get_distinct_temp_paths() {
        let dir = temp_dir("same_name_virtual");
        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        let first = add_item(
            &mut app,
            dir.join("a").join("clip.wav"),
            MediaSource::Virtual,
        );
        let second = add_item(
            &mut app,
            dir.join("b").join("clip.wav"),
            MediaSource::Virtual,
        );
        for id in [first, second] {
            app.item_for_id_mut(id).expect("item").virtual_audio =
                Some(Arc::new(crate::audio::AudioBuffer::from_channels(vec![
                    vec![0.0, 0.15, 0.0],
                ])));
        }

        let prepared = app
            .prepare_external_drag_paths_for_ids(&[first, second])
            .expect("prepare");

        assert_eq!(prepared.paths.len(), 2);
        assert_eq!(prepared.temp_paths.len(), 2);
        assert_ne!(prepared.paths[0], prepared.paths[1]);
        assert!(prepared.paths.iter().all(|path| path.is_file()));
    }

    #[test]
    fn external_drag_guard_converts_native_panic_to_error() {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = start_native_file_drag_guarded(|| -> Result<NativeDragOutcome, String> {
            panic!("simulated native drag panic");
        });
        std::panic::set_hook(hook);
        let err = result.expect_err("panic should be converted into an error");
        assert!(err.contains("native drag panicked"));
    }

    #[test]
    fn external_drag_payload_canonicalize_rejects_missing_file() {
        let dir = temp_dir("canonical_missing");
        let missing = dir.join("missing.wav");
        let err = canonicalize_drag_payload_paths(&[missing]).expect_err("missing path");
        assert!(err.contains("not a file"));
    }
}
