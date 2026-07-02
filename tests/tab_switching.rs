//! Regression coverage for editor tab-switching, specifically that closing a
//! background (non-active) tab does not change which tab is active.
//!
//! `close_tab_at` used to recompute `active_tab` purely from the *closed* index,
//! so closing any other tab could yank the active tab to a different one (and
//! trigger a needless re-decode of it).
#![cfg(feature = "kittest")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui_kittest::Harness;
use neowaves::kittest::harness_with_startup;
use neowaves::{StartupConfig, WavesPreviewer};

fn make_temp_dir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "neowaves_tab_switching_{tag}_{}_{}_{}",
        std::process::id(),
        now_ms,
        seq
    ));
    std::fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

fn synth(sr: u32, secs: f32, freq: f32) -> Vec<Vec<f32>> {
    let frames = ((sr as f32) * secs).max(1.0) as usize;
    let mut mono = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = (i as f32) / (sr as f32);
        mono.push((t * freq * std::f32::consts::TAU).sin() * 0.25);
    }
    vec![mono.clone(), mono]
}

fn write_wav(dir: &Path, name: &str, freq: f32) -> PathBuf {
    let path = dir.join(name);
    neowaves::wave::export_channels_audio(&synth(48_000, 1.0, freq), 48_000, &path)
        .expect("write wav");
    path
}

fn harness_with_folder(dir: PathBuf) -> Harness<'static, WavesPreviewer> {
    let mut cfg = StartupConfig::default();
    cfg.open_folder = Some(dir);
    cfg.open_first = false;
    harness_with_startup(cfg)
}

fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        if !harness.state().files.is_empty() {
            break;
        }
        if start.elapsed() > Duration::from_secs(20) {
            panic!("scan timeout");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Writes a,b,c into `dir` (must be done *before* the harness scans the folder)
/// and returns their paths in order.
fn write_three(dir: &Path) -> [PathBuf; 3] {
    [
        write_wav(dir, "a.wav", 220.0),
        write_wav(dir, "b.wav", 330.0),
        write_wav(dir, "c.wav", 440.0),
    ]
}

/// Scans the folder and opens a,b,c as three tabs (active ends on the last
/// opened).
fn open_three_tabs(harness: &mut Harness<'static, WavesPreviewer>, paths: &[PathBuf; 3]) {
    wait_for_scan(harness);
    for p in paths {
        assert!(harness.state_mut().test_open_tab_for_path(p), "open {p:?}");
        harness.run_steps(1);
    }
    assert_eq!(harness.state().tabs.len(), 3);
}

#[test]
fn closing_first_background_tab_keeps_active_tab() {
    let dir = make_temp_dir("close_first_bg");
    let [a, _b, c] = write_three(&dir);
    let mut harness = harness_with_folder(dir.clone());
    open_three_tabs(&mut harness, &[a.clone(), _b.clone(), c.clone()]);

    // Active tab is c (last opened).
    assert_eq!(
        harness.state().test_active_tab_path().as_deref(),
        Some(c.as_path())
    );

    // Close a (the first, background tab).
    assert!(harness.state_mut().test_close_tab_for_path(&a));
    harness.run_steps(1);

    assert_eq!(harness.state().tabs.len(), 2);
    assert_eq!(
        harness.state().test_active_tab_path().as_deref(),
        Some(c.as_path()),
        "closing a background tab must not change the active tab"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_last_background_tab_keeps_active_tab() {
    let dir = make_temp_dir("close_last_bg");
    let [a, _b, c] = write_three(&dir);
    let mut harness = harness_with_folder(dir.clone());
    open_three_tabs(&mut harness, &[a.clone(), _b.clone(), c.clone()]);

    // Re-activate a (idx 0), leaving c (idx 2) as a background tab.
    assert!(harness.state_mut().test_open_tab_for_path(&a));
    harness.run_steps(1);
    assert_eq!(
        harness.state().test_active_tab_path().as_deref(),
        Some(a.as_path())
    );

    // Close c (a later, background tab).
    assert!(harness.state_mut().test_close_tab_for_path(&c));
    harness.run_steps(1);

    assert_eq!(harness.state().tabs.len(), 2);
    assert_eq!(
        harness.state().test_active_tab_path().as_deref(),
        Some(a.as_path()),
        "closing a later background tab must not change the active tab"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_active_tab_falls_to_neighbour() {
    let dir = make_temp_dir("close_active");
    let [a, b, c] = write_three(&dir);
    let mut harness = harness_with_folder(dir.clone());
    open_three_tabs(&mut harness, &[a.clone(), b.clone(), c.clone()]);

    // Make the middle tab (b) active, then close it.
    assert!(harness.state_mut().test_open_tab_for_path(&b));
    harness.run_steps(1);
    assert_eq!(
        harness.state().test_active_tab_path().as_deref(),
        Some(b.as_path())
    );

    assert!(harness.state_mut().test_close_tab_for_path(&b));
    harness.run_steps(1);

    assert_eq!(harness.state().tabs.len(), 2);
    // After removing b, tabs are [a, c]; the right neighbour (c) becomes active.
    let active = harness.state().test_active_tab_path();
    assert!(
        active.is_some(),
        "closing the active tab should leave another tab active"
    );
    assert_eq!(
        active.as_deref(),
        Some(c.as_path()),
        "closing the active middle tab should activate its right neighbour"
    );
    assert!(active.as_deref() != Some(a.as_path()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_background_tab_from_list_view_keeps_no_active_tab() {
    let dir = make_temp_dir("close_bg_from_list");
    let [a, b, c] = write_three(&dir);
    let mut harness = harness_with_folder(dir.clone());
    open_three_tabs(&mut harness, &[a.clone(), b.clone(), c.clone()]);

    // Go back to the list: no editor tab is active there.
    harness.state_mut().test_switch_to_list_workspace();
    harness.state_mut().active_tab = None;
    harness.run_steps(1);

    // Closing a background tab from the list must not conjure up an
    // "active" editor tab out of thin air.
    assert!(harness.state_mut().test_close_tab_for_path(&b));
    harness.run_steps(1);

    assert_eq!(harness.state().tabs.len(), 2);
    assert_eq!(
        harness.state().active_tab,
        None,
        "no tab was active before the close, so none may be active after"
    );
    assert!(!harness.state().test_is_editor_workspace_active());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn closing_only_tab_clears_active_and_returns_to_list() {
    let dir = make_temp_dir("close_only");
    let a = write_wav(&dir, "only.wav", 220.0);
    let mut harness = harness_with_folder(dir.clone());
    wait_for_scan(&mut harness);

    assert!(harness.state_mut().test_open_tab_for_path(&a));
    harness.run_steps(1);
    assert_eq!(harness.state().tabs.len(), 1);

    assert!(harness.state_mut().test_close_tab_for_path(&a));
    harness.run_steps(1);

    assert_eq!(harness.state().tabs.len(), 0);
    assert_eq!(harness.state().test_active_tab_path(), None);
    assert!(
        !harness.state().test_is_editor_workspace_active(),
        "closing the only tab should leave the editor workspace"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
