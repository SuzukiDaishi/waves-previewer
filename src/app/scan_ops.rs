use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::types::ScanMessage;
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn start_scan_folder(&mut self, dir: PathBuf) {
        self.scan_rx = Some(self.spawn_scan_worker(dir, self.skip_dotfiles));
        self.scan_in_progress = true;
        self.scan_started_at = Some(Instant::now());
        self.scan_found_count = 0;
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.files.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
        self.selected = None;
        self.selected_multi.clear();
        self.select_anchor = None;
        self.reset_meta_pool();
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
        let mut added = 0usize;
        for p in batch {
            if self.path_index.contains_key(&p) {
                continue;
            }
            let item = self.make_media_item(p.clone());
            let id = item.id;
            self.path_index.insert(p.clone(), id);
            self.item_index.insert(id, self.items.len());
            self.items.push(item);
            added += 1;
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
        if added > 0 {
            self.scan_found_count = self.scan_found_count.saturating_add(added);
        }
    }

    pub(super) fn process_scan_messages(&mut self) {
        let Some(rx) = &self.scan_rx else {
            return;
        };
        let mut done = false;
        let mut batches: Vec<Vec<PathBuf>> = Vec::new();
        let start = Instant::now();
        let budget = Duration::from_millis(3);
        loop {
            if start.elapsed() >= budget {
                break;
            }
            match rx.try_recv() {
                Ok(ScanMessage::Batch(batch)) => batches.push(batch),
                Ok(ScanMessage::Done) => {
                    done = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }

        for batch in batches {
            self.append_scanned_paths(batch);
        }

        if done {
            self.scan_rx = None;
            self.scan_in_progress = false;
            self.scan_started_at = None;
            if !self.external_sources.is_empty() {
                self.apply_external_mapping();
            }
            self.apply_filter_from_search();
            self.apply_sort();
        }
    }
}
