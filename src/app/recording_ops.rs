use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use super::*;

const RECORDING_COMMAND_RUN: u8 = 0;
const RECORDING_COMMAND_STOP: u8 = 1;
const RECORDING_COMMAND_DISCARD: u8 = 2;
const LIVE_WAVEFORM_WINDOW_SECS: f32 = 40.0;

fn write_recording_buffer(
    writer: &mut crate::wav_stream::StreamingWaveWriter,
    interleaved: &[f32],
    channels: u16,
    overview_block: usize,
    waveform_buf: &mut Vec<f32>,
    waveform_start_frame: &mut Option<u64>,
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
        if waveform_buf.is_empty() {
            *waveform_start_frame = Some(buffer_start_frame + frame as u64);
        }
        let mono = (0..ch)
            .map(|channel| interleaved[frame * ch + channel])
            .sum::<f32>()
            / ch as f32;
        waveform_buf.push(mono);
        if waveform_buf.len() >= overview_block {
            flush_recording_waveform(waveform_buf, waveform_start_frame, tx);
        }
    }
    let _ = tx.send(RecordingWorkerMsg::WrittenFrames(writer.frames()));
    Ok(())
}

fn flush_recording_waveform(
    waveform_buf: &mut Vec<f32>,
    waveform_start_frame: &mut Option<u64>,
    tx: &std::sync::mpsc::Sender<crate::app::types::RecordingWorkerMsg>,
) {
    use crate::app::types::RecordingWorkerMsg;
    if waveform_buf.is_empty() {
        return;
    }
    let start_frame = waveform_start_frame.take().unwrap_or(0);
    let end_frame = start_frame.saturating_add(waveform_buf.len() as u64);
    let min = waveform_buf.iter().copied().fold(f32::MAX, f32::min);
    let max = waveform_buf.iter().copied().fold(f32::MIN, f32::max);
    let _ = tx.send(RecordingWorkerMsg::WaveformBlock {
        min,
        max,
        start_frame,
        end_frame,
    });
    waveform_buf.clear();
}

fn push_live_waveform_block(
    overview: &mut std::collections::VecDeque<(f32, f32)>,
    first_frame: &mut u64,
    block_frames: u64,
    capacity: usize,
    start_frame: u64,
    value: (f32, f32),
) {
    if overview.is_empty() {
        *first_frame = start_frame;
    }
    overview.push_back(value);
    while overview.len() > capacity.max(1) {
        overview.pop_front();
        *first_frame = first_frame.saturating_add(block_frames);
    }
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

            let mut waveform_buf: Vec<f32> = Vec::new();
            let mut waveform_start_frame: Option<u64> = None;
            let overview_block = (sample_rate as usize / 50).max(512); // ~50 blocks/s
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
                    cap_rx.recv_timeout(std::time::Duration::from_millis(100))
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
                            overview_block,
                            &mut waveform_buf,
                            &mut waveform_start_frame,
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
                        } else if last_checkpoint.elapsed() >= std::time::Duration::from_secs(1) {
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

            flush_recording_waveform(
                &mut waveform_buf,
                &mut waveform_start_frame,
                &worker_tx_clone,
            );
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

        let overview_block = (sample_rate as usize / 50).max(512);

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
        self.recording_tab.recording_display_name = format!(
            "Recording {}.wav",
            chrono::Local::now().format("%Y-%m-%d %H-%M-%S")
        );
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

    pub(super) fn discard_recording(&mut self) {
        use crate::app::types::RecordingState;
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

    /// Copies the finished temp recording to a user-chosen location.
    pub(super) fn save_recording_as(&mut self) {
        use crate::app::types::RecordingState;
        let Some(src) = self.recording_tab.last_recording_path.clone() else {
            return;
        };
        let default_name = if self.recording_tab.recording_display_name.is_empty() {
            "Recording.wav".to_string()
        } else {
            self.recording_tab.recording_display_name.clone()
        };
        let Some(dest) = self.pick_recording_save_dialog(&default_name) else {
            return;
        };
        // The temp take is a WAV byte stream; copying it under another
        // extension would produce a mislabeled, undecodable file.
        let is_wav = dest
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("wav"))
            .unwrap_or(false);
        let dest = if is_wav {
            dest
        } else {
            dest.with_extension("wav")
        };
        match std::fs::copy(&src, &dest) {
            Ok(_) => {
                self.recording_tab.progress_message = format!("Saved: {}", dest.display());
            }
            Err(err) => {
                self.recording_tab.state = RecordingState::Error(format!("save failed: {err}"));
            }
        }
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
                    RecordingWorkerMsg::WaveformBlock {
                        min,
                        max,
                        start_frame,
                        end_frame,
                    } => {
                        let capacity = (LIVE_WAVEFORM_WINDOW_SECS
                            / self.recording_tab.overview_block_secs.max(0.0001))
                        .ceil()
                        .max(1.0) as usize;
                        let block_frames = (self.recording_tab.overview_block_secs
                            * self.recording_tab.recording_sample_rate.max(1) as f32)
                            .round() as u64;
                        push_live_waveform_block(
                            &mut self.recording_tab.waveform_overview,
                            &mut self.recording_tab.waveform_start_frame,
                            block_frames,
                            capacity,
                            start_frame,
                            (min, max),
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

    pub(super) fn open_recording_in_editor(&mut self, _ctx: &egui::Context) {
        let Some(tmp_path) = self.recording_tab.last_recording_path.clone() else {
            return;
        };
        let Some(item_path) = self.ensure_virtual_item_for_recording(&tmp_path) else {
            return;
        };
        self.open_or_activate_tab(&item_path);
        self.workspace_view = WorkspaceView::Editor;
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
    fn live_waveform_scrolls_one_block_without_500_point_jump() {
        let mut overview = std::collections::VecDeque::new();
        let mut first_frame = 0u64;
        let block_frames = 960u64;
        let capacity = 2_000usize;
        for index in 0..2_500u64 {
            let previous_first = first_frame;
            push_live_waveform_block(
                &mut overview,
                &mut first_frame,
                block_frames,
                capacity,
                index * block_frames,
                (-0.5, 0.5),
            );
            assert!(overview.len() <= capacity);
            assert!(first_frame.saturating_sub(previous_first) <= block_frames);
        }
        assert_eq!(overview.len(), capacity);
        assert_eq!(first_frame, 500 * block_frames);
        assert_eq!(overview.len() as u64 * block_frames, 40 * 48_000);
    }
}
