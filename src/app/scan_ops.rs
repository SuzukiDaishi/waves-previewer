use std::path::PathBuf;
use std::time::Instant;

use super::types::{
    ListLoadKind, PendingListLoadTarget, PendingListLoadTargetKind, ScanMessage, ScanPendingBatch,
    ScanRequestKind,
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
        self.scan_found_live = None;
        self.clear_list_seek_runtime();
    }

    /// Dropping a million MediaItems (plus the path/index maps) frees ~1GB
    /// of heap; doing it inline stalled the UI for hundreds of ms when
    /// loading a new folder over an existing large list. Hand the old
    /// contents to a low-priority thread instead.
    pub(super) fn drop_list_contents_in_background(&mut self) {
        let items = std::mem::take(&mut self.items);
        let path_index = std::mem::take(&mut self.path_index);
        let item_index = std::mem::take(&mut self.item_index);
        let files = std::mem::take(&mut self.files);
        let original_files = std::mem::take(&mut self.original_files);
        let folder_intern = std::mem::take(&mut self.folder_intern);
        if items.len() > 50_000 {
            let retired = std::sync::Arc::clone(&self.retired_list_drops);
            retired.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let retired_for_worker = std::sync::Arc::clone(&retired);
            let spawned = std::thread::Builder::new()
                .name("neowaves-retired-list-drop".to_string())
                .spawn(move || {
                    crate::app::threading::lower_current_thread_priority();
                    drop((
                        items,
                        path_index,
                        item_index,
                        files,
                        original_files,
                        folder_intern,
                    ));
                    retired_for_worker.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                    crate::ui_wake::wake_ui();
                });
            if spawned.is_err() {
                retired.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    fn reset_list_contents_for_folder_load(&mut self) {
        self.clear_list_load_runtime();
        self.note_files_membership_changed();
        self.drop_list_contents_in_background();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.transcript_ai_inflight.clear();
        self.cancel_list_preview_job();
        self.list_preview_pending_path = None;
        self.list_preview_prefetch_tx = None;
        self.list_preview_prefetch_rx = None;
        self.list_preview_prefetch_inflight.clear();
        self.clear_list_preview_cache();
        self.clear_list_art_texture_cache();
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
        self.list_max_duration_secs = 0.0;
        self.reset_meta_pool();
    }

    fn reset_list_contents_for_explicit_replace(&mut self) {
        self.note_files_membership_changed();
        self.root = None;
        self.clear_list_load_runtime();
        self.drop_list_contents_in_background();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.transcript_ai_inflight.clear();
        self.clear_list_art_texture_cache();
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
        self.list_max_duration_secs = 0.0;
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
            // The root is set by now, so the pool about to be built is
            // sized for the right kind of storage.
            self.refresh_root_locality();
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
        let pending_target = target_kind
            .and_then(|kind| self.resolve_pending_list_load_target(&paths, kind, auto_scroll));
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
        if self.search_query.trim().is_empty() {
            // files/original_files were maintained incrementally during the
            // scan; re-collecting 1M ids here just stalled the finish frame.
            self.note_files_membership_changed();
            if self.sort_dir != super::types::SortDir::None {
                self.request_sort();
            }
        } else {
            self.refresh_filter_then_sort();
        }
    }

    /// Files the walker has found so far, including the ones still queued in
    /// `scan_pending_batches`. The atomic runs ahead of the batched `Progress`
    /// messages, so it is the live number; `scan_found_count` is the last
    /// message received. Take whichever is further along.
    pub(super) fn scan_discovered_count(&self) -> usize {
        let live = self
            .scan_found_live
            .as_ref()
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0);
        self.scan_found_count.max(live)
    }

    /// Apply part of one scanner message. Returns true once the batch is
    /// exhausted. Deadline checks happen between individual rows, so neither
    /// a high-tier batch nor an unexpectedly expensive allocation owns a
    /// complete UI frame.
    fn append_scanned_paths_until(
        &mut self,
        batch: &mut ScanPendingBatch,
        started: Instant,
        budget: std::time::Duration,
    ) -> bool {
        if batch.is_done() {
            return true;
        }
        self.note_files_membership_changed();
        let has_search = !self.search_query.trim().is_empty();
        let query = self.search_query.to_lowercase();
        while batch.next < batch.paths.len() {
            if batch.next > 0 && started.elapsed() >= budget {
                break;
            }
            let p = std::mem::take(&mut batch.paths[batch.next]);
            batch.next += 1;
            if self.path_index.contains_key(&p) {
                continue;
            }
            let item = self.make_media_item(p.clone());
            let id = item.id;
            let row = self.items.len();
            if self.items.try_push(item).is_err() {
                self.debug_log("scan stopped: unable to allocate another media-item chunk");
                self.scan_rx = None;
                self.scan_pending_batches.clear();
                self.scan_worker_done = true;
                self.scan_in_progress = false;
                return true;
            }
            self.path_index.insert(p.clone(), id);
            self.item_index.insert(id, row);
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
        batch.is_done()
    }

    pub(super) fn process_scan_messages(&mut self) {
        if self.scan_rx.is_none() && self.scan_pending_batches.is_empty() && !self.scan_worker_done
        {
            return;
        }
        if self.perf.memory_pressure == super::perf_profile::MemoryPressure::Critical
            && self
                .retired_list_drops
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
        {
            // The bounded scanner channel now supplies backpressure while
            // the previous large list is actually being freed.
            return;
        }

        let start = Instant::now();
        // Keep directory ingestion moving during playback, but do not let
        // PathBuf allocation/hash-map growth consume an audio/UI frame. The
        // size comes from the machine tier (see perf_profile.rs): the same
        // slice that is nothing on a workstation is a dropped frame on a
        // two-core laptop.
        let budget = self
            .perf
            .list_append_budget(self.playback_is_playing_now() || self.playback_session.is_playing);

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
                Ok(ScanMessage::Batch(batch)) => {
                    self.scan_pending_batches
                        .push_back(ScanPendingBatch::new(batch));
                    self.scan_pending_peak =
                        self.scan_pending_peak.max(self.scan_pending_batches.len());
                }
                Ok(ScanMessage::Progress {
                    visited,
                    matched,
                    io_sample_micros,
                }) => {
                    self.scan_visited_count = self.scan_visited_count.max(visited);
                    self.scan_found_count = self.scan_found_count.max(matched);
                    if let Some(micros) = io_sample_micros {
                        self.perf
                            .note_io_latency(std::time::Duration::from_micros(micros));
                    }
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
            let Some(mut batch) = self.scan_pending_batches.pop_front() else {
                break;
            };
            let done = self.append_scanned_paths_until(&mut batch, start, budget);
            self.maybe_apply_pending_list_load_target();
            if !done {
                self.scan_pending_batches.push_front(batch);
                break;
            }
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
