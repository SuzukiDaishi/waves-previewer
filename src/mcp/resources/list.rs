use anyhow::Result;

use crate::mcp::state::McpState;
use crate::mcp::types::ResourceDescriptor;

pub fn list_resources(_state: &McpState) -> Result<Vec<ResourceDescriptor>> {
    Ok(Vec::new())
}
