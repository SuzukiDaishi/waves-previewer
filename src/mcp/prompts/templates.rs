use anyhow::{anyhow, Result};

use crate::mcp::types::{PromptDescriptor, PromptResult};

pub fn list_prompts() -> Result<Vec<PromptDescriptor>> {
    Ok(Vec::new())
}

pub fn get_prompt(name: &str, _args: serde_json::Value) -> Result<PromptResult> {
    Err(anyhow!("prompt not found: {name}"))
}
