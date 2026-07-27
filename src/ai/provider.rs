use std::fmt;
use std::path::Path;

use async_trait::async_trait;

use super::models::{
    AudioClassification, AudioEmbedding, InteractionRequest, InteractionResponse, UcsAnalysis,
    UcsEntry, VoiceTranscript,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub provider_name: String,
    pub interaction_model: String,
    pub embedding_model: Option<String>,
    pub supports_audio_understanding: bool,
    pub supports_audio_embedding: bool,
    pub supports_files_api: bool,
    pub max_inline_bytes: usize,
    pub max_embedding_audio_seconds: u32,
    pub embedding_dimensions: usize,
}

#[derive(Debug)]
pub enum AiError {
    MissingApiKey,
    FileRead(String),
    Unsupported(String),
    Network(String),
    RateLimited { retry_after_seconds: Option<u64> },
    Service(String),
    InvalidResponse(String),
    SchemaValidation(String),
    Cancelled,
    Storage(String),
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey => write!(f, "Gemini API key is not configured"),
            Self::FileRead(message) => write!(f, "could not read audio file: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported AI operation: {message}"),
            Self::Network(message) => write!(f, "network error: {message}"),
            Self::RateLimited {
                retry_after_seconds,
            } => match retry_after_seconds {
                Some(seconds) => write!(f, "API rate limited; retry after {seconds}s"),
                None => write!(f, "API rate limited"),
            },
            Self::Service(message) => write!(f, "AI service error: {message}"),
            Self::InvalidResponse(message) => write!(f, "invalid AI response: {message}"),
            Self::SchemaValidation(message) => {
                write!(f, "AI schema validation failed: {message}")
            }
            Self::Cancelled => write!(f, "AI operation was cancelled"),
            Self::Storage(message) => write!(f, "AI storage error: {message}"),
        }
    }
}

impl std::error::Error for AiError {}

#[async_trait]
pub trait AiProvider: Send + Sync {
    fn capabilities(&self) -> ProviderCapabilities;

    async fn interact(&self, request: InteractionRequest) -> Result<InteractionResponse, AiError>;

    async fn interact_stream(
        &self,
        request: InteractionRequest,
        on_text_delta: &(dyn Fn(String) + Send + Sync),
    ) -> Result<InteractionResponse, AiError> {
        let response = self.interact(request).await?;
        if !response.output_text.is_empty() {
            on_text_delta(response.output_text.clone());
        }
        Ok(response)
    }

    async fn classify_audio(&self, path: &Path) -> Result<AudioClassification, AiError>;

    async fn embed_audio(&self, path: &Path) -> Result<AudioEmbedding, AiError>;

    async fn embed_text(&self, text: &str) -> Result<AudioEmbedding, AiError>;

    async fn assign_ucs_tags(
        &self,
        path: &Path,
        candidates: &[UcsEntry],
    ) -> Result<UcsAnalysis, AiError>;

    async fn transcribe_voice(&self, path: &Path) -> Result<VoiceTranscript, AiError>;

    async fn transcribe_lyrics(&self, path: &Path) -> Result<VoiceTranscript, AiError>;
}
