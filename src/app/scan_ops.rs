use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::types::{
    ListLoadKind, PendingListLoadTarget, PendingListLoadTargetKind, ScanMessage, ScanRequestKind,
};
use super::WavesPreviewer;

impl WavesPreviewer {
    fn clear_list_load_runtime(&mut self) {
        self.scan_rx = None;
        self.scan_pending_batches.clear();
        self.scan_in_progress = false;
        self.scan_worker_done = false;
        self.scan_started_at = None;
        self.scan_found_count = 0;
        self.scan_visited_count = 0;
        self.scan_load_kind = None;
        self.scan_pending_target = None;
    }

    fn reset_list_contents_for_folder_load(&mut self) {
        self.clear_list_load_runtime();
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.files.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.transcript_ai_inflight.clear();
        self.cancel_list_preview_job();
        self.list_preview_pending_path = None;
        self.list_preview_prefetch_tx = None;
        self.list_preview_prefetch_rx = None;
        self.list_preview_prefetch_inflight.clear();
        self.list_preview_cache.clear();
        self.list_preview_cache_order.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
        self.reset_all_feature_analysis_state();
        self.selected = None;
        self.selected_multi.clear();
        self.select_anchor = None;
        self.sample_rate_override.clear();
        self.sample_rate_probe_cache.clear();
        self.bit_depth_override.clear();
        self.format_override.clear();
        self.reset_meta_pool();
    }

    fn reset_list_contents_for_explicit_replace(&mut self) {
        self.root = None;
        self.clear_list_load_runtime();
        self.files.clear();
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.transcript_ai_inflight.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
        self.reset_all_feature_analysis_state();
        self.selected = None;
        self.selected_multi.clear();
        self.select_anchor = None;
        self.reset_meta_pool();
    }

    fn start_list_load(
        &mut self,
        request: ScanRequestKind,
        kind: ListLoadKind,
        replace: bool,
        pending_target: Option<PendingListLoadTarget>,
    ) {
        if replace {
            match kind {
                ListLoadKind::Folder => self.reset_list_contents_for_folder_load(),
                ListLoadKind::Files => self.reset_list_contents_for_explicit_replace(),
            }
        } else {
            self.clear_list_load_runtime();
            self.ensure_meta_pool();
        }

        self.scan_pending_target = pending_target;
        self.maybe_apply_pending_list_load_target();
        self.scan_load_kind = Some(kind);
        self.scan_in_progress = true;
        self.scan_worker_done = false;
        self.scan_started_at = Some(Instant::now());
        self.scan_rx = Some(self.spawn_scan_worker(request, self.skip_dotfiles));
    }

    pub(super) fn start_scan_folder(&mut self, dir: PathBuf) {
        self.start_list_load(
            ScanRequestKind::Folder { root: dir },
            ListLoadKind::Folder,
            true,
            None,
        );
    }

    pub(super) fn start_explicit_file_load(
        &mut self,
        paths: Vec<PathBuf>,
        replace: bool,
        target_kind: Option<PendingListLoadTargetKind>,
        auto_scroll: bool,
    ) {
        let pending_target = target_kind.and_then(|kind| {
            self.resolve_pending_list_load_target(&paths, kind, auto_scroll)
        });
        self.start_list_load(
            ScanRequestKind::Explicit { paths },
            ListLoadKind::Files,
            replace,
            pending_target,
        );
    }

    fn maybe_apply_pending_list_load_target(&mut self) -> bool {
        let Some(target) = self.scan_pending_target.clone() else {
            return false;
        };
        let applied = match target.kind {
            PendingListLoadTargetKind::Select => {
                self.select_loaded_target_path(&target.path, target.auto_scroll)
            }
            PendingListLoadTargetKind::OpenEditor => {
                self.open_loaded_target_in_editor(&target.path, target.auto_scroll)
            }
        };
        if applied {
            self.scan_pending_target = None;
        }
        applied
    }

    fn finalize_list_load(&mut self) {
        self.maybe_apply_pending_list_load_target();
        self.scan_rx = None;
        self.scan_pending_batches.clear();
        self.scan_in_progress = false;
        self.scan_worker_done = false;
        self.scan_started_at = None;
        self.scan_visited_count = self.scan_visited_count.max(self.items.len());
        self.scan_found_count = self.scan_found_count.max(self.items.len());
        self.scan_load_kind = None;
        self.scan_pending_target = None;
        if !self.external_sources.is_empty() {
            self.apply_external_mapping();
        }
        self.apply_filter_from_search();
        self.apply_sort();
    }

    pub(super) fn append_scanned_paths(&mut self, batch: Vec<PathBuf>) {
        if batch.is_empty() {
            return;
        }
        let has_search = !self.search_query.trim().is_empty();
        let query = self.search_query.to_lowercase();
        self.items.reserve(batch.len());
        if !has_search {
            self.files.reserve(batch.len());
            self.original_files.reserve(batch.len());
        }
        for p in batch {
            if self.path_index.contains_key(&p) {
                continue;
            }
            let item = self.make_media_item(p.clone());
            let id = item.id;
            self.path_index.insert(p.clone(), id);
            self.item_index.insert(id, self.items.len());
            self.items.push(item);
            if !has_search {
                self.files.push(id);
                self.original_files.push(id);
            } else {
                let name = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let parent = p
                    .parent()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let matches = name.contains(&query) || parent.contains(&query);
                if matches {
                    self.files.push(id);
                    self.original_files.push(id);
                }
            }
        }
    }

    pub(super) fn process_scan_messages(&mut self) {
        if self.scan_rx.is_none() && self.scan_pending_batches.is_empty() && !self.scan_worker_done {
            return;
        }

        let start = Instant::now();
        let budget = Duration::from_millis(3);

        loop {
            if start.elapsed() >= budget {
                break;
            }
            let next = {
                let Some(rx) = &self.scan_rx else {
                    break;
                };
                rx.try_recv()
            };
            match next {
                Ok(ScanMessage::Batch(batch)) => self.scan_pending_batches.push_back(batch),
                Ok(ScanMessage::Progress { visited, matched }) => {
                    self.scan_visited_count = self.scan_visited_count.max(visited);
                    self.scan_found_count = self.scan_found_count.max(matched);
                }
                Ok(ScanMessage::Done) => {
                    self.scan_rx = None;
                    self.scan_worker_done = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.scan_rx = None;
                    self.scan_worker_done = true;
                    break;
                }
            }
        }

        while start.elapsed() < budget {
            let Some(batch) = self.scan_pending_batches.pop_front() else {
                break;
            };
            self.append_scanned_paths(batch);
            self.maybe_apply_pending_list_load_target();
        }

        if self.scan_worker_done && self.scan_pending_batches.is_empty() {
            self.finalize_list_load();
        }
    }

    pub(super) fn clear_scan_state(&mut self) {
        self.clear_list_load_runtime();
    }

    pub(super) fn topbar_scan_activity_text(&self) -> Option<String> {
        if !self.scan_in_progress {
            return None;
        }
        let elapsed = self
            .scan_started_at
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);
        let label = match self.scan_load_kind.unwrap_or(ListLoadKind::Folder) {
            ListLoadKind::Folder => "Scanning folder",
            ListLoadKind::Files => "Loading files",
        };
        if self.scan_visited_count > 0 {
            Some(format!(
                "{label}: {} files / {} entries ({elapsed:.1}s)",
                self.scan_found_count, self.scan_visited_count
            ))
        } else {
            Some(format!(
                "{label}: {} files ({elapsed:.1}s)",
                self.scan_found_count
            ))
        }
    }
}
