use std::path::PathBuf;

use super::{meta, transcript};

impl super::WavesPreviewer {
    pub(super) fn reset_meta_pool(&mut self) {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(6);
        let (pool, rx) = crate::app::meta::spawn_meta_pool(workers);
        self.meta_pool = Some(pool);
        self.meta_rx = Some(rx);
        self.meta_inflight.clear();
        self.meta_sort_pending = false;
        self.meta_sort_last_applied = None;
        self.list_meta_prefetch_cursor = 0;
        self.transcript_inflight.clear();
    }

    pub(super) fn ensure_meta_pool(&mut self) {
        if self.meta_pool.is_none() {
            self.reset_meta_pool();
        }
    }

    pub(super) fn queue_meta_for_path(&mut self, path: &PathBuf, priority: bool) {
        if self.is_virtual_path(path) {
            return;
        }
        if self.meta_for_path(path).is_some() {
            return;
        }
        if !priority
            && self.item_bg_mode != crate::app::types::ItemBgMode::Standard
            && self.files.len() >= crate::app::LIST_BG_META_LARGE_THRESHOLD
            && self.meta_inflight.len() >= crate::app::LIST_BG_META_INFLIGHT_LIMIT
        {
            // Keep large-list background coloring from building an unbounded decode backlog.
            return;
        }
        self.ensure_meta_pool();
        if let Some(pool) = &self.meta_pool {
            if self.meta_inflight.contains(path) {
                if priority {
                    pool.promote_path(path);
                }
                return;
            }
            self.meta_inflight.insert(path.clone());
            if priority {
                pool.enqueue_front(meta::MetaTask::Header(path.clone()));
            } else {
                pool.enqueue(meta::MetaTask::Header(path.clone()));
            }
        }
    }

    pub(super) fn queue_header_meta_for_path(&mut self, path: &PathBuf, priority: bool) {
        if self.is_virtual_path(path) {
            return;
        }
        if self.meta_for_path(path).is_some() {
            return;
        }
        if !priority
            && self.item_bg_mode != crate::app::types::ItemBgMode::Standard
            && self.files.len() >= crate::app::LIST_BG_META_LARGE_THRESHOLD
            && self.meta_inflight.len() >= crate::app::LIST_BG_META_INFLIGHT_LIMIT
        {
            return;
        }
        self.ensure_meta_pool();
        if let Some(pool) = &self.meta_pool {
            if self.meta_inflight.contains(path) {
                if priority {
                    pool.promote_path(path);
                }
                return;
            }
            self.meta_inflight.insert(path.clone());
            let task = meta::MetaTask::HeaderOnly(path.clone());
            if priority {
                pool.enqueue_front(task);
            } else {
                pool.enqueue(task);
            }
        }
    }

    pub(super) fn queue_full_meta_for_path(&mut self, path: &PathBuf, priority: bool) {
        if self.is_virtual_path(path) {
            return;
        }
        if !priority
            && self.item_bg_mode != crate::app::types::ItemBgMode::Standard
            && self.files.len() >= crate::app::LIST_BG_META_LARGE_THRESHOLD
            && self.meta_inflight.len() >= crate::app::LIST_BG_META_INFLIGHT_LIMIT
        {
            return;
        }
        self.ensure_meta_pool();
        if let Some(pool) = &self.meta_pool {
            if self.meta_inflight.contains(path) {
                if priority {
                    pool.promote_path(path);
                }
                return;
            }
            self.meta_inflight.insert(path.clone());
            let task = meta::MetaTask::Decode(path.clone());
            if priority {
                pool.enqueue_front(task);
            } else {
                pool.enqueue(task);
            }
        }
    }

    pub(super) fn queue_transcript_for_path(&mut self, path: &PathBuf, priority: bool) {
        if self.is_virtual_path(path) {
            return;
        }
        let Some(srt_path) = transcript::srt_path_for_audio(path) else {
            return;
        };
        if !srt_path.is_file() {
            self.clear_transcript_for_path(path);
            self.transcript_inflight.remove(path);
            return;
        }
        if self.transcript_for_path(path).is_some() {
            return;
        }
        self.ensure_meta_pool();
        if let Some(pool) = &self.meta_pool {
            if self.transcript_inflight.contains(path) {
                if priority {
                    pool.promote_path(path);
                }
                return;
            }
            self.transcript_inflight.insert(path.clone());
            if priority {
                pool.enqueue_front(meta::MetaTask::Transcript(path.clone()));
            } else {
                pool.enqueue(meta::MetaTask::Transcript(path.clone()));
            }
        }
    }

    pub(super) fn pump_list_meta_prefetch(&mut self) {
        if self.scan_in_progress {
            return;
        }
        if self.active_tab.is_some()
            || self.item_bg_mode == crate::app::types::ItemBgMode::Standard
            || self.files.is_empty()
        {
            self.list_meta_prefetch_cursor = 0;
            return;
        }
        if self.files.len() >= crate::app::LIST_BG_META_LARGE_THRESHOLD {
            // Visible-window prefetch in `ui/list.rs` is enough for very large lists.
            self.list_meta_prefetch_cursor = 0;
            return;
        }
        let total = self.files.len();
        self.list_meta_prefetch_cursor %= total;
        let mut scanned = 0usize;
        let mut queued = 0usize;
        while scanned < total && queued < crate::app::LIST_META_PREFETCH_BUDGET {
            let idx = (self.list_meta_prefetch_cursor + scanned) % total;
            scanned += 1;
            let Some(path) = self.path_for_row(idx).cloned() else {
                continue;
            };
            if self.is_virtual_path(&path) {
                continue;
            }
            if self.meta_for_path(&path).is_some() || self.meta_inflight.contains(&path) {
                continue;
            }
            self.queue_meta_for_path(&path, false);
            queued += 1;
        }
        self.list_meta_prefetch_cursor = (self.list_meta_prefetch_cursor + scanned) % total;
    }

    fn flush_pending_meta_sort(&mut self, ctx: &egui::Context, force: bool) {
        if !self.meta_sort_pending {
            return;
        }
        let now = std::time::Instant::now();
        let due = force
            || self
                .meta_sort_last_applied
                .map(|last| {
                    now.duration_since(last)
                        >= std::time::Duration::from_millis(crate::app::META_SORT_MIN_INTERVAL_MS)
                })
                .unwrap_or(true);
        if due {
            self.apply_sort();
            self.meta_sort_pending = false;
            self.meta_sort_last_applied = Some(now);
            ctx.request_repaint();
        } else {
            // Keep pumping frames until the next sort window opens.
            ctx.request_repaint();
        }
    }

    pub(super) fn drain_meta_updates(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.meta_rx else {
            return;
        };
        let mut updates: Vec<meta::MetaUpdate> = Vec::new();
        let mut drained = 0usize;
        while drained < crate::app::META_UPDATE_FRAME_BUDGET {
            match rx.try_recv() {
                Ok(update) => {
                    updates.push(update);
                    drained += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if updates.is_empty() {
            self.flush_pending_meta_sort(ctx, false);
            return;
        }
        let mut meta_sort_dirty = false;
        let mut transcript_sort_dirty = false;
        let mut refilter = false;
        for update in updates {
            match update {
                meta::MetaUpdate::Header {
                    path: p,
                    meta: m,
                    finalized,
                } => {
                    if finalized {
                        self.meta_inflight.remove(&p);
                    }
                    if self.set_meta_for_path(&p, m) {
                        meta_sort_dirty = true;
                    }
                    self.update_csv_export_progress_for_path(&p);
                }
                meta::MetaUpdate::Full(p, m) => {
                    self.meta_inflight.remove(&p);
                    if self.set_meta_for_path(&p, m) {
                        meta_sort_dirty = true;
                    }
                    self.update_csv_export_progress_for_path(&p);
                }
                meta::MetaUpdate::Transcript(p, t) => {
                    self.transcript_inflight.remove(&p);
                    if self.set_transcript_for_path(&p, t) {
                        if !self.search_query.trim().is_empty() {
                            refilter = true;
                        } else if self.sort_key_uses_transcript() {
                            transcript_sort_dirty = true;
                        }
                    }
                }
            }
        }
        if refilter {
            self.apply_filter_from_search();
            self.apply_sort();
            self.meta_sort_pending = false;
            self.meta_sort_last_applied = Some(std::time::Instant::now());
            ctx.request_repaint();
        } else {
            if meta_sort_dirty && self.sort_key_uses_meta() {
                self.meta_sort_pending = true;
            }
            if transcript_sort_dirty {
                self.meta_sort_pending = true;
            }
            self.flush_pending_meta_sort(ctx, false);
            ctx.request_repaint();
        }
        if drained >= crate::app::META_UPDATE_FRAME_BUDGET {
            // Avoid a stall by continuing to consume backlog in future frames.
            ctx.request_repaint();
        }
    }
}
