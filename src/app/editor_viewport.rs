use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, ColorImage, TextureOptions};

use super::helpers::{db_to_color, lerp_color};
use super::render::waveform_pyramid as wf_cache;
use super::types::{
    EditorTab, EditorViewportCachePayload, EditorViewportJobMsg, EditorViewportPayloadKind,
    EditorViewportRenderCache, EditorViewportRenderKey, EditorViewportRenderPayload,
    EditorViewportRenderQuality, EditorViewportWaveLane, EditorViewportWavePayload,
    SpectrogramConfig, SpectrogramData, SpectrogramScale, ViewMode,
};

#[derive(Clone, Debug)]
pub(super) struct EditorViewportHint {
    pub view_mode: ViewMode,
    pub display_samples_len: usize,
    pub start: usize,
    pub end: usize,
    pub wave_width_px: usize,
    pub lane_height_px: usize,
    pub lane_count: usize,
    pub use_mixdown: bool,
    pub visible_channels: Vec<usize>,
}

#[derive(Clone)]
enum EditorViewportRequest {
    Waveform {
        tab_path: PathBuf,
        generation: u64,
        quality: EditorViewportRenderQuality,
        key: EditorViewportRenderKey,
        lanes: Vec<WaveLaneRequest>,
    },
    Spectral {
        tab_path: PathBuf,
        generation: u64,
        quality: EditorViewportRenderQuality,
        key: EditorViewportRenderKey,
        specs: Arc<Vec<SpectrogramData>>,
        lane_spec_indices: Vec<usize>,
        wave_width_px: usize,
        lane_height_px: usize,
        lane_count: usize,
        start: usize,
        end: usize,
        vertical_zoom: f32,
        vertical_view_center: f32,
        cfg: SpectrogramConfig,
        view_mode: ViewMode,
    },
}

#[derive(Clone)]
enum WaveLaneRequest {
    Overview {
        overview: Vec<(f32, f32)>,
        display_samples_len: usize,
        start: usize,
        end: usize,
        bins: usize,
    },
    Samples {
        samples: Vec<f32>,
        bins: usize,
        render_raw: bool,
    },
    MixdownSamples {
        channels: Vec<Vec<f32>>,
        bins: usize,
        render_raw: bool,
    },
    Pyramid {
        pyramid: Arc<wf_cache::PeakPyramid>,
        start: usize,
        end: usize,
        bins: usize,
        spp: f32,
    },
}

impl super::WavesPreviewer {
    pub(super) fn editor_visible_vertical_fraction(vertical_zoom: f32) -> f32 {
        1.0 / vertical_zoom.clamp(
            super::EDITOR_MIN_VERTICAL_ZOOM,
            super::EDITOR_MAX_VERTICAL_ZOOM,
        )
    }

    pub(super) fn editor_clamped_vertical_center(vertical_zoom: f32, center: f32) -> f32 {
        let zoom = vertical_zoom.clamp(
            super::EDITOR_MIN_VERTICAL_ZOOM,
            super::EDITOR_MAX_VERTICAL_ZOOM,
        );
        if zoom <= 1.0 {
            0.0
        } else {
            let visible_fraction = Self::editor_visible_vertical_fraction(zoom).clamp(0.0, 1.0);
            let limit = (1.0 - visible_fraction).max(0.0);
            center.clamp(-limit, limit)
        }
    }

    pub(super) fn editor_vertical_range_for_view(
        view_mode: ViewMode,
        vertical_zoom: f32,
        vertical_view_center: f32,
        _cfg: &SpectrogramConfig,
    ) -> (f32, f32) {
        let center = if matches!(
            view_mode,
            ViewMode::Waveform | ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel
        ) {
            Self::editor_clamped_vertical_center(vertical_zoom, vertical_view_center)
        } else {
            0.0
        };
        let visible_fraction =
            Self::editor_visible_vertical_fraction(vertical_zoom).clamp(0.0, 1.0);
        let center_frac = ((center + 1.0) * 0.5).clamp(0.0, 1.0);
        let min_frac = (center_frac - visible_fraction * 0.5).clamp(0.0, 1.0);
        let max_frac = (center_frac + visible_fraction * 0.5).clamp(0.0, 1.0);
        if max_frac <= min_frac {
            (0.0, 1.0)
        } else {
            (min_frac, max_frac)
        }
    }

    pub(super) fn invalidate_editor_viewport_cache(tab: &mut EditorTab) {
        tab.viewport_source_generation = tab.viewport_source_generation.wrapping_add(1).max(1);
        tab.viewport_render_requested_generation = 0;
        tab.viewport_render_requested_key = None;
        tab.viewport_render_pending_fine_at = None;
        tab.viewport_render_inflight_coarse_generation = None;
        tab.viewport_render_inflight_fine_generation = None;
        tab.viewport_render_coarse = None;
        tab.viewport_render_fine = None;
        tab.viewport_render_last = None;
    }

    fn ensure_editor_viewport_channel(&mut self) {
        if self.editor_viewport_tx.is_none() || self.editor_viewport_rx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<EditorViewportJobMsg>();
            self.editor_viewport_tx = Some(tx);
            self.editor_viewport_rx = Some(rx);
        }
    }

    fn bump_editor_viewport_generation(&mut self) -> u64 {
        self.editor_viewport_generation_counter = self
            .editor_viewport_generation_counter
            .wrapping_add(1)
            .max(1);
        self.editor_viewport_generation_counter
    }

    pub(super) fn apply_editor_viewport_render_updates(&mut self, ctx: &egui::Context) {
        let mut messages = Vec::new();
        if let Some(rx) = &self.editor_viewport_rx {
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }
        for msg in messages {
            let EditorViewportJobMsg::Ready {
                tab_path,
                generation,
                quality,
                key,
                payload,
            } = msg;
            let Some(tab) = self.tabs.iter_mut().find(|tab| tab.path == tab_path) else {
                continue;
            };
            if generation != tab.viewport_render_requested_generation {
                continue;
            }
            if tab
                .viewport_render_requested_key
                .as_ref()
                .map(|requested| requested != &key)
                .unwrap_or(true)
            {
                continue;
            }
            let payload = match payload {
                EditorViewportRenderPayload::Waveform(waveform) => {
                    EditorViewportCachePayload::Waveform(waveform)
                }
                EditorViewportRenderPayload::Image(image) => {
                    let texture_id = format!(
                        "editor_viewport_{}_{}_{}",
                        tab.path.display(),
                        generation,
                        match quality {
                            EditorViewportRenderQuality::Coarse => "coarse",
                            EditorViewportRenderQuality::Fine => "fine",
                        }
                    );
                    let texture =
                        ctx.load_texture(texture_id, (*image).clone(), TextureOptions::LINEAR);
                    EditorViewportCachePayload::Image {
                        image,
                        texture: Some(texture),
                    }
                }
            };
            let cache = EditorViewportRenderCache {
                key,
                quality,
                ready_at: Instant::now(),
                payload,
            };
            tab.viewport_render_last = Some(cache.clone());
            match quality {
                EditorViewportRenderQuality::Coarse => {
                    tab.viewport_render_inflight_coarse_generation = None;
                    tab.viewport_render_coarse = Some(cache);
                }
                EditorViewportRenderQuality::Fine => {
                    tab.viewport_render_inflight_fine_generation = None;
                    tab.viewport_render_pending_fine_at = None;
                    tab.viewport_render_fine = Some(cache);
                }
            }
            ctx.request_repaint();
        }
    }

    pub(super) fn ensure_editor_viewport_for_tab(
        &mut self,
        tab_idx: usize,
        hint: EditorViewportHint,
    ) {
        let Some(request_key) = self.build_editor_viewport_key(tab_idx, &hint) else {
            return;
        };
        let now = Instant::now();
        let (key_changed, needs_fine, previous_generation) = if let Some(tab) =
            self.tabs.get(tab_idx)
        {
            let key_changed = tab
                .viewport_render_requested_key
                .as_ref()
                .map(|existing| existing != &request_key)
                .unwrap_or(true);
            let needs_fine = !key_changed
                && tab.viewport_render_fine.as_ref().map(|cache| &cache.key) != Some(&request_key)
                && tab.viewport_render_inflight_fine_generation.is_none()
                && tab
                    .viewport_render_pending_fine_at
                    .map(|deadline| now >= deadline)
                    .unwrap_or(false);
            (
                key_changed,
                needs_fine,
                tab.viewport_render_requested_generation,
            )
        } else {
            return;
        };
        let generation = if key_changed {
            self.bump_editor_viewport_generation()
        } else {
            previous_generation
        };
        let mut queue_coarse = false;
        let mut queue_fine = false;
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            if key_changed {
                tab.viewport_render_last = tab
                    .viewport_render_fine
                    .clone()
                    .or_else(|| tab.viewport_render_coarse.clone())
                    .or_else(|| tab.viewport_render_last.clone());
                tab.viewport_render_requested_key = Some(request_key.clone());
                tab.viewport_render_requested_generation = generation;
                tab.viewport_render_pending_fine_at =
                    Some(now + Duration::from_millis(super::EDITOR_VIEWPORT_COARSE_FINE_DELAY_MS));
                tab.viewport_render_inflight_coarse_generation = None;
                tab.viewport_render_inflight_fine_generation = None;
                tab.viewport_render_coarse = None;
                tab.viewport_render_fine = None;
                queue_coarse = true;
            } else if needs_fine {
                queue_fine = true;
            }
        } else {
            return;
        }
        if queue_coarse {
            if let Some(request) = self.build_editor_viewport_request(
                tab_idx,
                hint.clone(),
                request_key.clone(),
                generation,
                EditorViewportRenderQuality::Coarse,
            ) {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.viewport_render_inflight_coarse_generation = Some(generation);
                }
                self.queue_editor_viewport_render(request);
            }
        }
        if queue_fine {
            if let Some(request) = self.build_editor_viewport_request(
                tab_idx,
                hint,
                request_key,
                generation,
                EditorViewportRenderQuality::Fine,
            ) {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.viewport_render_inflight_fine_generation = Some(generation);
                }
                self.queue_editor_viewport_render(request);
            }
        }
    }

    #[allow(dead_code)]
    pub(super) fn best_editor_viewport_cache<'a>(
        tab: &'a EditorTab,
        key: &EditorViewportRenderKey,
    ) -> Option<&'a EditorViewportRenderCache> {
        let exact = |cache: &'a EditorViewportRenderCache| cache.key == *key;
        let compatible = |cache: &'a EditorViewportRenderCache| {
            cache.key.kind == key.kind
                && cache.key.view_mode == key.view_mode
                && cache.key.source_generation == key.source_generation
                && cache.key.lane_count == key.lane_count
                && cache.key.use_mixdown == key.use_mixdown
                && cache.key.visible_channels == key.visible_channels
        };
        tab.viewport_render_fine
            .as_ref()
            .filter(|cache| exact(cache))
            .or_else(|| {
                tab.viewport_render_coarse
                    .as_ref()
                    .filter(|cache| exact(cache))
            })
            .or_else(|| {
                tab.viewport_render_last
                    .as_ref()
                    .filter(|cache| compatible(cache))
            })
            .or_else(|| {
                tab.viewport_render_fine
                    .as_ref()
                    .filter(|cache| compatible(cache))
            })
            .or_else(|| {
                tab.viewport_render_coarse
                    .as_ref()
                    .filter(|cache| compatible(cache))
            })
    }

    fn build_editor_viewport_key(
        &self,
        tab_idx: usize,
        hint: &EditorViewportHint,
    ) -> Option<EditorViewportRenderKey> {
        let tab = self.tabs.get(tab_idx)?;
        let kind = match hint.view_mode {
            ViewMode::Waveform => EditorViewportPayloadKind::Waveform,
            ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => {
                if self.spectro_cache.contains_key(&tab.path) {
                    EditorViewportPayloadKind::Spectral
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        Some(EditorViewportRenderKey {
            kind: kind.clone(),
            view_mode: hint.view_mode,
            source_generation: tab.viewport_source_generation,
            display_samples_len: hint.display_samples_len,
            start: hint.start,
            end: hint.end,
            lane_count: hint.lane_count,
            lane_height_px: hint.lane_height_px.max(1),
            wave_width_px: hint.wave_width_px.max(1),
            use_mixdown: hint.use_mixdown,
            visible_channels: hint.visible_channels.clone(),
            samples_per_px_bits: tab.samples_per_px.to_bits(),
            vertical_zoom_bits: tab.vertical_zoom.to_bits(),
            vertical_view_center_bits: tab.vertical_view_center.to_bits(),
            scale_bits: 0,
            spectro_cfg_digest: match kind {
                EditorViewportPayloadKind::Spectral => {
                    Self::editor_spectro_cfg_digest(&self.spectro_cfg)
                }
                EditorViewportPayloadKind::Waveform => 0,
            },
        })
    }

    fn build_editor_viewport_request(
        &self,
        tab_idx: usize,
        hint: EditorViewportHint,
        key: EditorViewportRenderKey,
        generation: u64,
        quality: EditorViewportRenderQuality,
    ) -> Option<EditorViewportRequest> {
        let tab = self.tabs.get(tab_idx)?;
        match hint.view_mode {
            ViewMode::Waveform => {
                let bins = match quality {
                    EditorViewportRenderQuality::Coarse => hint
                        .wave_width_px
                        .min(super::EDITOR_VIEWPORT_COARSE_MAX_COLUMNS)
                        .max(32),
                    EditorViewportRenderQuality::Fine => hint.wave_width_px.max(1),
                };
                let spp = tab.samples_per_px.max(0.0001);
                let visible_len = hint.end.saturating_sub(hint.start);
                let raw_mode = matches!(quality, EditorViewportRenderQuality::Fine) && spp < 2.0;
                let mut lanes = Vec::with_capacity(hint.lane_count.max(1));
                if tab.loading && !tab.loading_waveform_minmax.is_empty() {
                    for _ in 0..hint.lane_count.max(1) {
                        lanes.push(WaveLaneRequest::Overview {
                            overview: tab.loading_waveform_minmax.clone(),
                            display_samples_len: hint.display_samples_len.max(1),
                            start: hint.start,
                            end: hint.end,
                            bins,
                        });
                    }
                } else if hint.use_mixdown {
                    let channel_slices = tab
                        .ch_samples
                        .iter()
                        .map(|channel| {
                            let start = hint.start.min(channel.len());
                            let end = hint.end.min(channel.len()).max(start);
                            channel[start..end].to_vec()
                        })
                        .collect::<Vec<_>>();
                    lanes.push(WaveLaneRequest::MixdownSamples {
                        channels: channel_slices,
                        bins,
                        render_raw: raw_mode && visible_len <= hint.wave_width_px.saturating_mul(4),
                    });
                } else {
                    for &channel_idx in &hint.visible_channels {
                        let Some(channel) = tab.ch_samples.get(channel_idx) else {
                            continue;
                        };
                        let start = hint.start.min(channel.len());
                        let end = hint.end.min(channel.len()).max(start);
                        if matches!(quality, EditorViewportRenderQuality::Fine)
                            && spp >= 32.0
                            && tab.waveform_pyramid.is_some()
                        {
                            if let Some(pyramid_set) = tab.waveform_pyramid.as_ref() {
                                if let Some(pyramid) = pyramid_set.channels.get(channel_idx) {
                                    lanes.push(WaveLaneRequest::Pyramid {
                                        pyramid: pyramid.clone(),
                                        start: hint.start,
                                        end: hint.end,
                                        bins,
                                        spp,
                                    });
                                    continue;
                                }
                            }
                        }
                        lanes.push(WaveLaneRequest::Samples {
                            samples: channel[start..end].to_vec(),
                            bins,
                            render_raw: raw_mode
                                && visible_len <= hint.wave_width_px.saturating_mul(4),
                        });
                    }
                }
                Some(EditorViewportRequest::Waveform {
                    tab_path: tab.path.clone(),
                    generation,
                    quality,
                    key,
                    lanes,
                })
            }
            ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => {
                let specs = self.spectro_cache.get(&tab.path)?.clone();
                let lane_spec_indices = if hint.use_mixdown {
                    vec![0usize; hint.lane_count.max(1)]
                } else if hint.visible_channels.is_empty() {
                    (0..hint.lane_count.max(1)).collect()
                } else {
                    hint.visible_channels
                        .iter()
                        .copied()
                        .take(hint.lane_count.max(1))
                        .collect::<Vec<_>>()
                };
                Some(EditorViewportRequest::Spectral {
                    tab_path: tab.path.clone(),
                    generation,
                    quality,
                    key,
                    specs,
                    lane_spec_indices,
                    wave_width_px: hint.wave_width_px.max(1),
                    lane_height_px: hint.lane_height_px.max(1),
                    lane_count: hint.lane_count.max(1),
                    start: hint.start,
                    end: hint.end,
                    vertical_zoom: tab.vertical_zoom,
                    vertical_view_center: tab.vertical_view_center,
                    cfg: self.spectro_cfg.clone(),
                    view_mode: hint.view_mode,
                })
            }
            _ => None,
        }
    }

    fn queue_editor_viewport_render(&mut self, request: EditorViewportRequest) {
        self.ensure_editor_viewport_channel();
        let Some(tx) = self.editor_viewport_tx.as_ref().cloned() else {
            return;
        };
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            match request {
                EditorViewportRequest::Waveform {
                    tab_path,
                    generation,
                    quality,
                    key,
                    lanes,
                } => {
                    let payload =
                        EditorViewportRenderPayload::Waveform(Self::render_waveform_payload(lanes));
                    let _ = tx.send(EditorViewportJobMsg::Ready {
                        tab_path,
                        generation,
                        quality,
                        key,
                        payload,
                    });
                }
                EditorViewportRequest::Spectral {
                    tab_path,
                    generation,
                    quality,
                    key,
                    specs,
                    lane_spec_indices,
                    wave_width_px,
                    lane_height_px,
                    lane_count,
                    start,
                    end,
                    vertical_zoom,
                    vertical_view_center,
                    cfg,
                    view_mode,
                } => {
                    let image = Arc::new(Self::render_spectral_viewport_image(
                        &specs,
                        &lane_spec_indices,
                        wave_width_px,
                        lane_height_px,
                        lane_count,
                        start,
                        end,
                        vertical_zoom,
                        vertical_view_center,
                        &cfg,
                        view_mode,
                        quality,
                    ));
                    let _ = tx.send(EditorViewportJobMsg::Ready {
                        tab_path,
                        generation,
                        quality,
                        key,
                        payload: EditorViewportRenderPayload::Image(image),
                    });
                }
            }
        });
    }

    fn render_waveform_payload(lanes: Vec<WaveLaneRequest>) -> EditorViewportWavePayload {
        let mut out = Vec::with_capacity(lanes.len());
        for lane in lanes {
            match lane {
                WaveLaneRequest::Overview {
                    overview,
                    display_samples_len,
                    start,
                    end,
                    bins,
                } => {
                    let mut peaks = Vec::new();
                    if !overview.is_empty() && display_samples_len > 0 && end > start {
                        let visible_len = end.saturating_sub(start).max(1);
                        for col in 0..bins.max(1) {
                            let s0 = start.saturating_add(
                                ((visible_len as u128).saturating_mul(col as u128)
                                    / bins.max(1) as u128) as usize,
                            );
                            let s1 = start.saturating_add(
                                ((visible_len as u128).saturating_mul((col + 1) as u128)
                                    / bins.max(1) as u128) as usize,
                            );
                            let mut i0 = ((s0 as u128).saturating_mul(overview.len() as u128)
                                / display_samples_len.max(1) as u128)
                                as usize;
                            let mut i1 = (((s1.max(s0 + 1) as u128)
                                .saturating_mul(overview.len() as u128))
                            .saturating_add(display_samples_len.max(1) as u128 - 1)
                                / display_samples_len.max(1) as u128)
                                as usize;
                            i0 = i0.min(overview.len().saturating_sub(1));
                            i1 = i1.clamp(i0 + 1, overview.len());
                            let mut mn = 1.0f32;
                            let mut mx = -1.0f32;
                            for &(lo, hi) in &overview[i0..i1] {
                                mn = mn.min(lo);
                                mx = mx.max(hi);
                            }
                            peaks.push(wf_cache::Peak { min: mn, max: mx });
                        }
                    }
                    out.push(EditorViewportWaveLane::Peaks(peaks));
                }
                WaveLaneRequest::Samples {
                    samples,
                    bins,
                    render_raw,
                } => {
                    if render_raw {
                        out.push(EditorViewportWaveLane::Samples(samples));
                    } else {
                        let mut peaks = Vec::new();
                        wf_cache::build_visible_minmax(&samples, bins.max(1), &mut peaks);
                        out.push(EditorViewportWaveLane::Peaks(peaks));
                    }
                }
                WaveLaneRequest::MixdownSamples {
                    channels,
                    bins,
                    render_raw,
                } => {
                    if render_raw {
                        let mut mono = Vec::new();
                        wf_cache::build_mixdown_visible(
                            &channels,
                            0,
                            channels.first().map(|c| c.len()).unwrap_or(0),
                            &mut mono,
                        );
                        out.push(EditorViewportWaveLane::Samples(mono));
                    } else {
                        let mut peaks = Vec::new();
                        let len = channels.first().map(|c| c.len()).unwrap_or(0);
                        wf_cache::build_mixdown_minmax_visible(
                            &channels,
                            0,
                            len,
                            bins.max(1),
                            &mut peaks,
                        );
                        out.push(EditorViewportWaveLane::Peaks(peaks));
                    }
                }
                WaveLaneRequest::Pyramid {
                    pyramid,
                    start,
                    end,
                    bins,
                    spp,
                } => {
                    let mut peaks = Vec::new();
                    pyramid.query_columns(start, end, bins.max(1), spp, &mut peaks);
                    out.push(EditorViewportWaveLane::Peaks(peaks));
                }
            }
        }
        EditorViewportWavePayload { lanes: out }
    }

    pub(crate) fn render_spectral_viewport_image(
        specs: &[SpectrogramData],
        lane_spec_indices: &[usize],
        wave_width_px: usize,
        lane_height_px: usize,
        lane_count: usize,
        start: usize,
        end: usize,
        vertical_zoom: f32,
        vertical_view_center: f32,
        cfg: &SpectrogramConfig,
        view_mode: ViewMode,
        quality: EditorViewportRenderQuality,
    ) -> ColorImage {
        let target_w = match quality {
            EditorViewportRenderQuality::Coarse => (wave_width_px / 4).clamp(48, 160),
            EditorViewportRenderQuality::Fine => (wave_width_px / 2).clamp(96, 384),
        };
        let target_lane_h = match quality {
            EditorViewportRenderQuality::Coarse => (lane_height_px / 4).clamp(32, 96),
            EditorViewportRenderQuality::Fine => (lane_height_px / 2).clamp(64, 192),
        };
        let total_h = target_lane_h.saturating_mul(lane_count.max(1)).max(1);
        let mut image =
            ColorImage::filled([target_w.max(1), total_h], Color32::from_rgb(12, 14, 18));
        let (visible_min, visible_max) = Self::editor_vertical_range_for_view(
            view_mode,
            vertical_zoom,
            vertical_view_center,
            cfg,
        );
        for lane_idx in 0..lane_count.max(1) {
            let spec = lane_spec_indices
                .get(lane_idx)
                .and_then(|&idx| specs.get(idx))
                .or_else(|| specs.first());
            let Some(spec) = spec else {
                continue;
            };
            if spec.frames == 0 || spec.bins == 0 || spec.values_db.is_empty() {
                continue;
            }
            let frame_step = spec.frame_step.max(1);
            let f0 = (start / frame_step).min(spec.frames.saturating_sub(1));
            let mut f1 = (end / frame_step).min(spec.frames);
            if f1 <= f0 {
                f1 = (f0 + 1).min(spec.frames);
            }
            let frame_count = f1.saturating_sub(f0).max(1);
            let max_bin = spec.bins.saturating_sub(1).max(1);
            let sr = spec.sample_rate.max(1) as f32;
            let mut max_freq = sr * 0.5;
            if cfg.max_freq_hz > 0.0 {
                max_freq = cfg.max_freq_hz.min(max_freq).max(1.0);
            }
            let log_min = 20.0_f32.min(max_freq).max(1.0);
            let mel_max = 2595.0 * (1.0 + max_freq / 700.0).log10();
            let mel_min = 1.0_f32;
            let lane_y0 = lane_idx.saturating_mul(target_lane_h);
            for x in 0..target_w.max(1) {
                let frame_idx = f0 + ((x * frame_count) / target_w.max(1)).min(frame_count - 1);
                let base = frame_idx * spec.bins;
                for y in 0..target_lane_h.max(1) {
                    let frac_local =
                        1.0 - (y as f32 / target_lane_h.saturating_sub(1).max(1) as f32);
                    let frac = visible_min + frac_local * (visible_max - visible_min);
                    let bin = match view_mode {
                        ViewMode::Spectrogram => {
                            (frac.clamp(0.0, 1.0) * max_bin as f32).round() as usize
                        }
                        ViewMode::Log => {
                            let freq = if max_freq <= log_min {
                                frac * max_freq
                            } else {
                                let ratio = max_freq / log_min;
                                log_min * ratio.powf(frac.clamp(0.0, 1.0))
                            };
                            ((freq / max_freq).clamp(0.0, 1.0) * max_bin as f32).round() as usize
                        }
                        ViewMode::Mel => {
                            let mel = match cfg.mel_scale {
                                SpectrogramScale::Linear => mel_max * frac.clamp(0.0, 1.0),
                                SpectrogramScale::Log => {
                                    if mel_max <= mel_min {
                                        mel_max * frac.clamp(0.0, 1.0)
                                    } else {
                                        let ratio = mel_max / mel_min;
                                        mel_min * ratio.powf(frac.clamp(0.0, 1.0))
                                    }
                                }
                            };
                            let freq = 700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0);
                            ((freq / max_freq).clamp(0.0, 1.0) * max_bin as f32).round() as usize
                        }
                        _ => 0,
                    };
                    let idx = base + bin.min(max_bin);
                    let db_raw = spec
                        .values_db
                        .get(idx)
                        .copied()
                        .unwrap_or(-120.0)
                        .clamp(cfg.db_floor, 0.0);
                    let norm = if (0.0 - cfg.db_floor).abs() < f32::EPSILON {
                        0.0
                    } else {
                        (db_raw - cfg.db_floor) / (0.0 - cfg.db_floor)
                    };
                    let image_y = lane_y0 + y.min(target_lane_h.saturating_sub(1));
                    let pixel_idx = image_y * target_w + x;
                    if let Some(pixel) = image.pixels.get_mut(pixel_idx) {
                        *pixel = db_to_color(-80.0 + norm.clamp(0.0, 1.0) * 80.0);
                    }
                }
            }
        }
        image
    }

    pub(crate) fn editor_spectro_cfg_digest(cfg: &SpectrogramConfig) -> u64 {
        let mut hasher = DefaultHasher::new();
        cfg.fft_size.hash(&mut hasher);
        cfg.hop_size.hash(&mut hasher);
        cfg.max_frames.hash(&mut hasher);
        cfg.db_floor.to_bits().hash(&mut hasher);
        cfg.max_freq_hz.to_bits().hash(&mut hasher);
        cfg.show_note_labels.hash(&mut hasher);
        match cfg.window {
            super::types::WindowFunction::Hann => 1u8.hash(&mut hasher),
            super::types::WindowFunction::BlackmanHarris => 2u8.hash(&mut hasher),
        }
        match cfg.scale {
            SpectrogramScale::Linear => 1u8.hash(&mut hasher),
            SpectrogramScale::Log => 2u8.hash(&mut hasher),
        }
        match cfg.mel_scale {
            SpectrogramScale::Linear => 3u8.hash(&mut hasher),
            SpectrogramScale::Log => 4u8.hash(&mut hasher),
        }
        hasher.finish()
    }

    #[allow(dead_code)]
    pub(super) fn paint_waveform_payload_lane(
        painter: &egui::Painter,
        lane_rect: egui::Rect,
        wave_width: f32,
        scale: f32,
        vertical_zoom: f32,
        vertical_view_center: f32,
        lane: &EditorViewportWaveLane,
    ) {
        match lane {
            EditorViewportWaveLane::Peaks(peaks) => {
                let mut shapes = Vec::new();
                super::WavesPreviewer::push_peak_shapes(
                    &mut shapes,
                    peaks,
                    lane_rect,
                    wave_width,
                    scale,
                    vertical_zoom,
                    vertical_view_center,
                );
                if !shapes.is_empty() {
                    painter.extend(shapes);
                }
            }
            EditorViewportWaveLane::Samples(samples) => {
                if samples.is_empty() {
                    return;
                }
                let base_y = super::WavesPreviewer::waveform_center_y(
                    lane_rect,
                    vertical_zoom,
                    vertical_view_center,
                );
                let denom = (samples.len() - 1).max(1) as f32;
                let mut points = Vec::with_capacity(samples.len());
                for (idx, &sample) in samples.iter().enumerate() {
                    let v = (sample * scale).clamp(-1.0, 1.0);
                    let t = idx as f32 / denom;
                    let sx = lane_rect.left() + t * wave_width;
                    let sy = super::WavesPreviewer::waveform_y_from_amp(
                        lane_rect,
                        vertical_zoom,
                        vertical_view_center,
                        v,
                    );
                    points.push((egui::pos2(sx, sy), v));
                }
                for idx in 1..points.len() {
                    let col = lerp_color(
                        Color32::from_rgb(80, 200, 255),
                        Color32::from_rgb(255, 70, 70),
                        points[idx - 1].1.abs().clamp(0.0, 1.0).powf(0.6),
                    );
                    painter.line_segment(
                        [points[idx - 1].0, points[idx].0],
                        egui::Stroke::new(1.0, col),
                    );
                }
                if samples.len() <= wave_width as usize {
                    for (point, value) in points {
                        let col = lerp_color(
                            Color32::from_rgb(80, 200, 255),
                            Color32::from_rgb(255, 70, 70),
                            value.abs().clamp(0.0, 1.0).powf(0.6),
                        );
                        painter.line_segment(
                            [egui::pos2(point.x, base_y), point],
                            egui::Stroke::new(1.0, col),
                        );
                    }
                }
            }
        }
    }
}
