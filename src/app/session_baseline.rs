//! "What changed since **you** last opened this session?"
//!
//! A colleague replacing a wav on the share does not touch the `.nwsess` at
//! all, so the conflict detection in `session_sync` cannot see it. This does:
//! it remembers what every referenced file looked like at the end of the last
//! open (in the per-user store, see `session_store`) and diffs against that
//! on the next one.
//!
//! Two tiers, because a session here can reference a hundred thousand files
//! on a network share and hashing all of them on every open would cost more
//! than the work the user came to do:
//!
//! 1. **stat every file** for `(size, mtime)`. One syscall each, and it
//!    settles the overwhelming majority -- nothing moved, nothing to do.
//! 2. **hash only the files whose stat moved.** Cost is proportional to what
//!    actually changed, and it is what tells a real edit apart from a file
//!    that was merely copied back or touched.
//!
//! A separate low-priority pass fills in hashes for files that have never had
//! one, so the baseline converges toward being able to make that distinction
//! everywhere. It is persisted, so it picks up where it left off across runs.
//!
//! All of it runs on workers. The UI thread never stats and never hashes.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, UNIX_EPOCH};

use super::session_store::{now_unix, FileBaseline, TrackedKind};

/// Ceiling on concurrent stat/hash workers. `perf.scan_pool_workers` narrows
/// this further, and already returns 2 when the list root is a share.
const MAX_WORKERS: usize = 4;
/// Results applied per frame.
const DRAIN_PER_FRAME: usize = 256;
/// How many never-hashed files one open will hash in the background before
/// leaving the rest for next time. Bounded so a fresh hundred-thousand-file
/// session does not read every byte it references the first time it is
/// opened; the baseline fills in over a few sessions instead.
const BACKFILL_PER_OPEN: usize = 512;

/// How a referenced file differs from the baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// The bytes are different from what they were.
    Changed,
    /// Referenced now, but not present at the last scan.
    Added,
    /// Was there at the last scan, and is not now.
    Removed,
    /// Still referenced, but this scan could not read it -- a permission
    /// error, or a share that went away. Deliberately not `Changed`: we did
    /// not establish that anything changed, and saying so would be asserting
    /// something we never checked.
    Unreadable,
}

impl ChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Changed => "Changed",
            Self::Added => "Added",
            Self::Removed => "Removed",
            Self::Unreadable => "Unreadable",
        }
    }
}

/// One row of the "changed since you last opened this" report.
#[derive(Clone, Debug)]
pub struct FileChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub tracked: TrackedKind,
    /// Size now (0 for a removed file).
    pub size: u64,
    /// When this scan noticed, which is what the list shows as "detected".
    pub detected_at: i64,
}

/// One file for the workers to look at.
struct Job {
    path: PathBuf,
    tracked: TrackedKind,
    /// What the store had for it, if anything.
    previous: Option<FileBaseline>,
    /// True for the low-priority "give this file a hash even though its stat
    /// did not move" pass.
    backfill: bool,
}

/// What a worker found.
struct Probe {
    path: PathBuf,
    tracked: TrackedKind,
    /// What the probe established: present, gone, or unreadable.
    probe: FileProbe,
    /// The row to store, or `None` to leave the existing row alone.
    ///
    /// Leaving it alone is not the same as storing nothing: a baseline row
    /// is only advanced when the worker actually established what the file
    /// contains now. Overwriting it otherwise throws away a hash that is
    /// still true, and the second tier has nothing left to compare against.
    row: Option<FileBaseline>,
    change: Option<ChangeKind>,
}

pub(super) struct BaselineScanState {
    pub total: usize,
    pub done: usize,
    rx: std::sync::mpsc::Receiver<Probe>,
    cancel: Arc<AtomicBool>,
    /// Bumped per session open; a result from a superseded scan is dropped
    /// rather than applied to the session that replaced it.
    pub generation: u64,
    pub session_key: String,
    /// Baseline rows to write when the scan finishes.
    rows: Vec<(PathBuf, FileBaseline)>,
    /// Paths the session no longer references, or that vanished.
    removed: Vec<PathBuf>,
    /// The report, built as results land.
    pub changes: Vec<FileChange>,
    /// `None` on a session this user has never opened: the scan records a
    /// baseline and reports nothing, because otherwise every file in a new
    /// session would be "Added".
    pub since: Option<i64>,
    pub started_at: Instant,
}

impl BaselineScanState {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return 1.0;
        }
        (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
    }
}

/// Retry delays for the bulk scan.
///
/// The save path retries a sharing violation over most of a second, which is
/// right when losing the race means losing a save. Here it is wrong: this
/// stats every file the session references, and a permission error is
/// usually permanent, so a long backoff would spend minutes proving what one
/// call already established.
const SCAN_RETRY_DELAYS_MS: [u64; 1] = [50];

/// What one look at a referenced file established.
///
/// "Could not read it" and "it is not there" are different answers and must
/// not be collapsed. Treating an unreadable file as missing reports it as
/// removed -- which is false, it is still there -- and drops its baseline
/// row, so the next open reports it a second time as newly added and its
/// hash is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileProbe {
    /// Size and mtime.
    Present(u64, u128),
    /// The file is genuinely gone.
    Missing,
    /// Still there as far as we know, but this scan could not read it.
    Unreadable,
}

fn stat_of(path: &Path) -> FileProbe {
    let meta = match super::session_sync::retry_shared_io_with(&SCAN_RETRY_DELAYS_MS, || {
        std::fs::metadata(path)
    }) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return FileProbe::Missing,
        // Anything else -- no permission, a share that dropped, a path that
        // stopped making sense -- means we did not find out. Say so rather
        // than declaring the file gone.
        Err(_) => return FileProbe::Unreadable,
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    FileProbe::Present(meta.len(), mtime)
}

/// The whole decision, in one place.
///
/// Returns what the worker should report and whether it needs to hash. Kept
/// free of I/O so every branch is testable without a filesystem.
fn classify(
    previous: Option<&FileBaseline>,
    probe: FileProbe,
    hash: Option<&str>,
) -> Option<ChangeKind> {
    match (previous, probe) {
        // We could not look. Report that, and nothing stronger.
        (_, FileProbe::Unreadable) => Some(ChangeKind::Unreadable),
        // Gone, and we knew about it.
        (Some(_), FileProbe::Missing) => Some(ChangeKind::Removed),
        // Gone, and we never knew about it: nothing to say.
        (None, FileProbe::Missing) => None,
        // New reference since the last scan.
        (None, FileProbe::Present(..)) => Some(ChangeKind::Added),
        (Some(previous), FileProbe::Present(size, mtime)) => {
            if previous.size == size && previous.mtime_ns == mtime {
                // Tier 1 settled it: untouched.
                return None;
            }
            match (previous.content_hash.as_deref(), hash) {
                // Tier 2 settled it: touched, but the bytes are the same.
                // Copying a file back or restoring it from a backup lands
                // here, and reporting it would be a false alarm.
                (Some(before), Some(now)) if before == now => None,
                (Some(_), Some(_)) => Some(ChangeKind::Changed),
                // No hash to compare against. The stat moved, and we cannot
                // prove the bytes did not, so say so -- and the hash the
                // worker just took makes the next comparison exact.
                _ => Some(ChangeKind::Changed),
            }
        }
    }
}

/// The baseline row to store after probing one file, or `None` to leave the
/// stored row untouched.
///
/// The rule is: **only advance a row when the new content is actually
/// known.** Storing what we merely observed instead loses information --
/// a hash that is still true gets replaced by "no hash", the second tier
/// is left with nothing to compare against, and a byte-identical rewrite
/// starts reporting as a change.
///
/// Pure, so every branch is testable without touching a filesystem.
fn next_baseline_row(
    previous: Option<&FileBaseline>,
    tracked: TrackedKind,
    probe: FileProbe,
    wanted_hash: bool,
    hash: Option<String>,
    now: i64,
) -> Option<FileBaseline> {
    let (size, mtime_ns) = match probe {
        FileProbe::Present(size, mtime_ns) => (size, mtime_ns),
        // Gone: handled as a removal, not a row.
        FileProbe::Missing => return None,
        // We learned nothing, so there is nothing to record. Advancing the
        // row here would throw away a hash that may still be true.
        FileProbe::Unreadable => return None,
    };

    if !wanted_hash {
        // Tier 1 said nothing moved, so tier 2 never ran. The stored hash
        // still describes these bytes -- carry it, and its original
        // detection time, forward untouched.
        return previous.map(|prev| FileBaseline {
            kind: tracked,
            size,
            mtime_ns,
            content_hash: prev.content_hash.clone(),
            recorded_at: prev.recorded_at,
        });
    }

    if hash.is_none() && previous.is_some_and(|prev| prev.content_hash.is_some()) {
        // We meant to hash and could not -- an unreadable file, a share that
        // dropped. Advancing the row here would trade a known-good hash for
        // nothing and, because the stored stat would then match the file,
        // nothing would ever hash it again. Leave it for the next scan.
        return None;
    }

    Some(FileBaseline {
        kind: tracked,
        size,
        mtime_ns,
        content_hash: hash,
        recorded_at: now,
    })
}

impl super::WavesPreviewer {
    /// Ask the store what this user knew about the session last time. The
    /// scan starts when the answer lands in `drain_session_store`.
    pub(super) fn begin_session_change_check(&mut self) {
        self.session_file_changes = None;
        self.cancel_baseline_scan();
        self.session_store_load = None;
        let Some(path) = self.project_path.clone() else {
            return;
        };
        if !self.session_store.is_enabled() {
            // Nothing can answer "since you last opened it", so skip the
            // snapshot below -- on a large list it is a six-figure clone and
            // sort on the UI thread, for an answer nobody will read.
            return;
        }
        // Snapshot what the session references *now*, before the store's
        // reply arrives a frame or more later. By then the list may have
        // pruned rows whose file is gone -- and a file that vanished is
        // exactly what this feature has to report, so reading the live list
        // at that point would silently lose it.
        self.baseline_tracked = self.tracked_session_files();
        let key = super::session_store::session_key(self.session_id.as_deref(), &path);
        if let Some(request) = self.session_store.load(key.clone()) {
            self.session_store_load = Some((request, key));
        }
    }

    /// Apply whatever the store worker has answered. Cheap: at most a
    /// handful of replies ever queue.
    pub(super) fn drain_session_store(&mut self) {
        loop {
            let Ok(reply) = self.session_store_rx.try_recv() else {
                return;
            };
            match reply {
                super::session_store::StoreReply::Loaded {
                    request,
                    visit,
                    baseline,
                } => {
                    let Some((pending, key)) = self.session_store_load.clone() else {
                        continue;
                    };
                    if pending != request {
                        // A reply for a session the user has since closed.
                        continue;
                    }
                    self.session_store_load = None;
                    if let Some(visit) = visit.as_ref() {
                        self.debug_log(format!(
                            "session last opened here {} (scanned {}, revision {})",
                            format_stamp(visit.last_opened_at),
                            format_stamp(visit.last_scanned_at),
                            visit
                                .last_revision
                                .map(|r| r.to_string())
                                .unwrap_or_else(|| "unknown".to_string())
                        ));
                    }
                    let since = visit.map(|v| v.last_opened_at);
                    self.begin_baseline_scan(key, since, baseline);
                }
                super::session_store::StoreReply::History { request, entries } => {
                    if self.session_history_request == Some(request) {
                        self.session_history_request = None;
                        self.session_history_entries = entries;
                    }
                }
                super::session_store::StoreReply::HistoryBytes { request, bytes } => {
                    let Some((pending, intent)) = self.session_history_pending.clone() else {
                        continue;
                    };
                    if pending != request {
                        continue;
                    }
                    self.session_history_pending = None;
                    match bytes {
                        Some(bytes) => self.apply_session_history_bytes(bytes, intent),
                        None => self.push_toast(
                            super::types::ToastSeverity::Error,
                            "That version is no longer in the local history",
                        ),
                    }
                }
                super::session_store::StoreReply::Failed { request, error } => {
                    if self.session_store_load.as_ref().map(|(r, _)| *r) == Some(request) {
                        self.session_store_load = None;
                    }
                    if self.session_history_request == Some(request) {
                        self.session_history_request = None;
                    }
                    if self.session_history_pending.as_ref().map(|(r, _)| *r) == Some(request) {
                        self.session_history_pending = None;
                    }
                    self.debug_log(format!("session store error: {error}"));
                }
            }
        }
    }

    /// Every file the open session refers to, with what kind it is.
    fn tracked_session_files(&self) -> Vec<(PathBuf, TrackedKind)> {
        let mut out: Vec<(PathBuf, TrackedKind)> = self
            .items
            .iter()
            .filter(|item| item.source == super::types::MediaSource::File)
            .map(|item| (item.path.clone(), TrackedKind::Audio))
            .collect();
        for source in &self.external_sources {
            out.push((source.path.clone(), TrackedKind::ExternalData));
        }
        out.sort();
        out.dedup_by(|a, b| a.0 == b.0);
        out
    }

    /// Start the "what changed since you last opened this" scan. Called once
    /// the store has answered with the previous visit and baseline.
    pub(super) fn begin_baseline_scan(
        &mut self,
        session_key: String,
        since: Option<i64>,
        baseline: Vec<(PathBuf, FileBaseline)>,
    ) {
        if let Some(state) = self.baseline_scan.as_ref() {
            state.cancel.store(true, Ordering::Relaxed);
        }
        let tracked = std::mem::take(&mut self.baseline_tracked);
        let mut previous: HashMap<PathBuf, FileBaseline> = baseline.into_iter().collect();

        let mut jobs: VecDeque<Job> = VecDeque::with_capacity(tracked.len());
        let mut backfill_budget = BACKFILL_PER_OPEN;
        for (path, kind) in &tracked {
            let prior = previous.remove(path);
            // A file with no stored hash gets one, budget permitting, so the
            // baseline converges toward exact comparisons.
            let backfill = prior
                .as_ref()
                .map(|b| b.content_hash.is_none())
                .unwrap_or(true)
                && backfill_budget > 0;
            if backfill {
                backfill_budget -= 1;
            }
            jobs.push_back(Job {
                path: path.clone(),
                tracked: *kind,
                previous: prior,
                backfill,
            });
        }
        // Anything left in `previous` is no longer referenced by the session.
        // Not a change in the file -- the session stopped pointing at it --
        // so it is dropped from the baseline without a report.
        let stale: Vec<PathBuf> = previous.into_keys().collect();

        let total = jobs.len();
        self.baseline_tracked_count = total;
        self.baseline_scan_generation = self.baseline_scan_generation.wrapping_add(1);
        if total == 0 {
            // Nothing to look at: still record the visit so the next open has
            // an anchor.
            self.session_store.update_baseline(
                session_key.clone(),
                Vec::new(),
                stale,
            );
            let now = now_unix();
            if let Some(path) = self.project_path.clone() {
                self.session_store.record_visit(
                    session_key,
                    path,
                    now,
                    now,
                    self.session_revision,
                );
            }
            return;
        }

        let queue = Arc::new(Mutex::new(jobs));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<Probe>();
        let workers = self.perf.scan_pool_workers(MAX_WORKERS).min(total.max(1));
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let cancel = Arc::clone(&cancel);
            let tx = tx.clone();
            let spawned = std::thread::Builder::new()
                .name("neowaves-session-baseline".to_string())
                .spawn(move || {
                    crate::app::threading::lower_current_thread_priority();
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let job = queue.lock().ok().and_then(|mut q| q.pop_front());
                        let Some(job) = job else { break };
                        let probe = stat_of(&job.path);
                        let stat_moved = match (job.previous.as_ref(), probe) {
                            (Some(prev), FileProbe::Present(size, mtime)) => {
                                prev.size != size || prev.mtime_ns != mtime
                            }
                            _ => false,
                        };
                        // Tier 2 only runs when tier 1 found a difference, or
                        // when this file has never been hashed and the
                        // backfill budget covered it.
                        let wanted_hash = matches!(probe, FileProbe::Present(..))
                            && (stat_moved || job.backfill);
                        let hash = if wanted_hash {
                            super::session_sync::hash_file_content(&job.path).ok()
                        } else {
                            None
                        };
                        let change = classify(job.previous.as_ref(), probe, hash.as_deref());
                        let row = next_baseline_row(
                            job.previous.as_ref(),
                            job.tracked,
                            probe,
                            wanted_hash,
                            hash,
                            now_unix(),
                        );
                        if tx
                            .send(Probe {
                                path: job.path,
                                tracked: job.tracked,
                                probe,
                                row,
                                change,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    crate::ui_wake::wake_ui();
                });
            if spawned.is_err() {
                break;
            }
        }
        drop(tx);

        self.baseline_scan = Some(BaselineScanState {
            total,
            done: 0,
            rx,
            cancel,
            generation: self.baseline_scan_generation,
            session_key,
            rows: Vec::with_capacity(total),
            removed: stale,
            changes: Vec::new(),
            since,
            started_at: Instant::now(),
        });
    }

    pub(super) fn cancel_baseline_scan(&mut self) {
        if let Some(state) = self.baseline_scan.take() {
            state.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Apply worker results, capped per frame. Finishes the scan -- writing
    /// the new baseline and raising the report -- when the last one lands.
    pub(super) fn drain_baseline_scan(&mut self) {
        let Some(state) = self.baseline_scan.as_mut() else {
            return;
        };
        if state.generation != self.baseline_scan_generation {
            self.baseline_scan = None;
            return;
        }
        let now = now_unix();
        let mut applied = 0usize;
        let mut disconnected = false;
        while applied < DRAIN_PER_FRAME {
            match state.rx.try_recv() {
                Ok(probe) => {
                    applied += 1;
                    state.done += 1;
                    // Only a file we established to be gone leaves the
                    // baseline. One we merely could not read keeps its row,
                    // so the next scan still has something to compare with.
                    if probe.probe == FileProbe::Missing {
                        state.removed.push(probe.path.clone());
                    } else if let Some(row) = probe.row {
                        // `None` means "leave the stored row as it is" --
                        // see `next_baseline_row`.
                        state.rows.push((probe.path.clone(), row));
                    }
                    if let Some(kind) = probe.change {
                        let size = match probe.probe {
                            FileProbe::Present(size, _) => size,
                            _ => 0,
                        };
                        state.changes.push(FileChange {
                            path: probe.path,
                            kind,
                            tracked: probe.tracked,
                            size,
                            detected_at: now,
                        });
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let finished = disconnected || state.done >= state.total;
        if !finished {
            return;
        }
        let Some(state) = self.baseline_scan.take() else {
            return;
        };
        self.finish_baseline_scan(state, now);
    }

    fn finish_baseline_scan(&mut self, state: BaselineScanState, now: i64) {
        let elapsed = state.started_at.elapsed();
        let BaselineScanState {
            session_key,
            rows,
            removed,
            mut changes,
            since,
            ..
        } = state;
        self.debug_log(format!(
            "session file check finished in {:.1}s ({} files)",
            elapsed.as_secs_f32(),
            self.baseline_tracked_count
        ));

        self.session_store
            .update_baseline(session_key.clone(), rows, removed);
        if let Some(path) = self.project_path.clone() {
            self.session_store
                .record_visit(session_key, path, now, now, self.session_revision);
        }

        // A session this user has never opened has no "since" to report
        // against, and every file in it would show up as Added. Record the
        // baseline and say nothing.
        let Some(since) = since else {
            self.debug_log(format!(
                "session baseline recorded for the first time ({} files)",
                self.baseline_tracked_count
            ));
            return;
        };
        if changes.is_empty() {
            self.debug_log("no referenced files changed since the last open".to_string());
            return;
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        let summary = summarize(&changes);
        self.debug_log(format!("{summary} since {}", format_stamp(since)));
        self.push_toast(
            super::types::ToastSeverity::Warning,
            format!("{summary} since you last opened this session ({})", format_stamp(since)),
        );
        self.session_file_changes = Some(super::types::SessionFileChanges {
            since,
            changes,
        });
    }
}

impl super::WavesPreviewer {
    /// A referenced file changed while the session was open -- the folder
    /// watch saw it, or this app wrote it.
    ///
    /// Re-record it now, with the time it was noticed. Without this the same
    /// change would be reported again on the next open, as though the user
    /// had not already watched it happen.
    pub(super) fn note_session_file_changed(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() || self.project_path.is_none() {
            return;
        }
        if !self.session_store.is_enabled() {
            return;
        }
        // Membership is answered from the path index, not by building the
        // tracked list: the watch fires every few seconds, and this list can
        // hold a hundred thousand rows.
        let jobs: Vec<(PathBuf, TrackedKind)> = paths
            .into_iter()
            .filter_map(|path| {
                // Virtual rows carry a path that names no file on disk, so
                // they must not be tracked -- a stat would fail and report
                // them as removed.
                let is_source_file = self
                    .item_for_path(&path)
                    .map(|item| item.source == super::types::MediaSource::File)
                    .unwrap_or(false);
                if is_source_file {
                    return Some((path, TrackedKind::Audio));
                }
                self.external_sources
                    .iter()
                    .any(|source| source.path == path)
                    .then_some((path, TrackedKind::ExternalData))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let Some(session_path) = self.project_path.clone() else {
            return;
        };
        let key = super::session_store::session_key(self.session_id.as_deref(), &session_path);
        let (tx, rx) = std::sync::mpsc::channel::<(Vec<(PathBuf, FileBaseline)>, Vec<PathBuf>)>();
        let spawned = std::thread::Builder::new()
            .name("neowaves-session-baseline-note".to_string())
            .spawn(move || {
                let now = now_unix();
                let mut rows = Vec::new();
                let mut removed = Vec::new();
                for (path, kind) in jobs {
                    match stat_of(&path) {
                        FileProbe::Missing => removed.push(path),
                        // We learned nothing about it, so there is nothing
                        // worth writing. Whatever the store holds is at
                        // least as good as what we could put there.
                        FileProbe::Unreadable => {}
                        FileProbe::Present(size, mtime_ns) => {
                            // Same rule as the scan: only advance the row
                            // when the new content is actually known. A row
                            // with no hash would replace one that may still
                            // carry a good one, and nothing would re-hash it
                            // because the stat would then match.
                            let Some(hash) =
                                super::session_sync::hash_file_content(&path).ok()
                            else {
                                continue;
                            };
                            rows.push((
                                path,
                                FileBaseline {
                                    kind,
                                    size,
                                    mtime_ns,
                                    content_hash: Some(hash),
                                    recorded_at: now,
                                },
                            ));
                        }
                    }
                }
                let _ = tx.send((rows, removed));
                crate::ui_wake::wake_ui();
            });
        if spawned.is_err() {
            return;
        }
        self.baseline_notes.push((key, rx));
    }

    /// Hand finished re-probes to the store. Called from the frame loop
    /// beside the scan drain.
    pub(super) fn drain_baseline_notes(&mut self) {
        if self.baseline_notes.is_empty() {
            return;
        }
        let mut still_running = Vec::new();
        for (key, rx) in std::mem::take(&mut self.baseline_notes) {
            match rx.try_recv() {
                Ok((rows, removed)) => {
                    self.session_store.update_baseline(key, rows, removed);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => still_running.push((key, rx)),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
            }
        }
        self.baseline_notes = still_running;
    }

    /// Store the document a save just replaced, so it can be got back.
    ///
    /// The bytes came free: the compare-and-swap read them to decide whether
    /// the save was allowed. History lives per user rather than beside the
    /// session, so a shared folder never fills up with other people's
    /// versions -- the shared-side insurance stays the single `.nwsess.bak`.
    pub(super) fn capture_replaced_session_version(
        &mut self,
        path: &Path,
        previous: Option<Vec<u8>>,
        revision: Option<u64>,
        saved_by: Option<String>,
        saved_at: Option<String>,
    ) {
        let Some(bytes) = previous.filter(|bytes| !bytes.is_empty()) else {
            return;
        };
        let key = super::session_store::session_key(self.session_id.as_deref(), path);
        let fingerprint = super::session_sync::SessionFingerprint::of_bytes(&bytes);
        self.session_store.capture_history(
            key,
            path.to_path_buf(),
            revision,
            saved_by,
            saved_at,
            fingerprint.short_hex(),
            bytes,
        );
    }

    /// Ask the store for this session's stored versions and open the window.
    pub(super) fn open_session_history_window(&mut self) {
        let Some(path) = self.project_path.clone() else {
            self.push_toast(
                super::types::ToastSeverity::Info,
                "No session is open",
            );
            return;
        };
        if !self.session_store.is_enabled() {
            self.push_toast(
                super::types::ToastSeverity::Info,
                "Local session history is unavailable on this machine",
            );
            return;
        }
        let key = super::session_store::session_key(self.session_id.as_deref(), &path);
        self.session_history_entries.clear();
        self.session_history_request = self.session_store.list_history(key);
        self.show_session_history_window = true;
    }

    /// Fetch a stored version's bytes; `apply_session_history_bytes` does the
    /// work when they arrive.
    pub(super) fn request_session_history(
        &mut self,
        id: i64,
        intent: super::types::SessionHistoryIntent,
    ) {
        let Some(request) = self.session_store.restore_history(id) else {
            return;
        };
        self.session_history_pending = Some((request, intent));
    }

    fn apply_session_history_bytes(
        &mut self,
        bytes: Vec<u8>,
        intent: super::types::SessionHistoryIntent,
    ) {
        match intent {
            super::types::SessionHistoryIntent::SaveAs(target) => {
                match super::session_sync::retry_shared_io(|| std::fs::write(&target, &bytes)) {
                    Ok(()) => self.push_toast(
                        super::types::ToastSeverity::Info,
                        format!("Wrote the stored version to {}", target.display()),
                    ),
                    Err(err) => self.push_toast(
                        super::types::ToastSeverity::Error,
                        format!("Could not write {}: {err}", target.display()),
                    ),
                }
            }
            super::types::SessionHistoryIntent::Restore => {
                let Some(path) = self.project_path.clone() else {
                    return;
                };
                // Force, because the point is to replace what is there. The
                // document being replaced goes into history on the way past,
                // so restoring is itself undoable.
                match self.restore_session_document(&path, &bytes) {
                    Ok(()) => {
                        self.push_toast(
                            super::types::ToastSeverity::Info,
                            "Restored an earlier version of this session",
                        );
                        self.show_session_history_window = false;
                        self.queue_project_open(path);
                    }
                    Err(err) => self.push_toast(
                        super::types::ToastSeverity::Error,
                        format!("Restore failed: {err}"),
                    ),
                }
            }
        }
    }

    /// Write a stored version over the session file, keeping what it
    /// replaces.
    fn restore_session_document(&mut self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        let current = super::session_sync::read_session_state(path)
            .map_err(|err| format!("read {}: {err}", path.display()))?;
        let nonce = Self::save_nonce();
        let temp = path.with_extension(format!("nwsess.{nonce}.tmp"));
        super::session_sync::retry_shared_io(|| std::fs::write(&temp, bytes))
            .map_err(|e| e.to_string())?;
        if let Err(err) = super::session_sync::atomic_replace_file(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(err.to_string());
        }
        if let Some(previous) = current.bytes() {
            let stamp = current.stamp();
            self.capture_replaced_session_version(
                path,
                Some(previous.to_vec()),
                stamp.revision,
                stamp.saved_by,
                stamp.saved_at,
            );
        }
        // The document on disk is no longer the one we had loaded; the
        // reopen that follows re-establishes the fingerprint.
        self.session_disk_fingerprint = None;
        Ok(())
    }
}

/// "12 source files changed", "3 changed, 1 added" -- whatever actually
/// happened, in one line.
pub(super) fn summarize(changes: &[FileChange]) -> String {
    let count = |kind: ChangeKind| changes.iter().filter(|c| c.kind == kind).count();
    let changed = count(ChangeKind::Changed);
    let added = count(ChangeKind::Added);
    let removed = count(ChangeKind::Removed);
    let unreadable = count(ChangeKind::Unreadable);
    let mut parts = Vec::new();
    if changed > 0 {
        parts.push(format!("{changed} changed"));
    }
    if added > 0 {
        parts.push(format!("{added} added"));
    }
    if removed > 0 {
        parts.push(format!("{removed} removed"));
    }
    if unreadable > 0 {
        parts.push(format!("{unreadable} unreadable"));
    }
    if parts.is_empty() {
        return "No referenced files changed".to_string();
    }
    let noun = if changes.len() == 1 { "file" } else { "files" };
    format!("{} referenced {noun}: {}", changes.len(), parts.join(", "))
}

/// Unix seconds as a local timestamp the user can match against their own
/// working day.
pub(super) fn format_stamp(unix: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(unix, 0) {
        chrono::offset::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => "an earlier session".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(size: u64, mtime: u128, hash: Option<&str>) -> FileBaseline {
        FileBaseline {
            kind: TrackedKind::Audio,
            size,
            mtime_ns: mtime,
            content_hash: hash.map(str::to_string),
            recorded_at: 0,
        }
    }

    #[test]
    fn an_untouched_file_is_not_reported_and_needs_no_hash() {
        let previous = base(100, 5, Some("abc"));
        assert_eq!(classify(Some(&previous), FileProbe::Present(100, 5), None), None);
    }

    #[test]
    fn a_file_touched_without_changing_its_bytes_is_not_reported() {
        // The point of the second tier: copied back, restored from a backup,
        // or re-exported byte-identically. The mtime moved; the audio did not.
        let previous = base(100, 5, Some("abc"));
        assert_eq!(classify(Some(&previous), FileProbe::Present(100, 9), Some("abc")), None);
    }

    #[test]
    fn a_file_whose_bytes_changed_is_reported() {
        let previous = base(100, 5, Some("abc"));
        assert_eq!(
            classify(Some(&previous), FileProbe::Present(120, 9), Some("def")),
            Some(ChangeKind::Changed)
        );
    }

    #[test]
    fn a_stat_change_with_no_stored_hash_is_reported_conservatively() {
        // Nothing to compare against, so we cannot prove the bytes are the
        // same. Saying nothing would be the wrong way to be wrong.
        let previous = base(100, 5, None);
        assert_eq!(
            classify(Some(&previous), FileProbe::Present(100, 9), Some("abc")),
            Some(ChangeKind::Changed)
        );
    }

    #[test]
    fn a_new_reference_is_added_and_a_vanished_one_is_removed() {
        assert_eq!(classify(None, FileProbe::Present(10, 1), None), Some(ChangeKind::Added));
        let previous = base(10, 1, Some("abc"));
        assert_eq!(classify(Some(&previous), FileProbe::Missing, None), Some(ChangeKind::Removed));
    }

    #[test]
    fn a_file_that_was_never_there_and_still_is_not_says_nothing() {
        assert_eq!(classify(None, FileProbe::Missing, None), None);
    }

    // ---- Which baseline row gets stored -----------------------------

    #[test]
    fn an_unchanged_file_keeps_the_hash_it_already_had() {
        // The regression that made the second tier useless: an open where
        // nothing moved used to overwrite the row with "no hash", so the
        // next byte-identical rewrite had nothing to compare against.
        let previous = base(100, 5, Some("abc"));
        let row = next_baseline_row(
            Some(&previous),
            TrackedKind::Audio,
            FileProbe::Present(100, 5),
            false, // tier 1 settled it; no hash was taken
            None,
            999,
        )
        .expect("an existing file still gets a row");
        assert_eq!(
            row.content_hash.as_deref(),
            Some("abc"),
            "the stored hash still describes these bytes and must survive"
        );
        assert_eq!(
            row.recorded_at, previous.recorded_at,
            "nothing was detected, so the detection time must not move"
        );
    }

    #[test]
    fn a_file_that_could_not_be_hashed_keeps_its_known_hash() {
        // Unreadable file, or a share that dropped mid-scan. Advancing the
        // row would swap a good hash for nothing -- and because the stored
        // stat would then match, nothing would ever hash it again.
        let previous = base(100, 5, Some("abc"));
        let row = next_baseline_row(
            Some(&previous),
            TrackedKind::Audio,
            FileProbe::Present(120, 9),
            true, // we wanted a hash
            None, // ...and did not get one
            999,
        );
        assert!(
            row.is_none(),
            "a failed read must leave the stored row alone for the next scan"
        );
    }

    #[test]
    fn a_successful_hash_advances_the_row() {
        let previous = base(100, 5, Some("abc"));
        let row = next_baseline_row(
            Some(&previous),
            TrackedKind::Audio,
            FileProbe::Present(120, 9),
            true,
            Some("def".to_string()),
            999,
        )
        .expect("row");
        assert_eq!(row.content_hash.as_deref(), Some("def"));
        assert_eq!(row.size, 120);
        assert_eq!(row.mtime_ns, 9);
        assert_eq!(row.recorded_at, 999, "this is a fresh detection");
    }

    #[test]
    fn a_new_file_is_recorded_even_when_it_could_not_be_hashed() {
        // Nothing to preserve, so recording the stat is still progress.
        let row = next_baseline_row(None, TrackedKind::Audio, FileProbe::Present(10, 1), true, None, 999)
            .expect("row");
        assert_eq!(row.content_hash, None);
        assert_eq!(row.size, 10);
    }

    #[test]
    fn a_missing_file_produces_no_row() {
        let previous = base(100, 5, Some("abc"));
        assert!(
            next_baseline_row(Some(&previous), TrackedKind::Audio, FileProbe::Missing, false, None, 999)
                .is_none(),
            "a vanished file is a removal, not a row"
        );
    }

    #[test]
    fn a_backfilled_hash_is_stored_without_claiming_a_change() {
        // The "hold hashes in advance" pass: stat unchanged, but this file
        // had never been hashed.
        let previous = base(100, 5, None);
        let row = next_baseline_row(
            Some(&previous),
            TrackedKind::Audio,
            FileProbe::Present(100, 5),
            true,
            Some("abc".to_string()),
            999,
        )
        .expect("row");
        assert_eq!(row.content_hash.as_deref(), Some("abc"));
        assert_eq!(
            classify(Some(&previous), FileProbe::Present(100, 5), Some("abc")),
            None,
            "backfilling a hash is not a change"
        );
    }

    // ---- A file we could not read is not a file that was deleted -------

    #[test]
    fn an_unreadable_file_is_not_reported_as_removed() {
        // Collapsing every stat error into "gone" claimed a file had been
        // deleted when it was only unreadable -- a permission error, or a
        // share that dropped mid-scan.
        let previous = base(100, 5, Some("abc"));
        assert_eq!(
            classify(Some(&previous), FileProbe::Unreadable, None),
            Some(ChangeKind::Unreadable),
            "we did not establish it was gone, and must not say so"
        );
    }

    #[test]
    fn an_unreadable_file_keeps_its_baseline_row() {
        // The follow-on damage: being treated as removed dropped the row, so
        // the next open reported the same file again as newly added, with
        // its hash gone.
        let previous = base(100, 5, Some("abc"));
        assert!(
            next_baseline_row(
                Some(&previous),
                TrackedKind::Audio,
                FileProbe::Unreadable,
                false,
                None,
                999,
            )
            .is_none(),
            "nothing was learned, so the stored row must be left alone"
        );
    }

    #[test]
    fn an_unreadable_file_is_reported_even_when_it_is_new_to_us() {
        // No baseline to protect, but the user still wants to know a file
        // this session points at cannot be read.
        assert_eq!(
            classify(None, FileProbe::Unreadable, None),
            Some(ChangeKind::Unreadable)
        );
    }

    #[test]
    fn a_genuinely_missing_file_is_still_reported_as_removed() {
        // The case the three-way split must not regress.
        let previous = base(100, 5, Some("abc"));
        assert_eq!(
            classify(Some(&previous), FileProbe::Missing, None),
            Some(ChangeKind::Removed)
        );
    }

    #[test]
    fn the_summary_counts_unreadable_files_separately() {
        let change = |kind| FileChange {
            path: PathBuf::from("/a.wav"),
            kind,
            tracked: TrackedKind::Audio,
            size: 1,
            detected_at: 0,
        };
        assert_eq!(
            summarize(&[change(ChangeKind::Unreadable)]),
            "1 referenced file: 1 unreadable"
        );
    }

    #[test]
    fn the_summary_names_what_actually_happened() {
        let change = |kind| FileChange {
            path: PathBuf::from("/a.wav"),
            kind,
            tracked: TrackedKind::Audio,
            size: 1,
            detected_at: 0,
        };
        assert_eq!(
            summarize(&[change(ChangeKind::Changed)]),
            "1 referenced file: 1 changed"
        );
        let mixed = [
            change(ChangeKind::Changed),
            change(ChangeKind::Added),
            change(ChangeKind::Removed),
        ];
        assert_eq!(
            summarize(&mixed),
            "3 referenced files: 1 changed, 1 added, 1 removed"
        );
        assert_eq!(summarize(&[]), "No referenced files changed");
    }

    #[test]
    fn a_stat_reads_size_and_mtime_of_a_real_file() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "neowaves_baseline_stat_{}_{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&path, b"twelve bytes").expect("write fixture");
        let FileProbe::Present(size, mtime) = stat_of(&path) else {
            panic!("a readable file must probe as present");
        };
        assert_eq!(size, 12);
        assert!(mtime > 0);

        // A path *below* a plain file: not found, and not readable either.
        // The distinction that matters is that it is not reported as a file
        // that used to be there and has been deleted.
        let under_a_file = path.join("child.wav");
        assert_eq!(
            stat_of(&under_a_file),
            FileProbe::Unreadable,
            "an error that is not NotFound must not be read as a deletion"
        );

        std::fs::remove_file(&path).expect("cleanup");
        assert_eq!(
            stat_of(&path),
            FileProbe::Missing,
            "a genuinely absent file is missing, not unreadable"
        );
    }
}
