//! Multi Edits in the app: tabs, the source cache, the background mix and
//! its playback, the mixdown to a virtual row, undo, and the session round
//! trip. The document rules are `multi_edit.rs`; the samples are
//! `multi_edit_render.rs`; the screen is `ui/multi_edit.rs`.
//!
//! Nothing here reads a user's file on the UI thread. A clip's audio is either
//! handed over as the samples the app already holds (an edited buffer, a
//! virtual row) or named as a file, and a worker decodes, resamples and
//! measures it. The mix is rendered on a worker too and handed to the
//! playback engine whole, the way "Play Selected Together" is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use super::loading_ops::{poll_job, JobPoll};
use super::multi_edit::{ClipSource, MultiEditDoc, NewClip, TrackKind};
use super::multi_edit_render::{SourceAudio, SourceMap};
use super::render::waveform_pyramid::{PeakPyramid, DEFAULT_BASE_BIN_SAMPLES};
use super::types::{MediaSource, ToastSeverity, UndoScope, WorkspaceView};
use super::{PlaybackSourceKind, WavesPreviewer};
use crate::audio::AudioBuffer;

/// Snapshots kept per timeline for Ctrl+Z.
const MULTI_EDIT_UNDO_LIMIT: usize = 100;

/// Where the preview's video panel ids start. They share the decode workers'
/// id space with editor tabs, whose ids count up from 1, so these start far
/// above anything a session could open.
const MULTI_EDIT_VIDEO_ID_BASE: u64 = 1 << 48;

/// The preview's picture for one video file: the same panel an editor tab
/// uses, with its own decode worker.
pub(crate) struct MultiEditVideo {
    pub id: u64,
    pub panel: super::types::VideoPanelState,
}

/// A timeline's audio, as the cache holds it.
pub(crate) enum SourceSlot {
    Loading,
    Ready(Arc<LoadedSource>),
    /// Could not be read. Not retried until the row changes: a file that
    /// fails to decode would otherwise be read again every frame.
    Failed(String),
    /// The clip's row is not in the list (removed, or a session opened
    /// without it). Asked again once the row is back.
    NotInList,
}

pub(crate) struct LoadedSource {
    /// At the file's own rate, the row's pending gain applied: what a
    /// mixdown resamples from, so an export never goes through `out_sr`.
    pub native: Arc<SourceAudio>,
    /// At `out_sr`: what playback mixes.
    pub playback: Arc<SourceAudio>,
    /// Peaks of `playback`, for drawing clips.
    pub peaks: Arc<PeakPyramid>,
    stamp: SourceStamp,
}

/// What a row looked like when its audio was read. A different stamp means
/// the row now sounds different, and the cache reads it again.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SourceStamp {
    gain_bits: u32,
    asset_revision: u64,
    edited: bool,
}

enum SourceInput {
    Samples {
        channels: Arc<Vec<Vec<f32>>>,
        sample_rate: u32,
    },
    File(PathBuf),
}

struct SourceRequest {
    path: PathBuf,
    input: SourceInput,
    gain_db: f32,
    stamp: SourceStamp,
}

struct SourceLoaded {
    path: PathBuf,
    result: Result<LoadedSource, String>,
}

struct SourceJob {
    rx: mpsc::Receiver<SourceLoaded>,
    pending: Vec<PathBuf>,
}

struct RenderJob {
    rx: mpsc::Receiver<Arc<AudioBuffer>>,
    doc_id: String,
    rev: u64,
}

/// The most recent playback mix and which edit it reflects.
pub(crate) struct MixReady {
    pub doc_id: String,
    pub rev: u64,
    pub audio: Arc<AudioBuffer>,
}

pub(crate) struct ExportJob {
    rx: mpsc::Receiver<Result<PathBuf, String>>,
    pub cancel: Arc<AtomicBool>,
    /// Fraction done, as `f32::to_bits`.
    pub progress: Arc<AtomicU32>,
    pub doc_id: String,
}

impl ExportJob {
    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }
}

/// What a pointer drag on the timeline is doing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ClipDragKind {
    /// `grab_secs` is where on the clip it was picked up.
    Move { grab_secs: f64 },
    TrimStart,
    TrimEnd,
    FadeIn,
    FadeOut,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClipDrag {
    pub clip_id: String,
    pub kind: ClipDragKind,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LaneDrag {
    pub track_id: String,
    pub lane_id: String,
    pub point: usize,
}

#[derive(Default)]
pub(crate) struct MultiEditUi {
    pub selected_clip: Option<String>,
    pub clip_drag: Option<ClipDrag>,
    pub lane_drag: Option<LaneDrag>,
    /// (track id, name being typed).
    pub renaming_track: Option<(String, String)>,
    /// (timeline id, name being typed).
    pub renaming_doc: Option<(String, String)>,
    pub confirm_delete_doc: Option<String>,
}

#[derive(Default)]
pub(crate) struct MultiEditRuntime {
    pub docs: Vec<MultiEditDoc>,
    /// The timeline the Multi Edit workspace shows.
    pub active: Option<String>,
    undo: HashMap<String, (Vec<MultiEditDoc>, Vec<MultiEditDoc>)>,
    /// Bumped on every edit; a mix is stamped with the revision it rendered.
    revs: HashMap<String, u64>,
    pub sources: HashMap<PathBuf, SourceSlot>,
    source_jobs: Vec<SourceJob>,
    render: Option<RenderJob>,
    pub mix: Option<MixReady>,
    /// When the last edit happened, for the render debounce.
    edited_at: Option<Instant>,
    pub export: Option<ExportJob>,
    pub playhead_secs: HashMap<String, f64>,
    /// A timeline whose Play was pressed before its mix existed.
    play_when_ready: Option<String>,
    /// Video preview panels, by source file. Only the one on screen is kept.
    pub video_panels: HashMap<PathBuf, MultiEditVideo>,
    next_video_id: u64,
    pub ui: MultiEditUi,
}

impl MultiEditRuntime {
    pub fn doc(&self, id: &str) -> Option<&MultiEditDoc> {
        self.docs.iter().find(|doc| doc.id == id)
    }

    pub fn doc_mut(&mut self, id: &str) -> Option<&mut MultiEditDoc> {
        self.docs.iter_mut().find(|doc| doc.id == id)
    }

    pub fn rev(&self, id: &str) -> u64 {
        self.revs.get(id).copied().unwrap_or(0)
    }

    /// The loaded source a clip plays, if it is ready.
    pub fn ready_source(&self, path: &Path) -> Option<&Arc<LoadedSource>> {
        match self.sources.get(path) {
            Some(SourceSlot::Ready(source)) => Some(source),
            _ => None,
        }
    }

    fn doc_sources_loading(&self, doc: &MultiEditDoc) -> bool {
        doc.sources()
            .iter()
            .any(|source| matches!(self.sources.get(&source.path), Some(SourceSlot::Loading)))
    }
}

/// Decode, resample and measure one source. Runs on a worker.
fn load_source(
    request: SourceRequest,
    out_sr: u32,
    quality: crate::wave::ResampleQuality,
) -> Result<LoadedSource, String> {
    let (channels, sample_rate) = match request.input {
        SourceInput::Samples {
            channels,
            sample_rate,
        } => (channels, sample_rate.max(1)),
        SourceInput::File(path) => {
            let (channels, sr) =
                crate::audio_io::decode_audio_multi(&path).map_err(|err| err.to_string())?;
            (Arc::new(channels), sr.max(1))
        }
    };
    if channels.is_empty() || channels[0].is_empty() {
        return Err("no audio".to_string());
    }
    let channels = if request.gain_db.abs() > 0.0001 {
        let gain = crate::app::helpers::db_to_amp(request.gain_db);
        let mut owned = (*channels).clone();
        for channel in &mut owned {
            for v in channel.iter_mut() {
                *v *= gain;
            }
        }
        Arc::new(owned)
    } else {
        channels
    };
    let native = Arc::new(SourceAudio {
        channels,
        sample_rate,
    });
    let playback = if sample_rate == out_sr {
        native.clone()
    } else {
        Arc::new(SourceAudio {
            channels: Arc::new(crate::wave::resample_channels_quality(
                &native.channels,
                sample_rate,
                out_sr,
                quality,
            )),
            sample_rate: out_sr,
        })
    };
    let peaks = Arc::new(PeakPyramid::from_mixdown_channels(
        &playback.channels,
        playback.frames(),
        DEFAULT_BASE_BIN_SAMPLES,
    ));
    Ok(LoadedSource {
        native,
        playback,
        peaks,
        stamp: request.stamp,
    })
}

impl WavesPreviewer {
    pub(super) fn is_multi_edit_workspace_active(&self) -> bool {
        self.workspace_view == WorkspaceView::MultiEdit && self.multi_edit_active_doc().is_some()
    }

    pub(super) fn multi_edit_active_doc(&self) -> Option<&MultiEditDoc> {
        let id = self.multi_edit.active.as_deref()?;
        self.multi_edit.doc(id).filter(|doc| doc.open)
    }

    pub(super) fn multi_edit_active_doc_mut(&mut self) -> Option<&mut MultiEditDoc> {
        let id = self.multi_edit.active.clone()?;
        self.multi_edit.doc_mut(&id).filter(|doc| doc.open)
    }

    /// Leave whatever workspace is up for a timeline, the way the other
    /// workspace switches do: drop the editor preview and any activation
    /// still waiting.
    fn multi_edit_enter_workspace(&mut self, id: &str) {
        if let Some(idx) = self.active_tab {
            self.clear_preview_if_any(idx);
        }
        self.pending_editor_autoplay_path = None;
        self.pending_activate_path = None;
        self.pending_activate_kind = None;
        self.pending_activate_ready = false;
        self.multi_edit.active = Some(id.to_string());
        self.workspace_view = WorkspaceView::MultiEdit;
    }

    /// A new, empty timeline in its own tab.
    pub(super) fn multi_edit_new(&mut self) -> String {
        let name = (1..)
            .map(|n| format!("Multi Edit {n}"))
            .find(|name| !self.multi_edit.docs.iter().any(|doc| &doc.name == name))
            .unwrap_or_else(|| "Multi Edit".to_string());
        let mut doc = MultiEditDoc::new(name);
        doc.add_track(TrackKind::Audio);
        let id = doc.id.clone();
        self.multi_edit.docs.push(doc);
        self.multi_edit_enter_workspace(&id);
        id
    }

    pub(super) fn multi_edit_open(&mut self, id: &str) {
        let Some(doc) = self.multi_edit.doc_mut(id) else {
            return;
        };
        doc.open = true;
        self.multi_edit_enter_workspace(id);
    }

    /// Close a timeline's tab. The timeline stays in the session.
    pub(super) fn multi_edit_close(&mut self, id: &str) {
        if self.multi_edit_playback_is(id) {
            self.audio.stop();
            self.playback_session.source = PlaybackSourceKind::None;
            self.playback_session.is_playing = false;
        }
        if let Some(doc) = self.multi_edit.doc_mut(id) {
            doc.open = false;
        }
        if self.multi_edit.active.as_deref() == Some(id) {
            self.multi_edit.active = self
                .multi_edit
                .docs
                .iter()
                .find(|doc| doc.open)
                .map(|doc| doc.id.clone());
            if self.workspace_view == WorkspaceView::MultiEdit {
                match self.multi_edit.active.clone() {
                    Some(next) => self.multi_edit_enter_workspace(&next),
                    None => self.workspace_view = WorkspaceView::List,
                }
            }
        }
    }

    /// Remove a timeline for good.
    pub(super) fn multi_edit_delete(&mut self, id: &str) {
        self.multi_edit_close(id);
        self.multi_edit.docs.retain(|doc| doc.id != id);
        self.multi_edit.undo.remove(id);
        self.multi_edit.revs.remove(id);
        self.multi_edit.playhead_secs.remove(id);
        if self.multi_edit.mix.as_ref().is_some_and(|mix| mix.doc_id == id) {
            self.multi_edit.mix = None;
        }
    }

    /// Everything about Multi Edits that belongs to a session, forgotten.
    pub(super) fn multi_edit_reset(&mut self) {
        if matches!(self.playback_session.source, PlaybackSourceKind::MultiEdit(_)) {
            self.audio.stop();
            self.playback_session.source = PlaybackSourceKind::None;
            self.playback_session.is_playing = false;
        }
        if let Some(export) = self.multi_edit.export.as_ref() {
            export.cancel.store(true, Ordering::Relaxed);
        }
        self.multi_edit = MultiEditRuntime::default();
        if self.workspace_view == WorkspaceView::MultiEdit {
            self.workspace_view = WorkspaceView::List;
        }
    }

    /// Snapshot the active timeline before an edit, for Ctrl+Z.
    pub(super) fn multi_edit_checkpoint(&mut self) {
        let Some(doc) = self.multi_edit_active_doc().cloned() else {
            return;
        };
        let stacks = self.multi_edit.undo.entry(doc.id.clone()).or_default();
        stacks.1.clear();
        if stacks.0.last() == Some(&doc) {
            return;
        }
        stacks.0.push(doc);
        if stacks.0.len() > MULTI_EDIT_UNDO_LIMIT {
            stacks.0.remove(0);
        }
        self.last_undo_scope = UndoScope::MultiEdit;
    }

    /// Note that the active timeline changed: its mix is out of date.
    pub(super) fn multi_edit_touched(&mut self) {
        let Some(id) = self.multi_edit.active.clone() else {
            return;
        };
        *self.multi_edit.revs.entry(id).or_default() += 1;
        self.multi_edit.edited_at = Some(Instant::now());
    }

    fn multi_edit_restore(&mut self, redo: bool) -> bool {
        let Some(id) = self.multi_edit.active.clone() else {
            return false;
        };
        let Some(current) = self.multi_edit.doc(&id).cloned() else {
            return false;
        };
        let stacks = self.multi_edit.undo.entry(id.clone()).or_default();
        let (from, to) = if redo {
            (&mut stacks.1, &mut stacks.0)
        } else {
            (&mut stacks.0, &mut stacks.1)
        };
        let Some(mut restored) = from.pop() else {
            return false;
        };
        to.push(current.clone());
        // The view is where the user is looking, not part of the edit.
        restored.view = current.view;
        restored.open = true;
        if let Some(doc) = self.multi_edit.doc_mut(&id) {
            *doc = restored;
        }
        self.multi_edit.ui.clip_drag = None;
        self.multi_edit.ui.lane_drag = None;
        self.last_undo_scope = UndoScope::MultiEdit;
        self.multi_edit_touched();
        true
    }

    pub(super) fn multi_edit_undo(&mut self) -> bool {
        self.multi_edit_restore(false)
    }

    pub(super) fn multi_edit_redo(&mut self) -> bool {
        self.multi_edit_restore(true)
    }

    /// Clips for list rows, with what the list knows about each. A row with
    /// no known length yet is left out; `insert_clips` would skip it anyway.
    pub(super) fn multi_edit_new_clips(&self, paths: &[PathBuf]) -> Vec<NewClip> {
        paths
            .iter()
            .filter_map(|path| {
                let item = self.item_for_path(path)?;
                let len_secs = item
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.duration_secs)
                    .map(f64::from)
                    .or_else(|| {
                        let asset = &item.audio_asset;
                        asset
                            .frame_count
                            .filter(|_| asset.sample_rate > 0)
                            .map(|frames| frames as f64 / asset.sample_rate as f64)
                    })
                    .or_else(|| {
                        item.virtual_audio.as_ref().map(|audio| {
                            audio.len() as f64 / item.audio_asset.sample_rate.max(1) as f64
                        })
                    })?;
                let asset_id = (item.source == MediaSource::Virtual)
                    .then(|| item.audio_asset.id.to_hex());
                Some(NewClip {
                    source: ClipSource {
                        path: path.clone(),
                        asset_id,
                    },
                    name: item.display_name.clone(),
                    len_secs,
                    is_video: crate::media_kind::is_video_path(path),
                })
            })
            .collect()
    }

    /// Place list rows on the active timeline at `at_secs`: on `track` when
    /// it holds their kind, otherwise on a track that does (made if there is
    /// none). Video files go to a video track, everything else to an audio
    /// track, each group back to back. Returns how many clips were placed.
    pub(super) fn multi_edit_drop_paths(
        &mut self,
        track: Option<usize>,
        at_secs: f64,
        paths: &[PathBuf],
    ) -> usize {
        let clips = self.multi_edit_new_clips(paths);
        if clips.is_empty() {
            if !paths.is_empty() {
                self.push_toast(
                    ToastSeverity::Info,
                    "Nothing placed: the rows' lengths are not known yet",
                );
            }
            return 0;
        }
        let first_sr = paths
            .first()
            .map(|path| self.resolve_file_sample_rate(path))
            .filter(|rate| !rate.is_assumed())
            .map(|rate| rate.hz);
        self.multi_edit_checkpoint();
        let Some(doc) = self.multi_edit_active_doc_mut() else {
            return 0;
        };
        let (videos, audios): (Vec<NewClip>, Vec<NewClip>) =
            clips.into_iter().partition(|clip| clip.is_video);
        let mut placed = 0;
        for (kind, group) in [(TrackKind::Audio, audios), (TrackKind::Video, videos)] {
            if group.is_empty() {
                continue;
            }
            let target = match track {
                Some(idx) if doc.tracks.get(idx).is_some_and(|t| t.kind == kind) => idx,
                // Dropped on a track of the other kind: the first track that
                // takes these, or a new one.
                Some(_) => doc
                    .tracks
                    .iter()
                    .position(|t| t.kind == kind)
                    .unwrap_or_else(|| doc.add_track(kind)),
                // Dropped below the tracks: a new track, as asked.
                None => doc.add_track(kind),
            };
            placed += doc.insert_clips(target, at_secs, group).len();
        }
        if doc.timeline_sr == 0 {
            if let Some(sr) = first_sr {
                doc.timeline_sr = sr;
            }
        }
        self.multi_edit_touched();
        placed
    }

    /// The rows a drag from the list pane carries: the whole selection, in
    /// list order, when the dragged row is part of it; otherwise that row.
    /// Built once, when the drag starts.
    pub(super) fn multi_edit_drag_paths_for_row(&self, row: usize) -> Vec<PathBuf> {
        if self.selected_multi.contains(&row) && self.selected_multi.len() > 1 {
            return self
                .selected_multi
                .iter()
                .filter_map(|&r| self.path_for_row(r).cloned())
                .collect();
        }
        self.path_for_row(row).cloned().into_iter().collect()
    }

    /// Select a clip, and its row in the list with it.
    pub(super) fn multi_edit_select_clip(&mut self, clip_id: Option<String>) {
        let source = clip_id.as_deref().and_then(|id| {
            self.multi_edit_active_doc()
                .and_then(|doc| doc.clip(id))
                .map(|clip| clip.source.path.clone())
        });
        self.multi_edit.ui.selected_clip = clip_id;
        let Some(path) = source else {
            return;
        };
        if let Some(row) = self.row_for_path(&path) {
            self.selected = Some(row);
            self.selected_multi.clear();
            self.selected_multi.insert(row);
            self.select_anchor = Some(row);
            self.scroll_to_selected = true;
        }
    }

    fn multi_edit_source_stamp(&self, path: &Path) -> Option<SourceStamp> {
        let item = self.item_for_path(path)?;
        Some(SourceStamp {
            gain_bits: item.pending_gain_db.to_bits(),
            asset_revision: item.audio_asset.revision.0,
            edited: self.tabs.iter().any(|tab| tab.dirty && tab.path == path)
                || self.edited_cache.contains_key(path),
        })
    }

    /// What to read for a row: the samples the row plays now when the app
    /// already holds them, otherwise the file behind it.
    fn multi_edit_source_request(&self, path: &Path) -> Option<SourceRequest> {
        let item = self.item_for_path(path)?;
        let stamp = self.multi_edit_source_stamp(path)?;
        let input = if let Some(tab) = self.tabs.iter().find(|tab| tab.dirty && tab.path == path) {
            SourceInput::Samples {
                channels: tab.ch_samples_arc.clone(),
                sample_rate: tab.buffer_sample_rate,
            }
        } else if let Some(cached) = self.edited_cache.get(path) {
            SourceInput::Samples {
                channels: Arc::new(cached.ch_samples.clone()),
                sample_rate: cached.buffer_sample_rate,
            }
        } else if let Some(audio) = item.virtual_audio.as_ref() {
            SourceInput::Samples {
                channels: audio.channels.clone(),
                sample_rate: item.audio_asset.sample_rate,
            }
        } else {
            SourceInput::File(item.audio_asset.backing.file_path()?.to_path_buf())
        };
        Some(SourceRequest {
            path: path.to_path_buf(),
            input,
            gain_db: item.pending_gain_db,
            stamp,
        })
    }

    /// Ask for every source the active timeline needs and does not have, or
    /// has in a version the row no longer plays.
    fn multi_edit_request_sources(&mut self) {
        let Some(doc) = self.multi_edit_active_doc() else {
            return;
        };
        let mut requests = Vec::new();
        let mut missing = Vec::new();
        for source in doc.sources() {
            let stale = match self.multi_edit.sources.get(&source.path) {
                None => true,
                Some(SourceSlot::Loading) => false,
                Some(SourceSlot::Failed(_)) => false,
                Some(SourceSlot::NotInList) => self.item_for_path(&source.path).is_some(),
                Some(SourceSlot::Ready(loaded)) => {
                    self.multi_edit_source_stamp(&source.path) != Some(loaded.stamp)
                        && self.item_for_path(&source.path).is_some()
                }
            };
            if !stale {
                continue;
            }
            match self.multi_edit_source_request(&source.path) {
                Some(request) => requests.push(request),
                None => missing.push(source.path.clone()),
            }
        }
        for path in missing {
            self.multi_edit.sources.insert(path, SourceSlot::NotInList);
        }
        if requests.is_empty() {
            return;
        }
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let quality = Self::to_wave_resample_quality(self.src_quality);
        let pending: Vec<PathBuf> = requests.iter().map(|r| r.path.clone()).collect();
        for path in &pending {
            self.multi_edit.sources.insert(path.clone(), SourceSlot::Loading);
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            for request in requests {
                let path = request.path.clone();
                let result = load_source(request, out_sr, quality);
                if tx.send(SourceLoaded { path, result }).is_err() {
                    return;
                }
                crate::ui_wake::wake_ui();
            }
        });
        self.multi_edit.source_jobs.push(SourceJob { rx, pending });
    }

    fn multi_edit_drain_sources(&mut self) {
        let mut arrived = false;
        let mut jobs = std::mem::take(&mut self.multi_edit.source_jobs);
        jobs.retain_mut(|job| loop {
            match job.rx.try_recv() {
                Ok(loaded) => {
                    job.pending.retain(|path| path != &loaded.path);
                    let slot = match loaded.result {
                        Ok(source) => {
                            // A timeline's export rate comes from its first
                            // source when nothing reported it on the drop.
                            let native_sr = source.native.sample_rate;
                            for doc in &mut self.multi_edit.docs {
                                if doc.timeline_sr == 0
                                    && doc.sources().iter().any(|s| s.path == loaded.path)
                                {
                                    doc.timeline_sr = native_sr;
                                }
                            }
                            SourceSlot::Ready(Arc::new(source))
                        }
                        Err(err) => SourceSlot::Failed(err),
                    };
                    self.multi_edit.sources.insert(loaded.path, slot);
                    arrived = true;
                }
                Err(mpsc::TryRecvError::Empty) => break true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // The reader stopped without answering for these; say so
                    // on the clips rather than leaving them loading forever.
                    for path in job.pending.drain(..) {
                        self.multi_edit
                            .sources
                            .insert(path, SourceSlot::Failed("reader stopped".to_string()));
                        arrived = true;
                    }
                    break false;
                }
            }
        });
        self.multi_edit.source_jobs = jobs;
        if arrived {
            // New audio changes the mix even though the document did not.
            if let Some(id) = self.multi_edit.active.clone() {
                *self.multi_edit.revs.entry(id).or_default() += 1;
            }
            self.multi_edit.edited_at.get_or_insert_with(Instant::now);
        }
    }

    /// Start a playback mix of the active timeline when it is out of date,
    /// its sources are in, and edits have paused long enough.
    fn multi_edit_maybe_render(&mut self) {
        if self.multi_edit.render.is_some() {
            return;
        }
        let Some(doc) = self.multi_edit_active_doc() else {
            return;
        };
        let rev = self.multi_edit.rev(&doc.id);
        let current = self
            .multi_edit
            .mix
            .as_ref()
            .is_some_and(|mix| mix.doc_id == doc.id && mix.rev == rev);
        if current || self.multi_edit.doc_sources_loading(doc) {
            return;
        }
        let waited = self
            .multi_edit
            .edited_at
            .is_none_or(|at| at.elapsed() >= crate::app::ui_timing::MULTI_EDIT_RENDER_DEBOUNCE);
        let wanted_now = self.multi_edit.play_when_ready.as_deref() == Some(doc.id.as_str());
        if !waited && !wanted_now {
            return;
        }
        let mut sources = SourceMap::new();
        for source in doc.sources() {
            if let Some(loaded) = self.multi_edit.ready_source(&source.path) {
                sources.insert(source.path.clone(), loaded.playback.clone());
            }
        }
        let snapshot = doc.clone();
        let doc_id = doc.id.clone();
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            let mix = crate::app::multi_edit_render::render_timeline(
                &snapshot,
                &sources,
                out_sr,
                None,
                |_| {},
            )
            .unwrap_or_default();
            let _ = tx.send(Arc::new(AudioBuffer::from_channels(mix)));
            crate::ui_wake::wake_ui();
        });
        self.multi_edit.edited_at = None;
        self.multi_edit.render = Some(RenderJob { rx, doc_id, rev });
    }

    fn multi_edit_drain_render(&mut self) {
        let Some(job) = self.multi_edit.render.as_ref() else {
            return;
        };
        let audio = match poll_job(&job.rx) {
            JobPoll::Waiting => return,
            JobPoll::Ready(audio) => audio,
            JobPoll::Gone => {
                self.multi_edit.render = None;
                self.multi_edit.play_when_ready = None;
                self.push_toast(
                    ToastSeverity::Error,
                    "Multi Edit: mixing stopped without a result",
                );
                return;
            }
        };
        let Some(job) = self.multi_edit.render.take() else {
            return;
        };
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        if self.multi_edit_playback_is(&job.doc_id) {
            self.audio
                .set_samples_buffer_keep_time_pos(audio.clone(), out_sr, out_sr);
        }
        self.multi_edit.mix = Some(MixReady {
            doc_id: job.doc_id.clone(),
            rev: job.rev,
            audio,
        });
        if self.multi_edit.play_when_ready.as_deref() == Some(job.doc_id.as_str()) {
            self.multi_edit.play_when_ready = None;
            self.multi_edit_start_playback(&job.doc_id);
        }
    }

    pub(super) fn multi_edit_playback_is(&self, doc_id: &str) -> bool {
        matches!(&self.playback_session.source, PlaybackSourceKind::MultiEdit(id) if id == doc_id)
    }

    pub(super) fn multi_edit_is_playing(&self, doc_id: &str) -> bool {
        self.multi_edit_playback_is(doc_id) && self.audio.shared.playing.load(Ordering::Relaxed)
    }

    fn multi_edit_start_playback(&mut self, doc_id: &str) {
        let Some(mix) = self
            .multi_edit
            .mix
            .as_ref()
            .filter(|mix| mix.doc_id == doc_id)
            .map(|mix| mix.audio.clone())
        else {
            return;
        };
        if mix.is_empty() {
            self.push_toast(ToastSeverity::Info, "Nothing to play: the timeline is empty");
            return;
        }
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let len = mix.len();
        self.audio.stop();
        self.audio.set_loop_enabled(false);
        self.audio.set_samples_buffer(mix);
        self.playback_mark_buffer_source(PlaybackSourceKind::MultiEdit(doc_id.to_string()), out_sr);
        let mut frame = (self.multi_edit_playhead(doc_id) * out_sr as f64) as usize;
        if frame + 1 >= len {
            frame = 0;
        }
        self.audio.seek_to_sample(frame);
        self.audio.play();
    }

    /// Play or stop the active timeline.
    pub(super) fn multi_edit_toggle_play(&mut self) {
        let Some(id) = self.multi_edit.active.clone() else {
            return;
        };
        if self.multi_edit_is_playing(&id) {
            self.audio.stop();
            self.multi_edit_sync_playhead();
            return;
        }
        if self.multi_edit.play_when_ready.take().is_some() {
            return;
        }
        let rev = self.multi_edit.rev(&id);
        let fresh = self
            .multi_edit
            .mix
            .as_ref()
            .is_some_and(|mix| mix.doc_id == id && mix.rev == rev);
        if fresh {
            self.multi_edit_start_playback(&id);
        } else {
            self.multi_edit.play_when_ready = Some(id);
        }
    }

    pub(super) fn multi_edit_playhead(&self, doc_id: &str) -> f64 {
        self.multi_edit.playhead_secs.get(doc_id).copied().unwrap_or(0.0)
    }

    /// Follow the transport while it plays the active timeline.
    fn multi_edit_sync_playhead(&mut self) {
        let Some(id) = self.multi_edit.active.clone() else {
            return;
        };
        if !self.multi_edit_playback_is(&id) {
            return;
        }
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let pos = self.audio.shared.play_pos.load(Ordering::Relaxed);
        self.multi_edit
            .playhead_secs
            .insert(id, pos as f64 / out_sr as f64);
    }

    pub(super) fn multi_edit_seek(&mut self, secs: f64) {
        let Some(id) = self.multi_edit.active.clone() else {
            return;
        };
        let secs = secs.max(0.0);
        self.multi_edit.playhead_secs.insert(id.clone(), secs);
        if self.multi_edit_playback_is(&id) {
            let out_sr = self.audio.shared.out_sample_rate.max(1);
            self.audio.seek_to_sample((secs * out_sr as f64) as usize);
        }
    }

    /// Mix the active timeline at its own rate into a new virtual row.
    pub(super) fn multi_edit_start_export(&mut self) {
        if self.multi_edit.export.is_some() {
            return;
        }
        let Some(doc) = self.multi_edit_active_doc().cloned() else {
            return;
        };
        if doc.end_secs() <= 0.0 {
            self.push_toast(ToastSeverity::Info, "Nothing to export: the timeline is empty");
            return;
        }
        if self.multi_edit.doc_sources_loading(&doc) {
            self.push_toast(ToastSeverity::Info, "Still reading the timeline's audio");
            return;
        }
        let mut natives: Vec<(PathBuf, Arc<SourceAudio>)> = Vec::new();
        let mut unavailable = 0usize;
        for source in doc.sources() {
            match self.multi_edit.ready_source(&source.path) {
                Some(loaded) => natives.push((source.path.clone(), loaded.native.clone())),
                None => unavailable += 1,
            }
        }
        let timeline_sr = if doc.timeline_sr > 0 {
            doc.timeline_sr
        } else {
            natives
                .first()
                .map(|(_, source)| source.sample_rate)
                .unwrap_or(self.audio.shared.out_sample_rate.max(1))
        };
        if unavailable > 0 {
            self.push_toast(
                ToastSeverity::Warning,
                format!("{unavailable} clip source(s) could not be read and are left out"),
            );
        }
        let quality = Self::to_wave_resample_quality(self.src_quality);
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(0f32.to_bits()));
        let (tx, rx) = mpsc::channel();
        let (cancel_w, progress_w) = (cancel.clone(), progress.clone());
        let doc_id = doc.id.clone();
        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            let result = (|| {
                let mut sources = SourceMap::new();
                for (path, source) in natives {
                    if cancel_w.load(Ordering::Relaxed) {
                        return Err("cancelled".to_string());
                    }
                    let source = if source.sample_rate == timeline_sr {
                        source
                    } else {
                        Arc::new(SourceAudio {
                            channels: Arc::new(crate::wave::resample_channels_quality(
                                &source.channels,
                                source.sample_rate,
                                timeline_sr,
                                quality,
                            )),
                            sample_rate: timeline_sr,
                        })
                    };
                    sources.insert(path, source);
                }
                let mix = crate::app::multi_edit_render::render_timeline(
                    &doc,
                    &sources,
                    timeline_sr,
                    Some(&cancel_w),
                    |done| {
                        progress_w.store(done.to_bits(), Ordering::Relaxed);
                        crate::ui_wake::wake_ui();
                    },
                )
                .ok_or_else(|| "cancelled".to_string())?;
                let path = crate::app::temp_audio_ops::allocate_neowaves_temp_cache_path(
                    "multi_edit",
                    "wav",
                )
                .ok_or_else(|| "no temporary folder to write to".to_string())?;
                crate::wave::export_channels_audio_with_depth(
                    &mix,
                    timeline_sr,
                    &path,
                    Some(crate::wave::WavBitDepth::Float32),
                )
                .map_err(|err| err.to_string())?;
                Ok(path)
            })();
            let _ = tx.send(result);
            crate::ui_wake::wake_ui();
        });
        self.multi_edit.export = Some(ExportJob {
            rx,
            cancel,
            progress,
            doc_id,
        });
    }

    pub(super) fn multi_edit_cancel_export(&mut self) {
        if let Some(job) = self.multi_edit.export.as_ref() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn multi_edit_drain_export(&mut self) {
        let Some(job) = self.multi_edit.export.as_ref() else {
            return;
        };
        let result = match poll_job(&job.rx) {
            JobPoll::Waiting => return,
            JobPoll::Ready(result) => result,
            JobPoll::Gone => Err("the mixdown stopped without a result".to_string()),
        };
        let Some(job) = self.multi_edit.export.take() else {
            return;
        };
        match result {
            Ok(path) => {
                let name = self
                    .multi_edit
                    .doc(&job.doc_id)
                    .map(|doc| format!("{}.wav", doc.name))
                    .unwrap_or_else(|| "Multi Edit.wav".to_string());
                let row = self.add_file_backed_virtual_row(&path, &name);
                if let Some(idx) = self.row_for_path(&row) {
                    self.selected = Some(idx);
                    self.selected_multi.clear();
                    self.selected_multi.insert(idx);
                    self.select_anchor = Some(idx);
                    self.scroll_to_selected = true;
                }
                let shown = self
                    .item_for_path(&row)
                    .map(|item| item.display_name.clone())
                    .unwrap_or(name);
                self.push_toast(
                    ToastSeverity::Info,
                    format!("Mixed down to \"{shown}\" (virtual row in the list)"),
                );
            }
            Err(err) if err == "cancelled" => {
                self.push_toast(ToastSeverity::Info, "Mixdown cancelled");
            }
            Err(err) => {
                self.push_toast(ToastSeverity::Error, format!("Mixdown failed: {err}"));
            }
        }
    }

    /// The video under the playhead: the topmost video track's clip there,
    /// as (source file, seconds into it).
    pub(super) fn multi_edit_video_at_playhead(&self) -> Option<(PathBuf, f64)> {
        let doc = self.multi_edit_active_doc()?;
        let playhead = self.multi_edit_playhead(&doc.id);
        doc.tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video && doc.track_audible(track))
            .find_map(|track| {
                track
                    .clips
                    .iter()
                    .find(|clip| clip.start_secs <= playhead && playhead < clip.end_secs())
                    .map(|clip| {
                        (
                            clip.source.path.clone(),
                            clip.in_secs + (playhead - clip.start_secs),
                        )
                    })
            })
    }

    /// The preview panel for a video file, made on first use. Every other
    /// panel is dropped: one picture is shown, so one file is kept open.
    pub(super) fn multi_edit_video_panel(&mut self, path: &Path) -> &mut MultiEditVideo {
        self.multi_edit.video_panels.retain(|p, _| p == path);
        if !self.multi_edit.video_panels.contains_key(path) {
            let id = MULTI_EDIT_VIDEO_ID_BASE + self.multi_edit.next_video_id;
            self.multi_edit.next_video_id += 1;
            self.multi_edit.video_panels.insert(
                path.to_path_buf(),
                MultiEditVideo {
                    id,
                    panel: super::types::VideoPanelState::new(
                        super::video_ops::placeholder_stream_info(),
                    ),
                },
            );
        }
        self.multi_edit
            .video_panels
            .get_mut(path)
            .expect("inserted above")
    }

    /// Ask the preview panel's worker for the picture at `secs` of `path`.
    pub(super) fn multi_edit_request_video(&mut self, path: &Path, secs: f64, playing: bool) {
        let perf = self.perf;
        let Some(video) = self.multi_edit.video_panels.get_mut(path) else {
            return;
        };
        let id = video.id;
        if let Some(request) =
            super::video_ops::prepare_video_request(&mut video.panel, perf, secs, playing)
        {
            self.send_video_request(id, request);
        }
    }

    /// Per-frame upkeep: sources in, mixes out, the playhead followed.
    pub(super) fn tick_multi_edit(&mut self, ctx: &egui::Context) {
        if !self.is_multi_edit_workspace_active() && !self.multi_edit.video_panels.is_empty() {
            // Off screen: close the file; the workers go with their panels.
            self.multi_edit.video_panels.clear();
        }
        if self.multi_edit.docs.is_empty() && self.multi_edit.export.is_none() {
            return;
        }
        self.multi_edit_drain_sources();
        if self.is_multi_edit_workspace_active() || self.multi_edit.play_when_ready.is_some() {
            self.multi_edit_request_sources();
            self.multi_edit_maybe_render();
        }
        self.multi_edit_drain_render();
        self.multi_edit_drain_export();
        self.multi_edit_sync_playhead();
        let busy = !self.multi_edit.source_jobs.is_empty()
            || self.multi_edit.render.is_some()
            || self.multi_edit.export.is_some()
            || self.multi_edit.edited_at.is_some();
        let playing = self
            .multi_edit
            .active
            .as_deref()
            .is_some_and(|id| self.multi_edit_is_playing(id));
        if playing && self.is_multi_edit_workspace_active() {
            ctx.request_repaint_after(crate::app::ui_timing::ANIMATION_FRAME);
        } else if busy {
            ctx.request_repaint_after(crate::app::ui_timing::PROGRESS_REFRESH);
        }
    }

    /// The timelines as a session stores them: file paths in the session's
    /// path form, virtual rows kept by label and asset id.
    pub(super) fn multi_edit_docs_for_session(
        &self,
        base_dir: &Path,
        path_mode: super::project::SessionPathMode,
    ) -> Vec<MultiEditDoc> {
        let mut docs = self.multi_edit.docs.clone();
        for clip in docs
            .iter_mut()
            .flat_map(|doc| doc.tracks.iter_mut())
            .flat_map(|track| track.clips.iter_mut())
        {
            if clip.source.asset_id.is_none() {
                clip.source.path =
                    PathBuf::from(super::project::session_path(&clip.source.path, base_dir, path_mode));
            }
        }
        docs
    }

    /// Adopt a session's timelines, resolving each clip back to a row: a file
    /// by its path, a virtual row by its asset id.
    pub(super) fn multi_edit_load_from_session(&mut self, mut docs: Vec<MultiEditDoc>, base_dir: &Path) {
        self.multi_edit_reset();
        let virtual_by_asset: HashMap<String, PathBuf> = self
            .items
            .iter()
            .filter(|item| item.source == MediaSource::Virtual)
            .map(|item| (item.audio_asset.id.to_hex(), item.path.clone()))
            .collect();
        for clip in docs
            .iter_mut()
            .flat_map(|doc| doc.tracks.iter_mut())
            .flat_map(|track| track.clips.iter_mut())
        {
            match clip.source.asset_id.as_ref() {
                Some(asset) => {
                    if let Some(path) = virtual_by_asset.get(asset) {
                        clip.source.path = path.clone();
                    }
                }
                None => {
                    let raw = clip.source.path.to_string_lossy().to_string();
                    clip.source.path = super::project::resolve_path(&raw, base_dir);
                }
            }
        }
        self.multi_edit.active = docs.iter().find(|doc| doc.open).map(|doc| doc.id.clone());
        self.multi_edit.docs = docs;
    }
}
