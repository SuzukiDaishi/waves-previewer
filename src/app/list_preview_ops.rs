use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::types::{
    ListPreviewCacheEntry, ListPreviewPrefetchResult, ListPreviewResult, ListPreviewSettings,
    SrcQuality,
};

impl super::WavesPreviewer {
    fn list_preview_quality_for_path(&self, path: &Path) -> SrcQuality {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        // List preview prioritizes continuity over maximum SRC fidelity.
        match ext.as_str() {
            "mp3" | "m4a" | "ogg" => SrcQuality::Fast,
            _ => match self.src_quality {
                SrcQuality::Best => SrcQuality::Good,
                q => q,
            },
        }
    }

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
    ) -> u32 {
        let mut cur_sr = in_sr.max(1);
        if let Some(target) = target_sr.filter(|v| *v > 0) {
            let target = target.max(1);
            if cur_sr != target {
                for c in channels.iter_mut() {
                    *c = crate::wave::resample_quality(
                        c,
                        cur_sr,
                        target,
                        crate::wave::ResampleQuality::Best,
                    );
                }
            }
            cur_sr = target;
        }
        if let Some(depth) = bit_depth {
            crate::wave::quantize_channels_in_place(channels, depth);
        }
        let rate_ratio = cur_sr as f32 / out_sr.max(1) as f32;
        if rate_ratio < 0.25 || rate_ratio > 4.0 {
            let fallback_quality = if target_sr.is_some() {
                crate::wave::ResampleQuality::Best
            } else {
                resample_quality
            };
            for c in channels.iter_mut() {
                *c = crate::wave::resample_quality(c, cur_sr, out_sr.max(1), fallback_quality);
            }
            return out_sr.max(1);
        }
        cur_sr
    }

    fn preview_settings_for_path(&self, path: &Path) -> ListPreviewSettings {
        ListPreviewSettings {
            out_sr: self.audio.shared.out_sample_rate.max(1),
            target_sr: self
                .sample_rate_override
                .get(path)
                .copied()
                .filter(|v| *v > 0),
            bit_depth: self.bit_depth_override.get(path).copied(),
            quality: self.list_preview_quality_for_path(path),
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
    ) -> Option<(std::sync::Arc<crate::audio::AudioBuffer>, bool, u32)> {
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
        Some((entry.audio, entry.truncated, entry.play_sr.max(1)))
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

    pub(super) fn spawn_list_preview_async(
        &mut self,
        path: PathBuf,
        max_secs: f32,
        emit_every_secs: f32,
    ) {
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
            let _ = crate::audio_io::decode_audio_multi_progressive(
                &path,
                max_secs,
                emit_every_secs,
                || job_epoch.load(Ordering::Relaxed) != job_id,
                |mut channels, in_sr, is_final| {
                    if job_epoch.load(Ordering::Relaxed) != job_id {
                        return false;
                    }
                    let play_sr = Self::remap_preview_channels(
                        &mut channels,
                        in_sr,
                        out_sr,
                        target_sr,
                        bit_depth,
                        resample_quality,
                    );
                    tx.send(ListPreviewResult {
                        path: path.clone(),
                        channels,
                        play_sr,
                        job_id,
                        is_final,
                        settings,
                    })
                    .is_ok()
                },
            );
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
                    let play_sr = Self::remap_preview_channels(
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
                        play_sr,
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
        if self.list_preview_prefetch_inflight.len()
            >= crate::app::LIST_PREVIEW_PREFETCH_INFLIGHT_MAX
        {
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
            if self.list_preview_prefetch_inflight.len()
                >= crate::app::LIST_PREVIEW_PREFETCH_INFLIGHT_MAX
            {
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
                                play_sr: res.play_sr.max(1),
                                truncated,
                                settings: res.settings,
                            },
                        );
                        self.debug_mark_list_preview_ready(&res.path);
                        if self.active_tab.is_none()
                            && self.playing_path.as_ref() == Some(&res.path)
                        {
                            self.audio.replace_samples_keep_pos(audio);
                            self.apply_list_preview_rate(res.play_sr.max(1));
                            if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                                self.audio.set_loop_region(0, buf.len());
                            }
                            self.audio.set_loop_enabled(false);
                            let selected_matches = self
                                .selected_path_buf()
                                .map(|p| p == res.path)
                                .unwrap_or(false);
                            if self.list_play_pending
                                || (self.auto_play_list_nav && selected_matches)
                            {
                                self.audio.play();
                                // Keep pending intent across prefix->full handoff so
                                // manual list playback does not stop at prefix length.
                                if res.is_final {
                                    self.list_play_pending = false;
                                }
                                self.debug_mark_list_play_start(&res.path);
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
                                        self.debug.stale_preview_cancel_count =
                                            self.debug.stale_preview_cancel_count.saturating_add(1);
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
                    if let Some((audio, truncated, play_sr)) = self.take_cached_list_preview(&path) {
                        let needs_play = self.list_play_pending || self.auto_play_list_nav;
                        let cached_secs = self.list_preview_cached_secs(audio.len(), play_sr);
                        let min_secs = if needs_play {
                            self.list_play_prefix_secs(&path) * 0.85
                        } else {
                            0.0
                        };
                        let use_cached_now = !truncated || cached_secs >= min_secs;
                        if use_cached_now {
                            self.audio.set_samples_buffer(audio);
                            self.apply_list_preview_rate(play_sr);
                            if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                                self.audio.set_loop_region(0, buf.len());
                            }
                            self.audio.set_loop_enabled(false);
                            self.audio.stop();
                            self.apply_effective_volume();
                            self.debug_mark_list_preview_ready(&path);
                            if needs_play {
                                self.audio.play();
                                // Keep pending intent while prefix buffer is active.
                                if !truncated {
                                    self.list_play_pending = false;
                                }
                                self.debug_mark_list_play_start(&path);
                            }
                            if truncated {
                                let continue_secs = if needs_play { 0.0 } else { 0.35 };
                                let emit_secs = if needs_play {
                                    crate::app::LIST_PLAY_EMIT_SECS
                                } else {
                                    0.0
                                };
                                self.spawn_list_preview_async(path, continue_secs, emit_secs);
                            }
                        } else {
                            self.evict_list_preview_cache_path(&path);
                            self.audio.stop();
                            self.audio.set_samples_mono(Vec::new());
                            let decode_secs = if needs_play {
                                self.list_play_prefix_secs(&path)
                            } else {
                                0.35
                            };
                            let emit_secs = if needs_play {
                                crate::app::LIST_PLAY_EMIT_SECS
                            } else {
                                0.0
                            };
                            self.spawn_list_preview_async(path, decode_secs, emit_secs);
                            self.apply_effective_volume();
                        }
                    } else {
                        self.audio.stop();
                        self.audio.set_samples_mono(Vec::new());
                        let decode_secs = if self.list_play_pending || self.auto_play_list_nav {
                            self.list_play_prefix_secs(&path)
                        } else {
                            0.35
                        };
                        let emit_secs = if self.list_play_pending || self.auto_play_list_nav {
                            crate::app::LIST_PLAY_EMIT_SECS
                        } else {
                            0.0
                        };
                        self.spawn_list_preview_async(path, decode_secs, emit_secs);
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
