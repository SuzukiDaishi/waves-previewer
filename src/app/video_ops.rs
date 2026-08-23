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

use super::types::{VideoPanelState, VideoPanelStatus};
use super::WavesPreviewer;

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
        frames: Vec<(f64, Arc<egui::ColorImage>)>,
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
        let decode_ahead = if playing {
            self.perf.video_decode_ahead_frames()
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
        let box_px = panel.wanted_box_px;
        if box_px.0 == 0 || box_px.1 == 0 {
            return;
        }

        // Half a frame of slack: below that the picture on screen is already
        // the right one and a request would only add work.
        let frame_secs = if panel.info.nominal_fps > 1.0 {
            1.0 / panel.info.nominal_fps as f64
        } else {
            1.0 / 30.0
        };
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
        // Never supersede a decode already in flight. Previously each UI frame
        // bumped the generation while playback advanced, so a decode taking
        // longer than one repaint was guaranteed to arrive "stale" and be
        // discarded. The next draw requests the newest playhead as soon as
        // this result lands.
        if panel.inflight {
            return;
        }

        if box_changed {
            // Frames were scaled into the old box on the way in; they cannot
            // be grown back.
            panel.ring.clear();
            panel.shown_pts = None;
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
            panel.ring.clear();
            panel.shown_pts = None;
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
    /// Runs behind the per-frame budget guard in `frame_ops.rs`. Video is
    /// latest-value-wins rather than a queue: the newest run for a tab
    /// replaces the ring, and only one texture upload happens per tab per
    /// frame.
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
        let ring_capacity = self.perf.video_decode_ahead_frames().max(2) * 2;
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
                    frames,
                } => {
                    let Some(panel) = self.video_panel_for_tab_id(tab_id) else {
                        continue;
                    };
                    panel.inflight = false;
                    // A run from a superseded seek, or one scaled for a panel
                    // size that has since changed, is not worth showing.
                    if generation != panel.generation || box_px != panel.box_px {
                        continue;
                    }
                    if matches!(panel.status, VideoPanelStatus::Probing) {
                        panel.status = VideoPanelStatus::Ready;
                    }
                    for (pts, image) in frames {
                        // Keep the ring sorted and free of duplicates without
                        // a sort: frames arrive in presentation order.
                        if panel.ring.back().map(|(back, _)| *back >= pts) == Some(true) {
                            continue;
                        }
                        panel.ring.push_back((pts, image));
                    }
                    while panel.ring.len() > ring_capacity {
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

    while let Ok(request) = rx.recv() {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        // Only the newest request matters: everything queued behind it is a
        // position the playhead has already left. Superseding happens here,
        // before any decoding starts, so there is nothing to interrupt
        // mid-batch and no per-request cancellation to track — `shutdown` is
        // the tab closing, and it is never cleared.
        let mut request = request;
        while let Ok(newer) = rx.try_recv() {
            request = newer;
        }

        if let Err(err) = decoder.seek(request.target_secs, request.max_forward_walk) {
            let _ = tx.send(VideoFrameMsg::Failed {
                tab_id,
                status: VideoPanelStatus::Failed(format!("{err:#}")),
            });
            crate::ui_wake::wake_ui();
            return;
        }

        let mut frames = Vec::new();
        let wanted = request.decode_ahead.max(1);
        for _ in 0..wanted {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            match decoder.next_frame(request.box_px, &shutdown) {
                Ok(Some(frame)) => frames.push((frame.pts_secs, frame.image)),
                Ok(None) => break,
                Err(err) => {
                    // A decode error mid-stream is usually one damaged frame,
                    // not a dead file. Deliver what we have and let the next
                    // request try again from a keyframe.
                    eprintln!("video: decode error in {}: {err:#}", path.display());
                    break;
                }
            }
        }
        if frames.is_empty() {
            // Still report back, or the panel would sit `inflight` forever and
            // keep the frame loop awake.
            let _ = tx.send(VideoFrameMsg::Frames {
                tab_id,
                generation: request.generation,
                box_px: request.box_px,
                frames: Vec::new(),
            });
        } else {
            let _ = tx.send(VideoFrameMsg::Frames {
                tab_id,
                generation: request.generation,
                box_px: request.box_px,
                frames,
            });
        }
        crate::ui_wake::wake_ui();
    }
}
