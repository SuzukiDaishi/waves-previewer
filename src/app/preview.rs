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
    }

    pub(super) fn current_tab_preview_message(&self, tab_idx: usize) -> Option<String> {
        if !self.current_tab_preview_busy(tab_idx) {
            return None;
        }
        let tool = self
            .heavy_preview_expected_tool
            .or(self.overlay_expected_tool);
        Some(match tool {
            Some(ToolKind::PitchShift) => "Previewing PitchShift...".to_string(),
            Some(ToolKind::TimeStretch) => "Previewing TimeStretch...".to_string(),
            Some(ToolKind::Speed) => "Previewing Speed...".to_string(),
            Some(ToolKind::SpectralWarp) => "Previewing Spectral Warp...".to_string(),
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
        self.audio.stop();
        self.audio.set_samples_mono(mono);
        self.playback_mark_buffer_source(
            super::PlaybackSourceKind::ToolPreview,
            self.audio.shared.out_sample_rate.max(1),
        );
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(tool);
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
        self.audio.stop();
        self.audio.set_samples_channels(channels);
        self.playback_mark_buffer_source(
            super::PlaybackSourceKind::ToolPreview,
            self.audio.shared.out_sample_rate.max(1),
        );
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(tool);
        }
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
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

    fn preview_peak_from_channels(channels: &[Vec<f32>], samples_len: usize) -> Option<f32> {
        let mono = Self::mixdown_channels(channels, samples_len);
        if mono.is_empty() {
            return None;
        }
        let mut peak = 0.0f32;
        for &sample in &mono {
            peak = peak.max(sample.abs());
        }
        (peak > 0.0).then_some(peak)
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
                let peak = Self::preview_peak_from_channels(fallback_channels, base_timeline_len)?;
                let gain = db_to_amp(target_db) / peak.max(1e-12);
                Self::scale_overview_in_place(&mut overview, gain, false);
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
                            let b0 = ((s as u128) * (bins as u128)
                                / (base_timeline_len as u128))
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
            let processed = Self::process_tool_segment_spliced(
                channel,
                tool,
                param,
                sample_rate.max(1),
                range,
            );
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

    fn spawn_long_preview_overview_for_tab(&mut self, tab_idx: usize, kind: LongPreviewJobKind) {
        self.spawn_overlay_job_for_tab(tab_idx, kind, None, true);
    }

    /// Long-clip Gain-curve preview: build the source overview off-thread and
    /// scale each bin by the envelope level at the bin's timeline position.
    fn spawn_gain_env_overview_for_tab(&mut self, tab_idx: usize, points: Vec<(usize, f32)>) {
        use std::sync::mpsc;

        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if points.is_empty() {
            return;
        }
        let path = tab.path.clone();
        let fallback_channels = tab.ch_samples.clone();
        let base_timeline_len = tab.samples_len.max(1);

        self.clear_heavy_overlay_state();
        self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
        let gen = self.overlay_gen_counter;
        self.overlay_expected_gen = gen;
        self.overlay_expected_path = Some(path.clone());
        self.overlay_expected_tool = Some(ToolKind::Gain);

        let (tx, rx) = mpsc::channel::<super::HeavyOverlayMessage>();
        std::thread::spawn(move || {
            let Some(mut overview) = Self::build_source_overview_bins(&path, &fallback_channels)
            else {
                return;
            };
            for channel in &mut overview {
                let bins_len = channel.len().max(1);
                for (idx, (mn, mx)) in channel.iter_mut().enumerate() {
                    let pos = (((idx as f64) + 0.5) * (base_timeline_len as f64)
                        / (bins_len as f64))
                        .round() as usize;
                    let db = crate::wave::gain_envelope_db_at(&points, 0.0, pos);
                    let g = db_to_amp(db);
                    let lo = *mn * g;
                    let hi = *mx * g;
                    *mn = lo.min(hi);
                    *mx = lo.max(hi);
                }
            }
            let overlay = Self::preview_overlay_from_overview(
                overview,
                ToolKind::Gain,
                base_timeline_len,
            );
            let _ = tx.send((path, ToolKind::Gain, overlay, gen, true));
        });
        self.heavy_overlay_rx = Some(rx);
    }

    pub(super) fn refresh_tool_preview_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if !Self::view_supports_wave_preview(tab.leaf_view_mode(), tab.show_waveform_overlay) {
            return;
        }
        if !Self::tool_supports_preview(tab.active_tool) {
            return;
        }
        if Self::preview_matches_tool(tab, tab.active_tool) {
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
        let sel_range = tab
            .selection
            .filter(|(s, e)| *e > *s && *e <= tab.samples_len);
        let gain_env_active = tab.gain_env_enabled && !tab.gain_env_points.is_empty();
        let gain_env_points = tab.gain_env_points.clone();
        let allow_light_preview = tab.samples_len <= LIVE_PREVIEW_SAMPLE_LIMIT;
        let use_path_preview = !allow_light_preview && !tab.dirty;
        let tab_path = tab.path.clone();
        let ch_samples = tab.ch_samples.clone();
        let samples_len = tab.samples_len;
        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
        let out_sample_rate = self.audio.shared.out_sample_rate.max(1);
        let decode_failed = self.is_decode_failed_path(&tab.path);
        // Custom channel view scopes destructive range edits; light previews
        // apply the same mask so what you hear matches what Apply does.
        let ch_mask = Self::editor_channel_mask(tab);
        let _ = tab;

        match tool {
            ToolKind::PitchShift | ToolKind::TimeStretch | ToolKind::Speed => {
                let param = match tool {
                    ToolKind::PitchShift => semitones,
                    ToolKind::TimeStretch => stretch_rate,
                    _ => speed_rate,
                };
                let is_noop = match tool {
                    ToolKind::PitchShift => semitones.abs() <= 0.0001,
                    _ => (param - 1.0).abs() <= 0.0001,
                };
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
                let n_in = ((fade_in_ms / 1000.0) * sr).round() as usize;
                let n_out = ((fade_out_ms / 1000.0) * sr).round() as usize;
                if !allow_light_preview {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.preview_audio_tool = None;
                    }
                    self.spawn_long_preview_overview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Fade {
                            fade_in_samples: n_in,
                            fade_out_samples: n_out,
                            fade_in_shape,
                            fade_out_shape,
                        },
                    );
                    return;
                }
                if n_in > 0 {
                    for (ci, ch) in overlay.iter_mut().enumerate() {
                        if ch_mask.as_ref().is_some_and(|m| !m[ci]) {
                            continue;
                        }
                        let nn = n_in.min(ch.len());
                        for i in 0..nn {
                            let t = i as f32 / nn.max(1) as f32;
                            let w = Self::fade_weight(fade_in_shape, t);
                            ch[i] *= w;
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
                        for i in 0..nn {
                            let t = i as f32 / nn.max(1) as f32;
                            let w = Self::fade_weight_out(fade_out_shape, t);
                            let idx = len - nn + i;
                            ch[idx] *= w;
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
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.preview_audio_tool = None;
                    }
                    if gain_env_active {
                        // Long clip: scale the overview bins by the envelope so
                        // the drawn curve still previews visually.
                        self.spawn_gain_env_overview_for_tab(tab_idx, gain_env_points);
                    } else {
                        self.spawn_long_preview_overview_for_tab(
                            tab_idx,
                            LongPreviewJobKind::Gain { gain_db },
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
                const DEFAULT_NORMALIZE_DB: f32 = -6.0;
                if (normalize_db - DEFAULT_NORMALIZE_DB).abs() <= 1e-6 {
                    return;
                }
                if !allow_light_preview {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.preview_audio_tool = None;
                    }
                    self.spawn_long_preview_overview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Normalize {
                            target_db: normalize_db,
                        },
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
                const DEFAULT_LOUDNESS_LUFS: f32 = -14.0;
                if (st.loudness_target_lufs - DEFAULT_LOUDNESS_LUFS).abs() <= 1e-6 {
                    return;
                }
                if !allow_light_preview {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.preview_audio_tool = None;
                    }
                    self.spawn_long_preview_overview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Loudness {
                            target_lufs: st.loudness_target_lufs,
                            out_sample_rate,
                        },
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
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.preview_audio_tool = None;
                    }
                    self.spawn_long_preview_overview_for_tab(
                        tab_idx,
                        LongPreviewJobKind::Reverse { range: sel_range },
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
            tab.preview_overlay = None;
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
            let _ = tx.send((path, tool, out, gen));
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
            let _ = tx.send((out_path, tool, out, gen));
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
