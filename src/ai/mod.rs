//! Provider-neutral AI contracts used by the Gemini integration.
//!
//! The module deliberately keeps audio/file mutations out of provider code.
//! Providers can suggest work and produce semantic data; application Actions
//! remain the only route that can change NeoWaves state.

pub mod cache;
pub mod mock;
pub mod models;
pub mod provider;

#[cfg(feature = "gemini")]
pub mod agent;
#[cfg(feature = "gemini")]
pub mod coordinator;
#[cfg(feature = "gemini")]
pub mod credentials;
#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "gemini")]
pub mod storage;

pub use mock::MockAiProvider;
pub use models::*;
pub use provider::{AiError, AiProvider, ProviderCapabilities};
