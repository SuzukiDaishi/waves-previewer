//! Sorting and filtering the file list, for lists of any size.
//!
//! A row's value in a column comes from one place, [`WavesPreviewer::column_value`]:
//! sorting orders by it, column filters test it, the filter dialog lists it.
//! Filtering is the search box and every column filter compiled into one
//! [`CompiledListFilter`] and applied in a single pass.
//!
//! Small lists sort and filter synchronously in one frame. Above
//! `list_sync_threshold` the work is sliced across frames under a small time
//! budget (the comparison sort itself runs on a worker thread) and the result
//! is adopted only if the list membership has not changed in the meantime.
//! Both paths share the per-row functions below, so they cannot disagree.
//!
//! Whichever path adopts a new `files`, it takes the selection by id first
//! and puts it back after (`capture_selection_ids` / `restore_selection_ids`):
//! the selection belongs to files, not to row numbers.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::time::UNIX_EPOCH;

use super::list_filter::{CellValue, ColumnValueKind, CompiledColumnFilter};
use super::text_match::TextMatcher;
use super::types::{MediaId, MediaItem, SortDir, SortKey};
use super::WavesPreviewer;

// The sync threshold and the slice budget both come from `PerfProfile`
// (src/app/perf_profile.rs): a two-core machine takes seconds over what a
// developer workstation finishes in one frame, so these cannot be one
// hard-coded number. See `list_sync_threshold()` / `list_job_frame_budget_ms()`.

/// Owned sort key so the comparison sort can run off the UI thread.
pub(super) enum OwnedKey {
    Str(String),
    Num(Option<f64>),
    Missing,
}

pub(super) type DecoratedRow = (OwnedKey, String, MediaId);

pub(super) struct SortBuildJob {
    pub request_id: u64,
    pub dir: SortDir,
    /// Snapshot of the row order to sort (ids resolve items by stable id).
    pub ids: Vec<MediaId>,
    pub cursor: usize,
    pub decorated: Vec<DecoratedRow>,
    pub membership_revision: u64,
    pub started_at: std::time::Instant,
}

pub(super) struct SortResult {
    pub request_id: u64,
    pub sorted: Vec<MediaId>,
    /// The sorted snapshot storage, returned so the UI thread can free it a
    /// slice at a time. Dropping ~1M snapshot strings in one go on the sort
    /// worker contended on the allocator with the UI thread (the strings
    /// were allocated there) and showed up as a 200ms+ frame.
    pub decorated: Vec<DecoratedRow>,
    pub membership_revision: u64,
    pub started_at: std::time::Instant,
}

/// The search box and the column filters, ready to test rows.
pub(super) struct CompiledListFilter {
    search: Option<TextMatcher>,
    columns: Vec<(SortKey, CompiledColumnFilter)>,
}

impl CompiledListFilter {
    pub(super) fn is_empty(&self) -> bool {
        self.search.is_none() && self.columns.is_empty()
    }

    /// The same filter without the one on `key`: what the filter dialog's
    /// value list is gathered through, as Excel lists a column's values
    /// among the rows the other columns' filters leave.
    pub(super) fn without_column(mut self, key: SortKey) -> Self {
        self.columns.retain(|(k, _)| *k != key);
        self
    }

    /// Columns whose conditions need the whole column first (top N, average).
    fn aggregate_columns(&self) -> Vec<usize> {
        self.columns
            .iter()
            .enumerate()
            .filter(|(_, (_, c))| c.needs_aggregate())
            .map(|(i, _)| i)
            .collect()
    }
}

/// A sliced filter pass for large lists. The selection is deliberately not
/// part of it: it is taken when the result is adopted. Taken at the start,
/// it put back whatever was selected then, undoing any click made while the
/// job ran.
pub(super) struct FilterJob {
    filter: CompiledListFilter,
    /// Columns still waiting for their top-N / average to be measured, and
    /// the values collected for each so far. Empty once resolved.
    aggregate_columns: Vec<usize>,
    aggregate_values: Vec<Vec<f64>>,
    cursor: usize,
    matched: Vec<MediaId>,
    membership_revision: u64,
}

impl WavesPreviewer {
    /// Lists at or below this size sort/filter synchronously in one frame.
    pub(super) fn list_sync_threshold(&self) -> usize {
        self.perf.list_sync_threshold()
    }

    fn list_job_frame_budget_ms(&self) -> f64 {
        if self.playback_is_playing_now() || self.playback_session.is_playing {
            0.25
        } else {
            self.perf.list_job_frame_budget_ms()
        }
    }

    pub(super) fn note_files_membership_changed(&mut self) {
        self.files_membership_revision = self.files_membership_revision.wrapping_add(1);
    }

    // ---- Column values ----

    /// A row's value in a column -- what sorting orders by, what column
    /// filters test and what the filter dialog lists. Durations are seconds
    /// and dates are seconds since the epoch, whatever unit the metadata
    /// stores them in.
    pub(super) fn column_value<'a>(&'a self, item: &'a MediaItem, key: SortKey) -> CellValue<'a> {
        let m = item.meta.as_deref();
        let num = |v: Option<f64>| {
            v.filter(|v| v.is_finite())
                .map(CellValue::Number)
                .unwrap_or(CellValue::Missing)
        };
        let text = |t: &'a str| {
            if t.is_empty() {
                CellValue::Missing
            } else {
                CellValue::Text(Cow::Borrowed(t))
            }
        };
        let since_epoch = |t: Option<std::time::SystemTime>| {
            t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
        };
        match key {
            SortKey::File => text(&item.display_name),
            SortKey::Folder => text(&item.display_folder),
            SortKey::Transcript => item
                .transcript
                .as_ref()
                .map(|t| text(&t.full_text))
                .unwrap_or(CellValue::Missing),
            SortKey::Type => CellValue::Text(Self::list_type_sort_key(item)),
            SortKey::Length => num(m.and_then(|m| m.duration_secs).map(f64::from)),
            SortKey::Channels => num(m.map(|m| m.channels as f64).filter(|v| *v > 0.0)),
            SortKey::SampleRate => num(self.effective_sample_rate_for_path(&item.path).map(f64::from)),
            SortKey::Bits => num(self
                .bit_depth_override
                .get(&item.path)
                .copied()
                .map(|v| v.bits_per_sample())
                .or_else(|| m.map(|m| m.bits_per_sample))
                .filter(|v| *v > 0)
                .map(f64::from)),
            SortKey::BitRate => num(m
                .and_then(|m| m.bit_rate_bps)
                .map(|v| v as f64)
                .filter(|v| *v > 0.0)),
            SortKey::Level => num(m.and_then(|m| m.peak_db).map(f64::from)),
            // LUFS uses the effective value: an override if present, else
            // the measured value plus the pending gain.
            SortKey::Lufs => num(self
                .lufs_override
                .get(&item.path)
                .copied()
                .or_else(|| m.and_then(|m| m.lufs_i.map(|x| x + item.pending_gain_db)))
                .map(f64::from)),
            SortKey::TruePeak => num(m
                .and_then(|m| m.true_peak_db.map(|v| v + item.pending_gain_db))
                .map(f64::from)),
            SortKey::LufsShort => num(m
                .and_then(|m| m.lufs_s_max.map(|v| v + item.pending_gain_db))
                .map(f64::from)),
            SortKey::LufsMomentary => num(m
                .and_then(|m| m.lufs_m_max.map(|v| v + item.pending_gain_db))
                .map(f64::from)),
            SortKey::Bpm => num(m.and_then(|m| m.bpm).filter(|v| *v > 0.0).map(f64::from)),
            SortKey::SilenceLead => {
                num(m.and_then(|m| m.silence_lead_ms).map(|ms| f64::from(ms) / 1000.0))
            }
            SortKey::SilenceTail => {
                num(m.and_then(|m| m.silence_tail_ms).map(|ms| f64::from(ms) / 1000.0))
            }
            SortKey::EdgeZero => Self::qa_value(self.qa_edge_zero_for_path(&item.path)),
            SortKey::OverPeak => Self::qa_value(self.qa_over_peak_for_path(&item.path)),
            SortKey::BlankPad => Self::qa_value(self.qa_blank_pad_for_path(&item.path)),
            SortKey::CreatedAt => num(since_epoch(m.and_then(|m| m.created_at))),
            SortKey::ModifiedAt => num(since_epoch(m.and_then(|m| m.modified_at))),
            SortKey::External(idx) => self
                .external_visible_columns
                .get(idx)
                .and_then(|col| item.external_value(col))
                .map(|v| text(v))
                .unwrap_or(CellValue::Missing),
            SortKey::Metadata(index) => match self.metadata_owned_sort_key(&item.path, index) {
                OwnedKey::Str(s) if !s.is_empty() => CellValue::Text(Cow::Owned(s)),
                OwnedKey::Num(Some(n)) => CellValue::Number(n),
                _ => CellValue::Missing,
            },
            // Zero comments is a value (0), not a blank.
            SortKey::Comments => {
                CellValue::Number(self.comment_summary_for_path(&item.path).total as f64)
            }
            SortKey::Status => item
                .status_id
                .as_deref()
                .and_then(|id| self.status_palette.get(id))
                .map(|def| text(&def.label))
                .unwrap_or(CellValue::Missing),
            SortKey::Tags => {
                let labels: Vec<Cow<'a, str>> = item
                    .tags
                    .iter()
                    .flat_map(|tags| tags.iter())
                    .map(|id| match self.tag_palette.get(id) {
                        Some(def) => Cow::Borrowed(def.label.as_str()),
                        None => Cow::Borrowed(id.as_ref()),
                    })
                    .collect();
                if labels.is_empty() {
                    CellValue::Missing
                } else {
                    CellValue::Texts(labels)
                }
            }
            SortKey::Note => text(&item.note),
            SortKey::Gain => CellValue::Number(f64::from(item.pending_gain_db)),
            SortKey::TranscriptLanguage => item
                .transcript_language
                .as_deref()
                .map(text)
                .unwrap_or(CellValue::Missing),
        }
    }

    fn qa_value(status: super::list_state_ops::QaStatus) -> CellValue<'static> {
        use super::list_state_ops::QaStatus;
        match status {
            QaStatus::Unknown => CellValue::Missing,
            QaStatus::Pass => CellValue::Text(Cow::Borrowed("OK")),
            QaStatus::Fail(_) => CellValue::Text(Cow::Borrowed("NG")),
        }
    }

    /// What a column holds. Built-in columns know; a metadata column is
    /// typed by the first value found in it (a numeric tag such as BPM or a
    /// track number filters as a number).
    pub(super) fn column_value_kind(&self, key: SortKey) -> ColumnValueKind {
        /// How many rows to look at before settling on text for a metadata
        /// column whose values have not been read yet.
        const METADATA_KIND_SAMPLE_ROWS: usize = 512;
        if let SortKey::Metadata(_) = key {
            for &id in self.files.iter().take(METADATA_KIND_SAMPLE_ROWS) {
                let Some(item) = self.item_for_id(id) else {
                    continue;
                };
                match self.column_value(item, key) {
                    CellValue::Number(_) => return ColumnValueKind::Number,
                    CellValue::Missing => continue,
                    _ => return ColumnValueKind::Text,
                }
            }
            return ColumnValueKind::Text;
        }
        key.value_kind()
    }

    /// Extract the sort key for one row as an owned value.
    pub(super) fn owned_sort_key(&self, item: &MediaItem, key: SortKey) -> OwnedKey {
        match key {
            // QA columns sort by verdict (NG, then OK, then not-yet-known),
            // not by the label's spelling.
            SortKey::EdgeZero => OwnedKey::Num(self.qa_edge_zero_for_path(&item.path).sort_rank()),
            SortKey::OverPeak => OwnedKey::Num(self.qa_over_peak_for_path(&item.path).sort_rank()),
            SortKey::BlankPad => OwnedKey::Num(self.qa_blank_pad_for_path(&item.path).sort_rank()),
            SortKey::Metadata(index) => self.metadata_owned_sort_key(&item.path, index),
            _ => match self.column_value(item, key) {
                CellValue::Text(t) => OwnedKey::Str(t.into_owned()),
                CellValue::Texts(v) => OwnedKey::Str(v.join(", ")),
                CellValue::Number(n) => OwnedKey::Num(Some(n)),
                // Blank text sorts as the empty string, blank numbers at the
                // end whichever the direction -- as before this was shared.
                CellValue::Missing => match key.value_kind() {
                    ColumnValueKind::Text => OwnedKey::Str(String::new()),
                    _ => OwnedKey::Num(None),
                },
            },
        }
    }

    /// One row of the decorate-sort-undecorate, for both sort paths.
    fn decorate_row(&self, id: MediaId, key: SortKey) -> DecoratedRow {
        match self.item_for_id(id) {
            Some(item) => (
                self.owned_sort_key(item, key),
                item.display_name.clone(),
                id,
            ),
            None => (OwnedKey::Missing, String::new(), id),
        }
    }

    pub(super) fn compare_decorated_rows(a: &DecoratedRow, b: &DecoratedRow, dir: SortDir) -> Ordering {
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

    // ---- Sort ----

    /// Sort `files` synchronously (the small-list path).
    pub(super) fn apply_sort(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let sort_started = std::time::Instant::now();
        self.sort_loading_started_at = Some(sort_started);
        let selection = self.capture_selection_ids();
        let key = self.sort_key;
        let dir = self.sort_dir;
        if dir == SortDir::None {
            self.files = self.original_files.clone();
        } else {
            // Decorate-sort-undecorate with owned keys, shared with the async
            // sort job. The owned clones only cost on this small-list path;
            // large lists go through request_sort() and sort off-thread.
            let mut decorated: Vec<DecoratedRow> =
                self.files.iter().map(|&id| self.decorate_row(id, key)).collect();
            decorated.sort_unstable_by(|a, b| Self::compare_decorated_rows(a, b, dir));
            self.files = decorated.into_iter().map(|e| e.2).collect();
        }
        self.restore_selection_ids(&selection);
        self.note_sort_finished(sort_started.elapsed());
    }

    /// Timing bookkeeping shared by both sort paths: the loading indicator's
    /// hold and the streaming-metadata re-sort debounce.
    fn note_sort_finished(&mut self, elapsed: std::time::Duration) {
        /// A sort slower than this was noticeable, so its "sorted" indicator
        /// stays up longer to explain the pause.
        const SLOW_SORT: std::time::Duration = std::time::Duration::from_millis(120);
        const HOLD_AFTER_FAST_SORT: std::time::Duration = std::time::Duration::from_millis(500);
        const HOLD_AFTER_SLOW_SORT: std::time::Duration = std::time::Duration::from_millis(900);
        self.sort_loading_last_ms = elapsed.as_secs_f32() * 1000.0;
        let hold = if elapsed >= SLOW_SORT {
            HOLD_AFTER_SLOW_SORT
        } else {
            HOLD_AFTER_FAST_SORT
        };
        self.sort_loading_hold_until = Some(std::time::Instant::now() + hold);
        self.sort_loading_started_at = None;
        // Any sort counts as "just sorted" for the streaming-metadata resort
        // debounce; without this the first metadata batch after a header
        // click re-sorted the whole list a second time in the same frame.
        self.meta_sort_last_applied = Some(std::time::Instant::now());
    }

    /// Request a re-sort of the current list. Small lists sort synchronously;
    /// large lists build the sort snapshot over multiple frames and sort on a
    /// worker thread while the UI keeps the old order.
    pub(super) fn request_sort(&mut self) {
        if self.files.len() <= self.list_sync_threshold() || self.sort_dir == SortDir::None {
            // Cancel any stale async job so its result cannot overwrite the
            // fresh synchronous order.
            self.sort_request_seq = self.sort_request_seq.wrapping_add(1);
            self.sort_job = None;
            self.sort_rx = None;
            self.apply_sort();
            return;
        }
        self.sort_request_seq = self.sort_request_seq.wrapping_add(1);
        self.sort_job = Some(SortBuildJob {
            request_id: self.sort_request_seq,
            dir: self.sort_dir,
            ids: self.files.clone(),
            cursor: 0,
            decorated: Vec::with_capacity(self.files.len()),
            membership_revision: self.files_membership_revision,
            started_at: std::time::Instant::now(),
        });
        self.sort_rx = None;
        self.sort_loading_started_at = Some(std::time::Instant::now());
    }

    pub(super) fn sort_job_active(&self) -> bool {
        self.sort_job.is_some() || self.sort_rx.is_some()
    }

    /// Rows per budget check in the sliced passes: checking the clock per
    /// row would dominate, and while audio plays the budget is a fraction of
    /// a millisecond, so the slices shrink with it.
    fn list_job_chunk(&self, normal: usize) -> usize {
        /// Rows per chunk when the budget is under a millisecond.
        const TIGHT_BUDGET_CHUNK: usize = 128;
        if self.list_job_frame_budget_ms() < 1.0 {
            TIGHT_BUDGET_CHUNK
        } else {
            normal
        }
    }

    /// Slice the decorate stage across frames, then hand the snapshot to a
    /// worker thread for the O(n log n) sort. Returns true while working (the
    /// caller should keep repaints coming).
    pub(super) fn pump_sort_job(&mut self) -> bool {
        /// Rows decorated between clock checks.
        const DECORATE_CHUNK: usize = 2_048;
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
        let frame_budget_ms = self.list_job_frame_budget_ms();
        let chunk_size = self.list_job_chunk(DECORATE_CHUNK);
        let started = std::time::Instant::now();
        while job.cursor < job.ids.len() {
            if started.elapsed().as_secs_f64() * 1000.0 >= frame_budget_ms {
                break;
            }
            let end = (job.cursor + chunk_size).min(job.ids.len());
            for idx in job.cursor..end {
                let row = self.decorate_row(job.ids[idx], key);
                job.decorated.push(row);
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
        let started_at = job.started_at;
        let mut decorated = job.decorated;
        std::thread::spawn(move || {
            decorated.sort_unstable_by(|a, b| Self::compare_decorated_rows(a, b, dir));
            let sorted: Vec<MediaId> = decorated.iter().map(|e| e.2).collect();
            let _ = tx.send(SortResult {
                request_id,
                sorted,
                decorated,
                membership_revision,
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
        if self.deferred_list_drop.is_empty() {
            // Move, don't copy: extending would memcpy the ~50MB snapshot.
            self.deferred_list_drop = result.decorated;
        } else {
            self.deferred_list_drop.extend(result.decorated);
        }
        if result.request_id != self.sort_request_seq {
            return true;
        }
        if result.membership_revision != self.files_membership_revision {
            // Rows were added/removed while sorting: run again on fresh data.
            self.request_sort();
            return true;
        }
        let selection = self.capture_selection_ids();
        self.files = result.sorted;
        self.restore_selection_ids(&selection);
        self.note_sort_finished(result.started_at.elapsed());
        true
    }

    // ---- Search ----

    /// Whether the search box matches a row: its name, folder, note,
    /// transcript, metadata summary or any external-sheet value. The
    /// metadata *fields* are checked separately (`metadata_search_matches`)
    /// because they live in a cache, not on the item.
    pub(super) fn item_matches_search(item: &MediaItem, matcher: &TextMatcher) -> bool {
        matcher.is_match(&item.display_name)
            || matcher.is_match(&item.display_folder)
            // The emptiness guard keeps the overwhelming majority of rows,
            // which have no note, off the matcher entirely.
            || (!item.note.is_empty() && matcher.is_match(&item.note))
            || item
                .transcript
                .as_ref()
                .is_some_and(|t| matcher.is_match(&t.full_text))
            || item
                .meta
                .as_ref()
                .is_some_and(|m| matcher.is_match(&Self::meta_search_summary(m)))
            || item
                .external
                .as_ref()
                .is_some_and(|ext| ext.values().any(|v| matcher.is_match(v)))
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

    // ---- Filter ----

    /// The search box and every column filter, compiled. A column filter
    /// that no longer compiles (a session from another build, say) is left
    /// out rather than hiding every row.
    pub(super) fn compile_list_filter(&self) -> CompiledListFilter {
        let now = chrono::Local::now();
        CompiledListFilter {
            search: TextMatcher::search(&self.search_query, self.search_use_regex),
            columns: self
                .column_filters
                .iter()
                .filter_map(|f| {
                    CompiledColumnFilter::compile(f.kind, &f.rule, now)
                        .ok()
                        .map(|c| (f.key, c))
                })
                .collect(),
        }
    }

    /// Whether a row passes the search and every column filter except
    /// `except` (used while measuring that column for its top N / average).
    pub(super) fn row_passes(
        &self,
        item: &MediaItem,
        filter: &CompiledListFilter,
        except: Option<usize>,
    ) -> bool {
        if let Some(matcher) = &filter.search {
            if !(Self::item_matches_search(item, matcher)
                || self.metadata_search_matches(&item.path, matcher))
            {
                return false;
            }
        }
        filter
            .columns
            .iter()
            .enumerate()
            .all(|(i, (key, column))| Some(i) == except || column.matches(&self.column_value(item, *key)))
    }

    /// Collects one row's numbers for the columns still being measured.
    fn feed_aggregates(
        &self,
        item: &MediaItem,
        filter: &CompiledListFilter,
        columns: &[usize],
        values: &mut [Vec<f64>],
    ) {
        for (slot, &col) in columns.iter().enumerate() {
            if self.row_passes(item, filter, Some(col)) {
                if let Some(n) = self.column_value(item, filter.columns[col].0).number() {
                    values[slot].push(n);
                }
            }
        }
    }

    fn resolve_aggregates(filter: &mut CompiledListFilter, columns: &[usize], values: &[Vec<f64>]) {
        for (slot, &col) in columns.iter().enumerate() {
            filter.columns[col].1.resolve_aggregates(&values[slot]);
        }
    }

    pub(super) fn has_column_filters(&self) -> bool {
        !self.column_filters.is_empty()
    }

    /// Filter synchronously (the small-list path) and adopt the result.
    pub(super) fn apply_filter_from_search(&mut self) {
        let mut filter = self.compile_list_filter();
        let columns = filter.aggregate_columns();
        if !columns.is_empty() {
            let mut values = vec![Vec::new(); columns.len()];
            for item in self.items.iter() {
                self.feed_aggregates(item, &filter, &columns, &mut values);
            }
            Self::resolve_aggregates(&mut filter, &columns, &values);
        }
        let matched: Vec<MediaId> = if filter.is_empty() {
            self.items.iter().map(|item| item.id).collect()
        } else {
            self.items
                .iter()
                .filter(|item| self.row_passes(item, &filter, None))
                .map(|item| item.id)
                .collect()
        };
        self.adopt_filtered(matched);
        self.search_dirty = false;
        self.search_deadline = None;
    }

    fn adopt_filtered(&mut self, matched: Vec<MediaId>) {
        let selection = self.capture_selection_ids();
        self.files = matched;
        self.original_files = self.files.clone();
        self.note_files_membership_changed();
        self.restore_selection_ids(&selection);
    }

    pub(super) fn filter_job_active(&self) -> bool {
        self.filter_job.is_some()
    }

    /// Apply the search box and the column filters to `files`, then re-sort.
    /// Small lists filter synchronously; large lists filter in per-frame
    /// slices and adopt the result (then re-sort) when done.
    pub(super) fn refresh_filter_then_sort(&mut self) {
        let filter = self.compile_list_filter();
        if filter.is_empty() || self.items.len() <= self.list_sync_threshold() {
            self.filter_job = None;
            self.apply_filter_from_search();
            if self.sort_dir != SortDir::None {
                self.request_sort();
            }
            return;
        }
        let aggregate_columns = filter.aggregate_columns();
        self.filter_job = Some(FilterJob {
            aggregate_values: vec![Vec::new(); aggregate_columns.len()],
            aggregate_columns,
            filter,
            cursor: 0,
            matched: Vec::new(),
            membership_revision: self.files_membership_revision,
        });
        self.search_dirty = false;
        self.search_deadline = None;
    }

    pub(super) fn pump_filter_job(&mut self) -> bool {
        /// Rows tested between clock checks.
        const FILTER_CHUNK: usize = 1_024;
        let Some(mut job) = self.filter_job.take() else {
            return false;
        };
        if job.membership_revision != self.files_membership_revision {
            self.refresh_filter_then_sort();
            return true;
        }
        let frame_budget_ms = self.list_job_frame_budget_ms();
        let chunk_size = self.list_job_chunk(FILTER_CHUNK);
        let started = std::time::Instant::now();
        let total = self.items.len();
        while job.cursor < total {
            if started.elapsed().as_secs_f64() * 1000.0 >= frame_budget_ms {
                break;
            }
            let end = (job.cursor + chunk_size).min(total);
            for idx in job.cursor..end {
                let item = &self.items[idx];
                if job.aggregate_columns.is_empty() {
                    if self.row_passes(item, &job.filter, None) {
                        job.matched.push(item.id);
                    }
                } else {
                    self.feed_aggregates(
                        item,
                        &job.filter,
                        &job.aggregate_columns,
                        &mut job.aggregate_values,
                    );
                }
            }
            job.cursor = end;
            if job.cursor >= total && !job.aggregate_columns.is_empty() {
                // Measuring pass done: resolve, then run the matching pass.
                Self::resolve_aggregates(&mut job.filter, &job.aggregate_columns, &job.aggregate_values);
                job.aggregate_columns.clear();
                job.aggregate_values.clear();
                job.cursor = 0;
            }
        }
        if job.cursor < total || !job.aggregate_columns.is_empty() {
            self.filter_job = Some(job);
            return true;
        }
        self.adopt_filtered(job.matched);
        if self.sort_dir != SortDir::None {
            self.request_sort();
        }
        true
    }

    /// Free a bounded slice of retired sort-snapshot entries per frame.
    fn pump_deferred_drop(&mut self) -> bool {
        /// Entries freed per frame while audio plays / otherwise: freeing
        /// strings contends on the allocator with the audio path.
        const DROP_PER_FRAME_PLAYING: usize = 1_024;
        const DROP_PER_FRAME_IDLE: usize = 16_384;
        let len = self.deferred_list_drop.len();
        if len == 0 {
            return false;
        }
        let drop_budget = if self.playback_is_playing_now() || self.playback_session.is_playing {
            DROP_PER_FRAME_PLAYING
        } else {
            DROP_PER_FRAME_IDLE
        };
        if len <= drop_budget {
            // Also release the (tens of MB) backing buffer itself.
            self.deferred_list_drop = Vec::new();
        } else {
            self.deferred_list_drop.truncate(len - drop_budget);
        }
        true
    }

    /// Per-frame pump for all async list jobs. Returns true while anything is
    /// still in flight so the frame loop keeps repaints scheduled.
    pub(super) fn pump_list_jobs(&mut self) -> bool {
        /// A pump stage slower than this is reported under NEOWAVES_BENCH_TRACE.
        const TRACE_SLOW_STAGE_MS: f64 = 30.0;
        let mut busy = false;
        busy |= self.pump_deferred_drop();
        let t0 = std::time::Instant::now();
        busy |= self.pump_filter_job();
        let t1 = std::time::Instant::now();
        busy |= self.pump_sort_job();
        let t2 = std::time::Instant::now();
        busy |= self.drain_sort_results();
        let t3 = std::time::Instant::now();
        if std::env::var_os("NEOWAVES_BENCH_TRACE").is_some() {
            let f = (t1 - t0).as_secs_f64() * 1000.0;
            let p = (t2 - t1).as_secs_f64() * 1000.0;
            let d = (t3 - t2).as_secs_f64() * 1000.0;
            if f > TRACE_SLOW_STAGE_MS || p > TRACE_SLOW_STAGE_MS || d > TRACE_SLOW_STAGE_MS {
                eprintln!("[trace] pump_list_jobs filter={f:.1}ms pump={p:.1}ms drain={d:.1}ms");
            }
        }
        busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn item_with_note(name: &str, note: &str) -> MediaItem {
        let path = PathBuf::from(format!("/audio/{name}"));
        MediaItem {
            id: 1,
            audio_asset: crate::audio_asset::AudioAssetDescriptor::external_unprobed(path.clone()),
            path,
            display_name: name.to_string(),
            display_folder: std::sync::Arc::from("audio"),
            source: super::super::types::MediaSource::File,
            meta: None,
            pending_gain_db: 0.0,
            note: note.to_string(),
            editor_notes: Vec::new(),
            status: super::super::types::MediaStatus::Ok,
            status_id: None,
            tags: None,
            transcript: None,
            transcript_document: None,
            transcript_language: None,
            external: None,
            virtual_audio: None,
            virtual_state: None,
        }
    }

    fn search(query: &str, regex: bool) -> TextMatcher {
        TextMatcher::search(query, regex).expect("non-empty query")
    }

    #[test]
    fn substring_search_matches_the_list_note() {
        let item = item_with_note("kick_01.wav", "needs a longer tail");
        assert!(WavesPreviewer::item_matches_search(&item, &search("longer tail", false)));
        // Case-insensitive, like every other field the search covers.
        let shouty = item_with_note("kick_01.wav", "NEEDS A LONGER TAIL");
        assert!(WavesPreviewer::item_matches_search(&shouty, &search("longer tail", false)));
    }

    #[test]
    fn substring_search_does_not_match_an_empty_note() {
        let item = item_with_note("kick_01.wav", "");
        assert!(!WavesPreviewer::item_matches_search(&item, &search("tail", false)));
        // A row whose note is set must not match a term that appears in
        // neither the note nor any other searched field.
        let noted = item_with_note("kick_01.wav", "needs a longer tail");
        assert!(!WavesPreviewer::item_matches_search(&noted, &search("reverb", false)));
    }

    #[test]
    fn substring_search_still_matches_the_filename() {
        let item = item_with_note("kick_01.wav", "unrelated");
        assert!(WavesPreviewer::item_matches_search(&item, &search("kick", false)));
    }

    #[test]
    fn regex_search_matches_the_list_note() {
        let item = item_with_note("kick_01.wav", "retake at 120bpm");
        assert!(WavesPreviewer::item_matches_search(&item, &search(r"\d+bpm", true)));

        let empty = item_with_note("kick_01.wav", "");
        assert!(!WavesPreviewer::item_matches_search(&empty, &search(r"\d+bpm", true)));
    }
}
