use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::risk::RiskClass;

pub(super) const UI_TEXT_CONTROL_IDS: &[&str] = &["list.search.query", "ai.similarity.query"];
pub(super) const UI_CHECKBOX_CONTROL_IDS: &[&str] = &["list.search.regex", "list.auto_play"];
pub(super) const UI_BUTTON_CONTROL_IDS: &[&str] = &[
    "assistant.open",
    "ai.review.open",
    "ai.index_visible_embeddings",
    "ai.index_selected_embeddings",
    "ai.cancel_embedding_batch",
    "ai.search_similarity_text",
    "ai.find_similar_selected",
    "ai.select_similarity_results",
    "list.search.clear",
    "list.selection.select_all",
    "list.selection.clear",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub risk: RiskClass,
    pub arguments_schema: Value,
    pub result_schema: Value,
    pub changes_state: bool,
    pub supports_undo: bool,
}

#[derive(Clone, Debug)]
pub struct ActionRegistry {
    descriptors: BTreeMap<&'static str, ActionDescriptor>,
}

impl Default for ActionRegistry {
    fn default() -> Self {
        let descriptors = [
            descriptor(
                "actions.coverage",
                "Return the auditable Action-domain coverage and explicit withholding reasons.",
                RiskClass::R0,
                object_schema(&[]),
                false,
            ),
            descriptor(
                "context.get",
                "Return the current workspace, stable media IDs, selection and editor range.",
                RiskClass::R0,
                object_schema(&[]),
                false,
            ),
            descriptor(
                "jobs.list",
                "Return active NeoWaves background jobs without exposing file paths.",
                RiskClass::R0,
                object_schema(&[]),
                false,
            ),
            descriptor(
                "session.inspect",
                "Return non-secret session status and counts.",
                RiskClass::R0,
                object_schema(&[]),
                false,
            ),
            descriptor(
                "list.inspect_selection",
                "Inspect selected media by stable MediaId.",
                RiskClass::R0,
                object_schema(&[]),
                false,
            ),
            descriptor(
                "list.query",
                "Search visible media names and return a bounded MediaId result set.",
                RiskClass::R0,
                object_schema(&["query"]),
                false,
            ),
            descriptor(
                "list.select",
                "Replace the visible list selection using stable MediaId values.",
                RiskClass::R1,
                object_schema(&["media_ids"]),
                true,
            ),
            descriptor(
                "ui.controls.list",
                "List the allowlisted NeoWaves buttons, checkboxes, and text inputs with current state.",
                RiskClass::R0,
                object_schema(&[]),
                false,
            ),
            descriptor(
                "ui.text.set",
                "Enter text into one allowlisted NeoWaves text input by stable control_id.",
                RiskClass::R1,
                object_schema(&["control_id", "text"]),
                true,
            ),
            descriptor(
                "ui.checkbox.set",
                "Set one allowlisted NeoWaves checkbox by stable control_id.",
                RiskClass::R1,
                object_schema(&["control_id", "checked"]),
                true,
            ),
            descriptor(
                "ui.button.press",
                "Press one allowlisted NeoWaves button by stable control_id; cloud audio still follows the foreground upload policy.",
                RiskClass::R1,
                object_schema(&["control_id"]),
                true,
            ),
            descriptor(
                "workspace.show",
                "Show List, Editor, Effect Graph, or Recording workspace.",
                RiskClass::R1,
                object_schema(&["workspace"]),
                true,
            ),
            descriptor(
                "transport.play_pause",
                "Toggle playback in the active workspace.",
                RiskClass::R1,
                object_schema(&[]),
                true,
            ),
            descriptor(
                "transport.stop",
                "Stop current playback.",
                RiskClass::R1,
                object_schema(&[]),
                true,
            ),
            descriptor(
                "editor.inspect",
                "Inspect a loaded editor tab by stable MediaId without exposing its path.",
                RiskClass::R0,
                object_schema(&["media_id"]),
                false,
            ),
            descriptor_with_undo(
                "editor.gain.apply",
                "Apply bounded gain to an editor range and create an Undo point.",
                RiskClass::R3,
                object_schema(&["media_id", "gain_db"]),
            ),
            descriptor_with_undo(
                "editor.trim.apply",
                "Destructively trim a loaded editor tab to a millisecond range with Undo.",
                RiskClass::R3,
                object_schema(&["media_id", "start_ms", "end_ms"]),
            ),
            descriptor_with_undo(
                "loop.set",
                "Set the editor playback loop to a bounded millisecond range with Undo.",
                RiskClass::R2,
                object_schema(&["media_id", "start_ms", "end_ms"]),
            ),
            descriptor_with_undo(
                "markers.add",
                "Add one labeled editor marker at a bounded millisecond position with Undo.",
                RiskClass::R2,
                object_schema(&["media_id", "position_ms"]),
            ),
            descriptor(
                "labels.accept",
                "Accept the current AI classification suggestion for one stable MediaId.",
                RiskClass::R3,
                object_schema(&["media_id", "class"]),
                true,
            ),
        ]
        .into_iter()
        .map(|descriptor| (descriptor.name, descriptor))
        .collect();
        Self { descriptors }
    }
}

impl ActionRegistry {
    pub fn get(&self, name: &str) -> Option<&ActionDescriptor> {
        self.descriptors.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ActionDescriptor> {
        self.descriptors.values()
    }

    pub fn function_declarations(
        &self,
        max_risk: RiskClass,
    ) -> Vec<crate::ai::models::FunctionDeclaration> {
        self.iter()
            .filter(|descriptor| descriptor.risk <= max_risk)
            .map(|descriptor| crate::ai::models::FunctionDeclaration {
                name: descriptor.name.to_string(),
                description: descriptor.description.to_string(),
                parameters: descriptor.arguments_schema.clone(),
            })
            .collect()
    }
}

fn descriptor(
    name: &'static str,
    description: &'static str,
    risk: RiskClass,
    arguments_schema: Value,
    changes_state: bool,
) -> ActionDescriptor {
    ActionDescriptor {
        name,
        description,
        risk,
        arguments_schema: scoped_object_schema(name, arguments_schema),
        result_schema: json!({"type": "object"}),
        changes_state,
        supports_undo: false,
    }
}

fn descriptor_with_undo(
    name: &'static str,
    description: &'static str,
    risk: RiskClass,
    arguments_schema: Value,
) -> ActionDescriptor {
    ActionDescriptor {
        name,
        description,
        risk,
        arguments_schema: scoped_object_schema(name, arguments_schema),
        result_schema: json!({"type": "object"}),
        changes_state: true,
        supports_undo: true,
    }
}

fn scoped_object_schema(name: &str, mut schema: Value) -> Value {
    let allowed = action_argument_keys(name);
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.retain(|key, _| allowed.contains(&key.as_str()));
        let control_ids = match name {
            "ui.text.set" => Some(UI_TEXT_CONTROL_IDS),
            "ui.checkbox.set" => Some(UI_CHECKBOX_CONTROL_IDS),
            "ui.button.press" => Some(UI_BUTTON_CONTROL_IDS),
            _ => None,
        };
        if let Some(control_ids) = control_ids {
            if let Some(control_id) = properties.get_mut("control_id") {
                control_id["enum"] = json!(control_ids);
            }
        }
    }
    schema
}

pub(super) fn action_argument_keys(name: &str) -> &'static [&'static str] {
    match name {
        "list.query" => &["query", "limit"],
        "list.select" => &["media_ids"],
        "ui.text.set" => &["control_id", "text"],
        "ui.checkbox.set" => &["control_id", "checked"],
        "ui.button.press" => &["control_id"],
        "workspace.show" => &["workspace"],
        "editor.inspect" => &["media_id"],
        "editor.gain.apply" => &["media_id", "gain_db", "start_ms", "end_ms"],
        "editor.trim.apply" | "loop.set" => &["media_id", "start_ms", "end_ms"],
        "markers.add" => &["media_id", "position_ms", "label"],
        "labels.accept" => &["media_id", "class"],
        _ => &[],
    }
}

fn object_schema(required: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("query".into(), json!({"type": "string", "maxLength": 4096}));
    properties.insert(
        "control_id".into(),
        json!({"type": "string", "maxLength": 128}),
    );
    properties.insert("text".into(), json!({"type": "string", "maxLength": 4096}));
    properties.insert("checked".into(), json!({"type": "boolean"}));
    properties.insert(
        "limit".into(),
        json!({"type": "integer", "minimum": 1, "maximum": 200}),
    );
    properties.insert(
        "media_ids".into(),
        json!({
            "type": "array",
            "items": {"type": "integer", "minimum": 1},
            "maxItems": 200
        }),
    );
    properties.insert("media_id".into(), json!({"type": "integer", "minimum": 1}));
    properties.insert(
        "start_ms".into(),
        json!({"type": "integer", "minimum": 0, "maximum": 86400000}),
    );
    properties.insert(
        "end_ms".into(),
        json!({"type": "integer", "minimum": 1, "maximum": 86400000}),
    );
    properties.insert(
        "position_ms".into(),
        json!({"type": "integer", "minimum": 0, "maximum": 86400000}),
    );
    properties.insert(
        "gain_db".into(),
        json!({"type": "number", "minimum": -24.0, "maximum": 24.0}),
    );
    properties.insert("label".into(), json!({"type": "string", "maxLength": 128}));
    properties.insert(
        "class".into(),
        json!({"type": "string", "enum": ["se", "voice", "music", "other"]}),
    );
    properties.insert(
        "workspace".into(),
        json!({
            "type": "string",
            "enum": ["list", "editor", "effect_graph", "recording"]
        }),
    );
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_never_exposes_forbidden_capabilities() {
        let registry = ActionRegistry::default();
        for forbidden in [
            "shell.exec",
            "filesystem.write",
            "credentials.read",
            "app.force_quit",
        ] {
            assert!(registry.get(forbidden).is_none());
        }
    }

    #[test]
    fn read_only_declarations_exclude_state_changes() {
        let registry = ActionRegistry::default();
        let names: Vec<_> = registry
            .function_declarations(RiskClass::R0)
            .into_iter()
            .map(|declaration| declaration.name)
            .collect();
        assert!(names.contains(&"context.get".to_string()));
        assert!(!names.contains(&"list.select".to_string()));
        assert!(!names.contains(&"labels.accept".to_string()));
    }

    #[test]
    fn editor_mutations_are_declared_with_risk_and_undo() {
        let registry = ActionRegistry::default();
        for name in [
            "editor.gain.apply",
            "editor.trim.apply",
            "loop.set",
            "markers.add",
        ] {
            let descriptor = registry.get(name).expect("registered editor action");
            assert!(descriptor.risk.requires_approval());
            assert!(descriptor.changes_state);
            assert!(descriptor.supports_undo);
        }
    }

    #[test]
    fn ui_controls_are_allowlisted_in_function_schemas() {
        let registry = ActionRegistry::default();
        let declared = registry
            .function_declarations(RiskClass::R3)
            .into_iter()
            .map(|declaration| declaration.name)
            .collect::<Vec<_>>();
        assert!(declared.contains(&"ui.controls.list".to_string()));
        assert!(declared.contains(&"ui.text.set".to_string()));
        assert!(declared.contains(&"ui.checkbox.set".to_string()));
        assert!(declared.contains(&"ui.button.press".to_string()));
        for (action, expected_control) in [
            ("ui.text.set", "list.search.query"),
            ("ui.checkbox.set", "list.search.regex"),
            ("ui.button.press", "ai.index_visible_embeddings"),
        ] {
            let values = registry
                .get(action)
                .unwrap()
                .arguments_schema
                .pointer("/properties/control_id/enum")
                .and_then(Value::as_array)
                .expect("control enum");
            assert!(values.contains(&json!(expected_control)));
        }
    }
}
