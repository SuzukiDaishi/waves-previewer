use anyhow::Result;

use crate::mcp::state::McpState;
use crate::mcp::types::{GainArgs, GainClearArgs, LoopArgs, WriteLoopArgs};

pub fn tool_apply_gain(_state: &McpState, _args: GainArgs) -> Result<()> {
    Ok(())
}

pub fn tool_clear_gain(_state: &McpState, _args: GainClearArgs) -> Result<()> {
    Ok(())
}

pub fn tool_set_loop_markers(_state: &McpState, _args: LoopArgs) -> Result<()> {
    Ok(())
}

pub fn tool_write_loop_markers(_state: &McpState, _args: WriteLoopArgs) -> Result<()> {
    Ok(())
}
