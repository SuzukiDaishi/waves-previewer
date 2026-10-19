//! The Clipper beside the Brickwall Limiter, and oversampling in both: set
//! from their panels, applied under the ceiling, a curve on the clipper's
//! ceiling, and the settings through a session. See `docs/INSPECTOR_SPEC.md`
//! (Oversampling) and `src/oversample.rs`.
#![cfg(feature = "kittest")]

use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use neowaves::app::ToolKind;
use neowaves::kittest::harness_with_startup_size;
use neowaves::oversample::Oversampling;
use neowaves::{StartupConfig, WavesPreviewer};

type App = Harness<'static, WavesPreviewer>;

fn wait_until(harness: &mut App, what: &str, secs: u64, mut done: impl FnMut(&App) -> bool) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        if done(harness) {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A stereo virtual row `secs` long, open in the editor with `tool` active,
/// in a window tall enough for the whole Inspector panel.
fn open(secs: f32, tool: ToolKind) -> App {
    let mut harness =
        harness_with_startup_size(StartupConfig::default(), egui::vec2(1400.0, 1500.0));
    harness.run_steps(2);
    let path = harness
        .state_mut()
        .test_add_virtual_multichannel(2, secs)
        .expect("a virtual row");
    assert!(harness.state_mut().test_open_tab_for_path(&path));
    wait_until(&mut harness, "the tab", 60, |h| {
        h.state()
            .active_tab
            .and_then(|idx| h.state().tabs.get(idx))
            .is_some_and(|tab| tab.samples_len > 0 && !tab.loading)
    });
    assert!(harness.state_mut().test_switch_tool(tool));
    harness.run_steps(3);
    harness
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0f32, |m, x| m.max(x.abs()))
}

fn wait_for_full_preview(harness: &mut App) {
    wait_until(harness, "the full preview", 120, |h| {
        h.state().test_tool_preview_complete() == Some(true)
            && !h.state().test_preview_busy_for_active_tab()
    });
}

fn click(harness: &mut App, label: &str) {
    let found = harness.query_all_by_label(label).count();
    assert_eq!(found, 1, "`{label}` is on screen once");
    harness.get_by_label(label).click();
    harness.run_steps(3);
}

#[test]
fn both_panels_set_their_oversampling_and_the_clipper_its_hard_ceiling() {
    let mut harness = open(1.0, ToolKind::Clipper);
    let st = harness.state().test_tool_state().unwrap();
    assert_eq!(
        st.clipper.oversampling,
        Oversampling::X4,
        "the clipper starts at 4x"
    );
    assert!(st.clipper.hard_ceiling);
    click(&mut harness, "8x");
    click(&mut harness, "Hard ceiling");
    let st = harness.state().test_tool_state().unwrap();
    assert_eq!(st.clipper.oversampling, Oversampling::X8);
    assert!(!st.clipper.hard_ceiling);

    assert!(harness.state_mut().test_switch_tool(ToolKind::Limiter));
    harness.run_steps(3);
    let st = harness.state().test_tool_state().unwrap();
    assert_eq!(
        st.limiter.oversampling,
        Oversampling::Off,
        "the limiter starts as it was"
    );
    click(&mut harness, "2x");
    let st = harness.state().test_tool_state().unwrap();
    assert_eq!(st.limiter.oversampling, Oversampling::X2);
}

#[test]
fn the_clipper_keeps_what_it_applies_under_its_ceiling() {
    let mut harness = open(1.0, ToolKind::Clipper);
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.clipper.input_gain_db = 12.0));
    wait_for_full_preview(&mut harness);
    let (_, reduction) = harness.state().test_preview_analysis().expect("a trace");
    assert!(reduction > 3.0, "{reduction}");
    assert!(harness.state_mut().test_inspector_apply());
    wait_until(&mut harness, "apply", 60, |h| {
        !h.state().test_editor_apply_busy()
    });
    let ceiling = 10f32.powf(-1.0 / 20.0);
    let after = harness.state().test_tab_channel_samples().unwrap();
    for ch in &after {
        assert!(peak(ch) <= ceiling + 1e-6, "{}", peak(ch));
        assert!(peak(ch) > ceiling * 0.9, "pushed up to the ceiling");
    }
}

#[test]
fn an_oversampled_limiter_keeps_what_it_applies_under_its_ceiling() {
    let mut harness = open(1.0, ToolKind::Limiter);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.limiter.input_gain_db = 12.0;
        st.limiter.oversampling = Oversampling::X4;
    }));
    wait_for_full_preview(&mut harness);
    assert!(harness.state_mut().test_inspector_apply());
    wait_until(&mut harness, "apply", 60, |h| {
        !h.state().test_editor_apply_busy()
    });
    let ceiling = 10f32.powf(-1.0 / 20.0);
    for ch in &harness.state().test_tab_channel_samples().unwrap() {
        assert!(peak(ch) <= ceiling + 1e-6, "{}", peak(ch));
        assert!(peak(ch) > ceiling * 0.9, "pushed up to the ceiling");
    }
}

#[test]
fn a_curve_moves_the_clipper_s_ceiling() {
    let mut harness = open(2.0, ToolKind::Clipper);
    let n = harness.state().test_tab_channel_samples().unwrap()[0].len();
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.clipper.input_gain_db = 12.0));
    assert!(harness.state_mut().test_set_automation(
        ToolKind::Clipper,
        "ceiling",
        vec![(n / 2 - 1, -1.0), (n / 2, -12.0)],
    ));
    wait_for_full_preview(&mut harness);
    // The row's quiet stretches (0.15, pushed to 0.6) pass a -1 dB ceiling
    // and are cut to a -12 dB one.
    let preview = harness.state().test_tool_preview_channels().unwrap();
    let first = peak(&preview[0][n / 8..n * 3 / 8]);
    let second = peak(&preview[0][n * 5 / 8..n * 7 / 8]);
    assert!(first > 0.5, "{first}");
    assert!(second <= 10f32.powf(-12.0 / 20.0) + 1e-6, "{second}");
}

#[test]
fn the_clipper_and_oversampling_survive_a_session() {
    let mut harness = open(1.0, ToolKind::Clipper);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.clipper.input_gain_db = 6.5;
        st.clipper.knee_db = 3.0;
        st.clipper.oversampling = Oversampling::X16;
        st.clipper.hard_ceiling = false;
        st.limiter.oversampling = Oversampling::X8;
    }));
    let saved = harness.state().test_tool_state().unwrap();
    let dir = std::env::temp_dir().join(format!("neowaves_clipper_session_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let session = dir.join("clipper.nwsess");
    assert!(harness.state_mut().test_save_session_to(&session));
    assert!(harness.state_mut().test_open_session_from(&session));
    wait_until(&mut harness, "tab back", 60, |h| {
        h.state().test_tool_state().is_some()
    });
    let back = harness.state().test_tool_state().unwrap();
    assert_eq!(back.clipper, saved.clipper);
    assert_eq!(back.limiter, saved.limiter);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Evidence for a person to look at: the Clipper with a soft knee driven
/// into its ceiling, and the Brickwall Limiter oversampled, each with the
/// whole panel on screen.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_the_clipper_and_the_oversampled_limiter() {
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("clipper");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    let save = |harness: &mut App, name: &str| {
        harness.run_steps(4);
        harness
            .render()
            .expect("render")
            .save(out_dir.join(name))
            .expect("save the screenshot");
    };

    let mut harness = open(6.0, ToolKind::Clipper);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.clipper.input_gain_db = 9.0;
        st.clipper.knee_db = 3.0;
    }));
    wait_for_full_preview(&mut harness);
    save(&mut harness, "01_clipper_soft_knee.png");
    drop(harness);

    let mut harness = open(6.0, ToolKind::Limiter);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.limiter.input_gain_db = 12.0;
        st.limiter.oversampling = Oversampling::X4;
    }));
    wait_for_full_preview(&mut harness);
    save(&mut harness, "02_limiter_4x.png");
}
