//! Posting to, and reading, a shared session's conversation.
//!
//! Comments live in the `.nwsess` itself, which on a file server is a
//! document with several writers and no lock. Writing one the way a normal
//! save writes would be wrong twice over: it would push the author's
//! unsaved list and editor edits out with the comment, and it would raise the
//! "somebody else saved" conflict modal every time two people typed at once.
//!
//! So a comment takes its own path to disk:
//!
//! 1. read the document **on disk**, not the one in memory;
//! 2. union the outbox into its `comments` -- the merge is keyed by id, so it
//!    is commutative and idempotent (see [`super::comments`]);
//! 3. compare-and-swap it back.
//!
//! Nothing of the author's own editing state goes along, and a CAS miss is
//! not a conflict to ask about: it means somebody else committed first, so we
//! re-read, re-merge and try again. The result converges without the user
//! ever seeing a prompt. Only when the retries run out does the comment stay
//! in the outbox, visibly unsent, for the next attempt.
//!
//! Reading is the mirror image. The session watch already notices when the
//! document on disk stops matching ours; a comment pull turns that into new
//! messages in the window instead of a warning to reload, whenever the two
//! documents differ *only* in their conversation.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use super::comments::{self, CommentAnchor, CommentAuthor, CommentRef};
use super::project::{deserialize_project, serialize_project, ProjectComment};
use super::session_sync::{self, SessionFingerprint};

/// How many times a comment write re-reads and re-merges before giving up.
///
/// Every retry is provoked by somebody else committing, so the loop only
/// spins while other people are actively saving. Five is far past what a
/// human team produces and still bounded, which matters because this runs on
/// a worker holding an outbox the user can see.
const WRITE_ATTEMPTS: usize = 5;

/// Backoff before each retry. The first is immediate: the common case is one
/// colleague's save landing in the same second, and waiting helps nobody.
const RETRY_DELAYS_MS: [u64; 4] = [0, 50, 150, 400];

/// What a committed comment write leaves behind.
pub(super) struct CommentWriteResult {
    /// The conversation as it now stands on disk -- ours unioned with theirs.
    pub comments: Vec<ProjectComment>,
    pub fingerprint: SessionFingerprint,
    pub comment_free_fingerprint: SessionFingerprint,
    pub revision: u64,
}

/// A comment write in flight.
pub(super) struct CommentWriteState {
    pub rx: mpsc::Receiver<Result<CommentWriteResult, String>>,
    #[allow(dead_code)]
    pub started_at: Instant,
    /// What went out. Returned to the outbox if the write could not commit,
    /// so nothing the user typed is lost to a dropped share.
    pub sent: Vec<ProjectComment>,
}

/// What a read of the document's conversation found.
pub(super) struct CommentPull {
    pub comments: Vec<ProjectComment>,
    pub fingerprint: SessionFingerprint,
    pub comment_free_fingerprint: SessionFingerprint,
    pub revision: Option<u64>,
}

impl crate::app::WavesPreviewer {
    /// The identity this machine posts under: the OS account name as the key,
    /// the machine name to tell two people sharing one apart, and the
    /// `display_name=` pref as the label.
    pub(crate) fn comment_author(&self) -> CommentAuthor {
        CommentAuthor::local(self.session_display_name.as_deref())
    }

    /// Whether `comment` was written by whoever is sitting here.
    pub(crate) fn comment_is_mine(&self, comment: &ProjectComment) -> bool {
        comment.author_id == self.comment_author().id
    }

    /// True while a comment is on its way to disk, or waiting to be.
    pub(crate) fn comments_pending(&self) -> usize {
        self.comment_outbox.len() + self.comment_write.as_ref().map_or(0, |w| w.sent.len())
    }

    /// True for a comment that is in this window but not yet in the document.
    pub(crate) fn comment_is_unsent(&self, id: &str) -> bool {
        self.comment_outbox.iter().any(|c| c.id == id)
            || self
                .comment_write
                .as_ref()
                .is_some_and(|w| w.sent.iter().any(|c| c.id == id))
    }

    /// Add a comment, or a reply when `parent` is set. Returns its id.
    ///
    /// The comment appears in this window immediately and reaches disk on a
    /// worker: the document may be on a share, and a keystroke's worth of
    /// text is not worth an SMB round trip on the UI thread.
    pub(crate) fn post_comment(&mut self, parent: Option<String>, body: &str) -> Option<String> {
        let body = body.trim();
        if body.is_empty() {
            return None;
        }
        let author = self.comment_author();
        let comment = ProjectComment {
            id: comments::new_comment_id(),
            parent,
            author_id: author.id,
            author_host: author.host,
            author_name: author.name,
            created_at: session_sync::now_rfc3339(),
            edited_at: None,
            rev: 0,
            body: body.to_string(),
            deleted: false,
            resolved_by: None,
            resolved_at: None,
        };
        let id = comment.id.clone();
        self.enqueue_comment(comment);
        Some(id)
    }

    /// Rewrite a comment's body. Only its own author may, and the revision
    /// bump is what lets two of their processes agree on which text won.
    pub(crate) fn edit_comment(&mut self, id: &str, body: &str) -> bool {
        let body = body.trim();
        if body.is_empty() {
            return false;
        }
        let Some(mut comment) = self.comment_by_id(id).cloned() else {
            return false;
        };
        if !self.comment_is_mine(&comment) || comment.body == body {
            return false;
        }
        comment.body = body.to_string();
        comment.rev = comment.rev.saturating_add(1);
        comment.edited_at = Some(session_sync::now_rfc3339());
        self.enqueue_comment(comment);
        true
    }

    /// Withdraw a comment. The row stays as a tombstone: dropping it would
    /// let a colleague whose copy still holds it merge the text straight back
    /// in on their next post.
    pub(crate) fn delete_comment(&mut self, id: &str) -> bool {
        let Some(mut comment) = self.comment_by_id(id).cloned() else {
            return false;
        };
        if !self.comment_is_mine(&comment) || comment.deleted {
            return false;
        }
        comment.body.clear();
        comment.deleted = true;
        comment.rev = comment.rev.saturating_add(1);
        comment.edited_at = Some(session_sync::now_rfc3339());
        self.enqueue_comment(comment);
        true
    }

    /// Mark a thread settled, or reopen it. Anyone may: a thread is the
    /// team's, not the author's.
    pub(crate) fn set_thread_resolved(&mut self, id: &str, resolved: bool) -> bool {
        let Some(mut comment) = self.comment_by_id(id).cloned() else {
            return false;
        };
        if comment.resolved_at.is_some() == resolved {
            return false;
        }
        if resolved {
            comment.resolved_by = Some(self.comment_author().label().to_string());
            comment.resolved_at = Some(session_sync::now_rfc3339());
        } else {
            comment.resolved_by = None;
            comment.resolved_at = None;
        }
        comment.rev = comment.rev.saturating_add(1);
        comment.edited_at = Some(session_sync::now_rfc3339());
        self.enqueue_comment(comment);
        true
    }

    pub(crate) fn comment_by_id(&self, id: &str) -> Option<&ProjectComment> {
        self.comments.iter().find(|comment| comment.id == id)
    }

    /// Show it here now, and get it to the document when the worker is free.
    pub(crate) fn enqueue_comment(&mut self, comment: ProjectComment) {
        comments::merge_into(&mut self.comments, [comment.clone()]);
        // Replace rather than append: several quick edits to one comment
        // should send the last of them, not each keystroke's worth in turn.
        match self
            .comment_outbox
            .iter_mut()
            .find(|queued| queued.id == comment.id)
        {
            Some(queued) => *queued = comment,
            None => self.comment_outbox.push(comment),
        }
        self.flush_comment_outbox();
    }

    /// Start a write if there is something to send and nothing in flight.
    ///
    /// A session that has never been saved has no document to append to. Its
    /// comments stay in memory and go out with the first save, which carries
    /// `self.comments` like every other part of the session.
    pub(crate) fn flush_comment_outbox(&mut self) {
        if self.comment_outbox.is_empty() || self.comment_write.is_some() {
            return;
        }
        // A save rewrites the whole document, comments included; letting a
        // comment write race it would have the two clobber each other's
        // revisions for no gain. The outbox waits a frame.
        if self.session_save_state.is_some() || self.session_open_in_progress() {
            return;
        }
        let Some(path) = self.project_path.clone() else {
            return;
        };
        let ops = std::mem::take(&mut self.comment_outbox);
        let saved_by = self.session_saved_by();
        let (tx, rx) = mpsc::channel();
        let sent = ops.clone();
        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            let _ = tx.send(run_comment_write_job(&path, ops, &saved_by));
            // The frame loop sleeps when idle, so the result would otherwise
            // sit in the channel until the user moved the mouse.
            crate::ui_wake::wake_ui();
        });
        self.comment_write = Some(CommentWriteState {
            rx,
            started_at: Instant::now(),
            sent,
        });
    }

    /// Read the document's conversation without disturbing anything else.
    ///
    /// Used when the watch reports the file changed, when the window opens,
    /// and from its Refresh button. It never writes, so two people reading at
    /// once cost each other nothing.
    pub(crate) fn request_comment_pull(&mut self) {
        if self.comment_pull.is_some() {
            return;
        }
        let Some(path) = self.project_path.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            let _ = tx.send(run_comment_pull_job(&path));
            crate::ui_wake::wake_ui();
        });
        self.comment_pull = Some(rx);
    }

    // ---- References ------------------------------------------------------

    /// Turn a stored reference path into one this machine can open.
    ///
    /// Comment paths follow the session's own `path_mode` like every other
    /// stored source, so a relative one resolves against the `.nwsess` -- the
    /// reason a session on a share is relative by default is that colleagues
    /// mount it differently, and a comment naming a file is no exception.
    /// Pure string work: nothing here may touch the filesystem.
    pub(crate) fn resolve_comment_ref_path(&self, reference: &CommentRef) -> PathBuf {
        let base = self
            .project_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        super::project::resolve_path(&reference.path, &base)
    }

    /// Write a reference to a file the way this session stores paths.
    pub(crate) fn comment_ref_for_path(
        &self,
        path: &Path,
        anchor: Option<CommentAnchor>,
    ) -> CommentRef {
        let base = self
            .project_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        CommentRef {
            path: super::project::session_path(path, &base, self.session_path_mode),
            anchor,
        }
    }

    /// Go to what a reference points at.
    ///
    /// Two phases, like the transcript's seek and for the same reason: the
    /// file has to finish loading before there is a timeline to seek on, and
    /// the load is asynchronous. See `apply_pending_comment_jump`.
    pub(crate) fn request_comment_ref_jump(&mut self, reference: &CommentRef) {
        let path = self.resolve_comment_ref_path(reference);
        let anchor = reference.anchor;
        // A span or a spectral band can only be seen on the editor's canvas,
        // so those open it. A bare cursor stays in the list, where following
        // a reference costs nothing.
        let wants_editor = anchor.is_some_and(|anchor| {
            anchor.normalized_range().is_some() || anchor.freq_hz.is_some()
        });
        self.pending_comment_jump = Some((path.clone(), anchor));
        if wants_editor {
            self.open_or_activate_tab(&path);
            return;
        }
        if self.playing_path.as_deref() == Some(path.as_path()) {
            return;
        }
        if let Some(row) = self.row_for_path(&path) {
            self.select_and_load(row, true);
            return;
        }
        // Not in the list at all -- a colleague pointed at a file this
        // session no longer carries, or one only they can see.
        self.open_or_activate_tab(&path);
    }

    /// Land the jump once the file is actually loaded.
    pub(crate) fn apply_pending_comment_jump(&mut self) {
        let Some((path, anchor)) = self.pending_comment_jump.clone() else {
            return;
        };
        let tab_idx = self.tabs.iter().position(|tab| tab.path == path);
        // Wait for whichever surface is going to answer. Giving up early
        // would seek the previous file, which is worse than seeking late.
        if self.playing_path.as_deref() != Some(path.as_path()) && tab_idx.is_none() {
            return;
        }
        self.pending_comment_jump = None;
        let Some(anchor) = anchor else {
            return;
        };

        if let Some(tab_idx) = tab_idx {
            self.restore_comment_anchor_in_tab(tab_idx, anchor);
        }
        if self.playing_path.as_deref() != Some(path.as_path()) {
            return;
        }
        let out_sr = self.audio.shared.out_sample_rate.max(1) as f64;
        let mut samples = (anchor.start_sec.max(0.0) * out_sr).round() as usize;
        if let Some(tab) = self.tabs.iter().find(|tab| tab.path == path) {
            samples = self.map_display_to_audio_sample(tab, samples);
        }
        self.audio.seek_to_sample(samples);
    }

    /// Put the cursor, the selection and the spectral band back the way the
    /// author had them, the same restore the editor's own note list performs.
    fn restore_comment_anchor_in_tab(&mut self, tab_idx: usize, anchor: CommentAnchor) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        // Seconds against the source, converted here rather than stored as
        // samples: the author's buffer and this one need not agree on a rate.
        let rate = tab.buffer_sample_rate.max(1) as f64;
        let to_sample = |secs: f64| (secs.max(0.0) * rate).round() as usize;
        let start = to_sample(anchor.start_sec);
        tab.preview_offset_samples = Some(start);
        match anchor.normalized_range() {
            Some((from, to)) => {
                let (from, to) = (to_sample(from), to_sample(to));
                tab.selection = Some((from, to));
                tab.selection_anchor_sample = Some(from);
                tab.freq_selection = anchor.freq_hz;
            }
            None => {
                tab.selection = None;
                tab.selection_anchor_sample = None;
                tab.freq_selection = None;
            }
        }
    }

    /// Every file this conversation points at, for the "this file" filter.
    pub(crate) fn comment_mentions_path(&self, comment: &ProjectComment, path: &Path) -> bool {
        comments::find_refs(&comment.body)
            .into_iter()
            .any(|(_, reference)| self.resolve_comment_ref_path(&reference) == path)
    }

    /// Adopt whatever the comment workers finished. Runs every frame.
    pub(crate) fn drain_comment_jobs(&mut self) {
        self.drain_comment_write();
        self.drain_comment_pull();
        self.flush_comment_outbox();
    }

    fn drain_comment_write(&mut self) {
        let Some(state) = self.comment_write.as_ref() else {
            return;
        };
        let Ok(result) = state.rx.try_recv() else {
            return;
        };
        let state = self.comment_write.take().expect("checked above");
        match result {
            Ok(written) => {
                self.debug_log(format!(
                    "comments written: {} in the document (revision {}, {})",
                    written.comments.len(),
                    written.revision,
                    written.fingerprint.short_hex()
                ));
                // Merged, never assigned. A comment written while this one
                // was in flight is still only in memory, and the document we
                // just read knows nothing about it -- assigning would drop it
                // from the window while it sat in the outbox waiting its turn.
                comments::merge_into(&mut self.comments, written.comments);
                // Adopt what we just wrote as the baseline. Without this the
                // watch reports our own post back to us as "someone else
                // saved", and the next real save would see a false conflict.
                self.session_disk_fingerprint = Some(written.fingerprint);
                self.session_comment_free_fingerprint = Some(written.comment_free_fingerprint);
                self.session_revision = Some(written.revision);
                self.restart_session_watch();
            }
            Err(error) => {
                self.debug_log(format!("comment write failed: {error}"));
                // Back in the queue, in front of anything typed since, so the
                // document still receives them in the order they were written.
                let mut restored = state.sent;
                restored.append(&mut self.comment_outbox);
                self.comment_outbox = restored;
                self.push_toast(
                    crate::app::types::ToastSeverity::Warning,
                    format!(
                        "Comment not shared yet — {error}. It stays here and will be retried."
                    ),
                );
            }
        }
    }

    fn drain_comment_pull(&mut self) {
        let Some(rx) = self.comment_pull.as_ref() else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.comment_pull = None;
        let pending = self.session_changed_pending.take();
        match result {
            Ok(pull) => {
                let added = comments::merge_into(&mut self.comments, pull.comments);
                // The whole point of the comparison: a document that differs
                // from ours *only* in its conversation is a colleague talking,
                // not a colleague saving. Take their words and stay quiet.
                let comments_only = self
                    .session_comment_free_fingerprint
                    .is_some_and(|ours| ours == pull.comment_free_fingerprint);
                if comments_only {
                    self.session_disk_fingerprint = Some(pull.fingerprint);
                    self.session_revision = pull.revision;
                    self.restart_session_watch();
                    if added > 0 {
                        self.debug_log(format!("{added} comment(s) arrived from another writer"));
                    }
                } else if let Some(changed) = pending {
                    self.announce_session_changed_on_disk(changed);
                }
            }
            Err(error) => {
                self.debug_log(format!("comment pull failed: {error}"));
                // We failed to establish what changed, so the conservative
                // answer stands: warn about it. Swallowing a real save
                // because a read hiccuped is the one outcome to avoid.
                if let Some(changed) = pending {
                    self.announce_session_changed_on_disk(changed);
                }
            }
        }
    }
}

/// Read the document, union the outbox into its conversation, and swap it
/// back -- retrying from the top whenever somebody commits underneath us.
fn run_comment_write_job(
    path: &Path,
    ops: Vec<ProjectComment>,
    saved_by: &str,
) -> Result<CommentWriteResult, String> {
    let mut last_conflict = None;
    for attempt in 0..WRITE_ATTEMPTS {
        if attempt > 0 {
            let delay = RETRY_DELAYS_MS[(attempt - 1).min(RETRY_DELAYS_MS.len() - 1)];
            if delay > 0 {
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
        }
        let before = session_sync::read_session_state(path)
            .map_err(|err| format!("Failed to read {}: {err}", path.display()))?;
        let (expected, bytes) = match (before.fingerprint(), before.bytes()) {
            (Some(fingerprint), Some(bytes)) => (fingerprint, bytes),
            // Nothing to append to. Not retried: the file will not come back
            // on its own, and the comment is safe in the outbox meanwhile.
            _ => {
                return Err(format!(
                    "the session file is not on disk: {}",
                    path.display()
                ))
            }
        };
        let text = std::str::from_utf8(bytes)
            .map_err(|_| format!("Session file is not valid UTF-8: {}", path.display()))?;
        let mut project = deserialize_project(text).map_err(|e| e.to_string())?;

        comments::merge_into(&mut project.comments, ops.iter().cloned());

        // Stamped like any other write, so the history and the watch see one
        // consistent sequence of revisions rather than two interleaved ones.
        let stamp = before.stamp();
        let revision = stamp.revision.unwrap_or(0).saturating_add(1);
        project.revision = Some(revision);
        project.saved_at = Some(session_sync::now_rfc3339());
        project.saved_by = Some(saved_by.to_string());
        let encoded = serialize_project(&project).map_err(|e| e.to_string())?;

        // The check that decides. A miss means somebody committed between
        // our read and here -- their document is the one to merge into, so go
        // back and do exactly that.
        let now = session_sync::read_session_state(path)
            .map_err(|err| format!("Failed to read {}: {err}", path.display()))?;
        if now.fingerprint() != Some(expected) {
            last_conflict = Some(now.stamp().describe());
            continue;
        }

        let nonce = crate::app::WavesPreviewer::save_nonce();
        let temp = path.with_extension(format!("nwsess.{nonce}.tmp"));
        session_sync::retry_shared_io(|| std::fs::write(&temp, &encoded))
            .map_err(|e| e.to_string())?;
        if let Err(error) = session_sync::atomic_replace_file(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(error.to_string());
        }

        let comment_free_fingerprint =
            super::project::comment_free_fingerprint(&mut project).map_err(|e| e.to_string())?;
        return Ok(CommentWriteResult {
            comments: std::mem::take(&mut project.comments),
            fingerprint: SessionFingerprint::of_bytes(encoded.as_bytes()),
            comment_free_fingerprint,
            revision,
        });
    }
    Err(match last_conflict {
        Some(who) => format!("the session is being saved faster than this could commit ({who})"),
        None => "the session is being saved faster than this could commit".to_string(),
    })
}

/// Read just the conversation, plus the two hashes needed to decide whether
/// anything else moved.
fn run_comment_pull_job(path: &Path) -> Result<CommentPull, String> {
    let disk = session_sync::read_session_state(path)
        .map_err(|err| format!("Failed to read {}: {err}", path.display()))?;
    let (Some(fingerprint), Some(bytes)) = (disk.fingerprint(), disk.bytes()) else {
        return Err(format!(
            "the session file is not on disk: {}",
            path.display()
        ));
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|_| format!("Session file is not valid UTF-8: {}", path.display()))?;
    let mut project = deserialize_project(text).map_err(|e| e.to_string())?;
    let comment_free_fingerprint =
        super::project::comment_free_fingerprint(&mut project).map_err(|e| e.to_string())?;
    Ok(CommentPull {
        comments: std::mem::take(&mut project.comments),
        fingerprint,
        comment_free_fingerprint,
        revision: project.revision,
    })
}
