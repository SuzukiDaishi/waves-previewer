//! Automation of the EQ, Compressor and Limiter (curves drawn on the
//! waveform), the progressive preview that keeps a long file's dynamics
//! audible as settings move, and the deeper limiter. See
//! `docs/INSPECTOR_SPEC.md` and `src/app/tool_automation.rs`.
#![cfg(feature = "kittest")]

use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use neowaves::app::ToolKind;
use neowaves::kittest::harness_with_startup_size;
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

/// A stereo virtual row `secs` long, open in the editor with `tool` active.
fn open(secs: f32, tool: ToolKind) -> App {
    let mut harness =
        harness_with_startup_size(StartupConfig::default(), egui::vec2(1400.0, 860.0));
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

fn len(harness: &App) -> usize {
    harness.state().test_tab_channel_samples().unwrap()[0].len()
}

fn peak(v: &[f32]) -> f32 {
    v.iter().fold(0.0f32, |m, x| m.max(x.abs()))
}

fn rms(v: &[f32]) -> f32 {
    (v.iter().map(|x| x * x).sum::<f32>() / v.len().max(1) as f32).sqrt()
}

fn wait_for_full_preview(harness: &mut App) {
    wait_until(harness, "the full preview", 120, |h| {
        h.state().test_tool_preview_complete() == Some(true)
            && !h.state().test_preview_busy_for_active_tab()
    });
}

#[test]
fn a_long_file_previews_the_limiter_by_itself_around_the_playhead_first() {
    // 90 s: well past the length at which heavy tools used to wait for Refresh.
    let mut harness = open(90.0, ToolKind::Limiter);
    assert_eq!(
        harness.state().test_tool_session_manual(),
        Some(false),
        "a progressive tool previews automatically on a long file"
    );
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.limiter.input_gain_db = 30.0;
        st.limiter.ceiling_db = -20.0;
    }));
    let mut saw_partial = false;
    wait_until(&mut harness, "the window around the playhead", 60, |h| {
        saw_partial |= h.state().test_tool_preview_complete() == Some(false);
        h.state().test_tool_preview_complete().is_some()
    });
    assert!(saw_partial, "the window around the playhead came first");
    // Apply now, before the rest has arrived: the whole file is limited,
    // not only the window that was heard.
    assert!(harness.state_mut().test_inspector_apply());
    wait_until(&mut harness, "the apply", 120, |h| {
        !h.state().test_editor_apply_busy()
    });
    let after = harness.state().test_tab_channel_samples().unwrap();
    let ceiling = 10f32.powf(-20.0 / 20.0);
    let n = after[0].len();
    for ch in &after {
        assert!(peak(ch) <= ceiling + 1e-5, "{}", peak(ch));
        // Far past the window, too.
        assert!(
            peak(&ch[n * 3 / 4..]) > ceiling * 0.8,
            "the end was limited, not left quiet"
        );
    }
}

#[test]
fn the_limiter_goes_deeper_than_before() {
    let mut harness = open(2.0, ToolKind::Limiter);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.limiter.input_gain_db = 48.0;
        st.limiter.ceiling_db = -30.0;
    }));
    wait_for_full_preview(&mut harness);
    let preview = harness.state().test_tool_preview_channels().unwrap();
    let ceiling = 10f32.powf(-30.0 / 20.0);
    for ch in &preview {
        assert!(peak(ch) <= ceiling + 1e-5);
    }
    let st = harness.state().test_tool_state().unwrap();
    assert_eq!(
        (st.limiter.input_gain_db, st.limiter.ceiling_db),
        (48.0, -30.0)
    );
}

#[test]
fn a_threshold_curve_compresses_where_it_falls() {
    let mut harness = open(2.0, ToolKind::Compressor);
    let n = len(&harness);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.compressor.threshold_db = 0.0;
        st.compressor.ratio = 10.0;
        st.compressor.knee_db = 0.0;
    }));
    assert!(harness.state_mut().test_set_automation(
        ToolKind::Compressor,
        "threshold",
        vec![(n / 2 - 1, 0.0), (n / 2, -40.0)],
    ));
    wait_for_full_preview(&mut harness);
    let src = harness.state().test_tab_channel_samples().unwrap();
    let preview = harness.state().test_tool_preview_channels().unwrap();
    let q = n / 4;
    let change = |a: &[f32], b: &[f32]| 20.0 * (rms(b) / rms(a)).log10();
    let first = change(&src[0][q..2 * q], &preview[0][q..2 * q]);
    let second = change(&src[0][3 * q..], &preview[0][3 * q..]);
    assert!(
        first.abs() < 0.5,
        "untouched before the curve falls: {first}"
    );
    assert!(second < -10.0, "compressed after it: {second}");
    let (_, reduction) = harness.state().test_preview_analysis().expect("a trace");
    assert!(reduction > 10.0, "{reduction}");
}

#[test]
fn a_curve_is_drawn_on_the_waveform_and_undone_step_by_step() {
    let mut harness = open(2.0, ToolKind::Compressor);
    assert!(harness
        .state_mut()
        .test_select_automation(ToolKind::Compressor, "threshold"));
    harness.run_steps(3);
    let canvas = harness
        .state()
        .test_editor_wave_canvas_rect()
        .expect("the canvas");
    let at = canvas.center();
    // Two clicks in place add a point where they are.
    for _ in 0..2 {
        harness.hover_at(at);
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
    }
    harness.run_steps(3);
    let points = harness
        .state()
        .test_automation_points(ToolKind::Compressor, "threshold")
        .unwrap();
    assert_eq!(points.len(), 1, "{points:?}");
    // The canvas's middle is the middle of the threshold's range.
    assert!((points[0].1 - (-30.0)).abs() < 2.0, "{points:?}");
    // The point is a draft step: Ctrl+Z takes it back.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(3);
    let points = harness
        .state()
        .test_automation_points(ToolKind::Compressor, "threshold")
        .unwrap_or_default();
    assert!(points.is_empty(), "{points:?}");
}

#[test]
fn an_eq_band_takes_its_curves_with_it_when_it_goes() {
    let mut harness = open(2.0, ToolKind::Eq);
    let n = len(&harness);
    assert!(harness.state_mut().test_set_automation(
        ToolKind::Eq,
        "band2.freq",
        vec![(0, 200.0), (n, 8_000.0)],
    ));
    assert!(harness
        .state_mut()
        .test_set_automation(ToolKind::Eq, "band2.gain", vec![(0, 9.0)],));
    assert!(harness
        .state_mut()
        .test_set_automation(ToolKind::Eq, "output", vec![(0, -3.0)]));
    assert_eq!(harness.state().test_automation_lane_count(), 3);
    wait_for_full_preview(&mut harness);
    // The default band 2 is a bell at 0 dB: only its gain curve makes it act.
    let src = harness.state().test_tab_channel_samples().unwrap();
    let preview = harness.state().test_tool_preview_channels().unwrap();
    assert_ne!(src, preview);
    // Delete band 2: its lanes go, the output's stays.
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.eq.bands[1] = neowaves::eq::EqBand::default();
    }));
    harness.run_steps(3);
    // The panel prunes lanes of bands that are gone when it is drawn.
    assert_eq!(harness.state().test_automation_lane_count(), 1);
    assert!(harness
        .state()
        .test_automation_points(ToolKind::Eq, "output")
        .is_some());
}

/// Evidence for a person to look at: an EQ band swept by its curve, a
/// compressor's falling threshold, a deep limiter and a resonant high cut.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_automation_and_the_new_ranges() {
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("tool_automation");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    let save = |harness: &mut App, name: &str| {
        harness.run_steps(4);
        harness
            .render()
            .expect("render")
            .save(out_dir.join(name))
            .expect("save the screenshot");
    };

    // EQ: band 2 swept 200 Hz -> 8 kHz with a +9 dB gain, the graph at the
    // playhead mid-file.
    let mut harness = open(6.0, ToolKind::Eq);
    let n = len(&harness);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.eq.bands[1].gain_db = 9.0;
        st.eq.bands[1].q = 2.0;
    }));
    assert!(harness.state_mut().test_set_automation(
        ToolKind::Eq,
        "band2.freq",
        vec![(n / 10, 200.0), (n / 2, 2_000.0), (n * 9 / 10, 8_000.0)],
    ));
    harness.state_mut().test_audio_seek_to_sample(n / 2);
    wait_for_full_preview(&mut harness);
    save(&mut harness, "01_eq_band_sweep.png");
    drop(harness);

    // Compressor: the threshold falls half way through.
    let mut harness = open(6.0, ToolKind::Compressor);
    let n = len(&harness);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.compressor.threshold_db = -6.0;
        st.compressor.ratio = 8.0;
    }));
    assert!(harness.state_mut().test_set_automation(
        ToolKind::Compressor,
        "threshold",
        vec![(n / 4, -6.0), (n * 3 / 4, -40.0)],
    ));
    wait_for_full_preview(&mut harness);
    save(&mut harness, "02_comp_threshold_curve.png");
    drop(harness);

    // Limiter: driven deep into a low ceiling.
    let mut harness = open(6.0, ToolKind::Limiter);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.limiter.input_gain_db = 40.0;
        st.limiter.ceiling_db = -20.0;
    }));
    wait_for_full_preview(&mut harness);
    save(&mut harness, "03_limiter_deep.png");
    drop(harness);

    // A resonant high cut.
    let mut harness = open(6.0, ToolKind::Eq);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.eq.bands[2] = neowaves::eq::EqBand {
            enabled: true,
            kind: neowaves::eq::EqKind::HighCut,
            freq_hz: 3_000.0,
            slope: neowaves::eq::CutSlope::Db24,
            cut_q: 4.0,
            ..neowaves::eq::EqBand::default()
        };
    }));
    wait_for_full_preview(&mut harness);
    save(&mut harness, "04_resonant_high_cut.png");
}
