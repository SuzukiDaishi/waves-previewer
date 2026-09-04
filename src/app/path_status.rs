//! Background "does this path exist" service.
//!
//! The list greys out rows whose file is gone, and a few other surfaces ask
//! the same question. Answering it means a `stat`, and on a network share a
//! single `stat` against an unresponsive server blocks for the SMB timeout --
//! tens of seconds. One of those on the UI thread is a hung window, so no
//! per-frame budget is small enough: the count has to be zero.
//!
//! So the UI never stats. It reads this cache, and a miss returns
//! [`PathStatus::Unknown`] while a low-priority worker takes the syscall.
//! Callers render `Unknown` the same as `Present` -- on a file server almost
//! every path does exist, so assuming otherwise would flash most of the list
//! grey on every open.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rustc_hash::{FxHashMap, FxHashSet};

/// What we currently know about a path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathStatus {
    /// Not probed yet, or the answer expired. Render optimistically.
    Unknown,
    Present,
    Missing,
}

impl PathStatus {
    /// How a caller that only distinguishes "grey this out" should read it.
    /// `Unknown` counts as present: see the module comment.
    pub fn treat_as_present(self) -> bool {
        !matches!(self, PathStatus::Missing)
    }
}

/// Answers older than this are re-probed. Remote paths get a much longer
/// window: the probe is expensive there, and a file on a share is far more
/// likely to be missing because the *link* is down than because the file
/// was deleted -- re-asking every couple of seconds just adds load.
const LOCAL_TTL: Duration = Duration::from_secs(2);
const REMOTE_TTL: Duration = Duration::from_secs(30);

/// Cap the cache so a million-row list cannot grow it without bound.
const MAX_CACHED: usize = 100_000;
/// Cap the queue too: past this the oldest requests are dropped, and the
/// rows that still want them re-request on the next frame they are drawn.
const MAX_QUEUED: usize = 4_096;

struct Queue {
    pending: Mutex<QueueInner>,
    ready: Condvar,
    stop: AtomicBool,
}

struct QueueInner {
    order: VecDeque<PathBuf>,
    /// Membership test so a row redrawn every frame enqueues once, not once
    /// per frame.
    queued: FxHashSet<PathBuf>,
}

/// Cache plus the worker that fills it.
pub struct PathStatusService {
    cache: FxHashMap<PathBuf, (PathStatus, Instant)>,
    /// Paths handed to the worker but not yet answered. Keeps a row from
    /// re-queueing while its probe is in flight.
    inflight: FxHashSet<PathBuf>,
    queue: Arc<Queue>,
    rx: std::sync::mpsc::Receiver<(PathBuf, bool)>,
    /// Probes served from cache vs. queued, for the Debug window.
    queued_total: u64,
    peak_queued: usize,
}

impl Default for PathStatusService {
    fn default() -> Self {
        Self::new()
    }
}

impl PathStatusService {
    pub fn new() -> Self {
        let queue = Arc::new(Queue {
            pending: Mutex::new(QueueInner {
                order: VecDeque::new(),
                queued: FxHashSet::default(),
            }),
            ready: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let (tx, rx) = std::sync::mpsc::sync_channel(256);
        let worker_queue = Arc::clone(&queue);
        let spawned = std::thread::Builder::new()
            .name("neowaves-path-status".to_string())
            .spawn(move || {
                crate::app::threading::lower_current_thread_priority();
                loop {
                    let path = {
                        let mut inner = worker_queue
                            .pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        loop {
                            if worker_queue.stop.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Some(path) = inner.order.pop_front() {
                                inner.queued.remove(&path);
                                break path;
                            }
                            let (guard, _) = worker_queue
                                .ready
                                .wait_timeout(inner, Duration::from_millis(500))
                                .unwrap_or_else(|e| e.into_inner());
                            inner = guard;
                        }
                    };
                    // The blocking syscall, on this thread and nowhere else.
                    let exists = path.is_file();
                    if tx.send((path, exists)).is_err() {
                        return;
                    }
                    crate::ui_wake::wake_ui();
                }
            });
        if spawned.is_err() {
            // No worker: `status()` still returns Unknown forever, which
            // renders every row normally. Degraded, but never blocking.
            queue.stop.store(true, Ordering::Relaxed);
        }
        Self {
            cache: FxHashMap::default(),
            inflight: FxHashSet::default(),
            queue,
            rx,
            queued_total: 0,
            peak_queued: 0,
        }
    }

    fn ttl_for(path: &Path) -> Duration {
        if crate::audio_io::is_remote_file_path(path) {
            REMOTE_TTL
        } else {
            LOCAL_TTL
        }
    }

    /// What we know about `path`, without ever touching the filesystem.
    ///
    /// A miss or a stale entry queues a probe and returns the last answer
    /// (or `Unknown` when there is none). This is the only method UI code
    /// should call.
    pub fn status(&mut self, path: &Path) -> PathStatus {
        let known = self.cache.get(path).copied();
        let stale = match known {
            Some((_, checked_at)) => checked_at.elapsed() >= Self::ttl_for(path),
            None => true,
        };
        if stale {
            self.enqueue(path);
        }
        known
            .map(|(status, _)| status)
            .unwrap_or(PathStatus::Unknown)
    }

    /// One-shot lookup for optional companion files such as subtitles.
    /// Missing is cached until explicit invalidation, so a visible row does
    /// not probe the same absent `.srt` every TTL interval.
    pub fn status_once(&mut self, path: &Path) -> PathStatus {
        if let Some((status, _)) = self.cache.get(path).copied() {
            return status;
        }
        self.enqueue(path);
        PathStatus::Unknown
    }

    fn enqueue(&mut self, path: &Path) {
        if self.inflight.contains(path) {
            return;
        }
        let mut inner = self.queue.pending.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.queued.insert(path.to_path_buf()) {
            return;
        }
        inner.order.push_back(path.to_path_buf());
        // Under pressure drop the oldest request rather than growing without
        // bound; whoever still wants it asks again next frame.
        while inner.order.len() > MAX_QUEUED {
            if let Some(dropped) = inner.order.pop_front() {
                inner.queued.remove(&dropped);
            }
        }
        self.peak_queued = self.peak_queued.max(inner.order.len());
        drop(inner);
        self.inflight.insert(path.to_path_buf());
        self.queued_total = self.queued_total.saturating_add(1);
        self.queue.ready.notify_one();
    }

    /// Take answers the worker has produced. Returns how many landed, so the
    /// frame loop can decide whether a repaint is owed.
    #[cfg(test)]
    pub fn drain(&mut self, max: usize) -> usize {
        self.drain_for(max, Duration::MAX)
    }

    pub fn drain_for(&mut self, max: usize, budget: Duration) -> usize {
        let started = Instant::now();
        let mut applied = 0usize;
        while applied < max {
            if applied > 0 && started.elapsed() >= budget {
                break;
            }
            match self.rx.try_recv() {
                Ok((path, exists)) => {
                    self.inflight.remove(&path);
                    if self.cache.len() >= MAX_CACHED {
                        self.cache.clear();
                    }
                    let status = if exists {
                        PathStatus::Present
                    } else {
                        PathStatus::Missing
                    };
                    self.cache.insert(path, (status, Instant::now()));
                    applied += 1;
                }
                Err(_) => break,
            }
        }
        applied
    }

    /// Forget one path (after a rename or delete, where the app already
    /// knows the answer changed and should not wait out the TTL).
    pub fn invalidate(&mut self, path: &Path) {
        self.cache.remove(path);
    }

    pub fn clear(&mut self) {
        self.cache.clear();
        self.inflight.clear();
        let mut inner = self.queue.pending.lock().unwrap_or_else(|e| e.into_inner());
        inner.order.clear();
        inner.queued.clear();
    }

    /// True while the worker still owes answers, so the frame loop knows not
    /// to go to sleep on a list that is still resolving.
    pub fn has_pending(&self) -> bool {
        !self.inflight.is_empty()
    }

    pub fn pending_count(&self) -> usize {
        self.inflight.len()
    }

    pub fn queued_total(&self) -> u64 {
        self.queued_total
    }

    pub fn peak_queued(&self) -> usize {
        self.peak_queued
    }

    /// Seed an answer the app already knows (the session parse worker stats
    /// every referenced path, so the list should not re-ask for those).
    pub fn preload(&mut self, path: &Path, exists: bool) {
        if self.cache.len() >= MAX_CACHED {
            return;
        }
        let status = if exists {
            PathStatus::Present
        } else {
            PathStatus::Missing
        };
        self.cache
            .insert(path.to_path_buf(), (status, Instant::now()));
    }
}

impl Drop for PathStatusService {
    fn drop(&mut self) {
        self.queue.stop.store(true, Ordering::Relaxed);
        self.queue.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "neowaves_path_status_{tag}_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Wait for the worker without ever blocking on it forever: the point of
    /// the service is that nothing waits, so the test bounds its own wait.
    fn settle(service: &mut PathStatusService) {
        for _ in 0..200 {
            service.drain(64);
            if !service.has_pending() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("path status worker did not answer within 2s");
    }

    #[test]
    fn a_first_look_never_stats_and_reports_unknown() {
        let dir = temp_dir("first_look");
        let missing = dir.join("nope.wav");
        let mut service = PathStatusService::new();

        // The call the UI makes. It must return immediately with no answer.
        assert_eq!(service.status(&missing), PathStatus::Unknown);
        // Unknown renders like a present file, so the list does not flash.
        assert!(PathStatus::Unknown.treat_as_present());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_worker_answers_present_and_missing() {
        let dir = temp_dir("answers");
        let present = dir.join("here.wav");
        let missing = dir.join("gone.wav");
        std::fs::write(&present, b"x").expect("write fixture");
        let mut service = PathStatusService::new();

        assert_eq!(service.status(&present), PathStatus::Unknown);
        assert_eq!(service.status(&missing), PathStatus::Unknown);
        settle(&mut service);

        assert_eq!(service.status(&present), PathStatus::Present);
        assert_eq!(service.status(&missing), PathStatus::Missing);
        assert!(!PathStatus::Missing.treat_as_present());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_row_redrawn_every_frame_queues_once() {
        let dir = temp_dir("dedupe");
        let path = dir.join("row.wav");
        let mut service = PathStatusService::new();

        // A visible row asks on every frame while its probe is in flight.
        for _ in 0..500 {
            service.status(&path);
        }
        assert_eq!(
            service.queued_total(),
            1,
            "an in-flight path must not be re-queued on every frame"
        );
        settle(&mut service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_fresh_answer_is_served_from_cache_without_requeueing() {
        let dir = temp_dir("cached");
        let path = dir.join("cached.wav");
        std::fs::write(&path, b"x").expect("write fixture");
        let mut service = PathStatusService::new();

        service.status(&path);
        settle(&mut service);
        let after_first = service.queued_total();

        for _ in 0..100 {
            assert_eq!(service.status(&path), PathStatus::Present);
        }
        assert_eq!(
            service.queued_total(),
            after_first,
            "a fresh cache entry must not queue more work"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_one_shot_companion_is_not_requeued() {
        let dir = temp_dir("one_shot_missing");
        let path = dir.join("clip.srt");
        let mut service = PathStatusService::new();

        assert_eq!(service.status_once(&path), PathStatus::Unknown);
        settle(&mut service);
        assert_eq!(service.status_once(&path), PathStatus::Missing);
        let after_first = service.queued_total();
        for _ in 0..500 {
            assert_eq!(service.status_once(&path), PathStatus::Missing);
        }
        assert_eq!(service.queued_total(), after_first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn preloaded_answers_skip_the_worker() {
        let dir = temp_dir("preload");
        let path = dir.join("known.wav");
        let mut service = PathStatusService::new();

        // The session parse worker already statted this one.
        service.preload(&path, false);

        assert_eq!(service.status(&path), PathStatus::Missing);
        assert_eq!(
            service.queued_total(),
            0,
            "a preloaded answer should not need a probe"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalidate_forces_a_fresh_probe() {
        let dir = temp_dir("invalidate");
        let path = dir.join("renamed.wav");
        let mut service = PathStatusService::new();
        service.preload(&path, true);
        assert_eq!(service.status(&path), PathStatus::Present);

        service.invalidate(&path);

        assert_eq!(service.status(&path), PathStatus::Unknown);
        assert_eq!(service.queued_total(), 1);
        settle(&mut service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_remote_path_gets_the_longer_ttl() {
        // UNC form is recognised on every platform, so this is not
        // Windows-only. The point is that a share is not re-probed every two
        // seconds just because the link is slow.
        let remote = PathBuf::from(r"\\server\share\clip.wav");
        let local = std::env::temp_dir().join("local_clip.wav");
        assert_eq!(PathStatusService::ttl_for(&remote), REMOTE_TTL);
        assert_eq!(PathStatusService::ttl_for(&local), LOCAL_TTL);
        assert!(REMOTE_TTL > LOCAL_TTL);
    }

    #[test]
    fn clear_drops_cache_and_queue() {
        let dir = temp_dir("clear");
        let path = dir.join("x.wav");
        let mut service = PathStatusService::new();
        service.preload(&path, true);
        service.status(&dir.join("y.wav"));

        service.clear();

        // Check the queue first: `status()` on a cleared cache is itself a
        // miss and would re-queue, hiding whether `clear` emptied anything.
        assert!(!service.has_pending(), "clear should drop in-flight probes");
        assert_eq!(service.status(&path), PathStatus::Unknown);
        let _ = std::fs::remove_dir_all(dir);
    }
}
