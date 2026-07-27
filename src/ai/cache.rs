use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AiCacheKey {
    pub content_hash: String,
    pub model: String,
    pub operation: String,
    pub schema_version: u32,
    pub prompt_version: u32,
}

impl AiCacheKey {
    pub fn stable_id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.content_hash.as_bytes());
        hasher.update([0]);
        hasher.update(self.model.as_bytes());
        hasher.update([0]);
        hasher.update(self.operation.as_bytes());
        hasher.update(self.schema_version.to_le_bytes());
        hasher.update(self.prompt_version.to_le_bytes());
        format!("{:x}", hasher.finalize())
    }
}

pub fn calculate_content_hash(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn canonical_json_hash(value: &serde_json::Value) -> String {
    fn canonicalize(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort_unstable();
                let mut ordered = serde_json::Map::with_capacity(map.len());
                for key in keys {
                    ordered.insert(key.clone(), canonicalize(&map[key]));
                }
                serde_json::Value::Object(ordered)
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.iter().map(canonicalize).collect())
            }
            _ => value.clone(),
        }
    }

    let canonical = serde_json::to_vec(&canonicalize(value)).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_hash_ignores_object_key_order() {
        let a = serde_json::json!({"a": 1, "b": {"x": 2, "y": 3}});
        let b = serde_json::json!({"b": {"y": 3, "x": 2}, "a": 1});
        assert_eq!(canonical_json_hash(&a), canonical_json_hash(&b));
    }

    #[test]
    fn cache_key_changes_with_prompt_version() {
        let base = AiCacheKey {
            content_hash: "hash".into(),
            model: "model".into(),
            operation: "classify".into(),
            schema_version: 1,
            prompt_version: 1,
        };
        let mut changed = base.clone();
        changed.prompt_version = 2;
        assert_ne!(base.stable_id(), changed.stable_id());
    }
}
