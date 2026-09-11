//! A file too large for a resident editor buffer still has to draw.
//!
//! Past the resident ceiling the decode publishes an overview and no PCM, and
//! the tab is marked `paged_asset`. The canvas computed its timeline length
//! from `loading || samples_len` alone, so a paged tab -- which is neither
//! loading nor holding samples -- measured zero: no zoom was initialised, no
//! ruler was drawn, and every lane's waveform was skipped. The editor opened on
//! a black rectangle with no message, and 12 channels is what gets an ordinary
//! two-and-a-half-minute file over that ceiling in the first place.
#![cfg(feature = "kittest")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use neowaves::kittest::harness_with_startup;
use neowaves::StartupConfig;

/// Small enough that a fraction of a second of 12-channel audio crosses it.
const RESIDENT_LIMIT_BYTES: u64 = 200_000;

fn make_temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "neowaves_editor_paged_{tag}_{}_{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// 12 channels of 24-bit/48 kHz, the shape that trips the ceiling in practice.
/// Every channel is loud so an empty canvas cannot be mistaken for silence.
fn make_multichannel_wav(dir: &std::path::Path, channels: usize, secs: f32) -> PathBuf {
    let sr = 48_000u32;
    let frames = (sr as f32 * secs) as usize;
    let chans: Vec<Vec<f32>> = (0..channels)
        .map(|ch| {
            (0..frames)
                .map(|i| {
                    let t = i as f32 / sr as f32;
                    let hz = 110.0 + 55.0 * ch as f32;
                    (t * hz * std::f32::consts::TAU).sin() * 0.9
                })
                .collect()
        })
        .collect();
    let path = dir.join(format!("{channels}ch.wav"));
    neowaves::wave::export_channels_audio_with_depth(
        &chans,
        sr,
        &path,
        Some(neowaves::wave::WavBitDepth::Pcm24),
    )
    .expect("write multichannel wav fixture");
    path
}

/// Scan `dir` into a fresh harness and wait for the list to settle.
///
/// A macro rather than a function: the harness type comes from `egui_kittest`,
/// which this test crate cannot name.
macro_rules! scanned_harness {
    ($dir:expr) => {{
        let cfg = StartupConfig {
            open_folder: Some($dir.to_path_buf()),
            open_first: false,
            ..StartupConfig::default()
        };
        let mut harness = harness_with_startup(cfg);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            harness.run_steps(1);
            if !harness.state().scan_in_progress && !harness.state().files.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "scan did not finish");
        }
        harness
    }};
}

#[test]
fn a_paged_tab_draws_its_overview_instead_of_an_empty_canvas() {
    std::env::set_var(
        "NEOWAVES_MAX_RESIDENT_DECODE_BYTES",
        RESIDENT_LIMIT_BYTES.to_string(),
    );
    let dir = make_temp_dir("overview");
    // 0.5 s x 12 ch x 4 B = 1.15 MB decoded, well past the ceiling above.
    let path = make_multichannel_wav(&dir, 12, 0.5);

    let mut harness = scanned_harness!(dir);
    assert!(
        harness.state_mut().test_probe_audio_asset_for_path(&path),
        "could not read the fixture's header back"
    );
    assert!(harness.state_mut().test_open_tab_for_path(&path));

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        harness.run_steps(1);
        if harness.state().test_active_tab_paged_asset() == Some(true) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the oversized fixture never took the paged path"
        );
    }

    // Let the canvas settle on the paged tab.
    harness.run_steps(4);

    let display_len = harness
        .state()
        .test_editor_display_samples_len()
        .expect("an active tab");
    assert!(
        display_len > 0,
        "a paged tab reported a zero-length timeline"
    );
    assert!(
        harness.state().test_active_tab_loading_waveform_ready(),
        "a paged tab kept no overview to draw from"
    );
    assert!(
        harness.state().test_active_tab_paged_asset() == Some(true),
        "the tab stopped being paged before the canvas was measured"
    );

    // Count only the draws from here on. The tab passes through a loading
    // state on its way to paged and draws its overview there, so the totals
    // are already non-zero by now whether or not the paged tab draws at all.
    let sum = |(raw, visible, pyramid): (u64, u64, u64)| raw + visible + pyramid;
    let before = sum(harness.state().test_waveform_lod_counts());
    harness.run_steps(3);
    let after = sum(harness.state().test_waveform_lod_counts());
    assert!(
        after > before,
        "the paged tab drew no waveform over three frames ({before} -> {after})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn more_than_two_channels_opens_on_per_channel_lanes() {
    let dir = make_temp_dir("lanes");
    // Short enough to stay under the resident ceiling and decode in full, so
    // this covers the ordinary multichannel tab rather than the paged one.
    let stereo = make_multichannel_wav(&dir, 2, 0.2);
    let twelve = make_multichannel_wav(&dir, 12, 0.2);

    let mut harness = scanned_harness!(dir);

    for (path, channels, expected_mode) in [(&stereo, 2usize, "mixdown"), (&twelve, 12usize, "all")]
    {
        assert!(harness.state_mut().test_open_tab_for_path(path));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            harness.run_steps(1);
            if harness.state().test_active_tab_channel_count() == Some(channels) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{} never finished decoding",
                path.display()
            );
        }
        assert_eq!(
            harness.state().test_active_tab_channel_view_mode(),
            Some(expected_mode),
            "{channels}-channel file opened on the wrong channel view"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
