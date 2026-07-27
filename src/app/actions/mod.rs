//! Provider-independent application Action contract.
//!
//! Actions expose a deliberately bounded subset of NeoWaves. Arbitrary shell
//! commands, raw filesystem access, credential readback, and forced process
//! termination are never registered here.

pub mod context;
pub mod coverage;
pub mod dispatcher;
pub mod registry;
pub mod risk;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use context::{ContextSnapshot, EditorRangeSnapshot};
pub use dispatcher::dispatch_action;
pub use registry::{ActionDescriptor, ActionRegistry};
pub use risk::{ApprovalRequest, ApprovalStore, ApprovalToken, RiskClass};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionRequest {
    pub action: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub expected_context_revision: Option<u64>,
    #[serde(default)]
    pub approval: Option<ApprovalToken>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionResult {
    pub action: String,
    pub summary: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub affected_media_ids: Vec<super::types::MediaId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DispatchOutcome {
    Completed { result: ActionResult },
    NeedsApproval { request: ApprovalRequest },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionError {
    UnknownAction(String),
    InvalidArguments(String),
    Precondition(String),
    StaleContext { expected: u64, actual: u64 },
    ApprovalRequired,
    ApprovalInvalid(String),
    Execution(String),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownAction(name) => write!(f, "unknown action: {name}"),
            Self::InvalidArguments(message) => write!(f, "invalid action arguments: {message}"),
            Self::Precondition(message) => write!(f, "action precondition failed: {message}"),
            Self::StaleContext { expected, actual } => {
                write!(
                    f,
                    "action context is stale (expected {expected}, current {actual})"
                )
            }
            Self::ApprovalRequired => write!(f, "action requires explicit approval"),
            Self::ApprovalInvalid(message) => write!(f, "action approval is invalid: {message}"),
            Self::Execution(message) => write!(f, "action execution failed: {message}"),
        }
    }
}

impl std::error::Error for ActionError {}
