use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

#[derive(Debug, Clone)]
pub struct McpState {
    pub allow_paths: Vec<PathBuf>,
    pub allow_write: bool,
    pub allow_export: bool,
    pub read_only: bool,
    pub last_screenshot: Option<PathBuf>,
}

impl McpState {
    pub fn new() -> Self {
        Self {
            allow_paths: Vec::new(),
            allow_write: false,
            allow_export: false,
            read_only: true,
            last_screenshot: None,
        }
    }
}

pub fn validate_read_path(state: &McpState, path: &Path) -> Result<()> {
    if state.allow_paths.is_empty() {
        return Ok(());
    }
    if is_allowed_path(&state.allow_paths, path) {
        Ok(())
    } else {
        Err(anyhow!("PERMISSION_DENIED: path not in allowlist"))
    }
}

pub fn validate_write_path(state: &McpState, path: &Path) -> Result<()> {
    if state.read_only || !state.allow_write {
        return Err(anyhow!("PERMISSION_DENIED: write disabled"));
    }
    validate_read_path(state, path)
}

fn is_allowed_path(allow_paths: &[PathBuf], path: &Path) -> bool {
    allow_paths.iter().any(|root| path.starts_with(root))
}
