//! Per-user record of what a session looked like the last time this person
//! opened it, plus a local history of the session document.
//!
//! Both belong here rather than in the `.nwsess` for two reasons. The first
//! is that they are *per user*: "changed since **you** last opened it" has no
//! meaning in a document a whole team shares. The second is the one the
//! shared-session work was about -- putting this in the document would make
//! every reader a writer again, since opening would have to record a new
//! baseline. A hundred thousand file hashes would also add megabytes to a
//! document that is parsed on every open.
//!
//! So: a SQLite database beside the metadata cache, keyed by the session's
//! own lineage id. It is a cache, not user data -- losing it costs one
//! silent re-baseline, never any of the user's work.
//!
//! Everything here blocks on disk. The connection lives on a single worker
//! thread ([`SessionStore`]); the UI thread only ever sends commands and
//! reads results out of a channel.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// Session document versions kept per session.
const HISTORY_PER_SESSION: usize = 20;
/// Total bytes of stored history across every session before the oldest are
/// dropped. A session document is usually kilobytes; a very large one is a
/// few megabytes, so this is roomy without being unbounded.
const HISTORY_TOTAL_BYTES_LIMIT: u64 = 256 * 1024 * 1024;

pub fn default_store_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(path)
                .join("neowaves")
                .join("session-state-v1.sqlite3"),
        );
    }
    if let Some(path) = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(path)
                .join("NeoWaves")
                .join("cache")
                .join("session-state-v1.sqlite3"),
        );
    }
    #[cfg(unix)]
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(home)
                .join(".cache")
                .join("neowaves")
                .join("session-state-v1.sqlite3"),
        );
    }
    None
}

/// What kind of reference a tracked file is, so the change list can say
/// whether it was audio or the spreadsheet the list is joined against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrackedKind {
    Audio,
    ExternalData,
}

impl TrackedKind {
    fn to_db(self) -> i64 {
        match self {
            Self::Audio => 0,
            Self::ExternalData => 1,
        }
    }

    fn from_db(value: i64) -> Self {
        match value {
            1 => Self::ExternalData,
            _ => Self::Audio,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::ExternalData => "data",
        }
    }
}

/// What we knew about one referenced file at the end of the last scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBaseline {
    pub kind: TrackedKind,
    pub size: u64,
    pub mtime_ns: u128,
    /// `None` until the background pass has hashed it. A stat difference on
    /// a file with no hash has to be reported, because there is nothing to
    /// compare against -- see `session_baseline`.
    pub content_hash: Option<String>,
    /// When this row was last written. This is the "detected at" the change
    /// list shows.
    pub recorded_at: i64,
}

/// What this user knows about a session they have opened before.
#[derive(Clone, Debug)]
pub struct SessionVisit {
    pub last_opened_at: i64,
    pub last_scanned_at: i64,
    pub last_revision: Option<u64>,
}

/// One stored version of a session document.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub id: i64,
    pub revision: Option<u64>,
    pub saved_by: Option<String>,
    pub saved_at: Option<String>,
    pub captured_at: i64,
    pub byte_len: u64,
    pub fingerprint: String,
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Stable key for a session document.
///
/// Prefers the document's own `session_id`, so the same session opened from
/// `Z:\proj\a.nwsess` on one day and `\\server\share\proj\a.nwsess` on the
/// next is still recognised as the same session. Documents written before
/// that field existed fall back to their path.
pub fn session_key(session_id: Option<&str>, path: &Path) -> String {
    match session_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => format!("id:{id}"),
        None => {
            let text = path.to_string_lossy();
            // Windows paths are case-insensitive; a session reopened with a
            // differently-cased path is the same session.
            #[cfg(windows)]
            let text = text.to_lowercase();
            format!("path:{text}")
        }
    }
}

// ---- The database ---------------------------------------------------------

struct Db {
    connection: Connection,
}

impl Db {
    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create session store directory {}", parent.display()))?;
        }
        match Self::open_initialized(path) {
            Ok(db) => Ok(db),
            Err(first_error) => {
                // Cache data, never user data: keep the broken file for
                // diagnosis and carry on with a fresh one.
                preserve_corrupt_files(path);
                Self::open_initialized(path).with_context(|| {
                    format!(
                        "recreate session store after error at {}: {first_error}",
                        path.display()
                    )
                })
            }
        }
    }

    fn open_initialized(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("open session store {}", path.display()))?;
        let db = Self { connection };
        db.initialize()?;
        Ok(db)
    }

    fn initialize(&self) -> Result<()> {
        self.connection.pragma_update(None, "journal_mode", "WAL")?;
        self.connection
            .pragma_update(None, "synchronous", "NORMAL")?;
        self.connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS session_visit (
                session_key     TEXT PRIMARY KEY,
                session_path    TEXT NOT NULL,
                last_opened_at  INTEGER NOT NULL,
                last_scanned_at INTEGER NOT NULL,
                last_revision   INTEGER
            );
            CREATE TABLE IF NOT EXISTS session_file_baseline (
                session_key  TEXT NOT NULL,
                path         TEXT NOT NULL,
                kind         INTEGER NOT NULL,
                size_text    TEXT NOT NULL,
                mtime_ns_text TEXT NOT NULL,
                content_hash TEXT,
                recorded_at  INTEGER NOT NULL,
                PRIMARY KEY (session_key, path)
            );
            CREATE INDEX IF NOT EXISTS session_file_baseline_session
                ON session_file_baseline(session_key);
            CREATE TABLE IF NOT EXISTS session_history (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                session_key  TEXT NOT NULL,
                session_path TEXT NOT NULL,
                revision     INTEGER,
                saved_by     TEXT,
                saved_at     TEXT,
                captured_at  INTEGER NOT NULL,
                fingerprint  TEXT NOT NULL,
                bytes        BLOB NOT NULL,
                byte_len     INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS session_history_key
                ON session_history(session_key, id);
            CREATE TABLE IF NOT EXISTS comment_read (
                session_key TEXT NOT NULL,
                comment_id  TEXT NOT NULL,
                read_at     INTEGER NOT NULL,
                PRIMARY KEY (session_key, comment_id)
            );
            ",
        )?;
        Ok(())
    }

    fn load_visit(&self, key: &str) -> Result<Option<SessionVisit>> {
        let visit = self
            .connection
            .query_row(
                "SELECT last_opened_at, last_scanned_at, last_revision
                 FROM session_visit WHERE session_key = ?1",
                params![key],
                |row| {
                    Ok(SessionVisit {
                        last_opened_at: row.get(0)?,
                        last_scanned_at: row.get(1)?,
                        last_revision: row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    })
                },
            )
            .optional()?;
        Ok(visit)
    }

    /// Which comments this person has already seen.
    ///
    /// Per user, so it cannot live in the shared document -- a `.nwsess` has
    /// nowhere to put a different answer for each colleague, and writing one
    /// on open would make every reader a writer again.
    fn load_comment_reads(&self, key: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .connection
            .prepare("SELECT comment_id FROM comment_read WHERE session_key = ?1")?;
        let rows = stmt.query_map(params![key], |row| row.get::<_, String>(0))?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    fn mark_comments_read(&self, key: &str, ids: &[String], read_at: i64) -> Result<()> {
        let mut stmt = self.connection.prepare(
            "INSERT INTO comment_read (session_key, comment_id, read_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_key, comment_id) DO NOTHING",
        )?;
        for id in ids {
            stmt.execute(params![key, id, read_at])?;
        }
        Ok(())
    }

    fn load_baseline(&self, key: &str) -> Result<Vec<(PathBuf, FileBaseline)>> {
        let mut stmt = self.connection.prepare(
            "SELECT path, kind, size_text, mtime_ns_text, content_hash, recorded_at
             FROM session_file_baseline WHERE session_key = ?1",
        )?;
        let rows = stmt.query_map(params![key], |row| {
            let path: String = row.get(0)?;
            let kind = TrackedKind::from_db(row.get(1)?);
            let size: String = row.get(2)?;
            let mtime: String = row.get(3)?;
            Ok((
                PathBuf::from(path),
                FileBaseline {
                    kind,
                    size: size.parse().unwrap_or(0),
                    mtime_ns: mtime.parse().unwrap_or(0),
                    content_hash: row.get(4)?,
                    recorded_at: row.get(5)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn record_visit(
        &self,
        key: &str,
        path: &Path,
        opened_at: i64,
        scanned_at: i64,
        revision: Option<u64>,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO session_visit
                 (session_key, session_path, last_opened_at, last_scanned_at, last_revision)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_key) DO UPDATE SET
                 session_path = excluded.session_path,
                 last_opened_at = excluded.last_opened_at,
                 last_scanned_at = excluded.last_scanned_at,
                 last_revision = excluded.last_revision",
            params![
                key,
                path.to_string_lossy(),
                opened_at,
                scanned_at,
                revision.map(|v| v as i64)
            ],
        )?;
        Ok(())
    }

    /// Replace the rows for the paths given, and drop rows for anything the
    /// session no longer references when `prune_to` lists the full set.
    fn update_baseline(
        &mut self,
        key: &str,
        rows: &[(PathBuf, FileBaseline)],
        removed: &[PathBuf],
    ) -> Result<()> {
        let tx = self.connection.transaction()?;
        {
            let mut upsert = tx.prepare(
                "INSERT INTO session_file_baseline
                     (session_key, path, kind, size_text, mtime_ns_text, content_hash, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(session_key, path) DO UPDATE SET
                     kind = excluded.kind,
                     size_text = excluded.size_text,
                     mtime_ns_text = excluded.mtime_ns_text,
                     content_hash = excluded.content_hash,
                     recorded_at = excluded.recorded_at",
            )?;
            for (path, baseline) in rows {
                upsert.execute(params![
                    key,
                    path.to_string_lossy(),
                    baseline.kind.to_db(),
                    baseline.size.to_string(),
                    baseline.mtime_ns.to_string(),
                    baseline.content_hash,
                    baseline.recorded_at,
                ])?;
            }
            let mut delete = tx.prepare(
                "DELETE FROM session_file_baseline WHERE session_key = ?1 AND path = ?2",
            )?;
            for path in removed {
                delete.execute(params![key, path.to_string_lossy()])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn capture_history(
        &mut self,
        key: &str,
        path: &Path,
        revision: Option<u64>,
        saved_by: Option<&str>,
        saved_at: Option<&str>,
        fingerprint: &str,
        bytes: &[u8],
    ) -> Result<()> {
        // The same document twice in a row is not a version worth keeping.
        let latest: Option<String> = self
            .connection
            .query_row(
                "SELECT fingerprint FROM session_history
                 WHERE session_key = ?1 ORDER BY id DESC LIMIT 1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        if latest.as_deref() == Some(fingerprint) {
            return Ok(());
        }
        self.connection.execute(
            "INSERT INTO session_history
                 (session_key, session_path, revision, saved_by, saved_at,
                  captured_at, fingerprint, bytes, byte_len)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                key,
                path.to_string_lossy(),
                revision.map(|v| v as i64),
                saved_by,
                saved_at,
                now_unix(),
                fingerprint,
                bytes,
                bytes.len() as i64,
            ],
        )?;
        self.prune_history(key)?;
        Ok(())
    }

    fn prune_history(&self, key: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM session_history
             WHERE session_key = ?1 AND id NOT IN (
                 SELECT id FROM session_history WHERE session_key = ?1
                 ORDER BY id DESC LIMIT ?2
             )",
            params![key, HISTORY_PER_SESSION as i64],
        )?;
        // Then the global byte cap, oldest first across every session.
        let total: i64 = self
            .connection
            .query_row(
                "SELECT COALESCE(SUM(byte_len), 0) FROM session_history",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if total as u64 <= HISTORY_TOTAL_BYTES_LIMIT {
            return Ok(());
        }
        let mut over = total as u64 - HISTORY_TOTAL_BYTES_LIMIT;
        let mut stmt = self
            .connection
            .prepare("SELECT id, byte_len FROM session_history ORDER BY captured_at ASC, id ASC")?;
        let mut doomed = Vec::new();
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
        for row in rows {
            let (id, len) = row?;
            doomed.push(id);
            over = over.saturating_sub(len.max(0) as u64);
            if over == 0 {
                break;
            }
        }
        for id in doomed {
            self.connection
                .execute("DELETE FROM session_history WHERE id = ?1", params![id])?;
        }
        Ok(())
    }

    fn list_history(&self, key: &str) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.connection.prepare(
            "SELECT id, revision, saved_by, saved_at, captured_at, byte_len, fingerprint
             FROM session_history WHERE session_key = ?1 ORDER BY id DESC",
        )?;
        let rows = stmt.query_map(params![key], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                revision: row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                saved_by: row.get(2)?,
                saved_at: row.get(3)?,
                captured_at: row.get(4)?,
                byte_len: row.get::<_, i64>(5)?.max(0) as u64,
                fingerprint: row.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn history_bytes(&self, id: i64) -> Result<Option<Vec<u8>>> {
        let bytes = self
            .connection
            .query_row(
                "SELECT bytes FROM session_history WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(bytes)
    }
}

fn preserve_corrupt_files(path: &Path) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    for suffix in ["", "-wal", "-shm"] {
        let mut source_name = path.as_os_str().to_os_string();
        source_name.push(suffix);
        let source = PathBuf::from(source_name);
        if !source.exists() {
            continue;
        }
        let mut preserved = source.as_os_str().to_os_string();
        preserved.push(format!(".corrupt-{}-{stamp}", std::process::id()));
        let _ = std::fs::rename(&source, PathBuf::from(preserved));
    }
}

// ---- The worker -----------------------------------------------------------

enum Command {
    /// Read the previous visit and baseline for a session.
    Load {
        key: String,
        request: u64,
    },
    RecordVisit {
        key: String,
        path: PathBuf,
        opened_at: i64,
        scanned_at: i64,
        revision: Option<u64>,
    },
    UpdateBaseline {
        key: String,
        rows: Vec<(PathBuf, FileBaseline)>,
        removed: Vec<PathBuf>,
    },
    CaptureHistory {
        key: String,
        path: PathBuf,
        revision: Option<u64>,
        saved_by: Option<String>,
        saved_at: Option<String>,
        fingerprint: String,
        bytes: Vec<u8>,
    },
    ListHistory {
        key: String,
        request: u64,
    },
    LoadCommentReads {
        key: String,
        request: u64,
    },
    MarkCommentsRead {
        key: String,
        ids: Vec<String>,
        read_at: i64,
    },
    RestoreHistory {
        id: i64,
        request: u64,
    },
    Stop,
}

/// What the worker sends back. Every variant carries the `request` id it
/// answers so a reply for a session the user has since closed is dropped
/// rather than applied.
pub enum StoreReply {
    Loaded {
        request: u64,
        visit: Option<SessionVisit>,
        baseline: Vec<(PathBuf, FileBaseline)>,
    },
    History {
        request: u64,
        entries: Vec<HistoryEntry>,
    },
    HistoryBytes {
        request: u64,
        bytes: Option<Vec<u8>>,
    },
    CommentReads {
        request: u64,
        ids: Vec<String>,
    },
    Failed {
        request: u64,
        error: String,
    },
}

pub struct SessionStore {
    tx: Option<mpsc::Sender<Command>>,
    next_request: u64,
}

impl Drop for SessionStore {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Command::Stop);
        }
    }
}

impl SessionStore {
    /// `path` of `None` disables the store entirely: every command is a
    /// no-op and every read answers "nothing known". That is the state under
    /// kittest without an explicit database, and on a machine with no
    /// resolvable cache directory.
    pub fn new(path: Option<PathBuf>) -> (Self, mpsc::Receiver<StoreReply>) {
        let (reply_tx, reply_rx) = mpsc::channel();
        let Some(path) = path else {
            return (
                Self {
                    tx: None,
                    next_request: 1,
                },
                reply_rx,
            );
        };
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("neowaves-session-store".to_string())
            .spawn(move || worker(path, rx, reply_tx));
        let tx = spawned.is_ok().then_some(tx);
        (
            Self {
                tx,
                next_request: 1,
            },
            reply_rx,
        )
    }

    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }

    fn next_request(&mut self) -> u64 {
        let id = self.next_request;
        self.next_request = self.next_request.wrapping_add(1).max(1);
        id
    }

    fn send(&self, command: Command) {
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(command);
        }
    }

    /// Ask for the previous visit + baseline. Returns the request id the
    /// reply will carry, or `None` when the store is disabled.
    pub fn load(&mut self, key: String) -> Option<u64> {
        self.tx.as_ref()?;
        let request = self.next_request();
        self.send(Command::Load { key, request });
        Some(request)
    }

    pub fn load_comment_reads(&mut self, key: String) -> Option<u64> {
        self.tx.as_ref()?;
        let request = self.next_request();
        self.send(Command::LoadCommentReads { key, request });
        Some(request)
    }

    pub fn mark_comments_read(&self, key: String, ids: Vec<String>, read_at: i64) {
        if ids.is_empty() {
            return;
        }
        self.send(Command::MarkCommentsRead { key, ids, read_at });
    }

    pub fn record_visit(
        &self,
        key: String,
        path: PathBuf,
        opened_at: i64,
        scanned_at: i64,
        revision: Option<u64>,
    ) {
        self.send(Command::RecordVisit {
            key,
            path,
            opened_at,
            scanned_at,
            revision,
        });
    }

    pub fn update_baseline(
        &self,
        key: String,
        rows: Vec<(PathBuf, FileBaseline)>,
        removed: Vec<PathBuf>,
    ) {
        if rows.is_empty() && removed.is_empty() {
            return;
        }
        self.send(Command::UpdateBaseline { key, rows, removed });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn capture_history(
        &self,
        key: String,
        path: PathBuf,
        revision: Option<u64>,
        saved_by: Option<String>,
        saved_at: Option<String>,
        fingerprint: String,
        bytes: Vec<u8>,
    ) {
        self.send(Command::CaptureHistory {
            key,
            path,
            revision,
            saved_by,
            saved_at,
            fingerprint,
            bytes,
        });
    }

    pub fn list_history(&mut self, key: String) -> Option<u64> {
        self.tx.as_ref()?;
        let request = self.next_request();
        self.send(Command::ListHistory { key, request });
        Some(request)
    }

    pub fn restore_history(&mut self, id: i64) -> Option<u64> {
        self.tx.as_ref()?;
        let request = self.next_request();
        self.send(Command::RestoreHistory { id, request });
        Some(request)
    }
}

fn worker(path: PathBuf, rx: mpsc::Receiver<Command>, reply: mpsc::Sender<StoreReply>) {
    crate::app::threading::lower_current_thread_priority();
    // Opened lazily: a database this session may never touch must not cost
    // anything at startup.
    let mut db: Option<Db> = None;
    let mut open_failed = false;
    while let Ok(command) = rx.recv() {
        if matches!(command, Command::Stop) {
            break;
        }
        if db.is_none() && !open_failed {
            match Db::open(&path) {
                Ok(opened) => db = Some(opened),
                Err(err) => {
                    open_failed = true;
                    eprintln!("neowaves: session store unavailable: {err:#}");
                }
            }
        }
        let Some(db) = db.as_mut() else {
            // Answer the requests that expect a reply so nothing waits forever.
            match command {
                Command::Load { request, .. } => {
                    let _ = reply.send(StoreReply::Loaded {
                        request,
                        visit: None,
                        baseline: Vec::new(),
                    });
                }
                Command::ListHistory { request, .. } => {
                    let _ = reply.send(StoreReply::History {
                        request,
                        entries: Vec::new(),
                    });
                }
                Command::RestoreHistory { request, .. } => {
                    let _ = reply.send(StoreReply::HistoryBytes {
                        request,
                        bytes: None,
                    });
                }
                _ => {}
            }
            crate::ui_wake::wake_ui();
            continue;
        };
        let outcome = run_command(db, command, &reply);
        if let Err((request, err)) = outcome {
            if let Some(request) = request {
                let _ = reply.send(StoreReply::Failed {
                    request,
                    error: format!("{err:#}"),
                });
            } else {
                eprintln!("neowaves: session store: {err:#}");
            }
        }
        crate::ui_wake::wake_ui();
    }
}

/// `Err((request, error))` -- `request` is set when a caller is waiting for
/// an answer that will now never come.
type CommandOutcome = std::result::Result<(), (Option<u64>, anyhow::Error)>;

fn run_command(db: &mut Db, command: Command, reply: &mpsc::Sender<StoreReply>) -> CommandOutcome {
    match command {
        Command::Stop => Ok(()),
        Command::Load { key, request } => {
            let visit = db.load_visit(&key).map_err(|e| (Some(request), e))?;
            let baseline = db.load_baseline(&key).map_err(|e| (Some(request), e))?;
            let _ = reply.send(StoreReply::Loaded {
                request,
                visit,
                baseline,
            });
            Ok(())
        }
        Command::RecordVisit {
            key,
            path,
            opened_at,
            scanned_at,
            revision,
        } => db
            .record_visit(&key, &path, opened_at, scanned_at, revision)
            .map_err(|e| (None, e)),
        Command::UpdateBaseline { key, rows, removed } => db
            .update_baseline(&key, &rows, &removed)
            .map_err(|e| (None, e)),
        Command::CaptureHistory {
            key,
            path,
            revision,
            saved_by,
            saved_at,
            fingerprint,
            bytes,
        } => db
            .capture_history(
                &key,
                &path,
                revision,
                saved_by.as_deref(),
                saved_at.as_deref(),
                &fingerprint,
                &bytes,
            )
            .map_err(|e| (None, e)),
        Command::LoadCommentReads { key, request } => {
            let ids = db
                .load_comment_reads(&key)
                .map_err(|e| (Some(request), e))?;
            let _ = reply.send(StoreReply::CommentReads { request, ids });
            Ok(())
        }
        Command::MarkCommentsRead { key, ids, read_at } => db
            .mark_comments_read(&key, &ids, read_at)
            .map_err(|e| (None, e)),
        Command::ListHistory { key, request } => {
            let entries = db.list_history(&key).map_err(|e| (Some(request), e))?;
            let _ = reply.send(StoreReply::History { request, entries });
            Ok(())
        }
        Command::RestoreHistory { id, request } => {
            let bytes = db.history_bytes(id).map_err(|e| (Some(request), e))?;
            let _ = reply.send(StoreReply::HistoryBytes { request, bytes });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "neowaves_session_store_{tag}_{}_{}.sqlite3",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        path
    }

    fn baseline(hash: Option<&str>) -> FileBaseline {
        FileBaseline {
            kind: TrackedKind::Audio,
            size: 42,
            mtime_ns: 1_700_000_000_000_000_000,
            content_hash: hash.map(str::to_string),
            recorded_at: 1_700_000_000,
        }
    }

    #[test]
    fn comment_reads_are_per_session_and_survive_being_recorded_twice() {
        let path = temp_db("comment_reads");
        let db = Db::open(&path).expect("open");
        assert!(db.load_comment_reads("id:abc").expect("load").is_empty());

        db.mark_comments_read("id:abc", &["one".to_string(), "two".to_string()], 100)
            .expect("mark");
        // Recording the same comment again is what happens every frame the
        // window is open, so it has to be free rather than an error.
        db.mark_comments_read("id:abc", &["one".to_string()], 200)
            .expect("mark again");

        let mut ids = db.load_comment_reads("id:abc").expect("load");
        ids.sort();
        assert_eq!(ids, vec!["one".to_string(), "two".to_string()]);
        assert!(
            db.load_comment_reads("id:other").expect("load").is_empty(),
            "another session's reads are not this one's"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_session_never_opened_before_has_no_visit() {
        let path = temp_db("fresh");
        let db = Db::open(&path).expect("open");
        assert!(db.load_visit("id:abc").expect("load").is_none());
        assert!(db.load_baseline("id:abc").expect("load").is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_baseline_round_trips_including_a_missing_hash() {
        let path = temp_db("roundtrip");
        let mut db = Db::open(&path).expect("open");
        let rows = vec![
            (PathBuf::from("/a/one.wav"), baseline(Some("deadbeef"))),
            (PathBuf::from("/a/two.wav"), baseline(None)),
        ];
        db.update_baseline("id:abc", &rows, &[]).expect("write");
        let mut loaded = db.load_baseline("id:abc").expect("read");
        loaded.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].1.content_hash.as_deref(), Some("deadbeef"));
        assert_eq!(
            loaded[1].1.content_hash, None,
            "an unhashed file must stay unhashed rather than come back as empty string"
        );
        assert_eq!(loaded[0].1.size, 42);
        assert_eq!(loaded[0].1.mtime_ns, 1_700_000_000_000_000_000);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_removed_reference_is_dropped_from_the_baseline() {
        let path = temp_db("removed");
        let mut db = Db::open(&path).expect("open");
        let rows = vec![(PathBuf::from("/a/one.wav"), baseline(None))];
        db.update_baseline("id:abc", &rows, &[]).expect("write");
        db.update_baseline("id:abc", &[], &[PathBuf::from("/a/one.wav")])
            .expect("remove");
        assert!(db.load_baseline("id:abc").expect("read").is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn baselines_of_different_sessions_do_not_mix() {
        let path = temp_db("scoped");
        let mut db = Db::open(&path).expect("open");
        db.update_baseline(
            "id:one",
            &[(PathBuf::from("/a/x.wav"), baseline(None))],
            &[],
        )
        .expect("write one");
        db.update_baseline(
            "id:two",
            &[(PathBuf::from("/a/y.wav"), baseline(None))],
            &[],
        )
        .expect("write two");
        assert_eq!(db.load_baseline("id:one").expect("read").len(), 1);
        assert_eq!(db.load_baseline("id:two").expect("read").len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_visit_round_trips_and_updates_in_place() {
        let path = temp_db("visit");
        let db = Db::open(&path).expect("open");
        let session = Path::new("/proj/a.nwsess");
        db.record_visit("id:abc", session, 100, 110, Some(3))
            .expect("first");
        db.record_visit("id:abc", session, 200, 210, Some(4))
            .expect("second");
        let visit = db.load_visit("id:abc").expect("load").expect("present");
        assert_eq!(visit.last_opened_at, 200);
        assert_eq!(visit.last_scanned_at, 210);
        assert_eq!(visit.last_revision, Some(4));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn history_keeps_the_newest_versions_and_drops_the_rest() {
        let path = temp_db("history_cap");
        let mut db = Db::open(&path).expect("open");
        let session = Path::new("/proj/a.nwsess");
        for revision in 1..=(HISTORY_PER_SESSION as u64 + 5) {
            db.capture_history(
                "id:abc",
                session,
                Some(revision),
                Some("tanaka"),
                None,
                &format!("hash{revision}"),
                format!("version {revision}").as_bytes(),
            )
            .expect("capture");
        }
        let entries = db.list_history("id:abc").expect("list");
        assert_eq!(entries.len(), HISTORY_PER_SESSION);
        assert_eq!(
            entries.first().and_then(|e| e.revision),
            Some(HISTORY_PER_SESSION as u64 + 5),
            "the newest version must come first"
        );
        assert!(
            entries.iter().all(|e| e.revision != Some(1)),
            "the oldest versions past the cap must be gone"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_the_same_document_twice_does_not_add_a_version() {
        let path = temp_db("history_dedup");
        let mut db = Db::open(&path).expect("open");
        let session = Path::new("/proj/a.nwsess");
        for _ in 0..3 {
            db.capture_history(
                "id:abc",
                session,
                Some(1),
                None,
                None,
                "samehash",
                b"identical",
            )
            .expect("capture");
        }
        assert_eq!(db.list_history("id:abc").expect("list").len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_stored_version_can_be_read_back_byte_for_byte() {
        let path = temp_db("history_bytes");
        let mut db = Db::open(&path).expect("open");
        let body = b"version = 2\nrevision = 7\n";
        db.capture_history(
            "id:abc",
            Path::new("/proj/a.nwsess"),
            Some(7),
            Some("tanaka"),
            Some("2026-08-31T14:32:00Z"),
            "h7",
            body,
        )
        .expect("capture");
        let entry = db
            .list_history("id:abc")
            .expect("list")
            .into_iter()
            .next()
            .expect("one entry");
        assert_eq!(entry.byte_len, body.len() as u64);
        assert_eq!(entry.saved_by.as_deref(), Some("tanaka"));
        let bytes = db.history_bytes(entry.id).expect("read").expect("present");
        assert_eq!(bytes, body);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_database_is_preserved_and_recreated() {
        let path = temp_db("corrupt");
        std::fs::write(&path, b"this is not a sqlite database at all").expect("write junk");
        let db = Db::open(&path).expect("a corrupt store must not stop the app");
        assert!(db.load_visit("id:abc").expect("usable").is_none());
        let preserved = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read dir")
            .flatten()
            .any(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with(
                    path.file_name()
                        .expect("file name")
                        .to_string_lossy()
                        .as_ref(),
                ) && name.contains(".corrupt-")
            });
        assert!(preserved, "the broken file must be kept for diagnosis");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_session_id_keys_the_same_session_across_different_mounts() {
        let a = session_key(Some("abc123"), Path::new(r"Z:\proj\a.nwsess"));
        let b = session_key(Some("abc123"), Path::new(r"\\server\share\proj\a.nwsess"));
        assert_eq!(a, b, "the lineage id, not the mount, identifies a session");
    }

    #[test]
    fn a_session_without_an_id_falls_back_to_its_path() {
        let a = session_key(None, Path::new("/proj/a.nwsess"));
        let b = session_key(None, Path::new("/proj/b.nwsess"));
        assert_ne!(a, b);
        assert!(a.starts_with("path:"));
        // An empty id is not an id.
        assert_eq!(a, session_key(Some("  "), Path::new("/proj/a.nwsess")));
    }

    #[test]
    fn a_disabled_store_answers_every_read_without_a_database() {
        let (mut store, rx) = SessionStore::new(None);
        assert!(!store.is_enabled());
        assert!(store.load("id:abc".to_string()).is_none());
        assert!(store.list_history("id:abc".to_string()).is_none());
        // And nothing is ever sent back.
        assert!(rx.try_recv().is_err());
    }
}
