use anyhow::Result;

use crate::mcp::state::{validate_read_path, McpState};
use crate::mcp::types::{OpenFilesArgs, OpenFolderArgs};

pub fn tool_open_folder(state: &McpState, args: OpenFolderArgs) -> Result<()> {
    let path = std::path::Path::new(&args.path);
    validate_read_path(state, path)?;
    Ok(())
}

pub fn tool_open_files(state: &McpState, args: OpenFilesArgs) -> Result<()> {
    for path in &args.paths {
        let path = std::path::Path::new(path);
        validate_read_path(state, path)?;
    }
    Ok(())
}
