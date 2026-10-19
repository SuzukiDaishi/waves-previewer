//! The Reverb in the editor: it finds the IR library and its default IR,
//! previews and applies with the tail ringing on, takes an IR picked in its
//! browser and a tail mode picked on its panel, follows a Mix curve, and
//! keeps its settings (the IR included) through a session. Needs the
//! IR-Library submodule checked out (a debug build reads it from vendor/).
//! See `docs/INSPECTOR_SPEC.md` (Reverb).
#![cfg(feature = "kittest")]

use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use neowaves::app::ToolKind;
use neowaves::impulse::{IrId, IrRef};
use neowaves::kittest::harness_with_startup_size;
use neowaves::reverb::ReverbTail;
use neowaves::{StartupConfig, WavesPreviewer};

type App = Harness<'static, WavesPreviewer>;

const UNIT_IMPULSE: &str = "Utility/Unit Impulse.wav";

fn library_present() -> bool {
    let present = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("vendor/IR-Library/License.md")
        .is_file();
    if !present {
        eprintln!("vendor/IR-Library is not checked out: skipped");
    }
    present
}

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

/// A stereo virtual row `secs` long, open in the editor with the Reverb.
fn open(secs: f32) -> App {
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
    assert!(harness.state_mut().test_switch_tool(ToolKind::Reverb));
    wait_until(&mut harness, "the IR library and the IR", 60, |h| {
        h.state().test_ir_catalog_len().is_some() && h.state().test_reverb_ir_ready()
    });
    harness
}

/// How alike two stretches are, whatever their level: 1 when one is the
/// other scaled.
fn correlation(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let ea: f64 = a.iter().map(|x| f64::from(*x).powi(2)).sum();
    let eb: f64 = b.iter().map(|x| f64::from(*x).powi(2)).sum();
    dot / (ea * eb).sqrt().max(1e-30)
}

fn level_db(a: &[f32], b: &[f32]) -> f64 {
    let ea: f64 = a.iter().map(|x| f64::from(*x).powi(2)).sum();
    let eb: f64 = b.iter().map(|x| f64::from(*x).powi(2)).sum();
    10.0 * (ea / eb.max(1e-30)).log10()
}

fn wait_for_preview(harness: &mut App) {
    wait_until(harness, "the preview of the settings as they are", 120, |h| {
        h.state().test_tool_preview_current() && !h.state().test_preview_busy_for_active_tab()
    });
}

fn set_ir(harness: &mut App, rel: &str) {
    let id = IrId::of(&IrRef::Library(rel.to_string()));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.reverb.ir = Some(id)));
    wait_until(harness, "the IR", 60, |h| h.state().test_reverb_ir_ready());
}

#[test]
fn the_default_ir_rings_on_past_the_end_and_apply_writes_it() {
    if !library_present() {
        return;
    }
    let mut harness = open(1.0);
    assert!(
        harness.state().test_ir_catalog_len().unwrap() > 700,
        "the whole library"
    );
    let before = harness.state().test_tab_channel_samples().unwrap();
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.reverb.mix_pct = 50.0));
    wait_for_preview(&mut harness);
    let preview = harness.state().test_tool_preview_channels().unwrap();
    assert!(
        preview[0].len() > before[0].len(),
        "the tail runs past the end"
    );
    assert!(harness.state_mut().test_inspector_apply());
    wait_until(&mut harness, "apply", 60, |h| {
        !h.state().test_editor_apply_busy()
    });
    let after = harness.state().test_tab_channel_samples().unwrap();
    assert_eq!(after, preview, "Apply writes what was heard");
    assert!(after[1][before[1].len()..].iter().any(|v| v.abs() > 1e-4));
}

#[test]
fn an_ir_picked_in_the_browser_and_a_tail_picked_on_the_panel() {
    if !library_present() {
        return;
    }
    let mut harness = open(1.0);
    // Just the one: the "Unit Impulse Pitch Remap" folder sorts ahead of it,
    // and the list draws only the rows in view.
    harness
        .state_mut()
        .test_set_ir_query("utility unit impulse");
    harness.run_steps(3);
    harness.get_by_label("Unit Impulse").click();
    harness.run_steps(3);
    let st = harness.state().test_tool_state().unwrap();
    assert_eq!(
        st.reverb.ir.map(IrId::get),
        Some(IrRef::Library(UNIT_IMPULSE.to_string()))
    );
    wait_until(&mut harness, "the IR", 60, |h| {
        h.state().test_reverb_ir_ready()
    });
    harness.get_by_label("Stop at the end").click();
    harness.run_steps(3);
    assert_eq!(
        harness.state().test_tool_state().unwrap().reverb.tail,
        ReverbTail::Cut
    );
    // A unit impulse all wet, stopped at the end: the audio as it was.
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.reverb.mix_pct = 100.0));
    wait_for_preview(&mut harness);
    let original = harness.state().test_tab_channel_samples().unwrap();
    let preview = harness.state().test_tool_preview_channels().unwrap();
    assert_eq!(preview[0].len(), original[0].len());
    // The impulse is a 44.1 kHz file: at another rate it is a band-limited
    // pulse, so the same sound rather than the same samples.
    let n = original[1].len();
    let (a, b) = (&preview[1][n / 8..n / 2], &original[1][n / 8..n / 2]);
    assert!(correlation(a, b) > 0.99, "{}", correlation(a, b));
    assert!(level_db(a, b).abs() < 1.0, "{} dB", level_db(a, b));
}

#[test]
fn a_mix_curve_keeps_the_first_half_dry() {
    if !library_present() {
        return;
    }
    let mut harness = open(2.0);
    set_ir(&mut harness, UNIT_IMPULSE);
    let n = harness.state().test_tab_channel_samples().unwrap()[0].len();
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.reverb.tail = ReverbTail::Overlap;
        st.reverb.predelay_ms = 100.0;
    }));
    assert!(harness.state_mut().test_set_automation(
        ToolKind::Reverb,
        "mix",
        vec![(n / 2 - 1, 0.0), (n / 2, 100.0)],
    ));
    wait_for_preview(&mut harness);
    let original = harness.state().test_tab_channel_samples().unwrap();
    let preview = harness.state().test_tool_preview_channels().unwrap();
    assert_eq!(
        &preview[0][..n / 4],
        &original[0][..n / 4],
        "dry where Mix is 0"
    );
    // Wet only and 100 ms late where Mix is 100. Channel 1 (137.5 Hz) is
    // three quarters of a period out of step with itself 100 ms on, so only
    // the delayed audio correlates.
    let q = n * 17 / 20;
    let delay = n / 20; // 100 ms of a two-second row
    let wet = &preview[1][q..q + 2_000];
    let late = correlation(wet, &original[1][q - delay..q - delay + 2_000]);
    let now = correlation(wet, &original[1][q..q + 2_000]);
    assert!(late > 0.99, "{late}");
    assert!(now < 0.5, "{now}");
}

#[test]
fn the_reverb_and_its_ir_survive_a_session() {
    if !library_present() {
        return;
    }
    let mut harness = open(1.0);
    set_ir(&mut harness, UNIT_IMPULSE);
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.reverb.mix_pct = 42.0;
        st.reverb.high_cut_hz = 6_000.0;
        st.reverb.tail = ReverbTail::Overlap;
    }));
    let saved = harness.state().test_tool_state().unwrap();
    let dir = std::env::temp_dir().join(format!("neowaves_reverb_session_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let session = dir.join("reverb.nwsess");
    assert!(harness.state_mut().test_save_session_to(&session));
    let text = std::fs::read_to_string(&session).expect("the session");
    assert!(
        text.contains("lib:Utility/Unit Impulse.wav"),
        "the IR by its library path"
    );
    assert!(harness.state_mut().test_open_session_from(&session));
    wait_until(&mut harness, "tab back", 60, |h| {
        h.state().test_tool_state().is_some()
    });
    assert_eq!(
        harness.state().test_tool_state().unwrap().reverb,
        saved.reverb
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Evidence for a person to look at: the Reverb's panel with a hall, its
/// tail drawn past the end of the waveform.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_the_reverb() {
    if !library_present() {
        return;
    }
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("reverb");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    let mut harness = open(4.0);
    set_ir(
        &mut harness,
        "Rooms/Commercial/Hotels/Ballrooms/Royal Ballroom.wav",
    );
    assert!(harness.state_mut().test_change_tool_setting(|st| {
        st.reverb.mix_pct = 45.0;
        st.reverb.predelay_ms = 25.0;
        st.reverb.length_pct = 80.0;
    }));
    wait_for_preview(&mut harness);
    harness.run_steps(4);
    harness
        .render()
        .expect("render")
        .save(out_dir.join("01_reverb_ballroom.png"))
        .expect("save the screenshot");
}
