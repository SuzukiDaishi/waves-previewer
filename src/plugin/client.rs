use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::plugin::protocol::{WorkerRequest, WorkerResponse};

static WORKER_TIMEOUT_COUNT: AtomicU64 = AtomicU64::new(0);

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
    }
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
    let Some(worker_path) = worker_exe_path() else {
        return Err("worker path resolve failed".to_string());
    };
    if !worker_path.is_file() {
        return Err(format!("worker not found: {}", worker_path.display()));
    }
    let timeout = request_timeout(request);
    let payload = serde_json::to_vec(request).map_err(|e| format!("encode request failed: {e}"))?;
    let mut child = Command::new(&worker_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn worker failed: {e}"))?;
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
                    return Ok(resp);
                }
                if !status.success() {
                    let err = shorten_err(&stderr);
                    return Err(if err.is_empty() {
                        format!("worker exited with status {status}")
                    } else {
                        err
                    });
                }
                return serde_json::from_slice(&stdout)
                    .map_err(|e| format!("decode worker output failed: {e}"));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    WORKER_TIMEOUT_COUNT.fetch_add(1, Ordering::Relaxed);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "worker timeout after {} ms",
                        timeout.as_millis()
                    ));
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            Err(e) => return Err(format!("wait worker failed: {e}")),
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
