use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::plugin::protocol::{WorkerRequest, WorkerResponse};

static WORKER_TIMEOUT_COUNT: AtomicU64 = AtomicU64::new(0);
static WORKER_SPAWN_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn worker_timeout_count() -> u64 {
    WORKER_TIMEOUT_COUNT.load(Ordering::Relaxed)
}

fn worker_exe_name() -> &'static str {
    #[cfg(windows)]
    {
        "neowaves_plugin_worker.exe"
    }
    #[cfg(not(windows))]
    {
        "neowaves_plugin_worker"
    }
}

fn gui_worker_exe_name() -> &'static str {
    #[cfg(windows)]
    {
        "neowaves_plugin_gui_worker.exe"
    }
    #[cfg(not(windows))]
    {
        "neowaves_plugin_gui_worker"
    }
}

fn worker_exe_path() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("NEOWAVES_PLUGIN_WORKER_PATH") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(worker_exe_name()))
}

fn gui_worker_exe_path() -> Option<PathBuf> {
    if let Ok(override_path) = std::env::var("NEOWAVES_PLUGIN_GUI_WORKER_PATH") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(gui_worker_exe_name()))
}

fn request_timeout(request: &WorkerRequest) -> Duration {
    if let Ok(raw) = std::env::var("NEOWAVES_PLUGIN_WORKER_TIMEOUT_MS") {
        if let Ok(ms) = raw.trim().parse::<u64>() {
            return Duration::from_millis(ms.max(10));
        }
    }
    match request {
        WorkerRequest::Ping => Duration::from_millis(2_000),
        WorkerRequest::Scan { .. } => Duration::from_millis(30_000),
        WorkerRequest::Probe { .. } => Duration::from_millis(30_000),
        WorkerRequest::ProcessFx { .. } => Duration::from_millis(120_000),
        WorkerRequest::GuiSessionOpen { .. } => Duration::from_millis(30_000),
        WorkerRequest::GuiSessionPoll { .. } => Duration::from_millis(10_000),
        WorkerRequest::GuiSessionClose { .. } => Duration::from_millis(10_000),
    }
}

fn prepare_worker_executable_named(worker_path: PathBuf) -> Result<(PathBuf, bool), String> {
    if !worker_path.is_file() {
        return Err(format!("worker not found: {}", worker_path.display()));
    }
    #[cfg(windows)]
    {
        let is_exe = worker_path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);
        if is_exe {
            let seq = WORKER_SPAWN_SEQ.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let temp_path = std::env::temp_dir().join(format!(
                "neowaves_plugin_worker_{pid}_{seq}.exe"
            ));
            match std::fs::copy(&worker_path, &temp_path) {
                Ok(_) => return Ok((temp_path, true)),
                Err(_) => {
                    // Fallback to the original path if copy fails.
                }
            }
        }
    }
    Ok((worker_path, false))
}

fn prepare_worker_executable() -> Result<(PathBuf, bool), String> {
    let Some(worker_path) = worker_exe_path() else {
        return Err("worker path resolve failed".to_string());
    };
    prepare_worker_executable_named(worker_path)
}

fn shorten_err(raw: &[u8]) -> String {
    const LIMIT: usize = 1600;
    let text = String::from_utf8_lossy(raw).trim().to_string();
    if text.len() <= LIMIT {
        text
    } else {
        format!("{}...", &text[..LIMIT])
    }
}

fn run_worker_process(request: &WorkerRequest) -> Result<WorkerResponse, String> {
    let (worker_path, cleanup_temp) = prepare_worker_executable()?;
    let timeout = request_timeout(request);
    let payload = serde_json::to_vec(request).map_err(|e| format!("encode request failed: {e}"))?;
    let mut last_spawn_err: Option<std::io::Error> = None;
    let mut child_opt = None;
    for attempt in 0..5usize {
        match Command::new(&worker_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => {
                child_opt = Some(child);
                break;
            }
            Err(err) => {
                let sharing_violation = err.raw_os_error() == Some(32);
                last_spawn_err = Some(err);
                if !sharing_violation || attempt == 4 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        }
    }
    let mut child = if let Some(child) = child_opt {
        child
    } else {
        let err = last_spawn_err
            .map(|e| format!("spawn worker failed: {e}"))
            .unwrap_or_else(|| "spawn worker failed".to_string());
        if cleanup_temp {
            let _ = std::fs::remove_file(&worker_path);
        }
        return Err(err);
    };
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .map_err(|e| format!("worker stdin write failed: {e}"))?;
    }

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_end(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_end(&mut stderr);
                }
                if let Ok(resp) = serde_json::from_slice::<WorkerResponse>(&stdout) {
                    if cleanup_temp {
                        let _ = std::fs::remove_file(&worker_path);
                    }
                    return Ok(resp);
                }
                if !status.success() {
                    let err = shorten_err(&stderr);
                    if cleanup_temp {
                        let _ = std::fs::remove_file(&worker_path);
                    }
                    return Err(if err.is_empty() {
                        format!("worker exited with status {status}")
                    } else {
                        err
                    });
                }
                let decoded =
                    serde_json::from_slice(&stdout).map_err(|e| format!("decode worker output failed: {e}"));
                if cleanup_temp {
                    let _ = std::fs::remove_file(&worker_path);
                }
                return decoded;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    WORKER_TIMEOUT_COUNT.fetch_add(1, Ordering::Relaxed);
                    let _ = child.kill();
                    let _ = child.wait();
                    if cleanup_temp {
                        let _ = std::fs::remove_file(&worker_path);
                    }
                    return Err(format!(
                        "worker timeout after {} ms",
                        timeout.as_millis()
                    ));
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            Err(e) => {
                if cleanup_temp {
                    let _ = std::fs::remove_file(&worker_path);
                }
                return Err(format!("wait worker failed: {e}"));
            }
        }
    }
}

pub fn run_request(request: &WorkerRequest) -> Result<WorkerResponse, String> {
    match run_worker_process(request) {
        Ok(resp) => Ok(resp),
        Err(err) => match request {
            WorkerRequest::Ping => Ok(crate::plugin::worker::handle_request(request.clone())),
            _ => Err(err),
        },
    }
}

pub struct GuiWorkerClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl GuiWorkerClient {
    pub fn spawn() -> Result<Self, String> {
        let Some(path) = gui_worker_exe_path() else {
            return Err("gui worker path resolve failed".to_string());
        };
        let (worker_path, cleanup_temp) = prepare_worker_executable_named(path)?;
        let mut child = Command::new(&worker_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn gui worker failed: {e}"))?;
        if cleanup_temp {
            let _ = std::fs::remove_file(worker_path);
        }
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "gui worker stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "gui worker stdout unavailable".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    pub fn request(&mut self, request: &WorkerRequest) -> Result<WorkerResponse, String> {
        let mut payload =
            serde_json::to_vec(request).map_err(|e| format!("encode gui request failed: {e}"))?;
        payload.push(b'\n');
        self.stdin
            .write_all(&payload)
            .map_err(|e| format!("gui worker write failed: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("gui worker flush failed: {e}"))?;
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .map_err(|e| format!("gui worker read failed: {e}"))?;
        if read == 0 {
            return Err("gui worker closed stdout".to_string());
        }
        serde_json::from_str::<WorkerResponse>(line.trim_end())
            .map_err(|e| format!("decode gui worker response failed: {e}"))
    }

    pub fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
