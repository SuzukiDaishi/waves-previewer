use std::path::{Path, PathBuf};

use super::helpers::db_to_amp;
use super::types::{
    EditorTab, FadeShape, PreviewOverlay, PreviewOverlayDetailKind, ToolKind, ViewMode,
};
use super::{WavesPreviewer, LIVE_PREVIEW_SAMPLE_LIMIT};

#[derive(Clone, Copy)]
enum LongPreviewJobKind {
    PitchShift {
        semitones: f32,
        range: Option<(usize, usize)>,
    },
    TimeStretch {
        rate: f32,
        range: Option<(usize, usize)>,
    },
    Speed {
        rate: f32,
        range: Option<(usize, usize)>,
    },
    Fade {
        fade_in_samples: usize,
        fade_out_samples: usize,
        fade_in_shape: FadeShape,
        fade_out_shape: FadeShape,
    },
    Gain {
        gain_db: f32,
    },
    Normalize {
        target_db: f32,
    },
    Loudness {
        target_lufs: f32,
        out_sample_rate: u32,
    },
    Reverse {
        range: Option<(usize, usize)>,
    },
    NoiseGate {
        params: crate::wave::NoiseGateParams,
    },
    Eq {
        params: crate::wave::ThreeBandEqParams,
    },
    Compressor {
        params: crate::wave::CompressorParams,
    },
    InsertSilence {
        position: usize,
        samples: usize,
    },
    DeClick {
        sensitivity: f32,
        range: Option<(usize, usize)>,
    },
    DeClip {
        sensitivity: f32,
        range: Option<(usize, usize)>,
    },
    DeHum {
        config: crate::app::dehum::DehumConfig,
        range: Option<(usize, usize)>,
    },
}

#[derive(Clone, Copy)]
enum FullOverlayRenderMode {
    Buffer,
    Path,
}

impl LongPreviewJobKind {
    fn tool(self) -> ToolKind {
        match self {
            LongPreviewJobKind::PitchShift { .. } => ToolKind::PitchShift,
            LongPreviewJobKind::TimeStretch { .. } => ToolKind::TimeStretch,
            LongPreviewJobKind::Speed { .. } => ToolKind::Speed,
            LongPreviewJobKind::Fade { .. } => ToolKind::Fade,
            LongPreviewJobKind::Gain { .. } => ToolKind::Gain,
            LongPreviewJobKind::Normalize { .. } => ToolKind::Normalize,
            LongPreviewJobKind::Loudness { .. } => ToolKind::Loudness,
            LongPreviewJobKind::Reverse { .. } => ToolKind::Reverse,
            LongPreviewJobKind::NoiseGate { .. } => ToolKind::NoiseGate,
            LongPreviewJobKind::Eq { .. } => ToolKind::Eq,
            LongPreviewJobKind::Compressor { .. } => ToolKind::Compressor,
            LongPreviewJobKind::InsertSilence { .. } => ToolKind::InsertSilence,
            LongPreviewJobKind::DeClick { .. } => ToolKind::DeClick,
            LongPreviewJobKind::DeClip { .. } => ToolKind::DeClip,
            LongPreviewJobKind::DeHum { .. } => ToolKind::DeHum,
        }
    }

    fn final_timeline_len(self, base_timeline_len: usize) -> usize {
        match self {
            LongPreviewJobKind::TimeStretch { rate, range: None }
            | LongPreviewJobKind::Speed { rate, range: None } => {
                ((base_timeline_len as f64) * (rate.max(0.0001) as f64)).round() as usize
            }
            LongPreviewJobKind::TimeStretch {
                rate,
                range: Some((s, e)),
            }
            | LongPreviewJobKind::Speed {
                rate,
                range: Some((s, e)),
            } => {
                let sel = e.saturating_sub(s).min(base_timeline_len);
                let stretched = ((sel as f64) / (rate.max(0.0001) as f64)).round() as usize;
                base_timeline_len - sel + stretched
            }
            _ => base_timeline_len,
        }
        .max(1)
    }
}

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

    fn preview_matches_tool(tab: &EditorTab, tool: ToolKind) -> bool {
        let Some(overlay) = tab.preview_overlay.as_ref() else {
            return false;
        };
        if overlay.source_tool != tool {
            return false;
        }
        overlay.is_overview_only() || tab.preview_audio_tool == Some(tool)
    }

    pub(super) fn clear_heavy_preview_state(&mut self) {
        if let (Some(path), Some(tool)) = (
            self.heavy_preview_expected_path.clone(),
            self.heavy_preview_expected_tool,
        ) {
            self.cancel_pending_preview_autoplay_for(path.as_path(), tool);
        }
        self.heavy_preview_rx = None;
        self.heavy_preview_expected_gen = 0;
        self.heavy_preview_expected_path = None;
        self.heavy_preview_expected_tool = None;
    }

    pub(super) fn clear_heavy_overlay_state(&mut self) {
        self.heavy_overlay_rx = None;
        self.overlay_expected_gen = 0;
        self.overlay_expected_path = None;
        self.overlay_expected_tool = None;
    }

    pub(super) fn current_tab_preview_busy(&self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let path = tab.path.as_path();
        (self.heavy_preview_rx.is_some()
            && self.heavy_preview_expected_path.as_deref() == Some(path))
            || (self.heavy_overlay_rx.is_some()
                && self.overlay_expected_path.as_deref() == Some(path))
            || self
                .plugin_process_state
                .as_ref()
                .is_some_and(|state| state.tab_idx == tab_idx && !state.is_apply)
            || self
                .music_preview_state
                .as_ref()
                .is_some_and(|state| state.tab_path.as_path() == path)
    }

    pub(super) fn current_tab_preview_message(&self, tab_idx: usize) -> Option<String> {
        if !self.current_tab_preview_busy(tab_idx) {
            return None;
        }
        let tool = self
            .heavy_preview_expected_tool
            .or(self.overlay_expected_tool)
            .or_else(|| {
                self.plugin_process_state
                    .as_ref()
                    .filter(|state| state.tab_idx == tab_idx && !state.is_apply)
                    .map(|_| ToolKind::PluginFx)
            })
            .or_else(|| {
                let path = self.tabs.get(tab_idx)?.path.as_path();
                self.music_preview_state
                    .as_ref()
                    .filter(|state| state.tab_path.as_path() == path)
                    .map(|_| ToolKind::MusicAnalyze)
            });
        Some(match tool {
            Some(ToolKind::PitchShift) => "Previewing PitchShift...".to_string(),
            Some(ToolKind::TimeStretch) => "Previewing TimeStretch...".to_string(),
            Some(ToolKind::Speed) => "Previewing Speed...".to_string(),
            Some(ToolKind::PluginFx) => "Previewing Plugin FX...".to_string(),
            Some(ToolKind::MusicAnalyze) => "Previewing Music Analyze...".to_string(),
            Some(ToolKind::SpectralWarp) => "Previewing Spectral Warp...".to_string(),
            Some(ToolKind::SpectralBrush) => "Previewing Spectral Brush...".to_string(),
            Some(ToolKind::DeNoise) => "Previewing De-noise...".to_string(),
            _ => "Previewing...".to_string(),
        })
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
        let tool = tab.preview_audio_tool?;
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
        self.playback_mark_buffer_source(
            super::PlaybackSourceKind::ToolPreview,
            self.audio.shared.out_sample_rate.max(1),
        );
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
        if self.heavy_preview_rx.is_some()
            && self.heavy_preview_expected_path.as_deref() == Some(tab.path.as_path())
        {
            let tool = self.heavy_preview_expected_tool?;
            if tab.active_tool == tool || tab.preview_audio_tool == Some(tool) {
                return Some((tab.path.clone(), tool));
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

    fn build_overview_bins_from_channels(channels: &[Vec<f32>]) -> Vec<Vec<(f32, f32)>> {
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

    fn preview_overlay_from_overview(
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

    fn scale_overview_in_place(overview: &mut [Vec<(f32, f32)>], gain: f32, clamp_samples: bool) {
        for channel in overview {
            for (mn, mx) in channel {
                let lo = if clamp_samples {
                    (*mn * gain).clamp(-1.0, 1.0)
                } else {
                    *mn * gain
                };
                let hi = if clamp_samples {
                    (*mx * gain).clamp(-1.0, 1.0)
                } else {
                    *mx * gain
                };
                *mn = lo.min(hi);
                *mx = lo.max(hi);
            }
        }
    }

    fn apply_fade_to_overview_in_place(
        overview: &mut [Vec<(f32, f32)>],
        timeline_len: usize,
        fade_in_samples: usize,
        fade_out_samples: usize,
        fade_in_shape: FadeShape,
        fade_out_shape: FadeShape,
    ) {
        if timeline_len == 0 {
            return;
        }
        for channel in overview {
            let bins_len = channel.len().max(1);
            for (idx, (mn, mx)) in channel.iter_mut().enumerate() {
                let pos = (((idx as f64) + 0.5) * (timeline_len as f64) / (bins_len as f64)).round()
                    as usize;
                let mut weight = 1.0f32;
                if fade_in_samples > 0 && pos < fade_in_samples {
                    let t = pos as f32 / fade_in_samples.max(1) as f32;
                    weight *= Self::fade_weight(fade_in_shape, t.clamp(0.0, 1.0));
                }
                if fade_out_samples > 0 {
                    let fade_out_start = timeline_len.saturating_sub(fade_out_samples);
                    if pos >= fade_out_start {
                        let rel = pos.saturating_sub(fade_out_start);
                        let t = rel as f32 / fade_out_samples.max(1) as f32;
                        weight *= Self::fade_weight_out(fade_out_shape, t.clamp(0.0, 1.0));
                    }
                }
                *mn *= weight;
                *mx *= weight;
            }
        }
    }

    fn build_source_overview_bins(
        path: &Path,
        fallback_channels: &[Vec<f32>],
    ) -> Option<Vec<Vec<(f32, f32)>>> {
        if let Ok(Some(proxy)) = crate::audio_io::build_wav_proxy_preview(
            path,
            crate::audio_io::EDITOR_PROXY_OVERVIEW_MAX_TOTAL_SAMPLES,
        ) {
            let overview = Self::build_overview_bins_from_channels(&proxy.channels);
            if !overview.is_empty() {
                return Some(overview);
            }
        }
        let overview = Self::build_overview_bins_from_channels(fallback_channels);
        (!overview.is_empty()).then_some(overview)
    }

    fn build_long_preview_overlay(
        path: &Path,
        fallback_channels: &[Vec<f32>],
        kind: LongPreviewJobKind,
        base_timeline_len: usize,
    ) -> Option<PreviewOverlay> {
        let mut overview = Self::build_source_overview_bins(path, fallback_channels)?;
        match kind {
            LongPreviewJobKind::PitchShift { .. }
            | LongPreviewJobKind::TimeStretch { .. }
            | LongPreviewJobKind::Speed { .. } => {}
            LongPreviewJobKind::Fade {
                fade_in_samples,
                fade_out_samples,
                fade_in_shape,
                fade_out_shape,
            } => {
                Self::apply_fade_to_overview_in_place(
                    &mut overview,
                    base_timeline_len,
                    fade_in_samples,
                    fade_out_samples,
                    fade_in_shape,
                    fade_out_shape,
                );
            }
            LongPreviewJobKind::Gain { gain_db } => {
                Self::scale_overview_in_place(&mut overview, db_to_amp(gain_db), false);
            }
            LongPreviewJobKind::Normalize { target_db } => {
                let peak = fallback_channels
                    .iter()
                    .flat_map(|channel| channel.iter())
                    .fold(0.0_f32, |peak, &sample| peak.max(sample.abs()));
                if peak > 0.0 {
                    Self::scale_overview_in_place(
                        &mut overview,
                        db_to_amp(target_db) / peak.max(1e-12),
                        false,
                    );
                }
            }
            LongPreviewJobKind::Loudness {
                target_lufs,
                out_sample_rate,
            } => {
                let lufs = crate::wave::lufs_integrated_from_multi(
                    fallback_channels,
                    out_sample_rate.max(1),
                )
                .ok()?;
                if !lufs.is_finite() {
                    return None;
                }
                let gain = db_to_amp(target_lufs - lufs);
                Self::scale_overview_in_place(&mut overview, gain, true);
            }
            LongPreviewJobKind::Reverse { range } => {
                match range.filter(|(s, e)| *e > *s && *e <= base_timeline_len) {
                    Some((s, e)) => {
                        for channel in &mut overview {
                            let bins = channel.len();
                            if bins == 0 || base_timeline_len == 0 {
                                continue;
                            }
                            let b0 = ((s as u128) * (bins as u128) / (base_timeline_len as u128))
                                as usize;
                            let b1 = (((e as u128) * (bins as u128))
                                .div_ceil(base_timeline_len as u128))
                                as usize;
                            let b1 = b1.min(bins);
                            if b1 > b0 {
                                channel[b0..b1].reverse();
                            }
                        }
                    }
                    None => {
                        for channel in &mut overview {
                            channel.reverse();
                        }
                    }
                }
            }
            LongPreviewJobKind::NoiseGate { .. }
            | LongPreviewJobKind::Eq { .. }
            | LongPreviewJobKind::Compressor { .. }
            | LongPreviewJobKind::InsertSilence { .. }
            | LongPreviewJobKind::DeClick { .. }
            | LongPreviewJobKind::DeClip { .. }
            | LongPreviewJobKind::DeHum { .. } => {}
        }
        Some(Self::preview_overlay_from_overview(
            overview,
            kind.tool(),
            kind.final_timeline_len(base_timeline_len),
        ))
    }

    fn build_full_preview_overlay_from_channels(
        channels: &[Vec<f32>],
        kind: LongPreviewJobKind,
        sample_rate: u32,
    ) -> Option<PreviewOverlay> {
        let tool = kind.tool();
        let (param, range) = match kind {
            LongPreviewJobKind::PitchShift { semitones, range } => (semitones, range),
            LongPreviewJobKind::TimeStretch { rate, range } => (rate, range),
            LongPreviewJobKind::Speed { rate, range } => (rate, range),
            _ => return None,
        };
        let mut out = Vec::with_capacity(channels.len());
        let mut result_len = 0usize;
        for channel in channels {
            let processed =
                Self::process_tool_segment_spliced(channel, tool, param, sample_rate.max(1), range);
            result_len = processed.len();
            out.push(processed);
        }
        let timeline_len = out.get(0).map(Vec::len).unwrap_or(result_len).max(1);
        Some(Self::preview_overlay_from_channels(out, tool, timeline_len))
    }

    fn build_full_preview_overlay_from_path(
        path: &Path,
        kind: LongPreviewJobKind,
        out_sample_rate: u32,
        resample_quality: crate::wave::ResampleQuality,
        bit_depth: Option<crate::wave::WavBitDepth>,
    ) -> Option<PreviewOverlay> {
        let (mut channels, in_sr) = crate::wave::decode_wav_multi(path).ok()?;
        if in_sr != out_sample_rate {
            for channel in &mut channels {
                *channel = crate::wave::resample_quality(
                    channel,
                    in_sr,
                    out_sample_rate,
                    resample_quality,
                );
            }
        }
        if let Some(depth) = bit_depth {
            crate::wave::quantize_channels_in_place(&mut channels, depth);
        }
        Self::build_full_preview_overlay_from_channels(&channels, kind, out_sample_rate)
    }

    fn spawn_overlay_job_for_tab(
        &mut self,
        tab_idx: usize,
        kind: LongPreviewJobKind,
        full_render: Option<FullOverlayRenderMode>,
        send_overview_first: bool,
    ) {
        use std::sync::mpsc;

        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let path = tab.path.clone();
        let fallback_channels = tab.ch_samples.clone();
        let base_timeline_len = tab.samples_len.max(1);
        let out_sample_rate = self.audio.shared.out_sample_rate.max(1);
        let resample_quality = Self::to_wave_resample_quality(self.src_quality);
        let bit_depth = self.bit_depth_override.get(&path).copied();
        let tool = kind.tool();

        self.clear_heavy_overlay_state();
        self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
        let gen = self.overlay_gen_counter;
        self.overlay_expected_gen = gen;
        self.overlay_expected_path = Some(path.clone());
        self.overlay_expected_tool = Some(tool);

        let (tx, rx) = mpsc::channel::<super::HeavyOverlayMessage>();
        std::thread::spawn(move || {
            if send_overview_first || full_render.is_none() {
                if let Some(overlay) = Self::build_long_preview_overlay(
                    &path,
                    &fallback_channels,
                    kind,
                    base_timeline_len,
                ) {
                    let _ = tx.send((path.clone(), tool, overlay, gen, full_render.is_none()));
                } else if full_render.is_none() {
                    return;
                }
            }

            let Some(mode) = full_render else {
                return;
            };

            let overlay = match mode {
                FullOverlayRenderMode::Buffer => Self::build_full_preview_overlay_from_channels(
                    &fallback_channels,
                    kind,
                    out_sample_rate,
                ),
                FullOverlayRenderMode::Path => Self::build_full_preview_overlay_from_path(
                    &path,
                    kind,
                    out_sample_rate,
                    resample_quality,
                    bit_depth,
                ),
            };
            if let Some(overlay) = overlay {
                let _ = tx.send((path, tool, overlay, gen, true));
            }
        });
        self.heavy_overlay_rx = Some(rx);
    }

    fn spawn_long_processed_preview_for_tab(
        &mut self,
        tab_idx: usize,
        kind: LongPreviewJobKind,
        ch_mask: Option<Vec<bool>>,
    ) {
        use std::sync::mpsc;

        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let path = tab.path.clone();
        let channels = tab.ch_samples_arc.clone();
        let base_timeline_len = tab.samples_len.max(1);
        let sample_rate = tab.buffer_sample_rate.max(1);
        let tool = kind.tool();

        self.audio.stop();
        self.clear_heavy_preview_state();
        self.clear_heavy_overlay_state();

        self.heavy_preview_gen_counter = self.heavy_preview_gen_counter.wrapping_add(1);
        let preview_gen = self.heavy_preview_gen_counter;
        self.heavy_preview_expected_gen = preview_gen;
        self.heavy_preview_expected_path = Some(path.clone());
        self.heavy_preview_expected_tool = Some(tool);

        self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
        let overlay_gen = self.overlay_gen_counter;
        self.overlay_expected_gen = overlay_gen;
        self.overlay_expected_path = Some(path.clone());
        self.overlay_expected_tool = Some(tool);

        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(tool);
        }

        let (preview_tx, preview_rx) = mpsc::channel::<super::HeavyPreviewMessage>();
        let (overlay_tx, overlay_rx) = mpsc::channel::<super::HeavyOverlayMessage>();
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            let is_selected = |ci: usize| {
                ch_mask
                    .as_ref()
                    .and_then(|mask| mask.get(ci))
                    .copied()
                    .unwrap_or(true)
            };
            let mut playback = channels.as_ref().clone();
            match kind {
                LongPreviewJobKind::Fade {
                    fade_in_samples,
                    fade_out_samples,
                    fade_in_shape,
                    fade_out_shape,
                } => {
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if !is_selected(ci) {
                            continue;
                        }
                        let n_in = fade_in_samples.min(channel.len());
                        if n_in > 0 {
                            Self::apply_fade_in_to_slice(&mut channel[..n_in], fade_in_shape);
                        }
                        let n_out = fade_out_samples.min(channel.len());
                        if n_out > 0 {
                            let start = channel.len().saturating_sub(n_out);
                            Self::apply_fade_out_to_slice(&mut channel[start..], fade_out_shape);
                        }
                    }
                }
                LongPreviewJobKind::Gain { gain_db } => {
                    let gain = db_to_amp(gain_db);
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if is_selected(ci) {
                            for sample in channel {
                                *sample *= gain;
                            }
                        }
                    }
                }
                LongPreviewJobKind::Normalize { target_db } => {
                    let peak = playback
                        .iter()
                        .enumerate()
                        .filter(|(ci, _)| is_selected(*ci))
                        .flat_map(|(_, channel)| channel.iter())
                        .fold(0.0_f32, |peak, &sample| peak.max(sample.abs()));
                    if peak > 0.0 {
                        let gain = db_to_amp(target_db) / peak.max(1e-12);
                        for (ci, channel) in playback.iter_mut().enumerate() {
                            if is_selected(ci) {
                                for sample in channel {
                                    *sample *= gain;
                                }
                            }
                        }
                    }
                }
                LongPreviewJobKind::Loudness {
                    target_lufs,
                    out_sample_rate,
                } => {
                    if let Ok(lufs) =
                        crate::wave::lufs_integrated_from_multi(&playback, out_sample_rate)
                    {
                        if lufs.is_finite() {
                            let gain = db_to_amp(target_lufs - lufs);
                            for channel in &mut playback {
                                for sample in channel {
                                    *sample *= gain;
                                }
                            }
                        }
                    }
                }
                LongPreviewJobKind::Reverse { range } => {
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if !is_selected(ci) {
                            continue;
                        }
                        match range {
                            Some((start, end)) if end > start && end <= channel.len() => {
                                let xfade = crate::wave::splice_xfade_samples(
                                    sample_rate,
                                    end - start,
                                    end - start,
                                )
                                .min(256);
                                crate::wave::reverse_range_with_crossfade(
                                    channel, start, end, xfade,
                                );
                            }
                            _ => channel.reverse(),
                        }
                    }
                }
                LongPreviewJobKind::NoiseGate { params } => {
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if is_selected(ci) {
                            *channel = crate::wave::process_noise_gate_offline(
                                channel,
                                sample_rate,
                                &params,
                            );
                        }
                    }
                }
                LongPreviewJobKind::Eq { params } => {
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if is_selected(ci) {
                            *channel = crate::wave::process_three_band_eq_offline(
                                channel,
                                sample_rate,
                                &params,
                            );
                        }
                    }
                }
                LongPreviewJobKind::Compressor { params } => {
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if is_selected(ci) {
                            *channel = crate::wave::process_compressor_offline(
                                channel,
                                sample_rate,
                                &params,
                            );
                        }
                    }
                }
                LongPreviewJobKind::InsertSilence { position, samples } => {
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if is_selected(ci) {
                            let at = position.min(channel.len());
                            channel.splice(at..at, std::iter::repeat_n(0.0, samples));
                        }
                    }
                }
                LongPreviewJobKind::DeClick { sensitivity, range } => {
                    let config = crate::app::declick::DeclickConfig {
                        sensitivity: sensitivity.clamp(0.0, 1.0),
                        ..Default::default()
                    };
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if is_selected(ci) {
                            let (processed, _) = crate::app::declick::declick_channel(
                                channel,
                                sample_rate,
                                &config,
                                range,
                            );
                            *channel = processed;
                        }
                    }
                }
                LongPreviewJobKind::DeClip { sensitivity, range } => {
                    let config = crate::app::declip::DeclipConfig {
                        sensitivity: sensitivity.clamp(0.0, 1.0),
                        ..Default::default()
                    };
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if is_selected(ci) {
                            let (processed, _) = crate::app::declip::declip_channel(
                                channel,
                                sample_rate,
                                &config,
                                range,
                            );
                            *channel = processed;
                        }
                    }
                }
                LongPreviewJobKind::DeHum { config, range } => {
                    let fade = (sample_rate / 100).max(16) as usize;
                    for (ci, channel) in playback.iter_mut().enumerate() {
                        if !is_selected(ci) {
                            continue;
                        }
                        let filtered =
                            crate::app::dehum::dehum_channel(channel, sample_rate, &config);
                        *channel = match range {
                            Some((start, end)) => crate::app::dehum::splice_processed_range(
                                channel, &filtered, start, end, fade,
                            ),
                            None => filtered,
                        };
                    }
                }
                LongPreviewJobKind::PitchShift { .. }
                | LongPreviewJobKind::TimeStretch { .. }
                | LongPreviewJobKind::Speed { .. } => return,
            }
            if playback.first().is_some_and(|channel| channel.is_empty()) {
                return;
            }
            let overview = Self::build_overview_bins_from_channels(&playback);
            if !overview.is_empty() {
                let timeline_len = playback.first().map(Vec::len).unwrap_or(base_timeline_len);
                let overlay = Self::preview_overlay_from_overview(overview, tool, timeline_len);
                let _ = overlay_tx.send((path.clone(), tool, overlay, overlay_gen, true));
            }
            let _ = preview_tx.send((
                path,
                tool,
                super::HeavyPreviewAudio::Channels(playback),
                preview_gen,
            ));
        });
        self.heavy_preview_rx = Some(preview_rx);
        self.heavy_overlay_rx = Some(overlay_rx);
    }

    /// Render Pitch Shift preview audio and its waveform overlay from the same
    /// multi-channel result used by Apply. A single Signalsmith stream follows
    /// the complete curve, preserving channel coherence and exact duration.
    fn spawn_pitch_curve_preview_for_tab(
        &mut self,
        tab_idx: usize,
        points: Vec<(usize, f32)>,
        fallback_semitones: f32,
        range: Option<(usize, usize)>,
        use_path: bool,
    ) {
        use std::sync::mpsc;

        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let path = tab.path.clone();
        let fallback_channels = tab.ch_samples_arc.clone();
        let base_timeline_len = tab.samples_len.max(1);
        let sample_rate = self.audio.shared.out_sample_rate.max(1);
        let resample_quality = Self::to_wave_resample_quality(self.src_quality);
        let bit_depth = self.bit_depth_override.get(&path).copied();
        let tool = ToolKind::PitchShift;

        self.audio.stop();
        self.clear_heavy_preview_state();
        self.clear_heavy_overlay_state();

        self.heavy_preview_gen_counter = self.heavy_preview_gen_counter.wrapping_add(1);
        let preview_gen = self.heavy_preview_gen_counter;
        self.heavy_preview_expected_gen = preview_gen;
        self.heavy_preview_expected_path = Some(path.clone());
        self.heavy_preview_expected_tool = Some(tool);

        self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
        let overlay_gen = self.overlay_gen_counter;
        self.overlay_expected_gen = overlay_gen;
        self.overlay_expected_path = Some(path.clone());
        self.overlay_expected_tool = Some(tool);

        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(tool);
        }

        let (preview_tx, preview_rx) = mpsc::channel::<super::HeavyPreviewMessage>();
        let (overlay_tx, overlay_rx) = mpsc::channel::<super::HeavyOverlayMessage>();
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            let channels = if use_path {
                let Ok((mut decoded, input_sample_rate)) = crate::wave::decode_wav_multi(&path)
                else {
                    return;
                };
                if input_sample_rate != sample_rate {
                    decoded = crate::wave::resample_channels_quality(
                        &decoded,
                        input_sample_rate,
                        sample_rate,
                        resample_quality,
                    );
                }
                if let Some(depth) = bit_depth {
                    for channel in &mut decoded {
                        crate::wave::quantize_mono_in_place(channel, depth);
                    }
                }
                decoded
            } else {
                fallback_channels.as_ref().clone()
            };
            if channels.is_empty() || channels.first().is_none_or(Vec::is_empty) {
                return;
            }

            // Pitch shifting preserves the timeline. Publish a cheap source
            // overview before the expensive render so long clips get immediate
            // visual feedback while both the audition buffer and the exact
            // full-sample overlay are still pending.
            let overview = Self::build_overview_bins_from_channels(&channels);
            if !overview.is_empty() {
                let overlay =
                    Self::preview_overlay_from_overview(overview, tool, base_timeline_len);
                let _ = overlay_tx.send((path.clone(), tool, overlay, overlay_gen, false));
            }

            let playback = crate::wave::process_pitchshift_curve_multi_spliced(
                &channels,
                sample_rate,
                &points,
                fallback_semitones,
                range,
            );
            if playback.is_empty() || playback.first().is_none_or(Vec::is_empty) {
                return;
            }
            let timeline_len = playback.first().map(Vec::len).unwrap_or(1);
            let overlay = Self::preview_overlay_from_channels(playback.clone(), tool, timeline_len);
            let _ = overlay_tx.send((path.clone(), tool, overlay, overlay_gen, true));
            let _ = preview_tx.send((
                path,
                tool,
                super::HeavyPreviewAudio::Channels(playback),
                preview_gen,
            ));
        });
        self.heavy_preview_rx = Some(preview_rx);
        self.heavy_overlay_rx = Some(overlay_rx);
    }

    /// Long-clip Gain-curve preview: render both the audition buffer and the
    /// green overview from the same processed channels off-thread.
    fn spawn_gain_env_preview_for_tab(&mut self, tab_idx: usize, points: Vec<(usize, f32)>) {
        use std::sync::mpsc;

        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if points.is_empty() {
            return;
        }
        let path = tab.path.clone();
        let channels = tab.ch_samples_arc.clone();
        let base_timeline_len = tab.samples_len.max(1);
        let tool = ToolKind::Gain;

        self.audio.stop();
        self.clear_heavy_preview_state();
        self.clear_heavy_overlay_state();

        self.heavy_preview_gen_counter = self.heavy_preview_gen_counter.wrapping_add(1);
        let preview_gen = self.heavy_preview_gen_counter;
        self.heavy_preview_expected_gen = preview_gen;
        self.heavy_preview_expected_path = Some(path.clone());
        self.heavy_preview_expected_tool = Some(tool);

        self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
        let overlay_gen = self.overlay_gen_counter;
        self.overlay_expected_gen = overlay_gen;
        self.overlay_expected_path = Some(path.clone());
        self.overlay_expected_tool = Some(tool);

        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(tool);
        }

        let (preview_tx, preview_rx) = mpsc::channel::<super::HeavyPreviewMessage>();
        let (overlay_tx, overlay_rx) = mpsc::channel::<super::HeavyOverlayMessage>();
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            let mut playback = channels.as_ref().clone();
            for channel in &mut playback {
                crate::wave::apply_gain_envelope_in_place(channel, &points, 0.0, false);
            }
            if playback.first().is_none_or(|channel| channel.is_empty()) {
                return;
            }
            let overview = Self::build_overview_bins_from_channels(&playback);
            if !overview.is_empty() {
                let overlay =
                    Self::preview_overlay_from_overview(overview, tool, base_timeline_len);
                let _ = overlay_tx.send((path.clone(), tool, overlay, overlay_gen, true));
            }
            let _ = preview_tx.send((
                path,
                tool,
                super::HeavyPreviewAudio::Channels(playback),
                preview_gen,
            ));
        });
        self.heavy_preview_rx = Some(preview_rx);
        self.heavy_overlay_rx = Some(overlay_rx);
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
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if !Self::view_supports_wave_preview(tab.leaf_view_mode(), tab.show_waveform_overlay) {
            return;
        }
        if !Self::tool_supports_preview(tab.active_tool) {
            return;
        }
        if !force && Self::preview_matches_tool(tab, tab.active_tool) {
            return;
        }
        if self.current_tab_preview_busy(tab_idx) {
            return;
        }
        let tool = tab.active_tool;
        let st = tab.tool_state;
        let fade_in_ms = st.fade_in_ms;
        let fade_out_ms = st.fade_out_ms;
        let fade_in_shape = tab.fade_in_shape;
        let fade_out_shape = tab.fade_out_shape;
        let gain_db = st.gain_db;
        let normalize_db = st.normalize_target_db;
        let semitones = st.pitch_semitones;
        let stretch_rate = st.stretch_rate;
        let speed_rate = st.speed_rate;
        let noise_gate_params = crate::wave::NoiseGateParams {
            threshold_db: st.noise_gate_threshold_db,
            attack_ms: st.noise_gate_attack_ms,
            release_ms: st.noise_gate_release_ms,
        };
        let eq_params = crate::wave::ThreeBandEqParams {
            low_shelf_freq_hz: st.eq_low_shelf_freq_hz,
            low_shelf_gain_db: st.eq_low_shelf_gain_db,
            mid_freq_hz: st.eq_mid_freq_hz,
            mid_gain_db: st.eq_mid_gain_db,
            mid_q: st.eq_mid_q,
            high_shelf_freq_hz: st.eq_high_shelf_freq_hz,
            high_shelf_gain_db: st.eq_high_shelf_gain_db,
        };
        let compressor_params = crate::wave::CompressorParams {
            threshold_db: st.compressor_threshold_db,
            ratio: st.compressor_ratio,
            attack_ms: st.compressor_attack_ms,
            release_ms: st.compressor_release_ms,
            makeup_db: st.compressor_makeup_db,
        };
        let insert_silence_samples = ((st.insert_silence_ms.max(0.0) / 1000.0)
            * tab.buffer_sample_rate.max(1) as f32)
            .round() as usize;
        let declick_sensitivity = st.declick_sensitivity;
        let declip_sensitivity = st.declip_sensitivity;
        let dehum_config = crate::app::dehum::DehumConfig {
            base_hz: st.dehum_hz.clamp(20.0, 400.0),
            harmonics: st.dehum_harmonics.clamp(1, 16),
            q: st.dehum_q.clamp(5.0, 100.0),
            depth_db: st.dehum_depth_db.clamp(3.0, 80.0),
        };
        let sel_range = tab
            .selection
            .filter(|(s, e)| *e > *s && *e <= tab.samples_len);
        let gain_env_active = tab.gain_env_enabled && !tab.gain_env_points.is_empty();
        let gain_env_points = tab.gain_env_points.clone();
        let pitch_env_active = tab.pitch_env_enabled && !tab.pitch_env_points.is_empty();
        let pitch_env_points = tab.pitch_env_points.clone();
        let allow_light_preview = tab.samples_len <= LIVE_PREVIEW_SAMPLE_LIMIT;
        let use_path_preview = !allow_light_preview && !tab.dirty;
        let tab_path = tab.path.clone();
        let needs_owned_channels = allow_light_preview
            || matches!(tool, ToolKind::InvertPolarity | ToolKind::DcOffset)
            || (!use_path_preview && matches!(tool, ToolKind::TimeStretch | ToolKind::Speed));
        let ch_samples = if needs_owned_channels {
            tab.ch_samples.clone()
        } else {
            Vec::new()
        };
        let samples_len = tab.samples_len;
        let buffer_sample_rate = tab.buffer_sample_rate.max(1);
        let out_sample_rate = self.audio.shared.out_sample_rate.max(1);
        let decode_failed = self.is_decode_failed_path(&tab.path);
        // Custom channel view scopes destructive range edits; light previews
        // apply the same mask so what you hear matches what Apply does.
        let ch_mask = Self::editor_channel_mask(tab);
        let _ = tab;
        let insert_position = self.editor_insert_position(tab_idx);

        match tool {
            ToolKind::PitchShift => {
                let has_shift = if pitch_env_active {
                    pitch_env_points
                        .iter()
                        .any(|(_, semitones)| semitones.abs() > 0.0001)
                } else {
                    semitones.abs() > 0.0001
                };
                if !has_shift || decode_failed {
                    return;
                }
                self.spawn_pitch_curve_preview_for_tab(
                    tab_idx,
                    if pitch_env_active {
                        pitch_env_points
                    } else {
                        Vec::new()
                    },
                    semitones,
                    sel_range,
                    use_path_preview,
                );
            }
            ToolKind::TimeStretch | ToolKind::Speed => {
                let param = if matches!(tool, ToolKind::TimeStretch) {
                    stretch_rate
                } else {
                    speed_rate
                };
                let is_noop = (param - 1.0).abs() <= 0.0001;
                if is_noop || decode_failed {
                    return;
                }
                self.audio.stop();
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_audio_tool = Some(tool);
                }
                if use_path_preview {
                    self.spawn_heavy_preview_from_path(tab_path.clone(), tool, param, sel_range);
                    self.spawn_heavy_overlay_from_path(tab_path, tool, param, sel_range);
                } else {
                    let mono = Self::mixdown_channels(&ch_samples, samples_len);
                    if mono.is_empty() {
                        return;
                    }
                    self.spawn_heavy_preview_owned(mono, tool, param, sel_range);
                    self.spawn_heavy_overlay_for_tab(tab_idx, tool, param, sel_range);
                }
            }
            ToolKind::Fade => {
                if fade_in_ms <= 0.0 && fade_out_ms <= 0.0 {
                    return;
                }
                let mut overlay = ch_samples.clone();
                let fade_sr = buffer_sample_rate as f32;
                let n_in = ((fade_in_ms / 1000.0) * fade_sr).round() as usize;
                let n_out = ((fade_out_ms / 1000.0) * fade_sr).round() as usize;
                if !allow_light_preview {
                    self.spawn_long_processed_preview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Fade {
                            fade_in_samples: n_in,
                            fade_out_samples: n_out,
                            fade_in_shape,
                            fade_out_shape,
                        },
                        ch_mask,
                    );
                    return;
                }
                if n_in > 0 {
                    for (ci, ch) in overlay.iter_mut().enumerate() {
                        if ch_mask.as_ref().is_some_and(|m| !m[ci]) {
                            continue;
                        }
                        let nn = n_in.min(ch.len());
                        if nn > 0 {
                            Self::apply_fade_in_to_slice(&mut ch[..nn], fade_in_shape);
                        }
                    }
                }
                if n_out > 0 {
                    for (ci, ch) in overlay.iter_mut().enumerate() {
                        if ch_mask.as_ref().is_some_and(|m| !m[ci]) {
                            continue;
                        }
                        let len = ch.len();
                        let nn = n_out.min(len);
                        if nn > 0 {
                            Self::apply_fade_out_to_slice(&mut ch[len - nn..], fade_out_shape);
                        }
                    }
                }
                if overlay.first().map(|c| c.is_empty()).unwrap_or(true) {
                    return;
                }
                let playback = overlay.clone();
                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::Fade,
                        timeline_len,
                    ));
                }
                self.set_preview_channels(tab_idx, ToolKind::Fade, playback);
            }
            ToolKind::Gain => {
                if !gain_env_active && gain_db.abs() <= 1e-6 {
                    return;
                }
                if !allow_light_preview {
                    if gain_env_active {
                        // Long clip: scale the overview bins by the envelope so
                        // the drawn curve still previews visually.
                        self.spawn_gain_env_preview_for_tab(tab_idx, gain_env_points);
                    } else {
                        self.spawn_long_processed_preview_for_tab(
                            tab_idx,
                            LongPreviewJobKind::Gain { gain_db },
                            ch_mask,
                        );
                    }
                    return;
                }
                let mut overlay = ch_samples.clone();
                if gain_env_active {
                    for ch in overlay.iter_mut() {
                        crate::wave::apply_gain_envelope_in_place(
                            ch,
                            &gain_env_points,
                            gain_db,
                            false,
                        );
                    }
                } else {
                    let g = db_to_amp(gain_db);
                    for (ci, ch) in overlay.iter_mut().enumerate() {
                        if ch_mask.as_ref().is_some_and(|m| !m[ci]) {
                            continue;
                        }
                        for v in ch.iter_mut() {
                            *v *= g;
                        }
                    }
                }
                if overlay.first().map(|c| c.is_empty()).unwrap_or(true) {
                    return;
                }
                let playback = overlay.clone();
                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::Gain,
                        timeline_len,
                    ));
                }
                self.set_preview_channels(tab_idx, ToolKind::Gain, playback);
            }
            ToolKind::Normalize => {
                if !allow_light_preview {
                    self.spawn_long_processed_preview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Normalize {
                            target_db: normalize_db,
                        },
                        ch_mask,
                    );
                    return;
                }
                // Peak across the edited channels (matches the destructive
                // apply), then one uniform gain so balance is preserved.
                let mut peak = 0.0f32;
                for (ci, ch) in ch_samples.iter().enumerate() {
                    if ch_mask.as_ref().is_some_and(|m| !m[ci]) {
                        continue;
                    }
                    for &v in ch.iter() {
                        peak = peak.max(v.abs());
                    }
                }
                if peak <= 0.0 {
                    return;
                }
                let g = db_to_amp(normalize_db) / peak.max(1e-12);
                let mut overlay = ch_samples.clone();
                for (ci, ch) in overlay.iter_mut().enumerate() {
                    if ch_mask.as_ref().is_some_and(|m| !m[ci]) {
                        continue;
                    }
                    for v in ch.iter_mut() {
                        *v *= g;
                    }
                }
                if overlay.first().map(|c| c.is_empty()).unwrap_or(true) {
                    return;
                }
                let playback = overlay.clone();
                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::Normalize,
                        timeline_len,
                    ));
                }
                self.set_preview_channels(tab_idx, ToolKind::Normalize, playback);
            }
            ToolKind::Loudness => {
                if !allow_light_preview {
                    self.spawn_long_processed_preview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Loudness {
                            target_lufs: st.loudness_target_lufs,
                            out_sample_rate,
                        },
                        None,
                    );
                    return;
                }
                if let Ok(lufs) =
                    crate::wave::lufs_integrated_from_multi(&ch_samples, out_sample_rate)
                {
                    if !lufs.is_finite() {
                        return;
                    }
                    let gain_db = st.loudness_target_lufs - lufs;
                    let gain = db_to_amp(gain_db);
                    let mut overlay = ch_samples.clone();
                    // Match the unclamped destructive apply.
                    for ch in overlay.iter_mut() {
                        for v in ch.iter_mut() {
                            *v *= gain;
                        }
                    }
                    if overlay.first().map(|c| c.is_empty()).unwrap_or(true) {
                        return;
                    }
                    let playback = overlay.clone();
                    let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(samples_len);
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                            overlay,
                            ToolKind::Loudness,
                            timeline_len,
                        ));
                    }
                    self.set_preview_channels(tab_idx, ToolKind::Loudness, playback);
                }
            }
            ToolKind::Reverse => {
                if !allow_light_preview {
                    self.spawn_long_processed_preview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Reverse { range: sel_range },
                        ch_mask,
                    );
                    return;
                }
                let mut overlay = ch_samples.clone();
                let sr = self.audio.shared.out_sample_rate.max(1);
                for ch in overlay.iter_mut() {
                    match sel_range {
                        Some((s, e)) => {
                            let xf = crate::wave::splice_xfade_samples(sr, e - s, e - s).min(256);
                            crate::wave::reverse_range_with_crossfade(ch, s, e, xf);
                        }
                        None => ch.reverse(),
                    }
                }
                if overlay.first().map(|c| c.is_empty()).unwrap_or(true) {
                    return;
                }
                let playback = overlay.clone();
                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::Reverse,
                        timeline_len,
                    ));
                }
                self.set_preview_channels(tab_idx, ToolKind::Reverse, playback);
            }
            ToolKind::NoiseGate => {
                if !allow_light_preview {
                    self.spawn_long_processed_preview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::NoiseGate {
                            params: noise_gate_params,
                        },
                        ch_mask,
                    );
                    return;
                }
                let mut playback = ch_samples.clone();
                for (ci, channel) in playback.iter_mut().enumerate() {
                    if ch_mask.as_ref().is_some_and(|mask| !mask[ci]) {
                        continue;
                    }
                    *channel = crate::wave::process_noise_gate_offline(
                        channel,
                        out_sample_rate,
                        &noise_gate_params,
                    );
                }
                if playback.first().is_none_or(|channel| channel.is_empty()) {
                    return;
                }
                let overlay = Self::preview_overlay_from_channels(
                    playback.clone(),
                    ToolKind::NoiseGate,
                    samples_len,
                );
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(overlay);
                }
                self.set_preview_channels(tab_idx, ToolKind::NoiseGate, playback);
            }
            ToolKind::Eq => {
                if !allow_light_preview {
                    self.spawn_long_processed_preview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Eq { params: eq_params },
                        ch_mask,
                    );
                    return;
                }
                let mut playback = ch_samples.clone();
                for (ci, channel) in playback.iter_mut().enumerate() {
                    if ch_mask.as_ref().is_some_and(|mask| !mask[ci]) {
                        continue;
                    }
                    *channel = crate::wave::process_three_band_eq_offline(
                        channel,
                        out_sample_rate,
                        &eq_params,
                    );
                }
                if playback.first().is_none_or(|channel| channel.is_empty()) {
                    return;
                }
                let overlay = Self::preview_overlay_from_channels(
                    playback.clone(),
                    ToolKind::Eq,
                    samples_len,
                );
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(overlay);
                }
                self.set_preview_channels(tab_idx, ToolKind::Eq, playback);
            }
            ToolKind::Compressor => {
                if !allow_light_preview {
                    self.spawn_long_processed_preview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Compressor {
                            params: compressor_params,
                        },
                        ch_mask,
                    );
                    return;
                }
                let mut playback = ch_samples.clone();
                for (ci, channel) in playback.iter_mut().enumerate() {
                    if ch_mask.as_ref().is_some_and(|mask| !mask[ci]) {
                        continue;
                    }
                    *channel = crate::wave::process_compressor_offline(
                        channel,
                        out_sample_rate,
                        &compressor_params,
                    );
                }
                if playback.first().is_none_or(|channel| channel.is_empty()) {
                    return;
                }
                let overlay = Self::preview_overlay_from_channels(
                    playback.clone(),
                    ToolKind::Compressor,
                    samples_len,
                );
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(overlay);
                }
                self.set_preview_channels(tab_idx, ToolKind::Compressor, playback);
            }
            ToolKind::InsertSilence => {
                if insert_silence_samples == 0 {
                    return;
                }
                if !allow_light_preview {
                    self.spawn_long_processed_preview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::InsertSilence {
                            position: insert_position,
                            samples: insert_silence_samples,
                        },
                        None,
                    );
                    return;
                }
                let mut playback = ch_samples.clone();
                for channel in &mut playback {
                    let at = insert_position.min(channel.len());
                    channel.splice(at..at, std::iter::repeat_n(0.0, insert_silence_samples));
                }
                if playback.first().is_none_or(|channel| channel.is_empty()) {
                    return;
                }
                let timeline_len = playback.first().map(Vec::len).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        playback.clone(),
                        ToolKind::InsertSilence,
                        timeline_len,
                    ));
                }
                self.set_preview_channels(tab_idx, ToolKind::InsertSilence, playback);
            }
            ToolKind::DeClick | ToolKind::DeClip | ToolKind::DeHum => {
                let kind = match tool {
                    ToolKind::DeClick => LongPreviewJobKind::DeClick {
                        sensitivity: declick_sensitivity,
                        range: sel_range,
                    },
                    ToolKind::DeClip => LongPreviewJobKind::DeClip {
                        sensitivity: declip_sensitivity,
                        range: sel_range,
                    },
                    ToolKind::DeHum => LongPreviewJobKind::DeHum {
                        config: dehum_config,
                        range: sel_range,
                    },
                    _ => unreachable!(),
                };
                if !allow_light_preview {
                    self.spawn_long_processed_preview_for_tab(tab_idx, kind, None);
                    return;
                }
                let mut playback = ch_samples.clone();
                for channel in &mut playback {
                    match kind {
                        LongPreviewJobKind::DeClick { sensitivity, range } => {
                            let config = crate::app::declick::DeclickConfig {
                                sensitivity: sensitivity.clamp(0.0, 1.0),
                                ..Default::default()
                            };
                            let (processed, _) = crate::app::declick::declick_channel(
                                channel,
                                buffer_sample_rate,
                                &config,
                                range,
                            );
                            *channel = processed;
                        }
                        LongPreviewJobKind::DeClip { sensitivity, range } => {
                            let config = crate::app::declip::DeclipConfig {
                                sensitivity: sensitivity.clamp(0.0, 1.0),
                                ..Default::default()
                            };
                            let (processed, _) = crate::app::declip::declip_channel(
                                channel,
                                buffer_sample_rate,
                                &config,
                                range,
                            );
                            *channel = processed;
                        }
                        LongPreviewJobKind::DeHum { config, range } => {
                            let filtered = crate::app::dehum::dehum_channel(
                                channel,
                                buffer_sample_rate,
                                &config,
                            );
                            *channel = match range {
                                Some((start, end)) => crate::app::dehum::splice_processed_range(
                                    channel,
                                    &filtered,
                                    start,
                                    end,
                                    (buffer_sample_rate / 100).max(16) as usize,
                                ),
                                None => filtered,
                            };
                        }
                        _ => unreachable!(),
                    }
                }
                if playback.first().is_none_or(|channel| channel.is_empty()) {
                    return;
                }
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        playback.clone(),
                        tool,
                        samples_len,
                    ));
                }
                self.set_preview_channels(tab_idx, tool, playback);
            }
            ToolKind::InvertPolarity => {
                // Negation is O(n) with no analysis, so the light path is fine
                // even for long files (one buffer clone, same as the apply).
                let mut overlay = ch_samples.clone();
                let (s, e) = sel_range.unwrap_or((0, samples_len));
                for (ci, ch) in overlay.iter_mut().enumerate() {
                    if ch_mask.as_ref().is_some_and(|m| !m[ci]) {
                        continue;
                    }
                    let end = e.min(ch.len());
                    for v in &mut ch[s.min(end)..end] {
                        *v = -*v;
                    }
                }
                if overlay.first().map(|c| c.is_empty()).unwrap_or(true) {
                    return;
                }
                let playback = overlay.clone();
                let timeline_len = overlay.first().map(|c| c.len()).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::InvertPolarity,
                        timeline_len,
                    ));
                }
                self.set_preview_channels(tab_idx, ToolKind::InvertPolarity, playback);
            }
            ToolKind::DcOffset => {
                let mut overlay = ch_samples.clone();
                let (s, e) = sel_range.unwrap_or((0, samples_len));
                for (ci, ch) in overlay.iter_mut().enumerate() {
                    if ch_mask.as_ref().is_some_and(|m| !m[ci]) {
                        continue;
                    }
                    Self::dc_remove_range(ch, s, e);
                }
                if overlay.first().map(|c| c.is_empty()).unwrap_or(true) {
                    return;
                }
                let playback = overlay.clone();
                let timeline_len = overlay.first().map(|c| c.len()).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::DcOffset,
                        timeline_len,
                    ));
                }
                self.set_preview_channels(tab_idx, ToolKind::DcOffset, playback);
            }
            _ => {}
        }
    }

    pub(super) fn clear_preview_if_any(&mut self, tab_idx: usize) {
        let had_preview_audio = self
            .tabs
            .get(tab_idx)
            .and_then(|tab| tab.preview_audio_tool)
            .is_some();
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
        // also discard any in-flight preview/overlay job
        self.clear_heavy_preview_state();
        self.clear_heavy_overlay_state();
        self.cancel_music_preview_run();
    }

    pub(super) fn spawn_heavy_preview_owned(
        &mut self,
        mono: Vec<f32>,
        tool: ToolKind,
        param: f32,
        range: Option<(usize, usize)>,
    ) {
        use std::sync::mpsc;
        let sr = self.audio.shared.out_sample_rate;
        let path = self
            .active_tab
            .and_then(|idx| self.tabs.get(idx).map(|tab| tab.path.clone()))
            .unwrap_or_default();
        self.clear_heavy_preview_state();
        self.heavy_preview_gen_counter = self.heavy_preview_gen_counter.wrapping_add(1);
        let gen = self.heavy_preview_gen_counter;
        self.heavy_preview_expected_gen = gen;
        self.heavy_preview_expected_path = Some(path.clone());
        self.heavy_preview_expected_tool = Some(tool);
        let (tx, rx) = mpsc::channel::<super::HeavyPreviewMessage>();
        std::thread::spawn(move || {
            let out = match tool {
                ToolKind::PitchShift | ToolKind::TimeStretch | ToolKind::Speed => {
                    Self::process_tool_segment_spliced(&mono, tool, param, sr, range)
                }
                _ => mono,
            };
            let _ = tx.send((path, tool, super::HeavyPreviewAudio::Mono(out), gen));
        });
        self.heavy_preview_rx = Some(rx);
    }

    pub(super) fn spawn_heavy_preview_from_path(
        &mut self,
        path: PathBuf,
        tool: ToolKind,
        param: f32,
        range: Option<(usize, usize)>,
    ) {
        use std::sync::mpsc;
        let sr = self.audio.shared.out_sample_rate;
        let resample_quality = Self::to_wave_resample_quality(self.src_quality);
        let bit_depth = self.bit_depth_override.get(&path).copied();
        self.clear_heavy_preview_state();
        self.heavy_preview_gen_counter = self.heavy_preview_gen_counter.wrapping_add(1);
        let gen = self.heavy_preview_gen_counter;
        self.heavy_preview_expected_gen = gen;
        self.heavy_preview_expected_path = Some(path.clone());
        self.heavy_preview_expected_tool = Some(tool);
        let (tx, rx) = mpsc::channel::<super::HeavyPreviewMessage>();
        let out_path = path.clone();
        std::thread::spawn(move || {
            let (mut mono, in_sr) = match crate::wave::decode_wav_mono(&path) {
                Ok(v) => v,
                Err(_) => return,
            };
            mono = if in_sr != sr {
                crate::wave::resample_quality(&mono, in_sr, sr, resample_quality)
            } else {
                mono
            };
            if let Some(depth) = bit_depth {
                crate::wave::quantize_mono_in_place(&mut mono, depth);
            }
            let out = match tool {
                ToolKind::PitchShift | ToolKind::TimeStretch | ToolKind::Speed => {
                    Self::process_tool_segment_spliced(&mono, tool, param, sr, range)
                }
                _ => mono,
            };
            let _ = tx.send((out_path, tool, super::HeavyPreviewAudio::Mono(out), gen));
        });
        self.heavy_preview_rx = Some(rx);
    }

    // Spawn per-channel overlay generator (Pitch/Stretch) in a worker thread.
    // Note: Call this ONLY after UI borrows end (see E0499 note) to avoid nested &mut self borrows.
    pub(super) fn spawn_heavy_overlay_for_tab(
        &mut self,
        tab_idx: usize,
        tool: ToolKind,
        param: f32,
        range: Option<(usize, usize)>,
    ) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let send_overview_first = tab.samples_len > LIVE_PREVIEW_SAMPLE_LIMIT;
        let Some(kind) = Self::heavy_overlay_job_kind(tool, param, range) else {
            return;
        };
        self.spawn_overlay_job_for_tab(
            tab_idx,
            kind,
            Some(FullOverlayRenderMode::Buffer),
            send_overview_first,
        );
    }

    fn heavy_overlay_job_kind(
        tool: ToolKind,
        param: f32,
        range: Option<(usize, usize)>,
    ) -> Option<LongPreviewJobKind> {
        match tool {
            ToolKind::PitchShift => Some(LongPreviewJobKind::PitchShift {
                semitones: param,
                range,
            }),
            ToolKind::TimeStretch => Some(LongPreviewJobKind::TimeStretch { rate: param, range }),
            ToolKind::Speed => Some(LongPreviewJobKind::Speed { rate: param, range }),
            _ => None,
        }
    }

    pub(super) fn spawn_heavy_overlay_from_path(
        &mut self,
        path: PathBuf,
        tool: ToolKind,
        param: f32,
        range: Option<(usize, usize)>,
    ) {
        let Some(tab_idx) = self.tabs.iter().position(|tab| tab.path == path) else {
            return;
        };
        let send_overview_first = self
            .tabs
            .get(tab_idx)
            .map(|tab| tab.samples_len > LIVE_PREVIEW_SAMPLE_LIMIT)
            .unwrap_or(false);
        let Some(kind) = Self::heavy_overlay_job_kind(tool, param, range) else {
            return;
        };
        self.spawn_overlay_job_for_tab(
            tab_idx,
            kind,
            Some(FullOverlayRenderMode::Path),
            send_overview_first,
        );
    }

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
