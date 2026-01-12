use anyhow::Result;

use crate::mcp::state::McpState;
use crate::mcp::types::{ExportArgs, ExportResult};

pub fn tool_export_selected(_state: &McpState, _args: ExportArgs) -> Result<ExportResult> {
    Ok(ExportResult {
        ok: 0,
        failed: 0,
        success_paths: Vec::new(),
        failed_paths: Vec::new(),
    })
}
