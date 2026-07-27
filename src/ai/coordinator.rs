use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::task::AbortHandle;

use super::models::{AiAnalysisResult, InteractionRequest, InteractionResponse, UcsEntry};
use super::provider::{AiError, AiProvider};

#[derive(Clone, Debug)]
pub enum AiCommand {
    Interact {
        job_id: u64,
        request: InteractionRequest,
    },
    Classify {
        job_id: u64,
        media_id: u64,
        path: PathBuf,
    },
    EmbedAudio {
        job_id: u64,
        media_id: u64,
        path: PathBuf,
    },
    IndexAudio {
        job_id: u64,
        media_id: u64,
        path: PathBuf,
        database_path: PathBuf,
    },
    EmbedText {
        job_id: u64,
        text: String,
    },
    SearchText {
        job_id: u64,
        query: String,
        database_path: PathBuf,
        limit: usize,
    },
    SearchSimilarAudio {
        job_id: u64,
        media_id: u64,
        path: PathBuf,
        database_path: PathBuf,
        limit: usize,
    },
    AssignUcs {
        job_id: u64,
        media_id: u64,
        path: PathBuf,
        candidates: Arc<Vec<UcsEntry>>,
    },
    TranscribeVoice {
        job_id: u64,
        media_id: u64,
        path: PathBuf,
    },
    TranscribeLyrics {
        job_id: u64,
        media_id: u64,
        path: PathBuf,
    },
    Cancel {
        job_id: u64,
    },
    Shutdown,
}

impl AiCommand {
    fn job_id(&self) -> Option<u64> {
        match self {
            Self::Interact { job_id, .. }
            | Self::Classify { job_id, .. }
            | Self::EmbedAudio { job_id, .. }
            | Self::IndexAudio { job_id, .. }
            | Self::EmbedText { job_id, .. }
            | Self::SearchText { job_id, .. }
            | Self::SearchSimilarAudio { job_id, .. }
            | Self::AssignUcs { job_id, .. }
            | Self::TranscribeVoice { job_id, .. }
            | Self::TranscribeLyrics { job_id, .. }
            | Self::Cancel { job_id } => Some(*job_id),
            Self::Shutdown => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum AiEvent {
    Started {
        job_id: u64,
        media_id: Option<u64>,
        label: String,
    },
    InteractionCompleted {
        job_id: u64,
        response: InteractionResponse,
    },
    InteractionDelta {
        job_id: u64,
        text: String,
    },
    AnalysisCompleted {
        job_id: u64,
        media_id: Option<u64>,
        result: AiAnalysisResult,
    },
    Indexed {
        job_id: u64,
        media_id: u64,
        content_hash: String,
        cached: bool,
    },
    SimilarityCompleted {
        job_id: u64,
        query_media_id: Option<u64>,
        hits: Vec<super::storage::SimilarityHit>,
        indexed_segments: usize,
        backend: super::storage::SimilaritySearchBackend,
        warning: Option<String>,
    },
    Failed {
        job_id: u64,
        media_id: Option<u64>,
        error: String,
    },
    Cancelled {
        job_id: u64,
    },
}

pub struct AiCoordinator {
    command_tx: tokio::sync::mpsc::UnboundedSender<AiCommand>,
    event_rx: std::sync::mpsc::Receiver<AiEvent>,
}

impl AiCoordinator {
    pub fn spawn(provider: Arc<dyn AiProvider>) -> Result<Self, String> {
        let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("neowaves-ai-runtime".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = event_tx.send(AiEvent::Failed {
                            job_id: 0,
                            media_id: None,
                            error: format!("could not start AI runtime: {error}"),
                        });
                        return;
                    }
                };
                runtime.block_on(async move {
                    let interactive = Arc::new(tokio::sync::Semaphore::new(1));
                    let analysis = Arc::new(tokio::sync::Semaphore::new(2));
                    let embedding = Arc::new(tokio::sync::Semaphore::new(2));
                    let mut jobs: HashMap<u64, AbortHandle> = HashMap::new();
                    while let Some(command) = command_rx.recv().await {
                        match command {
                            AiCommand::Cancel { job_id } => {
                                if let Some(handle) = jobs.remove(&job_id) {
                                    handle.abort();
                                }
                                let _ = event_tx.send(AiEvent::Cancelled { job_id });
                            }
                            AiCommand::Shutdown => {
                                for (_, handle) in jobs.drain() {
                                    handle.abort();
                                }
                                break;
                            }
                            command => {
                                let job_id = command.job_id().unwrap_or_default();
                                let provider = Arc::clone(&provider);
                                let event_tx = event_tx.clone();
                                let interactive = Arc::clone(&interactive);
                                let analysis = Arc::clone(&analysis);
                                let embedding = Arc::clone(&embedding);
                                let task = tokio::spawn(async move {
                                    run_command(
                                        provider,
                                        command,
                                        event_tx,
                                        interactive,
                                        analysis,
                                        embedding,
                                    )
                                    .await;
                                });
                                jobs.insert(job_id, task.abort_handle());
                            }
                        }
                    }
                });
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            command_tx,
            event_rx,
        })
    }

    pub fn send(&self, command: AiCommand) -> Result<(), String> {
        self.command_tx
            .send(command)
            .map_err(|_| "AI runtime is not running".to_string())
    }

    pub fn try_recv(&self) -> Result<AiEvent, std::sync::mpsc::TryRecvError> {
        self.event_rx.try_recv()
    }
}

impl Drop for AiCoordinator {
    fn drop(&mut self) {
        let _ = self.command_tx.send(AiCommand::Shutdown);
    }
}

async fn run_command(
    provider: Arc<dyn AiProvider>,
    command: AiCommand,
    event_tx: std::sync::mpsc::Sender<AiEvent>,
    interactive: Arc<tokio::sync::Semaphore>,
    analysis: Arc<tokio::sync::Semaphore>,
    embedding: Arc<tokio::sync::Semaphore>,
) {
    match command {
        AiCommand::Interact { job_id, request } => {
            let _permit = interactive.acquire().await;
            let _ = event_tx.send(AiEvent::Started {
                job_id,
                media_id: None,
                label: "Gemini interaction".into(),
            });
            let delta_tx = event_tx.clone();
            let on_text_delta = move |text: String| {
                if !text.is_empty() {
                    let _ = delta_tx.send(AiEvent::InteractionDelta { job_id, text });
                }
            };
            match provider.interact_stream(request, &on_text_delta).await {
                Ok(response) => {
                    let _ = event_tx.send(AiEvent::InteractionCompleted { job_id, response });
                }
                Err(error) => {
                    let _ = event_tx.send(AiEvent::Failed {
                        job_id,
                        media_id: None,
                        error: error.to_string(),
                    });
                }
            }
        }
        AiCommand::Classify {
            job_id,
            media_id,
            path,
        } => {
            let _permit = analysis.acquire().await;
            send_started(&event_tx, job_id, Some(media_id), "Audio classification");
            finish_analysis(
                job_id,
                Some(media_id),
                provider
                    .classify_audio(&path)
                    .await
                    .map(AiAnalysisResult::Classification),
                &event_tx,
            );
        }
        AiCommand::EmbedAudio {
            job_id,
            media_id,
            path,
        } => {
            let _permit = embedding.acquire().await;
            send_started(&event_tx, job_id, Some(media_id), "Audio embedding");
            finish_analysis(
                job_id,
                Some(media_id),
                provider
                    .embed_audio(&path)
                    .await
                    .map(AiAnalysisResult::Embedding),
                &event_tx,
            );
        }
        AiCommand::IndexAudio {
            job_id,
            media_id,
            path,
            database_path,
        } => {
            let _permit = embedding.acquire().await;
            send_started(&event_tx, job_id, Some(media_id), "Audio embedding index");
            match ensure_audio_embeddings(provider.as_ref(), &path, &database_path, job_id).await {
                Ok(indexed) => {
                    let _ = event_tx.send(AiEvent::Indexed {
                        job_id,
                        media_id,
                        content_hash: indexed.content_hash,
                        cached: indexed.cached,
                    });
                }
                Err(error) => {
                    let _ = event_tx.send(AiEvent::Failed {
                        job_id,
                        media_id: Some(media_id),
                        error: error.to_string(),
                    });
                }
            }
        }
        AiCommand::EmbedText { job_id, text } => {
            let _permit = embedding.acquire().await;
            send_started(&event_tx, job_id, None, "Text embedding");
            finish_analysis(
                job_id,
                None,
                provider
                    .embed_text(&text)
                    .await
                    .map(AiAnalysisResult::Embedding),
                &event_tx,
            );
        }
        AiCommand::SearchText {
            job_id,
            query,
            database_path,
            limit,
        } => {
            let _permit = embedding.acquire().await;
            send_started(&event_tx, job_id, None, "Text-to-audio similarity search");
            let query_embedding = match provider.embed_text(&query).await {
                Ok(value) => value,
                Err(error) => {
                    let _ = event_tx.send(AiEvent::Failed {
                        job_id,
                        media_id: None,
                        error: error.to_string(),
                    });
                    return;
                }
            };
            let store = match super::storage::AiStore::open(&database_path) {
                Ok(store) => store,
                Err(error) => {
                    let _ = event_tx.send(AiEvent::Failed {
                        job_id,
                        media_id: None,
                        error: error.to_string(),
                    });
                    return;
                }
            };
            let indexed_segments = match store
                .model_embedding_count(&query_embedding.model, query_embedding.dimensions)
            {
                Ok(count) => count,
                Err(error) => {
                    let _ = event_tx.send(AiEvent::Failed {
                        job_id,
                        media_id: None,
                        error: error.to_string(),
                    });
                    return;
                }
            };
            let warning = super::storage::AiStore::exact_search_warning(indexed_segments);
            match store.search_auto(&query_embedding.values, &query_embedding.model, limit) {
                Ok(outcome) => {
                    let _ = event_tx.send(AiEvent::SimilarityCompleted {
                        job_id,
                        query_media_id: None,
                        hits: outcome.hits,
                        indexed_segments,
                        backend: outcome.backend,
                        warning,
                    });
                }
                Err(error) => {
                    let _ = event_tx.send(AiEvent::Failed {
                        job_id,
                        media_id: None,
                        error: error.to_string(),
                    });
                }
            }
        }
        AiCommand::SearchSimilarAudio {
            job_id,
            media_id,
            path,
            database_path,
            limit,
        } => {
            let _permit = embedding.acquire().await;
            send_started(
                &event_tx,
                job_id,
                Some(media_id),
                "Audio-to-audio similarity search",
            );
            let indexed =
                match ensure_audio_embeddings(provider.as_ref(), &path, &database_path, job_id)
                    .await
                {
                    Ok(indexed) => indexed,
                    Err(error) => {
                        let _ = event_tx.send(AiEvent::Failed {
                            job_id,
                            media_id: Some(media_id),
                            error: error.to_string(),
                        });
                        return;
                    }
                };
            let _ = event_tx.send(AiEvent::Indexed {
                job_id,
                media_id,
                content_hash: indexed.content_hash.clone(),
                cached: indexed.cached,
            });
            let store = match super::storage::AiStore::open(&database_path) {
                Ok(store) => store,
                Err(error) => {
                    let _ = event_tx.send(AiEvent::Failed {
                        job_id,
                        media_id: Some(media_id),
                        error: error.to_string(),
                    });
                    return;
                }
            };
            let indexed_segments =
                match store.model_embedding_count(&indexed.model, indexed.dimensions) {
                    Ok(count) => count,
                    Err(error) => {
                        let _ = event_tx.send(AiEvent::Failed {
                            job_id,
                            media_id: Some(media_id),
                            error: error.to_string(),
                        });
                        return;
                    }
                };
            let candidate_limit = limit.saturating_add(8).clamp(1, 200);
            let mut best_hits = HashMap::new();
            let mut backend = super::storage::SimilaritySearchBackend::ExactCosine;
            for query_embedding in &indexed.embeddings {
                let outcome = match store.search_auto(
                    &query_embedding.values,
                    &indexed.model,
                    candidate_limit,
                ) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let _ = event_tx.send(AiEvent::Failed {
                            job_id,
                            media_id: Some(media_id),
                            error: error.to_string(),
                        });
                        return;
                    }
                };
                backend = outcome.backend;
                for hit in outcome
                    .hits
                    .into_iter()
                    .filter(|hit| hit.content_hash != indexed.content_hash)
                {
                    let key = (
                        hit.content_hash.clone(),
                        hit.segment_start_ms,
                        hit.segment_end_ms,
                    );
                    best_hits
                        .entry(key)
                        .and_modify(|best: &mut super::storage::SimilarityHit| {
                            if hit.score > best.score {
                                *best = hit.clone();
                            }
                        })
                        .or_insert(hit);
                }
            }
            let mut hits = best_hits.into_values().collect::<Vec<_>>();
            hits.sort_by(|left, right| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(limit.clamp(1, 200));
            let warning = if hits.is_empty() {
                Some(
                    "No other indexed sounds are available. Index candidate sounds, then retry."
                        .to_string(),
                )
            } else {
                super::storage::AiStore::exact_search_warning(indexed_segments)
            };
            let _ = event_tx.send(AiEvent::SimilarityCompleted {
                job_id,
                query_media_id: Some(media_id),
                hits,
                indexed_segments,
                backend,
                warning,
            });
        }
        AiCommand::AssignUcs {
            job_id,
            media_id,
            path,
            candidates,
        } => {
            let _permit = analysis.acquire().await;
            send_started(&event_tx, job_id, Some(media_id), "UCS suggestions");
            finish_analysis(
                job_id,
                Some(media_id),
                provider
                    .assign_ucs_tags(&path, candidates.as_slice())
                    .await
                    .map(AiAnalysisResult::Ucs),
                &event_tx,
            );
        }
        AiCommand::TranscribeVoice {
            job_id,
            media_id,
            path,
        } => {
            let _permit = analysis.acquire().await;
            send_started(&event_tx, job_id, Some(media_id), "Voice transcription");
            finish_analysis(
                job_id,
                Some(media_id),
                provider
                    .transcribe_voice(&path)
                    .await
                    .map(AiAnalysisResult::Voice),
                &event_tx,
            );
        }
        AiCommand::TranscribeLyrics {
            job_id,
            media_id,
            path,
        } => {
            let _permit = analysis.acquire().await;
            send_started(&event_tx, job_id, Some(media_id), "Lyrics transcription");
            finish_analysis(
                job_id,
                Some(media_id),
                provider
                    .transcribe_lyrics(&path)
                    .await
                    .map(AiAnalysisResult::Lyrics),
                &event_tx,
            );
        }
        AiCommand::Cancel { .. } | AiCommand::Shutdown => {}
    }
}

struct IndexedAudioEmbeddings {
    content_hash: String,
    model: String,
    dimensions: usize,
    embeddings: Vec<super::models::AudioEmbedding>,
    cached: bool,
}

async fn ensure_audio_embeddings(
    provider: &dyn AiProvider,
    path: &std::path::Path,
    database_path: &std::path::Path,
    job_id: u64,
) -> Result<IndexedAudioEmbeddings, AiError> {
    let hash_path = path.to_path_buf();
    let (content_hash, duration_ms) = tokio::task::spawn_blocking(move || {
        let hash = super::cache::calculate_content_hash(&hash_path)
            .map_err(|error| AiError::FileRead(error.to_string()))?;
        let duration_ms = crate::audio_io::read_audio_info(&hash_path)
            .ok()
            .and_then(|info| info.duration_secs)
            .filter(|duration| duration.is_finite() && *duration >= 0.0)
            .map(|duration| (f64::from(duration) * 1_000.0).round() as u64);
        Ok::<_, AiError>((hash, duration_ms))
    })
    .await
    .map_err(|error| AiError::Service(format!("audio hashing worker failed: {error}")))??;

    let capabilities = provider.capabilities();
    let model = capabilities.embedding_model.unwrap_or_default();
    if model.trim().is_empty() || capabilities.embedding_dimensions == 0 {
        return Err(AiError::Unsupported(
            "the provider does not expose an audio embedding model".into(),
        ));
    }
    let dimensions = capabilities.embedding_dimensions;
    let expected_segments =
        expected_embedding_segments(duration_ms, capabilities.max_embedding_audio_seconds);
    let store = super::storage::AiStore::open(database_path)?;
    let cached_embeddings = store.embeddings_for_content(&content_hash, &model, dimensions)?;
    let cached = cached_embeddings.len() >= expected_segments;
    let embeddings = if cached {
        store.put_file_location(&path.to_string_lossy(), &content_hash)?;
        cached_embeddings
    } else {
        let prepare_path = path.to_path_buf();
        let max_seconds = capabilities.max_embedding_audio_seconds;
        let prepared = tokio::task::spawn_blocking(move || {
            prepare_embedding_audio(&prepare_path, duration_ms, max_seconds, job_id)
        })
        .await
        .map_err(|error| {
            AiError::Service(format!(
                "audio embedding preparation worker failed: {error}"
            ))
        })??;
        let mut embeddings = Vec::with_capacity(prepared.len());
        for segment in &prepared {
            let mut embedding = provider.embed_audio(&segment.path).await?;
            embedding.segment_start_ms = segment.start_ms;
            embedding.segment_end_ms = segment.end_ms;
            embeddings.push(embedding);
        }
        let format = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        store.put_content(&content_hash, duration_ms.unwrap_or_default(), format)?;
        for embedding in &embeddings {
            store.put_embedding(&content_hash, embedding)?;
        }
        store.put_file_location(&path.to_string_lossy(), &content_hash)?;
        embeddings
    };
    if embeddings.is_empty() {
        return Err(AiError::Storage(
            "audio embedding index returned no query segments".into(),
        ));
    }
    Ok(IndexedAudioEmbeddings {
        content_hash,
        model,
        dimensions,
        embeddings,
        cached,
    })
}

const EMBEDDING_SEGMENT_SECONDS: u64 = 60;

fn expected_embedding_segments(duration_ms: Option<u64>, max_seconds: u32) -> usize {
    if duration_ms.is_some_and(|duration| duration > u64::from(max_seconds) * 1_000) {
        3
    } else {
        1
    }
}

#[derive(Debug)]
struct PreparedEmbeddingAudio {
    path: PathBuf,
    start_ms: u64,
    end_ms: u64,
    temporary: bool,
}

impl Drop for PreparedEmbeddingAudio {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn prepare_embedding_audio(
    path: &std::path::Path,
    duration_ms: Option<u64>,
    max_seconds: u32,
    job_id: u64,
) -> Result<Vec<PreparedEmbeddingAudio>, AiError> {
    let embedding_native = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("wav") || extension.eq_ignore_ascii_case("mp3")
        });
    let within_limit =
        duration_ms.is_some_and(|duration| duration <= u64::from(max_seconds) * 1_000);
    let within_inline_limit = std::fs::metadata(path)
        .ok()
        .is_some_and(|metadata| metadata.len() <= super::gemini::MAX_INLINE_AUDIO_BYTES as u64);
    if embedding_native && within_limit && within_inline_limit {
        return Ok(vec![PreparedEmbeddingAudio {
            path: path.to_path_buf(),
            start_ms: 0,
            end_ms: duration_ms.unwrap_or_default(),
            temporary: false,
        }]);
    }

    let (channels, sample_rate) = crate::audio_io::decode_audio_multi(path)
        .map_err(|error| AiError::FileRead(error.to_string()))?;
    let frames = channels.iter().map(Vec::len).min().unwrap_or_default();
    if frames == 0 || sample_rate == 0 {
        return Err(AiError::FileRead("decoded audio is empty".into()));
    }
    let actual_duration_ms = ((frames as f64 / f64::from(sample_rate)) * 1_000.0).round() as u64;
    let duration_segment_frames =
        usize::try_from(u64::from(sample_rate).saturating_mul(EMBEDDING_SEGMENT_SECONDS))
            .unwrap_or(usize::MAX)
            .max(1);
    let pcm16_bytes_per_frame = channels.len().max(1).saturating_mul(2);
    let inline_audio_budget = super::gemini::MAX_INLINE_AUDIO_BYTES.saturating_sub(64 * 1024);
    let size_segment_frames = inline_audio_budget
        .checked_div(pcm16_bytes_per_frame)
        .unwrap_or(1)
        .max(1);
    let segment_frames = duration_segment_frames.min(size_segment_frames);
    let ranges = if actual_duration_ms > u64::from(max_seconds) * 1_000 {
        let start = (0, segment_frames.min(frames));
        let middle_start = frames.saturating_sub(segment_frames) / 2;
        let middle = (
            middle_start,
            middle_start.saturating_add(segment_frames).min(frames),
        );
        let end = (frames.saturating_sub(segment_frames), frames);
        vec![start, middle, end]
    } else {
        vec![(0, frames)]
    };

    let temp_dir = std::env::temp_dir()
        .join("NeoWaves")
        .join("gemini-embedding-segments");
    std::fs::create_dir_all(&temp_dir).map_err(|error| AiError::FileRead(error.to_string()))?;
    let mut prepared = Vec::with_capacity(ranges.len());
    for (index, range) in ranges.into_iter().enumerate() {
        let segment_path = temp_dir.join(format!(
            "segment_{}_{}_{}.wav",
            std::process::id(),
            job_id,
            index
        ));
        if let Err(error) = crate::wave::export_selection_wav_with_depth(
            &channels,
            sample_rate,
            range,
            &segment_path,
            Some(crate::wave::WavBitDepth::Pcm16),
        ) {
            return Err(AiError::FileRead(error.to_string()));
        }
        prepared.push(PreparedEmbeddingAudio {
            path: segment_path,
            start_ms: frames_to_ms(range.0, sample_rate),
            end_ms: frames_to_ms(range.1, sample_rate),
            temporary: true,
        });
    }
    Ok(prepared)
}

fn frames_to_ms(frame: usize, sample_rate: u32) -> u64 {
    ((frame as f64 / f64::from(sample_rate.max(1))) * 1_000.0).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_event(
        coordinator: &AiCoordinator,
        job_id: u64,
        predicate: impl Fn(&AiEvent) -> bool,
    ) -> AiEvent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match coordinator.try_recv() {
                Ok(event) if event_job_id(&event) == Some(job_id) && predicate(&event) => {
                    return event;
                }
                Ok(AiEvent::Failed {
                    job_id: failed_job,
                    error,
                    ..
                }) if failed_job == job_id => panic!("AI test job failed: {error}"),
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("AI coordinator disconnected")
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for AI test job {job_id}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn event_job_id(event: &AiEvent) -> Option<u64> {
        match event {
            AiEvent::Started { job_id, .. }
            | AiEvent::InteractionCompleted { job_id, .. }
            | AiEvent::InteractionDelta { job_id, .. }
            | AiEvent::AnalysisCompleted { job_id, .. }
            | AiEvent::Indexed { job_id, .. }
            | AiEvent::SimilarityCompleted { job_id, .. }
            | AiEvent::Failed { job_id, .. }
            | AiEvent::Cancelled { job_id } => Some(*job_id),
        }
    }

    #[test]
    fn long_embedding_audio_requires_three_cache_segments() {
        assert_eq!(expected_embedding_segments(Some(180_000), 180), 1);
        assert_eq!(expected_embedding_segments(Some(180_001), 180), 3);
        assert_eq!(expected_embedding_segments(None, 180), 1);
    }

    #[test]
    fn embedding_segment_frame_conversion_is_stable() {
        assert_eq!(frames_to_ms(48_000, 48_000), 1_000);
        assert_eq!(frames_to_ms(24_000, 48_000), 500);
    }

    #[test]
    fn selected_audio_similarity_indexes_the_reference_and_excludes_it_from_hits() {
        let unique = format!(
            "neowaves_ai_similar_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&directory).unwrap();
        let reference = directory.join("reference.wav");
        let candidate = directory.join("candidate.wav");
        crate::wave::export_channels_audio(&[vec![0.0; 4_800]], 48_000, &reference).unwrap();
        crate::wave::export_channels_audio(&[vec![0.25; 4_800]], 48_000, &candidate).unwrap();
        let database_path = directory.join("semantic.sqlite3");
        let coordinator =
            AiCoordinator::spawn(std::sync::Arc::new(crate::ai::MockAiProvider::default()))
                .unwrap();

        let mut reference_hash = String::new();
        for (job_id, media_id, path) in [(1, 11, reference.clone()), (2, 22, candidate.clone())] {
            coordinator
                .send(AiCommand::IndexAudio {
                    job_id,
                    media_id,
                    path,
                    database_path: database_path.clone(),
                })
                .unwrap();
            let event = wait_for_event(&coordinator, job_id, |event| {
                matches!(event, AiEvent::Indexed { .. })
            });
            if let AiEvent::Indexed { content_hash, .. } = event {
                if media_id == 11 {
                    reference_hash = content_hash;
                }
            }
        }

        coordinator
            .send(AiCommand::SearchSimilarAudio {
                job_id: 3,
                media_id: 11,
                path: reference,
                database_path,
                limit: 20,
            })
            .unwrap();
        let event = wait_for_event(&coordinator, 3, |event| {
            matches!(event, AiEvent::SimilarityCompleted { .. })
        });
        let AiEvent::SimilarityCompleted {
            query_media_id,
            hits,
            indexed_segments,
            ..
        } = event
        else {
            unreachable!();
        };
        assert_eq!(query_media_id, Some(11));
        assert_eq!(indexed_segments, 2);
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit.content_hash != reference_hash));
        assert!(hits
            .iter()
            .flat_map(|hit| &hit.path_keys)
            .any(|path| std::path::Path::new(path) == candidate));

        drop(coordinator);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn embedding_cache_detects_same_path_content_changes_by_hash() {
        let unique = format!(
            "neowaves_ai_hash_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("mutable.wav");
        let database_path = directory.join("semantic.sqlite3");
        crate::wave::export_channels_audio(&[vec![0.0; 4_800]], 48_000, &path).unwrap();
        let coordinator =
            AiCoordinator::spawn(std::sync::Arc::new(crate::ai::MockAiProvider::default()))
                .unwrap();

        let index = |job_id| {
            coordinator
                .send(AiCommand::IndexAudio {
                    job_id,
                    media_id: 7,
                    path: path.clone(),
                    database_path: database_path.clone(),
                })
                .unwrap();
            let event = wait_for_event(&coordinator, job_id, |event| {
                matches!(event, AiEvent::Indexed { .. })
            });
            let AiEvent::Indexed {
                content_hash,
                cached,
                ..
            } = event
            else {
                unreachable!();
            };
            (content_hash, cached)
        };

        let (first_hash, first_cached) = index(1);
        let (same_hash, same_cached) = index(2);
        assert!(!first_cached);
        assert!(same_cached);
        assert_eq!(first_hash, same_hash);

        crate::wave::export_channels_audio(&[vec![0.5; 4_800]], 48_000, &path).unwrap();
        let (changed_hash, changed_cached) = index(3);
        assert!(!changed_cached);
        assert_ne!(first_hash, changed_hash);

        drop(coordinator);
        let _ = std::fs::remove_dir_all(directory);
    }
}

fn send_started(
    event_tx: &std::sync::mpsc::Sender<AiEvent>,
    job_id: u64,
    media_id: Option<u64>,
    label: &str,
) {
    let _ = event_tx.send(AiEvent::Started {
        job_id,
        media_id,
        label: label.into(),
    });
}

fn finish_analysis(
    job_id: u64,
    media_id: Option<u64>,
    result: Result<AiAnalysisResult, super::provider::AiError>,
    event_tx: &std::sync::mpsc::Sender<AiEvent>,
) {
    match result {
        Ok(result) => {
            let _ = event_tx.send(AiEvent::AnalysisCompleted {
                job_id,
                media_id,
                result,
            });
        }
        Err(error) => {
            let _ = event_tx.send(AiEvent::Failed {
                job_id,
                media_id,
                error: error.to_string(),
            });
        }
    }
}
