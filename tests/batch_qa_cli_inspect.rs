//! End-to-end test for `--cli batch inspect` driving the real binary.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn make_temp_dir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "neowaves_batch_qa_cli_{tag}_{}_{}_{}",
        std::process::id(),
        now_ms,
        seq
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn tone(sr: u32, secs: f32, amp: f32) -> Vec<f32> {
    let frames = ((sr as f32) * secs).max(1.0) as usize;
    (0..frames)
        .map(|i| ((i as f32 / sr as f32) * 440.0 * std::f32::consts::TAU).sin() * amp)
        .collect()
}

fn run_cli(args: &[&str]) -> serde_json::Value {
    let exe = env!("CARGO_BIN_EXE_neowaves");
    let out = Command::new(exe)
        .arg("--cli")
        .args(args)
        .output()
        .expect("run neowaves --cli");
    assert!(
        out.status.success(),
        "cli failed: {:?}\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cli stdout is JSON")
}

#[test]
fn cli_batch_inspect_reports_expected_severities() {
    let dir = make_temp_dir("run");
    let sr = 48_000u32;
    neowaves::wave::export_channels_audio(&[tone(sr, 0.4, 0.99)], sr, &dir.join("hot.wav"))
        .expect("hot");
    let bad_loop = dir.join("bad_loop.wav");
    neowaves::wave::export_channels_audio(&[tone(sr, 0.4, 0.4)], sr, &bad_loop).expect("bad");
    neowaves::loop_markers::write_loop_markers(&bad_loop, Some((0, 10_000_000)))
        .expect("bad loop markers");
    let clean = dir.join("clean.wav");
    neowaves::wave::export_channels_audio(&[tone(sr, 0.4, 0.4)], sr, &clean).expect("clean");

    let session = dir.join("qa.nwsess");
    run_cli(&[
        "session",
        "new",
        "--folder",
        dir.to_str().unwrap(),
        "--output",
        session.to_str().unwrap(),
    ]);

    let report_json = dir.join("report.json");
    let report_csv = dir.join("report.csv");
    // Loose loudness window so only structural findings fire.
    let out = run_cli(&[
        "batch",
        "inspect",
        "--session",
        session.to_str().unwrap(),
        "--lufs-tolerance",
        "24",
        "--report",
        report_json.to_str().unwrap(),
    ]);
    let result = &out["result"];
    assert_eq!(result["counts"]["error"], 1, "{result}");
    let rows = result["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 3);
    // Errors sort first.
    assert!(rows[0]["path"].as_str().unwrap().ends_with("bad_loop.wav"));
    assert_eq!(rows[0]["severity"], "Error");
    let hot = rows
        .iter()
        .find(|r| r["path"].as_str().unwrap().ends_with("hot.wav"))
        .expect("hot row");
    assert_eq!(hot["severity"], "Warning");
    let clean_row = rows
        .iter()
        .find(|r| r["path"].as_str().unwrap().ends_with("clean.wav"))
        .expect("clean row");
    assert!(clean_row["severity"].is_null(), "{clean_row}");

    // JSON report parses; CSV report has header + 3 rows.
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report_json).unwrap()).unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 3);
    run_cli(&[
        "batch",
        "inspect",
        "--session",
        session.to_str().unwrap(),
        "--lufs-tolerance",
        "24",
        "--report",
        report_csv.to_str().unwrap(),
    ]);
    let csv = std::fs::read_to_string(&report_csv).unwrap();
    assert!(csv.lines().next().unwrap().starts_with("severity,file"));
    assert_eq!(csv.lines().count(), 4);

    // Loop-only mode still finds the bad loop (no decode-based checks).
    let out = run_cli(&[
        "batch",
        "inspect",
        "--session",
        session.to_str().unwrap(),
        "--no-loudness",
        "--no-true-peak",
        "--no-silence",
    ]);
    assert_eq!(out["result"]["counts"]["error"], 1);
    assert_eq!(out["result"]["counts"]["warning"], 0);

    // Naming rule: every fixture stem violates a strict prefix pattern.
    let out = run_cli(&[
        "batch",
        "inspect",
        "--session",
        session.to_str().unwrap(),
        "--no-loudness",
        "--no-true-peak",
        "--no-silence",
        "--no-loop",
        "--naming-pattern",
        "^(se|bgm)_[a-z0-9_]+$",
    ]);
    assert_eq!(out["result"]["counts"]["warning"], 3, "{}", out["result"]);
    // A pattern the stems match produces no findings.
    let out = run_cli(&[
        "batch",
        "inspect",
        "--session",
        session.to_str().unwrap(),
        "--no-loudness",
        "--no-true-peak",
        "--no-silence",
        "--no-loop",
        "--naming-pattern",
        "^[a-z_]+$",
    ]);
    assert_eq!(out["result"]["counts"]["warning"], 0, "{}", out["result"]);
    // An invalid pattern is a config error on every row.
    let out = run_cli(&[
        "batch",
        "inspect",
        "--session",
        session.to_str().unwrap(),
        "--no-loudness",
        "--no-true-peak",
        "--no-silence",
        "--no-loop",
        "--naming-pattern",
        "([unclosed",
    ]);
    assert_eq!(out["result"]["counts"]["error"], 3, "{}", out["result"]);

    let _ = std::fs::remove_dir_all(&dir);
}
