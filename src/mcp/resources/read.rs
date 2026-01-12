use anyhow::Result;

use crate::mcp::state::McpState;
use crate::mcp::types::ResourceContent;

pub fn read_resource(_state: &McpState, _uri: &str) -> Result<ResourceContent> {
    Ok(ResourceContent {
        uri: String::new(),
        mime_type: "text/plain".to_string(),
        data: None,
        text: None,
    })
}
