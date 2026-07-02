use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::*;

impl super::WavesPreviewer {
    pub(super) fn recording_refresh_devices(&mut self) {
        self.recording_tab.input_devices = crate::audio_capture::list_input_devices();
        self.audio_device_watch.last_default_input_id =
            crate::audio_capture::default_input_device_info().map(|info| info.id);
    }

    pub(super) fn start_recording(&mut self) {
        use crate::app::types::{RecordingSourceKind, RecordingState, RecordingWorkerMsg};
        use crate::audio_capture;

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = cancel.clone();
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
            // keep capture stream alive in this thread
            let _cap = capture_stream;

            let Some(tmp_path) =
                super::temp_audio_ops::allocate_neowaves_temp_cache_path("recording", "wav")
            else {
                let _ = worker_tx_clone.send(RecordingWorkerMsg::Error(
                    "create recording cache path failed".into(),
                ));
                return;
            };

            let spec = hound::WavSpec {
                channels,
                sample_rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            };
            let mut writer = match hound::WavWriter::create(&tmp_path, spec) {
                Ok(w) => w,
                Err(e) => {
                    let _ = worker_tx_clone.send(RecordingWorkerMsg::Error(e.to_string()));
                    return;
                }
            };

            let mut waveform_buf: Vec<f32> = Vec::new();
            let overview_block = (sample_rate as usize / 50).max(512); // ~50 blocks/s

            loop {
                if cancel_worker.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(msg) = err_rx.try_recv() {
                    // Stream broke (e.g. device unplugged): report it and stop,
                    // finalizing whatever was captured so far.
                    let _ = worker_tx_clone.send(RecordingWorkerMsg::Error(msg));
                    break;
                }
                match cap_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(interleaved) => {
                        if paused_worker.load(Ordering::Relaxed) {
                            // Discard captured audio while paused: keep draining the
                            // bounded channel (so the capture callback never blocks)
                            // but don't write it or report level/waveform updates,
                            // so resuming continues the file with no gap or glitch.
                            continue;
                        }

                        // Write samples
                        for &s in &interleaved {
                            let _ = writer.write_sample(s);
                        }

                        // Level (peak L/R)
                        let ch = channels as usize;
                        let n = interleaved.len() / ch.max(1);
                        let peak_l = (0..n)
                            .map(|i| interleaved[i * ch].abs())
                            .fold(0.0f32, f32::max);
                        let peak_r = if ch >= 2 {
                            (0..n)
                                .map(|i| interleaved[i * ch + 1].abs())
                                .fold(0.0f32, f32::max)
                        } else {
                            peak_l
                        };
                        let _ = worker_tx_clone.send(RecordingWorkerMsg::Level(peak_l, peak_r));

                        // Waveform overview (mono downmix)
                        for i in 0..n {
                            let mono =
                                (0..ch).map(|c| interleaved[i * ch + c]).sum::<f32>() / ch as f32;
                            waveform_buf.push(mono);
                            if waveform_buf.len() >= overview_block {
                                let min = waveform_buf.iter().cloned().fold(f32::MAX, f32::min);
                                let max = waveform_buf.iter().cloned().fold(f32::MIN, f32::max);
                                let _ = worker_tx_clone
                                    .send(RecordingWorkerMsg::WaveformBlock(min, max));
                                waveform_buf.clear();
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // normal idle, loop
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        break;
                    }
                }
            }

            // Finalize WAV
            match writer.finalize() {
                Ok(_) => {
                    let _ = worker_tx_clone.send(RecordingWorkerMsg::Finalized(tmp_path));
                }
                Err(e) => {
                    let _ = worker_tx_clone
                        .send(RecordingWorkerMsg::Error(format!("finalize failed: {e}")));
                }
            }
        });

        let overview_block = (sample_rate as usize / 50).max(512);

        self.recording_tab.state = RecordingState::Recording;
        self.recording_tab.cancel = cancel;
        self.recording_tab.paused = paused;
        self.recording_tab.paused.store(false, Ordering::Relaxed);
        self.recording_tab.pause_started_at = None;
        self.recording_tab.paused_accum = std::time::Duration::ZERO;
        self.recording_tab.rx = Some(app_rx);
        self.recording_tab.record_start = Some(std::time::Instant::now());
        self.recording_tab.waveform_overview.clear();
        self.recording_tab.overview_block_secs = overview_block as f32 / sample_rate.max(1) as f32;
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
        self.recording_tab.cancel.store(true, Ordering::Relaxed);
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
        self.recording_tab.cancel.store(true, Ordering::Relaxed);
        self.recording_tab.paused.store(false, Ordering::Relaxed);
        self.recording_tab.pause_started_at = None;
        self.recording_tab.paused_accum = std::time::Duration::ZERO;
        self.recording_tab.state = RecordingState::Idle;
        self.recording_tab.last_recording_path = None;
        self.recording_tab.rx = None;
        self.recording_tab.waveform_overview.clear();
        self.recording_tab.confirm_discard = false;
        self.recording_tab.level_l = 0.0;
        self.recording_tab.level_r = 0.0;
        self.recording_tab.peak_hold_l = 0.0;
        self.recording_tab.peak_hold_r = 0.0;
        self.recording_tab.progress_message = String::new();
    }

    /// Copies the finished temp recording to a user-chosen location.
    pub(super) fn save_recording_as(&mut self) {
        use crate::app::types::RecordingState;
        let Some(src) = self.recording_tab.last_recording_path.clone() else {
            return;
        };
        let default_name = src
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("recording.wav");
        let Some(dest) = self.pick_recording_save_dialog(default_name) else {
            return;
        };
        let dest = if dest.extension().is_none() {
            dest.with_extension("wav")
        } else {
            dest
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
            // update elapsed
            if self.recording_tab.state == RecordingState::Recording {
                if let Some(start) = self.recording_tab.record_start {
                    self.recording_tab.elapsed_secs = start
                        .elapsed()
                        .saturating_sub(self.recording_tab.paused_accum)
                        .as_secs_f32();
                }
            }
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
                    RecordingWorkerMsg::WaveformBlock(min, max) => {
                        self.recording_tab.waveform_overview.push((min, max));
                        // cap overview length
                        if self.recording_tab.waveform_overview.len() > 2000 {
                            self.recording_tab.waveform_overview.drain(0..500);
                        }
                    }
                    RecordingWorkerMsg::Finalized(path) => {
                        finalized_path = Some(path);
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

        // update elapsed
        if self.recording_tab.state == RecordingState::Recording {
            if let Some(start) = self.recording_tab.record_start {
                self.recording_tab.elapsed_secs = start
                    .elapsed()
                    .saturating_sub(self.recording_tab.paused_accum)
                    .as_secs_f32();
            }
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

        let (audio, sample_rate, bits_per_sample) = self.decode_audio_for_virtual(tmp_path)?;
        let stem = tmp_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("recording");
        let name = self.unique_virtual_display_name(&format!("{stem}.wav"));
        let virtual_state = Some(VirtualState {
            source: VirtualSourceRef::FilePath(tmp_path.to_path_buf()),
            op_chain: Vec::new(),
            sample_rate,
            channels: audio.channels.len().max(1) as u16,
            bits_per_sample,
        });
        let item = self.make_virtual_item(name, audio, sample_rate, bits_per_sample, virtual_state);
        let item_path = item.path.clone();
        let before = self.capture_list_selection_snapshot();
        self.add_virtual_item(item, None);
        self.after_add_refresh();
        self.record_list_insert_from_paths(&[item_path.clone()], before);
        self.recording_temp_files.push(tmp_path.to_path_buf());
        Some(item_path)
    }
}
