use anyhow::Result;

use crate::mcp::state::McpState;
use crate::mcp::types::{DebugSummary, ScreenshotArgs, ScreenshotResult};

pub fn tool_screenshot(_state: &McpState, _args: ScreenshotArgs) -> Result<ScreenshotResult> {
    Ok(ScreenshotResult {
        path: String::new(),
    })
}

pub fn tool_get_debug_summary(_state: &McpState) -> Result<DebugSummary> {
    Ok(DebugSummary {
        selected_paths: Vec::new(),
        active_tab_path: None,
        mode: None,
        playing: false,
    })
}
