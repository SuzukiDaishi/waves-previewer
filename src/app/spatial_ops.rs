//! Object audio in the app: reading scenes on a worker, keeping the recent
//! ones, the per-frame hand-off of the playing source's mix to the engine,
//! and the user's spatial edits (kept in the session, never in the file).
//! The rules live in `crate::spatial` and `crate::adm`; the callback side is
//! `crate::object_mix`. See `docs/SPATIAL_AUDIO_SPEC.md`.
//!
//! A source plays through the object mix only while the engine streams that
//! very file. Until its scene is read the mix is *pending*: the transport
//! holds, silent, rather than play beds and objects as if they were speaker
//! channels.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Instant;

use crate::object_mix::{MixKeyCache, ObjectMix, ObjectMixSource};
use crate::spatial::scene::{ObjectScene, SceneEdits};

use super::loading_ops::{poll_job, JobPoll};
use super::WavesPreviewer;

/// How many read scenes are kept. A scene is the size of its keyframes --
/// tens of megabytes for a long master -- and the list rarely moves between
/// more than a few object files at once.
const SCENE_CACHE_LEN: usize = 6;

/// A scene, and for a TrueHD stream the decoded copy that plays it.
type SceneResult = Result<(Arc<ObjectScene>, Option<PathBuf>), String>;

pub(crate) enum SceneSlot {
    Loading {
        rx: Receiver<SceneResult>,
        cancel: Arc<AtomicBool>,
        /// 0..1 as f32 bits.
        progress: Arc<AtomicU32>,
    },
    Ready(Arc<ObjectScene>),
    Failed(String),
}

/// What the UI can say about a file's scene.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SceneStatus {
    Absent,
    Loading(f32),
    Ready(Arc<ObjectScene>),
    Failed(String),
}

struct CachedScene {
    slot: SceneSlot,
    used: Instant,
    keys: MixKeyCache,
}

/// Who is heard: muted elements, and soloed ones (when any is soloed, only
/// those). Per file, per run -- like a mixer's buttons, not saved.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ElementMonitor {
    pub muted: BTreeSet<Arc<str>>,
    pub soloed: BTreeSet<Arc<str>>,
}

impl ElementMonitor {
    pub fn audible(&self, key: &str) -> bool {
        if !self.soloed.is_empty() {
            return self.soloed.contains(key);
        }
        !self.muted.contains(key)
    }
}

/// What the engine was last given, so an unchanged frame does nothing.
#[derive(Clone, Debug, PartialEq)]
struct AppliedMix {
    engine: usize,
    source: PathBuf,
    stream: PathBuf,
    /// 0 while pending; the scene's address once ready.
    scene: usize,
    rev: u64,
}

#[derive(Default)]
pub(crate) struct SpatialRuntime {
    scenes: HashMap<PathBuf, CachedScene>,
    applied: Option<AppliedMix>,
    /// Bumped by anything that changes a mix: an edit, a mute, a solo.
    pub rev: u64,
    pub monitor: HashMap<PathBuf, ElementMonitor>,
    /// The user's edits per file (session state; see `session_ops`).
    pub edits: HashMap<PathBuf, Arc<SceneEdits>>,
    /// Bumped by every edit, for the session's unsaved-work check.
    pub edit_rev: u64,
    pub saved_edit_rev: u64,
    /// What a session save in flight will have written, once it lands.
    pub pending_saved_edit_rev: Option<u64>,
    /// An ADM BWF export being written.
    pub export: Option<AdmExportJob>,
    /// TrueHD streams decoded this run, to the WAVE that plays each.
    pub truehd_copies: HashMap<PathBuf, PathBuf>,
    /// A TrueHD row asked to play before its copy was ready.
    pub truehd_play_when_ready: Option<PathBuf>,
}

pub(crate) struct AdmExportJob {
    rx: Receiver<Result<PathBuf, String>>,
    cancel: Arc<AtomicBool>,
    /// 0..1 as f32 bits.
    progress: Arc<AtomicU32>,
}

impl WavesPreviewer {
    /// Start reading `path`'s scene if nothing has yet. Cheap to call every
    /// frame.
    pub(super) fn ensure_object_scene(&mut self, path: &Path) {
        if let Some(cached) = self.spatial.scenes.get_mut(path) {
            cached.used = Instant::now();
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(0f32.to_bits()));
        let worker_path = path.to_path_buf();
        let worker_cancel = Arc::clone(&cancel);
        let worker_progress = Arc::clone(&progress);
        let truehd = self.is_truehd_source(path);
        // A TrueHD stream is decoded whole into the temp cache: the copy is
        // what plays, and the decode is what yields the scene.
        let copy_dest = truehd
            .then(|| super::temp_audio_ops::allocate_neowaves_temp_cache_path("truehd", "wav"))
            .flatten();
        self.debug_log(format!(
            "spatial: {} {}",
            if truehd {
                "decoding TrueHD"
            } else {
                "reading scene of"
            },
            path.display()
        ));
        let spawned = std::thread::Builder::new()
            .name("object-scene".to_string())
            .spawn(move || {
                let mut report = |p: f32| worker_progress.store(p.to_bits(), Ordering::Relaxed);
                let result = if truehd {
                    decode_truehd_copy(&worker_path, copy_dest, &worker_cancel, &mut report)
                } else {
                    crate::adm::load_scene(&worker_path, Some(&worker_cancel), &mut report)
                        .map_err(|err| format!("{err:#}"))
                        .and_then(|scene| {
                            // `chna` and `axml` that describe nothing the mix
                            // can play (an `axml` of other metadata): play the
                            // file as plain channels rather than as silence.
                            if scene.elements.is_empty() {
                                Err(format!(
                                    "no beds or objects to play{}",
                                    if scene.diagnostics.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" ({})", scene.diagnostics.join("; "))
                                    }
                                ))
                            } else {
                                Ok((Arc::new(scene), None))
                            }
                        })
                };
                let _ = tx.send(result);
                crate::ui_wake::wake_ui();
            });
        let slot = match spawned {
            Ok(_) => SceneSlot::Loading {
                rx,
                cancel,
                progress,
            },
            Err(err) => SceneSlot::Failed(format!("could not start the reader: {err}")),
        };
        self.spatial.scenes.insert(
            path.to_path_buf(),
            CachedScene {
                slot,
                used: Instant::now(),
                keys: MixKeyCache::default(),
            },
        );
        self.evict_object_scenes();
    }

    fn evict_object_scenes(&mut self) {
        while self.spatial.scenes.len() > SCENE_CACHE_LEN {
            let Some(oldest) = self
                .spatial
                .scenes
                .iter()
                .filter(|(_, cached)| !matches!(cached.slot, SceneSlot::Loading { .. }))
                .min_by_key(|(_, cached)| cached.used)
                .map(|(path, _)| path.clone())
            else {
                return;
            };
            self.spatial.scenes.remove(&oldest);
        }
    }

    pub(super) fn object_scene_status(&self, path: &Path) -> SceneStatus {
        match self.spatial.scenes.get(path).map(|cached| &cached.slot) {
            None => SceneStatus::Absent,
            Some(SceneSlot::Loading { progress, .. }) => {
                SceneStatus::Loading(f32::from_bits(progress.load(Ordering::Relaxed)))
            }
            Some(SceneSlot::Ready(scene)) => SceneStatus::Ready(Arc::clone(scene)),
            Some(SceneSlot::Failed(message)) => SceneStatus::Failed(message.clone()),
        }
    }

    pub(super) fn object_scene(&self, path: &Path) -> Option<Arc<ObjectScene>> {
        match self.spatial.scenes.get(path).map(|cached| &cached.slot) {
            Some(SceneSlot::Ready(scene)) => Some(Arc::clone(scene)),
            _ => None,
        }
    }

    /// Ask where to, then write `tab_idx`'s file as a new ADM BWF with its
    /// spatial edits, on a worker.
    pub(super) fn spatial_start_export(&mut self, tab_idx: usize) {
        if self.spatial.export.is_some() {
            return;
        }
        let Some(path) = self.tabs.get(tab_idx).map(|tab| tab.path.clone()) else {
            return;
        };
        let Some(scene) = self.object_scene(&path) else {
            return;
        };
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "export".to_string());
        let Some(dest) = self.pick_adm_export_dialog(&format!("{stem}_spatial.wav")) else {
            return;
        };
        let edits = self.spatial_edits_for(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(0f32.to_bits()));
        let (worker_cancel, worker_progress) = (Arc::clone(&cancel), Arc::clone(&progress));
        self.debug_log(format!(
            "spatial: exporting {} -> {}",
            path.display(),
            dest.display()
        ));
        let spawned = std::thread::Builder::new()
            .name("adm-export".to_string())
            .spawn(move || {
                let result = crate::adm::export::export_adm(
                    &path,
                    &dest,
                    &scene,
                    edits.as_deref(),
                    &worker_cancel,
                    &mut |p| worker_progress.store(p.to_bits(), Ordering::Relaxed),
                )
                .map(|()| dest)
                .map_err(|err| format!("{err:#}"));
                let _ = tx.send(result);
                crate::ui_wake::wake_ui();
            });
        match spawned {
            Ok(_) => {
                self.spatial.export = Some(AdmExportJob {
                    rx,
                    cancel,
                    progress,
                })
            }
            Err(err) => self.push_toast(
                super::types::ToastSeverity::Error,
                format!("Could not start the export: {err}"),
            ),
        }
    }

    pub(super) fn spatial_export_progress(&self) -> Option<f32> {
        self.spatial
            .export
            .as_ref()
            .map(|job| f32::from_bits(job.progress.load(Ordering::Relaxed)))
    }

    pub(super) fn spatial_cancel_export(&mut self) {
        if let Some(job) = &self.spatial.export {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn drain_spatial_export(&mut self) {
        let Some(job) = &self.spatial.export else {
            return;
        };
        let outcome = match poll_job(&job.rx) {
            JobPoll::Waiting => return,
            JobPoll::Ready(result) => result,
            JobPoll::Gone => Err("the export ended without an answer".to_string()),
        };
        self.spatial.export = None;
        match outcome {
            Ok(dest) => {
                self.debug_log(format!("spatial: exported {}", dest.display()));
                self.push_toast(
                    super::types::ToastSeverity::Info,
                    format!(
                        "Exported ADM BWF: {}",
                        dest.file_name()
                            .map(|n| n.to_string_lossy())
                            .unwrap_or_default()
                    ),
                );
            }
            Err(message) => {
                self.debug_log(format!("spatial: export failed: {message}"));
                self.push_toast(
                    super::types::ToastSeverity::Error,
                    format!("ADM BWF export failed: {message}"),
                );
            }
        }
    }

    /// Land finished scene reads. Not deferrable: a playing source is
    /// holding silent until its scene arrives.
    pub(super) fn drain_object_scene_jobs(&mut self) {
        self.drain_spatial_export();
        let mut landed: Vec<(PathBuf, SceneSlot)> = Vec::new();
        let mut copies: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (path, cached) in &self.spatial.scenes {
            let SceneSlot::Loading { rx, .. } = &cached.slot else {
                continue;
            };
            match poll_job(rx) {
                JobPoll::Waiting => {}
                JobPoll::Ready(Ok((scene, copy))) => {
                    if let Some(copy) = copy {
                        copies.push((path.clone(), copy));
                    }
                    landed.push((path.clone(), SceneSlot::Ready(scene)))
                }
                JobPoll::Ready(Err(message)) => {
                    landed.push((path.clone(), SceneSlot::Failed(message)))
                }
                JobPoll::Gone => landed.push((
                    path.clone(),
                    SceneSlot::Failed("the scene reader ended without an answer".to_string()),
                )),
            }
        }
        for (path, slot) in landed {
            match &slot {
                SceneSlot::Ready(scene) => {
                    self.debug_log(format!(
                        "spatial: {} -- {} bed + {} obj{}",
                        path.display(),
                        scene.bed_count(),
                        scene.object_count(),
                        if scene.diagnostics.is_empty() {
                            String::new()
                        } else {
                            format!("; {}", scene.diagnostics.join("; "))
                        }
                    ));
                }
                SceneSlot::Failed(message) => {
                    self.debug_log(format!("spatial: {} failed: {message}", path.display()));
                }
                SceneSlot::Loading { .. } => {}
            }
            if let Some(cached) = self.spatial.scenes.get_mut(&path) {
                cached.slot = slot;
            }
        }
        for (path, copy) in copies {
            self.spatial.truehd_copies.insert(path.clone(), copy);
            self.truehd_copy_ready(&path);
        }
    }

    /// A TrueHD stream's copy has landed: play the row that was waiting for
    /// it, and open the editor tabs that were.
    fn truehd_copy_ready(&mut self, path: &Path) {
        if self.spatial.truehd_play_when_ready.as_deref() == Some(path) {
            self.spatial.truehd_play_when_ready = None;
            if self.playing_path.as_deref() == Some(path)
                && self.try_activate_list_stream_transport(path)
            {
                self.audio.play();
            }
        }
        let waiting = self
            .tabs
            .iter()
            .any(|tab| tab.path.as_path() == path && tab.loading);
        if waiting {
            self.spawn_editor_decode(path.to_path_buf());
        }
    }

    /// A TrueHD stream: a `.thd` / `.mlp`, or a transport stream whose
    /// metadata says its audio is TrueHD. A lookup, never a read.
    pub(super) fn is_truehd_source(&self, path: &Path) -> bool {
        crate::audio_io::is_truehd_path(path)
            || self
                .meta_for_path(path)
                .and_then(|meta| meta.object_audio.as_ref())
                .is_some_and(|summary| summary.format == crate::spatial::ObjectFormat::TrueHd)
    }

    /// The decoded copy a TrueHD stream plays from, once there is one.
    pub(super) fn truehd_copy(&self, path: &Path) -> Option<&PathBuf> {
        self.spatial.truehd_copies.get(path)
    }

    /// Ask for a TrueHD stream's copy; `play` plays the list row when it
    /// lands. Returns whether the copy is already there.
    pub(super) fn request_truehd_copy(&mut self, path: &Path, play: bool) -> bool {
        if self.truehd_copy(path).is_some() {
            return true;
        }
        if play {
            self.spatial.truehd_play_when_ready = Some(path.to_path_buf());
        }
        let started = !self.spatial.scenes.contains_key(path);
        self.ensure_object_scene(path);
        if started {
            self.push_toast(
                super::types::ToastSeverity::Info,
                "Decoding TrueHD\u{2026} it plays when ready",
            );
        }
        false
    }

    /// Forget a file's scene (it changed on disk, or was renamed): the next
    /// play reads it again.
    pub(super) fn forget_object_scene(&mut self, path: &Path) {
        // A copy of a stream that changed is a copy of something else now.
        self.spatial.truehd_copies.remove(path);
        if let Some(cached) = self.spatial.scenes.remove(path) {
            if let SceneSlot::Loading { cancel, .. } = cached.slot {
                cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    /// The object-audio file the playing source is, when the engine is
    /// streaming that very file.
    fn playing_object_source(&self) -> Option<(PathBuf, PathBuf)> {
        let path = match &self.playback_session.source {
            super::PlaybackSourceKind::ListPreview(path)
            | super::PlaybackSourceKind::EditorTab(path) => path,
            _ => return None,
        };
        if !self.source_content(path).object_audio {
            return None;
        }
        let stream = self.resolved_audio_file_path(path)?;
        self.audio
            .is_streaming_wav_path(&stream)
            .then(|| (path.clone(), stream))
    }

    /// Give the engine the mix for what is playing, or take it away. Cheap
    /// when nothing moved: it rebuilds only when the source, its scene, an
    /// edit, a mute or a solo changed.
    pub(super) fn sync_object_mix(&mut self) {
        let engine = Arc::as_ptr(&self.audio.shared) as usize;
        let Some((source, stream)) = self.playing_object_source() else {
            if self.spatial.applied.take().is_some() || self.audio.object_mix().is_some() {
                self.audio.set_object_mix(None);
            }
            return;
        };
        self.ensure_object_scene(&source);
        let scene = self.object_scene(&source);
        let failed = matches!(self.object_scene_status(&source), SceneStatus::Failed(_));
        let applied = AppliedMix {
            engine,
            source: source.clone(),
            stream: stream.clone(),
            scene: scene
                .as_ref()
                .map(|scene| Arc::as_ptr(scene) as usize)
                .unwrap_or(if failed { 1 } else { 0 }),
            rev: self.spatial.rev,
        };
        if self.spatial.applied.as_ref() == Some(&applied) {
            return;
        }
        let Some((tracks, frames, file_sr)) = self.audio.streaming_wav_shape() else {
            return;
        };
        let mix_source = ObjectMixSource {
            stream_path: Some(stream),
            tracks,
            frames,
            file_sr,
        };
        let mix = match scene {
            Some(scene)
                if scene.shape.tracks as usize == tracks
                    && scene.shape.frames as usize == frames =>
            {
                let edits = self.spatial.edits.get(&source).cloned();
                let monitor = self
                    .spatial
                    .monitor
                    .get(&source)
                    .cloned()
                    .unwrap_or_default();
                let cache = &mut self
                    .spatial
                    .scenes
                    .get_mut(&source)
                    .expect("the scene was just read from here")
                    .keys;
                Some(ObjectMix::from_scene(
                    &scene,
                    edits.as_deref(),
                    |element| monitor.audible(&element.key),
                    cache,
                    mix_source,
                ))
            }
            Some(scene) => {
                self.debug_log(format!(
                    "spatial: {} -- the scene is for {} tracks x {} frames, the stream is {tracks} x {frames}; playing it as channels",
                    source.display(),
                    scene.shape.tracks,
                    scene.shape.frames
                ));
                None
            }
            // A scene that cannot be read plays as channels, which is
            // what the file is underneath; the Debug log says why.
            None if failed => None,
            None => Some(ObjectMix::pending(mix_source)),
        };
        self.audio.set_object_mix(mix.map(Arc::new));
        self.spatial.applied = Some(applied);
    }

    /// Mark a change to what a mix contains (mute, solo, an edit).
    pub(super) fn spatial_mix_changed(&mut self) {
        self.spatial.rev = self.spatial.rev.wrapping_add(1);
    }
}

/// How many spatial edits can be undone per tab.
const SPATIAL_UNDO_LEN: usize = 100;

/// The Spatial view's state for one editor tab.
#[derive(Clone, Debug)]
pub struct SpatialTabState {
    /// The element being shown in the lanes and edited.
    pub selected: Option<Arc<str>>,
    /// The keyframe picked in the lanes, by index into the selection's.
    pub selected_key: Option<usize>,
    /// The timeline's left edge and width, in seconds (`view_secs <= 0`
    /// shows the whole file).
    pub view_start: f64,
    pub view_secs: f64,
    /// How long a keyframe placed from the room views glides in.
    pub ramp_secs: f32,
    pub drag: Option<SpatialDrag>,
    /// Earlier versions of this file's edits, for Ctrl+Z / Ctrl+Y.
    pub undo: Vec<Option<Arc<SceneEdits>>>,
    pub redo: Vec<Option<Arc<SceneEdits>>>,
}

impl Default for SpatialTabState {
    fn default() -> Self {
        Self {
            selected: None,
            selected_key: None,
            view_start: 0.0,
            view_secs: 0.0,
            ramp_secs: 0.0,
            drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
}

/// A gesture in progress. The undo point is taken when it starts, so one
/// drag is one step back.
#[derive(Clone, Debug, PartialEq)]
pub enum SpatialDrag {
    /// Moving the selected element in a room view, writing a keyframe at
    /// the playhead.
    Room,
    /// Moving keyframe `index` of the selected element in lane `axis`.
    Lane { axis: usize, index: usize },
}

impl WavesPreviewer {
    pub(super) fn spatial_edits_for(&self, path: &Path) -> Option<Arc<SceneEdits>> {
        self.spatial.edits.get(path).cloned()
    }

    /// Remember the edits as they are, for one Ctrl+Z.
    pub(super) fn spatial_checkpoint(&mut self, tab_idx: usize) {
        let Some(path) = self.tabs.get(tab_idx).map(|tab| tab.path.clone()) else {
            return;
        };
        let before = self.spatial_edits_for(&path);
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        tab.spatial.undo.push(before);
        if tab.spatial.undo.len() > SPATIAL_UNDO_LEN {
            tab.spatial.undo.remove(0);
        }
        tab.spatial.redo.clear();
    }

    fn spatial_store_edits(&mut self, path: &Path, edits: Option<Arc<SceneEdits>>) {
        match edits.filter(|edits| !edits.is_empty()) {
            Some(edits) => {
                self.spatial.edits.insert(path.to_path_buf(), edits);
            }
            None => {
                self.spatial.edits.remove(path);
            }
        }
        self.spatial.edit_rev = self.spatial.edit_rev.wrapping_add(1);
        self.spatial_mix_changed();
    }

    /// Replace `element_key`'s keyframes in `path`'s edits. Only that
    /// element is copied; every other one is shared with the version before.
    pub(super) fn spatial_set_keyframes(
        &mut self,
        path: &Path,
        scene: &ObjectScene,
        element_key: &Arc<str>,
        keyframes: Vec<crate::spatial::scene::Keyframe>,
    ) {
        let mut edits = self
            .spatial_edits_for(path)
            .map(|edits| (*edits).clone())
            .unwrap_or_default();
        edits.shape = Some(scene.shape);
        edits
            .elements
            .insert(Arc::clone(element_key), Arc::from(keyframes));
        self.spatial_store_edits(path, Some(Arc::new(edits)));
    }

    /// Put `element_key` back to the file's own keyframes.
    pub(super) fn spatial_reset_element(&mut self, path: &Path, element_key: &str) {
        let Some(mut edits) = self.spatial_edits_for(path).map(|edits| (*edits).clone()) else {
            return;
        };
        if edits.elements.remove(element_key).is_some() {
            self.spatial_store_edits(path, Some(Arc::new(edits)));
        }
    }

    pub(super) fn spatial_undo(&mut self, tab_idx: usize, redo: bool) -> bool {
        let Some(path) = self.tabs.get(tab_idx).map(|tab| tab.path.clone()) else {
            return false;
        };
        let current = self.spatial_edits_for(&path);
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        let (from, to) = if redo {
            (&mut tab.spatial.redo, &mut tab.spatial.undo)
        } else {
            (&mut tab.spatial.undo, &mut tab.spatial.redo)
        };
        let Some(target) = from.pop() else {
            return false;
        };
        to.push(current);
        tab.spatial.drag = None;
        tab.spatial.selected_key = None;
        self.spatial_store_edits(&path, target);
        true
    }

    /// Mute or solo one element of `path` (per run, not saved).
    pub(super) fn spatial_toggle_monitor(
        &mut self,
        path: &Path,
        element_key: &Arc<str>,
        solo: bool,
    ) {
        let monitor = self.spatial.monitor.entry(path.to_path_buf()).or_default();
        let set = if solo {
            &mut monitor.soloed
        } else {
            &mut monitor.muted
        };
        if !set.remove(element_key) {
            set.insert(Arc::clone(element_key));
        }
        self.spatial_mix_changed();
    }

    /// Whether `path` has spatial edits not yet in a saved session.
    pub(super) fn spatial_edits_unsaved(&self) -> bool {
        self.spatial.edit_rev != self.spatial.saved_edit_rev
    }
}

/// Decode a TrueHD stream into `dest` and return its scene and the copy.
#[cfg(feature = "truehd")]
fn decode_truehd_copy(
    path: &Path,
    dest: Option<PathBuf>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> SceneResult {
    let dest = dest.ok_or_else(|| "no room in the temp cache for the decoded copy".to_string())?;
    match crate::audio_truehd::decode_to_wave(path, &dest, cancel, progress) {
        Ok(scene) => Ok((Arc::new(scene), Some(dest))),
        Err(err) => {
            let _ = std::fs::remove_file(&dest);
            Err(format!("{err:#}"))
        }
    }
}

#[cfg(not(feature = "truehd"))]
fn decode_truehd_copy(
    _path: &Path,
    _dest: Option<PathBuf>,
    _cancel: &AtomicBool,
    _progress: &mut dyn FnMut(f32),
) -> SceneResult {
    Err("this build reads no TrueHD (it needs the `truehd` feature)".to_string())
}
