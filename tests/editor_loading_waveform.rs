//! While a file decodes, the editor draws the part of it that exists -- in the
//! place that part belongs.
//!
//! The overview used to be built from the decoded prefix alone and then spread
//! across every bin, so the head of the file was drawn over the whole canvas
//! and redrawn at a different scale on each progress emit. The waveform
//! stretched out from the left while it loaded, and nothing on it agreed with
//! the playhead, which has always run on the whole file's timeline.
#![cfg(feature = "kittest")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use neowaves::kittest::harness_with_startup;
use neowaves::StartupConfig;

fn make_temp_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "neowaves_editor_loading_{tag}_{}_{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Long enough that the decode cannot finish inside the first frames, and
/// loud all the way through so a filled bin is unmistakable.
fn make_long_mp3(dir: &std::path::Path, secs: f32) -> PathBuf {
    let sr = 44_100u32;
    let frames = (sr as f32 * secs) as usize;
    let chans: Vec<Vec<f32>> = (0..2)
        .map(|ch| {
            (0..frames)
                .map(|i| {
                    let t = i as f32 / sr as f32;
                    (t * (220.0 + 110.0 * ch as f32) * std::f32::consts::TAU).sin() * 0.5
                })
                .collect()
        })
        .collect();
    let path = dir.join("long.mp3");
    neowaves::wave::export_channels_audio(&chans, sr, &path).expect("write mp3 fixture");
    path
}

#[test]
fn a_loading_waveform_fills_its_canvas_instead_of_stretching_across_it() {
    let dir = make_temp_dir("fill");
    let path = make_long_mp3(&dir, 40.0);

    let cfg = StartupConfig {
        open_folder: Some(dir.clone()),
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
    assert!(harness.state_mut().test_open_tab_for_path(&path));

    // Watch the decode go by. Every sample of it must satisfy the same rule:
    // the drawn part of the overview does not run ahead of the decoded part of
    // the file. Before the fix the first number was pinned at 1.0 from the
    // first emit, whatever the second one said.
    let mut samples = 0usize;
    let mut worst_lead = f32::NEG_INFINITY;
    let deadline = Instant::now() + Duration::from_secs(60);
    while harness.state().test_active_tab_loading_waveform_ready() && Instant::now() < deadline {
        if let Some((filled, decoded)) = harness.state().test_active_tab_loading_overview_progress()
        {
            // The initial picture is the list thumbnail -- the whole file,
            // already correct -- so only partial states are evidence either
            // way; a full overview with no decode yet is that thumbnail.
            if decoded > 0.02 && decoded < 0.9 {
                samples += 1;
                worst_lead = worst_lead.max(filled - decoded);
                assert!(
                    filled <= decoded + 0.25,
                    "the overview drew {:.0}% of the canvas with {:.0}% decoded",
                    filled * 100.0,
                    decoded * 100.0
                );
            }
        }
        harness.run_steps(1);
    }

    assert!(
        samples > 0,
        "never observed a partial decode; the fixture is too short to prove anything"
    );
    assert!(
        worst_lead < 0.25,
        "the drawn fraction ran {worst_lead:.2} ahead of the decoded fraction"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
