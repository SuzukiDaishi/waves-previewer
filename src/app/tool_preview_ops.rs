//! The editor's tool preview: settings in, the processed audio out, swapped
//! in under the running playhead.
//!
//! One engine for every rendered tool. Each frame it asks what the active
//! tab's settings describe ([`PreviewKey`]); when that differs from the
//! preview it holds and the settings have settled for
//! [`TOOL_PREVIEW_DEBOUNCE`], it renders the op on a worker with the very
//! function Apply uses (`tool_render::render`). A newer request cancels an
//! older one; a result whose key is no longer wanted is dropped.
//!
//! Spawning is deferrable (it is a request, not something the user waits on
//! this frame); installing a finished preview is not -- the user is
//! listening for it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use super::editor_tool::PreviewKind;
use super::loading_ops::{poll_job, JobPoll};
use super::tool_render::{self, RenderError, ToolRenderOutput, ToolRenderRequest};
use super::tool_render::RenderTarget;
use super::tool_session::{
    mask_bits, AnalysisKey, AnalysisKind, PreviewKey, SessionPhase, SourceAnalysis,
    ToolPreviewResult, ToolSession,
};
use super::types::PreviewOverlay;
use super::{PlaybackSourceKind, WavesPreviewer, LIVE_PREVIEW_SAMPLE_LIMIT};

/// How long settings must rest before the preview follows them. While a
/// drag goes on, the preview is re-rendered at this interval instead, so the
/// change is heard as it happens without rendering on every frame.
pub const TOOL_PREVIEW_DEBOUNCE: Duration = Duration::from_millis(200);

/// The output fades back in over this long after a preview is swapped in
/// under a running playhead: two different signals meet there, and without
/// the fade the jump clicks.
pub const PREVIEW_SWAP_FADE_MS: f32 = 6.0;

/// A finished render on its way back from the worker.
pub(super) struct ToolPreviewReady {
    pub output: ToolRenderOutput,
    pub channels: Arc<Vec<Vec<f32>>>,
    pub overlay: PreviewOverlay,
}

pub(super) struct ToolPreviewJob {
    pub key: PreviewKey,
    pub rx: mpsc::Receiver<Result<ToolPreviewReady, Option<String>>>,
    pub cancel: Arc<AtomicBool>,
    pub started: Instant,
}

/// A source analysis on its way back from the worker.
pub(super) struct SourceAnalysisJob {
    pub key: AnalysisKey,
    pub rx: mpsc::Receiver<SourceAnalysis>,
    pub cancel: Arc<AtomicBool>,
}

/// The engine's state: at most one render and one analysis in flight.
#[derive(Default)]
pub(super) struct ToolPreviewEngine {
    pub job: Option<ToolPreviewJob>,
    pub last_spawn: Option<Instant>,
    pub analysis_job: Option<SourceAnalysisJob>,
}

/// How long the live meters integrate a peak over.
const LIVE_LEVEL_WINDOW_SECS: f64 = 0.05;
/// How fast a live meter falls once the level drops, dB per second.
const LIVE_FALL_DB_PER_SEC: f32 = 24.0;
/// The floor of the live meters, dBFS.
const LIVE_FLOOR_DB: f32 = -90.0;

impl WavesPreviewer {
    /// Whether the active tool of `tab_idx` is previewed by rendering, in
    /// the view it is shown in.
    pub(super) fn tool_preview_applies(&self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let spec = tab.active_tool.spec();
        if spec.preview != PreviewKind::Rendered || tab.read_only || tab.paged_asset {
            return false;
        }
        // Spectral tools are heard from the spectrogram, where the waveform
        // overlay is optional; everything else needs a view that draws it.
        spec.category.is_spectral()
            || Self::view_supports_wave_preview(tab.leaf_view_mode(), tab.show_waveform_overlay)
    }

    /// What the active tool's settings describe on `tab_idx` now, as the key
    /// its preview would carry and the request that renders it. `None` when
    /// there is nothing to hear: neutral settings, or a tool that lacks what
    /// it needs.
    pub(super) fn tool_preview_target(
        &self,
        tab_idx: usize,
    ) -> Option<(PreviewKey, ToolRenderRequest)> {
        let tab = self.tabs.get(tab_idx)?;
        if tab.loading || tab.samples_len == 0 {
            return None;
        }
        let tool = tab.active_tool;
        let op = Self::tool_op_for_tab(tab, tool)?;
        if op.is_neutral() {
            return None;
        }
        let target = self.tool_target_for_tab(tab_idx, tool);
        let req = self.tool_request_for_tab(tab_idx, op, target)?;
        let key = PreviewKey::new(tab, &req.op, req.target, req.ch_mask.as_deref());
        Some((key, req))
    }

    /// Ask for a preview of the current settings now, without waiting for
    /// them to settle (the Preview button, Refresh, a test).
    pub(super) fn tool_preview_request_now(&mut self, tab_idx: usize) {
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.tool_session.phase = SessionPhase::Previewing;
            tab.tool_session.last_change_at = None;
            tab.tool_session.error = None;
            tab.tool_session.ab_original = false;
        }
        // Asked for now, so started now: the render is in flight before the
        // next frame, and a Play pressed in between waits for it.
        if let Some((key, req)) = self.tool_preview_target(tab_idx) {
            let fresh = self
                .tabs
                .get(tab_idx)
                .is_some_and(|tab| tab.tool_session.fresh_preview(&key).is_some());
            let in_flight = self
                .tool_preview
                .job
                .as_ref()
                .is_some_and(|job| job.key == key);
            if !fresh && !in_flight {
                self.spawn_tool_preview(key, req);
            }
        }
    }

    /// Whether a preview render for `tab_idx` is in flight.
    pub(super) fn tool_preview_busy_for_tab(&self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        self.tool_preview
            .job
            .as_ref()
            .is_some_and(|job| job.key.tab_id == tab.tab_id)
    }

    fn spawn_tool_preview(&mut self, key: PreviewKey, req: ToolRenderRequest) {
        if let Some(job) = self.tool_preview.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        let tool = key.tool;
        let long = req.channels.first().map_or(0, Vec::len) > LIVE_PREVIEW_SAMPLE_LIMIT;
        std::thread::spawn(move || {
            let result = match tool_render::render(&req, &worker_cancel) {
                Ok(mut output) => {
                    let channels = std::mem::take(&mut output.channels);
                    let timeline_len = channels.first().map(Vec::len).unwrap_or(0);
                    // Long clips draw an overview: a full-sample overlay would
                    // be another clip-sized copy for detail nobody can see
                    // zoomed out.
                    let overlay = if long {
                        WavesPreviewer::preview_overlay_from_overview(
                            WavesPreviewer::build_overview_bins_from_channels(&channels),
                            tool,
                            timeline_len,
                        )
                    } else {
                        WavesPreviewer::preview_overlay_from_channels(
                            channels.clone(),
                            tool,
                            timeline_len,
                        )
                    };
                    Ok(ToolPreviewReady {
                        output,
                        channels: Arc::new(channels),
                        overlay,
                    })
                }
                Err(RenderError::Invalid(msg)) => Err(Some(msg)),
                Err(RenderError::NothingToDo | RenderError::Cancelled) => Err(None),
            };
            let _ = tx.send(result);
            crate::ui_wake::wake_ui();
        });
        // The tab's preview belongs to this tool from now on: Play waits for
        // the render rather than auditioning the unprocessed audio.
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.tab_id == key.tab_id) {
            tab.preview_audio_tool = Some(tool);
        }
        self.tool_preview.job = Some(ToolPreviewJob {
            key,
            rx,
            cancel,
            started: Instant::now(),
        });
        self.tool_preview.last_spawn = Some(Instant::now());
    }

    /// Start a render when the active tab's settings call for one. Deferrable.
    pub(super) fn tick_tool_preview_spawn(&mut self, ctx: &egui::Context) {
        if !self.is_editor_workspace_active() {
            return;
        }
        let Some(tab_idx) = self.active_tab else {
            return;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.tool_session.phase != SessionPhase::Previewing || !self.tool_preview_applies(tab_idx)
        {
            return;
        }
        let Some((key, req)) = self.tool_preview_target(tab_idx) else {
            // Back to settings that change nothing: hear the original again.
            if self
                .tabs
                .get(tab_idx)
                .is_some_and(|tab| tab.tool_session.preview.is_some())
            {
                self.uninstall_tool_preview(tab_idx);
            }
            return;
        };
        let session = &self.tabs[tab_idx].tool_session;
        if session.fresh_preview(&key).is_some()
            || self
                .tool_preview
                .job
                .as_ref()
                .is_some_and(|job| job.key == key)
            || session.manual
        {
            return;
        }
        let now = Instant::now();
        let since_change = session
            .last_change_at
            .map_or(Duration::MAX, |at| now.saturating_duration_since(at));
        let since_spawn = self
            .tool_preview
            .last_spawn
            .map_or(Duration::MAX, |at| now.saturating_duration_since(at));
        if since_change < TOOL_PREVIEW_DEBOUNCE && since_spawn < TOOL_PREVIEW_DEBOUNCE {
            let wait = TOOL_PREVIEW_DEBOUNCE.saturating_sub(since_change.min(since_spawn));
            ctx.request_repaint_after(wait);
            return;
        }
        self.spawn_tool_preview(key, req);
    }

    /// Take a finished render. Not deferrable: the user is listening for it.
    pub(super) fn drain_tool_preview_jobs(&mut self) {
        let Some(job) = self.tool_preview.job.as_ref() else {
            return;
        };
        let polled = poll_job(&job.rx);
        let key = job.key;
        match polled {
            JobPoll::Waiting => {}
            JobPoll::Gone => {
                // The worker died without answering; the next tick starts over.
                self.tool_preview.job = None;
                self.debug_log("tool preview worker stopped without a result".to_string());
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.tab_id == key.tab_id) {
                    tab.tool_session.error = Some("The preview stopped unexpectedly".into());
                }
            }
            JobPoll::Ready(result) => {
                self.tool_preview.job = None;
                let Some(tab_idx) = self.tabs.iter().position(|t| t.tab_id == key.tab_id) else {
                    return;
                };
                match result {
                    Ok(ready) => {
                        // Only a preview of what the settings say now is kept.
                        let wanted = self
                            .tool_preview_target(tab_idx)
                            .is_some_and(|(want, _)| want == key);
                        if wanted {
                            self.install_tool_preview(tab_idx, key, ready);
                        }
                    }
                    Err(why) => {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.tool_session.error = why;
                        }
                    }
                }
            }
        }
    }

    /// What the active tool would measure of its source now, if it draws one.
    fn source_analysis_target(
        &self,
        tab_idx: usize,
    ) -> Option<(AnalysisKey, Arc<Vec<Vec<f32>>>, (usize, usize), Option<Vec<bool>>, u32)> {
        let tab = self.tabs.get(tab_idx)?;
        if tab.loading || tab.samples_len == 0 || tab.ch_samples.is_empty() {
            return None;
        }
        let tool = tab.active_tool;
        let kind = AnalysisKind::for_tool(tool)?;
        let target = self.tool_target_for_tab(tab_idx, tool);
        let range = match target {
            RenderTarget::Whole => (0, tab.samples_len),
            RenderTarget::Range(a, b) => (a.min(b), a.max(b).min(tab.samples_len)),
            RenderTarget::InsertAt(_) => return None,
        };
        let mask = Self::tool_channel_mask(tab, tool);
        let key = AnalysisKey {
            tab_id: tab.tab_id,
            audio_rev: tab.audio_rev,
            kind,
            target,
            mask_bits: mask_bits(mask.as_deref()),
        };
        Some((key, tab.ch_samples_arc.clone(), range, mask, tab.buffer_sample_rate.max(1)))
    }

    /// Measure the active tool's source when its target changed. Deferrable.
    pub(super) fn tick_source_analysis(&mut self) {
        if !self.is_editor_workspace_active() {
            return;
        }
        let Some(tab_idx) = self.active_tab else {
            return;
        };
        let Some((key, channels, (s, e), mask, sr)) = self.source_analysis_target(tab_idx) else {
            return;
        };
        let held = self.tabs[tab_idx]
            .tool_session
            .source_analysis
            .as_ref()
            .is_some_and(|a| a.key == key);
        let running = self
            .tool_preview
            .analysis_job
            .as_ref()
            .is_some_and(|job| job.key == key);
        if held || running {
            return;
        }
        if let Some(old) = self.tool_preview.analysis_job.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            let scope: Vec<&[f32]> = channels
                .iter()
                .enumerate()
                .filter(|(ci, _)| mask.as_ref().map_or(true, |m| m.get(*ci).copied().unwrap_or(true)))
                .map(|(_, c)| c.as_slice())
                .collect();
            if worker_cancel.load(Ordering::Relaxed) {
                return;
            }
            let analysis = match key.kind {
                AnalysisKind::Spectrum => SourceAnalysis {
                    key,
                    spectrum: Some(crate::app::render::mini_meter::average_spectrum(&scope, s, e, sr)),
                    levels: None,
                },
                AnalysisKind::Levels => {
                    let ranged: Vec<&[f32]> = scope.iter().map(|c| &c[s.min(c.len())..e.min(c.len())]).collect();
                    let mut levels = crate::dynamics::level_trace(&ranged, sr);
                    levels.start = s;
                    SourceAnalysis {
                        key,
                        spectrum: None,
                        levels: Some(levels),
                    }
                }
            };
            let _ = tx.send(analysis);
            crate::ui_wake::wake_ui();
        });
        self.tool_preview.analysis_job = Some(SourceAnalysisJob { key, rx, cancel });
    }

    /// Take a finished source analysis. Cheap; every frame.
    pub(super) fn drain_source_analysis(&mut self) {
        let Some(job) = self.tool_preview.analysis_job.as_ref() else {
            return;
        };
        match poll_job(&job.rx) {
            JobPoll::Waiting => {}
            JobPoll::Gone => {
                self.tool_preview.analysis_job = None;
                self.debug_log("tool source analysis stopped without a result".to_string());
            }
            JobPoll::Ready(analysis) => {
                self.tool_preview.analysis_job = None;
                let Some(tab_idx) = self.tabs.iter().position(|t| t.tab_id == analysis.key.tab_id) else {
                    return;
                };
                let wanted = self
                    .source_analysis_target(tab_idx)
                    .is_some_and(|(key, ..)| key == analysis.key);
                if wanted {
                    self.tabs[tab_idx].tool_session.source_analysis = Some(Arc::new(analysis));
                }
            }
        }
    }

    /// Refresh what the tool panel shows while the tab plays: the heard
    /// spectrum (EQ), and in / out / gain at the playhead (dynamics). Reads
    /// a window of samples around the playhead; called by the panel.
    pub(super) fn update_tool_live_meters(&mut self, tab_idx: usize, now: f64) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let tool = tab.active_tool;
        let Some(kind) = AnalysisKind::for_tool(tool) else {
            return;
        };
        let playing = self.playback_is_playing_now() && self.tool_preview_tab_is_audible(tab_idx);
        let dt = (now - tab.tool_session.live.at).clamp(0.0, 0.25) as f32;
        if !playing {
            let live = &mut self.tabs[tab_idx].tool_session.live;
            live.playing = false;
            live.at = now;
            live.source_pos = None;
            let fall = LIVE_FALL_DB_PER_SEC * 4.0 * dt;
            live.in_db = (live.in_db - fall).max(LIVE_FLOOR_DB);
            live.out_db = (live.out_db - fall).max(LIVE_FLOOR_DB);
            live.gain_db = (live.gain_db + fall).min(0.0);
            live.spectrum_db.clear();
            return;
        }
        let previewing = matches!(self.playback_session.source, PlaybackSourceKind::ToolPreview);
        let source_pos = self.tool_preview_source_pos(tab_idx).max(0.0) as usize;
        let sr = tab.buffer_sample_rate.max(1);
        let level_n = crate::sample_rate::frames_for_secs(LIVE_LEVEL_WINDOW_SECS, sr).max(1);
        let peak_before = |chans: &[Vec<f32>], end: usize, n: usize| -> f32 {
            let start = end.saturating_sub(n);
            chans
                .iter()
                .flat_map(|c| c[start.min(c.len())..end.min(c.len())].iter())
                .fold(0.0f32, |m, v| m.max(v.abs()))
        };
        // What is heard: the preview buffer while it plays, else the tab.
        let engine_audio = previewing.then(|| self.audio.shared.samples.load_full()).flatten();
        let heard_pos = if previewing {
            self.audio.shared.play_pos.load(Ordering::Relaxed)
        } else {
            source_pos
        };
        let heard: &[Vec<f32>] = match engine_audio.as_deref() {
            Some(audio) => audio.channels.as_slice(),
            None => tab.ch_samples.as_slice(),
        };
        let to_db = |amp: f32| if amp > 0.0 { (20.0 * amp.log10()).max(LIVE_FLOOR_DB) } else { LIVE_FLOOR_DB };
        let in_db = to_db(peak_before(&tab.ch_samples, source_pos, level_n));
        let out_db = to_db(peak_before(heard, heard_pos, level_n));
        let gain_db = tab
            .tool_session
            .preview
            .as_ref()
            .and_then(|p| match p.output.analysis.as_deref() {
                Some(super::tool_render::RenderAnalysis::Dynamics(trace)) if !tab.tool_session.ab_original => {
                    trace.gain_db_at(source_pos)
                }
                _ => None,
            })
            .unwrap_or(0.0);
        let mut spectrum = None;
        let interval = self.perf.mini_meter_spectrum_interval().as_secs_f64();
        if kind == AnalysisKind::Spectrum && now - tab.tool_session.live.spectrum_at >= interval {
            use crate::app::render::mini_meter;
            let n = mini_meter::spectrum_history_len(sr);
            let end = heard_pos.min(heard.first().map_or(0, Vec::len));
            let start = end.saturating_sub(n);
            let inv = 1.0 / heard.len().max(1) as f32;
            let mono: Vec<f32> = (start..end)
                .map(|i| heard.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() * inv)
                .collect();
            let mut columns = Vec::new();
            mini_meter::spectrum_columns_in(
                &mono,
                sr,
                mini_meter::ANALYZER_COLUMNS,
                crate::sample_rate::AUDIBLE_LOW_HZ,
                crate::sample_rate::AUDIBLE_HIGH_HZ,
                &mut columns,
            );
            spectrum = Some(columns);
        }
        let live = &mut self.tabs[tab_idx].tool_session.live;
        let fall = LIVE_FALL_DB_PER_SEC * dt;
        live.playing = true;
        live.at = now;
        live.source_pos = Some(source_pos);
        live.in_db = in_db.max(live.in_db - fall);
        live.out_db = out_db.max(live.out_db - fall);
        live.gain_db = gain_db;
        if let Some(columns) = spectrum {
            let since = (now - live.spectrum_at).clamp(0.0, 0.25) as f32;
            crate::app::render::mini_meter::smooth_spectrum_db(&mut live.spectrum_db, &columns, since);
            live.spectrum_at = now;
        }
    }

    /// Whether `tab_idx` is what the engine is playing (its own audio or its
    /// preview).
    pub(super) fn tool_preview_tab_is_audible(&self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        match &self.playback_session.source {
            PlaybackSourceKind::EditorTab(path) => *path == tab.path,
            PlaybackSourceKind::ToolPreview => self.active_tab == Some(tab_idx),
            _ => false,
        }
    }

    /// The engine's position, in samples of the tab's own buffer.
    pub(super) fn tool_preview_source_pos(&self, tab_idx: usize) -> f64 {
        let pos_f = self.audio.shared.play_pos_f.load(Ordering::Relaxed);
        let Some(tab) = self.tabs.get(tab_idx) else {
            return pos_f;
        };
        match &self.playback_session.source {
            PlaybackSourceKind::ToolPreview => tab
                .tool_session
                .preview
                .as_ref()
                .map_or(pos_f, |p| p.output.time_map.to_source(pos_f)),
            _ => {
                // An exact-stream transport plays the file at its own rate.
                let transport_sr = self.playback_session.transport_sr.max(1) as f64;
                let buffer_sr = tab.buffer_sample_rate.max(1) as f64;
                pos_f * buffer_sr / transport_sr
            }
        }
    }

    fn install_tool_preview(&mut self, tab_idx: usize, key: PreviewKey, ready: ToolPreviewReady) {
        let ToolPreviewReady {
            output,
            channels,
            overlay,
        } = ready;
        let audible = self.tool_preview_tab_is_audible(tab_idx);
        let src_pos = audible.then(|| self.tool_preview_source_pos(tab_idx));
        let was_playing = self.playback_is_playing_now();
        let buffer_sr = key.buffer_sr;
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let tool = key.tool;
        let new_pos = src_pos.map(|src| output.time_map.to_output(src));
        let audio = Arc::new(crate::audio::AudioBuffer::from_shared_channels(
            channels.clone(),
        ));
        tab.preview_overlay = Some(overlay);
        tab.preview_audio_tool = Some(tool);
        tab.preview_audio_buffer = Some((tool, audio.clone()));
        tab.tool_session.preview = Some(ToolPreviewResult {
            key,
            output,
            channels,
        });
        tab.tool_session.error = None;
        let ab_original = tab.tool_session.ab_original;
        let path = tab.path.clone();
        if let Some(new_pos) = new_pos.filter(|_| !ab_original) {
            if was_playing && self.playback_mode_needs_fx_buffer() {
                // A pitch/stretch playback mode plays a rendered copy of the
                // source; there is no seamless swap into one. Stop, and the
                // next Play renders the preview through it.
                self.audio.stop();
            }
            let fade = if was_playing {
                PREVIEW_SWAP_FADE_MS
            } else {
                0.0
            };
            self.audio.set_samples_buffer_at(audio, new_pos, fade);
            self.playback_mark_buffer_source(PlaybackSourceKind::ToolPreview, buffer_sr);
            if let Some(tab) = self.tabs.get(tab_idx) {
                self.apply_loop_mode_for_tab(tab);
            }
            self.apply_effective_volume();
        }
        self.finish_pending_preview_autoplay(tab_idx, &path, tool);
    }

    /// Stop hearing the preview: the tab's own audio comes back at the same
    /// place in it, still playing if it was.
    pub(super) fn uninstall_tool_preview(&mut self, tab_idx: usize) {
        let playing_preview = matches!(
            self.playback_session.source,
            PlaybackSourceKind::ToolPreview
        ) && self.active_tab == Some(tab_idx);
        if playing_preview {
            let src_pos = self.tool_preview_source_pos(tab_idx);
            let was_playing = self.playback_is_playing_now();
            if let Some((path, buffer_sr, channels)) = self.tabs.get(tab_idx).map(|tab| {
                (
                    tab.path.clone(),
                    tab.buffer_sample_rate.max(1),
                    tab.ch_samples_arc.clone(),
                )
            }) {
                let fade = if was_playing {
                    PREVIEW_SWAP_FADE_MS
                } else {
                    0.0
                };
                self.audio.set_samples_buffer_at(
                    Arc::new(crate::audio::AudioBuffer::from_shared_channels(channels)),
                    src_pos,
                    fade,
                );
                self.playback_mark_buffer_source(PlaybackSourceKind::EditorTab(path), buffer_sr);
                if let Some(tab) = self.tabs.get(tab_idx) {
                    self.apply_loop_mode_for_tab(tab);
                }
                self.apply_effective_volume();
            }
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.tool_session.preview = None;
            tab.tool_session.ab_original = false;
            if tab
                .preview_audio_tool
                .is_some_and(|tool| !tool.previews_live())
            {
                tab.preview_audio_tool = None;
                tab.preview_audio_buffer = None;
                tab.preview_overlay = None;
            }
        }
    }

    /// Drop the render in flight for `tab_idx`, if any.
    pub(super) fn cancel_tool_preview_for_tab(&mut self, tab_idx: usize) {
        let Some(tab_id) = self.tabs.get(tab_idx).map(|tab| tab.tab_id) else {
            return;
        };
        if self
            .tool_preview
            .job
            .as_ref()
            .is_some_and(|job| job.key.tab_id == tab_id)
        {
            if let Some(job) = self.tool_preview.job.take() {
                job.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    /// A new session for the tab's active tool: nothing previewed, a fresh
    /// draft history. Heavy tools start manual on long clips.
    pub(super) fn reset_tool_session(&mut self, tab_idx: usize) {
        self.cancel_tool_preview_for_tab(tab_idx);
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            let manual =
                tab.active_tool.spec().heavy && tab.samples_len > LIVE_PREVIEW_SAMPLE_LIMIT;
            tab.tool_session = ToolSession::started(tab, manual);
        }
    }

    /// Esc: the settings go back to where the session started and the
    /// preview goes away. Returns whether anything was discarded.
    pub(super) fn discard_tool_draft(&mut self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let had_something = tab.tool_session.preview.is_some()
            || tab.tool_session.phase == SessionPhase::Previewing
            || tab.tool_session.has_draft_history(false);
        if !had_something {
            return false;
        }
        self.cancel_tool_preview_for_tab(tab_idx);
        self.uninstall_tool_preview(tab_idx);
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            if let Some(base) = tab.tool_session.discard() {
                base.restore(tab);
            }
        }
        true
    }

    /// Ctrl+Z / Ctrl+Y on the draft. Returns whether a step was taken.
    pub(super) fn tool_draft_undo_redo(&mut self, tab_idx: usize, redo: bool) -> bool {
        let now = Instant::now();
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        if !tab.tool_session.has_draft_history(redo) {
            return false;
        }
        let current = super::tool_session::DraftSnapshot::capture(tab);
        let target = if redo {
            tab.tool_session.redo(current, now)
        } else {
            tab.tool_session.undo(current, now)
        };
        match target {
            Some(snapshot) => {
                snapshot.restore(tab);
                true
            }
            None => false,
        }
    }

    /// A/B: hear the original or the preview, at the same place.
    pub(super) fn toggle_tool_preview_ab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let Some(preview) = tab.tool_session.preview.as_ref() else {
            return;
        };
        let to_original = !tab.tool_session.ab_original;
        let channels = preview.channels.clone();
        let time_map = preview.output.time_map;
        let buffer_sr = tab.buffer_sample_rate.max(1);
        let path = tab.path.clone();
        let original = tab.ch_samples_arc.clone();
        let audible = self.tool_preview_tab_is_audible(tab_idx);
        let was_playing = self.playback_is_playing_now();
        let fade = if was_playing {
            PREVIEW_SWAP_FADE_MS
        } else {
            0.0
        };
        if audible {
            let src_pos = self.tool_preview_source_pos(tab_idx);
            if to_original {
                self.audio.set_samples_buffer_at(
                    Arc::new(crate::audio::AudioBuffer::from_shared_channels(original)),
                    src_pos,
                    fade,
                );
                self.playback_mark_buffer_source(PlaybackSourceKind::EditorTab(path), buffer_sr);
            } else {
                self.audio.set_samples_buffer_at(
                    Arc::new(crate::audio::AudioBuffer::from_shared_channels(channels)),
                    time_map.to_output(src_pos),
                    fade,
                );
                self.playback_mark_buffer_source(PlaybackSourceKind::ToolPreview, buffer_sr);
            }
            if let Some(tab) = self.tabs.get(tab_idx) {
                self.apply_loop_mode_for_tab(tab);
            }
            self.apply_effective_volume();
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.tool_session.ab_original = to_original;
        }
    }

    /// The preview Apply can commit as it is: rendered from this tab's
    /// current audio with exactly these settings.
    pub(super) fn tool_preview_for_request(
        &self,
        tab_idx: usize,
        req: &ToolRenderRequest,
    ) -> Option<ToolRenderOutput> {
        let tab = self.tabs.get(tab_idx)?;
        let key = PreviewKey::new(tab, &req.op, req.target, req.ch_mask.as_deref());
        tab.tool_session
            .fresh_preview(&key)
            .map(ToolPreviewResult::to_output)
    }
}
