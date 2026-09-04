//! Persistent and in-memory cache for list-oriented metadata summaries.
//!
//! All SQLite access is intended to happen on the metadata worker. The GUI
//! only communicates with [`MetadataSummaryPool`] through channels.

use super::{
    summarize_path, MetadataSummary, ScanBudget, SummaryRequest, ASWG_SCHEMA_VERSION,
    PARSER_SCHEMA_VERSION,
};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, LazyLock, Mutex};
use std::time::UNIX_EPOCH;

pub const MEMORY_SUMMARY_LIMIT: usize = 16_384;
pub const MEMORY_BYTE_LIMIT: usize = 128 * 1024 * 1024;
pub const DISK_SOFT_LIMIT: u64 = 512 * 1024 * 1024;
const FINGERPRINT_EDGE_BYTES: usize = 64 * 1024;
static NORMALIZER_VERSION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "ucs-{}:{};aswg-{}",
        super::ucs::VERSION,
        super::ucs::ALIAS_TABLE_SHA256,
        ASWG_SCHEMA_VERSION
    )
});

#[derive(Clone, Debug, PartialEq, Eq)]
struct CacheKey {
    canonical_path: String,
    size: u64,
    mtime_ns: u128,
    fingerprint: String,
}

#[derive(Clone, Debug)]
pub struct CacheLookup {
    pub summary: MetadataSummary,
    pub hit: bool,
}

pub fn default_cache_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(path)
                .join("neowaves")
                .join("metadata-v1.sqlite3"),
        );
    }
    if let Some(path) = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(path)
                .join("NeoWaves")
                .join("cache")
                .join("metadata-v1.sqlite3"),
        );
    }
    #[cfg(unix)]
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(home)
                .join(".cache")
                .join("neowaves")
                .join("metadata-v1.sqlite3"),
        );
    }
    None
}

pub struct MetadataCache {
    connection: Connection,
    path: PathBuf,
    disk_soft_limit: u64,
}

impl MetadataCache {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_limit(path, DISK_SOFT_LIMIT)
    }

    pub fn open_with_limit(path: &Path, disk_soft_limit: u64) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create metadata cache directory {}", parent.display()))?;
        }
        let disk_soft_limit = effective_disk_soft_limit(path, disk_soft_limit);
        match Self::open_initialized(path, disk_soft_limit) {
            Ok(cache) => Ok(cache),
            Err(first_error) => {
                // A corrupt database is cache data, never user data. Preserve
                // it for diagnosis and start with a clean database.
                preserve_corrupt_cache_files(path);
                Self::open_initialized(path, disk_soft_limit).with_context(|| {
                    format!(
                        "recreate metadata cache after error at {}: {first_error}",
                        path.display()
                    )
                })
            }
        }
    }

    fn open_initialized(path: &Path, disk_soft_limit: u64) -> Result<Self> {
        let connection = Self::open_connection(path)?;
        let cache = Self {
            connection,
            path: path.to_path_buf(),
            disk_soft_limit,
        };
        cache.initialize()?;
        Ok(cache)
    }

    fn open_connection(path: &Path) -> Result<Connection> {
        Connection::open(path).with_context(|| format!("open metadata cache {}", path.display()))
    }

    fn initialize(&self) -> Result<()> {
        self.connection.pragma_update(None, "journal_mode", "WAL")?;
        self.connection
            .pragma_update(None, "synchronous", "NORMAL")?;
        self.connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS metadata_summary (
                canonical_path TEXT NOT NULL,
                size_text TEXT NOT NULL,
                mtime_ns_text TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                parser_schema INTEGER NOT NULL,
                normalizer_version TEXT NOT NULL,
                coverage_json TEXT NOT NULL,
                summary_json TEXT NOT NULL,
                stored_bytes INTEGER NOT NULL,
                last_access INTEGER NOT NULL,
                PRIMARY KEY (
                    canonical_path, size_text, mtime_ns_text, fingerprint,
                    parser_schema, normalizer_version
                )
            );
            CREATE INDEX IF NOT EXISTS metadata_summary_lru
                ON metadata_summary(last_access);
            ",
        )?;
        Ok(())
    }

    pub fn get_or_scan(
        &mut self,
        path: &Path,
        request: SummaryRequest,
        budget: ScanBudget,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<CacheLookup> {
        let key = cache_key(path)?;
        let cached = self.lookup(&key)?;
        if let Some(summary) = cached.as_ref() {
            if coverage_satisfies(&summary.coverage, &request) {
                return Ok(CacheLookup {
                    summary: summary.clone(),
                    hit: true,
                });
            }
        }
        let scanned = summarize_path(path, request, budget, cancel)?;
        let summary = match cached {
            Some(previous) => merge_summary_coverage(previous, scanned),
            None => scanned,
        };
        self.store(&key, &summary)?;
        self.prune_if_needed()?;
        Ok(CacheLookup {
            summary,
            hit: false,
        })
    }

    fn lookup(&self, key: &CacheKey) -> Result<Option<MetadataSummary>> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "
                SELECT coverage_json, summary_json
                FROM metadata_summary
                WHERE canonical_path = ?1
                  AND size_text = ?2
                  AND mtime_ns_text = ?3
                  AND fingerprint = ?4
                  AND parser_schema = ?5
                  AND normalizer_version = ?6
                ",
                params![
                    key.canonical_path,
                    key.size.to_string(),
                    key.mtime_ns.to_string(),
                    key.fingerprint,
                    PARSER_SCHEMA_VERSION,
                    NORMALIZER_VERSION.as_str(),
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((coverage_json, summary_json)) = row else {
            return Ok(None);
        };
        let mut summary: MetadataSummary = serde_json::from_str(&summary_json)?;
        summary.coverage = serde_json::from_str(&coverage_json)?;
        self.connection.execute(
            "
            UPDATE metadata_summary SET last_access = unixepoch()
            WHERE canonical_path = ?1
              AND size_text = ?2
              AND mtime_ns_text = ?3
              AND fingerprint = ?4
              AND parser_schema = ?5
              AND normalizer_version = ?6
            ",
            params![
                key.canonical_path,
                key.size.to_string(),
                key.mtime_ns.to_string(),
                key.fingerprint,
                PARSER_SCHEMA_VERSION,
                NORMALIZER_VERSION.as_str(),
            ],
        )?;
        Ok(Some(summary))
    }

    fn store(&self, key: &CacheKey, summary: &MetadataSummary) -> Result<()> {
        if !cache_volume_has_headroom(&self.path) {
            return Ok(());
        }
        let coverage_json = serde_json::to_string(&summary.coverage)?;
        let summary_json = serde_json::to_string(summary)?;
        self.connection.execute(
            "
            INSERT OR REPLACE INTO metadata_summary (
                canonical_path, size_text, mtime_ns_text, fingerprint,
                parser_schema, normalizer_version, coverage_json, summary_json,
                stored_bytes, last_access
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, unixepoch())
            ",
            params![
                key.canonical_path,
                key.size.to_string(),
                key.mtime_ns.to_string(),
                key.fingerprint,
                PARSER_SCHEMA_VERSION,
                NORMALIZER_VERSION.as_str(),
                coverage_json,
                summary_json,
                summary_json.len() as i64,
            ],
        )?;
        Ok(())
    }

    fn prune_if_needed(&self) -> Result<()> {
        let stored: i64 = self.connection.query_row(
            "SELECT COALESCE(SUM(stored_bytes), 0) FROM metadata_summary",
            [],
            |row| row.get(0),
        )?;
        let stored = stored.max(0) as u64;
        if stored <= self.disk_soft_limit {
            return Ok(());
        }
        let target = self.disk_soft_limit.saturating_mul(9) / 10;
        let mut remaining = stored;
        let mut statement = self.connection.prepare(
            "
            SELECT canonical_path, size_text, mtime_ns_text, fingerprint
            FROM metadata_summary ORDER BY last_access ASC
            ",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut victims = Vec::new();
        for row in rows {
            if remaining <= target {
                break;
            }
            let victim = row?;
            let bytes: i64 = self.connection.query_row(
                "
                SELECT stored_bytes FROM metadata_summary
                WHERE canonical_path=?1 AND size_text=?2
                  AND mtime_ns_text=?3 AND fingerprint=?4
                ",
                params![victim.0, victim.1, victim.2, victim.3],
                |row| row.get(0),
            )?;
            remaining = remaining.saturating_sub(bytes.max(0) as u64);
            victims.push(victim);
        }
        drop(statement);
        for victim in victims {
            self.connection.execute(
                "
                DELETE FROM metadata_summary
                WHERE canonical_path=?1 AND size_text=?2
                  AND mtime_ns_text=?3 AND fingerprint=?4
                ",
                params![victim.0, victim.1, victim.2, victim.3],
            )?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn cache_volume_has_headroom(path: &Path) -> bool {
    let probe = path.parent().unwrap_or(path);
    volume_space(probe)
        .map(|(free, total)| free > (1024 * 1024 * 1024u64).max(total / 20))
        // Failure to ask for space must not disable the cache forever; the
        // actual SQLite write still reports a full volume safely.
        .unwrap_or(true)
}

fn effective_disk_soft_limit(path: &Path, requested: u64) -> u64 {
    let probe = path.parent().unwrap_or(path);
    volume_space(probe)
        .map(|(free, total)| disk_limit_for_space(requested, free, total))
        .unwrap_or(requested)
}

fn disk_limit_for_space(requested: u64, free: u64, total: u64) -> u64 {
    let reserve = (1024 * 1024 * 1024u64).max(total / 20);
    requested.min(free.saturating_sub(reserve))
}

#[cfg(windows)]
fn volume_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
    (ok != 0).then_some((available.min(free), total))
}

#[cfg(unix)]
fn volume_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as u64;
    Some((
        (stats.f_bavail as u64).saturating_mul(block),
        (stats.f_blocks as u64).saturating_mul(block),
    ))
}

#[cfg(not(any(windows, unix)))]
fn volume_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

fn preserve_corrupt_cache_files(path: &Path) {
    let stamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    for suffix in ["", "-wal", "-shm"] {
        let mut source_name = path.as_os_str().to_os_string();
        source_name.push(suffix);
        let source = PathBuf::from(source_name);
        if !source.exists() {
            continue;
        }
        let mut preserved_name = source.as_os_str().to_os_string();
        preserved_name.push(format!(".corrupt-{}-{stamp}", std::process::id()));
        let _ = std::fs::rename(&source, PathBuf::from(preserved_name));
    }
}

fn cache_key(path: &Path) -> Result<CacheKey> {
    let canonical = std::fs::canonicalize(path)
        .with_context(|| format!("canonicalize metadata source {}", path.display()))?;
    let metadata = std::fs::metadata(&canonical)?;
    let size = metadata.len();
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut file = File::open(&canonical)?;
    let mut hasher = Sha256::new();
    hasher.update(size.to_le_bytes());
    let first_len = size.min(FINGERPRINT_EDGE_BYTES as u64) as usize;
    let mut first = vec![0; first_len];
    file.read_exact(&mut first)?;
    hasher.update(&first);
    if size > FINGERPRINT_EDGE_BYTES as u64 {
        let last_len = size.min(FINGERPRINT_EDGE_BYTES as u64) as usize;
        file.seek(SeekFrom::Start(size - last_len as u64))?;
        let mut last = vec![0; last_len];
        file.read_exact(&mut last)?;
        hasher.update(&last);
    }
    Ok(CacheKey {
        canonical_path: canonical.to_string_lossy().into_owned(),
        size,
        mtime_ns,
        fingerprint: format!("{:x}", hasher.finalize()),
    })
}

fn coverage_satisfies(coverage: &[String], request: &SummaryRequest) -> bool {
    let covered: HashSet<&str> = coverage.iter().map(String::as_str).collect();
    let fields = request.fields.is_empty() && covered.contains("__all__")
        || request
            .fields
            .iter()
            .all(|field| covered.contains(field.as_str()) || covered.contains("__all__"));
    fields && (!request.include_raw || covered.contains("__raw__"))
}

fn merge_summary_coverage(
    mut previous: MetadataSummary,
    current: MetadataSummary,
) -> MetadataSummary {
    for field in current.fields {
        if let Some(existing) = previous
            .fields
            .iter_mut()
            .find(|existing| existing.key == field.key)
        {
            *existing = field;
        } else {
            previous.fields.push(field);
        }
    }
    previous
        .fields
        .sort_by(|left, right| left.key.cmp(&right.key));
    for (key, values) in current.raw_fields {
        previous.raw_fields.insert(key, values);
    }
    for coverage in current.coverage {
        if !previous.coverage.contains(&coverage) {
            previous.coverage.push(coverage);
        }
    }
    previous.coverage.sort();
    previous.schema_version = current.schema_version;
    previous.source = current.source;
    previous.container = current.container;
    previous.completion = current.completion;
    previous.unknown_nodes = current.unknown_nodes;
    previous.diagnostics = current.diagnostics;
    previous
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SummaryPriority {
    Background,
    SortFilter,
    Visible,
    Selected,
}

#[derive(Clone, Debug)]
struct SummaryTask {
    path: PathBuf,
    request: SummaryRequest,
    budget: ScanBudget,
    generation: u64,
    priority: SummaryPriority,
}

#[derive(Debug)]
pub struct SummaryUpdate {
    pub path: PathBuf,
    pub generation: u64,
    pub summary: Result<MetadataSummary, String>,
    pub cache_hit: bool,
}

struct QueueInner {
    tasks: HashMap<PathBuf, SummaryTask>,
    high: VecDeque<PathBuf>,
    low: VecDeque<PathBuf>,
    running: HashMap<PathBuf, Arc<AtomicBool>>,
}

struct SharedQueue {
    inner: Mutex<QueueInner>,
    wake: Condvar,
    stop: AtomicBool,
    paused: AtomicBool,
}

pub struct MetadataSummaryPool {
    shared: Arc<SharedQueue>,
}

impl MetadataSummaryPool {
    pub fn new(cache_path: Option<PathBuf>) -> (Self, mpsc::Receiver<SummaryUpdate>) {
        Self::new_with_disk_limit(cache_path, DISK_SOFT_LIMIT)
    }

    pub fn new_with_disk_limit(
        cache_path: Option<PathBuf>,
        disk_soft_limit: u64,
    ) -> (Self, mpsc::Receiver<SummaryUpdate>) {
        let shared = Arc::new(SharedQueue {
            inner: Mutex::new(QueueInner {
                tasks: HashMap::new(),
                high: VecDeque::new(),
                low: VecDeque::new(),
                running: HashMap::new(),
            }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
        });
        let (result_tx, result_rx) = mpsc::sync_channel(64);
        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("metadata-summary-cache".to_string())
            .spawn(move || summary_worker(worker_shared, cache_path, disk_soft_limit, result_tx))
            .expect("spawn metadata summary worker");
        (Self { shared }, result_rx)
    }

    pub fn set_paused(&self, paused: bool) {
        let changed = self.shared.paused.swap(paused, Ordering::Relaxed) != paused;
        if changed && !paused {
            self.shared.wake.notify_all();
        }
    }

    pub fn enqueue(
        &self,
        path: PathBuf,
        request: SummaryRequest,
        budget: ScanBudget,
        generation: u64,
        priority: SummaryPriority,
    ) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(task) = inner.tasks.get_mut(&path) {
            for field in request.fields {
                if !task.request.fields.contains(&field) {
                    task.request.fields.push(field);
                }
            }
            task.request.include_raw |= request.include_raw;
            task.generation = generation;
            if budget.max_bytes_read > task.budget.max_bytes_read
                || budget.max_nodes > task.budget.max_nodes
            {
                task.budget = budget;
            }
            if priority > task.priority {
                task.priority = priority;
                inner.high.push_front(path);
            }
        } else {
            let task = SummaryTask {
                path: path.clone(),
                request,
                budget,
                generation,
                priority,
            };
            inner.tasks.insert(path.clone(), task);
            if priority >= SummaryPriority::Visible {
                inner.high.push_back(path);
            } else {
                inner.low.push_back(path);
            }
        }
        self.shared.wake.notify_one();
    }

    pub fn promote(&self, path: &Path, priority: SummaryPriority) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(task) = inner.tasks.get_mut(path) {
            if priority > task.priority {
                task.priority = priority;
                inner.high.push_front(path.to_path_buf());
                self.shared.wake.notify_one();
            }
        }
    }

    pub fn cancel(&self, path: &Path) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.tasks.remove(path);
        if let Some(cancel) = inner.running.get(path) {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for MetadataSummaryPool {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
    }
}

fn summary_worker(
    shared: Arc<SharedQueue>,
    cache_path: Option<PathBuf>,
    disk_soft_limit: u64,
    result_tx: mpsc::SyncSender<SummaryUpdate>,
) {
    // Do not contend with native window/audio startup. SQLite is opened only
    // when the first metadata column actually requests work.
    crate::app::threading::lower_current_thread_priority();
    let mut cache: Option<MetadataCache> = None;
    let mut cache_initialized = false;
    loop {
        let (task, cancel) = {
            let mut inner = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if shared.stop.load(Ordering::Relaxed) {
                    return;
                }
                if shared.paused.load(Ordering::Relaxed) {
                    inner = shared
                        .wake
                        .wait(inner)
                        .unwrap_or_else(|error| error.into_inner());
                    continue;
                }
                let path = inner.high.pop_front().or_else(|| inner.low.pop_front());
                if let Some(path) = path {
                    let Some(task) = inner.tasks.remove(&path) else {
                        continue;
                    };
                    let cancel = Arc::new(AtomicBool::new(false));
                    inner.running.insert(path, Arc::clone(&cancel));
                    break (task, cancel);
                }
                inner = shared
                    .wake
                    .wait(inner)
                    .unwrap_or_else(|error| error.into_inner());
            }
        };
        if !cache_initialized {
            cache = cache_path
                .as_deref()
                .and_then(|path| MetadataCache::open_with_limit(path, disk_soft_limit).ok());
            cache_initialized = true;
        }
        let result = if let Some(cache) = cache.as_mut() {
            cache
                .get_or_scan(
                    &task.path,
                    task.request,
                    task.budget,
                    Some(Arc::clone(&cancel)),
                )
                .map(|lookup| (lookup.summary, lookup.hit))
        } else {
            summarize_path(
                &task.path,
                task.request,
                task.budget,
                Some(Arc::clone(&cancel)),
            )
            .map(|summary| (summary, false))
        };
        {
            let mut inner = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.running.remove(&task.path);
        }
        if cancel.load(Ordering::Relaxed) {
            continue;
        }
        let update = match result {
            Ok((summary, hit)) => SummaryUpdate {
                path: task.path,
                generation: task.generation,
                summary: Ok(summary),
                cache_hit: hit,
            },
            Err(error) => SummaryUpdate {
                path: task.path,
                generation: task.generation,
                summary: Err(format!("{error:#}")),
                cache_hit: false,
            },
        };
        if result_tx.send(update).is_err() {
            return;
        }
    }
}

pub struct SummaryMemoryCache {
    entries: HashMap<PathBuf, Arc<MetadataSummary>>,
    order: VecDeque<PathBuf>,
    estimated_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for SummaryMemoryCache {
    fn default() -> Self {
        Self::with_limits(MEMORY_SUMMARY_LIMIT, MEMORY_BYTE_LIMIT)
    }
}

impl SummaryMemoryCache {
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            estimated_bytes: 0,
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    pub fn set_limits(&mut self, max_entries: usize, max_bytes: usize) {
        self.max_entries = max_entries.max(1);
        self.max_bytes = max_bytes.max(1);
        self.evict_to_limit();
    }

    pub fn estimated_bytes(&self) -> usize {
        self.estimated_bytes
    }

    pub fn get(&mut self, path: &Path) -> Option<Arc<MetadataSummary>> {
        let value = self.entries.get(path).cloned()?;
        self.touch(path);
        Some(value)
    }

    pub fn peek(&self, path: &Path) -> Option<&Arc<MetadataSummary>> {
        self.entries.get(path)
    }

    pub fn insert(&mut self, path: PathBuf, summary: MetadataSummary) -> Arc<MetadataSummary> {
        if let Some(previous) = self.entries.remove(&path) {
            self.estimated_bytes = self
                .estimated_bytes
                .saturating_sub(estimate_summary_bytes(&previous));
            self.order.retain(|candidate| candidate != &path);
        }
        let summary = Arc::new(summary);
        self.estimated_bytes = self
            .estimated_bytes
            .saturating_add(estimate_summary_bytes(&summary));
        self.entries.insert(path.clone(), Arc::clone(&summary));
        self.order.push_back(path);
        self.evict_to_limit();
        summary
    }

    fn evict_to_limit(&mut self) {
        while self.entries.len() > self.max_entries || self.estimated_bytes > self.max_bytes {
            let Some(victim) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&victim) {
                self.estimated_bytes = self
                    .estimated_bytes
                    .saturating_sub(estimate_summary_bytes(&removed));
            }
        }
    }

    fn touch(&mut self, path: &Path) {
        self.order.retain(|candidate| candidate != path);
        self.order.push_back(path.to_path_buf());
    }
}

fn estimate_summary_bytes(summary: &MetadataSummary) -> usize {
    serde_json::to_vec(summary)
        .map(|bytes| bytes.len())
        .unwrap_or(1024)
        .saturating_add(std::mem::size_of::<MetadataSummary>())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetadataColumnDefinition {
    pub key: String,
    pub label: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn minimal_wav() -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&96_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0u32.to_le_bytes());
        wav
    }

    fn empty_summary(path: &str, padding: usize) -> MetadataSummary {
        MetadataSummary {
            schema_version: crate::metadata::PARSER_SCHEMA_VERSION,
            source: PathBuf::from(path),
            container: crate::metadata::ContainerKind::Unknown,
            completion: crate::metadata::Completion::Complete,
            fields: Vec::new(),
            raw_fields: Default::default(),
            unknown_nodes: 0,
            diagnostics: Vec::new(),
            coverage: vec!["x".repeat(padding)],
        }
    }

    #[test]
    fn memory_summary_cache_is_lru_and_byte_bounded() {
        let mut cache = SummaryMemoryCache::with_limits(2, usize::MAX);
        cache.insert(PathBuf::from("a"), empty_summary("a", 0));
        cache.insert(PathBuf::from("b"), empty_summary("b", 0));
        assert!(cache.get(Path::new("a")).is_some());
        cache.insert(PathBuf::from("c"), empty_summary("c", 0));
        assert!(cache.peek(Path::new("a")).is_some());
        assert!(cache.peek(Path::new("b")).is_none());
        assert!(cache.peek(Path::new("c")).is_some());

        let mut tiny = SummaryMemoryCache::with_limits(100, 1_024);
        tiny.insert(PathBuf::from("large"), empty_summary("large", 8_192));
        assert!(tiny.peek(Path::new("large")).is_none());
        assert_eq!(tiny.estimated_bytes(), 0);
    }

    #[test]
    fn summary_pool_pause_defers_disk_scan_until_resumed() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves-metadata-pause-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("fixture.wav");
        std::fs::write(&source, minimal_wav()).unwrap();
        let (pool, rx) = MetadataSummaryPool::new(None);
        pool.set_paused(true);
        pool.enqueue(
            source,
            SummaryRequest::default(),
            ScanBudget::list(),
            1,
            SummaryPriority::Background,
        );
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(40))
                .is_err(),
            "paused summary worker must not touch queued sources"
        );
        pool.set_paused(false);
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok(),
            "summary scan must resume after playback protection ends"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_hit_and_source_change_invalidation() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves-metadata-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("fixture.wav");
        let db = dir.join("cache.sqlite3");
        File::create(&source)
            .unwrap()
            .write_all(&minimal_wav())
            .unwrap();
        let mut cache = MetadataCache::open(&db).unwrap();
        let first = cache
            .get_or_scan(&source, SummaryRequest::default(), ScanBudget::list(), None)
            .unwrap();
        assert!(!first.hit);
        let second = cache
            .get_or_scan(&source, SummaryRequest::default(), ScanBudget::list(), None)
            .unwrap();
        assert!(second.hit);
        let mut changed = minimal_wav();
        changed.push(0);
        File::create(&source).unwrap().write_all(&changed).unwrap();
        let third = cache
            .get_or_scan(&source, SummaryRequest::default(), ScanBudget::list(), None)
            .unwrap();
        assert!(!third.hit);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_cache_is_preserved_and_recreated() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves-metadata-corrupt-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("cache.sqlite3");
        std::fs::write(&db, b"not a sqlite database").unwrap();
        let cache = MetadataCache::open(&db).expect("recreate corrupt cache");
        assert_eq!(cache.path(), db);
        drop(cache);
        assert!(std::fs::read_dir(&dir).unwrap().any(|entry| {
            entry
                .ok()
                .is_some_and(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
        }));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_merges_incremental_field_coverage() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves-metadata-coverage-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("fixture.wav");
        let db = dir.join("cache.sqlite3");
        std::fs::write(&source, minimal_wav()).unwrap();
        let mut cache = MetadataCache::open(&db).unwrap();
        let first_request = SummaryRequest {
            fields: vec!["title".to_string()],
            include_raw: false,
        };
        assert!(
            !cache
                .get_or_scan(&source, first_request, ScanBudget::list(), None)
                .unwrap()
                .hit
        );
        let second_request = SummaryRequest {
            fields: vec!["artist".to_string()],
            include_raw: true,
        };
        assert!(
            !cache
                .get_or_scan(&source, second_request, ScanBudget::list(), None)
                .unwrap()
                .hit
        );
        let combined_request = SummaryRequest {
            fields: vec!["title".to_string(), "artist".to_string()],
            include_raw: true,
        };
        let combined = cache
            .get_or_scan(&source, combined_request, ScanBudget::list(), None)
            .unwrap();
        assert!(combined.hit);
        assert!(combined.summary.coverage.contains(&"title".to_string()));
        assert!(combined.summary.coverage.contains(&"artist".to_string()));
        assert!(combined.summary.coverage.contains(&"__raw__".to_string()));
        drop(cache);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn disk_limit_preserves_the_larger_of_one_gib_or_five_percent() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let requested = 512 * 1024 * 1024;
        assert_eq!(disk_limit_for_space(requested, 3 * GIB, 100 * GIB), 0);
        assert_eq!(
            disk_limit_for_space(requested, 6 * GIB, 100 * GIB),
            requested
        );
        assert_eq!(disk_limit_for_space(requested, GIB / 2, 8 * GIB), 0);
    }
}
