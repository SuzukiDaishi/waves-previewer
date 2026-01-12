use anyhow::Result;

use crate::mcp::state::McpState;
use crate::mcp::types::{ModeArgs, PitchArgs, SpeedArgs, StretchArgs, VolumeArgs};

pub fn tool_play(_state: &McpState) -> Result<()> {
    Ok(())
}

pub fn tool_stop(_state: &McpState) -> Result<()> {
    Ok(())
}

pub fn tool_set_volume(_state: &McpState, _args: VolumeArgs) -> Result<()> {
    Ok(())
}

pub fn tool_set_mode(_state: &McpState, _args: ModeArgs) -> Result<()> {
    Ok(())
}

pub fn tool_set_speed(_state: &McpState, _args: SpeedArgs) -> Result<()> {
    Ok(())
}

pub fn tool_set_pitch(_state: &McpState, _args: PitchArgs) -> Result<()> {
    Ok(())
}

pub fn tool_set_stretch(_state: &McpState, _args: StretchArgs) -> Result<()> {
    Ok(())
}
