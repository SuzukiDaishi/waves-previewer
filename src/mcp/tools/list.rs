use anyhow::Result;

use crate::mcp::state::McpState;
use crate::mcp::types::{ListFilesArgs, ListFilesResult, SelectionArgs, SelectionResult};

pub fn tool_list_files(_state: &McpState, _args: ListFilesArgs) -> Result<ListFilesResult> {
    Ok(ListFilesResult {
        total: 0,
        items: Vec::new(),
    })
}

pub fn tool_get_selection(_state: &McpState) -> Result<SelectionResult> {
    Ok(SelectionResult {
        selected_paths: Vec::new(),
        active_tab_path: None,
    })
}

pub fn tool_set_selection(_state: &McpState, _args: SelectionArgs) -> Result<SelectionResult> {
    Ok(SelectionResult {
        selected_paths: Vec::new(),
        active_tab_path: None,
    })
}
