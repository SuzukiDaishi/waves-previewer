use super::helpers::db_to_amp;
use super::WavesPreviewer;

impl WavesPreviewer {
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

        let requested = self
            .audio_output_device_name
            .clone()
            .or_else(|| self.audio.output_device_name().map(|v| v.to_string()));
        let try_engine = crate::audio::AudioEngine::new_with_output_device_name_and_sample_rate(
            requested.as_deref(),
            Some(preferred_sr),
        );
        match try_engine {
            Ok(engine) => {
                let actual_sr = engine.shared.out_sample_rate.max(1);
                let actual = engine
                    .output_device_name()
                    .map(|v| v.to_string())
                    .filter(|v| !v.trim().is_empty());
                self.audio = engine;
                self.audio_output_device_name = requested.or(actual);
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
            || self.playback_session.dry_audio.is_none();
        if !needs_render {
            self.playback_session.last_applied_master_gain_db = master_gain_db;
            return;
        }
        let Some(dry_audio) = self
            .playback_session
            .dry_audio
            .clone()
            .or_else(|| self.audio.shared.samples.load_full())
        else {
            self.playback_session.last_applied_master_gain_db = master_gain_db;
            self.playback_session.last_applied_file_gain_db = file_gain_db;
            return;
        };
        self.playback_session.dry_audio = Some(dry_audio.clone());

        let gain = db_to_amp(file_gain_db).clamp(0.0, 16.0);
        let mut channels = dry_audio.channels.clone();
        if (gain - 1.0).abs() > 1.0e-6 {
            for channel in &mut channels {
                for sample in channel {
                    *sample = (*sample * gain).clamp(-1.0, 1.0);
                }
            }
        }
        self.audio.replace_samples_keep_pos(std::sync::Arc::new(
            crate::audio::AudioBuffer::from_channels(channels),
        ));
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
                self.audio_output_devices = devices;
                if let Some(name) = self.audio_output_device_name.clone() {
                    if !self.audio_output_devices.iter().any(|d| d == &name) {
                        self.audio_output_error = Some(format!(
                            "Output device not available: {name}. Using default."
                        ));
                        self.audio_output_device_name = None;
                    }
                }
            }
            Err(err) => {
                self.audio_output_devices.clear();
                self.audio_output_error = Some(format!("Failed to list output devices: {err}"));
            }
        }
    }

    fn sync_after_audio_engine_replaced(&mut self) {
        self.audio.stop();
        self.playing_path = None;
        self.list_play_pending = false;
        self.list_preview_pending_path = None;
        self.cancel_list_preview_job();
        self.playback_session.source = crate::app::PlaybackSourceKind::None;
        self.playback_session.transport = crate::app::PlaybackTransportKind::Buffer;
        self.playback_session.is_playing = false;
        self.playback_session.transport_sr = self.audio.shared.out_sample_rate.max(1);
        self.playback_session.dry_audio = None;
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

        self.audio.stop();
        let try_engine =
            crate::audio::AudioEngine::new_with_output_device_name(requested.as_deref());
        match try_engine {
            Ok(engine) => {
                let actual = engine
                    .output_device_name()
                    .map(|v| v.to_string())
                    .filter(|v| !v.trim().is_empty());
                self.audio = engine;
                self.audio_output_device_name = requested.or(actual);
                self.audio_output_error = None;
                self.sync_after_audio_engine_replaced();
                self.refresh_audio_output_devices();
                if persist {
                    self.save_prefs();
                }
                true
            }
            Err(err) => {
                if requested.is_some() {
                    match crate::audio::AudioEngine::new() {
                        Ok(engine) => {
                            self.audio = engine;
                            self.audio_output_device_name = None;
                            self.audio_output_error = Some(format!(
                                "Failed to switch output device: {err}. Fallback to default output."
                            ));
                            self.sync_after_audio_engine_replaced();
                            self.refresh_audio_output_devices();
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
