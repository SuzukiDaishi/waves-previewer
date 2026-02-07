use std::path::{Path, PathBuf};

use super::types::{SpectrogramData, SpectrogramJobMsg};

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

    pub(super) fn update_spectro_cache_size(&mut self, path: &Path, new_bytes: usize) {
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

    pub(super) fn apply_spectrogram_updates(&mut self, ctx: &egui::Context) {
        let messages = self.collect_spectrogram_messages();
        for msg in messages {
            self.apply_spectrogram_message(ctx, msg);
        }
    }

    fn collect_spectrogram_messages(&mut self) -> Vec<SpectrogramJobMsg> {
        let mut messages = Vec::new();
        if let Some(rx) = &self.spectro_rx {
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }
        messages
    }

    fn apply_spectrogram_message(&mut self, ctx: &egui::Context, msg: SpectrogramJobMsg) {
        match msg {
            SpectrogramJobMsg::Tile(tile) => {
                let spec_entry = self
                    .spectro_cache
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
                        spec.values_db[base..base + len].copy_from_slice(&tile.values_db[..len]);
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
