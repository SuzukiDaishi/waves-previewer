use super::types::{PreviewOverlay, PreviewOverlayDetailKind, ToolKind, ViewMode};
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn tool_supports_preview(tool: ToolKind) -> bool {
        matches!(
            tool,
            ToolKind::Fade
                | ToolKind::PitchShift
                | ToolKind::TimeStretch
                | ToolKind::Speed
                | ToolKind::Gain
                | ToolKind::Normalize
                | ToolKind::Loudness
                | ToolKind::Reverse
                | ToolKind::InvertPolarity
                | ToolKind::DcOffset
                | ToolKind::NoiseGate
                | ToolKind::Eq
                | ToolKind::Compressor
                | ToolKind::InsertSilence
                | ToolKind::DeClick
                | ToolKind::DeClip
                | ToolKind::DeHum
        )
    }

    pub(super) fn view_supports_wave_preview(
        view_mode: ViewMode,
        show_waveform_overlay: bool,
    ) -> bool {
        matches!(view_mode, ViewMode::Waveform)
            || (matches!(
                view_mode,
                ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel
            ) && show_waveform_overlay)
    }




    #[cfg(feature = "kittest")]
    pub(super) fn current_tab_preview_busy(&self, tab_idx: usize) -> bool {
        if self.tool_preview_busy_for_tab(tab_idx) {
            return true;
        }
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let path = tab.path.as_path();
        self.plugin_process_state
                .as_ref()
            .is_some_and(|state| state.tab_idx == tab_idx && !state.is_apply)
            || self
                .music_preview_state
                .as_ref()
                .is_some_and(|state| state.tab_path.as_path() == path)
    }


    pub(super) fn preview_restore_audio_for_tab(&mut self, tab_idx: usize) {
        let source_time_sec = self.playback_current_source_time_sec();
        self.audio.stop();
        if self.try_activate_editor_stream_transport_for_tab(tab_idx) {
            if let Some(source_time_sec) = source_time_sec {
                self.playback_seek_to_source_time(self.mode, source_time_sec);
            }
            return;
        }
        if let Some(tab) = self.tabs.get(tab_idx) {
            let mut render_spec = self.offline_render_spec_for_path(&tab.path);
            render_spec.master_gain_db = 0.0;
            render_spec.file_gain_db = 0.0;
            let rendered = Self::render_channels_offline_with_spec(
                tab.ch_samples.clone(),
                tab.buffer_sample_rate.max(1),
                render_spec,
                false,
            );
            self.audio.set_samples_channels(rendered);
            // Reapply loop mode
            self.apply_loop_mode_for_tab(tab);
            let tab_path = tab.path.clone();
            self.playback_mark_buffer_source(
                super::PlaybackSourceKind::EditorTab(tab_path),
                tab.buffer_sample_rate,
            );
            if let Some(source_time_sec) = source_time_sec {
                self.playback_seek_to_source_time(self.mode, source_time_sec);
            }
        }
    }

    pub(super) fn set_preview_mono(&mut self, tab_idx: usize, tool: ToolKind, mono: Vec<f32>) {
        let preview_audio = std::sync::Arc::new(crate::audio::AudioBuffer::from_mono(mono));
        self.audio.stop();
        self.audio.set_samples_buffer(preview_audio.clone());
        self.playback_mark_buffer_source(
            super::PlaybackSourceKind::ToolPreview,
            self.audio.shared.out_sample_rate.max(1),
        );
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(tool);
            tab.preview_audio_buffer = Some((tool, preview_audio));
        }
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn set_preview_channels(
        &mut self,
        tab_idx: usize,
        tool: ToolKind,
        channels: Vec<Vec<f32>>,
    ) {
        let preview_audio = std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(channels));
        self.audio.stop();
        self.audio.set_samples_buffer(preview_audio.clone());
        self.playback_mark_buffer_source(
            super::PlaybackSourceKind::ToolPreview,
            self.audio.shared.out_sample_rate.max(1),
        );
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(tool);
            tab.preview_audio_buffer = Some((tool, preview_audio));
        }
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    /// [`Self::set_preview_channels`] without the stop: the buffer is
    /// swapped in place and the playhead position (and playing state)
    /// survive — used by the Plugin FX auto preview.
    pub(super) fn set_preview_channels_keep_pos(
        &mut self,
        tab_idx: usize,
        tool: ToolKind,
        mut channels: Vec<Vec<f32>>,
    ) {
        // Preview renders are already at the output rate on both sides of
        // the swap, so the time position maps 1:1.
        let sr = self.audio.shared.out_sample_rate.max(1);
        // Keep dry audio running while render-ahead is prepared, then blend
        // the first 10 ms after the live playhead into the new wet buffer.
        // This avoids a discontinuity without delaying the transport or
        // rewriting audio before the current position.
        if let Some(dry) = self.audio.shared.samples.load_full() {
            let start = self
                .audio
                .shared
                .play_pos
                .load(std::sync::atomic::Ordering::Relaxed);
            let fade_frames = ((sr as f32 * 0.010).round() as usize).max(1);
            for (channel_index, wet_channel) in channels.iter_mut().enumerate() {
                let dry_channel = dry
                    .channels
                    .get(channel_index)
                    .or_else(|| dry.channels.last());
                let Some(dry_channel) = dry_channel else {
                    continue;
                };
                let available = wet_channel
                    .len()
                    .min(dry_channel.len())
                    .saturating_sub(start)
                    .min(fade_frames);
                for offset in 0..available {
                    let wet = (offset + 1) as f32 / available.max(1) as f32;
                    let dry_mix = 1.0 - wet;
                    wet_channel[start + offset] =
                        dry_channel[start + offset] * dry_mix + wet_channel[start + offset] * wet;
                }
            }
        }
        let preview_audio = std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(channels));
        self.audio
            .set_samples_buffer_keep_time_pos(preview_audio.clone(), sr, sr);
        self.playback_mark_buffer_source(
            super::PlaybackSourceKind::ToolPreview,
            self.audio.shared.out_sample_rate.max(1),
        );
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(tool);
            tab.preview_audio_buffer = Some((tool, preview_audio));
        }
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    /// Resolve the audio represented by the green waveform. Full-sample
    /// overlays can provide their own channels; overview-only overlays use
    /// the audition buffer retained when the preview render completed.
    pub(super) fn visible_preview_audio_for_tab(
        &self,
        tab_idx: usize,
    ) -> Option<(ToolKind, std::sync::Arc<crate::audio::AudioBuffer>)> {
        let tab = self.tabs.get(tab_idx)?;
        if tab.active_tool == ToolKind::Trim
            || !Self::view_supports_wave_preview(tab.leaf_view_mode(), tab.show_waveform_overlay)
        {
            return None;
        }
        // A live preview plays the tab itself (see `ToolKind::previews_live`).
        let tool = tab.preview_audio_tool.filter(|tool| !tool.previews_live())?;
        let overlay = tab.preview_overlay.as_ref()?;
        if overlay.source_tool != tool {
            return None;
        }
        // A full-sample overlay is the strongest source of truth: its
        // channels are exactly what is currently drawn. This also keeps
        // continuously rebuilt previews (such as Loop Edit) from reusing an
        // older retained buffer for the same tool.
        if !overlay.is_overview_only()
            && !overlay.channels.is_empty()
            && overlay.channels.iter().any(|channel| !channel.is_empty())
        {
            return Some((
                tool,
                std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(
                    overlay.channels.clone(),
                )),
            ));
        }
        if let Some((buffer_tool, buffer)) = &tab.preview_audio_buffer {
            if *buffer_tool == tool && buffer.len() > 0 {
                return Some((tool, buffer.clone()));
            }
        }
        None
    }

    /// Make the visible green waveform the active playback source while
    /// preserving its current editor playhead position.
    pub(super) fn activate_visible_preview_audio_for_tab(&mut self, tab_idx: usize) -> bool {
        let display_pos = self.tabs.get(tab_idx).map(|tab| {
            let audio_pos = self
                .audio
                .shared
                .play_pos
                .load(std::sync::atomic::Ordering::Relaxed);
            self.map_audio_to_display_sample(tab, audio_pos)
        });
        let Some((tool, preview_audio)) = self.visible_preview_audio_for_tab(tab_idx) else {
            return false;
        };

        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_buffer = Some((tool, preview_audio.clone()));
        }
        let current_matches = matches!(
            self.playback_session.source,
            super::PlaybackSourceKind::ToolPreview
        ) && self
            .audio
            .shared
            .samples
            .load_full()
            .is_some_and(|current| std::sync::Arc::ptr_eq(&current, &preview_audio));
        if current_matches {
            return true;
        }

        self.audio.set_samples_buffer(preview_audio);
        // Previews are rendered at the tab's own rate.
        let preview_sr = self
            .tabs
            .get(tab_idx)
            .map(|tab| tab.buffer_sample_rate)
            .unwrap_or(self.audio.shared.out_sample_rate)
            .max(1);
        self.playback_mark_buffer_source(super::PlaybackSourceKind::ToolPreview, preview_sr);
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
        if let (Some(display_pos), Some(tab)) = (display_pos, self.tabs.get(tab_idx)) {
            let audio_pos = self.map_display_to_audio_sample(tab, display_pos);
            self.audio.seek_to_sample(audio_pos);
        }
        self.apply_effective_volume();
        true
    }

    fn pending_preview_render_target_for_tab(
        &self,
        tab_idx: usize,
    ) -> Option<(std::path::PathBuf, ToolKind)> {
        let tab = self.tabs.get(tab_idx)?;
        if let Some(job) = self.tool_preview.job.as_ref() {
            if job.key.tab_id == tab.tab_id && tab.active_tool == job.key.tool {
                return Some((tab.path.clone(), job.key.tool));
            }
        }
        if self
            .plugin_process_state
            .as_ref()
            .is_some_and(|state| state.tab_idx == tab_idx && !state.is_apply)
            && tab.active_tool == ToolKind::PluginFx
        {
            return Some((tab.path.clone(), ToolKind::PluginFx));
        }
        if self
            .music_preview_state
            .as_ref()
            .is_some_and(|state| state.tab_path == tab.path)
            && tab.active_tool == ToolKind::MusicAnalyze
        {
            return Some((tab.path.clone(), ToolKind::MusicAnalyze));
        }
        None
    }

    pub(super) fn cancel_pending_preview_autoplay_for(
        &mut self,
        path: &std::path::Path,
        tool: ToolKind,
    ) {
        let should_cancel = self
            .pending_preview_autoplay
            .as_ref()
            .is_some_and(|pending| pending.path.as_path() == path && pending.tool == tool);
        if should_cancel {
            self.pending_preview_autoplay = None;
        }
    }

    /// Queue a Play request while the active tool's audition buffer is still
    /// rendering. Playing the currently installed source here would briefly
    /// audition unprocessed audio and the later preview swap would stop it.
    pub(super) fn defer_play_until_pending_preview_is_ready(&mut self, tab_idx: usize) -> bool {
        let Some((path, tool)) = self.pending_preview_render_target_for_tab(tab_idx) else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let audio_len = self.audio.current_source_len();
        let mut audio_pos = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        if audio_len > 0 && audio_pos >= audio_len {
            audio_pos = 0;
        }
        let display_sample = self.map_audio_to_display_sample(tab, audio_pos);

        self.audio.stop();
        self.playback_session.last_play_start_display_sample = Some(display_sample);
        self.pending_preview_autoplay = Some(super::PendingPreviewAutoplay {
            path: path.clone(),
            tool,
            display_sample,
        });
        self.debug_log(format!(
            "editor play deferred until preview is ready: tool={} path={}",
            tool.label(),
            path.display()
        ));
        true
    }

    /// Complete a deferred Play request only after the matching processed
    /// buffer has been installed as the ToolPreview source.
    pub(super) fn finish_pending_preview_autoplay(
        &mut self,
        tab_idx: usize,
        path: &std::path::Path,
        tool: ToolKind,
    ) {
        let Some(pending) = self.pending_preview_autoplay.take() else {
            return;
        };
        if pending.path.as_path() != path || pending.tool != tool {
            self.pending_preview_autoplay = Some(pending);
            return;
        }
        let active_target = self.is_editor_workspace_active()
            && self.active_tab == Some(tab_idx)
            && self
                .tabs
                .get(tab_idx)
                .is_some_and(|tab| tab.path.as_path() == path);
        if !active_target {
            self.debug_log(format!(
                "deferred preview play cancelled after target changed: tool={} path={}",
                tool.label(),
                path.display()
            ));
            return;
        }

        if let Some(tab) = self.tabs.get(tab_idx) {
            let audio_sample = self.map_display_to_audio_sample(tab, pending.display_sample);
            self.audio.seek_to_sample(audio_sample);
        }
        self.playback_session.last_play_start_display_sample = Some(pending.display_sample);
        self.apply_effective_volume();
        if self.playback_mode_needs_fx_buffer() && !self.spawn_playback_fx_render(true) {
            self.playback_sync_state_snapshot();
            return;
        }
        self.audio.play();
        self.playback_sync_state_snapshot();
        self.debug_log(format!(
            "deferred editor play started with preview: tool={} path={}",
            tool.label(),
            path.display()
        ));
    }

    pub(super) fn build_overview_bins_from_channels(channels: &[Vec<f32>]) -> Vec<Vec<(f32, f32)>> {
        let bins = crate::app::render::waveform_pyramid::DEFAULT_LOADING_OVERVIEW_BINS;
        channels
            .iter()
            .map(|channel| {
                crate::wave::build_waveform_minmax_from_channels(
                    std::slice::from_ref(channel),
                    channel.len(),
                    bins,
                )
            })
            .filter(|bins| !bins.is_empty())
            .collect()
    }

    fn mixdown_overview_bins(channels: &[Vec<(f32, f32)>]) -> Option<Vec<(f32, f32)>> {
        if channels.is_empty() {
            return None;
        }
        let len = channels.iter().map(Vec::len).min().unwrap_or(0);
        if len == 0 {
            return None;
        }
        let inv = 1.0 / channels.len().max(1) as f32;
        let mut mixdown = Vec::with_capacity(len);
        for idx in 0..len {
            let mut mn = 0.0f32;
            let mut mx = 0.0f32;
            for channel in channels {
                let (lo, hi) = channel[idx];
                mn += lo * inv;
                mx += hi * inv;
            }
            mixdown.push((mn.min(mx), mn.max(mx)));
        }
        Some(mixdown)
    }

    pub(super) fn preview_overlay_from_overview(
        overview_channels: Vec<Vec<(f32, f32)>>,
        tool: ToolKind,
        timeline_len: usize,
    ) -> PreviewOverlay {
        let overview_mixdown = if overview_channels.len() > 1 {
            Self::mixdown_overview_bins(&overview_channels)
        } else {
            None
        };
        PreviewOverlay {
            channels: Vec::new(),
            mixdown: None,
            overview_channels,
            overview_mixdown,
            source_tool: tool,
            timeline_len: timeline_len.max(1),
            detail_kind: PreviewOverlayDetailKind::OverviewOnly,
            revision: PreviewOverlay::next_revision(),
        }
    }











    pub(super) fn refresh_tool_preview_for_tab(&mut self, tab_idx: usize) {
        self.refresh_tool_preview_for_tab_impl(tab_idx, false);
    }

    /// Explicit Preview requests rebuild even when an older preview for the
    /// same tool is still visible. The older result stays on screen and in the
    /// audition buffer until its replacement is ready.
    pub(super) fn rebuild_tool_preview_for_tab(&mut self, tab_idx: usize) {
        self.refresh_tool_preview_for_tab_impl(tab_idx, true);
    }

    fn refresh_tool_preview_for_tab_impl(&mut self, tab_idx: usize, force: bool) {
        // Every rendered tool previews through the engine
        // (`tool_preview_ops.rs`). A forced refresh -- the Preview button, a
        // canvas gesture that ended -- asks it for a render now. A passive one
        // (a tab shown again, a decode finished) leaves it to the engine's own
        // tick: a session that was previewing picks up where it was, and
        // opening a tool never starts a preview by itself.
        if !force || !self.tool_preview_applies(tab_idx) {
            return;
        }
        self.tool_preview_request_now(tab_idx);
    }

    pub(super) fn clear_preview_if_any(&mut self, tab_idx: usize) {
        // A preview from the engine goes away at the same place in the
        // audio, still playing; the session starts over.
        if self
            .tabs
            .get(tab_idx)
            .is_some_and(|tab| tab.tool_session.preview.is_some())
        {
            self.uninstall_tool_preview(tab_idx);
        }
        self.reset_tool_session(tab_idx);
        // A live preview never replaced the tab's audio: playback goes on,
        // and the engine's live panner is taken away by its sync.
        let had_preview_audio = self
            .tabs
            .get(tab_idx)
            .and_then(|tab| tab.preview_audio_tool)
            .is_some_and(|tool| !tool.previews_live());
        if had_preview_audio {
            self.audio.stop();
            self.preview_restore_audio_for_tab(tab_idx);
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = None;
            tab.preview_audio_buffer = None;
            tab.preview_overlay = None;
        }
        if let Some(path) = self.tabs.get(tab_idx).map(|tab| tab.path.clone()) {
            let should_cancel = self
                .pending_preview_autoplay
                .as_ref()
                .is_some_and(|pending| pending.path == path);
            if should_cancel {
                self.pending_preview_autoplay = None;
            }
        }
        // also discard any in-flight preview job
        self.cancel_tool_preview_for_tab(tab_idx);
        self.cancel_music_preview_run();
        // A plugin preview still rendering for this tab would otherwise land
        // after the tool or tab changed and put its audio back.
        if self
            .plugin_process_state
            .as_ref()
            .is_some_and(|state| state.tab_idx == tab_idx && !state.is_apply)
        {
            self.plugin_process_state = None;
        }
    }



    // Spawn per-channel overlay generator (Pitch/Stretch) in a worker thread.
    // Note: Call this ONLY after UI borrows end (see E0499 note) to avoid nested &mut self borrows.



    pub(super) fn preview_overlay_from_channels(
        channels: Vec<Vec<f32>>,
        tool: ToolKind,
        timeline_len: usize,
    ) -> PreviewOverlay {
        let mixdown = if channels.len() > 1 {
            let len = channels.get(0).map(|c| c.len()).unwrap_or(0);
            Some(Self::mixdown_channels(&channels, len))
        } else {
            None
        };
        PreviewOverlay {
            channels,
            mixdown,
            overview_channels: Vec::new(),
            overview_mixdown: None,
            source_tool: tool,
            timeline_len,
            detail_kind: PreviewOverlayDetailKind::FullSample,
            revision: PreviewOverlay::next_revision(),
        }
    }
}
