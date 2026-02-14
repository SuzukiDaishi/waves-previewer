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
            let (tx, rx) = std::sync::mpsc::channel::<super::types::SpectrogramJobMsg>();
            self.spectro_tx = Some(tx);
            self.spectro_rx = Some(rx);
        }
    }

    fn spawn_spectrogram_job(
        &mut self,
        path: PathBuf,
        channels: Vec<Vec<f32>>,
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
            let channel_count = channels.len().max(1);
            let len = channels.get(0).map(|c| c.len()).unwrap_or(0);
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
                let ch = channels.get(ci).map(|c| c.as_slice()).unwrap_or(&[]);
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
        let (path, view_mode, channels) = {
            let Some(tab) = self.tabs.get(tab_idx) else {
                return;
            };
            let path = tab.path.clone();
            let view_mode = tab.view_mode;
            let channel_view = tab.channel_view.clone();
            let channel_count = tab.ch_samples.len().max(1);
            let requested = channel_view.visible_indices(channel_count);
            let use_mixdown = channel_view.mode == ChannelViewMode::Mixdown || requested.is_empty();
            let channels = if use_mixdown {
                vec![super::WavesPreviewer::mixdown_channels(
                    &tab.ch_samples,
                    tab.samples_len,
                )]
            } else if channel_view.mode == ChannelViewMode::All {
                tab.ch_samples.clone()
            } else {
                requested
                    .iter()
                    .filter_map(|&idx| tab.ch_samples.get(idx).cloned())
                    .collect()
            };
            (path, view_mode, channels)
        };
        if view_mode == ViewMode::Waveform {
            return;
        }
        if let Some(specs) = self.spectro_cache.get(&path) {
            let empty_cached = specs
                .iter()
                .all(|s| s.frames == 0 || s.values_db.is_empty());
            let has_audio =
                !channels.is_empty() && channels.get(0).map(|c| !c.is_empty()).unwrap_or(false);
            if empty_cached && has_audio {
                self.purge_spectro_cache_entry(&path);
            } else {
                return;
            }
        }
        if self.spectro_inflight.contains(&path) {
            return;
        }
        let sr = self.audio.shared.out_sample_rate;
        let len = channels.get(0).map(|c| c.len()).unwrap_or(0);
        let params = crate::app::render::spectrogram::spectrogram_params(len, &self.spectro_cfg);
        if params.frames == 0 {
            let mut specs = Vec::with_capacity(channels.len().max(1));
            for _ in 0..channels.len().max(1) {
                specs.push(SpectrogramData {
                    frames: 0,
                    bins: params.bins,
                    frame_step: params.frame_step,
                    sample_rate: sr,
                    values_db: Vec::new(),
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
        let total_tiles = tiles_per_channel.saturating_mul(channels.len().max(1));
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
        self.spawn_spectrogram_job(path, channels, sr, self.spectro_cfg.clone(), generation);
    }
}
