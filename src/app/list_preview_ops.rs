use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::types::{
    ListPreviewCacheEntry, ListPreviewPrefetchResult, ListPreviewResult, ListPreviewSettings,
};

impl super::WavesPreviewer {
    pub(super) fn cancel_list_preview_job(&mut self) {
        self.list_preview_rx = None;
        self.list_preview_partial_ready = false;
        self.list_preview_job_id = self.list_preview_job_id.wrapping_add(1);
        self.list_preview_job_epoch
            .store(self.list_preview_job_id, Ordering::Relaxed);
    }

    fn remap_preview_channels(
        channels: &mut Vec<Vec<f32>>,
        in_sr: u32,
        out_sr: u32,
        target_sr: Option<u32>,
        bit_depth: Option<crate::wave::WavBitDepth>,
        resample_quality: crate::wave::ResampleQuality,
    ) {
        let mut cur_sr = in_sr.max(1);
        if let Some(target) = target_sr.filter(|v| *v > 0) {
            let target = target.max(1);
            if cur_sr != target {
                for c in channels.iter_mut() {
                    *c = crate::wave::resample_quality(c, cur_sr, target, resample_quality);
                }
            }
            cur_sr = target;
        }
        if cur_sr != out_sr {
            for c in channels.iter_mut() {
                *c = crate::wave::resample_quality(c, cur_sr, out_sr, resample_quality);
            }
        }
        if let Some(depth) = bit_depth {
            crate::wave::quantize_channels_in_place(channels, depth);
        }
    }

    fn preview_settings_for_path(&self, path: &Path) -> ListPreviewSettings {
        ListPreviewSettings {
            out_sr: self.audio.shared.out_sample_rate.max(1),
            target_sr: self.sample_rate_override.get(path).copied().filter(|v| *v > 0),
            bit_depth: self.bit_depth_override.get(path).copied(),
            quality: self.src_quality,
        }
    }

    fn touch_list_preview_cache_path(&mut self, path: &Path) {
        if let Some(pos) = self
            .list_preview_cache_order
            .iter()
            .position(|p| p.as_path() == path)
        {
            self.list_preview_cache_order.remove(pos);
        }
        self.list_preview_cache_order.push_back(path.to_path_buf());
    }

    fn insert_list_preview_cache_entry(&mut self, path: PathBuf, entry: ListPreviewCacheEntry) {
        self.list_preview_cache.insert(path.clone(), entry);
        self.touch_list_preview_cache_path(&path);
        while self.list_preview_cache_order.len() > crate::app::LIST_PREVIEW_CACHE_MAX {
            if let Some(oldest) = self.list_preview_cache_order.pop_front() {
                self.list_preview_cache.remove(&oldest);
            } else {
                break;
            }
        }
    }

    pub(super) fn evict_list_preview_cache_path(&mut self, path: &Path) {
        self.list_preview_cache.remove(path);
        if let Some(pos) = self
            .list_preview_cache_order
            .iter()
            .position(|p| p.as_path() == path)
        {
            self.list_preview_cache_order.remove(pos);
        }
        self.list_preview_prefetch_inflight.remove(path);
    }

    pub(super) fn take_cached_list_preview(
        &mut self,
        path: &Path,
    ) -> Option<(std::sync::Arc<crate::audio::AudioBuffer>, bool)> {
        let settings = self.preview_settings_for_path(path);
        let matches = self
            .list_preview_cache
            .get(path)
            .map(|entry| entry.settings == settings)
            .unwrap_or(false);
        if !matches {
            self.evict_list_preview_cache_path(path);
            return None;
        }
        let entry = self.list_preview_cache.get(path)?.clone();
        self.touch_list_preview_cache_path(path);
        Some((entry.audio, entry.truncated))
    }

    fn has_compatible_cached_list_preview(&mut self, path: &Path) -> bool {
        let settings = self.preview_settings_for_path(path);
        let matches = self
            .list_preview_cache
            .get(path)
            .map(|entry| entry.settings == settings)
            .unwrap_or(false);
        if matches {
            self.touch_list_preview_cache_path(path);
            true
        } else {
            if self.list_preview_cache.contains_key(path) {
                self.evict_list_preview_cache_path(path);
            }
            false
        }
    }

    pub(super) fn spawn_list_preview_async(&mut self, path: PathBuf, max_secs: f32) {
        use std::sync::mpsc;
        self.list_preview_job_id = self.list_preview_job_id.wrapping_add(1);
        let job_id = self.list_preview_job_id;
        self.list_preview_job_epoch.store(job_id, Ordering::Relaxed);
        self.list_preview_partial_ready = false;
        let job_epoch = self.list_preview_job_epoch.clone();
        let settings = self.preview_settings_for_path(&path);
        let out_sr = settings.out_sr;
        let target_sr = settings.target_sr;
        let bit_depth = settings.bit_depth;
        let resample_quality = Self::to_wave_resample_quality(settings.quality);
        let (tx, rx) = mpsc::channel::<ListPreviewResult>();
        std::thread::spawn(move || {
            let prefix = crate::wave::decode_wav_multi_prefix(&path, max_secs);
            let (mut prefix_channels, prefix_sr, truncated) = match prefix {
                Ok(v) => v,
                Err(_) => return,
            };
            Self::remap_preview_channels(
                &mut prefix_channels,
                prefix_sr,
                out_sr,
                target_sr,
                bit_depth,
                resample_quality,
            );
            if tx
                .send(ListPreviewResult {
                    path: path.clone(),
                    channels: prefix_channels,
                    job_id,
                    is_final: !truncated,
                    settings,
                })
                .is_err()
            {
                return;
            }
            if !truncated {
                return;
            }
            std::thread::yield_now();
            std::thread::sleep(std::time::Duration::from_millis(20));
            if job_epoch.load(Ordering::Relaxed) != job_id {
                return;
            }
            let full = crate::wave::decode_wav_multi(&path);
            let (mut full_channels, full_sr) = match full {
                Ok(v) => v,
                Err(_) => return,
            };
            if job_epoch.load(Ordering::Relaxed) != job_id {
                return;
            }
            Self::remap_preview_channels(
                &mut full_channels,
                full_sr,
                out_sr,
                target_sr,
                bit_depth,
                resample_quality,
            );
            let _ = tx.send(ListPreviewResult {
                path,
                channels: full_channels,
                job_id,
                is_final: true,
                settings,
            });
        });
        self.list_preview_rx = Some(rx);
    }

    fn spawn_list_preview_prefetch(&mut self, path: PathBuf, max_secs: f32) {
        use std::sync::mpsc;
        if self.list_preview_prefetch_tx.is_none() || self.list_preview_prefetch_rx.is_none() {
            let (tx, rx) = mpsc::channel::<ListPreviewPrefetchResult>();
            self.list_preview_prefetch_tx = Some(tx);
            self.list_preview_prefetch_rx = Some(rx);
        }
        let Some(tx) = self.list_preview_prefetch_tx.as_ref().cloned() else {
            return;
        };
        let settings = self.preview_settings_for_path(&path);
        let out_sr = settings.out_sr;
        let target_sr = settings.target_sr;
        let bit_depth = settings.bit_depth;
        let resample_quality = Self::to_wave_resample_quality(settings.quality);
        self.list_preview_prefetch_inflight.insert(path.clone());
        std::thread::spawn(move || {
            let entry = match crate::wave::decode_wav_multi_prefix(&path, max_secs) {
                Ok((mut channels, in_sr, truncated)) => {
                    Self::remap_preview_channels(
                        &mut channels,
                        in_sr,
                        out_sr,
                        target_sr,
                        bit_depth,
                        resample_quality,
                    );
                    Some(ListPreviewCacheEntry {
                        audio: std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(
                            channels,
                        )),
                        truncated,
                        settings,
                    })
                }
                Err(_) => None,
            };
            let _ = tx.send(ListPreviewPrefetchResult { path, entry });
        });
    }

    pub(super) fn queue_list_preview_prefetch_for_rows(
        &mut self,
        visible_first_row: Option<usize>,
        visible_last_row: Option<usize>,
    ) {
        if !self.auto_play_list_nav {
            return;
        }
        if self.list_preview_rx.is_some() || self.list_preview_pending_path.is_some() {
            // Prioritize interactive decode over speculative prefetch.
            return;
        }
        if self.active_tab.is_some() || self.scan_in_progress || self.files.is_empty() {
            return;
        }
        if self.list_preview_prefetch_inflight.len() >= crate::app::LIST_PREVIEW_PREFETCH_INFLIGHT_MAX {
            return;
        }
        let mut wanted: Vec<usize> = Vec::new();
        if let Some(sel) = self.selected {
            for d in 1..=6usize {
                let next = sel.saturating_add(d);
                if next < self.files.len() {
                    wanted.push(next);
                }
            }
        }
        if let (Some(first), Some(last)) = (visible_first_row, visible_last_row) {
            let after0 = last.saturating_add(1);
            if after0 < self.files.len() {
                wanted.push(after0);
            }
            let after1 = last.saturating_add(2);
            if after1 < self.files.len() {
                wanted.push(after1);
            }
            if first > 0 {
                wanted.push(first - 1);
            }
        }
        let mut queued = 0usize;
        for row in wanted {
            if self.list_preview_prefetch_inflight.len() >= crate::app::LIST_PREVIEW_PREFETCH_INFLIGHT_MAX {
                break;
            }
            if queued >= 2 {
                break;
            }
            let Some(path) = self.path_for_row(row).cloned() else {
                continue;
            };
            if self.is_virtual_path(&path) {
                continue;
            }
            if self.list_preview_prefetch_inflight.contains(&path) {
                continue;
            }
            if self.has_compatible_cached_list_preview(&path) {
                continue;
            }
            self.spawn_list_preview_prefetch(path, 0.35);
            queued += 1;
        }
    }

    pub(super) fn drain_list_preview_prefetch_results(&mut self) {
        let Some(rx) = self.list_preview_prefetch_rx.take() else {
            return;
        };
        let mut keep_rx = true;
        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    self.list_preview_prefetch_inflight.remove(&msg.path);
                    if let Some(entry) = msg.entry {
                        self.insert_list_preview_cache_entry(msg.path, entry);
                    } else {
                        self.evict_list_preview_cache_path(&msg.path);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    keep_rx = false;
                    break;
                }
            }
        }
        if keep_rx {
            self.list_preview_prefetch_rx = Some(rx);
        } else {
            self.list_preview_prefetch_tx = None;
            self.list_preview_prefetch_inflight.clear();
        }
    }

    pub(super) fn drain_list_preview_results(&mut self) {
        let Some(rx) = self.list_preview_rx.take() else {
            return;
        };
        let mut keep_rx = true;
        let mut pending_to_start: Option<PathBuf> = None;
        loop {
            match rx.try_recv() {
                Ok(res) => {
                    let latest_job = res.job_id == self.list_preview_job_id;
                    if latest_job {
                        if res.is_final {
                            self.list_preview_partial_ready = false;
                        } else {
                            self.list_preview_partial_ready = true;
                        }
                        let audio = std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(
                            res.channels,
                        ));
                        let truncated = !res.is_final;
                        self.insert_list_preview_cache_entry(
                            res.path.clone(),
                            ListPreviewCacheEntry {
                                audio: audio.clone(),
                                truncated,
                                settings: res.settings,
                            },
                        );
                        if self.active_tab.is_none() && self.playing_path.as_ref() == Some(&res.path) {
                            self.audio.replace_samples_keep_pos(audio);
                            let selected_matches = self
                                .selected_path_buf()
                                .map(|p| p == res.path)
                                .unwrap_or(false);
                            if self.list_play_pending
                                || (self.auto_play_list_nav && selected_matches)
                            {
                                self.audio.play();
                                self.list_play_pending = false;
                            }
                        }
                        if res.is_final {
                            keep_rx = false;
                        }
                        if pending_to_start.is_none() {
                            if let Some(pending) = self.list_preview_pending_path.clone() {
                                if pending != res.path {
                                    pending_to_start = Some(pending);
                                    // Do not wait for full decode of a stale row.
                                    if !res.is_final {
                                        keep_rx = false;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    keep_rx = false;
                    break;
                }
            }
        }
        if keep_rx {
            self.list_preview_rx = Some(rx);
        } else {
            self.list_preview_partial_ready = false;
            let pending = pending_to_start.or_else(|| self.list_preview_pending_path.take());
            if let Some(path) = pending {
                let selected_matches = self
                    .selected_path_buf()
                    .map(|p| p.as_path() == path.as_path())
                    .unwrap_or(false);
                self.list_preview_pending_path = None;
                if self.active_tab.is_none()
                    && selected_matches
                    && !self.is_virtual_path(&path)
                    && path.is_file()
                {
                    if let Some((audio, truncated)) = self.take_cached_list_preview(&path) {
                        self.audio.set_samples_buffer(audio);
                        self.audio.stop();
                        self.apply_effective_volume();
                        if self.list_play_pending || self.auto_play_list_nav {
                            self.audio.play();
                            self.list_play_pending = false;
                        }
                        if truncated {
                            self.spawn_list_preview_async(path, 0.35);
                        }
                    } else {
                        self.audio.stop();
                        self.audio.set_samples_mono(Vec::new());
                        self.spawn_list_preview_async(path, 0.35);
                        self.apply_effective_volume();
                    }
                } else {
                    self.list_play_pending = false;
                }
            } else {
                self.list_play_pending = false;
            }
        }
    }
}
