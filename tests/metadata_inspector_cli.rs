//! End-to-end coverage for the read-only Metadata Inspector CLI.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn make_temp_dir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "neowaves_metadata_cli_{tag}_{}_{}_{}",
        std::process::id(),
        now_ms,
        seq
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn tone(sample_rate: u32, seconds: f32) -> Vec<f32> {
    let frames = ((sample_rate as f32) * seconds).max(1.0) as usize;
    (0..frames)
        .map(|frame| {
            ((frame as f32 / sample_rate as f32) * 440.0 * std::f32::consts::TAU).sin() * 0.25
        })
        .collect()
}

fn run_cli_raw(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_neowaves"))
        .arg("--cli")
        .args(args)
        .output()
        .expect("run neowaves --cli")
}

fn run_cli(args: &[&str]) -> serde_json::Value {
    let output = run_cli_raw(args);
    assert!(
        output.status.success(),
        "cli failed: {args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI stdout is JSON")
}

fn file_sha256(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(path).expect("read file for hash"));
    format!("{:x}", hasher.finalize())
}

#[test]
fn metadata_commands_are_lossless_and_do_not_modify_the_source() {
    let dir = make_temp_dir("readonly");
    let input = dir.join("source.wav");
    neowaves::wave::export_channels_audio(
        &[tone(48_000, 0.05), tone(48_000, 0.05)],
        48_000,
        &input,
    )
    .expect("write WAV fixture");

    let before_metadata = std::fs::metadata(&input).expect("source metadata");
    let before_len = before_metadata.len();
    let before_modified = before_metadata.modified().expect("source mtime");
    let before_hash = file_sha256(&input);
    let input_str = input.to_str().expect("UTF-8 fixture path");

    let inspect = run_cli(&["item", "metadata", "inspect", "--input", input_str]);
    assert_eq!(inspect["ok"], true);
    assert_eq!(inspect["result"]["container"], "riff");
    assert_eq!(
        inspect["result"]["file_length"],
        before_len.to_string(),
        "u64 values use lossless decimal strings"
    );
    assert!(inspect["result"]["file_length_hex"]
        .as_str()
        .is_some_and(|value| value.starts_with("0x")));
    let nodes = inspect["result"]["nodes"].as_array().expect("nodes array");
    assert!(nodes.iter().any(|node| node["name"] == "fmt "));
    assert!(nodes.iter().any(|node| node["name"] == "data"));

    let summary = run_cli(&[
        "item",
        "metadata",
        "summary",
        "--input",
        input_str,
        "--include-raw",
    ]);
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["result"]["container"], "riff");
    assert!(summary["result"]["coverage"]
        .as_array()
        .expect("coverage array")
        .iter()
        .any(|value| value == "__raw__"));

    let read = run_cli(&[
        "item", "metadata", "payload", "read", "--input", input_str, "--offset", "0", "--length",
        "12", "--format", "hex",
    ]);
    assert_eq!(read["result"]["offset"], "0");
    assert_eq!(read["result"]["length"], "12");
    assert!(
        read["result"]["data"]
            .as_str()
            .expect("hex data")
            .starts_with("52 49 46 46"),
        "RIFF signature is visible"
    );

    let search = run_cli(&[
        "item", "metadata", "payload", "search", "--input", input_str, "--offset", "0", "--length",
        "12", "--kind", "fourcc", "--query", "RIFF",
    ]);
    assert_eq!(search["result"]["matches"][0]["offset"], "0");
    assert_eq!(search["result"]["matches"][0]["offset_hex"], "0x0");

    let hash = run_cli(&[
        "item",
        "metadata",
        "payload",
        "hash",
        "--input",
        input_str,
        "--offset",
        "0",
        "--length",
        "12",
        "--algorithm",
        "sha256",
    ]);
    assert_eq!(hash["result"]["algorithm"], "sha256");
    assert_eq!(
        hash["result"]["digest"]
            .as_str()
            .expect("SHA-256 digest")
            .len(),
        64
    );

    let extracted = dir.join("header.bin");
    let extracted_str = extracted.to_str().expect("UTF-8 output path");
    run_cli(&[
        "item",
        "metadata",
        "payload",
        "extract",
        "--input",
        input_str,
        "--offset",
        "0",
        "--length",
        "12",
        "--output",
        extracted_str,
    ]);
    assert_eq!(
        std::fs::read(&extracted).expect("read extracted payload"),
        std::fs::read(&input).expect("read source")[..12]
    );
    assert!(!extracted.with_extension("bin.part").exists());

    let refused = run_cli_raw(&[
        "item",
        "metadata",
        "payload",
        "extract",
        "--input",
        input_str,
        "--offset",
        "0",
        "--length",
        "12",
        "--output",
        extracted_str,
    ]);
    assert!(
        !refused.status.success(),
        "extract must refuse overwrite without --overwrite"
    );
    run_cli(&[
        "item",
        "metadata",
        "payload",
        "extract",
        "--input",
        input_str,
        "--offset",
        "0",
        "--length",
        "12",
        "--output",
        extracted_str,
        "--overwrite",
    ]);

    let after_metadata = std::fs::metadata(&input).expect("source metadata after commands");
    assert_eq!(after_metadata.len(), before_len);
    assert_eq!(
        after_metadata
            .modified()
            .expect("source mtime after commands"),
        before_modified
    );
    assert_eq!(file_sha256(&input), before_hash);
}
