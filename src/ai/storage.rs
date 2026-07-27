use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};

use hnsw_rs::prelude::{DistCosine, Hnsw};
use rusqlite::{params, Connection, OptionalExtension};

use super::models::AudioEmbedding;
use super::provider::AiError;

const SCHEMA_VERSION: i64 = 3;
pub const EXACT_SEARCH_WARNING_SEGMENTS: usize = 25_000;
pub const HNSW_RECOMMENDED_SEGMENTS: usize = 100_000;

#[derive(Clone, Debug, PartialEq)]
pub struct SimilarityHit {
    pub content_hash: String,
    pub segment_start_ms: u64,
    pub segment_end_ms: u64,
    pub score: f32,
    pub path_keys: Vec<String>,
}

pub struct AiStore {
    connection: Connection,
    cache_namespace: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimilaritySearchBackend {
    ExactCosine,
    HnswCosine,
}

impl SimilaritySearchBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactCosine => "exact_cosine",
            Self::HnswCosine => "hnsw_cosine",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SimilaritySearchOutcome {
    pub hits: Vec<SimilarityHit>,
    pub backend: SimilaritySearchBackend,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiBatchProgress {
    pub manifest_hash: String,
    pub status: String,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiBatchItem {
    pub ordinal: usize,
    pub path_key: String,
    pub attempts: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiStoreDiagnostics {
    pub schema_version: i64,
    pub content_objects: usize,
    pub embedding_segments: usize,
    pub active_batches: usize,
    pub failed_batch_items: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct HnswCacheKey {
    namespace: String,
    model: String,
    dimensions: usize,
    revision: u64,
    count: usize,
}

#[derive(Clone, Debug)]
struct IndexedSegment {
    content_hash: String,
    segment_start_ms: u64,
    segment_end_ms: u64,
}

struct HnswSearchIndex {
    graph: Hnsw<'static, f32, DistCosine>,
    segments: Vec<IndexedSegment>,
}

impl AiStore {
    pub fn open(path: &Path) -> Result<Self, AiError> {
        let connection =
            Connection::open(path).map_err(|error| AiError::Storage(error.to_string()))?;
        let store_path = absolute_cache_path(path);
        let mut store = Self {
            connection,
            cache_namespace: store_path.to_string_lossy().into_owned(),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self, AiError> {
        let connection =
            Connection::open_in_memory().map_err(|error| AiError::Storage(error.to_string()))?;
        let mut store = Self {
            connection,
            cache_namespace: format!("memory:{}", next_memory_store_id()),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), AiError> {
        let version: i64 = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| AiError::Storage(error.to_string()))?;
        if version > SCHEMA_VERSION {
            return Err(AiError::Storage(format!(
                "AI database schema {version} is newer than supported {SCHEMA_VERSION}"
            )));
        }
        if version == 0 {
            let transaction = self
                .connection
                .transaction()
                .map_err(|error| AiError::Storage(error.to_string()))?;
            transaction
                .execute_batch(
                    "
                    CREATE TABLE content_objects(
                        hash TEXT PRIMARY KEY,
                        duration_ms INTEGER NOT NULL DEFAULT 0,
                        format TEXT NOT NULL DEFAULT '',
                        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                    );
                    CREATE TABLE file_locations(
                        path_key TEXT PRIMARY KEY,
                        content_hash TEXT NOT NULL,
                        last_seen TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                        exists_now INTEGER NOT NULL DEFAULT 1
                    );
                    CREATE TABLE embedding_segments(
                        content_hash TEXT NOT NULL,
                        model TEXT NOT NULL,
                        dimensions INTEGER NOT NULL,
                        segment_start_ms INTEGER NOT NULL,
                        segment_end_ms INTEGER NOT NULL,
                        vector_blob BLOB NOT NULL,
                        normalized INTEGER NOT NULL,
                        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                        PRIMARY KEY(
                            content_hash, model, dimensions,
                            segment_start_ms, segment_end_ms
                        )
                    );
                    CREATE TABLE ai_analysis(
                        content_hash TEXT NOT NULL,
                        operation TEXT NOT NULL,
                        model TEXT NOT NULL,
                        schema_version INTEGER NOT NULL,
                        prompt_version INTEGER NOT NULL,
                        result_json TEXT NOT NULL,
                        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                        PRIMARY KEY(
                            content_hash, operation, model,
                            schema_version, prompt_version
                        )
                    );
                    CREATE TABLE label_reviews(
                        content_hash TEXT NOT NULL,
                        field TEXT NOT NULL,
                        suggestion TEXT NOT NULL,
                        decision TEXT NOT NULL,
                        decided_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                    );
                    CREATE TABLE agent_sessions(
                        id TEXT PRIMARY KEY,
                        project_ref TEXT,
                        title TEXT NOT NULL,
                        provider TEXT NOT NULL,
                        privacy_mode INTEGER NOT NULL
                    );
                    CREATE TABLE agent_messages(
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        session_id TEXT NOT NULL,
                        role TEXT NOT NULL,
                        content_json TEXT NOT NULL,
                        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                    );
                    CREATE TABLE agent_steps(
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        session_id TEXT NOT NULL,
                        action TEXT NOT NULL,
                        args_redacted TEXT NOT NULL,
                        result_redacted TEXT NOT NULL
                    );
                    CREATE TABLE approvals(
                        id INTEGER PRIMARY KEY,
                        plan_hash TEXT NOT NULL,
                        context_revision INTEGER NOT NULL,
                        status TEXT NOT NULL,
                        expires_at INTEGER NOT NULL
                    );
                    CREATE TABLE uploaded_files(
                        content_hash TEXT PRIMARY KEY,
                        uri_encrypted_ref TEXT NOT NULL,
                        expires_at INTEGER NOT NULL
                    );
                    CREATE TABLE ai_meta(
                        key TEXT PRIMARY KEY,
                        value INTEGER NOT NULL
                    );
                    INSERT INTO ai_meta(key, value) VALUES ('embedding_revision', 0);
                    CREATE TABLE ai_batch_jobs(
                        manifest_hash TEXT PRIMARY KEY,
                        operation TEXT NOT NULL,
                        status TEXT NOT NULL,
                        total INTEGER NOT NULL,
                        completed INTEGER NOT NULL DEFAULT 0,
                        failed INTEGER NOT NULL DEFAULT 0,
                        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                    );
                    CREATE TABLE ai_batch_items(
                        manifest_hash TEXT NOT NULL,
                        ordinal INTEGER NOT NULL,
                        path_key TEXT NOT NULL,
                        status TEXT NOT NULL,
                        attempts INTEGER NOT NULL DEFAULT 0,
                        content_hash TEXT,
                        error_code TEXT,
                        PRIMARY KEY(manifest_hash, ordinal)
                    );
                    CREATE INDEX ai_batch_items_status
                    ON ai_batch_items(manifest_hash, status, ordinal);
                    PRAGMA user_version = 3;
                    ",
                )
                .map_err(|error| AiError::Storage(error.to_string()))?;
            transaction
                .commit()
                .map_err(|error| AiError::Storage(error.to_string()))?;
        } else {
            if version == 1 {
                self.connection
                    .execute_batch(
                        "
                    CREATE TABLE IF NOT EXISTS ai_meta(
                        key TEXT PRIMARY KEY,
                        value INTEGER NOT NULL
                    );
                    INSERT OR IGNORE INTO ai_meta(key, value)
                    VALUES ('embedding_revision', 0);
                    ",
                    )
                    .map_err(|error| AiError::Storage(error.to_string()))?;
            }
            if version <= 2 {
                self.connection
                    .execute_batch(
                        "
                    CREATE TABLE IF NOT EXISTS ai_batch_jobs(
                        manifest_hash TEXT PRIMARY KEY,
                        operation TEXT NOT NULL,
                        status TEXT NOT NULL,
                        total INTEGER NOT NULL,
                        completed INTEGER NOT NULL DEFAULT 0,
                        failed INTEGER NOT NULL DEFAULT 0,
                        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
                    );
                    CREATE TABLE IF NOT EXISTS ai_batch_items(
                        manifest_hash TEXT NOT NULL,
                        ordinal INTEGER NOT NULL,
                        path_key TEXT NOT NULL,
                        status TEXT NOT NULL,
                        attempts INTEGER NOT NULL DEFAULT 0,
                        content_hash TEXT,
                        error_code TEXT,
                        PRIMARY KEY(manifest_hash, ordinal)
                    );
                    CREATE INDEX IF NOT EXISTS ai_batch_items_status
                    ON ai_batch_items(manifest_hash, status, ordinal);
                    PRAGMA user_version = 3;
                    ",
                    )
                    .map_err(|error| AiError::Storage(error.to_string()))?;
            }
        }
        Ok(())
    }

    pub fn put_content(
        &self,
        content_hash: &str,
        duration_ms: u64,
        format: &str,
    ) -> Result<(), AiError> {
        self.connection
            .execute(
                "INSERT INTO content_objects(hash, duration_ms, format)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(hash) DO UPDATE SET
                    duration_ms=excluded.duration_ms,
                    format=excluded.format",
                params![content_hash, duration_ms, format],
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        Ok(())
    }

    pub fn put_file_location(&self, path_key: &str, content_hash: &str) -> Result<(), AiError> {
        if path_key.trim().is_empty() {
            return Err(AiError::Storage("file location key is empty".into()));
        }
        self.connection
            .execute(
                "INSERT INTO file_locations(path_key, content_hash, exists_now)
                 VALUES (?1, ?2, 1)
                 ON CONFLICT(path_key) DO UPDATE SET
                    content_hash=excluded.content_hash,
                    last_seen=CURRENT_TIMESTAMP,
                    exists_now=1",
                params![path_key, content_hash],
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        Ok(())
    }

    pub fn put_embedding(
        &self,
        content_hash: &str,
        embedding: &AudioEmbedding,
    ) -> Result<(), AiError> {
        embedding.validate().map_err(AiError::SchemaValidation)?;
        self.connection
            .execute(
                "INSERT INTO embedding_segments(
                    content_hash, model, dimensions,
                    segment_start_ms, segment_end_ms, vector_blob, normalized
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
                 ON CONFLICT(
                    content_hash, model, dimensions,
                    segment_start_ms, segment_end_ms
                 ) DO UPDATE SET
                    vector_blob=excluded.vector_blob,
                    normalized=excluded.normalized,
                    created_at=CURRENT_TIMESTAMP",
                params![
                    content_hash,
                    embedding.model,
                    embedding.dimensions,
                    embedding.segment_start_ms,
                    embedding.segment_end_ms,
                    embedding_to_bytes(&embedding.values)
                ],
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        self.connection
            .execute(
                "UPDATE ai_meta SET value=value+1 WHERE key='embedding_revision'",
                [],
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        Ok(())
    }

    pub fn has_embedding(
        &self,
        content_hash: &str,
        model: &str,
        dimensions: usize,
    ) -> Result<bool, AiError> {
        self.connection
            .query_row(
                "SELECT 1 FROM embedding_segments
                 WHERE content_hash=?1 AND model=?2 AND dimensions=?3 LIMIT 1",
                params![content_hash, model, dimensions],
                |_| Ok(true),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(|error| AiError::Storage(error.to_string()))
    }

    pub fn embedding_segment_count(
        &self,
        content_hash: &str,
        model: &str,
        dimensions: usize,
    ) -> Result<usize, AiError> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM embedding_segments
                 WHERE content_hash=?1 AND model=?2 AND dimensions=?3",
                params![content_hash, model, dimensions],
                |row| row.get::<_, u64>(0),
            )
            .map(|count| count.min(usize::MAX as u64) as usize)
            .map_err(|error| AiError::Storage(error.to_string()))
    }

    pub fn embeddings_for_content(
        &self,
        content_hash: &str,
        model: &str,
        dimensions: usize,
    ) -> Result<Vec<AudioEmbedding>, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT segment_start_ms, segment_end_ms, vector_blob
                 FROM embedding_segments
                 WHERE content_hash=?1 AND model=?2 AND dimensions=?3 AND normalized=1
                 ORDER BY segment_start_ms, segment_end_ms",
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let rows = statement
            .query_map(params![content_hash, model, dimensions], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let mut embeddings = Vec::new();
        for row in rows {
            let (segment_start_ms, segment_end_ms, blob) =
                row.map_err(|error| AiError::Storage(error.to_string()))?;
            let values = embedding_from_bytes(&blob)?;
            let embedding = AudioEmbedding {
                model: model.to_owned(),
                dimensions,
                values,
                segment_start_ms,
                segment_end_ms,
            };
            embedding.validate().map_err(AiError::Storage)?;
            embeddings.push(embedding);
        }
        Ok(embeddings)
    }

    pub fn model_embedding_count(&self, model: &str, dimensions: usize) -> Result<usize, AiError> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM embedding_segments
                 WHERE model=?1 AND dimensions=?2 AND normalized=1",
                params![model, dimensions],
                |row| row.get::<_, u64>(0),
            )
            .map(|count| count.min(usize::MAX as u64) as usize)
            .map_err(|error| AiError::Storage(error.to_string()))
    }

    pub fn exact_search_warning(segment_count: usize) -> Option<String> {
        if segment_count >= HNSW_RECOMMENDED_SEGMENTS {
            Some(format!(
                "Index has {segment_count} segments; HNSW is used automatically and rebuilt in the background after embedding changes."
            ))
        } else if segment_count >= EXACT_SEARCH_WARNING_SEGMENTS {
            Some(format!(
                "Index has {segment_count} segments; exact cosine latency may be noticeable."
            ))
        } else {
            None
        }
    }

    pub fn search_exact(
        &self,
        query: &[f32],
        model: &str,
        limit: usize,
    ) -> Result<Vec<SimilarityHit>, AiError> {
        if query.is_empty() || query.iter().any(|value| !value.is_finite()) {
            return Err(AiError::SchemaValidation(
                "similarity query vector is empty or invalid".into(),
            ));
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT content_hash, segment_start_ms, segment_end_ms, vector_blob
                 FROM embedding_segments
                 WHERE model=?1 AND dimensions=?2 AND normalized=1",
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let rows = statement
            .query_map(params![model, query.len()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let mut candidates = Vec::new();
        for row in rows {
            let (content_hash, segment_start_ms, segment_end_ms, blob) =
                row.map_err(|error| AiError::Storage(error.to_string()))?;
            let vector = embedding_from_bytes(&blob)?;
            let score = dot_product(query, &vector)?;
            candidates.push((content_hash, segment_start_ms, segment_end_ms, score));
        }
        candidates.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(Ordering::Equal));
        candidates.truncate(limit.clamp(1, 200));
        let mut hits = Vec::with_capacity(candidates.len());
        for (content_hash, segment_start_ms, segment_end_ms, score) in candidates {
            let path_keys = self.file_locations_for_hash(&content_hash)?;
            hits.push(SimilarityHit {
                content_hash,
                segment_start_ms,
                segment_end_ms,
                score,
                path_keys,
            });
        }
        Ok(hits)
    }

    pub fn search_auto(
        &self,
        query: &[f32],
        model: &str,
        limit: usize,
    ) -> Result<SimilaritySearchOutcome, AiError> {
        let count = self.model_embedding_count(model, query.len())?;
        if count < HNSW_RECOMMENDED_SEGMENTS {
            return self
                .search_exact(query, model, limit)
                .map(|hits| SimilaritySearchOutcome {
                    hits,
                    backend: SimilaritySearchBackend::ExactCosine,
                });
        }
        let revision = self.embedding_revision()?;
        let key = HnswCacheKey {
            namespace: self.cache_namespace.clone(),
            model: model.to_owned(),
            dimensions: query.len(),
            revision,
            count,
        };
        let index = self.cached_or_build_hnsw(&key)?;
        let neighbours = index.graph.search(query, limit.clamp(1, 200), 96);
        let mut hits = Vec::with_capacity(neighbours.len());
        for neighbour in neighbours {
            let segment = index.segments.get(neighbour.d_id).ok_or_else(|| {
                AiError::Storage("HNSW result referenced an unknown segment".into())
            })?;
            hits.push(SimilarityHit {
                content_hash: segment.content_hash.clone(),
                segment_start_ms: segment.segment_start_ms,
                segment_end_ms: segment.segment_end_ms,
                score: (1.0 - neighbour.distance).clamp(-1.0, 1.0),
                path_keys: self.file_locations_for_hash(&segment.content_hash)?,
            });
        }
        Ok(SimilaritySearchOutcome {
            hits,
            backend: SimilaritySearchBackend::HnswCosine,
        })
    }

    fn embedding_revision(&self) -> Result<u64, AiError> {
        self.connection
            .query_row(
                "SELECT value FROM ai_meta WHERE key='embedding_revision'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| AiError::Storage(error.to_string()))
    }

    fn cached_or_build_hnsw(&self, key: &HnswCacheKey) -> Result<Arc<HnswSearchIndex>, AiError> {
        let cache = hnsw_cache();
        if let Some(index) = cache
            .lock()
            .map_err(|_| AiError::Storage("HNSW cache lock was poisoned".into()))?
            .get(key)
            .cloned()
        {
            return Ok(index);
        }

        let index = Arc::new(self.build_hnsw(&key.model, key.dimensions, key.count)?);
        let mut cache = cache
            .lock()
            .map_err(|_| AiError::Storage("HNSW cache lock was poisoned".into()))?;
        cache.retain(|cached, _| {
            cached.namespace != key.namespace
                || cached.model != key.model
                || cached.dimensions != key.dimensions
        });
        Ok(Arc::clone(cache.entry(key.clone()).or_insert(index)))
    }

    fn build_hnsw(
        &self,
        model: &str,
        dimensions: usize,
        expected_count: usize,
    ) -> Result<HnswSearchIndex, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT content_hash, segment_start_ms, segment_end_ms, vector_blob
                 FROM embedding_segments
                 WHERE model=?1 AND dimensions=?2 AND normalized=1
                 ORDER BY content_hash, segment_start_ms, segment_end_ms",
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let rows = statement
            .query_map(params![model, dimensions], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let mut segments = Vec::with_capacity(expected_count);
        let mut vectors = Vec::with_capacity(expected_count);
        for row in rows {
            let (content_hash, segment_start_ms, segment_end_ms, blob) =
                row.map_err(|error| AiError::Storage(error.to_string()))?;
            let vector = embedding_from_bytes(&blob)?;
            if vector.len() != dimensions {
                return Err(AiError::Storage(
                    "stored embedding dimensions do not match metadata".into(),
                ));
            }
            segments.push(IndexedSegment {
                content_hash,
                segment_start_ms,
                segment_end_ms,
            });
            vectors.push(vector);
        }
        if vectors.is_empty() {
            return Err(AiError::Storage("cannot build an empty HNSW index".into()));
        }
        let max_layer = 16.min(((vectors.len() as f32).ln().floor() as usize).max(1));
        let mut graph =
            Hnsw::<f32, DistCosine>::new(24, vectors.len(), max_layer, 200, DistCosine {});
        if vectors.len() < 1_024 {
            // The upstream parallel builder can produce a disconnected tiny
            // graph when only a handful of points race during insertion.
            // Production-scale indexes stay parallel; small indexes and tests
            // use deterministic sequential insertion.
            for (id, vector) in vectors.iter().enumerate() {
                graph.insert((vector.as_slice(), id));
            }
        } else {
            let references = vectors
                .iter()
                .enumerate()
                .map(|(id, vector)| (vector, id))
                .collect::<Vec<_>>();
            graph.parallel_insert(&references);
        }
        graph.set_searching_mode(true);
        Ok(HnswSearchIndex { graph, segments })
    }

    pub fn prepare_index_batch(
        &mut self,
        manifest_hash: &str,
        paths: &[String],
        retry_failed: bool,
    ) -> Result<AiBatchProgress, AiError> {
        if manifest_hash.len() != 64 || !manifest_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(AiError::SchemaValidation(
                "batch manifest hash must be 64 hexadecimal characters".into(),
            ));
        }
        if paths.is_empty() || paths.len() > 10_000 {
            return Err(AiError::SchemaValidation(
                "AI index batch must contain 1..=10000 files".into(),
            ));
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| AiError::Storage(error.to_string()))?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO ai_batch_jobs(
                    manifest_hash, operation, status, total
                 ) VALUES (?1, 'index_audio', 'pending', ?2)",
                params![manifest_hash, paths.len()],
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let stored_total: usize = transaction
            .query_row(
                "SELECT total FROM ai_batch_jobs WHERE manifest_hash=?1",
                params![manifest_hash],
                |row| row.get(0),
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        if stored_total != paths.len() {
            return Err(AiError::Storage(
                "batch manifest collides with a different item count".into(),
            ));
        }
        for (ordinal, path) in paths.iter().enumerate() {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO ai_batch_items(
                        manifest_hash, ordinal, path_key, status
                     ) VALUES (?1, ?2, ?3, 'pending')",
                    params![manifest_hash, ordinal, path],
                )
                .map_err(|error| AiError::Storage(error.to_string()))?;
            let stored_path: String = transaction
                .query_row(
                    "SELECT path_key FROM ai_batch_items
                     WHERE manifest_hash=?1 AND ordinal=?2",
                    params![manifest_hash, ordinal],
                    |row| row.get(0),
                )
                .map_err(|error| AiError::Storage(error.to_string()))?;
            if stored_path != *path {
                return Err(AiError::Storage(
                    "batch manifest collides with different paths".into(),
                ));
            }
        }
        transaction
            .execute(
                "UPDATE ai_batch_items SET status='pending'
                 WHERE manifest_hash=?1 AND status='running'",
                params![manifest_hash],
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        if retry_failed {
            transaction
                .execute(
                    "UPDATE ai_batch_items
                     SET status='pending', error_code=NULL
                     WHERE manifest_hash=?1 AND status='failed'",
                    params![manifest_hash],
                )
                .map_err(|error| AiError::Storage(error.to_string()))?;
        }
        transaction
            .execute(
                "UPDATE ai_batch_jobs SET status='running', updated_at=CURRENT_TIMESTAMP
                 WHERE manifest_hash=?1",
                params![manifest_hash],
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        transaction
            .commit()
            .map_err(|error| AiError::Storage(error.to_string()))?;
        self.batch_progress(manifest_hash)
    }

    pub fn claim_next_batch_item(
        &mut self,
        manifest_hash: &str,
    ) -> Result<Option<AiBatchItem>, AiError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let item = transaction
            .query_row(
                "SELECT ordinal, path_key, attempts
                 FROM ai_batch_items
                 WHERE manifest_hash=?1 AND status='pending'
                 ORDER BY ordinal LIMIT 1",
                params![manifest_hash],
                |row| {
                    Ok(AiBatchItem {
                        ordinal: row.get(0)?,
                        path_key: row.get(1)?,
                        attempts: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|error| AiError::Storage(error.to_string()))?;
        if let Some(item) = &item {
            transaction
                .execute(
                    "UPDATE ai_batch_items
                     SET status='running', attempts=attempts+1
                     WHERE manifest_hash=?1 AND ordinal=?2 AND status='pending'",
                    params![manifest_hash, item.ordinal],
                )
                .map_err(|error| AiError::Storage(error.to_string()))?;
        }
        transaction
            .commit()
            .map_err(|error| AiError::Storage(error.to_string()))?;
        Ok(item.map(|mut item| {
            item.attempts += 1;
            item
        }))
    }

    pub fn finish_batch_item(
        &self,
        manifest_hash: &str,
        ordinal: usize,
        content_hash: Option<&str>,
        error_code: Option<&str>,
    ) -> Result<(), AiError> {
        let (status, error_code) = if content_hash.is_some() {
            ("completed", None)
        } else {
            ("failed", Some(error_code.unwrap_or("unknown").trim()))
        };
        if error_code.is_some_and(|value| value.is_empty() || value.len() > 64) {
            return Err(AiError::SchemaValidation(
                "batch error code must contain 1..=64 characters".into(),
            ));
        }
        let changed = self
            .connection
            .execute(
                "UPDATE ai_batch_items
                 SET status=?3, content_hash=?4, error_code=?5
                 WHERE manifest_hash=?1 AND ordinal=?2 AND status='running'",
                params![manifest_hash, ordinal, status, content_hash, error_code],
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        if changed != 1 {
            return Err(AiError::Storage(
                "batch item is not in the running state".into(),
            ));
        }
        self.refresh_batch_progress(manifest_hash)?;
        Ok(())
    }

    pub fn batch_progress(&self, manifest_hash: &str) -> Result<AiBatchProgress, AiError> {
        self.connection
            .query_row(
                "SELECT status, total, completed, failed
                 FROM ai_batch_jobs WHERE manifest_hash=?1",
                params![manifest_hash],
                |row| {
                    Ok(AiBatchProgress {
                        manifest_hash: manifest_hash.to_owned(),
                        status: row.get(0)?,
                        total: row.get(1)?,
                        completed: row.get(2)?,
                        failed: row.get(3)?,
                    })
                },
            )
            .map_err(|error| AiError::Storage(error.to_string()))
    }

    fn refresh_batch_progress(&self, manifest_hash: &str) -> Result<(), AiError> {
        let (completed, failed, pending): (usize, usize, usize) = self
            .connection
            .query_row(
                "SELECT
                    SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status IN ('pending','running') THEN 1 ELSE 0 END)
                 FROM ai_batch_items WHERE manifest_hash=?1",
                params![manifest_hash],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let status = if pending > 0 {
            "running"
        } else if failed > 0 {
            "completed_with_errors"
        } else {
            "completed"
        };
        self.connection
            .execute(
                "UPDATE ai_batch_jobs
                 SET status=?2, completed=?3, failed=?4, updated_at=CURRENT_TIMESTAMP
                 WHERE manifest_hash=?1",
                params![manifest_hash, status, completed, failed],
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        Ok(())
    }

    pub fn diagnostics(&self) -> Result<AiStoreDiagnostics, AiError> {
        let schema_version = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let count = |table: &str, predicate: &str| -> Result<usize, AiError> {
            let sql = format!("SELECT COUNT(*) FROM {table} {predicate}");
            self.connection
                .query_row(&sql, [], |row| row.get::<_, u64>(0))
                .map(|value| value.min(usize::MAX as u64) as usize)
                .map_err(|error| AiError::Storage(error.to_string()))
        };
        Ok(AiStoreDiagnostics {
            schema_version,
            content_objects: count("content_objects", "")?,
            embedding_segments: count("embedding_segments", "")?,
            active_batches: count("ai_batch_jobs", "WHERE status IN ('pending','running')")?,
            failed_batch_items: count("ai_batch_items", "WHERE status='failed'")?,
        })
    }

    fn file_locations_for_hash(&self, content_hash: &str) -> Result<Vec<String>, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT path_key FROM file_locations
                 WHERE content_hash=?1 AND exists_now=1
                 ORDER BY path_key",
            )
            .map_err(|error| AiError::Storage(error.to_string()))?;
        let rows = statement
            .query_map(params![content_hash], |row| row.get::<_, String>(0))
            .map_err(|error| AiError::Storage(error.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| AiError::Storage(error.to_string()))
    }
}

fn hnsw_cache() -> &'static Mutex<HashMap<HnswCacheKey, Arc<HnswSearchIndex>>> {
    static CACHE: OnceLock<Mutex<HashMap<HnswCacheKey, Arc<HnswSearchIndex>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn absolute_cache_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn next_memory_store_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed)
}

pub fn embedding_to_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

pub fn embedding_from_bytes(bytes: &[u8]) -> Result<Vec<f32>, AiError> {
    if bytes.len() % 4 != 0 {
        return Err(AiError::Storage(
            "embedding BLOB length is not divisible by four".into(),
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn dot_product(a: &[f32], b: &[f32]) -> Result<f32, AiError> {
    if a.len() != b.len() {
        return Err(AiError::SchemaValidation(
            "embedding dimensions do not match".into(),
        ));
    }
    Ok(a.iter().zip(b).map(|(left, right)| left * right).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedding(hash_seed: f32) -> AudioEmbedding {
        AudioEmbedding {
            model: "model".into(),
            dimensions: 3,
            values: vec![hash_seed, 1.0 - hash_seed, 0.0],
            segment_start_ms: 0,
            segment_end_ms: 100,
        }
    }

    #[test]
    fn vector_blob_roundtrip_is_lossless() {
        let vector = vec![0.25, -0.5, 1.0];
        assert_eq!(
            embedding_from_bytes(&embedding_to_bytes(&vector)).unwrap(),
            vector
        );
    }

    #[test]
    fn exact_search_returns_best_dot_product_first() {
        let store = AiStore::open_in_memory().unwrap();
        let revision_before = store.embedding_revision().unwrap();
        store.put_embedding("a", &embedding(1.0)).unwrap();
        store.put_embedding("b", &embedding(0.0)).unwrap();
        store.put_file_location("C:/audio/a.wav", "a").unwrap();
        assert_eq!(store.embedding_revision().unwrap(), revision_before + 2);
        assert_eq!(store.embedding_segment_count("a", "model", 3).unwrap(), 1);
        assert_eq!(
            store
                .embedding_segment_count("missing", "model", 3)
                .unwrap(),
            0
        );
        assert_eq!(
            store.embeddings_for_content("a", "model", 3).unwrap(),
            vec![embedding(1.0)]
        );
        let hits = store.search_exact(&[1.0, 0.0, 0.0], "model", 10).unwrap();
        assert_eq!(hits[0].content_hash, "a");
        assert_eq!(hits[0].path_keys, vec!["C:/audio/a.wav"]);
        assert_eq!(store.model_embedding_count("model", 3).unwrap(), 2);
        assert!(AiStore::exact_search_warning(24_999).is_none());
        assert!(AiStore::exact_search_warning(25_000)
            .unwrap()
            .contains("noticeable"));
        assert!(AiStore::exact_search_warning(100_000)
            .unwrap()
            .contains("HNSW"));
    }

    #[test]
    fn hnsw_index_returns_the_nearest_normalized_vector() {
        let store = AiStore::open_in_memory().unwrap();
        store.put_embedding("a", &embedding(1.0)).unwrap();
        store.put_embedding("b", &embedding(0.0)).unwrap();
        let index = store.build_hnsw("model", 3, 2).unwrap();
        let neighbours = index.graph.search(&[1.0, 0.0, 0.0], 1, 16);
        assert_eq!(neighbours.len(), 1);
        assert_eq!(
            index.segments[neighbours[0].d_id].content_hash,
            "a".to_string()
        );
        assert!(1.0 - neighbours[0].distance > 0.99);
    }

    #[test]
    fn schema_one_is_migrated_with_an_embedding_revision() {
        let mut store = AiStore::open_in_memory().unwrap();
        store
            .connection
            .execute_batch("DROP TABLE ai_meta; PRAGMA user_version = 1;")
            .unwrap();
        store.migrate().unwrap();
        assert_eq!(store.embedding_revision().unwrap(), 0);
        let version: i64 = store
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn batch_resume_requeues_running_items_and_preserves_partial_failures() {
        let mut store = AiStore::open_in_memory().unwrap();
        let manifest = "a".repeat(64);
        let paths = vec!["C:/audio/a.wav".into(), "C:/audio/b.wav".into()];
        store.prepare_index_batch(&manifest, &paths, false).unwrap();
        let first = store.claim_next_batch_item(&manifest).unwrap().unwrap();
        assert_eq!(first.ordinal, 0);

        store.prepare_index_batch(&manifest, &paths, false).unwrap();
        let resumed = store.claim_next_batch_item(&manifest).unwrap().unwrap();
        assert_eq!(resumed.ordinal, 0);
        assert_eq!(resumed.attempts, 2);
        store
            .finish_batch_item(&manifest, 0, None, Some("provider_error"))
            .unwrap();
        let second = store.claim_next_batch_item(&manifest).unwrap().unwrap();
        store
            .finish_batch_item(&manifest, second.ordinal, Some("hash-b"), None)
            .unwrap();
        let progress = store.batch_progress(&manifest).unwrap();
        assert_eq!(progress.status, "completed_with_errors");
        assert_eq!((progress.completed, progress.failed), (1, 1));

        store.prepare_index_batch(&manifest, &paths, true).unwrap();
        let retry = store.claim_next_batch_item(&manifest).unwrap().unwrap();
        assert_eq!(retry.ordinal, 0);
        store
            .finish_batch_item(&manifest, retry.ordinal, Some("hash-a"), None)
            .unwrap();
        assert_eq!(store.batch_progress(&manifest).unwrap().status, "completed");
    }
}
