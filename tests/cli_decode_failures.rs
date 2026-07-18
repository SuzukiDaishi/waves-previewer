//! End-to-end tests for CLI decode-failure reporting: undecodable files must
//! surface in the envelope `warnings`/`failed_paths` (and on stderr), and a
//! batch where every attempted file fails must exit nonzero.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn make_temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "neowaves_cli_decode_failures_{tag}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn tone(sr: u32, secs: f32) -> Vec<f32> {
    let frames = ((sr as f32) * secs).max(1.0) as usize;
    (0..frames)
        .map(|i| ((i as f32 / sr as f32) * 440.0 * std::f32::consts::TAU).sin() * 0.4)
        .collect()
}

fn write_junk_wav(path: &Path) {
    // A .wav extension with no RIFF structure: scanned into sessions by
    // extension, but every decode attempt fails.
    std::fs::write(path, b"this is not a wav file at all").expect("write junk wav");
}

fn run_cli_raw(args: &[&str]) -> Output {
    let exe = env!("CARGO_BIN_EXE_neowaves");
    Command::new(exe)
        .arg("--cli")
        .args(args)
        .output()
        .expect("run neowaves --cli")
}

fn run_cli_ok(args: &[&str]) -> (serde_json::Value, String) {
    let out = run_cli_raw(args);
    assert!(
        out.status.success(),
        "cli failed: {:?}\nstdout: {}\nstderr: {}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (
        serde_json::from_slice(&out.stdout).expect("cli stdout is JSON"),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn new_session(dir: &Path) -> PathBuf {
    let session = dir.join("decode.nwsess");
    run_cli_ok(&[
        "session",
        "new",
        "--folder",
        dir.to_str().unwrap(),
        "--output",
        session.to_str().unwrap(),
    ]);
    session
}

fn warnings_of(envelope: &serde_json::Value) -> Vec<String> {
    envelope["warnings"]
        .as_array()
        .expect("warnings array")
        .iter()
        .map(|w| w.as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn cli_list_query_warns_on_undecodable_file() {
    let dir = make_temp_dir("list");
    let sr = 48_000u32;
    neowaves::wave::export_channels_audio(&[tone(sr, 0.25)], sr, &dir.join("good.wav"))
        .expect("good wav");
    write_junk_wav(&dir.join("broken.wav"));

    let (out, stderr) = run_cli_ok(&["list", "query", "--folder", dir.to_str().unwrap()]);
    assert_eq!(out["ok"], true);
    assert_eq!(out["result"]["total"], 2, "both rows still listed");
    let warnings = warnings_of(&out);
    assert!(
        warnings.iter().any(|w| w.contains("broken.wav")),
        "warnings must name the undecodable file: {warnings:?}"
    );
    assert!(
        !warnings.iter().any(|w| w.contains("good.wav")),
        "readable files must not warn: {warnings:?}"
    );
    assert!(
        stderr.contains("warning:") && stderr.contains("broken.wav"),
        "warnings must be mirrored to stderr: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_batch_export_reports_partial_decode_failures_and_still_succeeds() {
    let dir = make_temp_dir("export_partial");
    let sr = 48_000u32;
    neowaves::wave::export_channels_audio(&[tone(sr, 0.25)], sr, &dir.join("good.wav"))
        .expect("good wav");
    write_junk_wav(&dir.join("broken.wav"));
    let session = new_session(&dir);

    let out_dir = dir.join("out");
    let (out, stderr) = run_cli_ok(&[
        "batch",
        "export",
        "--session",
        session.to_str().unwrap(),
        "--output-dir",
        out_dir.to_str().unwrap(),
    ]);
    assert_eq!(out["ok"], true, "partial failure keeps exit 0");
    let mutated = out["result"]["mutated_paths"].as_array().unwrap();
    assert_eq!(mutated.len(), 1);
    assert!(mutated[0]["source"].as_str().unwrap().contains("good.wav"));
    let failed = out["result"]["failed_paths"].as_array().unwrap();
    assert_eq!(failed.len(), 1);
    assert!(failed[0]["path"].as_str().unwrap().contains("broken.wav"));
    let warnings = warnings_of(&out);
    assert!(
        warnings.iter().any(|w| w.contains("broken.wav")),
        "failed exports must surface as warnings: {warnings:?}"
    );
    assert!(
        stderr.contains("warning:") && stderr.contains("broken.wav"),
        "warnings must be mirrored to stderr: {stderr}"
    );
    assert!(out_dir.join("good.wav").is_file(), "good file exported");

    // Loudness plan over the same session also warns about the broken file.
    let (plan, _) = run_cli_ok(&[
        "batch",
        "loudness",
        "plan",
        "--session",
        session.to_str().unwrap(),
        "--target-lufs",
        "-23",
    ]);
    let warnings = warnings_of(&plan);
    assert!(
        warnings.iter().any(|w| w.contains("broken.wav")),
        "loudness plan must warn about undecodable files: {warnings:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_batch_export_fails_when_every_file_is_undecodable() {
    let dir = make_temp_dir("export_all_fail");
    write_junk_wav(&dir.join("broken_a.wav"));
    write_junk_wav(&dir.join("broken_b.wav"));
    let session = new_session(&dir);

    let out_dir = dir.join("out");
    let out = run_cli_raw(&[
        "batch",
        "export",
        "--session",
        session.to_str().unwrap(),
        "--output-dir",
        out_dir.to_str().unwrap(),
    ]);
    assert!(
        !out.status.success(),
        "all-files-failed batch export must exit nonzero\nstdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("error envelope is still JSON");
    assert_eq!(envelope["ok"], false);
    let errors = envelope["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap_or_default().contains("failed to export")),
        "errors: {errors:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error:"),
        "errors must be mirrored to stderr: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
