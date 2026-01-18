use std::path::{Path, PathBuf};

use super::types::{
    SpectrogramConfig, SpectrogramData, SpectrogramJobMsg, SpectrogramProgress, SpectrogramTile,
    ViewMode,
};

impl super::WavesPreviewer {
    pub(super) fn touch_spectro_cache(&mut self, path: &Path) {
        if let Some(pos) = self
            .spectro_cache_order
            .iter()
            .position(|p| p.as_path() == path)
        {
            self.spectro_cache_order.remove(pos);
        }
        self.spectro_cache_order.push_back(path.to_path_buf());
    }

    fn update_spectro_cache_size(&mut self, path: &Path, new_bytes: usize) {
        let prev = self
            .spectro_cache_sizes
            .insert(path.to_path_buf(), new_bytes)
            .unwrap_or(0);
        if new_bytes >= prev {
            self.spectro_cache_bytes = self.spectro_cache_bytes.saturating_add(new_bytes - prev);
        } else {
            self.spectro_cache_bytes = self.spectro_cache_bytes.saturating_sub(prev - new_bytes);
        }
    }

    fn evict_spectro_cache_if_needed(&mut self) {
        while self.spectro_cache_bytes > super::SPECTRO_CACHE_MAX_BYTES {
            let Some(path) = self.spectro_cache_order.pop_front() else {
                break;
            };
            if self.spectro_inflight.contains(&path) {
                // Keep in-flight items; push to back for later eviction.
                self.spectro_cache_order.push_back(path);
                break;
            }
            self.purge_spectro_cache_entry(&path);
        }
    }

    pub(super) fn purge_spectro_cache_entry(&mut self, path: &Path) {
        if let Some(flag) = self.spectro_cancel.remove(path) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.spectro_inflight.remove(path);
        self.spectro_progress.remove(path);
        self.spectro_cache.remove(path);
        if let Some(prev) = self.spectro_cache_sizes.remove(path) {
            self.spectro_cache_bytes = self.spectro_cache_bytes.saturating_sub(prev);
        }
        if let Some(pos) = self
            .spectro_cache_order
            .iter()
            .position(|p| p.as_path() == path)
        {
            self.spectro_cache_order.remove(pos);
        }
    }

    fn ensure_spectro_channel(&mut self) {
        if self.spectro_tx.is_none() || self.spectro_rx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<SpectrogramJobMsg>();
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
                let _ = tx.send(SpectrogramJobMsg::Done(path));
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
                    let _ = tx.send(SpectrogramJobMsg::Tile(SpectrogramTile {
                        path: path.clone(),
                        channel_index: ci,
                        channel_count,
                        frames: params.frames,
                        bins: params.bins,
                        frame_step: params.frame_step,
                        sample_rate,
                        start_frame: start,
                        values_db: values,
                    }));
                    start = end;
                }
            }
            let _ = tx.send(SpectrogramJobMsg::Done(path));
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
            let use_mixdown =
                channel_view.mode == super::types::ChannelViewMode::Mixdown || requested.is_empty();
            let channels = if use_mixdown {
                vec![super::WavesPreviewer::mixdown_channels(
                    &tab.ch_samples,
                    tab.samples_len,
                )]
            } else if channel_view.mode == super::types::ChannelViewMode::All {
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
        if self.spectro_cache.contains_key(&path) || self.spectro_inflight.contains(&path) {
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
        self.spectro_inflight.insert(path.clone());
        self.spawn_spectrogram_job(path, channels, sr, self.spectro_cfg.clone());
    }

    pub(super) fn drain_spectrogram_jobs(&mut self, ctx: &egui::Context) {
        let mut messages = Vec::new();
        if let Some(rx) = &self.spectro_rx {
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }
        for msg in messages {
            match msg {
                SpectrogramJobMsg::Tile(tile) => {
                    let spec_entry =
                        self.spectro_cache
                            .entry(tile.path.clone())
                            .or_insert_with(|| {
                                let mut specs = Vec::with_capacity(tile.channel_count.max(1));
                                for _ in 0..tile.channel_count.max(1) {
                                    specs.push(SpectrogramData {
                                        frames: 0,
                                        bins: 0,
                                        frame_step: tile.frame_step,
                                        sample_rate: tile.sample_rate,
                                        values_db: Vec::new(),
                                    });
                                }
                                std::sync::Arc::new(specs)
                            });
                    let specs = std::sync::Arc::make_mut(spec_entry);
                    let mut size_changed = false;
                    if specs.len() < tile.channel_count.max(1) {
                        let missing = tile.channel_count.max(1) - specs.len();
                        for _ in 0..missing {
                            specs.push(SpectrogramData {
                                frames: 0,
                                bins: 0,
                                frame_step: tile.frame_step,
                                sample_rate: tile.sample_rate,
                                values_db: Vec::new(),
                            });
                        }
                    }
                    if let Some(spec) = specs.get_mut(tile.channel_index) {
                        if spec.frames != tile.frames
                            || spec.bins != tile.bins
                            || spec.frame_step != tile.frame_step
                            || spec.sample_rate != tile.sample_rate
                        {
                            spec.frames = tile.frames;
                            spec.bins = tile.bins;
                            spec.frame_step = tile.frame_step;
                            spec.sample_rate = tile.sample_rate;
                            spec.values_db = vec![-120.0; tile.frames.saturating_mul(tile.bins)];
                            size_changed = true;
                        }
                        let base = tile.start_frame.saturating_mul(tile.bins);
                        let end = base
                            .saturating_add(tile.values_db.len())
                            .min(spec.values_db.len());
                        let len = end.saturating_sub(base);
                        if len > 0 {
                            spec.values_db[base..base + len]
                                .copy_from_slice(&tile.values_db[..len]);
                        }
                    }
                    if let Some(progress) = self.spectro_progress.get_mut(&tile.path) {
                        progress.done_tiles = progress.done_tiles.saturating_add(1);
                    }
                    if size_changed {
                        let bytes: usize = specs
                            .iter()
                            .map(|s| s.values_db.len().saturating_mul(std::mem::size_of::<f32>()))
                            .sum();
                        self.update_spectro_cache_size(&tile.path, bytes);
                        self.touch_spectro_cache(&tile.path);
                        self.evict_spectro_cache_if_needed();
                    }
                    ctx.request_repaint();
                }
                SpectrogramJobMsg::Done(path) => {
                    self.spectro_inflight.remove(&path);
                    self.spectro_progress.remove(&path);
                    self.spectro_cancel.remove(&path);
                    self.touch_spectro_cache(&path);
                    self.evict_spectro_cache_if_needed();
                    ctx.request_repaint();
                }
            }
        }
    }

    pub(super) fn cancel_spectrogram_for_path(&mut self, path: &Path) {
        self.purge_spectro_cache_entry(path);
    }

    pub(super) fn cancel_all_spectrograms(&mut self) {
        let paths: Vec<PathBuf> = self.spectro_inflight.iter().cloned().collect();
        for p in paths {
            self.cancel_spectrogram_for_path(&p);
        }
    }
}
