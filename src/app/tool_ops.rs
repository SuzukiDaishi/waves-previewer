use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::tooling::{ToolDef, ToolJob, ToolLogEntry, ToolRunResult};
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn tools_config_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        let mut path = PathBuf::from(base);
        path.push("NeoWaves");
        let _ = std::fs::create_dir_all(&path);
        path.push("tools.toml");
        Some(path)
    }

    pub(super) fn tools_log_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        let mut path = PathBuf::from(base);
        path.push("NeoWaves");
        let _ = std::fs::create_dir_all(&path);
        path.push("tool_log.txt");
        Some(path)
    }

    pub(super) fn write_sample_tools_config(&self) -> std::result::Result<PathBuf, String> {
        let path = Self::tools_config_path()
            .ok_or_else(|| "Could not resolve tools.toml path.".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let sample = r#"# NeoWaves tools config
# Use {path}, {dir}, {stem}, {ext}, {outdir}, {basename}, {cwd}, {args}

[[tool]]
name = "Download Whisper Models"
group = "Setup"
description = "Download whisper.cpp models into the HF cache"
command = "powershell -ExecutionPolicy Bypass -File \"{cwd}\\commands\\download_whisper.ps1\" {args}"
per_file = false
args = ""

[[tool]]
name = "Generate SRT (Root)"
group = "Transcription"
description = "Generate .srt files under a root folder"
command = "powershell -ExecutionPolicy Bypass -File \"{cwd}\\commands\\generate_srt.ps1\" -Root \"{cwd}\" {args}"
per_file = false
args = ""

[[tool]]
name = "Generate SRT (Selection Folder)"
group = "Transcription"
description = "Generate .srt files under the selected file's folder"
command = "powershell -ExecutionPolicy Bypass -File \"{cwd}\\commands\\generate_srt.ps1\" -Root \"{dir}\" {args}"
per_file = false
args = ""
"#;
        std::fs::write(&path, sample).map_err(|e| e.to_string())?;
        Ok(path)
    }

    pub(super) fn load_tools_config(&mut self) {
        let Some(path) = Self::tools_config_path() else {
            self.tool_defs.clear();
            return;
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => {
                self.tool_defs.clear();
                return;
            }
        };
        let parsed: Result<super::tooling::ToolsConfig, _> = toml::from_str(&text);
        match parsed {
            Ok(cfg) => {
                self.tool_defs = cfg.tool.unwrap_or_default();
            }
            Err(err) => {
                self.tool_defs.clear();
                self.debug_log(format!("tools.toml parse error: {err}"));
            }
        }
    }

    pub(super) fn expand_tool_command(template: &str, path: Option<&Path>, args: &str) -> String {
        let empty = std::borrow::Cow::from("");
        let (path_s, dir, stem, ext, basename) = if let Some(path) = path {
            let dir = path
                .parent()
                .map(|p| p.to_string_lossy())
                .unwrap_or_default();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            let basename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            (
                path.to_string_lossy(),
                dir,
                std::borrow::Cow::from(stem),
                std::borrow::Cow::from(ext),
                std::borrow::Cow::from(basename),
            )
        } else {
            (
                empty.clone(),
                empty.clone(),
                empty.clone(),
                empty.clone(),
                empty.clone(),
            )
        };
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        template
            .replace("{path}", &path_s)
            .replace("{dir}", dir.as_ref())
            .replace("{stem}", stem.as_ref())
            .replace("{ext}", ext.as_ref())
            .replace("{outdir}", dir.as_ref())
            .replace("{basename}", basename.as_ref())
            .replace("{cwd}", &cwd)
            .replace("{args}", args)
    }

    fn is_dangerous_command(cmd: &str) -> bool {
        let s = cmd.to_ascii_lowercase();
        let tokens = [
            " rm ", " del ", " erase ", " rmdir ", " rd ", " mv ", " move ",
        ];
        if s.contains('>') {
            return true;
        }
        tokens.iter().any(|t| s.contains(t))
    }

    pub(super) fn enqueue_tool_runs(&mut self, tool: &ToolDef, paths: &[PathBuf], args: &str) {
        let per_file = tool.per_file.unwrap_or(true);
        if per_file {
            for path in paths {
                let command = Self::expand_tool_command(&tool.command, Some(path), args);
                let job = ToolJob {
                    tool: tool.clone(),
                    path: Some(path.clone()),
                    command,
                };
                self.tool_queue.push_back(job);
            }
        } else {
            let path = paths.get(0).map(|p| p.as_path());
            let command = Self::expand_tool_command(&tool.command, path, args);
            let job = ToolJob {
                tool: tool.clone(),
                path: paths.get(0).cloned(),
                command,
            };
            self.tool_queue.push_back(job);
        }
    }

    pub(super) fn start_tool_job(&mut self, job: ToolJob) {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<ToolRunResult>();
        self.tool_run_rx = Some(rx);
        self.tool_worker_busy = true;
        std::thread::spawn(move || {
            let result = super::tooling::run_tool_command(job);
            let _ = tx.send(result);
        });
    }

    fn append_tool_log(&mut self, entry: ToolLogEntry) {
        let log_entry = entry.clone();
        self.tool_log.push_front(entry);
        while self.tool_log.len() > self.tool_log_max {
            self.tool_log.pop_back();
        }
        if let Some(path) = Self::tools_log_path() {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| {
                    let status = if log_entry.ok { "OK" } else { "FAIL" };
                    writeln!(
                        f,
                        "[{:?}] {} {} {}\n{}",
                        log_entry.timestamp,
                        status,
                        log_entry.tool_name,
                        log_entry
                            .path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "(none)".to_string()),
                        log_entry.command
                    )
                });
        }
    }

    pub(super) fn process_tool_queue(&mut self) {
        if self.tool_worker_busy || self.pending_tool_confirm.is_some() {
            return;
        }
        let Some(job) = self.tool_queue.pop_front() else {
            return;
        };
        let confirm = job.tool.confirm.unwrap_or(false) || Self::is_dangerous_command(&job.command);
        if confirm {
            self.pending_tool_confirm = Some(job);
        } else {
            self.start_tool_job(job);
        }
    }

    pub(super) fn process_tool_results(&mut self) {
        let Some(rx) = &self.tool_run_rx else {
            return;
        };
        if let Ok(result) = rx.try_recv() {
            self.tool_run_rx = None;
            self.tool_worker_busy = false;
            let entry = ToolLogEntry {
                timestamp: std::time::SystemTime::now(),
                tool_name: result.job.tool.name.clone(),
                path: result.job.path.clone(),
                command: result.job.command.clone(),
                ok: result.ok,
                status_code: result.status_code,
                stdout: result.stdout,
                stderr: result.stderr,
                duration: result.duration,
            };
            self.append_tool_log(entry);
        }
    }
}
