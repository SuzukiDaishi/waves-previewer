//! The editor's canvas column: the waveform on top, the mini meter on the
//! bottom edge with a splitter between them, and channel lanes that scroll
//! once there are more of them than fit. See `src/app/editor_layout.rs`.
//!
//! WAV decodes up to 26 channels (`test_samples/formats/README.md`), so the
//! file-backed test uses that many and the larger counts come from virtual
//! rows, which hold their samples in memory.
#![cfg(feature = "kittest")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{Modifiers, MouseWheelUnit};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use neowaves::app::ViewMode;
use neowaves::kittest::harness_with_startup_size;
use neowaves::{StartupConfig, WavesPreviewer};

type App = Harness<'static, WavesPreviewer>;

/// The lowest a lane is drawn before the lanes scroll (`MIN_LANE_H`).
const MIN_LANE_H: f32 = 44.0;
/// The meter's height before the splitter is touched (`MINI_METER_DEFAULT_H`).
const MINI_METER_DEFAULT_H: f32 = 180.0;
/// One mouse-wheel notch, in points.
const NOTCH: f32 = 120.0;

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "neowaves_editor_lanes_{tag}_{}_{now}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn wait_until(harness: &mut App, what: &str, secs: u64, mut done: impl FnMut(&mut App) -> bool) {
    let start = Instant::now();
    while !done(harness) {
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_tab(harness: &mut App) {
    wait_until(harness, "the tab", 60, |h| {
        h.state()
            .active_tab
            .and_then(|idx| h.state().tabs.get(idx))
            .is_some_and(|tab| tab.samples_len > 0 && !tab.loading)
    });
    // A few frames for the canvas, the meter and the lane geometry to settle.
    harness.run_steps(4);
}

/// The committed 26-channel fixture -- the most channels a WAV decodes --
/// opened in the editor from a folder of its own.
fn open_26_channels(tag: &str, size: egui::Vec2) -> (App, PathBuf) {
    let dir = temp_dir(tag);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test_samples")
        .join("formats")
        .join("ch_26.wav");
    let file = dir.join("ch_26.wav");
    std::fs::copy(&fixture, &file).expect("copy the 26-channel fixture");
    let cfg = StartupConfig {
        open_folder: Some(dir.clone()),
        open_first: false,
        ..StartupConfig::default()
    };
    let mut harness = harness_with_startup_size(cfg, size);
    wait_until(&mut harness, "the scan", 15, |h| {
        !h.state().scan_in_progress && !h.state().files.is_empty()
    });
    assert!(harness.state_mut().test_open_tab_for_path(&file));
    wait_for_tab(&mut harness);
    (harness, dir)
}

/// A virtual row of `channels` channels opened in the editor.
fn open_virtual(channels: usize, secs: f32, size: egui::Vec2) -> App {
    let mut harness = harness_with_startup_size(StartupConfig::default(), size);
    harness.run_steps(2);
    let path = harness
        .state_mut()
        .test_add_virtual_multichannel(channels, secs)
        .expect("a virtual row");
    assert!(harness.state_mut().test_open_tab_for_path(&path));
    wait_for_tab(&mut harness);
    harness
}

struct Lanes {
    lane_h: f32,
    count: usize,
    scroll: f32,
    max_scroll: f32,
    visible: std::ops::Range<usize>,
}

fn lanes(harness: &App) -> Lanes {
    let (lane_h, count, scroll, max_scroll, visible) = harness
        .state()
        .test_editor_lane_geometry()
        .expect("the lanes were drawn");
    Lanes {
        lane_h,
        count,
        scroll,
        max_scroll,
        visible,
    }
}

fn canvas(harness: &App) -> egui::Rect {
    harness
        .state()
        .test_editor_wave_canvas_rect()
        .expect("the waveform canvas")
}

fn meter(harness: &App) -> egui::Rect {
    harness
        .state()
        .test_editor_mini_meter_rect()
        .expect("the mini meter strip")
}

fn splitter(harness: &App) -> egui::Rect {
    harness
        .state()
        .test_editor_meter_splitter_rect()
        .expect("the meter splitter")
}

fn samples_per_px(harness: &App) -> f32 {
    harness.state().test_tab_samples_per_px().expect("a tab")
}

/// One wheel notch at `pos` (positive `dy` scrolls up), with `modifiers`
/// held, then enough frames for egui's smoothing to deliver all of it.
fn wheel_at(harness: &mut App, pos: egui::Pos2, dy: f32, modifiers: Modifiers) {
    harness.hover_at(pos);
    harness.run_steps(1);
    harness.event_modifiers(
        egui::Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            phase: egui::TouchPhase::Move,
            delta: egui::vec2(0.0, dy),
            modifiers,
        },
        modifiers,
    );
    harness.run_steps(30);
}

fn drag(harness: &mut App, from: egui::Pos2, to: egui::Pos2) {
    harness.hover_at(from);
    harness.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(1);
    let steps = 8;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        harness.event(egui::Event::PointerMoved(from + (to - from) * t));
        harness.run_steps(1);
    }
    harness.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(3);
}

fn double_click_at(harness: &mut App, pos: egui::Pos2) {
    for _ in 0..2 {
        harness.hover_at(pos);
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
    }
    harness.run_steps(3);
}

fn near(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn the_meter_sits_on_the_bottom_edge_and_the_waveform_takes_the_rest() {
    let mut harness = open_virtual(2, 2.0, egui::vec2(1280.0, 720.0));
    let (wave0, meter0, split0) = (canvas(&harness), meter(&harness), splitter(&harness));
    assert!(
        near(meter0.height(), MINI_METER_DEFAULT_H, 0.5),
        "{meter0:?}"
    );
    assert!(near(split0.bottom(), meter0.top(), 0.5));
    assert!(
        wave0.bottom() < split0.top(),
        "{wave0:?} runs into {split0:?}"
    );

    // Taller: the meter keeps its height on the bottom edge, and every
    // point of the new height goes to the waveform.
    harness.set_size(egui::vec2(1280.0, 1000.0));
    harness.run_steps(4);
    let (wave1, meter1) = (canvas(&harness), meter(&harness));
    assert!(near(meter1.height(), meter0.height(), 0.5), "{meter1:?}");
    assert!(
        near(meter1.bottom() - meter0.bottom(), 280.0, 1.0),
        "{meter1:?}"
    );
    assert!(
        near(wave1.height() - wave0.height(), 280.0, 1.0),
        "{wave0:?} -> {wave1:?}"
    );

    // Wider: the waveform's bottom edge and the meter stay where they are.
    // The waveform's height used to follow its width (`width * 0.35`), which
    // dragged the meter around with it. (Its top may still move, if the
    // toolbar above it wraps onto fewer rows.)
    harness.set_size(egui::vec2(1700.0, 1000.0));
    harness.run_steps(4);
    let (wave2, meter2) = (canvas(&harness), meter(&harness));
    assert!(wave2.width() > wave1.width() + 300.0);
    assert!(
        near(wave2.bottom(), wave1.bottom(), 1.0),
        "{wave1:?} -> {wave2:?}"
    );
    assert!(
        near(meter2.top(), meter1.top(), 1.0),
        "{meter1:?} -> {meter2:?}"
    );
    assert!(near(meter2.height(), meter1.height(), 0.5));
}

#[test]
fn the_splitter_trades_meter_height_for_waveform_height() {
    let mut harness = open_virtual(2, 2.0, egui::vec2(1280.0, 900.0));
    let (wave0, meter0) = (canvas(&harness), meter(&harness));
    let grip = splitter(&harness).center();
    drag(&mut harness, grip, grip - egui::vec2(0.0, 120.0));
    let (wave1, meter1) = (canvas(&harness), meter(&harness));
    assert!(
        near(meter1.height(), meter0.height() + 120.0, 2.0),
        "{meter0:?} -> {meter1:?}"
    );
    assert!(near(meter1.bottom(), meter0.bottom(), 0.5));
    assert!(
        near(wave1.height(), wave0.height() - 120.0, 2.0),
        "{wave0:?} -> {wave1:?}"
    );

    // Dragging far past either end stops at the limits, not beyond them.
    let grip = splitter(&harness).center();
    drag(&mut harness, grip, grip + egui::vec2(0.0, 2_000.0));
    assert!(
        near(meter(&harness).height(), 64.0, 0.5),
        "{:?}",
        meter(&harness)
    );

    // A double click puts it back.
    let grip = splitter(&harness).center();
    double_click_at(&mut harness, grip);
    harness.run_steps(2);
    assert!(
        near(meter(&harness).height(), MINI_METER_DEFAULT_H, 0.5),
        "{:?}",
        meter(&harness)
    );
}

#[test]
fn twenty_six_channels_keep_their_lane_height_and_scroll() {
    let (mut harness, dir) = open_26_channels("scroll", egui::vec2(1280.0, 720.0));
    let start = lanes(&harness);
    assert_eq!(start.count, 26);
    assert_eq!(
        start.lane_h, MIN_LANE_H,
        "lanes shrank instead of scrolling"
    );
    assert!(start.max_scroll > 0.0);
    assert_eq!(start.scroll, 0.0);
    assert_eq!(start.visible.start, 0);
    assert!(start.visible.end < 26);
    // The meter used to be crowded out by this many lanes.
    let wave = canvas(&harness);
    assert!(meter(&harness).top() > wave.bottom());
    let bar = harness
        .state()
        .test_editor_lane_scrollbar_rect()
        .expect("a scrollbar while the lanes overflow");
    assert!(wave.contains(bar.center()));

    // Alt+wheel over the waveform scrolls the lanes and does not zoom.
    let spp0 = samples_per_px(&harness);
    wheel_at(&mut harness, wave.center(), -NOTCH, Modifiers::ALT);
    let after_alt = lanes(&harness);
    assert!(after_alt.scroll > 0.0, "Alt+wheel did not scroll");
    assert_eq!(samples_per_px(&harness), spp0, "Alt+wheel zoomed");

    // The plain wheel over the left scale scrolls too.
    let gutter = egui::pos2(wave.left() + 12.0, wave.center().y);
    wheel_at(&mut harness, gutter, -NOTCH, Modifiers::NONE);
    let after_gutter = lanes(&harness);
    assert!(
        after_gutter.scroll > after_alt.scroll,
        "the wheel over the scale did not scroll"
    );
    assert_eq!(
        samples_per_px(&harness),
        spp0,
        "the wheel over the scale zoomed"
    );

    // The plain wheel over the waveform still zooms, and leaves the lanes be.
    wheel_at(&mut harness, wave.center(), NOTCH, Modifiers::NONE);
    assert_ne!(
        samples_per_px(&harness),
        spp0,
        "the plain wheel stopped zooming"
    );
    assert_eq!(lanes(&harness).scroll, after_gutter.scroll);

    // The thumb dragged past the bottom scrolls to the last lane, no further.
    let bar = harness.state().test_editor_lane_scrollbar_rect().unwrap();
    let now = lanes(&harness);
    let thumb_h = (bar.height() * wave.height() / (now.count as f32 * now.lane_h))
        .max(18.0)
        .min(bar.height());
    let thumb_top = bar.top() + (bar.height() - thumb_h) * now.scroll / now.max_scroll;
    let thumb_from = egui::pos2(bar.center().x, thumb_top + thumb_h * 0.5);
    let thumb_to = egui::pos2(bar.center().x, bar.bottom() + 200.0);
    let selection_before = harness.state().test_tab_selection();
    drag(&mut harness, thumb_from, thumb_to);
    // The scrollbar is on top of the canvas: the drag selected no audio.
    assert_eq!(harness.state().test_tab_selection(), selection_before);
    let end = lanes(&harness);
    assert!(
        near(end.scroll, end.max_scroll, 0.5),
        "scroll {} of {}",
        end.scroll,
        end.max_scroll
    );
    assert_eq!(end.visible.end, 26);
    // And the lanes keep their height down there.
    assert_eq!(end.lane_h, MIN_LANE_H);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_mixdown_is_one_lane_that_fills_the_canvas() {
    let (mut harness, dir) = open_26_channels("mixdown", egui::vec2(1280.0, 720.0));
    assert!(harness.state_mut().test_set_channel_view_mixdown());
    harness.run_steps(4);
    let mix = lanes(&harness);
    assert_eq!(mix.count, 1);
    assert!(near(mix.lane_h, canvas(&harness).height(), 0.5));
    assert_eq!(mix.max_scroll, 0.0);
    assert!(harness.state().test_editor_lane_scrollbar_rect().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hundreds_of_channels_draw_only_the_lanes_on_screen() {
    let mut harness = open_virtual(256, 0.5, egui::vec2(1280.0, 720.0));
    let top = lanes(&harness);
    assert_eq!(top.count, 256);
    assert_eq!(top.lane_h, MIN_LANE_H);
    let on_screen = (canvas(&harness).height() / MIN_LANE_H).ceil() as usize + 1;
    assert!(top.visible.len() <= on_screen, "{:?}", top.visible);
    assert!(
        meter(&harness).height() >= 64.0,
        "the meter was crowded out"
    );

    // To the middle, then to the end.
    harness
        .state_mut()
        .test_set_editor_lane_scroll(128.0 * MIN_LANE_H);
    harness.run_steps(3);
    let middle = lanes(&harness);
    assert_eq!(middle.visible.start, 128);
    harness.state_mut().test_set_editor_lane_scroll(f32::MAX);
    harness.run_steps(3);
    let end = lanes(&harness);
    assert_eq!(end.scroll, end.max_scroll);
    assert_eq!(end.visible.end, 256);
    assert!(end.visible.len() <= on_screen);
}

#[test]
fn the_channel_menus_scroll_instead_of_running_off_the_screen() {
    let mut harness = open_virtual(256, 0.25, egui::vec2(1280.0, 720.0));
    // The toolbar's menu button: the topmost of the "Channels" on screen.
    harness
        .query_all_by_label("Channels")
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .expect("the Channels menu button")
        .click();
    harness.run_steps(4);
    // 256 rows of checkboxes would put the menu's "Clear" thousands of
    // points down; every "Clear" on screen now has to be on the screen.
    let screen_h = 720.0;
    let clears: Vec<egui::Rect> = harness
        .query_all_by_label("Clear")
        .map(|node| node.rect())
        .collect();
    assert!(!clears.is_empty(), "the Channels menu did not open");
    for rect in clears {
        assert!(
            rect.bottom() <= screen_h,
            "a Clear is off the screen at {rect:?}"
        );
    }
}

#[test]
fn a_spectrogram_of_many_channels_renders_the_lanes_in_view() {
    let mut harness = open_virtual(128, 1.0, egui::vec2(1280.0, 720.0));
    assert!(harness
        .state_mut()
        .test_set_view_mode(ViewMode::Spectrogram));
    wait_until(&mut harness, "the spectrogram viewport", 120, |h| {
        h.state()
            .test_editor_viewport_window()
            .is_some_and(|(_, ready)| ready)
    });
    let lanes_top = lanes(&harness);
    let ((first, window, total), _) = harness.state().test_editor_viewport_window().unwrap();
    assert_eq!(total, 128);
    assert_eq!(first, 0);
    // The lanes on screen and one past the bottom edge, not all 128.
    assert!(
        window <= lanes_top.visible.len() + 2,
        "rendered {window} lanes"
    );

    // Scrolled to the middle, the next render follows.
    harness
        .state_mut()
        .test_set_editor_lane_scroll(64.0 * MIN_LANE_H);
    harness.run_steps(2);
    wait_until(&mut harness, "the scrolled viewport", 120, |h| {
        h.state()
            .test_editor_viewport_window()
            .is_some_and(|((first, _, _), ready)| ready && first == 63)
    });
}

/// Evidence for the layout, for a person to look at: 26 channels at the top
/// and the bottom of their scroll, 256 in the middle, 128 as a spectrogram,
/// and the meter after the splitter was dragged up.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_many_channel_lanes() {
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("editor_lanes");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    let save = |harness: &mut App, name: &str| {
        harness
            .render()
            .expect("render")
            .save(out_dir.join(name))
            .expect("save the screenshot");
    };

    let (mut harness, dir) = open_26_channels("render", egui::vec2(1400.0, 860.0));
    save(&mut harness, "01_26ch_top.png");
    harness.state_mut().test_set_editor_lane_scroll(f32::MAX);
    harness.run_steps(4);
    save(&mut harness, "02_26ch_bottom.png");
    let grip = splitter(&harness).center();
    drag(&mut harness, grip, grip - egui::vec2(0.0, 140.0));
    save(&mut harness, "03_26ch_meter_taller.png");
    drop(harness);
    let _ = std::fs::remove_dir_all(&dir);

    let mut harness = open_virtual(256, 0.5, egui::vec2(1400.0, 860.0));
    harness
        .state_mut()
        .test_set_editor_lane_scroll(120.0 * MIN_LANE_H + 17.0);
    harness.run_steps(4);
    save(&mut harness, "04_256ch_middle.png");
    drop(harness);

    let mut harness = open_virtual(128, 1.0, egui::vec2(1400.0, 860.0));
    assert!(harness
        .state_mut()
        .test_set_view_mode(ViewMode::Spectrogram));
    harness
        .state_mut()
        .test_set_editor_lane_scroll(40.0 * MIN_LANE_H);
    wait_until(&mut harness, "the spectrogram viewport", 120, |h| {
        h.state()
            .test_editor_viewport_window()
            .is_some_and(|(_, ready)| ready)
    });
    harness.run_steps(4);
    save(&mut harness, "05_128ch_spectrogram.png");
}
