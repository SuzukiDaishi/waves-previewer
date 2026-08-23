use std::path::PathBuf;

use super::*;

const EDITOR_PREVIEW_PREFIX_SECS_COMPRESSED: f32 = 0.8;
const EDITOR_PROGRESSIVE_EMIT_SECS_COMPRESSED: f32 = 0.75;
const EDITOR_STREAMING_PROGRESS_EMIT_SECS: f32 = 0.25;

impl super::WavesPreviewer {
    pub(super) fn spawn_editor_decode(&mut self, path: PathBuf) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc};
        self.cancel_editor_decode();
        let target_sr = self
            .sample_rate_override
            .get(&path)
            .copied()
            .filter(|v| *v > 0);
        let decode_path = self
            .item_for_path(&path)
            .and_then(|item| item.audio_asset.backing.file_path().map(PathBuf::from))
            .unwrap_or_else(|| path.clone());
        let source_sr_hint = self
            .meta_for_path(&path)
            .map(|meta| meta.sample_rate)
            .filter(|v| *v > 0);
        let source_sr_hint = source_sr_hint.or_else(|| {
            self.item_for_path(&path)
                .map(|item| item.audio_asset.sample_rate)
                .filter(|value| *value > 0)
        });
        let preferred_out_sr = target_sr.or(source_sr_hint);
        let _ = self.ensure_output_sample_rate(preferred_out_sr);

        self.editor_decode_job_id = self.editor_decode_job_id.wrapping_add(1);
        let job_id = self.editor_decode_job_id;
        let out_sr = self.audio.shared.out_sample_rate;
        let resample_quality = Self::to_wave_resample_quality(self.src_quality);
        let bit_depth = self.bit_depth_override.get(&path).copied();
        let estimated_total_frames = self.estimate_editor_total_frames_cached(&path, out_sr);
        let total_source_frames_hint = self.estimate_editor_total_source_frames_cached(&path);
        let paged_asset = self
            .item_for_path(&path)
            .map(|item| !item.audio_asset.may_reside_in_memory())
            .unwrap_or(false);
        let strategy = Self::editor_decode_strategy(&decode_path);
        self.debug_log(format!(
        "editor decode spawn: {} strategy={} out_sr={} preferred_out_sr={:?} target_sr={:?} bits={:?} est_frames={:?}",
        path.display(),
        match strategy {
            EditorDecodeStrategy::CompressedProgressiveFull => "compressed-progressive-full",
            EditorDecodeStrategy::StreamingOverviewFinalAudio => "streaming-overview-final-audio",
        },
        out_sr,
        preferred_out_sr,
        target_sr,
        bit_depth,
        estimated_total_frames
    ));
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_thread = cancel.clone();
        let path_for_thread = path.clone();
        let decode_path_for_thread = decode_path;
        let (tx, rx) = mpsc::channel::<EditorDecodeResult>();
        std::thread::spawn(move || match strategy {
            EditorDecodeStrategy::CompressedProgressiveFull => {
                let mut partial_emitted = false;
                let progressive = crate::audio_io::decode_audio_multi_progressive(
                    &decode_path_for_thread,
                    EDITOR_PREVIEW_PREFIX_SECS_COMPRESSED,
                    EDITOR_PROGRESSIVE_EMIT_SECS_COMPRESSED,
                    || cancel_thread.load(Ordering::Relaxed),
                    |chans, in_sr, is_final| {
                        if cancel_thread.load(Ordering::Relaxed) {
                            return false;
                        }
                        let decoded_source_frames = chans.first().map(|c| c.len()).unwrap_or(0);
                        let visual_total_frames = estimated_total_frames.or_else(|| {
                            Some(Self::convert_source_frames_to_output_frames(
                                decoded_source_frames,
                                in_sr.max(1),
                                out_sr.max(1),
                            ))
                        });
                        if is_final {
                            let chans = Self::process_editor_decode_channels(
                                chans,
                                in_sr,
                                out_sr,
                                target_sr,
                                bit_depth,
                                resample_quality,
                            );
                            let decoded_frames = chans.first().map(|c| c.len()).unwrap_or(0);
                            let (waveform_minmax, waveform_pyramid) =
                                Self::build_editor_waveform_cache(&chans, decoded_frames);
                            let chans_arc = std::sync::Arc::new(chans.clone());
                            return tx
                                .send(EditorDecodeResult {
                                    path: path_for_thread.clone(),
                                    event: EditorDecodeEvent::FinalReady,
                                    channels: chans,
                                    channels_arc: Some(chans_arc),
                                    waveform_minmax,
                                    waveform_pyramid,
                                    loading_waveform_minmax: Vec::new(),
                                    buffer_sample_rate: out_sr.max(1),
                                    job_id,
                                    error: None,
                                    stage: EditorDecodeStage::FinalizingWaveform,
                                    decoded_frames,
                                    decoded_source_frames,
                                    total_source_frames: Some(decoded_source_frames),
                                    visual_total_frames,
                                    progress_emit_gap_ms: None,
                                    finalize_audio_ms: None,
                                    finalize_waveform_ms: None,
                                })
                                .is_ok();
                        }
                        let sent = tx
                            .send(EditorDecodeResult {
                                path: path_for_thread.clone(),
                                event: EditorDecodeEvent::Progress,
                                channels: Vec::new(),
                                channels_arc: None,
                                waveform_minmax: Vec::new(),
                                waveform_pyramid: None,
                                loading_waveform_minmax: Self::build_loading_overview_from_channels(
                                    &chans,
                                ),
                                buffer_sample_rate: out_sr.max(1),
                                job_id,
                                error: None,
                                stage: if partial_emitted {
                                    EditorDecodeStage::StreamingFull
                                } else {
                                    EditorDecodeStage::Preview
                                },
                                decoded_frames: Self::convert_source_frames_to_output_frames(
                                    decoded_source_frames,
                                    in_sr.max(1),
                                    out_sr.max(1),
                                ),
                                decoded_source_frames,
                                total_source_frames: None,
                                visual_total_frames,
                                progress_emit_gap_ms: None,
                                finalize_audio_ms: None,
                                finalize_waveform_ms: None,
                            })
                            .is_ok();
                        partial_emitted = true;
                        sent
                    },
                );
                if let Err(err) = progressive {
                    if !cancel_thread.load(Ordering::Relaxed) {
                        let _ = tx.send(EditorDecodeResult {
                            path: path_for_thread,
                            event: EditorDecodeEvent::Failed,
                            channels: Vec::new(),
                            channels_arc: None,
                            waveform_minmax: Vec::new(),
                            waveform_pyramid: None,
                            loading_waveform_minmax: Vec::new(),
                            buffer_sample_rate: out_sr.max(1),
                            job_id,
                            error: Some(err.to_string()),
                            stage: EditorDecodeStage::StreamingFull,
                            decoded_frames: 0,
                            decoded_source_frames: 0,
                            total_source_frames: None,
                            visual_total_frames: estimated_total_frames,
                            progress_emit_gap_ms: None,
                            finalize_audio_ms: None,
                            finalize_waveform_ms: None,
                        });
                    }
                }
            }
            EditorDecodeStrategy::StreamingOverviewFinalAudio => {
                let mut source_sr = source_sr_hint.unwrap_or(0).max(1);
                let mut total_source_frames = total_source_frames_hint.filter(|v| *v > 0);
                if total_source_frames.is_none() {
                    if let (Some(estimated), Some(in_sr)) =
                        (estimated_total_frames.filter(|v| *v > 0), source_sr_hint)
                    {
                        let out_sr_u128 = out_sr.max(1) as u128;
                        let in_sr_u128 = in_sr.max(1) as u128;
                        total_source_frames = Some(
                            (((estimated as u128)
                                .saturating_mul(in_sr_u128)
                                .saturating_add(out_sr_u128 / 2))
                                / out_sr_u128) as usize,
                        )
                        .filter(|v| *v > 0);
                    }
                }
                let mut overview = total_source_frames.map(|frames| {
                    crate::app::render::waveform_pyramid::StreamingWaveformOverview::new(
                        frames.max(1),
                        crate::app::render::waveform_pyramid::DEFAULT_LOADING_OVERVIEW_BINS,
                    )
                });
                let mut full_source_channels: Vec<Vec<f32>> = Vec::new();
                let mut last_progress_emit_at: Option<std::time::Instant> = None;
                if let Ok(Some(overview_proxy)) = crate::audio_io::build_wav_proxy_preview(
                    &decode_path_for_thread,
                    crate::audio_io::EDITOR_PROXY_OVERVIEW_MAX_TOTAL_SAMPLES,
                ) {
                    source_sr = source_sr.max(overview_proxy.source_sample_rate.max(1));
                    total_source_frames = Some(overview_proxy.total_source_frames);
                    let visual_total_frames = Some(Self::convert_source_frames_to_output_frames(
                        overview_proxy.total_source_frames,
                        source_sr,
                        out_sr.max(1),
                    ));
                    if overview.is_none() {
                        overview = Some(
                            crate::app::render::waveform_pyramid::StreamingWaveformOverview::new(
                                overview_proxy.total_source_frames.max(1),
                                crate::app::render::waveform_pyramid::DEFAULT_LOADING_OVERVIEW_BINS,
                            ),
                        );
                    }
                    let loading_waveform_minmax =
                        Self::build_loading_overview_from_channels(&overview_proxy.channels);
                    if let Some(builder) = overview.as_mut() {
                        builder.seed_from_minmax(&loading_waveform_minmax);
                    }
                    last_progress_emit_at = Some(std::time::Instant::now());
                    if tx
                        .send(EditorDecodeResult {
                            path: path_for_thread.clone(),
                            event: EditorDecodeEvent::Progress,
                            channels: Vec::new(),
                            channels_arc: None,
                            waveform_minmax: Vec::new(),
                            waveform_pyramid: None,
                            loading_waveform_minmax: loading_waveform_minmax.clone(),
                            buffer_sample_rate: out_sr.max(1),
                            job_id,
                            error: None,
                            stage: EditorDecodeStage::Preview,
                            decoded_frames: 0,
                            decoded_source_frames: 0,
                            total_source_frames,
                            visual_total_frames,
                            progress_emit_gap_ms: None,
                            finalize_audio_ms: None,
                            finalize_waveform_ms: None,
                        })
                        .is_err()
                    {
                        return;
                    }
                    if paged_asset {
                        let _ = tx.send(EditorDecodeResult {
                            path: path_for_thread,
                            event: EditorDecodeEvent::PagedReady,
                            channels: Vec::new(),
                            channels_arc: None,
                            waveform_minmax: loading_waveform_minmax.clone(),
                            waveform_pyramid: None,
                            loading_waveform_minmax,
                            buffer_sample_rate: out_sr.max(1),
                            job_id,
                            error: None,
                            stage: EditorDecodeStage::Preview,
                            decoded_frames: visual_total_frames.unwrap_or(0),
                            decoded_source_frames: 0,
                            total_source_frames,
                            visual_total_frames,
                            progress_emit_gap_ms: None,
                            finalize_audio_ms: None,
                            finalize_waveform_ms: None,
                        });
                        return;
                    }
                }
                let stream = crate::audio_io::decode_audio_multi_streaming_chunks(
                    &decode_path_for_thread,
                    EDITOR_STREAMING_PROGRESS_EMIT_SECS,
                    || cancel_thread.load(Ordering::Relaxed),
                    |chunk, in_sr, decoded_source_frames, is_final| {
                        if cancel_thread.load(Ordering::Relaxed) {
                            return false;
                        }
                        source_sr = in_sr.max(1);
                        if overview.is_none() {
                            overview = Some(
                                crate::app::render::waveform_pyramid::StreamingWaveformOverview::new(
                                    total_source_frames
                                        .unwrap_or(decoded_source_frames.max(1))
                                        .max(1),
                                    crate::app::render::waveform_pyramid::DEFAULT_LOADING_OVERVIEW_BINS,
                                ),
                            );
                        }
                        if full_source_channels.is_empty() {
                            full_source_channels = vec![Vec::new(); chunk.len().max(1)];
                            if let Some(total) = total_source_frames {
                                for ch in &mut full_source_channels {
                                    let _ = ch.try_reserve(total);
                                }
                            }
                        }
                        if full_source_channels.len() != chunk.len() {
                            full_source_channels.resize_with(chunk.len(), Vec::new);
                        }
                        let start_frame_source =
                            full_source_channels.first().map(|c| c.len()).unwrap_or(0);
                        if let Some(builder) = overview.as_mut() {
                            builder.append_mixdown_chunk(start_frame_source, &chunk);
                        }
                        for (dst, src) in full_source_channels.iter_mut().zip(chunk.iter()) {
                            dst.extend_from_slice(src);
                        }
                        let visual_total_frames = total_source_frames.map(|frames| {
                            Self::convert_source_frames_to_output_frames(
                                frames,
                                source_sr,
                                out_sr.max(1),
                            )
                        });
                        let loading_waveform_minmax = overview
                            .as_ref()
                            .map(|builder| builder.snapshot_minmax())
                            .unwrap_or_default();
                        if !is_final {
                            let now = std::time::Instant::now();
                            let gap_ms = last_progress_emit_at
                                .map(|prev| now.duration_since(prev).as_secs_f32() * 1000.0);
                            last_progress_emit_at = Some(now);
                            return tx
                                .send(EditorDecodeResult {
                                    path: path_for_thread.clone(),
                                    event: EditorDecodeEvent::Progress,
                                    channels: Vec::new(),
                                    channels_arc: None,
                                    waveform_minmax: Vec::new(),
                                    waveform_pyramid: None,
                                    loading_waveform_minmax,
                                    buffer_sample_rate: out_sr.max(1),
                                    job_id,
                                    error: None,
                                    stage: if gap_ms.is_some() {
                                        EditorDecodeStage::StreamingFull
                                    } else {
                                        EditorDecodeStage::Preview
                                    },
                                    decoded_frames: Self::convert_source_frames_to_output_frames(
                                        decoded_source_frames,
                                        source_sr,
                                        out_sr.max(1),
                                    ),
                                    decoded_source_frames,
                                    total_source_frames,
                                    visual_total_frames,
                                    progress_emit_gap_ms: gap_ms,
                                    finalize_audio_ms: None,
                                    finalize_waveform_ms: None,
                                })
                                .is_ok();
                        }
                        true
                    },
                );
                if let Err(err) = stream {
                    if !cancel_thread.load(Ordering::Relaxed) {
                        let _ = tx.send(EditorDecodeResult {
                            path: path_for_thread,
                            event: EditorDecodeEvent::Failed,
                            channels: Vec::new(),
                            channels_arc: None,
                            waveform_minmax: Vec::new(),
                            waveform_pyramid: None,
                            loading_waveform_minmax: Vec::new(),
                            buffer_sample_rate: out_sr.max(1),
                            job_id,
                            error: Some(err.to_string()),
                            stage: EditorDecodeStage::StreamingFull,
                            decoded_frames: 0,
                            decoded_source_frames: 0,
                            total_source_frames,
                            visual_total_frames: estimated_total_frames,
                            progress_emit_gap_ms: None,
                            finalize_audio_ms: None,
                            finalize_waveform_ms: None,
                        });
                    }
                    return;
                }
                if cancel_thread.load(Ordering::Relaxed) {
                    return;
                }
                let loading_waveform_minmax = overview
                    .as_ref()
                    .map(|builder| builder.snapshot_minmax())
                    .unwrap_or_default();
                let decoded_source_frames =
                    full_source_channels.first().map(|c| c.len()).unwrap_or(0);
                let visual_total_frames = total_source_frames.map(|frames| {
                    Self::convert_source_frames_to_output_frames(frames, source_sr, out_sr.max(1))
                });
                let _ = tx.send(EditorDecodeResult {
                    path: path_for_thread.clone(),
                    event: EditorDecodeEvent::Progress,
                    channels: Vec::new(),
                    channels_arc: None,
                    waveform_minmax: Vec::new(),
                    waveform_pyramid: None,
                    loading_waveform_minmax: loading_waveform_minmax.clone(),
                    buffer_sample_rate: out_sr.max(1),
                    job_id,
                    error: None,
                    stage: EditorDecodeStage::FinalizingAudio,
                    decoded_frames: Self::convert_source_frames_to_output_frames(
                        decoded_source_frames,
                        source_sr,
                        out_sr.max(1),
                    ),
                    decoded_source_frames,
                    total_source_frames,
                    visual_total_frames,
                    progress_emit_gap_ms: last_progress_emit_at
                        .map(|prev| prev.elapsed().as_secs_f32() * 1000.0),
                    finalize_audio_ms: None,
                    finalize_waveform_ms: None,
                });
                let finalize_audio_started = std::time::Instant::now();
                let channels = Self::process_editor_decode_channels(
                    full_source_channels,
                    source_sr,
                    out_sr,
                    target_sr,
                    bit_depth,
                    resample_quality,
                );
                let finalize_audio_ms = finalize_audio_started.elapsed().as_secs_f32() * 1000.0;
                if cancel_thread.load(Ordering::Relaxed) {
                    return;
                }
                let decoded_frames = channels.first().map(|c| c.len()).unwrap_or(0);
                let _ = tx.send(EditorDecodeResult {
                    path: path_for_thread.clone(),
                    event: EditorDecodeEvent::Progress,
                    channels: Vec::new(),
                    channels_arc: None,
                    waveform_minmax: Vec::new(),
                    waveform_pyramid: None,
                    loading_waveform_minmax,
                    buffer_sample_rate: out_sr.max(1),
                    job_id,
                    error: None,
                    stage: EditorDecodeStage::FinalizingWaveform,
                    decoded_frames,
                    decoded_source_frames,
                    total_source_frames,
                    visual_total_frames,
                    progress_emit_gap_ms: None,
                    finalize_audio_ms: Some(finalize_audio_ms),
                    finalize_waveform_ms: None,
                });
                let finalize_waveform_started = std::time::Instant::now();
                let (waveform_minmax, waveform_pyramid) =
                    Self::build_editor_waveform_cache(&channels, decoded_frames);
                let finalize_waveform_ms =
                    finalize_waveform_started.elapsed().as_secs_f32() * 1000.0;
                // Clone here (worker thread) so the UI thread can adopt both the
                // owned buffer and the shared Arc without a deep copy.
                let channels_arc = std::sync::Arc::new(channels.clone());
                let _ = tx.send(EditorDecodeResult {
                    path: path_for_thread,
                    event: EditorDecodeEvent::FinalReady,
                    channels,
                    channels_arc: Some(channels_arc),
                    waveform_minmax,
                    waveform_pyramid,
                    loading_waveform_minmax: Vec::new(),
                    buffer_sample_rate: out_sr.max(1),
                    job_id,
                    error: None,
                    stage: EditorDecodeStage::FinalizingWaveform,
                    decoded_frames,
                    decoded_source_frames,
                    total_source_frames,
                    visual_total_frames,
                    progress_emit_gap_ms: None,
                    finalize_audio_ms: Some(finalize_audio_ms),
                    finalize_waveform_ms: Some(finalize_waveform_ms),
                });
            }
        });
        self.editor_decode_state = Some(EditorDecodeState {
            path,
            started_at: std::time::Instant::now(),
            rx,
            _keepalive_tx: None,
            cancel,
            job_id,
            partial_ready: false,
            stage: EditorDecodeStage::Preview,
            decoded_frames: 0,
            estimated_total_frames,
            total_source_frames: total_source_frames_hint,
            visual_total_frames: estimated_total_frames,
            decoded_source_frames: 0,
            loading_waveform_updates: 0,
            max_progress_gap_ms: 0.0,
        });
    }

    pub(super) fn spawn_editor_decode_from_audio_buffer(
        &mut self,
        path: PathBuf,
        audio: std::sync::Arc<crate::audio::AudioBuffer>,
        input_sr: u32,
    ) {
        use std::sync::atomic::AtomicBool;
        use std::sync::{mpsc, Arc};
        self.cancel_editor_decode();
        let target_sr = self
            .sample_rate_override
            .get(&path)
            .copied()
            .filter(|v| *v > 0);
        let preferred_out_sr = target_sr.or(Some(input_sr.max(1)));
        let _ = self.ensure_output_sample_rate(preferred_out_sr);
        self.editor_decode_job_id = self.editor_decode_job_id.wrapping_add(1);
        let job_id = self.editor_decode_job_id;
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let resample_quality = Self::to_wave_resample_quality(self.src_quality);
        let bit_depth = self.bit_depth_override.get(&path).copied();
        let estimated_total_frames = Some(Self::convert_source_frames_to_output_frames(
            audio.len(),
            input_sr.max(1),
            out_sr,
        ));
        let (tx, rx) = mpsc::channel::<EditorDecodeResult>();
        let path_for_thread = path.clone();
        std::thread::spawn(move || {
            let channels = Self::process_editor_decode_channels(
                (*audio.channels).clone(),
                input_sr.max(1),
                out_sr,
                target_sr,
                bit_depth,
                resample_quality,
            );
            let decoded_frames = channels.first().map(|c| c.len()).unwrap_or(0);
            let (waveform_minmax, waveform_pyramid) =
                Self::build_editor_waveform_cache(&channels, decoded_frames);
            let channels_arc = std::sync::Arc::new(channels.clone());
            let _ = tx.send(EditorDecodeResult {
                path: path_for_thread,
                event: EditorDecodeEvent::FinalReady,
                channels,
                channels_arc: Some(channels_arc),
                waveform_minmax,
                waveform_pyramid,
                loading_waveform_minmax: Vec::new(),
                buffer_sample_rate: out_sr,
                job_id,
                error: None,
                stage: EditorDecodeStage::FinalizingWaveform,
                decoded_frames,
                decoded_source_frames: decoded_frames,
                total_source_frames: Some(decoded_frames),
                visual_total_frames: Some(decoded_frames),
                progress_emit_gap_ms: None,
                finalize_audio_ms: None,
                finalize_waveform_ms: None,
            });
        });
        self.editor_decode_state = Some(EditorDecodeState {
            path,
            started_at: std::time::Instant::now(),
            rx,
            _keepalive_tx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            job_id,
            partial_ready: false,
            stage: EditorDecodeStage::FinalizingAudio,
            decoded_frames: 0,
            estimated_total_frames,
            total_source_frames: estimated_total_frames,
            visual_total_frames: estimated_total_frames,
            decoded_source_frames: 0,
            loading_waveform_updates: 0,
            max_progress_gap_ms: 0.0,
        });
    }

    pub(super) fn spawn_editor_decode_from_ready_channels(
        &mut self,
        path: PathBuf,
        channels: Vec<Vec<f32>>,
        buffer_sample_rate: u32,
    ) {
        use std::sync::atomic::AtomicBool;
        use std::sync::{mpsc, Arc};
        self.cancel_editor_decode();
        let buffer_sample_rate = buffer_sample_rate.max(1);
        let _ = self.ensure_output_sample_rate(Some(buffer_sample_rate));
        self.editor_decode_job_id = self.editor_decode_job_id.wrapping_add(1);
        let job_id = self.editor_decode_job_id;
        let estimated_total_frames = channels.first().map(|c| c.len());
        let (tx, rx) = mpsc::channel::<EditorDecodeResult>();
        let path_for_thread = path.clone();
        std::thread::spawn(move || {
            let decoded_frames = channels.first().map(|c| c.len()).unwrap_or(0);
            let (waveform_minmax, waveform_pyramid) =
                Self::build_editor_waveform_cache(&channels, decoded_frames);
            let channels_arc = std::sync::Arc::new(channels.clone());
            let _ = tx.send(EditorDecodeResult {
                path: path_for_thread,
                event: EditorDecodeEvent::FinalReady,
                channels,
                channels_arc: Some(channels_arc),
                waveform_minmax,
                waveform_pyramid,
                loading_waveform_minmax: Vec::new(),
                buffer_sample_rate,
                job_id,
                error: None,
                stage: EditorDecodeStage::FinalizingWaveform,
                decoded_frames,
                decoded_source_frames: decoded_frames,
                total_source_frames: Some(decoded_frames),
                visual_total_frames: Some(decoded_frames),
                progress_emit_gap_ms: None,
                finalize_audio_ms: None,
                finalize_waveform_ms: None,
            });
        });
        self.editor_decode_state = Some(EditorDecodeState {
            path,
            started_at: std::time::Instant::now(),
            rx,
            _keepalive_tx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            job_id,
            partial_ready: false,
            stage: EditorDecodeStage::FinalizingWaveform,
            decoded_frames: 0,
            estimated_total_frames,
            total_source_frames: estimated_total_frames,
            visual_total_frames: estimated_total_frames,
            decoded_source_frames: 0,
            loading_waveform_updates: 0,
            max_progress_gap_ms: 0.0,
        });
    }

    pub(super) fn drain_editor_decode(&mut self) {
        fn remap_view_for_display_len(
            tab: &mut EditorTab,
            old_display_len: usize,
            old_view: usize,
            old_spp: f32,
            new_display_len: usize,
        ) {
            if new_display_len == 0 {
                tab.samples_per_px = 0.0;
                tab.view_offset = 0;
                tab.view_offset_exact = 0.0;
                return;
            }
            if old_display_len > 0 && new_display_len != old_display_len {
                let ratio = new_display_len as f32 / old_display_len as f32;
                if old_spp > 0.0 {
                    tab.samples_per_px =
                        (old_spp * ratio).max(crate::app::EDITOR_MIN_SAMPLES_PER_PX);
                } else {
                    tab.samples_per_px = 0.0;
                }
                tab.view_offset = ((old_view as f32) * ratio).round() as usize;
                tab.view_offset_exact = tab.view_offset as f64;
                tab.loop_xfade_samples = ((tab.loop_xfade_samples as f32) * ratio).round() as usize;
            } else if old_spp <= 0.0 {
                tab.samples_per_px = 0.0;
            }
        }

        let clear_edit_path = self.editor_clear_edit_pending.clone();
        let mut decode_update_tab: Option<(usize, bool)> = None;
        let mut clear_edit_completed: Option<(usize, PathBuf)> = None;
        let mut decode_refresh_preview: Option<usize> = None;
        let mut decode_cancel_preview = false;
        let mut decode_error: Option<(PathBuf, String)> = None;
        let mut decode_done = false;
        let mut marker_updates: Vec<(usize, PathBuf)> = Vec::new();
        let mut spectro_reset_paths: Vec<PathBuf> = Vec::new();
        let mut decode_partial_events: Vec<(PathBuf, usize, EditorDecodeStage)> = Vec::new();
        let mut decode_final_events: Vec<(PathBuf, usize)> = Vec::new();
        let mut decode_progress_gap_ms: Vec<f32> = Vec::new();
        let mut decode_finalize_audio_ms: Vec<f32> = Vec::new();
        let mut decode_finalize_waveform_ms: Vec<f32> = Vec::new();
        let mut decode_done_loading_stats: Option<(u64, f32)> = None;
        let mut paged_ready_tab: Option<usize> = None;
        let mut decode_worker_disconnected: Option<PathBuf> = None;
        if let Some(state) = &mut self.editor_decode_state {
            loop {
                let res = match state.rx.try_recv() {
                    Ok(res) => res,
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        if !decode_done {
                            decode_worker_disconnected = Some(state.path.clone());
                        }
                        break;
                    }
                };
                if res.job_id != state.job_id {
                    continue;
                }
                state.stage = res.stage;
                state.decoded_frames = res.decoded_frames;
                state.decoded_source_frames = res.decoded_source_frames;
                state.total_source_frames = res.total_source_frames.or(state.total_source_frames);
                state.visual_total_frames = res.visual_total_frames.or(state.visual_total_frames);
                if let Some(gap_ms) = res.progress_emit_gap_ms {
                    state.max_progress_gap_ms = state.max_progress_gap_ms.max(gap_ms);
                    decode_progress_gap_ms.push(gap_ms);
                }
                if let Some(value_ms) = res.finalize_audio_ms {
                    decode_finalize_audio_ms.push(value_ms);
                }
                if let Some(value_ms) = res.finalize_waveform_ms {
                    decode_finalize_waveform_ms.push(value_ms);
                }
                if !res.loading_waveform_minmax.is_empty() {
                    state.loading_waveform_updates =
                        state.loading_waveform_updates.saturating_add(1);
                }
                if matches!(res.event, EditorDecodeEvent::Failed) || res.error.is_some() {
                    let err = res.error.unwrap_or_else(|| "decode failed".to_string());
                    decode_error = Some((res.path.clone(), err));
                    if let Some(idx) = self.tabs.iter().position(|t| t.path == res.path) {
                        if let Some(tab) = self.tabs.get_mut(idx) {
                            tab.loading = false;
                            tab.loading_waveform_minmax.clear();
                            tab.samples_len_visual = tab.samples_len;
                        }
                    }
                    decode_done = true;
                    decode_done_loading_stats =
                        Some((state.loading_waveform_updates, state.max_progress_gap_ms));
                    continue;
                }
                if let Some(idx) = self.tabs.iter().position(|t| t.path == res.path) {
                    if let Some(tab) = self.tabs.get_mut(idx) {
                        let old_display_len = if tab.loading && tab.samples_len_visual > 0 {
                            tab.samples_len_visual
                        } else {
                            tab.samples_len
                        };
                        let old_view = tab.view_offset;
                        let old_spp = tab.samples_per_px;
                        let had_preview =
                            tab.preview_audio_tool.is_some() || tab.preview_overlay.is_some();
                        match res.event {
                            EditorDecodeEvent::FinalReady => {
                                let is_clear_edit = clear_edit_path.as_ref() == Some(&res.path);
                                tab.paged_asset = false;
                                tab.preview_audio_tool = None;
                                tab.preview_overlay = None;
                                let old_audio_len = tab.samples_len;
                                tab.ch_samples = res.channels;
                                // Prefer the worker-built Arc; cloning the full
                                // buffer here would stall the UI thread.
                                tab.ch_samples_arc = match res.channels_arc {
                                    Some(arc) => arc,
                                    None => std::sync::Arc::new(tab.ch_samples.clone()),
                                };
                                tab.buffer_sample_rate = res.buffer_sample_rate.max(1);
                                tab.samples_len =
                                    tab.ch_samples.first().map(|c| c.len()).unwrap_or(0);
                                tab.waveform_minmax = res.waveform_minmax;
                                tab.waveform_pyramid = res.waveform_pyramid;
                                tab.loading = false;
                                tab.loading_waveform_minmax.clear();
                                tab.samples_len_visual = tab.samples_len;
                                Self::invalidate_editor_viewport_cache(tab);
                                if tab.samples_len != old_audio_len {
                                    spectro_reset_paths.push(tab.path.clone());
                                }
                                if !is_clear_edit {
                                    marker_updates.push((idx, res.path.clone()));
                                }
                                let new_display_len = if tab.loading && tab.samples_len_visual > 0 {
                                    tab.samples_len_visual
                                } else {
                                    tab.samples_len
                                };
                                remap_view_for_display_len(
                                    tab,
                                    old_display_len,
                                    old_view,
                                    old_spp,
                                    new_display_len,
                                );
                                Self::editor_clamp_ranges(tab);
                                Self::invalidate_editor_viewport_cache(tab);
                                decode_update_tab = Some((idx, is_clear_edit));
                                if is_clear_edit {
                                    clear_edit_completed = Some((idx, res.path.clone()));
                                }
                                // A session may restore the active tool and its
                                // parameters without persisting a lightweight
                                // overview-only preview. Do not implicitly
                                // recreate that transient preview when decode
                                // finishes. Existing/full previews (or previews
                                // requested while loading) are rebuilt below.
                                if had_preview {
                                    decode_refresh_preview = Some(idx);
                                }
                                if had_preview && self.active_tab == Some(idx) {
                                    decode_cancel_preview = true;
                                }
                            }
                            EditorDecodeEvent::Progress => {
                                tab.loading = true;
                                if !res.loading_waveform_minmax.is_empty() {
                                    tab.loading_waveform_minmax = res.loading_waveform_minmax;
                                    Self::invalidate_editor_viewport_cache(tab);
                                }
                                if let Some(visual_total_frames) =
                                    res.visual_total_frames.filter(|v| *v > 0)
                                {
                                    tab.samples_len_visual =
                                        visual_total_frames.max(tab.samples_len);
                                } else if tab.samples_len_visual == 0 {
                                    tab.samples_len_visual = tab.samples_len;
                                }
                                let new_display_len = if tab.samples_len_visual > 0 {
                                    tab.samples_len_visual
                                } else {
                                    tab.samples_len
                                };
                                remap_view_for_display_len(
                                    tab,
                                    old_display_len,
                                    old_view,
                                    old_spp,
                                    new_display_len,
                                );
                                Self::invalidate_editor_viewport_cache(tab);
                            }
                            EditorDecodeEvent::PagedReady => {
                                let is_clear_edit = clear_edit_path.as_ref() == Some(&res.path);
                                tab.preview_audio_tool = None;
                                tab.preview_overlay = None;
                                tab.ch_samples.clear();
                                tab.ch_samples_arc = std::sync::Arc::new(Vec::new());
                                tab.buffer_sample_rate = res.buffer_sample_rate.max(1);
                                if !res.waveform_minmax.is_empty() {
                                    tab.waveform_minmax = res.waveform_minmax;
                                }
                                if !res.loading_waveform_minmax.is_empty() {
                                    tab.loading_waveform_minmax = res.loading_waveform_minmax;
                                }
                                if let Some(visual_total_frames) = res.visual_total_frames {
                                    tab.samples_len_visual = visual_total_frames;
                                }
                                tab.samples_len = 0;
                                tab.loading = false;
                                tab.paged_asset = true;
                                Self::invalidate_editor_viewport_cache(tab);
                                paged_ready_tab = Some(idx);
                                if is_clear_edit {
                                    clear_edit_completed = Some((idx, res.path.clone()));
                                }
                            }
                            EditorDecodeEvent::Failed => {}
                        }
                    }
                }
                match res.event {
                    EditorDecodeEvent::FinalReady => {
                        decode_final_events.push((res.path.clone(), res.decoded_frames));
                        decode_done = true;
                        decode_done_loading_stats =
                            Some((state.loading_waveform_updates, state.max_progress_gap_ms));
                    }
                    EditorDecodeEvent::PagedReady => {
                        decode_final_events.push((res.path.clone(), res.decoded_frames));
                        decode_done = true;
                        decode_done_loading_stats =
                            Some((state.loading_waveform_updates, state.max_progress_gap_ms));
                    }
                    EditorDecodeEvent::Progress => {
                        if !state.partial_ready {
                            decode_partial_events.push((
                                res.path.clone(),
                                res.decoded_frames,
                                res.stage,
                            ));
                        }
                        state.partial_ready = true;
                    }
                    EditorDecodeEvent::Failed => {}
                }
            }
        }
        if let Some(path) = decode_worker_disconnected {
            if let Some(idx) = self.tabs.iter().position(|tab| tab.path == path) {
                if let Some(tab) = self.tabs.get_mut(idx) {
                    tab.loading = false;
                    tab.paged_asset = tab.ch_samples.is_empty() && tab.samples_len_visual > 0;
                    Self::invalidate_editor_viewport_cache(tab);
                }
            }
            decode_error = Some((
                path,
                "waveform worker ended before publishing a final result".to_string(),
            ));
            decode_done = true;
        }
        for (path, decoded_frames, stage) in decode_partial_events {
            self.debug_mark_editor_open_partial(&path, decoded_frames, stage);
        }
        for (path, decoded_frames) in decode_final_events {
            self.debug_mark_editor_open_final(&path, decoded_frames);
        }
        for value_ms in decode_progress_gap_ms {
            Self::debug_push_latency_sample(
                &mut self.debug.editor_decode_progress_emit_ms,
                value_ms,
            );
        }
        for value_ms in decode_finalize_audio_ms {
            Self::debug_push_latency_sample(
                &mut self.debug.editor_decode_finalize_audio_ms,
                value_ms,
            );
        }
        for value_ms in decode_finalize_waveform_ms {
            Self::debug_push_latency_sample(
                &mut self.debug.editor_decode_finalize_waveform_ms,
                value_ms,
            );
        }
        if let Some((updates, max_gap_ms)) = decode_done_loading_stats {
            self.debug.editor_loading_waveform_updates = self
                .debug
                .editor_loading_waveform_updates
                .saturating_add(updates);
            if max_gap_ms > 0.0 {
                Self::debug_push_latency_sample(
                    &mut self.debug.editor_loading_progress_max_gap_ms,
                    max_gap_ms,
                );
            }
        }
        for path in spectro_reset_paths {
            self.cancel_spectrogram_for_path(&path);
            self.cancel_feature_analysis_for_path(&path);
        }
        if let Some((path, err)) = decode_error {
            self.debug_log(format!("editor decode failed: {} ({err})", path.display()));
            self.cancel_editor_playback_handoff_for_path(&path);
            if self.editor_clear_edit_pending.as_ref() == Some(&path) {
                self.editor_clear_edit_pending = None;
                self.push_toast(
                    crate::app::types::ToastSeverity::Error,
                    format!("Clear Edit failed: {err}"),
                );
            }
        }
        if decode_cancel_preview {
            self.cancel_heavy_preview();
        }
        if !marker_updates.is_empty() {
            let out_sr = self.audio.shared.out_sample_rate;
            for (idx, path) in marker_updates {
                let file_sr = self.sample_rate_for_path(&path, out_sr);
                if let Some(tab) = self.tabs.get_mut(idx) {
                    if tab.loop_region.is_none() && tab.loop_markers_saved.is_none() {
                        Self::set_loop_region_from_file_markers(tab, &path, file_sr, out_sr);
                    }
                    // A reopened cached edit already carries the user's
                    // unsaved markers. Do not replace those with the source
                    // file's marker set when the ready-channel decode
                    // finalizes.
                    if !tab.markers_dirty {
                        Self::load_markers_for_tab(tab, &path, out_sr, file_sr);
                    }
                }
            }
        }
        if let Some((idx, is_clear_edit)) = decode_update_tab {
            // Unified gain framework: a pending list gain becomes a regular
            // editor edit (undo/dirty) as soon as the tab has real audio.
            if !is_clear_edit {
                self.editor_bake_pending_gain_into_tab(idx);
            }
            if self.active_tab == Some(idx) {
                let tab_audio = self.tabs.get(idx).and_then(|tab| {
                    if tab.ch_samples.is_empty() {
                        None
                    } else {
                        Some((
                            tab.path.clone(),
                            tab.buffer_sample_rate.max(1),
                            tab.ch_samples_arc.clone(),
                        ))
                    }
                });
                if let Some((tab_path, buffer_sr, channels)) = tab_audio {
                    if !self.try_activate_editor_stream_transport_for_tab(idx) {
                        let handoff = self.editor_playback_handoff_matches(&tab_path);
                        self.set_editor_shared_buffer_transport_preserving_time(
                            tab_path.as_path(),
                            channels,
                            buffer_sr,
                        );
                        self.playback_mark_buffer_source(
                            super::PlaybackSourceKind::EditorTab(tab_path.clone()),
                            buffer_sr,
                        );
                        if let Some(tab) = self.tabs.get(idx) {
                            self.apply_loop_mode_for_tab(tab);
                        }
                        if handoff {
                            self.finish_editor_playback_handoff(&tab_path, true);
                        }
                    }
                }
            }
        }
        if let Some((idx, path)) = clear_edit_completed {
            // Clear Edit is a hard reset: unlike other destructive edits, it
            // cannot leave an undo point back to the discarded edited audio.
            if let Some(tab) = self.tabs.get_mut(idx) {
                tab.dirty = false;
                tab.undo_stack.clear();
                tab.undo_bytes = 0;
                tab.redo_stack.clear();
                tab.redo_bytes = 0;
            }
            self.edited_cache.remove(&path);
            self.cancel_spectrogram_for_path(&path);
            self.cancel_feature_analysis_for_path(&path);
            self.advance_asset_revision_for_path(&path);
            self.editor_clear_edit_pending = None;
        }
        if let Some(idx) = paged_ready_tab {
            let _ = self.try_activate_editor_stream_transport_for_tab(idx);
        }
        if let Some(idx) = decode_refresh_preview {
            if self.active_tab == Some(idx) {
                self.refresh_tool_preview_for_tab(idx);
            }
        }
        if decode_done {
            self.editor_decode_state = None;
        }
    }
}
