use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use neowaves::plugin::{WorkerRequest, WorkerResponse};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn unique_temp_dir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("neowaves_{tag}_{stamp}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_sleep_worker_script(path: &Path) {
    #[cfg(windows)]
    {
        let script = "@echo off\r\nping -n 6 127.0.0.1 >nul\r\n";
        std::fs::write(path, script).expect("write worker script");
    }
    #[cfg(not(windows))]
    {
        let script = "#!/bin/sh\nsleep 5\n";
        std::fs::write(path, script).expect("write worker script");
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod +x");
    }
}

fn restore_env_var(key: &str, value: Option<String>) {
    match value {
        Some(v) => unsafe { std::env::set_var(key, v) },
        None => unsafe { std::env::remove_var(key) },
    }
}

#[test]
fn worker_timeout_falls_back_and_records_counter() {
    let _env_guard = ENV_LOCK.lock().expect("env lock");
    let dir = unique_temp_dir("plugin_timeout");
    let script_path = if cfg!(windows) {
        dir.join("slow_worker.cmd")
    } else {
        dir.join("slow_worker.sh")
    };
    write_sleep_worker_script(&script_path);

    let prev_worker_path = std::env::var("NEOWAVES_PLUGIN_WORKER_PATH").ok();
    let prev_timeout = std::env::var("NEOWAVES_PLUGIN_WORKER_TIMEOUT_MS").ok();
    unsafe {
        std::env::set_var(
            "NEOWAVES_PLUGIN_WORKER_PATH",
            script_path.to_string_lossy().to_string(),
        );
        std::env::set_var("NEOWAVES_PLUGIN_WORKER_TIMEOUT_MS", "60");
    }

    let before = neowaves::plugin::client::worker_timeout_count();
    let started = Instant::now();
    let resp = neowaves::plugin::client::run_request(&WorkerRequest::Ping).expect("run request");
    let elapsed = started.elapsed();
    let after = neowaves::plugin::client::worker_timeout_count();

    restore_env_var("NEOWAVES_PLUGIN_WORKER_PATH", prev_worker_path);
    restore_env_var("NEOWAVES_PLUGIN_WORKER_TIMEOUT_MS", prev_timeout);

    assert!(matches!(resp, WorkerResponse::Pong));
    assert!(after > before, "timeout counter should increment");
    assert!(
        elapsed < Duration::from_secs(2),
        "timeout fallback should return quickly, elapsed={elapsed:?}"
    );

    let _ = std::fs::remove_dir_all(dir);
}
