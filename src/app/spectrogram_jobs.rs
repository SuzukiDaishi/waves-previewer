use std::path::PathBuf;

use super::types::{
    ChannelViewMode, SpectrogramConfig, SpectrogramData, SpectrogramProgress, ViewMode,
};

impl super::WavesPreviewer {
    fn bump_spectrogram_generation(&mut self, path: &PathBuf) -> u64 {
        self.spectro_generation_counter = self.spectro_generation_counter.wrapping_add(1);
        let generation = self.spectro_generation_counter;
        self.spectro_generation.insert(path.clone(), generation);
        generation
    }

    fn ensure_spectro_channel(&mut self) {
        if self.spectro_tx.is_none() || self.spectro_rx.is_none() {
            let (tx, rx) = std::sync::mpsc::sync_channel::<super::types::SpectrogramJobMsg>(
                self.perf.spectrogram_queue_tiles(),
            );
            self.spectro_tx = Some(tx);
            self.spectro_rx = Some(rx);
        }
    }

    fn spawn_spectrogram_job(
        &mut self,
        path: PathBuf,
        channels: std::sync::Arc<Vec<Vec<f32>>>,
        channel_indices: Vec<usize>,
        use_mixdown: bool,
        samples_len: usize,
        sample_rate: u32,
        cfg: SpectrogramConfig,
        generation: u64,
    ) {
        self.ensure_spectro_channel();
        let Some(tx) = self.spectro_tx.as_ref().cloned() else {
            return;
        };
        let cancel = self
            .spectro_cancel
            .get(&path)
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            // Clip-sized channel selection and mixdown belong on this worker,
            // never on the immediate-mode UI thread.
            let mixdown = use_mixdown
                .then(|| super::WavesPreviewer::mixdown_channels(&channels, samples_len));
            let channel_count = if use_mixdown {
                1
            } else {
                channel_indices.len().max(1)
            };
            let len = mixdown
                .as_ref()
                .map(Vec::len)
                .or_else(|| {
                    channel_indices
                        .first()
                        .and_then(|&index| channels.get(index))
                        .map(Vec::len)
                })
                .unwrap_or(0);
            let params = crate::app::render::spectrogram::spectrogram_params(len, &cfg);
            if params.frames == 0 {
                let _ = tx.send(super::types::SpectrogramJobMsg::Done { path, generation });
                return;
            }
            let tile_frames = super::SPECTRO_TILE_FRAMES;
            for ci in 0..channel_count {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let ch = if let Some(mixdown) = mixdown.as_ref() {
                    mixdown.as_slice()
                } else {
                    channel_indices
                        .get(ci)
                        .and_then(|&index| channels.get(index))
                        .map(Vec::as_slice)
                        .unwrap_or(&[])
                };
                let mut start = 0usize;
                while start < params.frames {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let end = (start + tile_frames).min(params.frames);
                    let values = crate::app::render::spectrogram::compute_spectrogram_tile(
                        ch,
                        sample_rate,
                        &params,
                        start,
                        end,
                    );
                    let _ = tx.send(super::types::SpectrogramJobMsg::Tile(
                        super::types::SpectrogramTile {
                            path: path.clone(),
                            generation,
                            channel_index: ci,
                            channel_count,
                            frames: params.frames,
                            bins: params.bins,
                            frame_step: params.frame_step,
                            sample_rate,
                            start_frame: start,
                            values_db: values,
                        },
                    ));
                    start = end;
                }
            }
            let _ = tx.send(super::types::SpectrogramJobMsg::Done { path, generation });
        });
    }

    pub(super) fn queue_spectrogram_for_tab(&mut self, tab_idx: usize) {
        let (path, view_mode, has_audio) = {
            let Some(tab) = self.tabs.get(tab_idx) else {
                return;
            };
            (
                tab.path.clone(),
                tab.leaf_view_mode(),
                tab.samples_len > 0 && tab.ch_samples_arc.iter().any(|channel| !channel.is_empty()),
            )
        };
        if view_mode == ViewMode::Waveform {
            return;
        }
        // These guards must precede any clip-sized work. Previously even a
        // fully cached spectrogram cloned or mixed the complete clip once per
        // UI frame before reaching these checks.
        if let Some(specs) = self.spectro_cache.get(&path) {
            let empty_cached = specs
                .iter()
                .all(|s| s.frames == 0 || s.values_db.is_empty());
            if empty_cached && has_audio {
                self.purge_spectro_cache_entry(&path);
            } else {
                return;
            }
        }
        if self.spectro_inflight.contains(&path) {
            return;
        }

        let (channels, channel_indices, use_mixdown, samples_len, buffer_sample_rate) = {
            let Some(tab) = self.tabs.get(tab_idx) else {
                return;
            };
            let channel_view = tab.channel_view.clone();
            let channel_count = tab.ch_samples.len().max(1);
            let requested = channel_view.visible_indices(channel_count);
            let use_mixdown = channel_view.mode == ChannelViewMode::Mixdown || requested.is_empty();
            let channel_indices = if use_mixdown {
                Vec::new()
            } else if channel_view.mode == ChannelViewMode::All {
                (0..tab.ch_samples_arc.len()).collect()
            } else {
                requested
                    .iter()
                    .filter_map(|&idx| tab.ch_samples_arc.get(idx).map(|_| idx))
                    .collect()
            };
            (
                tab.ch_samples_arc.clone(),
                channel_indices,
                use_mixdown,
                tab.samples_len,
                tab.buffer_sample_rate.max(1),
            )
        };
        let sr = buffer_sample_rate;
        let len = if use_mixdown {
            samples_len
        } else {
            channel_indices
                .first()
                .and_then(|&index| channels.get(index))
                .map(Vec::len)
                .unwrap_or(0)
        };
        let output_channel_count = if use_mixdown {
            1
        } else {
            channel_indices.len().max(1)
        };
        let params = crate::app::render::spectrogram::spectrogram_params(len, &self.spectro_cfg);
        if params.frames == 0 {
            let mut specs = Vec::with_capacity(output_channel_count);
            for _ in 0..output_channel_count {
                specs.push(SpectrogramData {
                    frames: 0,
                    bins: params.bins,
                    frame_step: params.frame_step,
                    sample_rate: sr,
                    values_db: Vec::new(),
                    values_max_db: f32::MIN,
                });
            }
            self.spectro_cache
                .insert(path.clone(), std::sync::Arc::new(specs));
            self.update_spectro_cache_size(&path, 0);
            self.touch_spectro_cache(&path);
            return;
        }
        let tile_frames = super::SPECTRO_TILE_FRAMES;
        let tiles_per_channel = (params.frames + tile_frames - 1) / tile_frames;
        let total_tiles = tiles_per_channel.saturating_mul(output_channel_count);
        self.spectro_progress.insert(
            path.clone(),
            SpectrogramProgress {
                done_tiles: 0,
                total_tiles,
                started_at: std::time::Instant::now(),
            },
        );
        self.spectro_cancel.insert(
            path.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let generation = self.bump_spectrogram_generation(&path);
        self.spectro_inflight.insert(path.clone());
        self.spawn_spectrogram_job(
            path,
            channels,
            channel_indices,
            use_mixdown,
            samples_len,
            sr,
            self.spectro_cfg.clone(),
            generation,
        );
    }
}
