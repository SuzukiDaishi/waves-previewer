// Diagnostic benchmark for very large lists (500k files).
// Not run in CI: requires NEOWAVES_BENCH_DIR pointing at a folder tree of
// audio files (see scripts in the PR description / scratchpad).
//
//   NEOWAVES_BENCH_DIR=/tmp/flac500k cargo test --features kittest \
//       --test large_list_bench -- --nocapture --ignored
#![cfg(feature = "kittest")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use neowaves::kittest::harness_default;

fn rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: f64 = rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .unwrap_or(0.0);
            return kb / 1024.0;
        }
    }
    0.0
}

/// One-time fixture helper: writes a single short FLAC to
/// $NEOWAVES_BENCH_DIR/seed.flac; hardlink it N times from a shell script.
#[test]
#[ignore]
fn generate_seed_flac() {
    let dir = PathBuf::from(std::env::var("NEOWAVES_BENCH_DIR").expect("set NEOWAVES_BENCH_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let sr = 44_100u32;
    let frames = (sr as f32 * 0.5) as usize;
    let mono: Vec<f32> = (0..frames)
        .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.25)
        .collect();
    neowaves::wave::export_channels_audio(&[mono], sr, &dir.join("seed.flac")).unwrap();
}

#[test]
#[ignore]
fn bench_load_large_folder() {
    let dir = std::env::var("NEOWAVES_BENCH_DIR").expect("set NEOWAVES_BENCH_DIR");
    let dir = PathBuf::from(dir);
    assert!(dir.is_dir());

    let mut harness = harness_default();
    harness.step();
    eprintln!("[bench] baseline rss={:.0}MB", rss_mb());

    harness.state_mut().test_start_folder_load(dir);

    // Phase 1: scan+append until the list load fully finalizes.
    let started = Instant::now();
    let mut frames = 0u64;
    let mut worst_ms = 0.0f64;
    let mut last_log = Instant::now();
    loop {
        let t0 = Instant::now();
        harness.step();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        worst_ms = worst_ms.max(ms);
        frames += 1;
        let scanning = harness.state().test_topbar_scan_activity_text().is_some();
        if last_log.elapsed() > Duration::from_secs(2) {
            eprintln!(
                "[bench] t={:.1}s files={} frame_ms(last)={:.1} worst={:.1} rss={:.0}MB scanning={}",
                started.elapsed().as_secs_f64(),
                harness.state().test_files_len(),
                ms,
                worst_ms,
                rss_mb(),
                scanning,
            );
            last_log = Instant::now();
        }
        if !scanning && harness.state().test_files_len() > 0 {
            break;
        }
        if started.elapsed() > Duration::from_secs(1200) {
            panic!(
                "load did not finish in 20min: files={} worst_frame={worst_ms:.1}ms",
                harness.state().test_files_len()
            );
        }
    }
    eprintln!(
        "[bench] LOAD DONE files={} in {:.1}s frames={} worst_frame={:.1}ms rss={:.0}MB",
        harness.state().test_files_len(),
        started.elapsed().as_secs_f64(),
        frames,
        worst_ms,
        rss_mb(),
    );

    // Phase 2: steady-state frame cost.
    let mut total = 0.0f64;
    let mut worst = 0.0f64;
    const N: usize = 60;
    for _ in 0..N {
        let t0 = Instant::now();
        harness.step();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        worst = worst.max(ms);
    }
    eprintln!(
        "[bench] steady: avg={:.2}ms worst={:.2}ms rss={:.0}MB",
        total / N as f64,
        worst,
        rss_mb()
    );

    // Phase 3: async sort by File — the click itself must be ~free; the
    // decorate slices + worker sort run over the following frames.
    let t0 = Instant::now();
    harness.state_mut().test_request_sort_file_asc();
    let click_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut worst = 0.0f64;
    let mut frames = 0u64;
    while harness.state().test_sort_job_active() {
        let t = Instant::now();
        harness.step();
        worst = worst.max(t.elapsed().as_secs_f64() * 1000.0);
        frames += 1;
        assert!(frames < 100_000, "sort never settled");
    }
    eprintln!(
        "[bench] async sort-by-file: click={:.2}ms settle={:.1}ms frames={} worst_frame={:.1}ms",
        click_ms,
        t0.elapsed().as_secs_f64() * 1000.0,
        frames,
        worst
    );

    // Phase 4: metadata-sort storm — SampleRate sort streams header metadata
    // for the whole list; the UI must stay responsive while workers churn.
    let t0 = Instant::now();
    harness.state_mut().test_request_sort_sample_rate_asc();
    let click_ms = t0.elapsed().as_secs_f64() * 1000.0;
    while harness.state().test_sort_job_active() {
        harness.step();
    }
    let mut total = 0.0f64;
    let mut worst = 0.0f64;
    const STORM_FRAMES: usize = 240;
    for i in 0..STORM_FRAMES {
        if i % 10 == 0 {
            // Simulated user interaction during the storm.
            let target = (i * 997) % harness.state().test_files_len();
            harness.state_mut().test_select_and_load_row(target);
        }
        let t = Instant::now();
        harness.step();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        worst = worst.max(ms);
    }
    eprintln!(
        "[bench] meta-storm steady (SR sort active, selections every 10f): click={:.2}ms avg={:.2}ms worst={:.2}ms rss={:.0}MB",
        click_ms,
        total / STORM_FRAMES as f64,
        worst,
        rss_mb()
    );

    // Phase 5: scroll precision at the far end of the list.
    let last = harness.state().test_files_len() - 1;
    assert!(harness.state_mut().test_select_row_with_autoscroll(last));
    harness.step();
    harness.step();
    let scroll = harness.state().test_list_scroll_row();
    eprintln!("[bench] scroll to last row: selected={last} scroll_row={scroll}");
    assert!(
        scroll <= last && last < scroll + 200,
        "selected row must be inside the visible window: scroll={scroll} last={last}"
    );
    assert!(harness.state_mut().test_select_row_with_autoscroll(0));
    harness.step();
    harness.step();
    assert!(
        harness.state().test_list_scroll_row() < 100,
        "scroll back to top failed: {}",
        harness.state().test_list_scroll_row()
    );
    eprintln!("[bench] final rss={:.0}MB", rss_mb());
}
