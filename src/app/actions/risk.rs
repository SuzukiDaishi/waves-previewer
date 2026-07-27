use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai::cache::canonical_json_hash;

use super::{ActionError, ActionRequest};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskClass {
    R0,
    R1,
    R2,
    R3,
    R4,
}

impl RiskClass {
    pub fn requires_approval(self) -> bool {
        matches!(self, Self::R2 | Self::R3 | Self::R4)
    }

    pub fn allows_conversation_grant(self) -> bool {
        !matches!(self, Self::R4)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: u64,
    pub action: String,
    pub risk: RiskClass,
    pub context_revision: u64,
    pub plan_hash: String,
    pub summary: String,
    pub plan: Value,
    pub expires_at_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalToken {
    pub id: u64,
    pub plan_hash: String,
}

#[derive(Clone, Debug, Default)]
pub struct ApprovalStore {
    next_id: u64,
    pending: HashMap<u64, ApprovalRequest>,
    approved: HashMap<u64, ApprovalRequest>,
}

impl ApprovalStore {
    pub fn request(
        &mut self,
        action: &ActionRequest,
        risk: RiskClass,
        context_revision: u64,
        summary: impl Into<String>,
        plan: Value,
    ) -> ApprovalRequest {
        self.next_id = self.next_id.saturating_add(1).max(1);
        let plan_hash = canonical_json_hash(&serde_json::json!({
            "action": action.action,
            "arguments": action.arguments,
            "context_revision": context_revision,
            "plan": plan,
        }));
        let request = ApprovalRequest {
            id: self.next_id,
            action: action.action.clone(),
            risk,
            context_revision,
            plan_hash,
            summary: summary.into(),
            plan,
            expires_at_unix_ms: unix_ms()
                .saturating_add(Duration::from_secs(10 * 60).as_millis() as u64),
        };
        self.pending.insert(request.id, request.clone());
        request
    }

    pub fn approve(&mut self, id: u64) -> Result<ApprovalToken, ActionError> {
        let request = self
            .pending
            .remove(&id)
            .ok_or_else(|| ActionError::ApprovalInvalid("approval request not found".into()))?;
        if request.expires_at_unix_ms < unix_ms() {
            return Err(ActionError::ApprovalInvalid(
                "approval request has expired".into(),
            ));
        }
        let token = ApprovalToken {
            id,
            plan_hash: request.plan_hash.clone(),
        };
        self.approved.insert(id, request);
        Ok(token)
    }

    pub fn reject(&mut self, id: u64) -> bool {
        self.pending.remove(&id).is_some()
    }

    pub fn consume(
        &mut self,
        token: &ApprovalToken,
        action: &ActionRequest,
        context_revision: u64,
        plan: &Value,
    ) -> Result<(), ActionError> {
        let request = self
            .approved
            .remove(&token.id)
            .ok_or_else(|| ActionError::ApprovalInvalid("approval token not found".into()))?;
        if request.expires_at_unix_ms < unix_ms() {
            return Err(ActionError::ApprovalInvalid(
                "approval token has expired".into(),
            ));
        }
        if request.action != action.action {
            return Err(ActionError::ApprovalInvalid(
                "approval action does not match".into(),
            ));
        }
        if request.plan_hash != token.plan_hash {
            return Err(ActionError::ApprovalInvalid(
                "approval plan hash does not match".into(),
            ));
        }
        if request.context_revision != context_revision {
            return Err(ActionError::StaleContext {
                expected: request.context_revision,
                actual: context_revision,
            });
        }
        let current_plan_hash = canonical_json_hash(&serde_json::json!({
            "action": action.action,
            "arguments": action.arguments,
            "context_revision": context_revision,
            "plan": plan,
        }));
        if request.plan_hash != current_plan_hash {
            return Err(ActionError::ApprovalInvalid(
                "action arguments or execution plan changed after approval".into(),
            ));
        }
        Ok(())
    }

    pub fn pending(&self) -> impl Iterator<Item = &ApprovalRequest> {
        self.pending.values()
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_is_bound_to_plan_and_revision_and_single_use() {
        let action = ActionRequest {
            action: "editor.trim.apply".into(),
            arguments: serde_json::json!({"media_id": 3}),
            expected_context_revision: Some(7),
            approval: None,
        };
        let mut store = ApprovalStore::default();
        let request = store.request(
            &action,
            RiskClass::R3,
            7,
            "Trim audio",
            serde_json::json!({}),
        );
        let token = store.approve(request.id).unwrap();
        assert!(store
            .consume(&token, &action, 8, &serde_json::json!({}))
            .is_err());

        let request = store.request(
            &action,
            RiskClass::R3,
            7,
            "Trim audio",
            serde_json::json!({}),
        );
        let token = store.approve(request.id).unwrap();
        assert!(store
            .consume(&token, &action, 7, &serde_json::json!({}))
            .is_ok());
        assert!(store
            .consume(&token, &action, 7, &serde_json::json!({}))
            .is_err());

        let request = store.request(
            &action,
            RiskClass::R3,
            7,
            "Trim audio",
            serde_json::json!({}),
        );
        let token = store.approve(request.id).unwrap();
        let mut altered = action.clone();
        altered.arguments = serde_json::json!({"media_id": 99});
        assert!(store
            .consume(&token, &altered, 7, &serde_json::json!({}))
            .is_err());
    }
}
