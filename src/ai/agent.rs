use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::models::{FunctionResult, InteractionInput};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentLimits {
    pub max_steps: u32,
    pub max_files: usize,
    pub token_soft_budget: u64,
}

impl Default for AgentLimits {
    fn default() -> Self {
        Self {
            max_steps: 12,
            max_files: 200,
            token_soft_budget: 32_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentTraceEntry {
    pub step: u32,
    pub action: String,
    pub target_count: usize,
    pub duration_ms: u64,
    pub ok: bool,
    pub summary: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSession {
    pub previous_interaction_id: Option<String>,
    pub steps: u32,
    pub total_tokens: u64,
    pub trace: Vec<AgentTraceEntry>,
}

impl AgentSession {
    pub fn can_continue(&self, limits: &AgentLimits) -> Result<(), String> {
        if self.steps >= limits.max_steps {
            return Err(format!("agent step limit {} reached", limits.max_steps));
        }
        if self.total_tokens >= limits.token_soft_budget {
            return Err(format!(
                "agent token soft budget {} reached",
                limits.token_soft_budget
            ));
        }
        Ok(())
    }

    pub fn record_interaction(&mut self, id: String, tokens: u64) {
        self.previous_interaction_id = Some(id);
        self.steps = self.steps.saturating_add(1);
        self.total_tokens = self.total_tokens.saturating_add(tokens);
    }
}

pub fn function_results_input(results: &[FunctionResult]) -> InteractionInput {
    InteractionInput::Content(
        results
            .iter()
            .map(|result| {
                json!({
                    "type": "function_result",
                    "call_id": result.call_id,
                    "name": result.name,
                    "result": result.result,
                    "is_error": result.is_error
                })
            })
            .collect::<Vec<Value>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_stops_at_the_configured_step_limit() {
        let limits = AgentLimits {
            max_steps: 2,
            ..Default::default()
        };
        let mut session = AgentSession::default();
        assert!(session.can_continue(&limits).is_ok());
        session.record_interaction("one".into(), 1);
        assert!(session.can_continue(&limits).is_ok());
        session.record_interaction("two".into(), 1);
        assert!(session.can_continue(&limits).is_err());
    }
}
