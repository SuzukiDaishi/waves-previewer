use std::path::PathBuf;

use super::{meta, transcript};

/// How much metadata a visible list row may ask for right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListMetaDetail {
    /// Header fields only: no decode, no waveform thumb, no loudness. Enough
    /// for a row to show its name, length and format.
    HeaderOnly,
    /// Header plus the decode that fills `FileMeta::thumb` and the loudness
    /// columns.
    Thumb,
}

/// Pure policy behind `list_meta_detail_now`.
///
/// While the folder scan is still appending rows, listing every file outranks
/// drawing waveforms for the screenful that happens to be visible: the thumb
/// needs a whole-file decode (`MetaTask::Header` does the header pass and that
/// decode together, `MetaTask::Decode` the decode alone), and those reads
/// compete with the walker for the same disk and the same worker pool.
/// `MetaTask::HeaderOnly` skips the decode and still resolves the name, length,
/// format and duration a row needs. Filenames first, thumbs once the list is
/// complete.
///
/// The second condition is unchanged from the `large_bg_list` test this
/// replaces, which was spelled out separately in the row loop and in the
/// prefetch pass.
pub(crate) fn list_meta_detail_for(
    files_len: usize,
    bg_mode_needs_meta: bool,
    scan_in_progress: bool,
) -> ListMetaDetail {
    if scan_in_progress
        || (bg_mode_needs_meta && files_len >= crate::app::LIST_BG_META_LARGE_THRESHOLD)
    {
        ListMetaDetail::HeaderOnly
    } else {
        ListMetaDetail::Thumb
    }
}

impl super::WavesPreviewer {
    pub(crate) fn list_meta_detail_now(&self) -> ListMetaDetail {
        list_meta_detail_for(
            self.files.len(),
            self.item_bg_mode != crate::app::types::ItemBgMode::Standard,
            self.scan_in_progress,
        )
    }

    pub(crate) fn list_meta_detail_is_header_only(&self) -> bool {
        self.list_meta_detail_now() == ListMetaDetail::HeaderOnly
    }

    /// Queue whatever depth of metadata the list is allowed to ask for now.
    pub(super) fn queue_list_meta_for_path(&mut self, path: &PathBuf, priority: bool) {
        match self.list_meta_detail_now() {
            ListMetaDetail::HeaderOnly => self.queue_header_meta_for_path(path, priority),
            ListMetaDetail::Thumb => self.queue_meta_for_path(path, priority),
        }
    }

    /// Debounce for re-sorting while metadata streams in. A full decorate +
    /// sort of a 100k+ item list costs tens of ms (hundreds at 500k), so
    /// large lists re-sort less often, and the interval additionally scales
    /// with the measured cost of the previous sort so the UI thread never
    /// spends the majority of its time re-sorting.
    fn meta_sort_min_interval_ms(&self) -> u64 {
        let base = if self.files.len() > 20_000 {
            750
        } else {
            crate::app::META_SORT_MIN_INTERVAL_MS
        };
        let adaptive = (self.sort_loading_last_ms as u64).saturating_mul(8);
        base.max(adaptive.min(8_000))
    }

    /// Whether the current sort key can only be resolved by decoding the
    /// whole file. Header metadata (duration, channels, SR, bits, bitrate,
    /// BPM tag, file times) covers every other key.
    fn sort_key_needs_full_decode(&self) -> bool {
        matches!(
            self.sort_key,
            crate::app::types::SortKey::Level
                | crate::app::types::SortKey::Lufs
                | crate::app::types::SortKey::TruePeak
                | crate::app::types::SortKey::LufsShort
                | crate::app::types::SortKey::LufsMomentary
                | crate::app::types::SortKey::SilenceLead
                | crate::app::types::SortKey::SilenceTail
                | crate::app::types::SortKey::EdgeZero
                | crate::app::types::SortKey::OverPeak
                | crate::app::types::SortKey::BlankPad
        )
    }

    pub(super) fn prime_sort_metadata_prefetch(&mut self) {
        // Do NOT mass-enqueue metadata jobs here: on very large lists (100k+)
        // queueing one decode task per row stalls the app for the lifetime of
        // the backlog (a single header click used to queue 500k full decodes).
        // Reset the prefetch cursor instead and let `pump_list_meta_prefetch`
        // stream tasks each frame under its queue budget and inflight cap.
        self.list_meta_prefetch_cursor = 0;
    }

    pub(super) fn reset_meta_pool(&mut self) {
        // Reserve one core for the UI/audio threads; the workers also run at
        // lowered OS priority (see spawn_meta_pool). A two-core machine gets
        // exactly one worker (see PerfProfile).
        let workers = self.perf.meta_pool_workers();
        let (pool, rx) = crate::app::meta::spawn_meta_pool(workers);
        // Only pool construction site, so a pool recreated mid-session keeps
        // measuring Blank Pad at the user's threshold rather than the default.
        pool.set_blank_threshold_dbfs(self.blank_threshold_dbfs);
        self.meta_pool = Some(pool);
        self.meta_rx = Some(rx);
        self.meta_inflight.clear();
        self.meta_sort_pending = false;
        self.meta_sort_last_applied = None;
        self.list_meta_prefetch_cursor = 0;
        self.transcript_inflight.clear();
        self.transcript_ai_inflight.clear();
    }

    /// Publish the Blank Pad threshold to an already-running pool. Rows whose
    /// measurement was taken at the old threshold detect it as stale on the
    /// next frame and re-queue themselves, so no bulk invalidation is needed.
    pub(super) fn push_blank_threshold_to_meta_pool(&mut self) {
        if let Some(pool) = self.meta_pool.as_ref() {
            pool.set_blank_threshold_dbfs(self.blank_threshold_dbfs);
        }
        // Edited rows display from `edited_cache`, which no decode job ever
        // revisits — leaving them stale forever. Their samples are already in
        // memory, so rescan directly instead.
        let threshold = self.blank_threshold_dbfs;
        for cached in self.edited_cache.values_mut() {
            let sr = cached.buffer_sample_rate.max(1);
            if let Some(meta) = cached.display_meta.as_mut() {
                meta.blank_pad = Some(crate::app::inspection::scan_blank_pad(
                    &cached.ch_samples,
                    sr,
                    threshold,
                ));
            }
        }
    }

    pub(super) fn ensure_meta_pool(&mut self) {
        if self.meta_pool.is_none() {
            self.reset_meta_pool();
        }
    }

    /// Note whether the list now lives on a network share, and resize the
    /// metadata pool if that changed. Called whenever the list is replaced
    /// (folder scan, session open, explicit file load) -- the pool's worker
    /// count comes from this, and a share cannot serve local-disk
    /// concurrency without starving whatever the user opened.
    pub(super) fn refresh_root_locality(&mut self) {
        // Judge by an actual item rather than the root: a session has no
        // root at all, and its files are what the workers will be reading.
        let sample = self
            .root
            .clone()
            .or_else(|| self.items.first().map(|item| item.path.clone()));
        let remote = sample
            .as_deref()
            .map(crate::audio_io::is_remote_file_path)
            .unwrap_or(false);
        if self.perf.set_remote_root(remote) {
            self.debug_log(format!(
                "list root locality: {}",
                if remote { "remote" } else { "local" }
            ));
            if self.meta_pool.is_some() {
                self.reset_meta_pool();
            }
        }
    }

    /// Cancel any queued or running metadata job for a path (e.g. when the
    /// file is removed from the list) and clear the inflight marker so the
    /// row can be re-requested later if it comes back.
    pub(super) fn cancel_meta_for_path(&mut self, path: &std::path::Path) {
        if let Some(pool) = &self.meta_pool {
            // Queued tasks are dropped silently (no update will arrive);
            // running tasks send MetaUpdate::Cancelled, which is a harmless
            // second remove after the one below.
            let _ = pool.cancel_path(path);
        }
        self.meta_inflight.remove(path);
    }

    /// Backlog guard for large lists: a row that is (or was) visible enqueues
    /// a decode task every frame it stays unresolved, and fast scrolling used
    /// to accumulate an unbounded queue of full decodes. Visible rows keep
    /// re-requesting while on screen, so rejecting new tasks at the cap is
    /// self-healing once the backlog drains.
    fn meta_backlog_full(&self, priority: bool) -> bool {
        if self.files.len() < crate::app::LIST_BG_META_LARGE_THRESHOLD {
            return false;
        }
        let cap = if priority {
            crate::app::LIST_BG_META_INFLIGHT_LIMIT.saturating_mul(4)
        } else {
            crate::app::LIST_BG_META_INFLIGHT_LIMIT.saturating_mul(2)
        };
        // These limits were tuned against a local disk. A share cannot serve
        // that many concurrent reads without starving whatever the user is
        // waiting on, so scale them down when the root is remote.
        self.meta_inflight.len() >= self.perf.background_io_budget(cap)
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
            if self.meta_backlog_full(priority) {
                return;
            }
            self.meta_inflight.insert(path.clone());
            self.debug.meta_task_header_count = self.debug.meta_task_header_count.saturating_add(1);
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
        self.ensure_meta_pool();
        if let Some(pool) = &self.meta_pool {
            if self.meta_inflight.contains(path) {
                if priority {
                    pool.promote_path(path);
                }
                return;
            }
            if self.meta_backlog_full(priority) {
                return;
            }
            self.meta_inflight.insert(path.clone());
            self.debug.meta_task_header_only_count =
                self.debug.meta_task_header_only_count.saturating_add(1);
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
        self.ensure_meta_pool();
        if let Some(pool) = &self.meta_pool {
            if self.meta_inflight.contains(path) {
                if priority {
                    pool.promote_path(path);
                }
                return;
            }
            if self.meta_backlog_full(priority) {
                return;
            }
            self.meta_inflight.insert(path.clone());
            self.debug.meta_task_decode_count = self.debug.meta_task_decode_count.saturating_add(1);
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
        let sort_meta_prefetch = self.sort_key_uses_meta();
        let sort_transcript_prefetch = self.sort_key_uses_transcript();
        let need_prefetch = self.item_bg_mode != crate::app::types::ItemBgMode::Standard
            || sort_meta_prefetch
            || sort_transcript_prefetch;
        if !self.is_list_workspace_active() || self.files.is_empty() || !need_prefetch {
            self.list_meta_prefetch_cursor = 0;
            return;
        }
        if !sort_meta_prefetch
            && !sort_transcript_prefetch
            && self.files.len() >= crate::app::LIST_BG_META_LARGE_THRESHOLD
        {
            // Visible-window prefetch in `ui/list.rs` is enough for very large lists.
            self.list_meta_prefetch_cursor = 0;
            return;
        }
        let total = self.files.len();
        self.list_meta_prefetch_cursor %= total;
        let queue_budget =
            self.perf
                .background_io_budget(if sort_meta_prefetch || sort_transcript_prefetch {
                    crate::app::LIST_META_PREFETCH_BUDGET.saturating_mul(4)
                } else {
                    crate::app::LIST_META_PREFETCH_BUDGET
                });
        let inflight_cap =
            self.perf
                .background_io_budget(if sort_meta_prefetch || sort_transcript_prefetch {
                    crate::app::LIST_BG_META_INFLIGHT_LIMIT.saturating_mul(2)
                } else {
                    crate::app::LIST_BG_META_INFLIGHT_LIMIT
                });
        // Bound the per-frame walk as well: once most rows are resolved this
        // loop is a pure scan, and walking all 500k rows every frame costs
        // tens of ms. The cursor keeps advancing, so coverage is unchanged.
        let scan_budget = total.min(crate::app::LIST_META_PREFETCH_SCAN_BUDGET);
        let sort_needs_decode = self.sort_key_needs_full_decode();
        let mut scanned = 0usize;
        let mut queued = 0usize;
        while scanned < scan_budget && queued < queue_budget {
            if self.meta_inflight.len() >= inflight_cap {
                break;
            }
            let idx = (self.list_meta_prefetch_cursor + scanned) % total;
            scanned += 1;
            let Some(path) = self.path_for_row(idx).cloned() else {
                continue;
            };
            if self.is_virtual_path(&path) {
                continue;
            }
            if sort_meta_prefetch && !self.meta_inflight.contains(&path) {
                let meta = self.meta_for_path(&path);
                if sort_needs_decode {
                    let full_meta_attempted = meta
                        .map(|m| {
                            m.rms_db.is_some()
                                || m.lufs_i.is_some()
                                || m.decode_error.is_some()
                                || !m.thumb.is_empty()
                        })
                        .unwrap_or(false);
                    if meta.is_none() {
                        self.queue_meta_for_path(&path, false);
                        queued += 1;
                    } else if !full_meta_attempted {
                        // Level/LUFS sort: force one full decode pass so
                        // unknown values are resolved.
                        self.queue_full_meta_for_path(&path, false);
                        queued += 1;
                    }
                } else if meta.is_none() {
                    // Every other sort key is available from the header;
                    // don't spend a full decode per row on it.
                    self.queue_header_meta_for_path(&path, false);
                    queued += 1;
                } else if matches!(self.sort_key, crate::app::types::SortKey::Length)
                    && meta.is_some_and(|m| {
                        m.duration_secs.is_none()
                            && m.decode_error.is_none()
                            && m.rms_db.is_none()
                            && m.thumb.is_empty()
                    })
                {
                    // Rare formats whose header cannot resolve a duration:
                    // fall back to one decode pass for a stable Length key.
                    self.queue_full_meta_for_path(&path, false);
                    queued += 1;
                }
            }
            if sort_transcript_prefetch && !self.transcript_inflight.contains(&path) {
                if self.transcript_for_path(&path).is_none() {
                    self.queue_transcript_for_path(&path, false);
                    queued += 1;
                }
            }
            if !sort_meta_prefetch && !sort_transcript_prefetch {
                if self.meta_for_path(&path).is_some() || self.meta_inflight.contains(&path) {
                    continue;
                }
                self.queue_meta_for_path(&path, false);
                queued += 1;
            }
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
                        >= std::time::Duration::from_millis(self.meta_sort_min_interval_ms())
                })
                .unwrap_or(true);
        if due {
            if self.sort_job_active() {
                // An async sort is already running; let it finish and pick up
                // the accumulated changes on the next interval.
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            self.request_sort();
            self.meta_sort_pending = false;
            self.meta_sort_last_applied = Some(now);
            ctx.request_repaint();
        } else {
            // Wake up again exactly when the next sort window opens instead of
            // forcing max-rate repaints until then.
            let elapsed = self
                .meta_sort_last_applied
                .map(|last| now.duration_since(last))
                .unwrap_or_default();
            let interval = std::time::Duration::from_millis(self.meta_sort_min_interval_ms());
            ctx.request_repaint_after(interval.saturating_sub(elapsed));
        }
    }

    pub(super) fn drain_meta_updates(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.meta_rx else {
            return;
        };
        let mut updates: Vec<meta::MetaUpdate> = Vec::new();
        let mut drained = 0usize;
        let playback_guard = self.playback_is_playing_now() || self.playback_session.is_playing;
        let update_budget = if playback_guard {
            16
        } else {
            crate::app::META_UPDATE_FRAME_BUDGET
        };
        let time_budget_micros = if playback_guard { 250 } else { 1_000 };
        // Cap by count AND wall time: applying an update allocates (meta box,
        // art eviction), and a deep backlog must not own the frame.
        let drain_started = std::time::Instant::now();
        while drained < update_budget {
            match rx.try_recv() {
                Ok(update) => {
                    updates.push(update);
                    drained += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
            if drained % 8 == 0 && drain_started.elapsed().as_micros() > time_budget_micros {
                break;
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
                    self.update_loudnorm_progress_for_path(&p);
                }
                meta::MetaUpdate::Full(p, m) => {
                    self.meta_inflight.remove(&p);
                    if self.set_meta_for_path(&p, m) {
                        meta_sort_dirty = true;
                    }
                    self.update_csv_export_progress_for_path(&p);
                    self.update_loudnorm_progress_for_path(&p);
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
                meta::MetaUpdate::Cancelled(p) => {
                    self.meta_inflight.remove(&p);
                }
            }
        }
        if refilter {
            // Transcripts can stream in once per frame for thousands of files;
            // debounce the O(n) refilter + sort instead of running them inline.
            // Keep the existing deadline so a steady stream cannot starve it.
            self.search_dirty = true;
            if self.search_deadline.is_none() {
                // Match the sort debounce: on huge lists each refilter walks
                // every item, so let transcripts accumulate longer per pass.
                let debounce_ms = self.meta_sort_min_interval_ms().max(300);
                self.search_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(debounce_ms));
            }
            ctx.request_repaint();
        } else {
            if meta_sort_dirty && self.sort_key_uses_meta() {
                self.meta_sort_pending = true;
            }
            if transcript_sort_dirty {
                self.meta_sort_pending = true;
            }
            self.flush_pending_meta_sort(ctx, false);
            // Metadata streaming does not need 60fps repaints; 15fps keeps the
            // list visibly filling in while leaving CPU to the workers.
            ctx.request_repaint_after(std::time::Duration::from_millis(66));
        }
        if drained >= update_budget {
            // Avoid a stall by continuing to consume backlog in future frames.
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{list_meta_detail_for, ListMetaDetail};
    use crate::app::LIST_BG_META_LARGE_THRESHOLD;

    /// The user's stated priority: knowing how many files are loaded matters
    /// more than seeing waveforms. While the scan runs, no row may ask for the
    /// decode that draws one, at any list size or background mode.
    #[test]
    fn a_running_scan_always_withholds_the_thumb() {
        for files_len in [0usize, 1, 100, LIST_BG_META_LARGE_THRESHOLD, 1_000_000] {
            for bg_needs_meta in [false, true] {
                assert_eq!(
                    list_meta_detail_for(files_len, bg_needs_meta, true),
                    ListMetaDetail::HeaderOnly,
                    "files_len={files_len} bg={bg_needs_meta}"
                );
            }
        }
    }

    /// Once the scan is done the policy must be exactly what it was before this
    /// change: header-only only for a large list in a metadata-driven
    /// background mode. Anything else would be an unrequested behaviour change.
    #[test]
    fn the_idle_policy_matches_the_previous_large_bg_list_test() {
        for files_len in [
            0usize,
            LIST_BG_META_LARGE_THRESHOLD - 1,
            LIST_BG_META_LARGE_THRESHOLD,
            LIST_BG_META_LARGE_THRESHOLD + 1,
        ] {
            for bg_needs_meta in [false, true] {
                let legacy_large_bg_list =
                    bg_needs_meta && files_len >= LIST_BG_META_LARGE_THRESHOLD;
                let expected = if legacy_large_bg_list {
                    ListMetaDetail::HeaderOnly
                } else {
                    ListMetaDetail::Thumb
                };
                assert_eq!(
                    list_meta_detail_for(files_len, bg_needs_meta, false),
                    expected,
                    "files_len={files_len} bg={bg_needs_meta}"
                );
            }
        }
    }

    #[test]
    fn a_small_idle_list_in_standard_mode_gets_thumbs() {
        assert_eq!(
            list_meta_detail_for(200, false, false),
            ListMetaDetail::Thumb
        );
    }
}
