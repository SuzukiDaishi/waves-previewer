use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::plugin::protocol::{WorkerRequest, WorkerResponse};

static WORKER_TIMEOUT_COUNT: AtomicU64 = AtomicU64::new(0);
static WORKER_SPAWN_SEQ: AtomicU64 = AtomicU64::new(0);

fn apply_no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

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
        WorkerRequest::Heartbeat { .. } => Duration::from_millis(5_000),
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
            let temp_path =
                std::env::temp_dir().join(format!("neowaves_plugin_worker_{pid}_{seq}.exe"));
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
        let mut cmd = Command::new(&worker_path);
        apply_no_window(&mut cmd);
        match cmd
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
    // stdout/stderr は子プロセスの実行中から別スレッドで吸い出す。
    // 応答 (state blob 込みで数百 KB を超え得る) がパイプバッファを超えると、
    // 終了後にまとめて読む方式では子の write がブロックして永久に exit せず、
    // タイムアウト扱いで kill されてしまう。
    let stdout_handle = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            buf
        })
    });
    // 書き込み失敗 (broken pipe 等) は子が先に異常終了したケースなので、
    // ここでは即エラーにせず exit status / stderr からエラーを組み立てる。
    let mut stdin_error: Option<String> = None;
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(&payload) {
            stdin_error = Some(format!("worker stdin write failed: {e}"));
        }
    }

    let deadline = Instant::now() + timeout;
    let wait_result: Result<std::process::ExitStatus, String> = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    WORKER_TIMEOUT_COUNT.fetch_add(1, Ordering::Relaxed);
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!("worker timeout after {} ms", timeout.as_millis()));
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("wait worker failed: {e}"));
            }
        }
    };
    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    if cleanup_temp {
        let _ = std::fs::remove_file(&worker_path);
    }
    let status = wait_result?;
    if let Ok(resp) = serde_json::from_slice::<WorkerResponse>(&stdout) {
        return Ok(resp);
    }
    if !status.success() {
        let err = shorten_err(&stderr);
        return Err(if !err.is_empty() {
            err
        } else if let Some(stdin_err) = stdin_error {
            stdin_err
        } else {
            format!("worker exited with status {status}")
        });
    }
    if let Some(stdin_err) = stdin_error {
        return Err(stdin_err);
    }
    serde_json::from_slice(&stdout).map_err(|e| format!("decode worker output failed: {e}"))
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
    rx: mpsc::Receiver<Result<WorkerResponse, String>>,
    _reader_thread: std::thread::JoinHandle<()>,
    next_heartbeat_id: u64,
}

impl GuiWorkerClient {
    pub fn spawn() -> Result<Self, String> {
        let Some(path) = gui_worker_exe_path() else {
            return Err("gui worker path resolve failed".to_string());
        };
        let (worker_path, cleanup_temp) = prepare_worker_executable_named(path)?;
        let mut cmd = Command::new(&worker_path);
        apply_no_window(&mut cmd);
        let mut child = cmd
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

        let (tx, rx) = mpsc::channel::<Result<WorkerResponse, String>>();
        let reader_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = tx.send(Err("gui worker closed stdout".to_string()));
                        break;
                    }
                    Ok(_) => {
                        let result = serde_json::from_str::<WorkerResponse>(line.trim_end())
                            .map_err(|e| format!("decode gui worker response failed: {e}"));
                        if tx.send(result).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("gui worker read failed: {e}")));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            rx,
            _reader_thread: reader_thread,
            next_heartbeat_id: 0,
        })
    }

    fn send_raw(&mut self, request: &WorkerRequest) -> Result<(), String> {
        let mut payload =
            serde_json::to_vec(request).map_err(|e| format!("encode gui request failed: {e}"))?;
        payload.push(b'\n');
        self.stdin
            .write_all(&payload)
            .map_err(|e| format!("gui worker write failed: {e}"))?;
        self.stdin
            .flush()
            .map_err(|e| format!("gui worker flush failed: {e}"))
    }

    pub fn request(&mut self, request: &WorkerRequest) -> Result<WorkerResponse, String> {
        let timeout = request_timeout(request);
        self.send_raw(request)?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                WORKER_TIMEOUT_COUNT.fetch_add(1, Ordering::Relaxed);
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err(format!(
                    "gui worker timeout after {} ms",
                    timeout.as_millis()
                ));
            }
            match self.rx.recv_timeout(remaining.min(Duration::from_millis(250))) {
                Ok(Ok(resp)) => return Ok(resp),
                Ok(Err(e)) => return Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // タイムアウト期限まで再確認
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("gui worker reader thread disconnected".to_string());
                }
            }
        }
    }

    /// GUI worker が生きているかを heartbeat で確認する。
    /// ハングしていれば timeout エラーを返す（呼び出し元が kill 判断する）。
    pub fn heartbeat(&mut self) -> Result<(), String> {
        let id = self.next_heartbeat_id;
        self.next_heartbeat_id = self.next_heartbeat_id.wrapping_add(1);
        let req = WorkerRequest::Heartbeat { request_id: id };
        match self.request(&req)? {
            WorkerResponse::HeartbeatAck { request_id } if request_id == id => Ok(()),
            other => Err(format!("unexpected heartbeat response: {other:?}")),
        }
    }

    pub fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
