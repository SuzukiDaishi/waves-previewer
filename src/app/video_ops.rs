//! Feeding the editor's video panel: one decode worker per open video tab.
//!
//! Follows the shape `editor_viewport.rs` already uses for background renders
//! — a request channel out, a generation-stamped result channel back, and a
//! per-frame drain on the UI thread — with one difference that matters for
//! synchronisation. A viewport render is a one-off: ask, wait, draw. A video
//! panel has to be *on* the playhead, and a request/response round trip per
//! frame puts it one or two frames behind. So while the transport is running
//! the worker decodes ahead of the playhead and the UI picks the frame whose
//! time has just arrived out of what is already in hand.
//!
//! Nothing here touches the filesystem on the UI thread: opening the file,
//! parsing its header (which on a non-faststart movie seeks to the end) and
//! every decode happen on the worker.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::types::{VideoPanelState, VideoPanelStatus};
use super::WavesPreviewer;

const VIDEO_RESIZE_DEBOUNCE: Duration = Duration::from_millis(200);
const VIDEO_QUALITY_COOLDOWN: Duration = Duration::from_secs(3);
const VIDEO_QUALITY_UPGRADE_STABLE: Duration = Duration::from_secs(5);
const VIDEO_QUALITY_LONG_EDGES: [u32; 4] = [640, 960, 1280, 1920];
const VIDEO_PLAY_PREBUFFER_MAX: Duration = Duration::from_millis(150);
const VIDEO_PLAY_PREBUFFER_SECS: f64 = 0.100;

/// What the UI asks a worker for.
pub(super) struct VideoFrameRequest {
    pub generation: u64,
    /// Source-file time the panel wants a picture for.
    pub target_secs: f64,
    /// How many frames past `target_secs` to decode while we are here.
    pub decode_ahead: usize,
    pub box_px: (u32, u32),
    /// How far the decoder may walk forward before restarting from a keyframe.
    pub max_forward_walk: usize,
}

/// What a worker sends back.
pub(super) enum VideoFrameMsg {
    /// The file was opened and the stream described; sent once per worker.
    Opened {
        tab_id: u64,
        info: crate::video::VideoStreamInfo,
    },
    /// A run of frames in presentation order, for one request.
    Frames {
        tab_id: u64,
        generation: u64,
        box_px: (u32, u32),
        /// Actual uncompressed output selected by the backend. Media
        /// Foundation normally makes this the requested display size; a
        /// fallback decoder may return its native size and scale in Rust.
        output_px: (u32, u32),
        frames: Vec<(f64, Arc<egui::ColorImage>)>,
        /// False for an incremental chunk. The current generation remains in
        /// flight until its final chunk arrives.
        complete: bool,
        /// Worker time spent producing this chunk, for adaptive quality.
        decode_ms: f32,
    },
    /// The worker gave up; the panel says so and does not restart.
    Failed {
        tab_id: u64,
        status: VideoPanelStatus,
    },
}

/// The UI-side half of one tab's worker.
pub(super) struct VideoWorkerHandle {
    pub tab_id: u64,
    tx: Sender<VideoFrameRequest>,
    /// Set once, when the tab closes, and never cleared. Interrupts a decode
    /// already in flight so the worker does not finish a batch nobody is
    /// waiting for.
    shutdown: Arc<AtomicBool>,
}

impl VideoWorkerHandle {
    fn send(&self, request: VideoFrameRequest) -> bool {
        self.tx.send(request).is_ok()
    }
}

impl Drop for VideoWorkerHandle {
    fn drop(&mut self) {
        // Unblocks a decode already in flight; dropping `tx` then ends the
        // worker's receive loop and closes the file.
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl WavesPreviewer {
    fn ensure_video_channel(&mut self) -> Sender<VideoFrameMsg> {
        if self.video_frame_tx.is_none() || self.video_frame_rx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<VideoFrameMsg>();
            self.video_frame_tx = Some(tx);
            self.video_frame_rx = Some(rx);
        }
        self.video_frame_tx
            .as_ref()
            .expect("video channel just created")
            .clone()
    }

    /// Start (or restart) the decode worker for a tab whose source is a video.
    ///
    /// Safe to call every time a tab is opened: it is a no-op for an audio
    /// source, and replaces an existing worker for the same tab rather than
    /// stacking a second one.
    pub(super) fn spawn_video_worker_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let path = tab.path.clone();
        let tab_id = tab.tab_id;
        if !crate::media_kind::is_video_path(&path) || self.is_virtual_path(&path) {
            return;
        }
        if self
            .video_workers
            .iter()
            .any(|worker| worker.tab_id == tab_id)
        {
            return;
        }
        // A headless CLI render paints the editor into a PNG at a fixed frame
        // count. A decode worker there would make the output depend on whether
        // a frame happened to arrive in time, so the panel stays a placeholder.
        if self.headless {
            return;
        }

        let out_tx = self.ensure_video_channel();
        let (tx, rx) = std::sync::mpsc::channel::<VideoFrameRequest>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let worker_path = path.clone();
        std::thread::Builder::new()
            .name("neowaves-video".to_string())
            .spawn(move || {
                crate::app::threading::lower_current_thread_priority();
                video_worker_main(tab_id, worker_path, rx, out_tx, worker_shutdown);
            })
            .ok();

        self.video_workers.push(VideoWorkerHandle {
            tab_id,
            tx,
            shutdown,
        });
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            if tab.video_panel.is_none() {
                tab.video_panel = Some(VideoPanelState::new(placeholder_stream_info()));
            }
        }
    }

    /// Give every open video tab a worker, and drop workers whose tab is gone.
    ///
    /// Called once a frame rather than hooked into each of the half-dozen
    /// tab-creation sites (fresh open, cached restore, session restore, ...):
    /// there is exactly one rule — a video tab has a worker — and one place
    /// that enforces it. The scan is over at most `MAX_EDITOR_TABS` entries.
    pub(super) fn ensure_video_workers(&mut self) {
        if self.headless {
            return;
        }
        if !self.video_workers.is_empty() {
            self.prune_video_workers();
        }
        let wanted: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| {
                crate::media_kind::is_video_path(&tab.path)
                    && !self
                        .video_workers
                        .iter()
                        .any(|worker| worker.tab_id == tab.tab_id)
            })
            .map(|(idx, _)| idx)
            .collect();
        for idx in wanted {
            self.spawn_video_worker_for_tab(idx);
        }
    }

    /// Stop the worker for a tab that is closing or changing path.
    pub(super) fn stop_video_worker_for_tab_id(&mut self, tab_id: u64) {
        self.video_workers.retain(|worker| worker.tab_id != tab_id);
    }

    /// Drop workers whose tab is gone. Cheap enough to run whenever tabs move.
    pub(super) fn prune_video_workers(&mut self) {
        if self.video_workers.is_empty() {
            return;
        }
        let live: Vec<u64> = self.tabs.iter().map(|tab| tab.tab_id).collect();
        self.video_workers
            .retain(|worker| live.contains(&worker.tab_id));
    }

    /// True while any panel is waiting on a frame, so the frame loop keeps
    /// ticking until a paused seek lands.
    pub(super) fn video_panel_inflight(&self) -> bool {
        self.tabs
            .iter()
            .any(|tab| tab.video_panel.as_ref().is_some_and(|panel| panel.inflight))
    }

    /// While the audio clock is advancing, completed video chunks are not
    /// optional background decoration: delaying their drain can create the
    /// very ring underrun they were decoded to prevent.
    pub(super) fn video_updates_are_latency_critical(&self) -> bool {
        self.video_panel_inflight()
            && (self.playback_is_playing_now() || self.pending_video_play_start.is_some())
    }

    pub(super) fn video_play_start_pending(&self) -> bool {
        self.pending_video_play_start.is_some()
    }

    pub(super) fn cancel_pending_video_play_start(&mut self) -> bool {
        self.pending_video_play_start.take().is_some()
    }

    /// Return true when Play has been parked for the short video prebuffer.
    pub(super) fn defer_video_play_start_for_tab(&mut self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(panel) = tab.video_panel.as_ref() else {
            return false;
        };
        if matches!(
            panel.status,
            VideoPanelStatus::Failed(_) | VideoPanelStatus::Unsupported(_)
        ) {
            return false;
        }
        let target_secs = self.playback_current_source_time_sec().unwrap_or(0.0);
        if video_prebuffer_ready(panel, target_secs) {
            return false;
        }
        let tab_id = tab.tab_id;
        self.pending_video_play_start = Some(super::PendingVideoPlayStart {
            tab_id,
            target_secs,
            deadline: Instant::now() + VIDEO_PLAY_PREBUFFER_MAX,
        });
        self.request_video_frame_for_tab(tab_idx, target_secs, true);
        true
    }

    /// Finish or keep pumping a pending 150 ms Play gate. Called after video
    /// chunks are drained so a just-arrived target frame can start audio in
    /// this same UI update.
    pub(super) fn tick_pending_video_play_start(&mut self, ctx: &egui::Context) {
        let Some(mut pending) = self.pending_video_play_start.take() else {
            return;
        };
        if self.playback_is_playing_now() || !self.is_editor_workspace_active() {
            return;
        }
        let Some(tab_idx) = self.active_tab.filter(|idx| {
            self.tabs
                .get(*idx)
                .is_some_and(|tab| tab.tab_id == pending.tab_id)
        }) else {
            return;
        };
        let now = Instant::now();
        let current_target = self
            .playback_current_source_time_sec()
            .unwrap_or(pending.target_secs);
        let frame_secs = self.tabs[tab_idx]
            .video_panel
            .as_ref()
            .map(video_frame_secs)
            .unwrap_or(1.0 / 30.0);
        if (current_target - pending.target_secs).abs() > frame_secs {
            pending.target_secs = current_target;
            pending.deadline = now + VIDEO_PLAY_PREBUFFER_MAX;
        }
        let ready = self.tabs[tab_idx]
            .video_panel
            .as_ref()
            .is_some_and(|panel| video_prebuffer_ready(panel, pending.target_secs));
        if ready || now >= pending.deadline {
            self.audio.play();
            self.playback_sync_state_snapshot();
            ctx.request_repaint();
            return;
        }

        let target_secs = pending.target_secs;
        self.pending_video_play_start = Some(pending);
        self.request_video_frame_for_tab(tab_idx, target_secs, true);
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    /// Ask this tab's worker for the picture at `target_secs`.
    ///
    /// Called from the editor draw, once per frame. Cheap by design: a couple
    /// of float comparisons and, at most, one non-blocking send.
    pub(super) fn request_video_frame_for_tab(
        &mut self,
        tab_idx: usize,
        target_secs: f64,
        playing: bool,
    ) {
        let perf = self.perf;
        let fps = self
            .tabs
            .get(tab_idx)
            .and_then(|tab| tab.video_panel.as_ref())
            .map(|panel| panel.info.nominal_fps)
            .unwrap_or(30.0);
        let decode_ahead = if playing {
            perf.video_decode_ahead_frames_for_fps(fps)
        } else {
            1
        };
        let max_forward_walk = self.perf.video_forward_walk_frames();
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let tab_id = tab.tab_id;
        let Some(panel) = tab.video_panel.as_mut() else {
            return;
        };
        if matches!(
            panel.status,
            VideoPanelStatus::Failed(_) | VideoPanelStatus::Unsupported(_)
        ) {
            return;
        }
        // The size the panel asked for on its last draw. Zero means the strip
        // is too narrow to show a picture, so nothing needs decoding.
        let wanted_box_px = panel.effective_wanted_box_px();
        if wanted_box_px.0 == 0 || wanted_box_px.1 == 0 {
            return;
        }

        // Half a frame of slack: below that the picture on screen is already
        // the right one and a request would only add work.
        let frame_secs = if panel.info.nominal_fps > 1.0 {
            1.0 / panel.info.nominal_fps as f64
        } else {
            1.0 / 30.0
        };
        // AAC priming/padding can make the audio transport a few samples (or
        // one AAC packet) longer than the video track. Media Foundation rejects
        // a seek beyond the video duration with MF_E_INVALIDREQUEST, so keep
        // decoder requests just inside the last frame while the painter still
        // uses the unmodified audio clock.
        let target_secs = video_decode_target_secs(panel, target_secs);
        let previous_target = if panel.last_target_secs.is_finite() {
            video_decode_target_secs(panel, panel.last_target_secs)
        } else {
            panel.last_target_secs
        };
        let backward_clock_jump =
            previous_target.is_finite() && target_secs + frame_secs < previous_target;
        panel.last_target_secs = target_secs;
        let newest_decoded = panel.ring.back().map(|(pts, _)| *pts).or(panel.shown_pts);
        let forward_underrun =
            playing && newest_decoded.is_some_and(|pts| target_secs > pts + frame_secs);
        let box_px = adaptive_video_box(
            panel,
            wanted_box_px,
            Instant::now(),
            decode_ahead,
            perf.video_ring_memory_bytes(),
            forward_underrun,
        );
        let have_frame_for_target = panel
            .frame_at(target_secs)
            .map(|(pts, _)| target_secs >= pts && target_secs - pts < frame_secs)
            .unwrap_or(false);
        let box_changed = panel.box_px != box_px;
        // Count only pictures still ahead of the playhead. Old frames remain
        // in the ring briefly for cheap backward nudges, so the total ring
        // length never represented how much read-ahead was left.
        let frames_ahead = panel
            .ring
            .iter()
            .filter(|(pts, _)| *pts > target_secs)
            .count();
        let ring_running_low = playing && frames_ahead <= decode_ahead / 2;
        if !box_changed && have_frame_for_target && !ring_running_low {
            return;
        }
        let newest_coverage = panel
            .ring
            .back()
            .map(|(pts, _)| *pts)
            .or(panel.shown_pts)
            .unwrap_or(panel.requested_secs);
        let discontinuous_forward = target_secs > newest_coverage + frame_secs * 2.0
            && target_secs > panel.requested_secs + frame_secs * 2.0;
        let discontinuous_backward = backward_clock_jump;
        // Sequential read-ahead is left alone, but a seek, stable resize or a
        // decoder that has fallen behind may supersede an active generation.
        // The worker checks its channel between native frames and switches to
        // this newest request without finishing the stale batch.
        if panel.inflight && !box_changed && !discontinuous_forward && !discontinuous_backward {
            return;
        }

        if box_changed {
            // Frames were scaled into the old box on the way in; they cannot
            // be grown back. Keep the current texture and its synchronized
            // PTS on screen until the replacement arrives, though: opening or
            // resizing the detached window must not blank a valid picture.
            panel.ring.clear();
            panel.box_px = box_px;
        }
        let outside_ring =
            panel
                .ring
                .front()
                .zip(panel.ring.back())
                .is_some_and(|((front, _), (back, _))| {
                    target_secs + frame_secs < *front || target_secs > *back + frame_secs
                });
        if outside_ring {
            // A discontinuous seek must be allowed to append older frames.
            // Keep the currently uploaded texture and PTS: during a forward
            // underrun it remains a safe (non-future) frame to display while
            // the worker catches up. A backward seek is hidden by the paint
            // rule until its replacement arrives.
            panel.ring.clear();
        }
        if discontinuous_backward {
            panel.seeking = true;
        }
        let request_secs = if playing && have_frame_for_target {
            panel
                .ring
                .back()
                .map(|(pts, _)| pts + frame_secs)
                .unwrap_or(target_secs)
        } else {
            target_secs
        };
        panel.generation = panel.generation.wrapping_add(1).max(1);
        panel.requested_secs = request_secs;
        panel.inflight = true;
        let request = VideoFrameRequest {
            generation: panel.generation,
            target_secs: request_secs,
            decode_ahead,
            box_px,
            max_forward_walk,
        };

        let sent = self
            .video_workers
            .iter()
            .find(|worker| worker.tab_id == tab_id)
            .map(|worker| worker.send(request))
            .unwrap_or(false);
        if !sent {
            if let Some(panel) = self
                .tabs
                .get_mut(tab_idx)
                .and_then(|tab| tab.video_panel.as_mut())
            {
                panel.inflight = false;
            }
        }
    }

    /// Take whatever the workers have produced and put it on the tabs.
    ///
    /// Idle results run behind the per-frame budget guard. Playback and
    /// prebuffer results are drained before drawing so incremental chunks can
    /// extend the ring as soon as they arrive. Texture upload still happens
    /// only for the one PTS selected by the painter.
    pub(super) fn apply_video_frame_updates(&mut self, ctx: &egui::Context) {
        let mut messages = Vec::new();
        if let Some(rx) = &self.video_frame_rx {
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }
        if messages.is_empty() {
            return;
        }
        let perf = self.perf;
        let mut repaint = false;
        for msg in messages {
            match msg {
                VideoFrameMsg::Opened { tab_id, info } => {
                    if let Some(panel) = self.video_panel_for_tab_id(tab_id) {
                        panel.info = info;
                        if matches!(panel.status, VideoPanelStatus::Probing) {
                            panel.status = VideoPanelStatus::Ready;
                        }
                        repaint = true;
                    }
                }
                VideoFrameMsg::Failed { tab_id, status } => {
                    if let Some(panel) = self.video_panel_for_tab_id(tab_id) {
                        panel.status = status;
                        panel.inflight = false;
                        panel.ring.clear();
                        panel.texture = None;
                        panel.shown_pts = None;
                        repaint = true;
                    }
                }
                VideoFrameMsg::Frames {
                    tab_id,
                    generation,
                    box_px,
                    output_px,
                    frames,
                    complete,
                    decode_ms,
                } => {
                    let Some(panel) = self.video_panel_for_tab_id(tab_id) else {
                        continue;
                    };
                    // A run from a superseded seek, or one scaled for a panel
                    // size that has since changed, is not worth showing.
                    if generation != panel.generation || box_px != panel.box_px {
                        continue;
                    }
                    panel.inflight = !complete;
                    panel.output_px = output_px;
                    let decoded_frame_count = frames.len() as u32;
                    if decoded_frame_count > 0 && decode_ms.is_finite() && decode_ms >= 0.0 {
                        // `decode_ms` is already a per-frame average for this
                        // chunk. Apply it once per picture so the 12-frame
                        // adaptation threshold does not accidentally mean 12
                        // chunks (normally about twice as many frames).
                        for _ in 0..decoded_frame_count {
                            panel.decode_ms_ema = if panel.decode_ms_ema > 0.0 {
                                panel.decode_ms_ema * 0.85 + decode_ms * 0.15
                            } else {
                                decode_ms
                            };
                        }
                        panel.decode_timing_samples = panel
                            .decode_timing_samples
                            .saturating_add(decoded_frame_count);
                    }
                    if matches!(panel.status, VideoPanelStatus::Probing) {
                        panel.status = VideoPanelStatus::Ready;
                    }
                    if !frames.is_empty() {
                        panel.seeking = false;
                    }
                    for (pts, image) in frames {
                        // Keep the ring sorted and free of duplicates without
                        // a sort: frames arrive in presentation order.
                        if panel.ring.back().map(|(back, _)| *back >= pts) == Some(true) {
                            continue;
                        }
                        panel.ring.push_back((pts, image));
                    }
                    let ring_capacity = perf
                        .video_decode_ahead_frames_for_fps(panel.info.nominal_fps)
                        .max(2)
                        * 2;
                    while panel.ring.len() > ring_capacity
                        || (panel.ring.len() > 1
                            && panel.ring_bytes() > perf.video_ring_memory_bytes())
                    {
                        panel.ring.pop_front();
                    }
                    repaint = true;
                }
            }
        }
        if repaint {
            ctx.request_repaint();
        }
    }

    fn video_panel_for_tab_id(&mut self, tab_id: u64) -> Option<&mut VideoPanelState> {
        self.tabs
            .iter_mut()
            .find(|tab| tab.tab_id == tab_id)
            .and_then(|tab| tab.video_panel.as_mut())
    }
}

fn capped_video_box(wanted: (u32, u32), source: (u32, u32), long_edge_cap: u32) -> (u32, u32) {
    let wanted = (
        wanted.0.max(1).min(source.0.max(1)),
        wanted.1.max(1).min(source.1.max(1)),
    );
    let long_edge = wanted.0.max(wanted.1);
    if long_edge <= long_edge_cap {
        return wanted;
    }
    let scale = long_edge_cap as f64 / long_edge as f64;
    (
        ((wanted.0 as f64 * scale).round() as u32).max(1),
        ((wanted.1 as f64 * scale).round() as u32).max(1),
    )
}

fn video_frame_secs(panel: &VideoPanelState) -> f64 {
    video_stream_frame_secs(&panel.info)
}

fn video_stream_frame_secs(info: &crate::video::VideoStreamInfo) -> f64 {
    if info.nominal_fps.is_finite() && info.nominal_fps > 1.0 {
        1.0 / info.nominal_fps as f64
    } else {
        1.0 / 30.0
    }
}

fn video_decode_target_secs(panel: &VideoPanelState, requested_secs: f64) -> f64 {
    video_decode_target_for_info(&panel.info, requested_secs)
}

fn video_decode_target_for_info(info: &crate::video::VideoStreamInfo, requested_secs: f64) -> f64 {
    let requested_secs = if requested_secs.is_finite() {
        requested_secs.max(0.0)
    } else {
        0.0
    };
    let duration = info.duration_secs;
    if !duration.is_finite() || duration <= 0.0 {
        return requested_secs;
    }
    // The source reader may reject a position after the final sample even if
    // it is still numerically below the track duration. One nominal frame is
    // the tightest portable bound and still includes that final sample in the
    // decoder's at-or-before selection (with its timestamp epsilon).
    let inside_end = (duration - video_stream_frame_secs(info)).max(0.0);
    requested_secs.min(inside_end)
}

fn video_prebuffer_ready(panel: &VideoPanelState, target_secs: f64) -> bool {
    let target_secs = video_decode_target_secs(panel, target_secs);
    if panel.frame_at(target_secs).is_none() {
        return false;
    }
    let frame_secs = video_frame_secs(panel);
    let remaining = (panel.info.duration_secs - target_secs).max(0.0);
    let horizon = VIDEO_PLAY_PREBUFFER_SECS.min(remaining);
    let required_last = target_secs + horizon - frame_secs;
    panel
        .ring
        .back()
        .is_some_and(|(pts, _)| *pts + 1.0e-7 >= required_last)
}

fn adaptive_video_box(
    panel: &mut VideoPanelState,
    wanted: (u32, u32),
    now: Instant,
    decode_ahead: usize,
    memory_budget: usize,
    forward_underrun: bool,
) -> (u32, u32) {
    if panel.observed_wanted_box_px != wanted {
        panel.observed_wanted_box_px = wanted;
        panel.wanted_box_changed_at = now;
    }
    if panel.stable_wanted_box_px == (0, 0) {
        panel.stable_wanted_box_px = wanted;
    } else if now.duration_since(panel.wanted_box_changed_at) >= VIDEO_RESIZE_DEBOUNCE
        && panel.stable_wanted_box_px != wanted
    {
        panel.stable_wanted_box_px = wanted;
        // Opening a genuinely large viewer starts at 720p. This is an initial
        // quality choice, not a five-second promotion from the inline size.
        if wanted.0.max(wanted.1) > VIDEO_QUALITY_LONG_EDGES[1] && panel.quality_level < 2 {
            panel.quality_level = 2;
            panel.quality_changed_at = now;
            panel.quality_stable_since = now;
        }
    }

    let stable = panel.stable_wanted_box_px;
    let source = (
        panel.info.display_width.max(1),
        panel.info.display_height.max(1),
    );
    let fps = if panel.info.nominal_fps.is_finite() && panel.info.nominal_fps > 1.0 {
        panel.info.nominal_fps
    } else {
        30.0
    };
    let frame_ms = 1_000.0 / fps;

    while panel
        .underrun_times
        .front()
        .is_some_and(|at| now.duration_since(*at) > Duration::from_secs(1))
    {
        panel.underrun_times.pop_front();
    }
    if forward_underrun && !panel.underrun_active {
        panel.underrun_times.push_back(now);
        panel.quality_stable_since = now;
    }
    panel.underrun_active = forward_underrun;

    let wanted_long = stable.0.max(stable.1);
    let mut max_level = VIDEO_QUALITY_LONG_EDGES
        .iter()
        .position(|edge| *edge >= wanted_long)
        .unwrap_or(VIDEO_QUALITY_LONG_EDGES.len() - 1);
    // Do not select a resolution whose target horizon alone exceeds this
    // tab's decoded-frame memory budget.
    while max_level > 0 {
        let candidate = capped_video_box(stable, source, VIDEO_QUALITY_LONG_EDGES[max_level]);
        let bytes = candidate.0 as usize * candidate.1 as usize * 4;
        if bytes.saturating_mul(decode_ahead.max(1)) <= memory_budget {
            break;
        }
        max_level -= 1;
    }
    let cooldown_done = now.duration_since(panel.quality_changed_at) >= VIDEO_QUALITY_COOLDOWN;
    let decode_too_slow =
        panel.decode_timing_samples >= 12 && panel.decode_ms_ema > frame_ms * 0.70;
    if cooldown_done
        && panel.quality_level > 0
        && (panel.underrun_times.len() >= 2 || decode_too_slow)
    {
        panel.quality_level -= 1;
        panel.quality_changed_at = now;
        panel.quality_stable_since = now;
        panel.underrun_times.clear();
        panel.decode_timing_samples = 0;
    } else {
        let frames_ahead = panel
            .ring
            .iter()
            .filter(|(pts, _)| *pts > panel.last_target_secs)
            .count();
        let can_upgrade = cooldown_done
            && panel.quality_level < max_level
            && now.duration_since(panel.quality_stable_since) >= VIDEO_QUALITY_UPGRADE_STABLE
            && panel.decode_timing_samples >= 12
            && panel.decode_ms_ema < frame_ms * 0.40
            && frames_ahead >= decode_ahead.max(2) / 2;
        if can_upgrade {
            panel.quality_level += 1;
            panel.quality_changed_at = now;
            panel.quality_stable_since = now;
            panel.decode_timing_samples = 0;
        }
    }

    capped_video_box(
        stable,
        source,
        VIDEO_QUALITY_LONG_EDGES[panel.quality_level.min(max_level)],
    )
}

/// Stand-in description used between "we know this is a video" and the
/// worker's first word about it, so the panel can reserve a 16:9 slot rather
/// than pop into existence a moment later.
fn placeholder_stream_info() -> crate::video::VideoStreamInfo {
    crate::video::VideoStreamInfo {
        coded_width: 16,
        coded_height: 9,
        display_width: 16,
        display_height: 9,
        rotation: crate::video::Rotation::None,
        duration_secs: 0.0,
        nominal_fps: 0.0,
        codec_label: String::new(),
        codec: crate::video::VideoCodec::Unknown,
    }
}

fn video_worker_main(
    tab_id: u64,
    path: PathBuf,
    rx: Receiver<VideoFrameRequest>,
    tx: Sender<VideoFrameMsg>,
    shutdown: Arc<AtomicBool>,
) {
    let mut decoder = match crate::video::open_video_decoder(&path) {
        Ok(decoder) => decoder,
        Err(err) => {
            let status = match err {
                crate::video::VideoOpenError::NoVideoTrack => {
                    VideoPanelStatus::Unsupported("no video track".to_string())
                }
                crate::video::VideoOpenError::UnsupportedCodec(codec) => {
                    VideoPanelStatus::Unsupported(codec)
                }
                crate::video::VideoOpenError::Failed(msg) => VideoPanelStatus::Failed(msg),
            };
            let _ = tx.send(VideoFrameMsg::Failed { tab_id, status });
            crate::ui_wake::wake_ui();
            return;
        }
    };
    let _ = tx.send(VideoFrameMsg::Opened {
        tab_id,
        info: decoder.info().clone(),
    });
    crate::ui_wake::wake_ui();

    let mut pending_request = None;
    loop {
        let request = match pending_request.take() {
            Some(request) => request,
            None => match rx.recv() {
                Ok(request) => request,
                Err(_) => return,
            },
        };
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        // Only the newest queued request matters: everything older is a
        // position or output size the playhead has already left. A request
        // that arrives after decoding starts is picked up by the per-frame
        // check in the read-ahead loop below.
        let mut request = request;
        while let Ok(newer) = rx.try_recv() {
            request = newer;
        }

        let mut output_px = match decoder.prepare_output(request.box_px) {
            Ok(output_px) => output_px,
            Err(err) => {
                let _ = tx.send(VideoFrameMsg::Failed {
                    tab_id,
                    status: VideoPanelStatus::Failed(format!("{err:#}")),
                });
                crate::ui_wake::wake_ui();
                return;
            }
        };
        // Requests can be queued before the UI receives `Opened`, while its
        // placeholder stream info still has duration 0. Clamp again here from
        // the decoder's authoritative info so startup timing cannot leak an
        // AAC-padded audio timestamp beyond the video track.
        let decode_target_secs = video_decode_target_for_info(decoder.info(), request.target_secs);
        if let Err(err) = decoder.seek(decode_target_secs, request.max_forward_walk) {
            let _ = tx.send(VideoFrameMsg::Failed {
                tab_id,
                status: VideoPanelStatus::Failed(format!("{err:#}")),
            });
            crate::ui_wake::wake_ui();
            return;
        }

        let wanted = request.decode_ahead.max(1);
        let mut chunk = Vec::with_capacity(2);
        let mut chunk_started = std::time::Instant::now();
        let mut decoded = 0usize;
        let mut reached_end = false;
        let mut superseded = false;
        while decoded < wanted {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            // A seek, catch-up or stable resize should not wait behind a long
            // read-ahead batch. Keep only the newest request and switch after
            // at most one native frame decode.
            if let Ok(newer) = rx.try_recv() {
                let mut newest = newer;
                while let Ok(next) = rx.try_recv() {
                    newest = next;
                }
                pending_request = Some(newest);
                superseded = true;
                break;
            }
            let frame_started = std::time::Instant::now();
            match decoder.next_frame(request.box_px, &shutdown) {
                Ok(Some(frame)) => {
                    let frame_ms = frame_started.elapsed().as_secs_f32() * 1_000.0;
                    output_px = (frame.image.size[0] as u32, frame.image.size[1] as u32);
                    chunk.push((frame.pts_secs, frame.image));
                    decoded += 1;
                    // The target frame is delivered alone. Afterwards use
                    // tiny chunks, also bounded by wall time, so the UI starts
                    // drawing while read-ahead is still being produced.
                    let flush = decoded == 1
                        || chunk.len() >= 2
                        || chunk_started.elapsed() >= std::time::Duration::from_millis(50)
                        || decoded == wanted;
                    if flush {
                        let complete = decoded == wanted;
                        let frames = std::mem::take(&mut chunk);
                        let per_frame_ms = if frames.is_empty() {
                            0.0
                        } else {
                            frame_ms.max(
                                chunk_started.elapsed().as_secs_f32() * 1_000.0
                                    / frames.len() as f32,
                            )
                        };
                        let _ = tx.send(VideoFrameMsg::Frames {
                            tab_id,
                            generation: request.generation,
                            box_px: request.box_px,
                            output_px,
                            frames,
                            complete,
                            decode_ms: per_frame_ms,
                        });
                        crate::ui_wake::wake_ui();
                        chunk_started = std::time::Instant::now();
                    }
                }
                Ok(None) => {
                    reached_end = true;
                    break;
                }
                Err(err) => {
                    // A decode error mid-stream is usually one damaged frame,
                    // not a dead file. Deliver what we have and let the next
                    // request try again from a keyframe.
                    eprintln!("video: decode error in {}: {err:#}", path.display());
                    reached_end = true;
                    break;
                }
            }
        }

        if superseded {
            continue;
        }
        if !chunk.is_empty() {
            let frame_count = chunk.len();
            let _ = tx.send(VideoFrameMsg::Frames {
                tab_id,
                generation: request.generation,
                box_px: request.box_px,
                output_px,
                frames: std::mem::take(&mut chunk),
                complete: true,
                decode_ms: chunk_started.elapsed().as_secs_f32() * 1_000.0 / frame_count as f32,
            });
            crate::ui_wake::wake_ui();
        } else if reached_end || decoded < wanted {
            // Still report completion, or the panel would sit in-flight
            // forever at EOF or after one damaged sample.
            let _ = tx.send(VideoFrameMsg::Frames {
                tab_id,
                generation: request.generation,
                box_px: request.box_px,
                output_px,
                frames: Vec::new(),
                complete: true,
                decode_ms: 0.0,
            });
            crate::ui_wake::wake_ui();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(now: Instant) -> VideoPanelState {
        let mut panel = VideoPanelState::new(crate::video::VideoStreamInfo {
            coded_width: 3840,
            coded_height: 2160,
            display_width: 3840,
            display_height: 2160,
            rotation: crate::video::Rotation::None,
            duration_secs: 10.0,
            nominal_fps: 30.0,
            codec_label: "test".to_string(),
            codec: crate::video::VideoCodec::H264,
        });
        panel.status = VideoPanelStatus::Ready;
        panel.wanted_box_changed_at = now;
        panel.quality_changed_at = now;
        panel.quality_stable_since = now;
        panel
    }

    fn frame(pts: f64) -> (f64, Arc<egui::ColorImage>) {
        (
            pts,
            Arc::new(egui::ColorImage::filled([16, 9], egui::Color32::BLACK)),
        )
    }

    #[test]
    fn detached_resize_is_debounced_and_starts_no_higher_than_720p() {
        let start = Instant::now();
        let mut panel = panel(start);
        assert_eq!(
            adaptive_video_box(&mut panel, (320, 180), start, 12, 48 << 20, false),
            (320, 180)
        );
        assert_eq!(
            panel.quality_level, 1,
            "a small inline box must not permanently lower detached quality"
        );
        let during_resize = adaptive_video_box(
            &mut panel,
            (1920, 1080),
            start + Duration::from_millis(50),
            12,
            48 << 20,
            false,
        );
        assert_eq!(during_resize, (320, 180));
        let stable = adaptive_video_box(
            &mut panel,
            (1920, 1080),
            start + Duration::from_millis(260),
            12,
            48 << 20,
            false,
        );
        assert_eq!(stable, (1280, 720));
    }

    #[test]
    fn slow_decode_drops_one_quality_level_after_cooldown() {
        let start = Instant::now();
        let mut panel = panel(start);
        panel.stable_wanted_box_px = (1920, 1080);
        panel.observed_wanted_box_px = (1920, 1080);
        panel.quality_level = 2;
        panel.decode_timing_samples = 12;
        panel.decode_ms_ema = 30.0;
        let box_px = adaptive_video_box(
            &mut panel,
            (1920, 1080),
            start + Duration::from_secs(4),
            12,
            48 << 20,
            false,
        );
        assert_eq!(panel.quality_level, 1);
        assert_eq!(box_px, (960, 540));
    }

    #[test]
    fn sixty_fps_read_ahead_respects_the_high_tier_memory_ceiling() {
        let start = Instant::now();
        let mut panel = panel(start);
        panel.stable_wanted_box_px = (3840, 2160);
        panel.observed_wanted_box_px = (3840, 2160);
        panel.quality_level = 3;
        let box_px = adaptive_video_box(&mut panel, (3840, 2160), start, 36, 96 << 20, false);
        let ring_bytes = box_px.0 as usize * box_px.1 as usize * 4 * 36;
        assert!(ring_bytes <= 96 << 20);
        assert_eq!(box_px, (960, 540));
    }

    #[test]
    fn two_distinct_underflows_in_one_second_lower_quality() {
        let start = Instant::now();
        let mut panel = panel(start);
        panel.stable_wanted_box_px = (1920, 1080);
        panel.observed_wanted_box_px = (1920, 1080);
        panel.quality_level = 2;
        let _ = adaptive_video_box(
            &mut panel,
            (1920, 1080),
            start + Duration::from_secs(4),
            12,
            48 << 20,
            true,
        );
        let _ = adaptive_video_box(
            &mut panel,
            (1920, 1080),
            start + Duration::from_millis(4_100),
            12,
            48 << 20,
            false,
        );
        let box_px = adaptive_video_box(
            &mut panel,
            (1920, 1080),
            start + Duration::from_millis(4_200),
            12,
            48 << 20,
            true,
        );
        assert_eq!(panel.quality_level, 1);
        assert_eq!(box_px, (960, 540));
    }

    #[test]
    fn prebuffer_requires_the_target_and_about_one_tenth_second_ahead() {
        let start = Instant::now();
        let mut panel = panel(start);
        panel.ring.push_back(frame(0.0));
        assert!(!video_prebuffer_ready(&panel, 0.0));
        panel.ring.push_back(frame(1.0 / 30.0));
        assert!(!video_prebuffer_ready(&panel, 0.0));
        panel.ring.push_back(frame(2.0 / 30.0));
        // The frame at 66.7 ms covers the slot ending at 100 ms.
        assert!(video_prebuffer_ready(&panel, 0.0));
    }

    #[test]
    fn audio_padding_is_clamped_inside_the_video_tail() {
        let start = Instant::now();
        let mut panel = panel(start);
        panel.info.duration_secs = 10.0;
        panel.info.nominal_fps = 30.0;
        let target = video_decode_target_secs(&panel, 10.08);
        assert!(target < 10.0);
        assert!((target - (10.0 - 1.0 / 30.0)).abs() < 1.0e-7);

        panel.ring.push_back(frame(10.0 - 1.0 / 30.0));
        assert!(video_prebuffer_ready(&panel, 10.08));
    }
}
