//! End-to-end test for `--cli batch engine-export` driving the real binary.

use std::path::PathBuf;
use std::process::Command;

fn make_temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "neowaves_engine_export_cli_{}",
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
fn cli_engine_export_writes_unity_json_and_wwise_tsv() {
    let dir = make_temp_dir();
    let sr = 48_000u32;
    let looped = dir.join("bgm_town.wav");
    neowaves::wave::export_channels_audio(&[tone(sr, 0.5)], sr, &looped).expect("looped");
    neowaves::loop_markers::write_loop_markers(&looped, Some((1_000, 20_000)))
        .expect("loop markers");
    neowaves::wave::export_channels_audio(&[tone(sr, 0.25)], sr, &dir.join("se_hit.wav"))
        .expect("oneshot");

    let session = dir.join("engine.nwsess");
    run_cli(&[
        "session",
        "new",
        "--folder",
        dir.to_str().unwrap(),
        "--output",
        session.to_str().unwrap(),
    ]);

    // Unity JSON
    let out_json = dir.join("unity.json");
    let out = run_cli(&[
        "batch",
        "engine-export",
        "--session",
        session.to_str().unwrap(),
        "--engine",
        "unity",
        "--output",
        out_json.to_str().unwrap(),
    ]);
    assert_eq!(out["result"]["entries"], 2, "{}", out["result"]);
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_json).unwrap()).unwrap();
    let rows = body.as_array().expect("array");
    assert_eq!(rows.len(), 2);
    let bgm = rows
        .iter()
        .find(|r| r["name"] == "bgm_town")
        .expect("bgm row");
    assert_eq!(bgm["loop"], true);
    assert_eq!(bgm["loopStart"], 1_000);
    assert_eq!(bgm["loopEnd"], 20_000);
    assert_eq!(bgm["sampleRate"], 48_000);
    let se = rows.iter().find(|r| r["name"] == "se_hit").expect("se row");
    assert_eq!(se["loop"], false);

    // Wwise TSV
    let out_tsv = dir.join("wwise.tsv");
    run_cli(&[
        "batch",
        "engine-export",
        "--session",
        session.to_str().unwrap(),
        "--engine",
        "wwise",
        "--output",
        out_tsv.to_str().unwrap(),
    ]);
    let tsv = std::fs::read_to_string(&out_tsv).unwrap();
    let lines: Vec<&str> = tsv.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].starts_with("ObjectName\tAudioFile"));
    assert!(tsv.contains("bgm_town\t"));
    assert!(tsv.contains("\t1000\t20000\t"));

    // Unknown engine fails.
    let exe = env!("CARGO_BIN_EXE_neowaves");
    let out = Command::new(exe)
        .args([
            "--cli",
            "batch",
            "engine-export",
            "--session",
            session.to_str().unwrap(),
            "--engine",
            "unreal",
            "--output",
            dir.join("x.json").to_str().unwrap(),
        ])
        .output()
        .expect("run");
    assert!(!out.status.success(), "unknown engine must fail");

    let _ = std::fs::remove_dir_all(&dir);
}
