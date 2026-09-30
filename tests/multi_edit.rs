//! Multi Edits: list rows laid out on tracks, played together, and mixed
//! down into a new list row. See `docs/MULTI_EDITS_SPEC.md`.
#![cfg(feature = "kittest")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use neowaves::app::SortKey;
use neowaves::kittest::harness_default;
use neowaves::WavesPreviewer;

type App = Harness<'static, WavesPreviewer>;

const FIXTURE_SR: u32 = 48_000;
const TONE_AMP: f32 = 0.4;

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let dir = std::env::temp_dir().join(format!(
        "neowaves_multi_edit_{tag}_{}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_tone(path: &Path, secs: f32) {
    let frames = (FIXTURE_SR as f32 * secs) as usize;
    let samples: Vec<f32> = (0..frames)
        .map(|i| (i as f32 / FIXTURE_SR as f32 * 330.0 * std::f32::consts::TAU).sin() * TONE_AMP)
        .collect();
    neowaves::wave::export_channels_audio(&[samples], FIXTURE_SR, path).expect("write tone");
}

fn wait_until(harness: &mut App, what: &str, mut done: impl FnMut(&mut App) -> bool) {
    let start = Instant::now();
    while !done(harness) {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A list holding these files with their lengths known -- what a drop needs.
fn list_with(files: &[PathBuf]) -> App {
    let mut harness = harness_default();
    harness.state_mut().test_replace_with_files(files);
    harness.run_steps(2);
    let files = files.to_vec();
    wait_until(&mut harness, "row metadata", |h| {
        files.iter().all(|f| h.state().test_meta_loaded(f))
    });
    harness
}

fn out_sr(harness: &App) -> f64 {
    harness.state().audio.shared.out_sample_rate.max(1) as f64
}

fn wait_for_mix(harness: &mut App) -> usize {
    let mut frames = 0;
    wait_until(harness, "the timeline mix", |h| {
        frames = h.state().test_multi_edit_mix_frames().unwrap_or(0);
        frames > 0
    });
    frames
}

#[test]
fn a_new_timeline_opens_as_a_workspace_with_one_track() {
    let mut harness = harness_default();
    harness.run_steps(2);
    // The menu bar has an Export of its own; the timeline adds one more.
    let exports_before = harness.query_all_by_label("Export").count();
    let id = harness.state_mut().test_multi_edit_new();
    harness.run_steps(3);
    assert!(harness.state().test_multi_edit_workspace_active());
    assert_eq!(harness.state().test_multi_edit_active_id(), Some(id));
    assert_eq!(
        harness.state().test_multi_edit_tracks(),
        vec![("Track 01".to_string(), false)]
    );
    // Not in any session yet: the tab carries the unsaved dot.
    assert!(harness.query_by_label("[\u{25CF} Multi Edit 1]").is_some(), "its tab");
    assert_eq!(harness.query_all_by_label("Export").count(), exports_before + 1);
    assert!(harness.query_by_label("\u{25B6} Play").is_some());
}

#[test]
fn rows_dropped_together_sit_back_to_back_and_play_as_one_mix() {
    let dir = temp_dir("drop");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    write_tone(&a, 1.0);
    write_tone(&b, 0.5);
    let mut harness = list_with(&[a.clone(), b.clone()]);
    harness.state_mut().test_multi_edit_new();
    let placed = harness
        .state_mut()
        .test_multi_edit_drop(Some(0), 0.25, &[a.clone(), b.clone()]);
    assert_eq!(placed, 2);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 2);
    assert_eq!((clips[0].0, &clips[0].1, clips[0].2), (0, &a, 0.25));
    assert_eq!((clips[1].0, &clips[1].1), (0, &b));
    assert!((clips[1].2 - 1.25).abs() < 1e-3, "b starts where a ends: {}", clips[1].2);

    let frames = wait_for_mix(&mut harness);
    let expected = (1.75 * out_sr(&harness)).round() as usize;
    assert!(frames.abs_diff(expected) <= 2, "{frames} vs {expected}");
    let silent = harness.state().test_multi_edit_mix_peak(0.0, 0.24).unwrap();
    let sounding = harness.state().test_multi_edit_mix_peak(0.5, 1.5).unwrap();
    assert!(silent < 1e-4, "nothing before the first clip: {silent}");
    assert!((sounding - TONE_AMP).abs() < 0.05, "the clips sound: {sounding}");

    // Space plays and stops the timeline, not the list.
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(1);
    assert!(harness.state().test_multi_edit_is_playing());
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(1);
    assert!(!harness.state().test_multi_edit_is_playing());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_gain_point_changes_what_is_heard() {
    let dir = temp_dir("gain");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a]);
    wait_for_mix(&mut harness);
    harness
        .state_mut()
        .test_multi_edit_add_gain_point(0, 0.0, -6.0206);
    wait_for_mix(&mut harness);
    let peak = harness.state().test_multi_edit_mix_peak(0.1, 0.9).unwrap();
    assert!((peak - TONE_AMP / 2.0).abs() < 0.02, "{peak}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_video_row_goes_to_a_video_track() {
    let dir = temp_dir("video");
    let a = dir.join("a.wav");
    write_tone(&a, 0.5);
    let video = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test_samples")
        .join("video")
        .join("video_sync_6s_30fps.mp4");
    let mut harness = list_with(&[a.clone(), video.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness
        .state_mut()
        .test_multi_edit_drop(Some(0), 0.0, &[a.clone(), video.clone()]);
    assert_eq!(
        harness.state().test_multi_edit_tracks(),
        vec![("Track 01".to_string(), false), ("Video 01".to_string(), true)]
    );
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 2);
    assert_eq!((clips[0].0, &clips[0].1), (0, &a));
    assert_eq!((clips[1].0, &clips[1].1), (1, &video));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_pane_is_the_list_itself() {
    let dir = temp_dir("pane");
    let (a, b, c) = (dir.join("alpha.wav"), dir.join("beta.wav"), dir.join("gamma.wav"));
    for path in [&a, &b, &c] {
        write_tone(path, 0.3);
    }
    let mut harness = list_with(&[a.clone(), b.clone(), c.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.run_steps(3);
    // The pane draws the list's rows.
    assert!(harness.query_by_label("beta.wav").is_some());

    // A filter set anywhere is the pane's filter too.
    harness
        .state_mut()
        .test_set_condition_filter(SortKey::File, "contains", "a", "")
        .unwrap();
    harness.run_steps(3);
    assert_eq!(
        harness.state().test_visible_list_paths(),
        vec![a.clone(), b.clone(), c.clone()],
        "all three contain an 'a'"
    );
    harness
        .state_mut()
        .test_set_condition_filter(SortKey::File, "begins with", "g", "")
        .unwrap();
    harness.run_steps(3);
    assert_eq!(harness.state().test_visible_list_paths(), vec![c.clone()]);
    assert!(harness.query_by_label("beta.wav").is_none(), "filtered out of the pane");

    // Clicking a row in the pane selects it in the list.
    harness.get_by_label("gamma.wav").click();
    harness.run_steps(2);
    assert_eq!(harness.state().test_selected_path(), Some(&c));
    assert!(harness.state().test_multi_edit_workspace_active(), "still on the timeline");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clicking_a_clip_selects_its_row() {
    let dir = temp_dir("select");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    write_tone(&a, 0.4);
    write_tone(&b, 0.4);
    let mut harness = list_with(&[a.clone(), b.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness
        .state_mut()
        .test_multi_edit_drop(Some(0), 0.0, &[a.clone(), b.clone()]);
    assert!(harness.state_mut().test_multi_edit_select_clip(1));
    assert_eq!(harness.state().test_selected_path(), Some(&b));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undo_takes_back_a_drop_and_redo_puts_it_back() {
    let dir = temp_dir("undo");
    let a = dir.join("a.wav");
    write_tone(&a, 0.4);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a]);
    assert_eq!(harness.state().test_multi_edit_clips().len(), 1);
    assert!(harness.state_mut().test_multi_edit_undo());
    assert_eq!(harness.state().test_multi_edit_clips().len(), 0);
    assert!(harness.state_mut().test_multi_edit_redo());
    assert_eq!(harness.state().test_multi_edit_clips().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_mixes_down_into_a_virtual_row() {
    let dir = temp_dir("export");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    write_tone(&a, 1.0);
    write_tone(&b, 0.5);
    let mut harness = list_with(&[a.clone(), b.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness
        .state_mut()
        .test_multi_edit_drop(Some(0), 0.0, &[a.clone(), b.clone()]);
    // The sources have to be read before a mixdown can start.
    wait_for_mix(&mut harness);
    harness.state_mut().test_multi_edit_export();
    assert!(harness.state().test_multi_edit_export_in_flight());
    wait_until(&mut harness, "the mixdown", |h| {
        !h.state().test_multi_edit_export_in_flight()
    });
    assert_eq!(harness.state().test_virtual_item_count(), 1);
    let row = harness
        .state()
        .items
        .iter()
        .find(|item| item.display_name == "Multi Edit 1.wav")
        .map(|item| item.path.clone())
        .expect("the mixdown's row");
    assert_eq!(harness.state().test_selected_path(), Some(&row), "and it is selected");
    // Its length is read from the file behind it.
    let mut secs = 0.0;
    wait_until(&mut harness, "the mixdown row's length", |h| {
        secs = h
            .state()
            .items
            .iter()
            .find(|item| item.path == row)
            .and_then(|item| item.meta.as_ref())
            .and_then(|meta| meta.duration_secs)
            .unwrap_or(0.0);
        secs > 0.0
    });
    assert!((secs - 1.5).abs() < 0.01, "{secs}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_timelines_survive_a_session_round_trip() {
    let dir = temp_dir("session");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    write_tone(&a, 0.6);
    write_tone(&b, 0.3);
    let session = dir.join("work.nwsess");
    {
        let mut harness = list_with(&[a.clone(), b.clone()]);
        let first = harness.state_mut().test_multi_edit_new();
        harness
            .state_mut()
            .test_multi_edit_drop(Some(0), 0.5, &[a.clone(), b.clone()]);
        harness
            .state_mut()
            .test_multi_edit_add_gain_point(0, 0.2, -3.0);
        let second = harness.state_mut().test_multi_edit_new();
        harness.state_mut().test_multi_edit_drop(None, 0.0, &[b.clone()]);
        // A closed tab keeps its timeline in the session.
        harness.state_mut().test_multi_edit_close(&second);
        harness.state_mut().test_multi_edit_open(&first);
        harness.run_steps(2);
        assert!(harness.state_mut().test_save_session_to(&session));
    }
    let mut harness = harness_default();
    harness.run_steps(2);
    assert!(harness.state_mut().test_open_session_from(&session));
    harness.run_steps(2);
    assert_eq!(
        harness.state().test_multi_edit_docs(),
        vec![
            ("Multi Edit 1".to_string(), true, 1, 2, 1),
            ("Multi Edit 2".to_string(), false, 2, 1, 0),
        ]
    );
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 2);
    assert_eq!((&clips[0].1, clips[0].2), (&a, 0.5), "paths resolve back to the files");
    assert_eq!(&clips[1].1, &b);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Press on `from`, move in steps to `to`, release: what a hand does.
fn drag(harness: &mut App, from: egui::Pos2, to: egui::Pos2) {
    drag_hold(harness, from, to);
    release(harness, to);
}

/// Press at `from` and move to `to`, still holding the button.
fn drag_hold(harness: &mut App, from: egui::Pos2, to: egui::Pos2) {
    harness.hover_at(from);
    harness.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(1);
    for step in 1..=6 {
        let t = step as f32 / 6.0;
        harness.event(egui::Event::PointerMoved(from + (to - from) * t));
        harness.run_steps(1);
    }
}

fn release(harness: &mut App, at: egui::Pos2) {
    harness.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.run_steps(2);
}

#[test]
fn a_row_dragged_from_the_pane_lands_on_the_track_under_the_pointer() {
    let dir = temp_dir("dnd");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    write_tone(&a, 2.0);
    write_tone(&b, 0.5);
    let mut harness = list_with(&[a.clone(), b.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.run_steps(3);

    let track = harness.get_by_label("Track 01").rect();
    // Well into the lane, right of the 150 px track header.
    let target = egui::pos2(track.left() + 300.0, track.center().y + 12.0);
    let row = harness.get_by_label("a.wav").rect().center();
    drag(&mut harness, row, target);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 1, "the drop landed");
    assert_eq!((clips[0].0, &clips[0].1), (0, &a));
    assert!(clips[0].2 > 0.0, "at the pointer, not at zero: {}", clips[0].2);

    // Dropped onto the clip that is now there: still this track.
    let row = harness.get_by_label("b.wav").rect().center();
    drag(&mut harness, row, target);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 2, "a drop onto a clip is a drop onto its track");
    assert!(clips.iter().all(|clip| clip.0 == 0), "{clips:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_preview_shows_the_video_under_the_playhead() {
    let dir = temp_dir("preview");
    let a = dir.join("a.wav");
    write_tone(&a, 0.5);
    let video = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test_samples")
        .join("video")
        .join("video_sync_6s_30fps.mp4");
    let mut harness = list_with(&[a.clone(), video.clone()]);
    harness.state_mut().test_multi_edit_new();
    // The video starts one second in.
    harness.state_mut().test_multi_edit_drop(None, 1.0, &[video.clone()]);
    harness.state_mut().test_multi_edit_seek(0.5);
    assert_eq!(harness.state().test_multi_edit_video_target(), None, "before the clip");
    harness.state_mut().test_multi_edit_seek(3.25);
    let (path, secs) = harness
        .state()
        .test_multi_edit_video_target()
        .expect("a video under the playhead");
    assert_eq!(path, video);
    assert!((secs - 2.25).abs() < 1e-9, "seconds into the file: {secs}");

    // The picture is decoded and shown (Media Foundation decodes the H.264
    // fixture; elsewhere this needs the `video` feature).
    if cfg!(windows) {
        harness.run_steps(2);
        wait_until(&mut harness, "a preview frame", |h| {
            h.state().test_multi_edit_video_shown_pts().is_some()
        });
        let pts = harness.state().test_multi_edit_video_shown_pts().unwrap();
        assert!((pts - 2.25).abs() < 0.1, "the frame at the playhead: {pts}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The timeline laid out like the design sketch, saved for a look at every
/// size and state the layout has to survive:
/// `debug/screenshot_verify/multi_edit/*.png`.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_multi_edit_timeline() {
    let dir = temp_dir("render");
    let names = ["aaaa.wav", "bbbb.wav", "cccc.wav", "dddd.wav", "eeee.wav", "ffff.wav"];
    let files: Vec<PathBuf> = names.iter().map(|n| dir.join(n)).collect();
    for (i, path) in files.iter().enumerate() {
        write_tone(path, 2.0 + i as f32);
    }
    let video = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test_samples")
        .join("video")
        .join("video_sync_6s_30fps.mp4");
    let mut all = files.clone();
    all.push(video.clone());
    let mut harness = list_with(&all);
    harness.set_size(egui::vec2(1600.0, 900.0));
    harness.state_mut().test_multi_edit_new();
    let state = harness.state_mut();
    // Track 01: two clips overlapping, which crossfade.
    state.test_multi_edit_drop(Some(0), 0.2, &[files[0].clone()]);
    state.test_multi_edit_drop(Some(0), 1.4, &[files[1].clone()]);
    state.test_multi_edit_drop(None, 5.0, &[files[1].clone()]);
    state.test_multi_edit_drop(None, 2.5, &[files[2].clone()]);
    state.test_multi_edit_rename_track(1, "A rather long track name that cannot fit");
    state.test_multi_edit_set_fades(3, 0.6, 1.2);
    for (secs, db) in [(1.0, -12.0), (2.0, 0.0), (3.0, 0.0), (4.0, -9.0), (5.0, -3.0), (8.0, -6.0)] {
        state.test_multi_edit_add_lane_point(2, "Gain", secs, db);
    }
    for (secs, st) in [(1.5, -6.0), (3.0, 5.0), (3.5, -8.0), (6.0, 7.0), (9.0, 0.0)] {
        state.test_multi_edit_add_lane_point(2, "Pitch", secs, st);
    }
    state.test_multi_edit_drop(None, 6.0, &[video.clone()]);
    state.test_multi_edit_set_fades(4, 0.4, 1.5);
    for (secs, db) in [(6.5, -20.0), (7.5, 0.0), (8.5, -6.0), (10.0, 0.0)] {
        state.test_multi_edit_add_lane_point(3, "Gain", secs, db);
    }
    state.test_multi_edit_add_marker_at(4.0);
    state.test_multi_edit_add_marker_at(9.5);
    // The picture window is an OS window in the app; in the harness it is
    // drawn over the page, so it is shut for these layout shots.
    state.test_multi_edit_set_show_video(3, false);
    state.test_multi_edit_seek(3.2);
    wait_for_mix(&mut harness);
    harness.run_steps(10);

    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("multi_edit");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    let shot = |harness: &mut App, name: &str| {
        harness.run_steps(4);
        harness
            .render()
            .expect("render the timeline")
            .save(out_dir.join(name))
            .expect("save the screenshot");
    };
    shot(&mut harness, "01_timeline_1600x900.png");

    // Lanes folded: their curves over the clips.
    harness.state_mut().test_multi_edit_set_lanes_collapsed(2, true);
    shot(&mut harness, "02_lanes_folded.png");
    harness.state_mut().test_multi_edit_set_lanes_collapsed(2, false);

    // Rows at their smallest, on a small window.
    harness.set_size(egui::vec2(1280.0, 720.0));
    harness.state_mut().test_multi_edit_set_row_zoom(0.5);
    shot(&mut harness, "03_rows_smallest_1280x720.png");

    // Tall rows on a large window.
    harness.set_size(egui::vec2(1920.0, 1080.0));
    harness.state_mut().test_multi_edit_set_row_zoom(1.6);
    shot(&mut harness, "04_rows_tall_1920x1080.png");

    // Rows held over Track 01, not yet dropped: two audio clips that would
    // land there, and a video clip that would go to the video track.
    harness.set_size(egui::vec2(1600.0, 900.0));
    harness.state_mut().test_multi_edit_set_row_zoom(1.0);
    harness.run_steps(4);
    assert!(harness
        .state_mut()
        .test_set_list_selection(&[files[3].clone(), files[4].clone(), video.clone()]));
    harness.run_steps(2);
    let track = harness.get_by_label("Track 01").rect();
    let target = egui::pos2(
        harness.state().test_multi_edit_x_for(3.0),
        track.center().y + 10.0,
    );
    let row = harness.get_by_label("dddd.wav").rect().center();
    drag_hold(&mut harness, row, target);
    harness.hover_at(target);
    harness.run_steps(2);
    assert!(harness.state().test_multi_edit_drop_preview().is_some());
    harness
        .render()
        .expect("render the drop preview")
        .save(out_dir.join("05_drop_preview.png"))
        .expect("save the screenshot");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A click at `pos`: press and release where it is.
fn click_at(harness: &mut App, pos: egui::Pos2) {
    harness.hover_at(pos);
    harness.run_steps(1);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);
    }
    harness.run_steps(1);
}

/// A spot on the first track's empty lane, far right, and a click there to
/// give the timeline the keys.
fn focus_timeline(harness: &mut App) {
    let track = harness.get_by_label("Track 01").rect();
    let x = harness.state().test_multi_edit_x_for(0.0) + 600.0;
    click_at(harness, egui::pos2(x, track.center().y + 16.0));
}

fn wheel(harness: &mut App, at: egui::Pos2, delta: egui::Vec2, modifiers: egui::Modifiers) {
    harness.hover_at(at);
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta,
        phase: egui::TouchPhase::Move,
        modifiers,
    });
    harness.run_steps(12);
}

#[test]
fn a_selected_track_is_deleted_with_the_delete_key() {
    let dir = temp_dir("del_track");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(None, 0.0, &[a]);
    harness.run_steps(3);
    assert_eq!(harness.state().test_multi_edit_tracks().len(), 2);
    focus_timeline(&mut harness);
    assert!(harness.state_mut().test_multi_edit_select_track(1));
    harness.key_press(egui::Key::Delete);
    harness.run_steps(2);
    assert_eq!(
        harness.state().test_multi_edit_tracks(),
        vec![("Track 01".to_string(), false)]
    );
    assert!(harness.state_mut().test_multi_edit_undo());
    assert_eq!(harness.state().test_multi_edit_tracks().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn arrows_move_the_playhead_and_alt_arrows_the_selected_clip() {
    let dir = temp_dir("arrows");
    let a = dir.join("a.wav");
    // Long enough that 40 px a second is not zoomed further out than the
    // timeline allows.
    write_tone(&a, 30.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.5, &[a]);
    // 40 px a second: the grid, and so an arrow step, is 2 s.
    harness.state_mut().test_multi_edit_set_zoom(40.0);
    harness.run_steps(3);
    focus_timeline(&mut harness);
    harness.state_mut().test_multi_edit_seek(0.0);
    harness.key_press(egui::Key::ArrowRight);
    harness.run_steps(2);
    assert!((harness.state().test_multi_edit_playhead() - 2.0).abs() < 1e-9);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowRight);
    harness.run_steps(2);
    let fine = harness.state().test_multi_edit_playhead();
    assert!((fine - 2.025).abs() < 1e-6, "Ctrl: one pixel's worth: {fine}");

    assert!(harness.state_mut().test_multi_edit_select_clip(0));
    harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::ArrowRight);
    harness.run_steps(2);
    let start = harness.state().test_multi_edit_clips()[0].2;
    assert!((start - 2.0).abs() < 1e-9, "the clip moved to the next grid line: {start}");
    assert!(harness.state_mut().test_multi_edit_undo());
    assert!((harness.state().test_multi_edit_clips()[0].2 - 0.5).abs() < 1e-9);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn m_adds_a_marker_at_the_playhead_and_the_session_keeps_it() {
    let dir = temp_dir("markers");
    let a = dir.join("a.wav");
    write_tone(&a, 2.0);
    let session = dir.join("m.nwsess");
    {
        let mut harness = list_with(&[a.clone()]);
        harness.state_mut().test_multi_edit_new();
        harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a.clone()]);
        harness.run_steps(3);
        focus_timeline(&mut harness);
        harness.state_mut().test_multi_edit_seek(1.5);
        harness.key_press(egui::Key::M);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_multi_edit_markers(),
            vec![(1.5, "M01".to_string())]
        );
        assert!(harness.state_mut().test_save_session_to(&session));
    }
    let mut harness = harness_default();
    harness.run_steps(2);
    assert!(harness.state_mut().test_open_session_from(&session));
    assert_eq!(
        harness.state().test_multi_edit_markers(),
        vec![(1.5, "M01".to_string())]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unsaved_timeline_carries_the_dot_until_the_session_holds_it() {
    let dir = temp_dir("dirty");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let session = dir.join("d.nwsess");
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.run_steps(2);
    assert!(harness.state().test_multi_edit_dirty(), "never saved into a session");
    assert!(harness.state_mut().test_save_session_to(&session));
    harness.run_steps(2);
    assert!(!harness.state().test_multi_edit_dirty());
    assert!(harness.query_by_label("[Multi Edit 1]").is_some());
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a]);
    harness.run_steps(2);
    assert!(harness.state().test_multi_edit_dirty(), "edited since");
    assert!(harness.query_by_label("[\u{25CF} Multi Edit 1]").is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_wheel_scrolls_time_and_shift_wheel_scrolls_the_tracks() {
    let dir = temp_dir("wheel");
    let a = dir.join("a.wav");
    write_tone(&a, 4.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a.clone()]);
    for start in [30.0, 10.0, 20.0, 5.0] {
        harness.state_mut().test_multi_edit_drop(None, start, &[a.clone()]);
    }
    harness.state_mut().test_multi_edit_set_zoom(40.0);
    harness.state_mut().test_multi_edit_set_row_zoom(3.0);
    harness.run_steps(3);
    let track = harness.get_by_label("Track 01").rect();
    let at = egui::pos2(harness.state().test_multi_edit_x_for(0.0) + 200.0, track.center().y);

    wheel(&mut harness, at, egui::vec2(0.0, -2.0), egui::Modifiers::NONE);
    let (_, scroll, scroll_y, _) = harness.state().test_multi_edit_view();
    assert!(scroll > 0.0, "the plain wheel moves along time: {scroll}");
    assert_eq!(scroll_y, 0.0, "and not down the tracks");

    // However hard it is spun, time stops with the end mid-view.
    for _ in 0..20 {
        wheel(&mut harness, at, egui::vec2(0.0, -50.0), egui::Modifiers::NONE);
    }
    let (_, far, _, _) = harness.state().test_multi_edit_view();
    assert!(far < 34.0, "scrolling stops before the end runs off: {far}");

    let (_, before, _, _) = harness.state().test_multi_edit_view();
    wheel(&mut harness, at, egui::vec2(0.0, -2.0), egui::Modifiers::SHIFT);
    let (_, after, scroll_y, zoom) = harness.state().test_multi_edit_view();
    let max_y = harness.state().test_multi_edit_max_scroll_y();
    assert!(
        scroll_y > 0.0,
        "Shift+wheel moves down the tracks: y {scroll_y} (max {max_y}, zoom {zoom}), time {before} -> {after}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stopping_returns_to_the_start_or_stays_as_the_editor_setting_says() {
    let dir = temp_dir("stop");
    let a = dir.join("a.wav");
    write_tone(&a, 4.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a]);
    wait_for_mix(&mut harness);

    harness.state_mut().test_set_return_to_last_start(true);
    harness.state_mut().test_multi_edit_seek(1.0);
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(1);
    assert!(harness.state().test_multi_edit_is_playing());
    harness.state_mut().test_set_transport_secs(2.5);
    harness.run_steps(1);
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(2);
    let back = harness.state().test_multi_edit_playhead();
    assert!((back - 1.0).abs() < 1e-3, "Return to last start: {back}");

    harness.state_mut().test_set_return_to_last_start(false);
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(1);
    harness.state_mut().test_set_transport_secs(3.0);
    harness.run_steps(1);
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(2);
    let stayed = harness.state().test_multi_edit_playhead();
    assert!((stayed - 3.0).abs() < 1e-3, "Continue from pause: {stayed}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn each_shown_video_track_gets_its_own_picture_window() {
    let video = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test_samples")
        .join("video")
        .join("video_sync_6s_30fps.mp4");
    let mut harness = list_with(&[video.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(None, 0.0, &[video.clone()]);
    harness.state_mut().test_multi_edit_drop(None, 0.0, &[video.clone()]);
    assert_eq!(
        harness.state().test_multi_edit_tracks(),
        vec![
            ("Track 01".to_string(), false),
            ("Video 01".to_string(), true),
            ("Video 02".to_string(), true),
        ]
    );
    harness.state_mut().test_multi_edit_seek(1.0);
    harness.run_steps(3);
    assert_eq!(harness.state().test_multi_edit_video_panel_count(), 2);
    harness.state_mut().test_multi_edit_set_show_video(1, false);
    harness.run_steps(3);
    assert_eq!(harness.state().test_multi_edit_video_panel_count(), 1);
}

#[test]
fn a_point_takes_a_typed_time_and_value() {
    let dir = temp_dir("point");
    let a = dir.join("a.wav");
    write_tone(&a, 2.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a]);
    harness.state_mut().test_multi_edit_add_gain_point(0, 1.0, -6.0);
    harness.state_mut().test_multi_edit_set_point(0, "Gain", 0, 1.5, -12.5);
    assert_eq!(
        harness.state().test_multi_edit_lane_points(0, "Gain"),
        vec![(1.5, -12.5)]
    );
    // Out of range is held to the lane's range.
    harness.state_mut().test_multi_edit_set_point(0, "Gain", 0, 1.5, 99.0);
    assert_eq!(harness.state().test_multi_edit_lane_points(0, "Gain")[0].1, 12.0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The spot `focus_timeline` clicks, from a track rect taken beforehand
/// (while a name field is open the name is not a label to find).
fn empty_lane_spot(harness: &App, track: egui::Rect) -> egui::Pos2 {
    let x = harness.state().test_multi_edit_x_for(0.0) + 600.0;
    egui::pos2(x, track.center().y + 16.0)
}

fn track_name(harness: &App) -> String {
    harness.state().test_multi_edit_tracks()[0].0.clone()
}

#[test]
fn a_name_field_lets_go_on_a_click_elsewhere_on_enter_and_on_escape() {
    let dir = temp_dir("rename");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.run_steps(3);
    let track = harness.get_by_label("Track 01").rect();

    // A click anywhere else takes the name as typed, and the timeline has
    // its keys back.
    harness.state_mut().test_multi_edit_begin_rename_track(0);
    harness.run_steps(2);
    harness.event(egui::Event::Text("X".into()));
    harness.run_steps(2);
    assert!(harness.state().test_multi_edit_renaming());
    let away = empty_lane_spot(&harness, track);
    click_at(&mut harness, away);
    assert!(!harness.state().test_multi_edit_renaming(), "a click elsewhere leaves the field");
    assert_eq!(track_name(&harness), "Track 01X");
    harness.run_steps(3);
    assert!(!harness.state().test_multi_edit_renaming(), "and it stays closed");
    harness.key_press(egui::Key::M);
    harness.run_steps(2);
    assert_eq!(harness.state().test_multi_edit_markers().len(), 1, "the keys are the timeline's again");

    // Enter takes it.
    harness.state_mut().test_multi_edit_begin_rename_track(0);
    harness.run_steps(2);
    harness.event(egui::Event::Text("Y".into()));
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert!(!harness.state().test_multi_edit_renaming());
    assert_eq!(track_name(&harness), "Track 01XY");

    // Escape leaves the name as it was.
    harness.state_mut().test_multi_edit_begin_rename_track(0);
    harness.run_steps(2);
    harness.event(egui::Event::Text("Z".into()));
    harness.run_steps(1);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(!harness.state().test_multi_edit_renaming());
    assert_eq!(track_name(&harness), "Track 01XY");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_enter_that_confirms_a_conversion_does_not_confirm_the_name() {
    let dir = temp_dir("rename_ime");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.run_steps(3);
    harness.state_mut().test_multi_edit_begin_rename_track(0);
    harness.run_steps(2);

    harness.event(egui::Event::Ime(egui::ImeEvent::Enabled));
    harness.event(egui::Event::Ime(egui::ImeEvent::Preedit("にほんご".into())));
    harness.run_steps(1);
    // An integration that lets the conversion's Enter through.
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert!(
        harness.state().test_multi_edit_renaming(),
        "the Enter that ends a conversion is the conversion's"
    );

    harness.event(egui::Event::Ime(egui::ImeEvent::Preedit(String::new())));
    harness.event(egui::Event::Ime(egui::ImeEvent::Commit("日本語".into())));
    harness.run_steps(2);
    assert!(harness.state().test_multi_edit_renaming(), "converted, still being named");
    harness.key_press(egui::Key::Enter);
    harness.run_steps(2);
    assert!(!harness.state().test_multi_edit_renaming(), "the next Enter takes the name");
    assert_eq!(track_name(&harness), "Track 01日本語");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_drag_from_the_list_shows_the_clips_it_would_place() {
    let dir = temp_dir("drop_preview");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    write_tone(&a, 2.0);
    write_tone(&b, 0.5);
    let mut harness = list_with(&[a.clone(), b.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.state_mut().test_multi_edit_seek(4.0);
    harness.run_steps(3);
    assert!(harness.state_mut().test_set_list_selection(&[a.clone(), b.clone()]));
    harness.run_steps(2);
    let track = harness.get_by_label("Track 01").rect();
    // The pointer puts the start at 1.55 s, near nothing; the end, 2.5 s on,
    // lands just past the playhead and is caught by it.
    let target = egui::pos2(
        harness.state().test_multi_edit_x_for(1.55),
        track.center().y + 12.0,
    );
    let row = harness.get_by_label("a.wav").rect().center();
    drag_hold(&mut harness, row, target);
    let (start, span, count) = harness
        .state()
        .test_multi_edit_drop_preview()
        .expect("the clips are shown while the rows are held over a track");
    assert_eq!(count, 2);
    assert!((span - 2.5).abs() < 1e-3, "both clips' length: {span}");
    assert!((start - 1.5).abs() < 1e-6, "the end snapped onto the playhead: {start}");
    assert!(harness.state().test_multi_edit_clips().is_empty(), "nothing placed yet");

    release(&mut harness, target);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 2, "{clips:?}");
    assert!((clips[0].2 - start).abs() < 1e-9, "placed where it was shown: {clips:?}");
    assert!((clips[1].2 - (start + 2.0)).abs() < 1e-3, "{clips:?}");
    assert_eq!(harness.state().test_multi_edit_drop_preview(), None, "gone with the drag");

    // A moved clip snaps by its end too: b (3.5 - 4.0 s) dragged 1.97 s
    // right puts its end at 5.97, which the grid line at 6 catches.
    let from = egui::pos2(harness.state().test_multi_edit_x_for(3.75), track.center().y + 12.0);
    let to = egui::pos2(harness.state().test_multi_edit_x_for(3.75 + 1.97), from.y);
    drag(&mut harness, from, to);
    let clips = harness.state().test_multi_edit_clips();
    assert!((clips[1].2 - 5.5).abs() < 1e-6, "the end caught by the grid: {clips:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn right_click_at(harness: &mut App, pos: egui::Pos2) {
    harness.hover_at(pos);
    harness.run_steps(1);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);
    }
    harness.run_steps(1);
}

fn paste(harness: &mut App) {
    harness.event(egui::Event::Paste(String::new()));
    harness.run_steps(2);
}

#[test]
fn ctrl_v_lays_copies_of_the_copied_clip_back_to_back() {
    let dir = temp_dir("copy_paste");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.5, &[a.clone()]);
    harness.run_steps(3);
    focus_timeline(&mut harness);
    assert!(harness.state_mut().test_multi_edit_select_clip(0));
    harness.event(egui::Event::Copy);
    harness.run_steps(2);
    assert_eq!(harness.state().test_multi_edit_clips().len(), 1, "a copy places nothing");

    harness.state_mut().test_multi_edit_seek(3.0);
    paste(&mut harness);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 2);
    assert_eq!((clips[1].0, &clips[1].1), (0, &a));
    assert!((clips[1].2 - 3.0).abs() < 1e-9 && (clips[1].3 - 1.0).abs() < 1e-3, "{clips:?}");
    let playhead = harness.state().test_multi_edit_playhead();
    assert!((playhead - 4.0).abs() < 1e-3, "the playhead moves to its end: {playhead}");

    // Again: right after the last one.
    paste(&mut harness);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 3);
    assert!((clips[2].2 - 4.0).abs() < 1e-3, "{clips:?}");

    // The Ctrl+V Windows reports through the keyboard hook (a clipboard
    // with no text), arriving with egui's own in the same frame: one paste.
    harness.state_mut().test_note_os_paste_key();
    paste(&mut harness);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 4, "one paste per press: {clips:?}");
    assert!((clips[3].2 - 5.0).abs() < 1e-3, "{clips:?}");
    harness.state_mut().test_note_os_paste_key();
    harness.run_steps(2);
    assert_eq!(harness.state().test_multi_edit_clips().len(), 5, "the hook alone pastes too");

    // Each paste is an undo step of its own.
    assert!(harness.state_mut().test_multi_edit_undo());
    assert_eq!(harness.state().test_multi_edit_clips().len(), 4);
    assert!(harness.state_mut().test_multi_edit_undo());
    assert_eq!(harness.state().test_multi_edit_clips().len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_x_cuts_and_a_video_clip_pastes_onto_a_video_track() {
    let dir = temp_dir("cut_paste");
    let a = dir.join("a.wav");
    write_tone(&a, 0.5);
    let video = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test_samples")
        .join("video")
        .join("video_sync_6s_30fps.mp4");
    let mut harness = list_with(&[a.clone(), video.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness
        .state_mut()
        .test_multi_edit_drop(Some(0), 0.0, &[a.clone(), video.clone()]);
    harness.run_steps(3);
    focus_timeline(&mut harness);

    // Cut the audio clip, paste it back elsewhere.
    assert!(harness.state_mut().test_multi_edit_select_clip(0));
    harness.event(egui::Event::Cut);
    harness.run_steps(2);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 1, "cut: {clips:?}");
    assert_eq!(clips[0].1, video);
    harness.state_mut().test_multi_edit_seek(2.0);
    paste(&mut harness);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 2);
    assert_eq!((clips[0].0, &clips[0].1), (0, &a), "back on its track: {clips:?}");
    assert!((clips[0].2 - 2.0).abs() < 1e-9);

    // A video clip goes to a video track, whatever track is selected.
    assert!(harness.state_mut().test_multi_edit_select_clip(1));
    harness.event(egui::Event::Copy);
    harness.run_steps(2);
    assert!(harness.state_mut().test_multi_edit_select_track(0));
    harness.state_mut().test_multi_edit_seek(8.0);
    paste(&mut harness);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 3);
    assert_eq!((clips[2].0, &clips[2].1), (1, &video), "{clips:?}");
    assert!((clips[2].2 - 8.0).abs() < 1e-9);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_clip_is_copied_and_pasted_from_the_right_click_menus() {
    let dir = temp_dir("copy_menu");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.state_mut().test_multi_edit_drop(Some(0), 0.5, &[a.clone()]);
    harness.run_steps(3);
    let track = harness.get_by_label("Track 01").rect();
    let on_clip = egui::pos2(harness.state().test_multi_edit_x_for(1.0), track.center().y + 12.0);
    right_click_at(&mut harness, on_clip);
    harness.get_by_label("Copy (Ctrl+C)").click();
    harness.run_steps(3);

    harness.state_mut().test_multi_edit_seek(4.0);
    harness.run_steps(2);
    let empty = egui::pos2(harness.state().test_multi_edit_x_for(6.0), on_clip.y);
    right_click_at(&mut harness, empty);
    harness.get_by_label("Paste at playhead (Ctrl+V)").click();
    harness.run_steps(3);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 2, "{clips:?}");
    assert!((clips[1].2 - 4.0).abs() < 1e-9, "at the playhead: {clips:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn arrows_stop_at_a_marker_on_the_way() {
    let dir = temp_dir("arrow_markers");
    let a = dir.join("a.wav");
    write_tone(&a, 30.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a]);
    // 40 px a second: an arrow step is 2 s.
    harness.state_mut().test_multi_edit_set_zoom(40.0);
    harness.state_mut().test_multi_edit_add_marker_at(0.7);
    harness.state_mut().test_multi_edit_add_marker_at(3.1);
    harness.run_steps(3);
    focus_timeline(&mut harness);
    harness.state_mut().test_multi_edit_seek(0.0);
    let mut stops = Vec::new();
    for key in [egui::Key::ArrowRight; 4] {
        harness.key_press(key);
        harness.run_steps(2);
        stops.push(harness.state().test_multi_edit_playhead());
    }
    for key in [egui::Key::ArrowLeft; 4] {
        harness.key_press(key);
        harness.run_steps(2);
        stops.push(harness.state().test_multi_edit_playhead());
    }
    let expected = [0.7, 2.0, 3.1, 4.0, 3.1, 2.0, 0.7, 0.0];
    assert!(
        stops.iter().zip(expected).all(|(got, want)| (got - want).abs() < 1e-9),
        "{stops:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_drop_near_the_playhead_lands_on_it() {
    let dir = temp_dir("snap_drop");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(40.0);
    harness.state_mut().test_multi_edit_seek(3.0);
    harness.run_steps(3);
    let track = harness.get_by_label("Track 01").rect();
    // Three pixels past the playhead, well inside the snap distance.
    let target = egui::pos2(
        harness.state().test_multi_edit_x_for(3.0) + 3.0,
        track.center().y + 12.0,
    );
    let row = harness.get_by_label("a.wav").rect().center();
    drag(&mut harness, row, target);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 1);
    assert!((clips[0].2 - 3.0).abs() < 1e-9, "snapped onto the playhead: {}", clips[0].2);
    let _ = std::fs::remove_dir_all(&dir);
}
