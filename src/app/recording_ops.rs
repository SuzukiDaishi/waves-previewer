use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use super::*;

const RECORDING_COMMAND_RUN: u8 = 0;
const RECORDING_COMMAND_STOP: u8 = 1;
const RECORDING_COMMAND_DISCARD: u8 = 2;
/// How long the worker waits for audio before rechecking Stop/Discard and
/// device errors: short enough that Stop feels immediate.
const RECORDING_COMMAND_POLL: std::time::Duration = std::time::Duration::from_millis(100);
/// How often the WAV header is rewritten mid-take, so a crash or power cut
/// loses at most this much of the recording.
const RECORDING_CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
/// Seconds of the take the live waveform shows.
pub(super) const LIVE_WAVEFORM_WINDOW_SECS: f32 = 10.0;
/// Overview blocks per second: 2.5 ms each, finer than one pixel of a
/// 10-second window on any display this runs on.
const LIVE_WAVEFORM_BLOCKS_PER_SEC: usize = 400;

pub(super) fn live_waveform_block_frames(sample_rate: u32) -> usize {
    (sample_rate as usize / LIVE_WAVEFORM_BLOCKS_PER_SEC).max(64)
}

/// Folds mono samples into fixed-size (min, max) blocks and hands them to the
/// UI a capture buffer at a time.
struct WaveformAccumulator {
    block_frames: usize,
    /// Absolute frame of the first finished-but-unsent block.
    out_start: Option<u64>,
    out: Vec<(f32, f32)>,
    /// The block being filled.
    len: usize,
    min: f32,
    max: f32,
}

impl WaveformAccumulator {
    fn new(block_frames: usize) -> Self {
        Self {
            block_frames: block_frames.max(1),
            out_start: None,
            out: Vec::new(),
            len: 0,
            min: f32::MAX,
            max: f32::MIN,
        }
    }

    fn push(&mut self, frame: u64, value: f32) {
        if self.out_start.is_none() && self.out.is_empty() && self.len == 0 {
            self.out_start = Some(frame);
        }
        self.min = self.min.min(value);
        self.max = self.max.max(value);
        self.len += 1;
        if self.len >= self.block_frames {
            self.close_block();
        }
    }

    fn close_block(&mut self) {
        if self.len == 0 {
            return;
        }
        self.out.push((self.min, self.max));
        self.len = 0;
        self.min = f32::MAX;
        self.max = f32::MIN;
    }

    /// Sends the finished blocks; with `include_partial`, the one still
    /// filling as well (end of take).
    fn send(
        &mut self,
        include_partial: bool,
        tx: &std::sync::mpsc::Sender<crate::app::types::RecordingWorkerMsg>,
    ) {
        if include_partial {
            self.close_block();
        }
        if self.out.is_empty() {
            return;
        }
        let start_frame = self.out_start.unwrap_or(0);
        let blocks = std::mem::take(&mut self.out);
        // A short final block is counted as full; only drawing reads this,
        // and the writer's frame count stays the authority on length.
        let end_frame =
            start_frame.saturating_add(blocks.len() as u64 * self.block_frames as u64);
        // The block still filling (if any) starts where these end.
        self.out_start = if self.len > 0 { Some(end_frame) } else { None };
        let _ = tx.send(crate::app::types::RecordingWorkerMsg::WaveformBlocks {
            start_frame,
            block_frames: self.block_frames as u64,
            blocks,
            end_frame,
        });
    }
}

fn write_recording_buffer(
    writer: &mut crate::wav_stream::StreamingWaveWriter,
    interleaved: &[f32],
    channels: u16,
    waveform: &mut WaveformAccumulator,
    tx: &std::sync::mpsc::Sender<crate::app::types::RecordingWorkerMsg>,
) -> anyhow::Result<()> {
    use crate::app::types::RecordingWorkerMsg;

    let ch = channels.max(1) as usize;
    let frame_count = interleaved.len() / ch;
    let complete_samples = frame_count.saturating_mul(ch);
    let buffer_start_frame = writer.frames();
    writer.write_interleaved_f32(&interleaved[..complete_samples])?;

    let peak_l = (0..frame_count)
        .map(|i| interleaved[i * ch].abs())
        .fold(0.0f32, f32::max);
    let peak_r = if ch >= 2 {
        (0..frame_count)
            .map(|i| interleaved[i * ch + 1].abs())
            .fold(0.0f32, f32::max)
    } else {
        peak_l
    };
    let _ = tx.send(RecordingWorkerMsg::Level(peak_l, peak_r));

    for frame in 0..frame_count {
        let mono = (0..ch)
            .map(|channel| interleaved[frame * ch + channel])
            .sum::<f32>()
            / ch as f32;
        waveform.push(buffer_start_frame + frame as u64, mono);
    }
    waveform.send(false, tx);
    let _ = tx.send(RecordingWorkerMsg::WrittenFrames(writer.frames()));
    Ok(())
}

fn push_live_waveform_blocks(
    overview: &mut std::collections::VecDeque<(f32, f32)>,
    first_frame: &mut u64,
    block_frames: u64,
    capacity: usize,
    start_frame: u64,
    blocks: &[(f32, f32)],
) {
    if overview.is_empty() {
        *first_frame = start_frame;
    }
    overview.extend(blocks.iter().copied());
    let excess = overview.len().saturating_sub(capacity.max(1));
    if excess > 0 {
        overview.drain(..excess);
        *first_frame = first_frame.saturating_add(block_frames * excess as u64);
    }
}

/// Name for the next marker on a take: M01, M02, ...
pub(super) fn next_marker_label(existing: &[crate::markers::MarkerEntry]) -> String {
    format!("M{:02}", existing.len() + 1)
}

impl super::WavesPreviewer {
    pub(super) fn recording_refresh_devices(&mut self) {
        self.recording_tab.input_devices = crate::audio_capture::list_input_devices();
        self.audio_device_watch.last_default_input_id =
            crate::audio_capture::default_input_device_info().map(|info| info.id);
    }

    pub(super) fn start_recording(&mut self) {
        use crate::app::types::{RecordingSourceKind, RecordingState, RecordingWorkerMsg};
        use crate::audio_capture;

        let recording_command = Arc::new(AtomicU8::new(RECORDING_COMMAND_RUN));
        let command_worker = recording_command.clone();
        let paused = Arc::new(AtomicBool::new(false));
        let paused_worker = paused.clone();

        let (worker_tx, app_rx) = std::sync::mpsc::channel::<RecordingWorkerMsg>();
        // bounded channel for raw capture data (non-blocking callback)
        let (cap_tx, cap_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(256);
        // channel for cpal stream errors (device unplugged etc.)
        let (err_tx, err_rx) = std::sync::mpsc::channel::<String>();
        let overruns = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let source = self.recording_tab.source.clone();
        let mic_id = self.recording_tab.selected_mic_id.clone();

        // Start capture stream
        let capture_result = match source {
            RecordingSourceKind::Microphone => audio_capture::start_microphone_capture(
                mic_id.as_deref(),
                cap_tx.clone(),
                err_tx.clone(),
                overruns.clone(),
            ),
            // Mixing system audio with the microphone needs two synchronized
            // streams; not implemented. The UI disables this option.
            RecordingSourceKind::SystemAndMicrophone => Err(anyhow::anyhow!(
                "System + Mic recording is not implemented yet"
            )),
            #[cfg(target_os = "windows")]
            RecordingSourceKind::System => audio_capture::start_wasapi_loopback_capture(
                cap_tx.clone(),
                err_tx.clone(),
                overruns.clone(),
            ),
            #[cfg(not(target_os = "windows"))]
            RecordingSourceKind::System => Err(anyhow::anyhow!(
                "System audio capture is not supported on this platform"
            )),
        };

        let capture_stream = match capture_result {
            Ok(s) => s,
            Err(e) => {
                self.recording_tab.state = RecordingState::Error(e.to_string());
                return;
            }
        };

        let channels = capture_stream.channels;
        let sample_rate = capture_stream.sample_rate;
        let overview_block = live_waveform_block_frames(sample_rate);
        let live_markers = Arc::new(std::sync::Mutex::new(
            Vec::<crate::markers::MarkerEntry>::new(),
        ));
        let markers_worker = live_markers.clone();

        // Worker thread: drain cap_rx → write temp WAV, update level/waveform
        let worker_tx_clone = worker_tx.clone();
        std::thread::spawn(move || {
            // The worker owns the stream so Stop can close capture before it
            // drains every buffer already queued by the callback.
            let mut capture_stream = Some(capture_stream);

            let Some(tmp_path) =
                super::temp_audio_ops::allocate_neowaves_temp_cache_path("recording", "wav")
            else {
                let _ = worker_tx_clone.send(RecordingWorkerMsg::Error(
                    "create recording cache path failed".into(),
                ));
                return;
            };

            let mut writer = match crate::wav_stream::StreamingWaveWriter::create_float32(
                &tmp_path,
                channels,
                sample_rate,
            ) {
                Ok(w) => w,
                Err(e) => {
                    let _ = worker_tx_clone.send(RecordingWorkerMsg::Error(e.to_string()));
                    return;
                }
            };

            let mut waveform = WaveformAccumulator::new(overview_block);
            let mut last_checkpoint = std::time::Instant::now();
            let mut stopping = false;
            let mut partial_error: Option<String> = None;
            let mut error_reported = false;
            let mut writer_failed = false;

            loop {
                match command_worker.load(Ordering::Acquire) {
                    RECORDING_COMMAND_DISCARD => {
                        capture_stream.take();
                        drop(writer);
                        let _ = std::fs::remove_file(&tmp_path);
                        let _ = worker_tx_clone.send(RecordingWorkerMsg::Discarded);
                        return;
                    }
                    RECORDING_COMMAND_STOP if !stopping => {
                        capture_stream.take();
                        stopping = true;
                    }
                    _ => {}
                }
                if !stopping {
                    if let Ok(msg) = err_rx.try_recv() {
                        // Stream broke (e.g. device unplugged): report it and stop,
                        // finalizing whatever was captured so far.
                        let _ = worker_tx_clone.send(RecordingWorkerMsg::Error(msg.clone()));
                        error_reported = true;
                        partial_error = Some(msg);
                        capture_stream.take();
                        stopping = true;
                    }
                }
                let received = if stopping {
                    // Dropping the capture stream drops the last sender. A
                    // blocking receive therefore drains every already-queued
                    // callback buffer and then returns Disconnected, including
                    // a callback that was in flight when Stop was pressed.
                    cap_rx
                        .recv()
                        .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected)
                } else {
                    cap_rx.recv_timeout(RECORDING_COMMAND_POLL)
                };
                match received {
                    Ok(interleaved) => {
                        if paused_worker.load(Ordering::Relaxed) {
                            // Discard captured audio while paused: keep draining the
                            // bounded channel (so the capture callback never blocks)
                            // but don't write it or report level/waveform updates,
                            // so resuming continues the file with no gap or glitch.
                            continue;
                        }
                        if writer_failed {
                            continue;
                        }

                        if let Err(err) = write_recording_buffer(
                            &mut writer,
                            &interleaved,
                            channels,
                            &mut waveform,
                            &worker_tx_clone,
                        ) {
                            let message = format!("recording write failed: {err}");
                            let _ =
                                worker_tx_clone.send(RecordingWorkerMsg::Error(message.clone()));
                            error_reported = true;
                            writer_failed = true;
                            partial_error = Some(message);
                            capture_stream.take();
                            stopping = true;
                        } else if last_checkpoint.elapsed() >= RECORDING_CHECKPOINT_INTERVAL {
                            if let Err(err) = writer.checkpoint() {
                                let message = format!("recording checkpoint failed: {err}");
                                let _ = worker_tx_clone
                                    .send(RecordingWorkerMsg::Error(message.clone()));
                                error_reported = true;
                                writer_failed = true;
                                partial_error = Some(message);
                                capture_stream.take();
                                stopping = true;
                            }
                            last_checkpoint = std::time::Instant::now();
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if stopping {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        break;
                    }
                }
            }

            waveform.send(true, &worker_tx_clone);
            let frames_before_finalize = writer.frames();
            let final_state = match writer.finalize() {
                Ok(state) => Some(state),
                Err(err) => {
                    partial_error.get_or_insert_with(|| format!("finalize failed: {err}"));
                    None
                }
            };
            let frames = final_state
                .map(|state| state.frames)
                .unwrap_or(frames_before_finalize);
            let block_align = channels.max(1) as u64 * 4;
            if let Ok(metadata) = std::fs::metadata(&tmp_path) {
                let file_frames = metadata.len().saturating_sub(80) / block_align;
                if file_frames != frames {
                    partial_error.get_or_insert_with(|| {
                        format!(
                            "recording frame verification failed: writer={frames}, file={file_frames}"
                        )
                    });
                }
            }
            // Markers go in only once the file is final: the cue chunk is
            // written by rewriting the RIFF, which must not race the writer.
            // Past the inline-rewrite limit they land in a sidecar instead,
            // which `markers::read_markers` finds the same way.
            let markers = markers_worker
                .lock()
                .map(|m| m.clone())
                .unwrap_or_default();
            if !markers.is_empty() {
                let markers: Vec<_> = markers
                    .into_iter()
                    .filter(|m| (m.sample as u64) <= frames)
                    .collect();
                if let Err(err) =
                    crate::markers::write_markers(&tmp_path, sample_rate, sample_rate, &markers)
                {
                    partial_error.get_or_insert_with(|| format!("write markers failed: {err}"));
                }
            }
            if !error_reported {
                if let Some(error) = partial_error.as_ref() {
                    let _ = worker_tx_clone.send(RecordingWorkerMsg::Error(error.clone()));
                }
            }
            let _ = worker_tx_clone.send(RecordingWorkerMsg::Finalized {
                path: tmp_path,
                frames,
                partial: partial_error.is_some(),
            });
        });

        let display_name = format!(
            "Recording {}.wav",
            chrono::Local::now().format("%Y-%m-%d %H-%M-%S")
        );
        let take_id = self.recording_tab.next_take_id;
        self.recording_tab.next_take_id += 1;
        self.recording_tab.takes.push(crate::app::types::RecordingTake {
            id: take_id,
            display_name: display_name.clone(),
            temp_path: None,
            item: None,
            frames: 0,
            sample_rate,
            markers: Vec::new(),
        });
        self.recording_tab.current_take = Some(take_id);
        self.recording_tab.live_markers = live_markers;

        self.recording_tab.state = RecordingState::Recording;
        self.recording_tab.recording_command = recording_command;
        self.recording_tab.paused = paused;
        self.recording_tab.paused.store(false, Ordering::Relaxed);
        self.recording_tab.pause_started_at = None;
        self.recording_tab.paused_accum = std::time::Duration::ZERO;
        self.recording_tab.rx = Some(app_rx);
        self.recording_tab.record_start = Some(std::time::Instant::now());
        self.recording_tab.waveform_overview.clear();
        self.recording_tab.waveform_start_frame = 0;
        self.recording_tab.overview_block_secs = overview_block as f32 / sample_rate.max(1) as f32;
        self.recording_tab.written_frames = 0;
        self.recording_tab.recording_sample_rate = sample_rate;
        self.recording_tab.recording_display_name = display_name;
        self.recording_tab.level_l = 0.0;
        self.recording_tab.level_r = 0.0;
        self.recording_tab.peak_hold_l = 0.0;
        self.recording_tab.peak_hold_r = 0.0;
        self.recording_tab.peak_hold_l_at = None;
        self.recording_tab.peak_hold_r_at = None;
        self.recording_tab.overrun_count = overruns;
        self.recording_tab.elapsed_secs = 0.0;
        self.recording_tab.progress_message = "Recording…".to_string();
    }

    pub(super) fn stop_recording(&mut self) {
        use crate::app::types::RecordingState;
        self.recording_tab
            .recording_command
            .store(RECORDING_COMMAND_STOP, Ordering::Release);
        self.recording_tab.state = RecordingState::Finalizing;
        self.recording_tab.progress_message = "Finalizing…".to_string();
    }

    pub(super) fn pause_recording(&mut self) {
        use crate::app::types::RecordingState;
        if self.recording_tab.state != RecordingState::Recording {
            return;
        }
        self.recording_tab.paused.store(true, Ordering::Relaxed);
        self.recording_tab.pause_started_at = Some(std::time::Instant::now());
        self.recording_tab.state = RecordingState::Paused;
        self.recording_tab.progress_message = "Paused".to_string();
    }

    pub(super) fn resume_recording(&mut self) {
        use crate::app::types::RecordingState;
        if self.recording_tab.state != RecordingState::Paused {
            return;
        }
        if let Some(started) = self.recording_tab.pause_started_at.take() {
            self.recording_tab.paused_accum += started.elapsed();
        }
        self.recording_tab.paused.store(false, Ordering::Relaxed);
        self.recording_tab.state = RecordingState::Recording;
        self.recording_tab.progress_message = "Recording…".to_string();
    }

    /// Throws away the take being captured. Takes that already stopped are
    /// discarded with `discard_take`.
    pub(super) fn discard_recording(&mut self) {
        use crate::app::types::RecordingState;
        if !matches!(
            self.recording_tab.state,
            RecordingState::Recording | RecordingState::Paused
        ) {
            self.recording_tab.confirm_discard = false;
            return;
        }
        if let Some(take_id) = self.recording_tab.current_take.take() {
            self.recording_tab.takes.retain(|take| take.id != take_id);
        }
        if let Ok(mut markers) = self.recording_tab.live_markers.lock() {
            markers.clear();
        }
        self.recording_tab
            .recording_command
            .store(RECORDING_COMMAND_DISCARD, Ordering::Release);
        self.recording_tab.paused.store(false, Ordering::Relaxed);
        self.recording_tab.pause_started_at = None;
        self.recording_tab.paused_accum = std::time::Duration::ZERO;
        self.recording_tab.state = RecordingState::Finalizing;
        self.recording_tab.last_recording_path = None;
        self.recording_tab.waveform_overview.clear();
        self.recording_tab.waveform_start_frame = 0;
        self.recording_tab.confirm_discard = false;
        self.recording_tab.level_l = 0.0;
        self.recording_tab.level_r = 0.0;
        self.recording_tab.peak_hold_l = 0.0;
        self.recording_tab.peak_hold_r = 0.0;
        self.recording_tab.progress_message = "Discarding recording...".to_string();
    }

    /// Where a take stands, read off its list row. `None` once the row is gone
    /// (removed from the list), which retires the take.
    pub(super) fn recording_take_state(
        &self,
        take: &crate::app::types::RecordingTake,
    ) -> Option<crate::app::types::RecordingTakeState> {
        use crate::app::types::{RecordingState, RecordingTakeState};
        if self.recording_tab.current_take == Some(take.id) {
            // A worker that failed before finalizing leaves nothing behind;
            // the take would otherwise read "Recording" forever.
            let capturing = matches!(
                self.recording_tab.state,
                RecordingState::Recording | RecordingState::Paused | RecordingState::Finalizing
            );
            if capturing && take.item.is_none() {
                return Some(RecordingTakeState::Recording);
            }
        }
        let item = self.item_for_id(take.item?)?;
        Some(match item.source {
            MediaSource::Virtual => RecordingTakeState::Stopped,
            _ => RecordingTakeState::Saved(item.path.clone()),
        })
    }

    /// Drops takes whose row has left the list, so removing the row there
    /// removes the take here. A handful of hash lookups; safe per frame.
    pub(super) fn prune_recording_takes(&mut self) {
        let keep: Vec<bool> = self
            .recording_tab
            .takes
            .iter()
            .map(|take| self.recording_take_state(take).is_some())
            .collect();
        let mut keep = keep.into_iter();
        self.recording_tab
            .takes
            .retain(|_| keep.next().unwrap_or(true));
    }

    fn recording_take_path(&self, take_id: u64) -> Option<PathBuf> {
        let take = self.recording_tab.takes.iter().find(|t| t.id == take_id)?;
        self.item_for_id(take.item?).map(|item| item.path.clone())
    }

    /// Drops a marker on the take being captured, at the frame written so far.
    pub(super) fn add_recording_marker(&mut self) {
        use crate::app::types::RecordingState;
        if !matches!(
            self.recording_tab.state,
            RecordingState::Recording | RecordingState::Paused
        ) {
            return;
        }
        let sample = self.recording_tab.written_frames as usize;
        let Ok(mut markers) = self.recording_tab.live_markers.lock() else {
            return;
        };
        // A second press on the same frame (paused, or a double click) would
        // only stack an invisible duplicate.
        if markers.iter().any(|m| m.sample == sample) {
            return;
        }
        let label = next_marker_label(&markers);
        markers.push(crate::markers::MarkerEntry { sample, label });
    }

    pub(super) fn open_recording_take_in_editor(&mut self, take_id: u64) {
        let Some(path) = self.recording_take_path(take_id) else {
            return;
        };
        self.open_or_activate_tab(&path);
        self.workspace_view = WorkspaceView::Editor;
    }

    /// Saves a stopped take where the user says. It goes through the list's
    /// save job, so the take's `(virtual)` row becomes the saved file's row --
    /// one row, not a virtual row plus a copy.
    pub(super) fn save_recording_take_as(&mut self, take_id: u64) {
        let Some(path) = self.recording_take_path(take_id) else {
            return;
        };
        if !self.is_virtual_path(&path) {
            return;
        }
        let default_name = self
            .item_for_path(&path)
            .map(|item| item.display_name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Recording.wav".to_string());
        let Some(dest) = self.pick_recording_save_dialog(&default_name) else {
            return;
        };
        let dest = if dest.extension().is_some() {
            dest
        } else {
            dest.with_extension("wav")
        };
        self.spawn_save_virtual_as(path, dest);
    }

    /// Discards a stopped take: its `(virtual)` row goes with it (undoable
    /// from the list). A saved take only leaves this list; its file and row
    /// stay.
    pub(super) fn discard_recording_take(&mut self, take_id: u64) {
        use crate::app::types::RecordingTakeState;
        self.recording_tab.confirm_discard_take = None;
        let Some(take) = self
            .recording_tab
            .takes
            .iter()
            .find(|t| t.id == take_id)
            .cloned()
        else {
            return;
        };
        if self.recording_take_state(&take) == Some(RecordingTakeState::Stopped) {
            if let Some(path) = self.recording_take_path(take_id) {
                self.remove_paths_from_list_with_undo(&[path]);
            }
        }
        self.recording_tab.takes.retain(|t| t.id != take_id);
    }

    pub(super) fn drain_recording_events(&mut self) {
        use crate::app::types::{RecordingState, RecordingWorkerMsg};

        let mut finalized_path: Option<std::path::PathBuf> = None;
        let mut error_msg: Option<String> = None;

        let Some(rx) = &self.recording_tab.rx else {
            return;
        };

        loop {
            match rx.try_recv() {
                Ok(msg) => match msg {
                    RecordingWorkerMsg::Level(l, r) => {
                        // exponential decay + peak hold
                        self.recording_tab.level_l = self.recording_tab.level_l * 0.85 + l * 0.15;
                        self.recording_tab.level_r = self.recording_tab.level_r * 0.85 + r * 0.15;
                    }
                    RecordingWorkerMsg::WaveformBlocks {
                        start_frame,
                        block_frames,
                        blocks,
                        end_frame,
                    } => {
                        let capacity = (LIVE_WAVEFORM_WINDOW_SECS
                            / self.recording_tab.overview_block_secs.max(0.0001))
                        .ceil()
                        .max(1.0) as usize;
                        push_live_waveform_blocks(
                            &mut self.recording_tab.waveform_overview,
                            &mut self.recording_tab.waveform_start_frame,
                            block_frames,
                            capacity,
                            start_frame,
                            &blocks,
                        );
                        self.recording_tab.written_frames =
                            self.recording_tab.written_frames.max(end_frame);
                    }
                    RecordingWorkerMsg::WrittenFrames(frames) => {
                        self.recording_tab.written_frames = frames;
                        self.recording_tab.elapsed_secs =
                            frames as f32 / self.recording_tab.recording_sample_rate.max(1) as f32;
                    }
                    RecordingWorkerMsg::Finalized {
                        path,
                        frames,
                        partial,
                    } => {
                        self.recording_tab.written_frames = frames;
                        self.recording_tab.elapsed_secs =
                            frames as f32 / self.recording_tab.recording_sample_rate.max(1) as f32;
                        if partial && error_msg.is_none() {
                            error_msg =
                                Some("Recording ended with a recoverable partial take".into());
                        }
                        finalized_path = Some(path);
                    }
                    RecordingWorkerMsg::Discarded => {
                        self.recording_tab.state = RecordingState::Idle;
                        self.recording_tab.current_take = None;
                        self.recording_tab.last_recording_path = None;
                        self.recording_tab.rx = None;
                        self.recording_tab.progress_message.clear();
                        break;
                    }
                    RecordingWorkerMsg::Error(msg) => {
                        error_msg = Some(msg);
                    }
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if self.recording_tab.state == RecordingState::Finalizing {
                        // worker done without sending Finalized? treat as error
                        if finalized_path.is_none() {
                            self.recording_tab.state =
                                RecordingState::Error("Worker disconnected".to_string());
                        }
                    }
                    break;
                }
            }
        }

        if let Some(msg) = error_msg {
            self.recording_tab.state = RecordingState::Error(msg);
        }
        if let Some(path) = finalized_path {
            self.recording_tab.last_recording_path = Some(path.clone());
            self.attach_finished_take(&path);
            if matches!(self.recording_tab.state, RecordingState::Error(_)) {
                // Keep the error visible; the partial take is still available.
                self.recording_tab.progress_message =
                    "Recording stopped — partial take saved".to_string();
            } else {
                self.recording_tab.state = RecordingState::Idle;
                self.recording_tab.progress_message = "Recording ready".to_string();
            }
            // Worker thread has exited and dropped its sender; drop the now-dead
            // receiver so we stop polling a disconnected channel every frame.
            self.recording_tab.rx = None;
        }
    }

    /// A take just stopped: it becomes a `(virtual)` row in the list right
    /// away, and the take remembers that row.
    pub(super) fn attach_finished_take(&mut self, tmp_path: &std::path::Path) {
        let in_flight = self
            .recording_tab
            .current_take
            .take()
            .filter(|id| self.recording_tab.takes.iter().any(|t| t.id == *id));
        let take_id = match in_flight {
            Some(id) => id,
            None => {
                // No take to attach to: a test injecting a finished file, or
                // a partial take whose entry was pruned while the worker
                // reported an error before finalizing. Stand one up.
                let id = self.recording_tab.next_take_id;
                self.recording_tab.next_take_id += 1;
                let display_name = if self.recording_tab.recording_display_name.is_empty() {
                    "Recording.wav".to_string()
                } else {
                    self.recording_tab.recording_display_name.clone()
                };
                self.recording_tab.takes.push(crate::app::types::RecordingTake {
                    id,
                    display_name,
                    temp_path: None,
                    item: None,
                    frames: self.recording_tab.written_frames,
                    sample_rate: self.recording_tab.recording_sample_rate,
                    markers: Vec::new(),
                });
                id
            }
        };
        let markers = self
            .recording_tab
            .live_markers
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        let item_path = self.ensure_virtual_item_for_recording(tmp_path);
        let item_id = item_path.as_ref().and_then(|p| self.path_index.get(p));
        let frames = self.recording_tab.written_frames;
        let row_name = item_path
            .as_ref()
            .and_then(|p| self.item_for_path(p))
            .map(|item| item.display_name.clone());
        if let Some(take) = self
            .recording_tab
            .takes
            .iter_mut()
            .find(|take| take.id == take_id)
        {
            take.temp_path = Some(tmp_path.to_path_buf());
            take.item = item_id;
            take.frames = frames;
            take.markers = markers;
            if let Some(name) = row_name {
                take.display_name = name;
            }
        }
    }

    /// Wraps a recorded temp WAV as a `(virtual)` list item (in-memory audio +
    /// `VirtualSourceRef::FilePath` pointing at the temp WAV). Returns the new
    /// item's `__virtual__` path, reusing an existing item if one was already
    /// created for this recording.
    pub(super) fn ensure_virtual_item_for_recording(
        &mut self,
        tmp_path: &std::path::Path,
    ) -> Option<PathBuf> {
        use crate::app::types::{VirtualSourceRef, VirtualState};

        if let Some(item) = self.items.iter().find(|item| {
            item.source == MediaSource::Virtual
                && item
                    .virtual_state
                    .as_ref()
                    .map(|s| matches!(&s.source, VirtualSourceRef::FilePath(p) if p == tmp_path))
                    .unwrap_or(false)
        }) {
            return Some(item.path.clone());
        }

        let asset = crate::audio_asset::AudioAssetDescriptor::managed(tmp_path.to_path_buf());
        let sample_rate = asset.sample_rate.max(1);
        let bits_per_sample = asset.bits_per_sample.max(1);
        let logical_name = if self.recording_tab.recording_display_name.is_empty() {
            "Recording.wav"
        } else {
            self.recording_tab.recording_display_name.as_str()
        };
        let name = self.unique_virtual_display_name(logical_name);
        let virtual_state = Some(VirtualState {
            source: VirtualSourceRef::FilePath(tmp_path.to_path_buf()),
            op_chain: Vec::new(),
            sample_rate,
            channels: asset.channels.max(1),
            bits_per_sample,
        });
        let item = self.make_virtual_item_with_asset(name, asset, None, None, virtual_state);
        let item_path = item.path.clone();
        let before = self.capture_list_selection_snapshot();
        self.add_virtual_item(item, None);
        self.after_add_refresh();
        self.record_list_insert_from_paths(&[item_path.clone()], before);
        self.recording_temp_files.push(tmp_path.to_path_buf());
        Some(item_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_waveform_scrolls_one_block_without_jump() {
        let mut overview = std::collections::VecDeque::new();
        let mut first_frame = 0u64;
        let block_frames = 120u64;
        let capacity = 4_000usize;
        for index in 0..5_000u64 {
            let previous_first = first_frame;
            push_live_waveform_blocks(
                &mut overview,
                &mut first_frame,
                block_frames,
                capacity,
                index * block_frames,
                &[(-0.5, 0.5)],
            );
            assert!(overview.len() <= capacity);
            assert!(first_frame.saturating_sub(previous_first) <= block_frames);
        }
        assert_eq!(overview.len(), capacity);
        assert_eq!(first_frame, 1_000 * block_frames);
        assert_eq!(overview.len() as u64 * block_frames, 10 * 48_000);
    }

    #[test]
    fn a_batch_larger_than_the_window_keeps_only_its_newest_blocks() {
        let mut overview = std::collections::VecDeque::new();
        let mut first_frame = 0u64;
        let blocks: Vec<(f32, f32)> = (0..10).map(|i| (i as f32, i as f32)).collect();
        push_live_waveform_blocks(&mut overview, &mut first_frame, 100, 4, 0, &blocks);
        assert_eq!(overview.len(), 4);
        assert_eq!(overview.front(), Some(&(6.0, 6.0)));
        assert_eq!(first_frame, 600);
    }

    #[test]
    fn block_size_gives_about_400_blocks_per_second() {
        assert_eq!(live_waveform_block_frames(48_000), 120);
        assert_eq!(live_waveform_block_frames(44_100), 110);
        assert_eq!(live_waveform_block_frames(8_000), 64);
    }

    #[test]
    fn accumulator_reports_contiguous_frames_across_buffers() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut acc = WaveformAccumulator::new(4);
        // Two buffers of 6 frames: the second block straddles them.
        for f in 0..6u64 {
            acc.push(f, f as f32);
        }
        acc.send(false, &tx);
        for f in 6..12u64 {
            acc.push(f, -(f as f32));
        }
        acc.send(false, &tx);
        acc.send(true, &tx);
        let mut next = 0u64;
        let mut all = Vec::new();
        while let Ok(crate::app::types::RecordingWorkerMsg::WaveformBlocks {
            start_frame,
            block_frames,
            blocks,
            end_frame,
        }) = rx.try_recv()
        {
            assert_eq!(start_frame, next, "blocks must not overlap or leave gaps");
            assert_eq!(block_frames, 4);
            next = end_frame;
            all.extend(blocks);
        }
        assert_eq!(all.len(), 3);
        assert_eq!(all[0], (0.0, 3.0));
        assert_eq!(all[1], (-7.0, 5.0));
        assert_eq!(all[2], (-11.0, -8.0));
    }

    #[test]
    fn marker_labels_count_up() {
        let mut markers = Vec::new();
        for sample in [10usize, 20, 30] {
            let label = next_marker_label(&markers);
            markers.push(crate::markers::MarkerEntry { sample, label });
        }
        let labels: Vec<_> = markers.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, ["M01", "M02", "M03"]);
    }
}
