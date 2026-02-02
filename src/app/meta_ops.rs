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

    pub(super) fn queue_full_meta_for_path(&mut self, path: &PathBuf, priority: bool) {
        if self.is_virtual_path(path) {
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

    pub(super) fn drain_meta_updates(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.meta_rx else {
            return;
        };
        let mut updates: Vec<meta::MetaUpdate> = Vec::new();
        while let Ok(update) = rx.try_recv() {
            updates.push(update);
        }
        if updates.is_empty() {
            return;
        }
        let mut resort = false;
        let mut refilter = false;
        for update in updates {
            match update {
                meta::MetaUpdate::Header(p, m) => {
                    if self.set_meta_for_path(&p, m) {
                        resort = true;
                    }
                    self.update_csv_export_progress_for_path(&p);
                }
                meta::MetaUpdate::Full(p, m) => {
                    self.meta_inflight.remove(&p);
                    if self.set_meta_for_path(&p, m) {
                        resort = true;
                    }
                    self.update_csv_export_progress_for_path(&p);
                }
                meta::MetaUpdate::Transcript(p, t) => {
                    self.transcript_inflight.remove(&p);
                    if self.set_transcript_for_path(&p, t)
                        && !self.search_query.trim().is_empty()
                    {
                        refilter = true;
                    }
                }
            }
        }
        if refilter {
            self.apply_filter_from_search();
            self.apply_sort();
            ctx.request_repaint();
        } else if resort {
            self.apply_sort();
            ctx.request_repaint();
        }
    }
}
