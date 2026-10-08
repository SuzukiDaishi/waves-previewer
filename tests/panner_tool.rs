//! The editor's Panner tool: a live preview the output callback applies
//! while the tab plays, and an Apply that writes the same pan. See
//! `src/panning.rs` and `docs/MULTICHANNEL_SPEC.md`.
#![cfg(feature = "kittest")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use neowaves::app::ToolKind;
use neowaves::kittest::harness_with_startup;
use neowaves::{StartupConfig, WavesPreviewer};

type App = Harness<'static, WavesPreviewer>;

const FIXTURE_SR: u32 = 48_000;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("neowaves_panner_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn tone(freq: f32, amp: f32) -> Vec<f32> {
    (0..(FIXTURE_SR / 10) as usize)
        .map(|i| (i as f32 / FIXTURE_SR as f32 * freq * std::f32::consts::TAU).sin() * amp)
        .collect()
}

fn wait_until(harness: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        if done(harness) {
            return;
        }
        if start.elapsed() > Duration::from_secs(20) {
            panic!("timeout waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The file open in the editor on the Panner.
fn open_on_panner(dir: &Path, path: &Path) -> App {
    let mut cfg = StartupConfig::default();
    cfg.open_folder = Some(dir.to_path_buf());
    cfg.open_first = false;
    let mut harness = harness_with_startup(cfg);
    wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
    assert!(harness.state_mut().test_open_tab_for_path(path));
    wait_until(&mut harness, "tab ready", |h| {
        h.state()
            .active_tab
            .and_then(|idx| h.state().tabs.get(idx))
            .is_some_and(|tab| tab.samples_len > 0 && !tab.loading)
    });
    assert!(harness.state_mut().test_set_active_tool(ToolKind::Panner));
    harness.run_steps(2);
    harness
}

fn samples(harness: &App) -> Vec<Vec<f32>> {
    harness.state().test_tab_channel_samples().expect("samples")
}

#[test]
fn a_stereo_pan_is_heard_live_while_playing_and_applied_as_heard() {
    let dir = temp_dir("stereo");
    let path = dir.join("stereo.wav");
    let (left, right) = (tone(220.0, 0.4), tone(330.0, 0.3));
    neowaves::wave::export_channels_audio(&[left.clone(), right.clone()], FIXTURE_SR, &path)
        .expect("export fixture");
    let mut harness = open_on_panner(&dir, &path);

    // Hard left, previewed while the tab plays.
    assert!(harness
        .state_mut()
        .test_set_panner(None, -1.0, 0.0, 0.0, 0.0));
    assert!(harness.state_mut().test_set_panner_preview(true));
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(2);
    assert!(harness.state().test_audio_is_playing());
    assert!(
        harness.state().test_playback_source_is_editor_path(&path),
        "the tab itself plays"
    );
    assert!(
        !harness.state().test_playback_source_is_tool_preview(),
        "no audition buffer"
    );
    assert_eq!(harness.state().test_engine_pan(), Some((2, 2)));
    assert_eq!(
        harness.state().test_engine_pan_gain(1, 1),
        Some(0.0),
        "the right is off"
    );
    assert_eq!(harness.state().test_engine_pan_gain(0, 0), Some(1.0));
    assert!(
        harness.state().test_panner_overlay_present(),
        "the green waveform"
    );

    // Turned the other way while it plays: heard at once, nothing stops.
    assert!(harness
        .state_mut()
        .test_set_panner(None, 1.0, 0.0, 0.0, 0.0));
    harness.run_steps(1);
    assert_eq!(harness.state().test_engine_pan_gain(0, 0), Some(0.0));
    assert!(harness.state().test_audio_is_playing());

    // Preview off: the panner goes, the playback stays.
    assert!(harness.state_mut().test_set_panner_preview(false));
    harness.run_steps(1);
    assert_eq!(harness.state().test_engine_pan(), None);
    assert!(harness.state().test_audio_is_playing());
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(1);

    // Applied: what was heard -- the left off, the right as it was.
    assert!(harness.state_mut().test_apply_panner());
    harness.run_steps(2);
    let applied = samples(&harness);
    assert_eq!(applied.len(), 2);
    assert!(
        applied[0].iter().all(|v| v.abs() < 1e-6),
        "the left is silent"
    );
    let worst = applied[1]
        .iter()
        .zip(&right)
        .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(worst < 1e-3, "the right is untouched ({worst})");
    assert!(harness.state_mut().test_editor_undo());
    harness.run_steps(2);
    let undone = samples(&harness);
    let worst = undone[0]
        .iter()
        .zip(&left)
        .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(worst < 1e-3, "undo brings the left back ({worst})");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_mono_file_is_panned_onto_a_stereo_pair() {
    let dir = temp_dir("mono");
    let path = dir.join("mono.wav");
    let mono = tone(440.0, 0.5);
    neowaves::wave::export_channels_audio(&[mono.clone()], FIXTURE_SR, &path)
        .expect("export fixture");
    let mut harness = open_on_panner(&dir, &path);

    assert!(harness
        .state_mut()
        .test_set_panner(None, -0.5, 0.0, 0.0, 0.0));
    assert!(harness.state_mut().test_set_panner_preview(true));
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(3);
    assert_eq!(
        harness.state().test_engine_pan(),
        Some((1, 2)),
        "one in, a stereo pair out"
    );
    assert_eq!(
        harness.state().test_playback_source_layout().as_deref(),
        Some("FL,FR"),
        "it plays as stereo"
    );
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(1);

    assert!(harness.state_mut().test_apply_panner());
    harness.run_steps(2);
    let applied = samples(&harness);
    assert_eq!(applied.len(), 2, "the file is stereo now");
    let half_way = std::f32::consts::FRAC_1_SQRT_2;
    for i in (0..mono.len()).step_by(97) {
        assert!(
            (applied[0][i] - mono[i]).abs() < 1e-3,
            "the left keeps the level"
        );
        assert!(
            (applied[1][i] - mono[i] * half_way).abs() < 1e-3,
            "the right turned down"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_quarter_yaw_turns_a_seven_one_four_centre_onto_the_right_side() {
    const LAYOUT: &str = "FL,FR,FC,LFE,BL,BR,SL,SR,TFL,TFR,TBL,TBR";
    let dir = temp_dir("714");
    let path = dir.join("bed.wav");
    let centre = tone(550.0, 0.4);
    let lfe = tone(60.0, 0.2);
    let mut channels = vec![vec![0.0f32; centre.len()]; 12];
    channels[2] = centre.clone();
    channels[3] = lfe.clone();
    neowaves::wave::export_channels_audio(&channels, FIXTURE_SR, &path).expect("export fixture");
    let mut harness = open_on_panner(&dir, &path);
    assert!(harness
        .state_mut()
        .test_set_channel_layout_override(&path, Some(LAYOUT)));
    harness.run_steps(1);

    // The default for 7.1.4 is the 3-D turn.
    assert!(harness
        .state_mut()
        .test_set_panner(None, 0.0, 90.0, 0.0, 0.0));
    assert!(harness.state_mut().test_apply_panner());
    harness.run_steps(2);
    let applied = samples(&harness);
    assert_eq!(applied.len(), 12);
    let close = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3);
    assert!(
        close(&applied[7], &centre),
        "the centre is on the right side (SR)"
    );
    assert!(close(&applied[3], &lfe), "the LFE does not move");
    for ch in [0usize, 1, 2, 4, 5, 6, 8, 9, 10, 11] {
        assert!(
            applied[ch].iter().all(|v| v.abs() < 1e-4),
            "channel {ch} is empty"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The Panner's panel for stereo, 7.1.4 (turned) and mono, saved for a look:
/// `debug/screenshot_verify/panner/*.png`.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_panner_panel() {
    const LAYOUT: &str = "FL,FR,FC,LFE,BL,BR,SL,SR,TFL,TFR,TBL,TBR";
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("panner");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    let save = |harness: &mut App, name: &str| {
        harness
            .render()
            .expect("render")
            .save(out_dir.join(name))
            .expect("save the screenshot");
    };

    let dir = temp_dir("render");
    let stereo = dir.join("stereo.wav");
    neowaves::wave::export_channels_audio(
        &[tone(220.0, 0.4), tone(330.0, 0.3)],
        FIXTURE_SR,
        &stereo,
    )
    .expect("export fixture");
    let mut harness = open_on_panner(&dir, &stereo);
    harness.set_size(egui::vec2(1400.0, 900.0));
    assert!(harness
        .state_mut()
        .test_set_panner(None, -0.4, 0.0, 0.0, 0.0));
    assert!(harness.state_mut().test_set_panner_preview(true));
    harness.run_steps(4);
    save(&mut harness, "01_stereo_balance.png");
    let _ = std::fs::remove_dir_all(&dir);

    let dir = temp_dir("render714");
    let bed = dir.join("bed.wav");
    let channels: Vec<Vec<f32>> = (0..12).map(|c| tone(110.0 * (c + 1) as f32, 0.2)).collect();
    neowaves::wave::export_channels_audio(&channels, FIXTURE_SR, &bed).expect("export fixture");
    let mut harness = open_on_panner(&dir, &bed);
    harness.set_size(egui::vec2(1400.0, 900.0));
    assert!(harness
        .state_mut()
        .test_set_channel_layout_override(&bed, Some(LAYOUT)));
    assert!(harness
        .state_mut()
        .test_set_panner(None, 0.0, 60.0, 30.0, 0.0));
    harness.run_steps(4);
    save(&mut harness, "02_714_turned.png");
    let _ = std::fs::remove_dir_all(&dir);

    let dir = temp_dir("rendermono");
    let mono = dir.join("mono.wav");
    neowaves::wave::export_channels_audio(&[tone(440.0, 0.5)], FIXTURE_SR, &mono)
        .expect("export fixture");
    let mut harness = open_on_panner(&dir, &mono);
    harness.set_size(egui::vec2(1400.0, 900.0));
    assert!(harness
        .state_mut()
        .test_set_panner(Some("Vbap"), 0.0, -20.0, 0.0, 0.0));
    harness.run_steps(4);
    save(&mut harness, "03_mono_to_stereo.png");
    let _ = std::fs::remove_dir_all(&dir);
}
