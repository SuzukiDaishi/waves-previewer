use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::types::{MediaId, MediaSource};
use super::{ExternalDragTempFile, PendingExternalDrag, WavesPreviewer};

const DRAG_TEMP_RETENTION: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct VirtualDragProvenance {
    schema_version: u32,
    asset_id: String,
    revision: u64,
    display_name: String,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
}

fn provenance_path(audio_path: &Path) -> PathBuf {
    let file_name = audio_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio.wav");
    audio_path.with_file_name(format!("{file_name}.neowaves-asset.json"))
}

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
        let mut temp_paths = prepared.temp_paths;
        let paths = match self.shell_compatible_drag_paths(&paths, &mut temp_paths) {
            Ok(paths) => paths,
            Err(err) => {
                self.set_external_drag_status(format!("Drag failed: {err}"));
                return;
            }
        };
        let now = Instant::now();
        for path in temp_paths {
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
            let has_transform = self.has_edits_for_path(&item.path)
                || self.pending_gain_db_for_path(&item.path).abs() > 0.0001
                || self.sample_rate_override.contains_key(&item.path)
                || self.bit_depth_override.contains_key(&item.path)
                || self.format_override.contains_key(&item.path);
            let path = if item.source == MediaSource::Virtual && !has_transform {
                let path = self.allocate_readable_drag_wav(&item.display_name)?;
                if let Err(asset_err) = item
                    .audio_asset
                    .access()
                    .materialize_current_revision(&path)
                {
                    // v1 sessions and in-progress resident drafts may not yet
                    // have a physical backing. Keep their asset identity while
                    // materializing the compatibility resident buffer.
                    let _ = std::fs::remove_file(&path);
                    let audio = item
                        .virtual_audio
                        .as_ref()
                        .ok_or_else(|| format!("{}: {asset_err:#}", item.display_name))?;
                    let sample_rate = item
                        .audio_asset
                        .sample_rate
                        .max(
                            item.virtual_state
                                .as_ref()
                                .map(|s| s.sample_rate)
                                .unwrap_or(0),
                        )
                        .max(1);
                    crate::wave::export_selection_wav(
                        &audio.channels,
                        sample_rate,
                        (0, audio.len()),
                        &path,
                    )
                    .map_err(|err| format!("{}: {err:#}", item.display_name))?;
                }
                let manifest_path = self.write_virtual_drag_provenance(&item, &path)?;
                temp_paths.push(path.clone());
                temp_paths.push(manifest_path);
                path
            } else if self.external_drag_should_materialize(&item.path, item.source) {
                let (audio, sample_rate) = self
                    .external_drag_audio_for_item(&item.path, item.source)
                    .map_err(|err| format!("{}: {err}", item.display_name))?;
                let path =
                    self.export_audio_to_drag_wav(&item.display_name, &audio, sample_rate)?;
                temp_paths.push(path.clone());
                if item.source == MediaSource::Virtual {
                    let manifest_path = self.write_virtual_drag_provenance(&item, &path)?;
                    temp_paths.push(manifest_path);
                }
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
        let (channels, new_sr) = Self::apply_gain_and_resample(
            (*audio.channels).clone(),
            sample_rate,
            gain_db,
            target_sr,
            Self::to_wave_resample_quality(self.src_quality),
        );
        Ok((
            Arc::new(crate::audio::AudioBuffer::from_channels(channels)),
            new_sr,
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
        let path = self.allocate_readable_drag_wav(display_name)?;
        crate::wave::export_selection_wav(
            &audio.channels,
            sample_rate.max(1),
            (0, audio.len()),
            &path,
        )
        .map_err(|err| format!("export drag wav failed: {err}"))?;
        Ok(path)
    }

    fn allocate_readable_drag_wav(&self, display_name: &str) -> Result<PathBuf, String> {
        self.allocate_drag_temp_path(display_name, "wav")
    }

    /// A free name for `display_name` inside the drag temp directory.
    ///
    /// Callers register whatever they create in `external_drag_temp_files` so
    /// the existing retention sweep removes it.
    fn allocate_drag_temp_path(
        &self,
        display_name: &str,
        extension: &str,
    ) -> Result<PathBuf, String> {
        let dir = super::temp_audio_ops::neowaves_temp_cache_dir("drag");
        std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let stem = Path::new(display_name)
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("NeoWaves Audio");
        let stem = crate::app::helpers::sanitize_filename_component(stem);
        for suffix in 0..1000u32 {
            let name = if suffix == 0 {
                format!("{stem}.{extension}")
            } else {
                format!("{stem} ({suffix}).{extension}")
            };
            let path = dir.join(name);
            if !path.exists() && !provenance_path(&path).exists() {
                return Ok(path);
            }
        }
        Err("could not allocate unique readable drag filename".to_string())
    }

    /// Rewrite the payload into paths the Win32 shell can actually parse.
    ///
    /// `drag` normalizes each path with `dunce::canonicalize` and feeds the
    /// result to `ILCreateFromPathW`, which does not understand the `\\?\`
    /// verbatim prefix. `dunce` keeps that prefix when it cannot safely drop
    /// it — paths over 260 characters, UNC network shares, and file names the
    /// legacy APIs reject — and `drag` then unwraps the shell failure into a
    /// panic (`drag-2.1.1/src/platform_impl/windows/mod.rs:370`). Copying such
    /// a file under the short temp directory gives the shell a path it accepts.
    fn shell_compatible_drag_paths(
        &mut self,
        paths: &[PathBuf],
        temp_paths: &mut Vec<PathBuf>,
    ) -> Result<Vec<PathBuf>, String> {
        self.rewrite_drag_paths_for_shell(paths, temp_paths, shell_facing_path)
    }

    /// The body of `shell_compatible_drag_paths`, with the normalization step
    /// injected.
    ///
    /// Only Windows can produce a verbatim path, so tests substitute a
    /// `facing` that returns one and exercise the copy branch on any platform.
    fn rewrite_drag_paths_for_shell(
        &mut self,
        paths: &[PathBuf],
        temp_paths: &mut Vec<PathBuf>,
        facing: impl Fn(&Path) -> Result<PathBuf, String>,
    ) -> Result<Vec<PathBuf>, String> {
        let mut out = Vec::with_capacity(paths.len());
        for path in paths {
            let facing = facing(path)?;
            if !is_verbatim_path(&facing) {
                out.push(facing);
                continue;
            }
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("NeoWaves Audio");
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("wav")
                .to_owned();
            let copy = self.allocate_drag_temp_path(name, &extension)?;
            std::fs::copy(path, &copy).map_err(|err| format!("{name}: copy failed: {err}"))?;
            self.debug_log(format!(
                "external drag: copied {name} to a short temp path (the shell cannot open the original)"
            ));
            temp_paths.push(copy.clone());
            out.push(copy);
        }
        Ok(out)
    }

    fn write_virtual_drag_provenance(
        &self,
        item: &super::types::MediaItem,
        audio_path: &Path,
    ) -> Result<PathBuf, String> {
        let manifest = VirtualDragProvenance {
            schema_version: 1,
            asset_id: item.audio_asset.id.to_hex(),
            revision: item.audio_asset.revision.0,
            display_name: item.display_name.clone(),
            sample_rate: item.audio_asset.sample_rate,
            channels: item.audio_asset.channels,
            bits_per_sample: item.audio_asset.bits_per_sample,
        };
        let manifest_path = provenance_path(audio_path);
        let json = serde_json::to_vec_pretty(&manifest).map_err(|err| err.to_string())?;
        std::fs::write(&manifest_path, json).map_err(|err| err.to_string())?;
        Ok(manifest_path)
    }

    pub(super) fn try_restore_virtual_drag_path(&mut self, audio_path: &Path) -> bool {
        let manifest_path = provenance_path(audio_path);
        let Ok(bytes) = std::fs::read(&manifest_path) else {
            return false;
        };
        let Ok(manifest) = serde_json::from_slice::<VirtualDragProvenance>(&bytes) else {
            return false;
        };
        if manifest.schema_version != 1 || !audio_path.is_file() {
            return false;
        }
        let Some(managed_path) =
            super::temp_audio_ops::allocate_neowaves_temp_cache_path("virtual_import", "wav")
        else {
            return false;
        };
        if std::fs::hard_link(audio_path, &managed_path).is_err()
            && std::fs::copy(audio_path, &managed_path).is_err()
        {
            return false;
        }
        let mut asset = crate::audio_asset::AudioAssetDescriptor::managed(managed_path.clone());
        if let Some(id) = crate::audio_asset::AudioAssetId::from_hex(&manifest.asset_id) {
            asset.id = id;
        }
        asset.revision = crate::audio_asset::AssetRevision(manifest.revision.max(1));
        asset.sample_rate = manifest.sample_rate.max(asset.sample_rate);
        asset.channels = manifest.channels.max(asset.channels);
        asset.bits_per_sample = manifest.bits_per_sample.max(asset.bits_per_sample);
        let name = self.unique_virtual_display_name(&manifest.display_name);
        let virtual_state = Some(super::types::VirtualState {
            source: super::types::VirtualSourceRef::Sidecar(
                manifest_path.to_string_lossy().to_string(),
            ),
            op_chain: Vec::new(),
            sample_rate: asset.sample_rate.max(1),
            channels: asset.channels.max(1),
            bits_per_sample: asset.bits_per_sample,
        });
        let item = self.make_virtual_item_with_asset(name, asset, None, None, virtual_state);
        let added_path = item.path.clone();
        self.add_virtual_item(item, None);
        self.recording_temp_files.push(managed_path);
        self.after_add_refresh();
        if let Some(row) = self.row_for_path(&added_path) {
            self.update_selection_on_click(row, egui::Modifiers::NONE);
        }
        true
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

/// Whether the Win32 shell will refuse this path.
///
/// `ILCreateFromPathW` cannot parse the `\\?\` verbatim prefix. Only Windows
/// produces such paths, but the check is plain string work so it compiles and
/// is tested everywhere.
fn is_verbatim_path(path: &Path) -> bool {
    path.as_os_str().to_string_lossy().starts_with(r"\\?\")
}

/// The path `drag` will hand to the shell, i.e. what its own
/// `dunce::canonicalize` call leaves behind.
fn shell_facing_path(path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    let resolved = dunce::canonicalize(path);
    #[cfg(not(windows))]
    let resolved = std::fs::canonicalize(path);
    resolved.map_err(|err| {
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        format!("{name}: {err}")
    })
}

fn start_native_file_drag_guarded<F>(start: F) -> Result<NativeDragOutcome, String>
where
    F: FnOnce() -> Result<NativeDragOutcome, String>,
{
    // `drag` unwraps shell failures rather than returning them, so a panic in
    // here is a failure mode this function handles — not a crash. Keep it out
    // of the crash reports the user is prompted to send; the message still
    // reaches the UI status and the debug log below.
    //
    // The modal drag loop pumps window messages, so app code can run
    // re-entrantly underneath and its panics are suppressed too. They are also
    // caught here rather than reaching the user as a crash, so the status line
    // and debug log stay the record either way.
    let _suppression = crate::crash_report::suppress_panic_reports();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(start)) {
        Ok(result) => result,
        Err(payload) => Err(format!(
            "native drag panicked: {}",
            crate::crash_report::panic_payload_message(payload.as_ref())
        )),
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
            audio_asset: crate::audio_asset::AudioAssetDescriptor::external(path.clone()),
            path: path.clone(),
            display_name,
            display_folder,
            source,
            meta: None,
            pending_gain_db: 0.0,
            note: String::new(),
            editor_notes: Vec::new(),
            status: MediaStatus::Ok,
            transcript: None,
            transcript_document: None,
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
        assert_eq!(prepared.temp_paths.len(), 2);
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
            prepared.paths[0]
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .contains("source"),
            "drag materialization should use a readable logical name"
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
        assert_eq!(prepared.temp_paths.len(), 4);
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
        assert!(err.contains("native drag panicked"), "{err}");
        // The payload is what says which shell call failed; without it the
        // debug log only records that something went wrong.
        assert!(err.contains("simulated native drag panic"), "{err}");
    }

    #[test]
    fn external_drag_guard_keeps_the_panic_out_of_crash_reports() {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {
            assert!(
                crate::crash_report::panic_reports_suppressed_for_test(),
                "the drag guard must suppress reporting while the panic unwinds"
            );
        }));
        let result = start_native_file_drag_guarded(|| -> Result<NativeDragOutcome, String> {
            panic!("simulated native drag panic");
        });
        std::panic::set_hook(hook);
        assert!(result.is_err());
    }

    #[test]
    fn shell_compatible_paths_leave_ordinary_files_untouched() {
        let dir = temp_dir("shell_ok");
        let wav = dir.join("source.wav");
        crate::wave::export_channels_audio(&[vec![0.0, 0.1, -0.1]], 48_000, &wav)
            .expect("write wav");
        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");

        let canonical = canonicalize_drag_payload_paths(&[wav]).expect("canonicalize");
        let mut temp_paths = Vec::new();
        let out = app
            .shell_compatible_drag_paths(&canonical, &mut temp_paths)
            .expect("shell compatible");

        assert_eq!(out.len(), 1);
        assert!(out[0].is_file());
        assert!(!is_verbatim_path(&out[0]));
        // A path the shell already accepts must not cost a copy.
        assert!(temp_paths.is_empty());
    }

    /// Scenario 1 & 2 (long paths, network shares) as far as a non-Windows
    /// host can reach: the normalization is stubbed to report what Windows
    /// would report, and the copy fallback is checked end to end.
    #[test]
    fn shell_hostile_paths_are_dragged_as_a_short_temp_copy() {
        let dir = temp_dir("shell_verbatim");
        let source = dir.join("deep take.flac");
        std::fs::write(&source, b"flac-bytes").expect("write source");
        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");

        let mut temp_paths = Vec::new();
        let out = app
            .rewrite_drag_paths_for_shell(
                std::slice::from_ref(&source),
                &mut temp_paths,
                // What `dunce::canonicalize` leaves behind for a path over 260
                // characters or one on a UNC share.
                |path| Ok(PathBuf::from(format!(r"\\?\C:{}", path.display()))),
            )
            .expect("rewrite");

        assert_eq!(out.len(), 1);
        let copy = &out[0];
        assert!(
            !is_verbatim_path(copy),
            "the shell must be handed a path it can parse, got {}",
            copy.display()
        );
        assert!(copy.is_file(), "the copy must exist before the drag starts");
        assert_eq!(std::fs::read(copy).unwrap(), b"flac-bytes");
        assert_eq!(
            copy.extension().and_then(|v| v.to_str()),
            Some("flac"),
            "the receiving application picks the handler by extension"
        );
        assert_ne!(copy, &source);
        // Registered so the existing retention sweep deletes it.
        assert_eq!(temp_paths, vec![copy.clone()]);
    }

    /// Scenario 3: a path the shell already accepts costs no copy.
    #[test]
    fn shell_compatible_paths_are_passed_through_without_a_copy() {
        let dir = temp_dir("shell_passthrough");
        let source = dir.join("plain.wav");
        std::fs::write(&source, b"wav-bytes").expect("write source");
        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");

        let mut temp_paths = Vec::new();
        let out = app
            .rewrite_drag_paths_for_shell(std::slice::from_ref(&source), &mut temp_paths, |path| {
                Ok(path.to_path_buf())
            })
            .expect("rewrite");

        assert_eq!(out, vec![source]);
        assert!(temp_paths.is_empty(), "no temp file should be created");
    }

    /// Scenario 1 on the real platform: a path past `MAX_PATH` is one
    /// `dunce::canonicalize` refuses to simplify, so the drag must fall back
    /// to a copy rather than panicking inside `drag`.
    #[cfg(windows)]
    #[test]
    fn windows_long_paths_fall_back_to_a_copy() {
        let dir = temp_dir("shell_long");
        let mut deep = dir.clone();
        // Push past MAX_PATH (260) so `dunce` keeps the verbatim prefix.
        while deep.as_os_str().len() < 300 {
            deep = deep.join("nested_directory_segment");
        }
        std::fs::create_dir_all(&deep).expect("create deep dir");
        let source = deep.join("take.wav");
        crate::wave::export_channels_audio(&[vec![0.0, 0.5, -0.5]], 48_000, &source)
            .expect("write wav");

        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        let canonical = canonicalize_drag_payload_paths(&[source]).expect("canonicalize");
        let mut temp_paths = Vec::new();
        let out = app
            .shell_compatible_drag_paths(&canonical, &mut temp_paths)
            .expect("shell compatible");

        assert_eq!(out.len(), 1);
        assert!(!is_verbatim_path(&out[0]), "{}", out[0].display());
        assert!(out[0].is_file());
        assert_eq!(temp_paths.len(), 1, "the long path must be copied");
    }

    /// Scenario 3 on the real platform.
    #[cfg(windows)]
    #[test]
    fn windows_short_paths_are_dragged_in_place() {
        let dir = temp_dir("shell_short");
        let source = dir.join("take.wav");
        crate::wave::export_channels_audio(&[vec![0.0, 0.5, -0.5]], 48_000, &source)
            .expect("write wav");

        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        let canonical = canonicalize_drag_payload_paths(&[source]).expect("canonicalize");
        let mut temp_paths = Vec::new();
        let out = app
            .shell_compatible_drag_paths(&canonical, &mut temp_paths)
            .expect("shell compatible");

        assert_eq!(out.len(), 1);
        assert!(!is_verbatim_path(&out[0]), "{}", out[0].display());
        assert!(temp_paths.is_empty(), "a short path must not be copied");
    }

    #[test]
    fn drag_temp_paths_keep_the_source_extension() {
        let app = WavesPreviewer::new_headless(Default::default()).expect("app");
        let flac = app
            .allocate_drag_temp_path("Kick Sample.flac", "flac")
            .expect("allocate");
        assert_eq!(flac.extension().and_then(|v| v.to_str()), Some("flac"));
        assert!(flac
            .file_stem()
            .and_then(|v| v.to_str())
            .is_some_and(|stem| stem.starts_with("Kick Sample")));
    }

    #[test]
    fn verbatim_paths_are_recognized_as_shell_hostile() {
        assert!(is_verbatim_path(Path::new(r"\\?\C:\audio\take.wav")));
        assert!(is_verbatim_path(Path::new(
            r"\\?\UNC\server\share\take.wav"
        )));
        assert!(!is_verbatim_path(Path::new(r"C:\audio\take.wav")));
        // A plain UNC path is fine — the shell resolves network shares.
        assert!(!is_verbatim_path(Path::new(r"\\server\share\take.wav")));
        assert!(!is_verbatim_path(Path::new("/home/user/take.wav")));
    }

    #[test]
    fn external_drag_payload_canonicalize_rejects_missing_file() {
        let dir = temp_dir("canonical_missing");
        let missing = dir.join("missing.wav");
        let err = canonicalize_drag_payload_paths(&[missing]).expect_err("missing path");
        assert!(err.contains("not a file"));
    }
}
