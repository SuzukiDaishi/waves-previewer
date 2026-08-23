use std::time::{Duration, Instant};

use super::helpers::db_to_amp;
use super::{AudioDeviceSnapshot, WavesPreviewer, AUDIO_DEVICE_POLL_INTERVAL_MS};
use crate::audio_channels::ChannelMapMode;

impl WavesPreviewer {
    /// How the current preference maps source channels onto the device.
    pub(super) fn audio_channel_map_mode(&self) -> ChannelMapMode {
        if self.audio_channel_map_direct {
            ChannelMapMode::Direct
        } else {
            ChannelMapMode::Auto
        }
    }

    /// Push the routing preference into the engine. Cheap and idempotent, so
    /// it is re-run after every engine swap (each one brings a fresh
    /// `SharedAudio` that starts at the default mode).
    pub(super) fn apply_audio_channel_map_mode(&self) {
        self.audio
            .set_channel_map_mode(self.audio_channel_map_mode());
    }

    /// Toggle the direct-routing preference, persisting it.
    pub(super) fn set_audio_channel_map_direct(&mut self, direct: bool) {
        if self.audio_channel_map_direct == direct {
            return;
        }
        self.audio_channel_map_direct = direct;
        self.apply_audio_channel_map_mode();
        self.save_prefs();
    }

    pub(super) fn ensure_output_sample_rate(&mut self, preferred_sr: Option<u32>) -> bool {
        let Some(preferred_sr) = preferred_sr.filter(|v| *v > 0) else {
            return true;
        };
        if !self.audio.has_output_stream() {
            return true;
        }
        let current_sr = self.audio.shared.out_sample_rate.max(1);
        if current_sr == preferred_sr {
            return true;
        }

        let requested = self.audio_output_device_name.clone();
        let try_engine = crate::audio::AudioEngine::new_with_output_device_name_and_sample_rate(
            requested.as_deref(),
            Some(preferred_sr),
        );
        match try_engine {
            Ok(engine) => {
                let actual_sr = engine.shared.out_sample_rate.max(1);
                self.audio = engine;
                self.audio_output_device_name = requested;
                self.audio_output_error = if actual_sr != preferred_sr {
                    Some(format!(
                        "Preferred output sample rate {preferred_sr}Hz is not available on current output device. Using {actual_sr}Hz."
                    ))
                } else {
                    None
                };
                self.sync_after_audio_engine_replaced();
                self.refresh_audio_output_devices();
                true
            }
            Err(err) => {
                self.audio_output_error = Some(format!(
                    "Failed to switch output sample rate to {preferred_sr}Hz: {err}."
                ));
                false
            }
        }
    }

    pub(super) fn apply_effective_volume(&mut self) {
        let master_gain_db = self.volume_db;
        let path_opt = self
            .playing_path
            .as_ref()
            .or_else(|| self.current_active_path());
        let file_gain_db = if let Some(p) = path_opt {
            self.pending_gain_db_for_path(p)
        } else {
            0.0
        };
        let base = db_to_amp(master_gain_db);
        self.audio.set_volume(base);

        if self.playback_session.transport == crate::app::PlaybackTransportKind::ExactStreamWav
            && file_gain_db.abs() > 0.0001
        {
            self.rebuild_current_buffer_with_mode();
            return;
        }

        let needs_render = self.playback_session.last_applied_file_gain_db != file_gain_db
            || (self.prepared_playback_fx_audio.is_none() && self.playback_base_audio.is_none());
        if !needs_render {
            self.playback_session.last_applied_master_gain_db = master_gain_db;
            return;
        }
        let Some(base_audio) = self
            .prepared_playback_fx_audio
            .clone()
            .or_else(|| self.playback_base_audio.clone())
            .or_else(|| self.playback_session.dry_audio.clone())
            .or_else(|| self.audio.shared.samples.load_full())
        else {
            self.playback_session.last_applied_master_gain_db = master_gain_db;
            self.playback_session.last_applied_file_gain_db = file_gain_db;
            return;
        };
        if self.prepared_playback_fx_audio.is_none() {
            self.playback_base_audio = Some(base_audio.clone());
            self.playback_session.dry_audio = Some(base_audio.clone());
        }

        let gain = db_to_amp(file_gain_db).clamp(0.0, 16.0);
        let rendered = if (gain - 1.0).abs() <= 1.0e-6 {
            // The overwhelmingly common path is 0 dB. Re-publish the Arc
            // directly instead of deep-copying every channel merely to
            // multiply it by 1.0. A three-minute stereo clip is roughly
            // 100 MiB, so this also avoids keeping a second playback-sized
            // allocation alive beside `playback_base_audio`.
            base_audio
        } else {
            let mut channels = (*base_audio.channels).clone();
            for channel in &mut channels {
                for sample in channel {
                    *sample = (*sample * gain).clamp(-1.0, 1.0);
                }
            }
            std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(channels))
        };
        self.audio.replace_samples_keep_pos(rendered);
        self.playback_session.last_applied_master_gain_db = master_gain_db;
        self.playback_session.last_applied_file_gain_db = file_gain_db;
    }

    pub(super) fn refresh_audio_output_devices(&mut self) {
        if !self.audio.has_output_stream() {
            if self.audio_output_devices.is_empty() {
                let label = self
                    .audio
                    .output_device_name()
                    .unwrap_or("Test Output Device")
                    .to_string();
                self.audio_output_devices = vec![label];
            }
            return;
        }
        match crate::audio::AudioEngine::list_output_devices() {
            Ok(devices) => {
                self.apply_audio_output_devices_list(devices);
            }
            Err(err) => {
                self.audio_output_devices.clear();
                self.audio_output_error = Some(format!("Failed to list output devices: {err}"));
            }
        }
    }

    fn apply_audio_output_devices_list(&mut self, devices: Vec<String>) {
        self.audio_output_devices = devices;
        let Some(name) = self.audio_output_device_name.clone() else {
            if self
                .audio_output_error
                .as_deref()
                .map(|err| err.starts_with("Failed to list output devices:"))
                .unwrap_or(false)
            {
                self.audio_output_error = None;
            }
            return;
        };
        if self.audio_output_devices.iter().any(|d| d == &name) {
            if self
                .audio_output_error
                .as_deref()
                .map(|err| err.starts_with("Output device not available:"))
                .unwrap_or(false)
            {
                self.audio_output_error = None;
            }
            return;
        }
        if let Some(resolved) = crate::audio::AudioEngine::resolve_output_device_name_for_list(
            &name,
            &self.audio_output_devices,
        ) {
            self.audio_output_device_name = Some(resolved);
            self.audio_output_error = None;
            return;
        }
        self.audio_output_error = Some(format!("Output device not available: {name}."));
    }

    fn capture_audio_device_snapshot() -> AudioDeviceSnapshot {
        let output_devices =
            crate::audio::AudioEngine::list_output_devices().map_err(|err| err.to_string());
        let default_output_name = crate::audio::AudioEngine::default_output_device_name()
            .ok()
            .flatten();
        let input_devices = crate::audio_capture::list_input_devices();
        let default_input_id =
            crate::audio_capture::default_input_device_info().map(|info| info.id);
        AudioDeviceSnapshot {
            output_devices,
            default_output_name,
            input_devices,
            default_input_id,
        }
    }

    pub(super) fn tick_audio_device_watch(&mut self, now: Instant) {
        self.drain_audio_device_watch();
        self.handle_output_stream_errors(now);
        if !self.audio.has_output_stream()
            || self.audio_device_watch.rx.is_some()
            || now < self.audio_device_watch.next_poll_at
        {
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel();
        self.audio_device_watch.rx = Some(rx);
        self.audio_device_watch.next_poll_at =
            now + Duration::from_millis(AUDIO_DEVICE_POLL_INTERVAL_MS);
        std::thread::spawn(move || {
            let _ = tx.send(Self::capture_audio_device_snapshot());
        });
    }

    /// Reopen the output when cpal reported the stream broken — typically the
    /// endpoint being unplugged. The user's choice is honoured: a pinned device
    /// is retried by name, "Default" reopens whatever the default now is.
    fn handle_output_stream_errors(&mut self, now: Instant) {
        if !self.audio.has_output_stream() {
            return;
        }
        let seq = self.audio.stream_error_seq();
        if seq != self.audio_device_watch.last_stream_error_seq {
            self.audio_device_watch.last_stream_error_seq = seq;
            self.audio_device_watch.output_recovery_pending = true;
        }
        if !self.audio_device_watch.output_recovery_pending {
            return;
        }
        // A disconnected endpoint may report once and then go silent forever.
        // Retry from persistent state at the device-poll cadence.
        if now < self.audio_device_watch.next_stream_error_retry_at {
            return;
        }
        self.audio_device_watch.next_stream_error_retry_at =
            now + Duration::from_millis(AUDIO_DEVICE_POLL_INTERVAL_MS);
        let requested = self.audio_output_device_name.clone();
        let reopened = self.reopen_output_device_keeping_playback(requested);
        let replacement_seq = self.audio.stream_error_seq();
        if reopened {
            // A just-opened stream can fail before this frame ends (for
            // example while the OS default endpoint is still changing). Do
            // not consume that new signal when resetting the counter.
            self.audio_device_watch.output_recovery_pending = replacement_seq != 0;
        }
        // A replacement engine starts its own error counter.
        self.audio_device_watch.last_stream_error_seq = replacement_seq;
    }

    fn drain_audio_device_watch(&mut self) {
        let Some(rx) = self.audio_device_watch.rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(snapshot) => self.apply_audio_device_snapshot(snapshot),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.audio_device_watch.rx = Some(rx);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
    }

    pub(super) fn apply_audio_device_snapshot(&mut self, snapshot: AudioDeviceSnapshot) {
        match snapshot.output_devices {
            Ok(devices) => self.apply_audio_output_devices_list(devices),
            Err(err) => {
                self.audio_output_devices.clear();
                self.audio_output_error = Some(format!("Failed to list output devices: {err}"));
            }
        }

        self.audio_device_watch.last_default_output_name = snapshot.default_output_name.clone();
        self.audio_device_watch.last_default_input_id = snapshot.default_input_id.clone();
        if self.recording_allows_device_list_refresh() {
            self.recording_tab.input_devices = snapshot.input_devices;
        }
        self.apply_default_output_follow_for_snapshot(snapshot.default_output_name.as_deref());
    }

    fn recording_allows_device_list_refresh(&self) -> bool {
        !matches!(
            self.recording_tab.state,
            crate::app::types::RecordingState::Recording
                | crate::app::types::RecordingState::Paused
                | crate::app::types::RecordingState::Finalizing
        )
    }

    /// The device to move to, if the OS default has changed out from under us.
    ///
    /// Only applies when the user left the output on "Default" — an explicitly
    /// pinned device is never overridden. Playback is *not* a reason to defer:
    /// when the default endpoint changes (or disappears) mid-playback, staying
    /// on the old one just plays to nothing, so the swap happens immediately
    /// and playback is resumed at the same position on the new device.
    pub(super) fn default_output_follow_target(
        &self,
        default_output_name: Option<&str>,
    ) -> Option<String> {
        if self.audio_output_device_name.is_some() || !self.recording_allows_device_list_refresh() {
            return None;
        }
        let default_output_name = default_output_name
            .map(str::trim)
            .filter(|name| !name.is_empty())?;
        let current_output_name = self
            .audio
            .output_device_name()
            .map(str::trim)
            .filter(|name| !name.is_empty())?;
        if current_output_name == default_output_name {
            None
        } else {
            Some(default_output_name.to_string())
        }
    }

    fn apply_default_output_follow_for_snapshot(
        &mut self,
        default_output_name: Option<&str>,
    ) -> bool {
        if !self.audio.has_output_stream()
            || self
                .default_output_follow_target(default_output_name)
                .is_none()
        {
            return false;
        }
        self.reopen_output_device_keeping_playback(None)
    }

    /// Swap only the output stream. `AudioEngine` carries the source, playhead,
    /// loop, rate and channel state into the new callback.
    fn reopen_output_device_keeping_playback(&mut self, requested: Option<String>) -> bool {
        let reopened = self.apply_audio_output_device_selection_inner(requested, false, false);
        if reopened {
            // Hot-plug fallback is an automatic recovery, not a user-facing
            // failure. Manual device selection still reports fallback errors.
            self.audio_output_error = None;
        }
        reopened
    }

    fn sync_after_audio_output_stream_reopened(&mut self) {
        self.apply_audio_channel_map_mode();
        self.playback_refresh_rate_for_current_source();
        self.apply_effective_volume();
        let seq = self.audio.stream_error_seq();
        self.audio_device_watch.last_stream_error_seq = seq;
        self.audio_device_watch.output_recovery_pending = seq != 0;
    }

    pub(super) fn sync_after_audio_engine_replaced(&mut self) {
        self.audio.stop();
        self.audio_device_watch.last_stream_error_seq = self.audio.stream_error_seq();
        self.audio_device_watch.output_recovery_pending = false;
        self.apply_audio_channel_map_mode();
        self.playing_path = None;
        self.list_play_pending = false;
        self.list_preview_pending_path = None;
        self.cancel_list_preview_job();
        self.playback_session.source = crate::app::PlaybackSourceKind::None;
        self.playback_session.transport = crate::app::PlaybackTransportKind::Buffer;
        self.playback_session.is_playing = false;
        self.playback_session.transport_sr = self.audio.shared.out_sample_rate.max(1);
        self.playback_set_applied_mapping(crate::app::RateMode::Speed, 1.0);
        self.playback_session.dry_audio = None;
        self.playback_base_audio = None;
        self.clear_playback_fx_state();
        self.playback_session.last_applied_master_gain_db = f32::NAN;
        self.playback_session.last_applied_file_gain_db = f32::NAN;
        self.playback_refresh_rate_for_current_source();
        self.apply_effective_volume();
    }

    pub(super) fn apply_audio_output_device_selection(
        &mut self,
        next: Option<String>,
        persist: bool,
    ) -> bool {
        self.apply_audio_output_device_selection_inner(next, persist, true)
    }

    fn apply_audio_output_device_selection_inner(
        &mut self,
        next: Option<String>,
        persist: bool,
        refresh_devices: bool,
    ) -> bool {
        let requested = next.and_then(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        // kittest/new_for_test has no real output stream; avoid hardware-dependent switching.
        if !self.audio.has_output_stream() {
            if let Some(name) = requested {
                if self.audio_output_devices.iter().any(|d| d == &name) {
                    self.audio_output_device_name = Some(name);
                    self.audio_output_error = None;
                } else {
                    self.audio_output_device_name = None;
                    self.audio_output_error =
                        Some("Output device not available in current runtime.".to_string());
                }
            } else {
                self.audio_output_device_name = None;
                self.audio_output_error = None;
            }
            if persist {
                self.save_prefs();
            }
            return true;
        }

        let preferred_sr = self.audio.shared.out_sample_rate.max(1);
        let reopen = self.audio.reopen_with_output_device_name_and_sample_rate(
            requested.as_deref(),
            Some(preferred_sr),
        );
        match reopen {
            Ok(()) => {
                self.audio_output_device_name = requested;
                self.audio_output_error = None;
                self.sync_after_audio_output_stream_reopened();
                if refresh_devices {
                    self.refresh_audio_output_devices();
                }
                if persist {
                    self.save_prefs();
                }
                true
            }
            Err(err) => {
                if requested.is_some() {
                    match self
                        .audio
                        .reopen_with_output_device_name_and_sample_rate(None, Some(preferred_sr))
                    {
                        Ok(()) => {
                            self.audio_output_device_name = None;
                            self.audio_output_error = Some(format!(
                                "Failed to switch output device: {err}. Fallback to default output."
                            ));
                            self.sync_after_audio_output_stream_reopened();
                            if refresh_devices {
                                self.refresh_audio_output_devices();
                            }
                            if persist {
                                self.save_prefs();
                            }
                            true
                        }
                        Err(fallback_err) => {
                            self.audio_output_error = Some(format!(
                                "Failed to switch output device: {err}. Fallback failed: {fallback_err}."
                            ));
                            false
                        }
                    }
                } else {
                    self.audio_output_error = Some(format!(
                        "Failed to initialize default output device: {err}."
                    ));
                    false
                }
            }
        }
    }
}
