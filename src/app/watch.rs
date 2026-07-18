//! Polling folder watch: a low-priority thread re-walks the list root on an
//! interval, diffs (path -> mtime, len) snapshots, and emits debounced
//! Added/Removed/Modified batches. Polling was chosen over the `notify`
//! crate deliberately — no new dependency, uniform behavior on network
//! drives — and the event interface would let a notify backend slot in
//! later without touching the consumers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    Added(PathBuf),
    Removed(PathBuf),
    Modified(PathBuf),
}

impl WatchEvent {
    pub fn path(&self) -> &Path {
        match self {
            WatchEvent::Added(p) | WatchEvent::Removed(p) | WatchEvent::Modified(p) => p,
        }
    }
}

/// (mtime, len) per supported audio file under the root.
pub type WatchSnapshot = HashMap<PathBuf, (Option<SystemTime>, u64)>;

/// Diff two snapshots into events, sorted by path for determinism.
pub fn diff_snapshots(old: &WatchSnapshot, new: &WatchSnapshot) -> Vec<WatchEvent> {
    let mut out = Vec::new();
    for (path, meta) in new {
        match old.get(path) {
            None => out.push(WatchEvent::Added(path.clone())),
            Some(prev) if prev != meta => out.push(WatchEvent::Modified(path.clone())),
            _ => {}
        }
    }
    for path in old.keys() {
        if !new.contains_key(path) {
            out.push(WatchEvent::Removed(path.clone()));
        }
    }
    out.sort_by(|a, b| a.path().cmp(b.path()));
    out
}

// ---- Self-write suppression ------------------------------------------------
// Writers inside the app (exports, metadata splices, gain overwrites) note
// their target here; the watch drain drops events for paths written within
// the TTL so the app doesn't react to its own output.

const SELF_WRITE_TTL: Duration = Duration::from_secs(15);

fn self_writes() -> &'static Mutex<HashMap<PathBuf, Instant>> {
    static REG: OnceLock<Mutex<HashMap<PathBuf, Instant>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn note_self_write(path: &Path) {
    let mut reg = self_writes().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    reg.retain(|_, t| now.duration_since(*t) < SELF_WRITE_TTL);
    reg.insert(path.to_path_buf(), now);
}

pub fn recently_self_written(path: &Path) -> bool {
    let reg = self_writes().lock().unwrap_or_else(|e| e.into_inner());
    reg.get(path)
        .map(|t| t.elapsed() < SELF_WRITE_TTL)
        .unwrap_or(false)
}

// ---- Watch thread ----------------------------------------------------------

pub struct FolderWatch {
    rx: std::sync::mpsc::Receiver<Vec<WatchEvent>>,
    // Snapshot copies for the respawn check only — the thread captured its
    // own clones, so mutating these would NOT retarget the running poller.
    root: PathBuf,
    interval_ms: u64,
    stop: Arc<AtomicBool>,
    /// Set by the UI thread while bulk operations / scans run; the poller
    /// idles and REBASELINES on resume (changes made during the pause are
    /// treated as self-caused or already covered by the operation itself).
    suspend: Arc<AtomicBool>,
}

impl Drop for FolderWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn scan_snapshot(root: &Path, skip_dotfiles: bool) -> WatchSnapshot {
    let mut snap = WatchSnapshot::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            !crate::app::WavesPreviewer::is_internal_temp_cache_path(e.path())
                && (!skip_dotfiles || !crate::app::WavesPreviewer::is_dotfile_path(e.path()))
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let supported = entry
            .path()
            .extension()
            .and_then(|s| s.to_str())
            .map(crate::audio_io::is_supported_extension)
            .unwrap_or(false);
        if !supported {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        snap.insert(
            entry.into_path(),
            (meta.modified().ok(), meta.len()),
        );
    }
    snap
}

/// Spawn the poller. The first walk only establishes the baseline; events
/// flow from the second walk on. Debounce: a batch flushes on the first
/// quiet poll after changes, or once it has aged past ~4 poll intervals —
/// so bulk copies arrive as one batch, but one continuously-rewritten file
/// can never starve unrelated events forever. Pending events are merged per
/// path (latest wins), so a hot file contributes one entry, not one per
/// poll. After a suspend (bulk op / rescan) the poller REBASELINES instead
/// of diffing across the pause.
pub fn spawn_folder_watch(root: PathBuf, interval_ms: u64, skip_dotfiles: bool) -> FolderWatch {
    let stop = Arc::new(AtomicBool::new(false));
    let suspend = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<Vec<WatchEvent>>();
    {
        let root = root.clone();
        let stop = Arc::clone(&stop);
        let suspend = Arc::clone(&suspend);
        let _ = std::thread::Builder::new()
            .name("neowaves-folder-watch".into())
            .spawn(move || {
                crate::app::threading::lower_current_thread_priority();
                let max_pending_age =
                    Duration::from_millis((interval_ms.saturating_mul(4)).max(1_000));
                let mut snapshot: Option<WatchSnapshot> = None;
                let mut pending: HashMap<PathBuf, WatchEvent> = HashMap::new();
                let mut pending_since: Option<Instant> = None;
                let mut was_suspended = false;
                loop {
                    std::thread::sleep(Duration::from_millis(interval_ms.max(20)));
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if suspend.load(Ordering::Relaxed) {
                        was_suspended = true;
                        continue;
                    }
                    let new = scan_snapshot(&root, skip_dotfiles);
                    if was_suspended || snapshot.is_none() {
                        was_suspended = false;
                        snapshot = Some(new);
                        pending.clear();
                        pending_since = None;
                        continue;
                    }
                    let events = match &snapshot {
                        Some(old) => diff_snapshots(old, &new),
                        None => Vec::new(),
                    };
                    snapshot = Some(new);
                    let quiet = events.is_empty();
                    for event in events {
                        pending.insert(event.path().to_path_buf(), event);
                    }
                    if !pending.is_empty() && pending_since.is_none() {
                        pending_since = Some(Instant::now());
                    }
                    let overdue = pending_since
                        .map(|t| t.elapsed() >= max_pending_age)
                        .unwrap_or(false);
                    if !pending.is_empty() && (quiet || overdue) {
                        let mut batch: Vec<WatchEvent> =
                            pending.drain().map(|(_, e)| e).collect();
                        batch.sort_by(|a, b| a.path().cmp(b.path()));
                        pending_since = None;
                        if tx.send(batch).is_err() {
                            break;
                        }
                    }
                }
            });
    }
    FolderWatch {
        rx,
        root,
        interval_ms,
        stop,
        suspend,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(entries: &[(&str, u64)]) -> WatchSnapshot {
        entries
            .iter()
            .map(|(p, len)| (PathBuf::from(p), (None, *len)))
            .collect()
    }

    #[test]
    fn diff_reports_added_removed_modified_sorted() {
        let old = snap(&[("a.wav", 10), ("b.wav", 20), ("c.wav", 30)]);
        let new = snap(&[("a.wav", 10), ("b.wav", 25), ("d.wav", 40)]);
        let events = diff_snapshots(&old, &new);
        assert_eq!(
            events,
            vec![
                WatchEvent::Modified(PathBuf::from("b.wav")),
                WatchEvent::Removed(PathBuf::from("c.wav")),
                WatchEvent::Added(PathBuf::from("d.wav")),
            ]
        );
        assert!(diff_snapshots(&new, &new).is_empty());
    }

    #[test]
    fn self_write_registry_expires_by_ttl() {
        let p = PathBuf::from("/tmp/self_write_probe.wav");
        assert!(!recently_self_written(&p));
        note_self_write(&p);
        assert!(recently_self_written(&p));
    }
}

impl crate::app::WavesPreviewer {
    /// Per-frame driver: keep the poller matched to the current root and
    /// enabled state, mirror the busy flag into its suspend switch, and
    /// apply any event batches.
    pub(super) fn tick_folder_watch(&mut self, ctx: &egui::Context) {
        // (Re)spawn or drop so the watch always matches root + pref. A scan
        // does NOT drop the watch (that would discard its snapshot and any
        // pending batch); it suspends it below, and the poller rebaselines
        // on resume.
        let desired_active = self.watch_folder_enabled && self.root.is_some();
        let matches = match (&self.folder_watch, self.root.as_ref()) {
            (None, _) => !desired_active,
            (Some(_), None) => false,
            (Some(w), Some(root)) => {
                desired_active
                    && &w.root == root
                    && w.interval_ms == self.watch_poll_interval_ms
            }
        };
        if !matches {
            self.folder_watch = if desired_active {
                let root = self.root.clone().expect("desired_active checked root");
                self.debug_log(format!("folder watch: start {}", root.display()));
                Some(spawn_folder_watch(
                    root,
                    self.watch_poll_interval_ms,
                    self.skip_dotfiles,
                ))
            } else {
                if self.folder_watch.is_some() {
                    self.debug_log("folder watch: stop".to_string());
                }
                None
            };
        }
        let Some(watch) = &self.folder_watch else {
            return;
        };
        // Bulk operations and rescans pause polling (their writes / churn
        // would spam events); the poller rebaselines when they finish.
        let busy = self.busy_overlay_blocking()
            || self.scan_in_progress
            || self.export_state.is_some()
            || self.bulk_resample_state.is_some()
            || self.batch_loudnorm_state.is_some()
            || self.inspection_run_state.is_some();
        watch.suspend.store(busy, std::sync::atomic::Ordering::Relaxed);

        let mut batches: Vec<Vec<WatchEvent>> = Vec::new();
        while let Ok(batch) = watch.rx.try_recv() {
            batches.push(batch);
        }
        if batches.is_empty() {
            return;
        }
        let mut added: Vec<PathBuf> = Vec::new();
        let mut removed = 0usize;
        let mut removed_skipped = 0usize;
        let mut modified = 0usize;
        let mut modified_skipped = 0usize;
        for event in batches.into_iter().flatten() {
            if recently_self_written(event.path()) {
                continue;
            }
            match event {
                WatchEvent::Added(path) => {
                    if self.path_index.contains_key(&path) {
                        // Removed+recreated across polls (delete-then-copy
                        // save): the row survives, but its caches describe
                        // the old contents.
                        if self.watch_apply_modified(&path) {
                            modified += 1;
                        } else {
                            modified_skipped += 1;
                        }
                    } else {
                        added.push(path);
                    }
                }
                WatchEvent::Removed(path) => {
                    if !self.path_index.contains_key(&path) {
                        continue;
                    }
                    if path.exists() {
                        // Recreated before the drain ran: treat as modified
                        // (remove_missing_path would refuse anyway).
                        if self.watch_apply_modified(&path) {
                            modified += 1;
                        } else {
                            modified_skipped += 1;
                        }
                        continue;
                    }
                    let tab_open = self.tabs.iter().any(|t| t.path == path);
                    if tab_open {
                        removed_skipped += 1;
                        continue;
                    }
                    self.remove_missing_path(&path);
                    removed += 1;
                }
                WatchEvent::Modified(path) => {
                    if !self.path_index.contains_key(&path) {
                        continue;
                    }
                    if self.watch_apply_modified(&path) {
                        modified += 1;
                    } else {
                        modified_skipped += 1;
                    }
                }
            }
        }
        if !added.is_empty() {
            let count = self.add_files_merge(&added);
            if count > 0 {
                self.after_add_refresh();
            }
        }
        let mut parts: Vec<String> = Vec::new();
        if !added.is_empty() {
            parts.push(format!("{} added", added.len()));
        }
        if removed > 0 {
            parts.push(format!("{removed} removed"));
        }
        if modified > 0 {
            parts.push(format!("{modified} changed"));
        }
        if removed_skipped + modified_skipped > 0 {
            parts.push(format!(
                "{} open in editor (kept)",
                removed_skipped + modified_skipped
            ));
        }
        if !parts.is_empty() {
            let msg = format!("Folder changed: {}", parts.join(", "));
            self.debug_log(format!("folder watch: {msg}"));
            self.push_toast(crate::app::types::ToastSeverity::Info, msg);
            ctx.request_repaint();
        }
    }

    /// Invalidate every cached view of a file the watcher saw change and
    /// queue a metadata re-resolve. Returns false (skip) while an editor tab
    /// holds the file — the tab keeps its in-memory copy untouched.
    fn watch_apply_modified(&mut self, path: &Path) -> bool {
        if self.tabs.iter().any(|t| t.path == path) {
            return false;
        }
        if let Some(id) = self.path_index.get(path) {
            if let Some(&idx) = self.item_index.get(&id) {
                if let Some(item) = self.items.get_mut(idx) {
                    item.meta = None;
                }
            }
        }
        self.cancel_meta_for_path(path);
        self.purge_spectro_cache_entry(path);
        self.cancel_feature_analysis_for_path(path);
        self.evict_list_preview_cache_path(path);
        self.lufs_override.remove(path);
        self.sample_rate_probe_cache.remove(path);
        self.queue_meta_for_path(&path.to_path_buf(), false);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_set_watch_interval_ms(&mut self, ms: u64) {
        self.watch_poll_interval_ms = ms.max(20);
        // Drop the current poller; the next frame respawns with the new rate.
        self.folder_watch = None;
    }

    #[cfg(feature = "kittest")]
    pub fn test_watch_active(&self) -> bool {
        self.folder_watch.is_some()
    }
}
