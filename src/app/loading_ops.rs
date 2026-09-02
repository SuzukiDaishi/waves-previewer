use egui::{Color32, RichText};

use super::types::{ProcessingResult, ProcessingTarget};
use super::BULK_RESAMPLE_BLOCK_SECS;

/// How long a job may hold the modal overlay before it offers a way out.
///
/// An export of a thousand files to a shared drive is slow, not stuck, so this
/// is generous. It is still finite because a job that will never answer is
/// indistinguishable, from the other side of the overlay, from an application
/// that has died.
const BUSY_OVERLAY_STALL_SECS: u64 = 30;

/// What a job's channel says this frame.
///
/// The third case is the one worth naming. A worker that returns or panics
/// without sending drops its sender, and every `try_recv` after that answers
/// `Disconnected` -- forever. Folded into "nothing yet" (`Err(_) => None`,
/// which is how these drains used to read) it becomes a job that never
/// completes: the modal overlay stays up with input blocked, the repaint
/// cadence stays pinned at 50ms or 60fps, and the quit prompt sits
/// underneath the overlay where it cannot be clicked. The state has to be
/// cleared by whoever polls it, so the poll has to be able to say so.
pub(in crate::app) enum JobPoll<T> {
    /// Still working. Ask again next frame.
    Waiting,
    /// The worker's answer.
    Ready(T),
    /// The sender is gone: nothing will ever arrive on this channel.
    Gone,
}

/// Poll a worker channel without losing the difference between "not yet" and
/// "never" -- see [`JobPoll`].
pub(in crate::app) fn poll_job<T>(rx: &std::sync::mpsc::Receiver<T>) -> JobPoll<T> {
    match rx.try_recv() {
        Ok(value) => JobPoll::Ready(value),
        Err(std::sync::mpsc::TryRecvError::Empty) => JobPoll::Waiting,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => JobPoll::Gone,
    }
}

impl super::WavesPreviewer {
    pub(super) fn tick_playback_fx_state(&mut self, ctx: &egui::Context) {
        let mut ready_result: Option<super::PlaybackFxResult> = None;
        let mut disconnected = false;
        if let Some(state) = &mut self.playback_fx_state {
            match state.rx.try_recv() {
                Ok(result) => ready_result = Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => disconnected = true,
            }
        }
        if disconnected {
            self.playback_fx_state = None;
            ctx.request_repaint();
            return;
        }
        let Some(result) = ready_result else {
            return;
        };
        let Some(state) = self.playback_fx_state.take() else {
            return;
        };
        let valid = state.job_id == result.job_id
            && state.source_generation == result.source_generation
            && state.source == result.source
            && state.mode == result.mode
            && (state.playback_rate - result.playback_rate).abs() <= 1.0e-6
            && (state.pitch_semitones - result.pitch_semitones).abs() <= 1.0e-6
            && state.source_generation == self.playback_source_generation
            && state.source == self.playback_session.source
            && state.mode == self.mode
            && (state.playback_rate - self.playback_rate).abs() <= 1.0e-6
            && (state.pitch_semitones - self.pitch_semitones).abs() <= 1.0e-6;
        if !valid {
            ctx.request_repaint();
            return;
        }
        let source_time_sec = self.playback_current_source_time_sec();
        self.apply_ready_playback_fx_audio(
            result.source,
            result.audio,
            result.buffer_sr,
            result.mode,
            result.playback_rate,
            source_time_sec,
            state.autoplay_when_ready,
        );
        ctx.request_repaint();
    }

    pub(super) fn tick_processing_state(&mut self, ctx: &egui::Context) {
        let mut processing_done: Option<(ProcessingResult, bool)> = None;
        let mut source_time_sec = None;
        let mut worker_gone = false;
        if let Some(state) = &mut self.processing {
            match poll_job(&state.rx) {
                JobPoll::Ready(res) => {
                    source_time_sec = state.source_time_sec;
                    processing_done = Some((res, state.autoplay_when_ready));
                }
                JobPoll::Waiting => {}
                JobPoll::Gone => worker_gone = true,
            }
        }
        if worker_gone {
            // Nothing to show the user: this is a preview decode, and the next
            // selection starts a new one. What matters is that the state does
            // not sit here holding the frame rate at 60fps and refusing every
            // later decode of the same target.
            self.debug_log("processing worker stopped without a result".to_string());
            self.processing = None;
            ctx.request_repaint();
            return;
        }
        if let Some((res, autoplay_when_ready)) = processing_done {
            if let Some(reason) = self
                .processing
                .as_ref()
                .and_then(|state| self.processing_discard_reason(state, &res))
            {
                self.debug_log(format!(
                    "processing discarded: job={} mode={:?} target={} reason={reason}",
                    res.job_id,
                    res.mode,
                    Self::format_processing_target(&res.target),
                ));
                self.processing = None;
                ctx.request_repaint();
                return;
            }
            let ProcessingResult {
                path,
                job_id,
                mode,
                target,
                samples,
                channels,
                waveform: _waveform,
                editor_waveform,
            } = res;
            // The worker prebuilds the editor waveform cache; only rebuild
            // here (UI thread) if a legacy result arrived without one.
            let rebuilt_cache =
                if matches!(target, ProcessingTarget::EditorTab(_)) && !channels.is_empty() {
                    editor_waveform.or_else(|| {
                        let samples_len = channels.get(0).map(|channel| channel.len()).unwrap_or(0);
                        Some(Self::build_editor_waveform_cache(&channels, samples_len))
                    })
                } else {
                    None
                };
            if matches!(target, ProcessingTarget::EditorTab(_)) {
                if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
                    if let Some(tab) = self.tabs.get_mut(idx) {
                        if let Some((waveform, waveform_pyramid)) = rebuilt_cache {
                            tab.waveform_minmax = waveform;
                            tab.waveform_pyramid = waveform_pyramid;
                        } else {
                            tab.waveform_minmax.clear();
                            tab.waveform_pyramid = None;
                        }
                        Self::invalidate_editor_viewport_cache(tab);
                    }
                }
            }
            if channels.is_empty() {
                self.audio.set_samples_mono(samples);
            } else {
                // `channels` is not used again below; moving it avoids a
                // full-buffer copy on the UI thread.
                self.audio.set_samples_channels(channels);
            }
            let source = match &target {
                ProcessingTarget::EditorTab(path) => {
                    super::PlaybackSourceKind::EditorTab(path.clone())
                }
                ProcessingTarget::ListPreview(path) => {
                    super::PlaybackSourceKind::ListPreview(path.clone())
                }
            };
            self.playback_mark_buffer_source(source, self.audio.shared.out_sample_rate.max(1));
            self.debug_log(format!(
                "processing applied: job={} mode={:?} target={} buffer_sr={}",
                job_id,
                mode,
                Self::format_processing_target(&target),
                self.audio.shared.out_sample_rate.max(1),
            ));
            self.audio.stop();
            // update current playing path (for effective volume using pending gains)
            self.playing_path = Some(path.clone());
            // full-buffer loop region if needed
            if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                self.audio.set_loop_region(0, buf.len());
            }
            if let Some(source_time_sec) = source_time_sec {
                self.playback_seek_to_source_time(mode, source_time_sec);
            }
            self.processing = None;
            let should_resume_list_play = matches!(target, ProcessingTarget::ListPreview(_))
                && self.is_list_workspace_active()
                && self.selected_path_buf().as_ref() == Some(&path)
                && (autoplay_when_ready || self.list_play_pending);
            if should_resume_list_play {
                self.audio.play();
                self.list_play_pending = false;
                self.debug_mark_list_play_start(&path);
            }
            ctx.request_repaint();
        }
    }

    /// Whether the modal input-blocking overlay is up. Editor applies are
    /// intentionally NOT included: they run per-tab (topbar activity +
    /// in-tab banner) and must not modal-block the whole app.
    pub(super) fn busy_overlay_blocking(&self) -> bool {
        let bulk_blocking = self
            .bulk_resample_state
            .as_ref()
            .map(|s| s.started_at.elapsed().as_secs() >= BULK_RESAMPLE_BLOCK_SECS)
            .unwrap_or(false);
        self.export_state.is_some()
            || self.csv_export_state.is_some()
            || self.session_save_state.is_some()
            || self.clipboard_prep_state.is_some()
            || bulk_blocking
    }

    /// How long the modal overlay has been up, or zero when nothing blocks.
    ///
    /// The longest-running job wins: what the user is waiting on is whichever
    /// one has kept them waiting.
    fn busy_overlay_elapsed(&self) -> std::time::Duration {
        let mut longest = std::time::Duration::ZERO;
        let mut consider = |started: std::time::Instant| {
            longest = longest.max(started.elapsed());
        };
        if let Some(state) = &self.export_state {
            consider(state.started_at);
        }
        if let Some(state) = &self.csv_export_state {
            consider(state.started_at);
        }
        if let Some(state) = &self.session_save_state {
            consider(state.started_at);
        }
        if let Some(state) = &self.clipboard_prep_state {
            consider(state.started_at);
        }
        if let Some(state) = &self.bulk_resample_state {
            consider(state.started_at);
        }
        longest
    }

    pub(super) fn ui_busy_overlay(&mut self, ctx: &egui::Context) {
        if !self.busy_overlay_blocking() {
            // Nothing is blocking, so a release from the last job that stalled
            // has nothing left to release. Re-arming it here means the next
            // job starts modal again, which is what it is for.
            self.busy_overlay_released = false;
            return;
        }
        if self.busy_overlay_released {
            return;
        }
        // The quit prompt is a plain window, so it draws *under* everything
        // below and would be dimmed and unclickable. Whatever this job is, the
        // user asking to close the application outranks it -- and a job that
        // never answers must never be the reason they cannot leave.
        if self.show_quit_prompt {
            return;
        }
        // Block input and show a modal spinner for operations that must not be interrupted.
        use egui::{Id, LayerId, Order};
        let elapsed = self.busy_overlay_elapsed();
        let stalled = elapsed.as_secs() >= BUSY_OVERLAY_STALL_SECS;
        let mut release = false;
        let screen = ctx.viewport_rect();
        // block input
        egui::Area::new("busy_block_input".into())
            .order(Order::Foreground)
            .show(ctx, |ui| {
                let _ = ui.allocate_rect(screen, egui::Sense::click_and_drag());
            });
        // darken background
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("busy_layer")));
        painter.rect_filled(screen, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 180));
        // centered box with spinner and text
        egui::Area::new("busy_center".into())
            .order(Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::window(ui.style()).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Spinner::new());
                        let msg = if let Some(p) = &self.processing {
                            p.msg.as_str()
                        } else if let Some(st) = &self.export_state {
                            st.msg.as_str()
                        } else if let Some(st) = &self.session_save_state {
                            st.msg.as_str()
                        } else if self.clipboard_prep_state.is_some() {
                            "Preparing clipboard..."
                        } else if self.csv_export_state.is_some() {
                            "Preparing CSV..."
                        } else if self.bulk_resample_state.is_some() {
                            "Applying sample rate..."
                        } else {
                            "Working..."
                        };
                        ui.label(RichText::new(msg).strong());
                        if let Some(state) = &mut self.bulk_resample_state {
                            let total = state.targets.len().max(1);
                            let pct = (state.index as f32 / total as f32).clamp(0.0, 1.0);
                            ui.add(
                                egui::ProgressBar::new(pct)
                                    .desired_width(180.0)
                                    .show_percentage(),
                            );
                            if ui.button("Cancel").clicked() {
                                state.cancel_requested = true;
                            }
                        }
                        if let Some(csv) = &self.csv_export_state {
                            if csv.total > 0 {
                                let pct = (csv.done as f32 / csv.total as f32).clamp(0.0, 1.0);
                                ui.add(
                                    egui::ProgressBar::new(pct)
                                        .desired_width(180.0)
                                        .show_percentage(),
                                );
                            }
                        }
                        // The way out of a job that stopped answering. A
                        // worker that dies without a word is caught by its own
                        // drain within a frame; this is for the other kind --
                        // alive, but wedged on a share that stopped
                        // responding -- where nothing will arrive to clear the
                        // overlay and the application otherwise reads as hung,
                        // with no way to save, to quit, or even to see what is
                        // behind the dimming.
                        if stalled {
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(format!(
                                    "Still working after {}s.",
                                    elapsed.as_secs()
                                ))
                                .weak(),
                            );
                            if ui
                                .button("Stop waiting")
                                .on_hover_text(
                                    "Give the window back. The job keeps running -- \
                                     this only stops it from blocking the application.",
                                )
                                .clicked()
                            {
                                release = true;
                            }
                        }
                    });
                });
            });
        if release {
            self.busy_overlay_released = true;
            self.push_toast(
                crate::app::types::ToastSeverity::Warning,
                "Stopped waiting. The job is still running in the background.".to_string(),
            );
        }
    }
}
