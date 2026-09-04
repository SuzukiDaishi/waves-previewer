//! Notices when somebody else saves the session that is open here.
//!
//! On a file server the `.nwsess` in front of the user is not the one on
//! disk for very long: a colleague saves, and the window keeps showing a
//! document that no longer exists anywhere but in this process. The save
//! path already refuses to overwrite in that state (see `session_sync`), but
//! finding out at save time -- after an hour of work -- is too late to be
//! useful. This probe says so while it is still cheap to act on.
//!
//! It only reports. Reloading is the user's decision: an automatic reload
//! would throw away whatever they have not saved yet.
//!
//! Polling, like `watch.rs`, and for the same reason: no new dependency, and
//! identical behavior on a network drive, where change notification is not
//! something a client can rely on. This one watches a single file, so a pass
//! is a `stat` -- and only re-reads the body when size or mtime moved.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::session_sync::{self, SessionFingerprint, SessionStamp};

/// What a pass found.
#[derive(Clone, Debug)]
pub(super) enum SessionWatchEvent {
    /// The document on disk is no longer the one this session was read from.
    Changed {
        fingerprint: SessionFingerprint,
        stamp: SessionStamp,
    },
    /// It is gone -- moved, renamed or deleted by someone else.
    Removed,
    /// The path stopped answering: the share dropped, or permissions
    /// changed. Deliberately distinct from `Changed`, because reporting a
    /// dead link as "someone saved" would send the user to reload a session
    /// that cannot be read.
    Unreadable(String),
}

/// The file being watched and the state that counts as "unchanged".
struct WatchTarget {
    path: PathBuf,
    fingerprint: SessionFingerprint,
}

pub(super) struct SessionWatch {
    rx: std::sync::mpsc::Receiver<SessionWatchEvent>,
    target: Arc<Mutex<WatchTarget>>,
    stop: Arc<AtomicBool>,
    /// Held up while this process is itself reading or writing the file, so
    /// the probe never reports our own save back to us.
    suspend: Arc<AtomicBool>,
}

impl Drop for SessionWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl SessionWatch {
    pub fn path(&self) -> PathBuf {
        self.target
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .path
            .clone()
    }

    pub fn set_suspended(&self, suspended: bool) {
        self.suspend.store(suspended, Ordering::Relaxed);
    }

    pub fn try_recv(&self) -> Option<SessionWatchEvent> {
        self.rx.try_recv().ok()
    }
}

/// Cheap identity of a file from its metadata alone, used to decide whether
/// reading the body is worth it.
type MetaProbe = (Option<SystemTime>, u64);

fn meta_probe(path: &Path) -> std::io::Result<MetaProbe> {
    let meta = std::fs::metadata(path)?;
    Ok((meta.modified().ok(), meta.len()))
}

/// Spawn the probe. `interval_ms` is a floor; the real delay also tracks how
/// long a pass actually took, so a share that answers slowly is polled less
/// rather than being hammered while the user waits on the same link.
pub(super) fn spawn_session_watch(
    path: PathBuf,
    fingerprint: SessionFingerprint,
    interval_ms: u64,
) -> SessionWatch {
    let stop = Arc::new(AtomicBool::new(false));
    let suspend = Arc::new(AtomicBool::new(false));
    let target = Arc::new(Mutex::new(WatchTarget { path, fingerprint }));
    let (tx, rx) = std::sync::mpsc::sync_channel::<SessionWatchEvent>(2);
    {
        let stop = Arc::clone(&stop);
        let suspend = Arc::clone(&suspend);
        let target = Arc::clone(&target);
        let _ = std::thread::Builder::new()
            .name("neowaves-session-watch".into())
            .spawn(move || {
                crate::app::threading::lower_current_thread_priority();
                let mut next_sleep = Duration::from_millis(interval_ms.max(250));
                // What we last told the UI about, so a document that stays
                // changed is reported once rather than every pass.
                let mut reported: Option<SessionFingerprint> = None;
                let mut reported_missing = false;
                let mut reported_error = false;
                let mut last_meta: Option<MetaProbe> = None;
                // An SMB client can serve a stale mtime and length, so
                // metadata alone can miss a colleague's save entirely. Read
                // the body outright every so often regardless.
                const PASSES_BETWEEN_FULL_READS: u32 = 6;
                let mut passes_since_full_read = PASSES_BETWEEN_FULL_READS;
                loop {
                    std::thread::sleep(next_sleep);
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if suspend.load(Ordering::Relaxed) {
                        continue;
                    }
                    let (path, expected) = {
                        let target = target.lock().unwrap_or_else(|e| e.into_inner());
                        (target.path.clone(), target.fingerprint)
                    };
                    let started = Instant::now();
                    let probe = meta_probe(&path);
                    let send = |event: SessionWatchEvent| {
                        if tx.send(event).is_ok() {
                            // The frame loop sleeps when idle, so a message
                            // nobody wakes for is a message nobody reads.
                            crate::ui_wake::wake_ui();
                        }
                    };
                    match probe {
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                            next_sleep =
                                super::watch::next_walk_delay(interval_ms, started.elapsed());
                            if !reported_missing {
                                reported_missing = true;
                                reported = None;
                                reported_error = false;
                                send(SessionWatchEvent::Removed);
                            }
                            continue;
                        }
                        Err(err) => {
                            next_sleep =
                                super::watch::next_walk_delay(interval_ms, started.elapsed());
                            if !reported_error {
                                reported_error = true;
                                send(SessionWatchEvent::Unreadable(err.to_string()));
                            }
                            continue;
                        }
                        Ok(meta) => {
                            reported_missing = false;
                            reported_error = false;
                            // Metadata is only a hint, but when it has not
                            // moved there is usually nothing to re-read --
                            // and re-reading a session on a share every pass
                            // costs the user's own link.
                            let unchanged = last_meta.as_ref() == Some(&meta);
                            last_meta = Some(meta);
                            if unchanged && passes_since_full_read < PASSES_BETWEEN_FULL_READS {
                                passes_since_full_read += 1;
                                next_sleep =
                                    super::watch::next_walk_delay(interval_ms, started.elapsed());
                                continue;
                            }
                            passes_since_full_read = 0;
                        }
                    }

                    let state = session_sync::read_session_state(&path);
                    next_sleep = super::watch::next_walk_delay(interval_ms, started.elapsed());
                    match state {
                        Ok(session_sync::SessionDiskState::Missing) => {
                            if !reported_missing {
                                reported_missing = true;
                                reported = None;
                                send(SessionWatchEvent::Removed);
                            }
                        }
                        Ok(
                            ref state @ session_sync::SessionDiskState::Present {
                                fingerprint, ..
                            },
                        ) => {
                            if fingerprint == expected {
                                // Back in agreement: a save of ours landed,
                                // or someone reverted. Re-arm.
                                reported = None;
                            } else if reported != Some(fingerprint) {
                                reported = Some(fingerprint);
                                // Only now is the stamp worth parsing.
                                let stamp = state.stamp();
                                send(SessionWatchEvent::Changed { fingerprint, stamp });
                            }
                        }
                        Err(err) => {
                            if !reported_error {
                                reported_error = true;
                                send(SessionWatchEvent::Unreadable(err.to_string()));
                            }
                        }
                    }
                }
            });
    }
    SessionWatch {
        rx,
        target,
        stop,
        suspend,
    }
}

impl super::WavesPreviewer {
    /// Start (or restart) watching the open session for other people's
    /// saves. A no-op when no session is open.
    pub(super) fn restart_session_watch(&mut self) {
        self.session_watch = None;
        let (Some(path), Some(fingerprint)) =
            (self.project_path.clone(), self.session_disk_fingerprint)
        else {
            return;
        };
        let interval = self.perf.session_watch_interval_ms();
        self.session_watch = Some(spawn_session_watch(path, fingerprint, interval));
    }

    /// Raise the standing reload warning.
    ///
    /// Split out from the probe because noticing the change and deciding to
    /// warn about it are no longer the same moment: a comment pull gets to
    /// rule the warning out first when the only difference is what somebody
    /// said.
    pub(super) fn announce_session_changed_on_disk(
        &mut self,
        changed: super::types::SessionChangedOnDisk,
    ) {
        self.push_toast(
            super::types::ToastSeverity::Warning,
            format!("Session changed on disk — {}", changed.on_disk),
        );
        self.session_changed_on_disk = Some(changed);
    }

    pub(super) fn stop_session_watch(&mut self) {
        self.session_watch = None;
    }

    /// Drain the probe. Cheap by construction -- at most a handful of
    /// messages ever queue -- but it still runs behind the frame budget with
    /// the other background drains.
    pub(super) fn tick_session_watch(&mut self) {
        let Some(watch) = self.session_watch.as_ref() else {
            return;
        };
        // Never diff against the file while we are the ones writing it.
        // A comment write replaces the document like any save does, so it
        // counts here too -- otherwise our own post comes back as somebody
        // else's.
        let busy = self.session_save_state.is_some()
            || self.session_open_in_progress()
            || self.comment_write.is_some();
        watch.set_suspended(busy);
        if busy {
            return;
        }
        let path = watch.path();
        let mut events = Vec::new();
        while let Some(event) = watch.try_recv() {
            events.push(event);
        }
        for event in events {
            match event {
                SessionWatchEvent::Changed { stamp, fingerprint } => {
                    let who = stamp.describe();
                    self.debug_log(format!(
                        "session changed on disk: {who} ({})",
                        fingerprint.short_hex()
                    ));
                    // Held, not raised. Posting a comment rewrites the
                    // document, so every colleague talking would otherwise
                    // stand up "your session is stale, reload it" -- and a
                    // reload discards unsaved edits, which makes that the
                    // most expensive false alarm this app can produce. The
                    // pull compares the two documents with their
                    // conversations removed and decides; see
                    // `drain_comment_pull`.
                    self.session_changed_pending = Some(super::types::SessionChangedOnDisk {
                        path: path.clone(),
                        on_disk: who,
                        removed: false,
                    });
                    self.request_comment_pull();
                }
                SessionWatchEvent::Removed => {
                    // A missing file is never "they just commented", so it
                    // does not wait on a pull that could only fail.
                    self.session_changed_pending = None;
                    self.debug_log(format!("session removed on disk: {}", path.display()));
                    self.push_toast(
                        super::types::ToastSeverity::Warning,
                        "Session file was removed on disk",
                    );
                    self.session_changed_on_disk = Some(super::types::SessionChangedOnDisk {
                        path: path.clone(),
                        on_disk: "removed".to_string(),
                        removed: true,
                    });
                }
                SessionWatchEvent::Unreadable(err) => {
                    // Not a change, and not something to prompt about: the
                    // link is down, and it usually comes back on its own.
                    self.debug_log(format!("session watch could not read the session: {err}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_session(tag: &str, body: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "neowaves_session_watch_{tag}_{}_{}.nwsess",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&path, body).expect("write fixture");
        path
    }

    fn wait_for(watch: &SessionWatch, timeout: Duration) -> Option<SessionWatchEvent> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(event) = watch.try_recv() {
                return Some(event);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    #[test]
    fn an_unchanged_file_reports_nothing() {
        let path = temp_session("quiet", "version = 2\nrevision = 1\n");
        let fingerprint =
            SessionFingerprint::of_bytes(&std::fs::read(&path).expect("read fixture"));
        let watch = spawn_session_watch(path.clone(), fingerprint, 20);
        assert!(
            wait_for(&watch, Duration::from_millis(400)).is_none(),
            "a file nobody touched must not look like somebody's save"
        );
        drop(watch);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_changed_file_is_reported_once_not_every_pass() {
        let path = temp_session("changed", "version = 2\nrevision = 1\n");
        let fingerprint =
            SessionFingerprint::of_bytes(&std::fs::read(&path).expect("read fixture"));
        let watch = spawn_session_watch(path.clone(), fingerprint, 20);
        std::fs::write(&path, "version = 2\nrevision = 2\nsaved_by = \"tanaka\"\n")
            .expect("rewrite fixture");
        match wait_for(&watch, Duration::from_secs(3)) {
            Some(SessionWatchEvent::Changed { stamp, .. }) => {
                assert_eq!(stamp.revision, Some(2));
                assert_eq!(stamp.saved_by.as_deref(), Some("tanaka"));
            }
            other => panic!("expected a Changed event, got {other:?}"),
        }
        assert!(
            wait_for(&watch, Duration::from_millis(400)).is_none(),
            "the same change must not be reported on every pass"
        );
        drop(watch);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_removed_file_reports_removal_rather_than_a_change() {
        let path = temp_session("removed", "version = 2\nrevision = 1\n");
        let fingerprint =
            SessionFingerprint::of_bytes(&std::fs::read(&path).expect("read fixture"));
        let watch = spawn_session_watch(path.clone(), fingerprint, 20);
        std::fs::remove_file(&path).expect("remove fixture");
        match wait_for(&watch, Duration::from_secs(3)) {
            Some(SessionWatchEvent::Removed) => {}
            other => panic!("expected Removed, got {other:?}"),
        }
        assert!(
            wait_for(&watch, Duration::from_millis(400)).is_none(),
            "a file that stays gone must not report every pass"
        );
        drop(watch);
    }

    #[test]
    fn a_suspended_watch_stays_quiet_while_we_are_the_writer() {
        let path = temp_session("suspended", "version = 2\nrevision = 1\n");
        let fingerprint =
            SessionFingerprint::of_bytes(&std::fs::read(&path).expect("read fixture"));
        let watch = spawn_session_watch(path.clone(), fingerprint, 20);
        watch.set_suspended(true);
        std::fs::write(&path, "version = 2\nrevision = 2\n").expect("rewrite fixture");
        assert!(
            wait_for(&watch, Duration::from_millis(500)).is_none(),
            "our own save must not come back to us as someone else's"
        );
        drop(watch);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_probe_backs_off_from_its_own_measured_cost() {
        // Shared with the folder watch: a share where one pass takes 30s must
        // not be re-probed 3s later.
        assert_eq!(
            super::super::watch::next_walk_delay(5_000, Duration::from_millis(50)),
            Duration::from_millis(5_000)
        );
        assert_eq!(
            super::super::watch::next_walk_delay(5_000, Duration::from_secs(30)),
            Duration::from_secs(120)
        );
    }
}
