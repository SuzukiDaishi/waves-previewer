use serde::Deserialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, Deserialize)]
pub struct ToolsConfig {
    pub tool: Option<Vec<ToolDef>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub command: String,
    pub description: Option<String>,
    pub confirm: Option<bool>,
    pub group: Option<String>,
    pub args: Option<String>,
    pub per_file: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct ToolJob {
    pub tool: ToolDef,
    pub path: Option<PathBuf>,
    pub command: String,
}

#[derive(Clone, Debug)]
pub struct ToolRunResult {
    pub job: ToolJob,
    pub ok: bool,
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
}

#[derive(Clone, Debug)]
pub struct ToolLogEntry {
    pub timestamp: SystemTime,
    pub tool_name: String,
    pub path: Option<PathBuf>,
    pub command: String,
    pub ok: bool,
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
}

pub fn run_tool_command(job: ToolJob) -> ToolRunResult {
    let start = SystemTime::now();
    let output = run_shell_command(&job.command);
    let duration = start.elapsed().unwrap_or(Duration::from_secs(0));
    let ok = output.status.success();
    ToolRunResult {
        job,
        ok,
        status_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        duration,
    }
}

#[cfg(target_os = "windows")]
fn run_shell_command(cmd: &str) -> std::process::Output {
    use std::os::windows::process::ExitStatusExt;
    std::process::Command::new("cmd")
        .args(["/C", cmd])
        .output()
        .unwrap_or_else(|_| std::process::Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
}

#[cfg(not(target_os = "windows"))]
fn run_shell_command(cmd: &str) -> std::process::Output {
    use std::os::unix::process::ExitStatusExt;
    std::process::Command::new("sh")
        .args(["-c", cmd])
        .output()
        .unwrap_or_else(|_| std::process::Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
}
