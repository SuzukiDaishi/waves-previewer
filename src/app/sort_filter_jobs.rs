//! Asynchronous sort / filter jobs for very large lists.
//!
//! A full decorate + sort of the list is O(n log n) and used to run
//! synchronously on the UI thread (hundreds of ms at 500k rows, seconds at
//! 1M). Here the decorate stage is sliced across frames under a small time
//! budget, the comparison sort runs on a worker thread, and the finished
//! order is adopted only if the list membership has not changed in the
//! meantime. Small lists keep the synchronous path (lower latency, simpler
//! test semantics).

use std::cmp::Ordering;
use std::time::UNIX_EPOCH;

use super::types::{MediaId, MediaItem, SortDir, SortKey};
use super::WavesPreviewer;

/// Lists at or below this size sort/filter synchronously in one frame.
pub(super) const LIST_JOB_SYNC_THRESHOLD: usize = 50_000;
/// Per-frame time budget for the sliced decorate / filter passes.
pub(super) const LIST_JOB_FRAME_BUDGET_MS: f64 = 2.0;

/// Owned sort key so the comparison sort can run off the UI thread.
pub(super) enum OwnedKey {
    Str(String),
    Num(Option<f64>),
    Missing,
}

pub(super) struct SortBuildJob {
    pub request_id: u64,
    pub dir: SortDir,
    /// Snapshot of the row order to sort (ids resolve items by stable id).
    pub ids: Vec<MediaId>,
    pub cursor: usize,
    pub decorated: Vec<(OwnedKey, String, MediaId)>,
    pub membership_revision: u64,
    pub selected_id: Option<MediaId>,
    pub started_at: std::time::Instant,
}

pub(super) struct SortResult {
    pub request_id: u64,
    pub sorted: Vec<MediaId>,
    pub membership_revision: u64,
    pub selected_id: Option<MediaId>,
    pub started_at: std::time::Instant,
}

pub(super) struct FilterJob {
    pub cursor: usize,
    pub matched: Vec<MediaId>,
    pub query_lower: String,
    pub regex: Option<regex::Regex>,
    pub membership_revision: u64,
    pub selected_id: Option<MediaId>,
}

impl WavesPreviewer {
    pub(super) fn note_files_membership_changed(&mut self) {
        self.files_membership_revision = self.files_membership_revision.wrapping_add(1);
    }

    /// Extract the sort key for one row as an owned value.
    pub(super) fn owned_sort_key(&self, item: &MediaItem, key: SortKey) -> OwnedKey {
        let m = item.meta.as_ref();
        match key {
            SortKey::File => OwnedKey::Str(item.display_name.clone()),
            SortKey::Folder => OwnedKey::Str(item.display_folder.to_string()),
            SortKey::Transcript => OwnedKey::Str(
                item.transcript
                    .as_ref()
                    .map(|t| t.full_text.clone())
                    .unwrap_or_default(),
            ),
            SortKey::Type => OwnedKey::Str(Self::list_type_sort_key(item).into_owned()),
            SortKey::Length => OwnedKey::Num(
                m.and_then(|m| m.duration_secs)
                    .filter(|v| v.is_finite())
                    .map(|v| v as f64),
            ),
            SortKey::Channels => {
                OwnedKey::Num(m.map(|m| m.channels as f64).filter(|v| *v > 0.0))
            }
            SortKey::SampleRate => OwnedKey::Num(
                self.sample_rate_override
                    .get(&item.path)
                    .copied()
                    .or_else(|| m.map(|m| m.sample_rate))
                    .filter(|v| *v > 0)
                    .map(|v| v as f64),
            ),
            SortKey::Bits => OwnedKey::Num(
                self.bit_depth_override
                    .get(&item.path)
                    .copied()
                    .map(|v| v.bits_per_sample())
                    .or_else(|| m.map(|m| m.bits_per_sample))
                    .filter(|v| *v > 0)
                    .map(|v| v as f64),
            ),
            SortKey::BitRate => OwnedKey::Num(
                m.and_then(|m| m.bit_rate_bps)
                    .map(|v| v as f64)
                    .filter(|v| *v > 0.0),
            ),
            SortKey::Level => OwnedKey::Num(
                m.and_then(|m| m.peak_db)
                    .filter(|v| v.is_finite())
                    .map(|v| v as f64),
            ),
            // LUFS sorting uses effective value: override if present, else base + gain.
            SortKey::Lufs => {
                let v = if let Some(v) = self.lufs_override.get(&item.path).copied() {
                    v
                } else {
                    m.and_then(|m| m.lufs_i.map(|x| x + item.pending_gain_db))
                        .unwrap_or(f32::NAN)
                };
                OwnedKey::Num(v.is_finite().then_some(v as f64))
            }
            SortKey::Bpm => OwnedKey::Num(
                m.and_then(|m| m.bpm)
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .map(|v| v as f64),
            ),
            SortKey::CreatedAt => OwnedKey::Num(
                m.and_then(|m| m.created_at)
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64()),
            ),
            SortKey::ModifiedAt => OwnedKey::Num(
                m.and_then(|m| m.modified_at)
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64()),
            ),
            SortKey::External(idx) => match self.external_visible_columns.get(idx) {
                Some(col) => OwnedKey::Str(item.external_value(col).cloned().unwrap_or_default()),
                None => OwnedKey::Missing,
            },
        }
    }

    pub(super) fn compare_decorated_rows(
        a: &(OwnedKey, String, MediaId),
        b: &(OwnedKey, String, MediaId),
        dir: SortDir,
    ) -> Ordering {
        let ord = match (&a.0, &b.0) {
            (OwnedKey::Missing, _) | (_, OwnedKey::Missing) => return Ordering::Equal,
            (OwnedKey::Str(x), OwnedKey::Str(y)) => Self::string_order(x, y, dir),
            (OwnedKey::Num(x), OwnedKey::Num(y)) => Self::option_num_order_f64(*x, *y, dir),
            _ => Ordering::Equal,
        };
        if ord == Ordering::Equal {
            // Equal keys tie-break by display name, then MediaId (scan order);
            // numeric keys tie constantly on big lists so this must stay cheap.
            a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2))
        } else {
            ord
        }
    }

    /// Request a re-sort of the current list. Small lists sort synchronously;
    /// large lists build the sort snapshot over multiple frames and sort on a
    /// worker thread while the UI keeps the old order.
    pub(super) fn request_sort(&mut self) {
        if self.files.len() <= LIST_JOB_SYNC_THRESHOLD || self.sort_dir == SortDir::None {
            // Cancel any stale async job so its result cannot overwrite the
            // fresh synchronous order.
            self.sort_request_seq = self.sort_request_seq.wrapping_add(1);
            self.sort_job = None;
            self.sort_rx = None;
            self.apply_sort();
            return;
        }
        self.sort_request_seq = self.sort_request_seq.wrapping_add(1);
        let selected_id = self.selected.and_then(|i| self.files.get(i).copied());
        self.sort_job = Some(SortBuildJob {
            request_id: self.sort_request_seq,
            dir: self.sort_dir,
            ids: self.files.clone(),
            cursor: 0,
            decorated: Vec::with_capacity(self.files.len()),
            membership_revision: self.files_membership_revision,
            selected_id,
            started_at: std::time::Instant::now(),
        });
        self.sort_rx = None;
        self.sort_loading_started_at = Some(std::time::Instant::now());
    }

    pub(super) fn sort_job_active(&self) -> bool {
        self.sort_job.is_some() || self.sort_rx.is_some()
    }

    /// Slice the decorate stage across frames, then hand the snapshot to a
    /// worker thread for the O(n log n) sort. Returns true while working (the
    /// caller should keep repaints coming).
    pub(super) fn pump_sort_job(&mut self) -> bool {
        let Some(mut job) = self.sort_job.take() else {
            return self.sort_rx.is_some();
        };
        if job.request_id != self.sort_request_seq
            || job.membership_revision != self.files_membership_revision
        {
            // Superseded or the list changed while decorating: restart fresh.
            if job.membership_revision != self.files_membership_revision {
                self.request_sort();
            }
            return true;
        }
        let key = self.sort_key;
        let started = std::time::Instant::now();
        while job.cursor < job.ids.len() {
            if started.elapsed().as_secs_f64() * 1000.0 >= LIST_JOB_FRAME_BUDGET_MS {
                break;
            }
            // Chunk the budget check: elapsed() per row would dominate.
            let end = (job.cursor + 2_048).min(job.ids.len());
            for idx in job.cursor..end {
                let id = job.ids[idx];
                let entry = match self.item_for_id(id) {
                    Some(item) => (
                        self.owned_sort_key(item, key),
                        item.display_name.clone(),
                        id,
                    ),
                    None => (OwnedKey::Missing, String::new(), id),
                };
                job.decorated.push(entry);
            }
            job.cursor = end;
        }
        if job.cursor < job.ids.len() {
            self.sort_job = Some(job);
            return true;
        }
        // Snapshot complete: sort off-thread.
        let (tx, rx) = std::sync::mpsc::channel();
        self.sort_rx = Some(rx);
        let dir = job.dir;
        let request_id = job.request_id;
        let membership_revision = job.membership_revision;
        let selected_id = job.selected_id;
        let started_at = job.started_at;
        let mut decorated = job.decorated;
        std::thread::spawn(move || {
            decorated.sort_unstable_by(|a, b| Self::compare_decorated_rows(a, b, dir));
            let sorted: Vec<MediaId> = decorated.into_iter().map(|e| e.2).collect();
            let _ = tx.send(SortResult {
                request_id,
                sorted,
                membership_revision,
                selected_id,
                started_at,
            });
        });
        true
    }

    pub(super) fn drain_sort_results(&mut self) -> bool {
        let Some(rx) = &self.sort_rx else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(res) => res,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.sort_rx = None;
                self.sort_loading_started_at = None;
                return false;
            }
        };
        self.sort_rx = None;
        if result.request_id != self.sort_request_seq {
            return true;
        }
        if result.membership_revision != self.files_membership_revision {
            // Rows were added/removed while sorting: run again on fresh data.
            self.request_sort();
            return true;
        }
        self.files = result.sorted;
        self.selected = result
            .selected_id
            .and_then(|id| self.files.iter().position(|&x| x == id));
        let elapsed = result.started_at.elapsed();
        self.sort_loading_last_ms = elapsed.as_secs_f32() * 1000.0;
        self.sort_loading_hold_until = Some(
            std::time::Instant::now()
                + std::time::Duration::from_millis(if elapsed.as_millis() >= 120 {
                    900
                } else {
                    500
                }),
        );
        self.sort_loading_started_at = None;
        self.meta_sort_last_applied = Some(std::time::Instant::now());
        true
    }

    // ---- Filter ----

    fn item_matches_query_lower(item: &MediaItem, query_lower: &str) -> bool {
        if item.display_name.to_lowercase().contains(query_lower) {
            return true;
        }
        if item.display_folder.to_lowercase().contains(query_lower) {
            return true;
        }
        if let Some(t) = item.transcript.as_ref() {
            if t.full_text.to_lowercase().contains(query_lower) {
                return true;
            }
        }
        if let Some(m) = item.meta.as_ref() {
            let summary = Self::meta_search_summary(m);
            if summary.contains(query_lower) {
                return true;
            }
        }
        if let Some(ext) = item.external.as_ref() {
            if ext.values().any(|v| v.to_lowercase().contains(query_lower)) {
                return true;
            }
        }
        false
    }

    fn item_matches_regex(item: &MediaItem, re: &regex::Regex) -> bool {
        if re.is_match(&item.display_name) || re.is_match(&item.display_folder) {
            return true;
        }
        if let Some(t) = item.transcript.as_ref() {
            if re.is_match(&t.full_text) {
                return true;
            }
        }
        if let Some(m) = item.meta.as_ref() {
            if re.is_match(&Self::meta_search_summary(m)) {
                return true;
            }
        }
        if let Some(ext) = item.external.as_ref() {
            if ext.values().any(|v| re.is_match(v)) {
                return true;
            }
        }
        false
    }

    pub(super) fn item_matches_filter(
        item: &MediaItem,
        query_lower: &str,
        regex: Option<&regex::Regex>,
    ) -> bool {
        match regex {
            Some(re) => Self::item_matches_regex(item, re),
            None => Self::item_matches_query_lower(item, query_lower),
        }
    }

    /// Searchable one-line summary of a row's metadata (lowercase by
    /// construction). Built on demand; storing it per item cost ~60 heap
    /// bytes x 1M rows and a rebuild on every metadata update.
    pub(super) fn meta_search_summary(m: &super::types::FileMeta) -> String {
        format!(
            "sr:{} bits:{} br:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1} bpm:{:.1}",
            m.sample_rate,
            m.bits_per_sample,
            m.bit_rate_bps.unwrap_or(0),
            m.channels,
            m.duration_secs.unwrap_or(0.0),
            m.peak_db.unwrap_or(0.0),
            m.lufs_i.unwrap_or(0.0),
            m.bpm.unwrap_or(0.0)
        )
    }

    pub(super) fn filter_job_active(&self) -> bool {
        self.filter_job.is_some()
    }

    /// Apply the current search box state to `files`. Small lists filter
    /// synchronously; large lists filter in per-frame slices and adopt the
    /// result (then re-sort) when done.
    pub(super) fn refresh_filter_then_sort(&mut self) {
        let query = self.search_query.trim().to_string();
        if query.is_empty() || self.items.len() <= LIST_JOB_SYNC_THRESHOLD {
            self.filter_job = None;
            self.apply_filter_from_search();
            if self.sort_dir != SortDir::None {
                self.request_sort();
            }
            return;
        }
        let regex = if self.search_use_regex {
            regex::RegexBuilder::new(&query)
                .case_insensitive(true)
                .build()
                .ok()
        } else {
            None
        };
        let selected_id = self.selected.and_then(|i| self.files.get(i).copied());
        self.filter_job = Some(FilterJob {
            cursor: 0,
            matched: Vec::new(),
            query_lower: query.to_lowercase(),
            regex,
            membership_revision: self.files_membership_revision,
            selected_id,
        });
        self.search_dirty = false;
        self.search_deadline = None;
    }

    pub(super) fn pump_filter_job(&mut self) -> bool {
        let Some(mut job) = self.filter_job.take() else {
            return false;
        };
        if job.membership_revision != self.files_membership_revision {
            self.refresh_filter_then_sort();
            return true;
        }
        let started = std::time::Instant::now();
        while job.cursor < self.items.len() {
            if started.elapsed().as_secs_f64() * 1000.0 >= LIST_JOB_FRAME_BUDGET_MS {
                break;
            }
            let end = (job.cursor + 1_024).min(self.items.len());
            for idx in job.cursor..end {
                let item = &self.items[idx];
                if Self::item_matches_filter(item, &job.query_lower, job.regex.as_ref()) {
                    job.matched.push(item.id);
                }
            }
            job.cursor = end;
        }
        if job.cursor < self.items.len() {
            self.filter_job = Some(job);
            return true;
        }
        // Adopt the filtered set.
        self.files = job.matched;
        self.original_files = self.files.clone();
        self.note_files_membership_changed();
        self.selected = job
            .selected_id
            .and_then(|id| self.files.iter().position(|&x| x == id));
        if self.selected.is_none() {
            self.selected_multi.clear();
            self.select_anchor = None;
        }
        if self.sort_dir != SortDir::None {
            self.request_sort();
        }
        true
    }

    /// Per-frame pump for all async list jobs. Returns true while anything is
    /// still in flight so the frame loop keeps repaints scheduled.
    pub(super) fn pump_list_jobs(&mut self) -> bool {
        let mut busy = false;
        busy |= self.pump_filter_job();
        busy |= self.pump_sort_job();
        busy |= self.drain_sort_results();
        busy
    }
}
