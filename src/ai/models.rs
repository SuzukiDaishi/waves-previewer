use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "gemini", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AudioKind {
    Se,
    Voice,
    Music,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "gemini", derive(schemars::JsonSchema))]
pub struct AudioContains {
    #[serde(default)]
    pub speech: bool,
    #[serde(default)]
    pub music: bool,
    #[serde(default)]
    pub sound_effects: bool,
    #[serde(default)]
    pub ambience: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "gemini", derive(schemars::JsonSchema))]
pub struct AudioClassification {
    pub primary_class: AudioKind,
    pub confidence: f32,
    #[serde(default)]
    pub contains: AudioContains,
    #[serde(default)]
    pub description_ja: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl AudioClassification {
    pub fn validate(&self) -> Result<(), String> {
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err("classification confidence must be in 0.0..=1.0".to_string());
        }
        if self.description_ja.chars().count() > 4_096
            || self.warnings.len() > 32
            || self
                .warnings
                .iter()
                .any(|warning| warning.chars().count() > 512)
        {
            return Err("classification text exceeds bounded output limits".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "gemini", derive(schemars::JsonSchema))]
pub struct UcsEntry {
    pub category_id: String,
    pub subcategory_id: String,
    pub description: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UcsCatalogDocument {
    Entries(Vec<UcsEntry>),
    Object { entries: Vec<UcsEntry> },
}

pub fn load_ucs_catalog(path: &Path) -> Result<Vec<UcsEntry>, String> {
    const MAX_CATALOG_BYTES: u64 = 2 * 1024 * 1024;
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("could not inspect UCS catalog: {error}"))?;
    if metadata.len() > MAX_CATALOG_BYTES {
        return Err("UCS catalog exceeds the 2 MiB limit".into());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read UCS catalog: {error}"))?;
    parse_ucs_catalog(&text)
}

pub fn parse_ucs_catalog(text: &str) -> Result<Vec<UcsEntry>, String> {
    let document = serde_json::from_str::<UcsCatalogDocument>(text)
        .map_err(|error| format!("invalid UCS catalog JSON: {error}"))?;
    let mut entries = match document {
        UcsCatalogDocument::Entries(entries) | UcsCatalogDocument::Object { entries } => entries,
    };
    if entries.is_empty() || entries.len() > 2_000 {
        return Err("UCS catalog must contain 1..=2000 entries".into());
    }
    let mut seen = BTreeSet::new();
    for entry in &mut entries {
        entry.category_id = entry.category_id.trim().to_string();
        entry.subcategory_id = entry.subcategory_id.trim().to_string();
        entry.description = entry.description.trim().to_string();
        for (field, value, max_chars) in [
            ("category_id", entry.category_id.as_str(), 128),
            ("subcategory_id", entry.subcategory_id.as_str(), 128),
            ("description", entry.description.as_str(), 512),
        ] {
            if value.is_empty()
                || value.chars().count() > max_chars
                || value.chars().any(char::is_control)
            {
                return Err(format!(
                    "UCS {field} must contain 1..={max_chars} non-control characters"
                ));
            }
        }
        if !seen.insert((entry.category_id.clone(), entry.subcategory_id.clone())) {
            return Err(format!(
                "duplicate UCS entry: {}/{}",
                entry.category_id, entry.subcategory_id
            ));
        }
    }
    Ok(entries)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "gemini", derive(schemars::JsonSchema))]
pub struct UcsCandidate {
    pub category_id: String,
    pub subcategory_id: String,
    pub confidence: f32,
    #[serde(default)]
    pub descriptors: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "gemini", derive(schemars::JsonSchema))]
pub struct UcsAnalysis {
    #[serde(default)]
    pub candidates: Vec<UcsCandidate>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl UcsAnalysis {
    pub fn validate_against(&self, allowed: &[UcsEntry]) -> Result<(), String> {
        if self.candidates.is_empty() || self.candidates.len() > 5 || self.warnings.len() > 32 {
            return Err("UCS output must contain 1..=5 candidates and at most 32 warnings".into());
        }
        let mut seen = BTreeSet::new();
        for candidate in &self.candidates {
            if !candidate.confidence.is_finite() || !(0.0..=1.0).contains(&candidate.confidence) {
                return Err("UCS confidence must be in 0.0..=1.0".to_string());
            }
            if !allowed.iter().any(|entry| {
                entry.category_id == candidate.category_id
                    && entry.subcategory_id == candidate.subcategory_id
            }) {
                return Err(format!(
                    "unknown UCS candidate: {}/{}",
                    candidate.category_id, candidate.subcategory_id
                ));
            }
            if candidate.descriptors.len() > 32
                || candidate.descriptors.iter().any(|descriptor| {
                    descriptor.is_empty()
                        || descriptor.chars().count() > 128
                        || descriptor.chars().any(char::is_control)
                })
            {
                return Err("UCS descriptors exceed bounded output limits".into());
            }
            if !seen.insert((&candidate.category_id, &candidate.subcategory_id)) {
                return Err("UCS output contains a duplicate candidate".into());
            }
        }
        if self
            .warnings
            .iter()
            .any(|warning| warning.chars().count() > 512)
        {
            return Err("UCS warnings exceed bounded output limits".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "gemini", derive(schemars::JsonSchema))]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    #[serde(default)]
    pub speaker: Option<String>,
    #[serde(default)]
    pub emotion: Option<String>,
    pub confidence: f32,
    #[serde(default)]
    pub uncertain_words: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "gemini", derive(schemars::JsonSchema))]
pub struct VoiceTranscript {
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub full_text: String,
    #[serde(default)]
    pub segments: Vec<TranscriptSegment>,
}

impl VoiceTranscript {
    pub fn validate(&self) -> Result<(), String> {
        if self.language.chars().count() > 64
            || self.full_text.len() > 1024 * 1024
            || self.segments.len() > 10_000
        {
            return Err("transcript exceeds bounded output limits".into());
        }
        let mut previous_end = 0;
        for segment in &self.segments {
            if segment.start_ms > segment.end_ms {
                return Err("transcript segment start is after end".to_string());
            }
            if !segment.confidence.is_finite() || !(0.0..=1.0).contains(&segment.confidence) {
                return Err("transcript confidence must be in 0.0..=1.0".to_string());
            }
            if segment.start_ms < previous_end {
                return Err("transcript segments overlap or are out of order".to_string());
            }
            if segment.text.chars().count() > 8_192
                || segment.speaker.as_ref().is_some_and(|value| {
                    value.chars().count() > 128 || value.chars().any(char::is_control)
                })
                || segment.emotion.as_ref().is_some_and(|value| {
                    value.chars().count() > 128 || value.chars().any(char::is_control)
                })
                || segment.uncertain_words.len() > 256
                || segment
                    .uncertain_words
                    .iter()
                    .any(|word| word.chars().count() > 128)
            {
                return Err("transcript segment exceeds bounded output limits".into());
            }
            previous_end = segment.end_ms;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioEmbedding {
    pub model: String,
    pub dimensions: usize,
    pub values: Vec<f32>,
    #[serde(default)]
    pub segment_start_ms: u64,
    #[serde(default)]
    pub segment_end_ms: u64,
}

impl AudioEmbedding {
    pub fn validate(&self) -> Result<(), String> {
        if self.dimensions == 0 || self.dimensions != self.values.len() {
            return Err("embedding dimensions do not match vector length".to_string());
        }
        if self.values.iter().any(|value| !value.is_finite()) {
            return Err("embedding contains a non-finite value".to_string());
        }
        if self.segment_start_ms > self.segment_end_ms {
            return Err("embedding segment start is after end".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    #[default]
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionDeclaration {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionResult {
    pub call_id: String,
    pub name: String,
    pub result: Value,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InteractionInput {
    Text(String),
    Content(Vec<Value>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InteractionRequest {
    pub input: InteractionInput,
    #[serde(default)]
    pub previous_interaction_id: Option<String>,
    #[serde(default)]
    pub system_instruction: Option<String>,
    #[serde(default)]
    pub tools: Vec<FunctionDeclaration>,
    #[serde(default = "default_true")]
    pub store: bool,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InteractionResponse {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub output_text: String,
    #[serde(default)]
    pub function_calls: Vec<FunctionCall>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AiAnalysisResult {
    Classification(AudioClassification),
    Ucs(UcsAnalysis),
    Voice(VoiceTranscript),
    Lyrics(VoiceTranscript),
    Embedding(AudioEmbedding),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_rejects_invalid_confidence() {
        let value = AudioClassification {
            primary_class: AudioKind::Se,
            confidence: 1.1,
            contains: AudioContains::default(),
            description_ja: String::new(),
            warnings: Vec::new(),
        };
        assert!(value.validate().is_err());
    }

    #[test]
    fn ucs_candidates_must_come_from_the_allowed_set() {
        let analysis = UcsAnalysis {
            candidates: vec![UcsCandidate {
                category_id: "DOOR".into(),
                subcategory_id: "METAL".into(),
                confidence: 0.9,
                descriptors: Vec::new(),
            }],
            warnings: Vec::new(),
        };
        assert!(analysis
            .validate_against(&[UcsEntry {
                category_id: "DOOR".into(),
                subcategory_id: "METAL".into(),
                description: String::new(),
            }])
            .is_ok());
        assert!(analysis.validate_against(&[]).is_err());
    }

    #[test]
    fn ucs_catalog_accepts_array_or_object_and_rejects_duplicates() {
        let array = parse_ucs_catalog(
            r#"[{"category_id":"IMPACTS","subcategory_id":"METAL","description":"Metal impacts"}]"#,
        )
        .unwrap();
        assert_eq!(array[0].subcategory_id, "METAL");
        let object = parse_ucs_catalog(
            r#"{"entries":[{"category_id":"DOORS","subcategory_id":"WOOD","description":"Wood doors"}]}"#,
        )
        .unwrap();
        assert_eq!(object.len(), 1);
        assert!(parse_ucs_catalog(
            r#"[
                {"category_id":"A","subcategory_id":"B","description":"one"},
                {"category_id":"A","subcategory_id":"B","description":"two"}
            ]"#
        )
        .is_err());
    }
}
