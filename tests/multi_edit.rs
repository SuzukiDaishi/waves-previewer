//! Multi Edits: list rows laid out on tracks, played together, and mixed
//! down into a new list row. See `docs/MULTI_EDITS_SPEC.md`.
#![cfg(feature = "kittest")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui_kittest::kittest::{NodeT, Queryable};
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
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_multi_edit_split_channels() {
    let dir = temp_dir("render_split");
    let surround = dir.join("surround.wav");
    let voice = dir.join("voice.wav");
    write_surround(&surround, 4.0);
    write_tone(&voice, 2.0);
    let mut harness = list_with(&[surround.clone(), voice.clone()]);
    harness.set_size(egui::vec2(1600.0, 900.0));
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(120.0);
    harness.state_mut().test_multi_edit_drop(Some(0), 0.5, &[surround.clone()]);
    harness.state_mut().test_multi_edit_drop(Some(0), 5.0, &[voice.clone()]);
    for (secs, pan) in [(5.0, -1.0), (7.0, 1.0)] {
        harness.state_mut().test_multi_edit_add_lane_point(0, "Pan", secs, pan);
    }
    wait_for_mix(&mut harness);
    harness.run_steps(4);

    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("multi_edit");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    let save = |harness: &mut App, name: &str| {
        harness
            .render()
            .expect("render the timeline")
            .save(out_dir.join(name))
            .expect("save the screenshot");
    };
    let track = harness.get_by_label("Track 01").rect();
    let on_clip = egui::pos2(harness.state().test_multi_edit_x_for(2.0), track.center().y + 12.0);
    right_click_at(&mut harness, on_clip);
    save(&mut harness, "10_clip_menu_split_channels.png");
    harness.get_by_label("Split into channels (6)").click();
    harness.run_steps(3);
    harness.hover_at(egui::pos2(800.0, 880.0));
    harness.run_steps(4);
    save(&mut harness, "11_split_into_six_tracks.png");

    // Track 01's voice sent to the centre in mono. The timeline is 5.1 now,
    // so its Pan lane turns it round the listener (the header says L180°).
    let chip = harness.get_by_label("St").rect();
    click_at(&mut harness, chip.center());
    harness.run_steps(2);
    save(&mut harness, "12_output_chip_menu.png");
    harness.get_by_label("Mono \u{2192} C").click();
    harness.run_steps(4);
    save(&mut harness, "13_mono_track_pan_turns.png");

    // Knobs turned on two of the split tracks, a fader down, one solo: the
    // header names the solo.
    harness.state_mut().test_multi_edit_set_track_mix(1, -6.0, -0.5, false);
    harness.state_mut().test_multi_edit_set_track_mix(2, 0.0, 0.25, true);
    harness.run_steps(4);
    save(&mut harness, "14_pan_knobs_and_solo.png");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_solo_is_named_in_the_header_even_when_its_row_is_too_short_to_show_it() {
    let dir = temp_dir("solo");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    write_tone(&a, 1.0);
    write_tone(&b, 1.0);
    let mut harness = list_with(&[a.clone(), b.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a.clone()]);
    harness.state_mut().test_multi_edit_drop(None, 2.0, &[b.clone()]);
    let tracks: Vec<usize> = harness.state().test_multi_edit_clips().iter().map(|c| c.0).collect();
    assert_eq!(tracks, vec![0, 1], "one clip on each track");
    // Rows at their lowest: one line, but Mute and Solo stay on it.
    harness.state_mut().test_multi_edit_set_row_zoom(0.1);
    harness.state_mut().test_multi_edit_set_track_mix(1, 0.0, 0.0, true);
    harness.run_steps(3);
    assert!(harness.query_by_label("SOLO \u{d7}1").is_some(), "the header says a solo is on");
    wait_for_mix(&mut harness);
    let first = harness.state().test_multi_edit_mix_peak(0.1, 0.9).unwrap();
    let second = harness.state().test_multi_edit_mix_peak(2.1, 2.9).unwrap();
    assert!(first < 1e-6, "the track that is not soloed is silent: {first}");
    assert!((second - TONE_AMP).abs() < 0.05, "{second}");

    harness.get_by_label("SOLO \u{d7}1").click();
    harness.run_steps(2);
    assert_eq!(harness.state().test_multi_edit_solos(), vec![false, false]);
    assert!(harness.query_by_label("SOLO \u{d7}1").is_none());
    wait_for_mix(&mut harness);
    let first = harness.state().test_multi_edit_mix_peak(0.1, 0.9).unwrap();
    assert!((first - TONE_AMP).abs() < 0.05, "both tracks again: {first}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn picking_a_row_in_the_pane_does_not_take_over_a_playing_timeline() {
    let dir = temp_dir("pane_pick");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    write_tone(&a, 2.0);
    write_tone(&b, 1.0);
    let mut harness = list_with(&[a.clone(), b.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a.clone()]);
    wait_for_mix(&mut harness);
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(1);
    assert!(harness.state().test_multi_edit_is_playing());
    // Picking `b` selects it; it does not swap the timeline's mix for it.
    assert!(harness.state_mut().test_select_and_load_row(1));
    harness.run_steps(2);
    assert!(harness.state().test_multi_edit_is_playing(), "the timeline still plays");
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(1);
    let _ = std::fs::remove_dir_all(&dir);
}

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

/// Six channels of one tone, channel N at `(N + 1) / 10`: which channel
/// ended up where shows in its level.
fn write_surround(path: &Path, secs: f32) {
    let frames = (FIXTURE_SR as f32 * secs) as usize;
    let channels: Vec<Vec<f32>> = (0..6)
        .map(|ch| {
            let amp = (ch + 1) as f32 / 10.0;
            (0..frames)
                .map(|i| (i as f32 / FIXTURE_SR as f32 * 330.0 * std::f32::consts::TAU).sin() * amp)
                .collect()
        })
        .collect();
    neowaves::wave::export_channels_audio(&channels, FIXTURE_SR, path).expect("write 5.1");
}

fn assert_channel_levels(peaks: &[f32]) {
    assert_eq!(peaks.len(), 6, "{peaks:?}");
    for (ch, peak) in peaks.iter().enumerate() {
        let want = (ch + 1) as f32 / 10.0;
        assert!((peak - want).abs() < 0.02, "channel {ch}: {peak} for {want} ({peaks:?})");
    }
}

#[test]
fn a_surround_clip_splits_into_a_track_per_speaker_and_mixes_back_to_its_channels() {
    let dir = temp_dir("split_channels");
    let a = dir.join("surround.wav");
    write_surround(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.state_mut().test_multi_edit_drop(Some(0), 0.5, &[a.clone()]);
    // The menu knows the channel count once the source is read.
    wait_for_mix(&mut harness);
    assert_eq!(harness.state().test_multi_edit_output().as_deref(), Some("FL,FR"));
    harness.run_steps(2);
    let track = harness.get_by_label("Track 01").rect();
    let on_clip = egui::pos2(harness.state().test_multi_edit_x_for(1.0), track.center().y + 12.0);
    right_click_at(&mut harness, on_clip);
    harness.get_by_label("Split into channels (6)").click();
    harness.run_steps(3);

    let names: Vec<String> = harness
        .state()
        .test_multi_edit_tracks()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        [
            "Track 01",
            "Track 01 \u{b7} L",
            "Track 01 \u{b7} R",
            "Track 01 \u{b7} C",
            "Track 01 \u{b7} LFE",
            "Track 01 \u{b7} Ls",
            "Track 01 \u{b7} Rs",
        ]
    );
    assert_eq!(
        harness.state().test_multi_edit_track_outputs(),
        ["St", "L", "R", "C", "LFE", "Ls", "Rs"]
    );
    assert_eq!(
        harness.state().test_multi_edit_clip_channels(),
        (0..6).map(Some).collect::<Vec<_>>()
    );
    // A stereo timeline could not keep the channels apart: it is 5.1 now.
    assert_eq!(
        harness.state().test_multi_edit_output().as_deref(),
        Some("FL,FR,FC,LFE,BL,BR")
    );
    // A combo box carries its choice as its value, not its label.
    assert!(
        harness
            .query_all_by_value("5.1 WAV / SMPTE")
            .any(|node| node.accesskit_node().role() == egui::accesskit::Role::ComboBox),
        "the Output combo says so"
    );
    wait_for_mix(&mut harness);
    assert_channel_levels(&harness.state().test_multi_edit_mix_channel_peaks().unwrap());

    // Playing it tells the device which speaker each channel is.
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(2);
    assert_eq!(
        harness.state().test_playback_source_layout().as_deref(),
        Some("FL,FR,FC,LFE,BL,BR")
    );
    harness.state_mut().test_request_workspace_play_toggle();
    harness.run_steps(1);

    // A track's chip sends it elsewhere: the LFE's channel onto the centre.
    let lfe_chip = harness.get_by_label("LFE").rect();
    click_at(&mut harness, lfe_chip.center());
    harness.get_by_label("Mono \u{2192} C").click();
    harness.run_steps(3);
    assert_eq!(harness.state().test_multi_edit_track_outputs()[4], "C");
    wait_for_mix(&mut harness);
    let peaks = harness.state().test_multi_edit_mix_channel_peaks().unwrap();
    assert!((peaks[2] - 0.7).abs() < 0.03, "C carries 0.3 + 0.4: {peaks:?}");
    assert!(peaks[3] < 1e-4, "and the LFE nothing: {peaks:?}");
    assert!(harness.state_mut().test_multi_edit_undo());
    harness.run_steps(2);
    assert_eq!(harness.state().test_multi_edit_track_outputs()[4], "LFE");

    // One undo step takes the split back whole.
    assert!(harness.state_mut().test_multi_edit_undo());
    harness.run_steps(2);
    assert_eq!(harness.state().test_multi_edit_tracks().len(), 1);
    assert_eq!(harness.state().test_multi_edit_output().as_deref(), Some("FL,FR"));
    assert!(harness.state_mut().test_multi_edit_redo());
    harness.run_steps(2);
    assert_eq!(harness.state().test_multi_edit_tracks().len(), 7);

    // The mixdown keeps the six channels.
    wait_for_mix(&mut harness);
    harness.state_mut().test_multi_edit_export();
    wait_until(&mut harness, "the mixdown", |h| {
        !h.state().test_multi_edit_export_in_flight()
    });
    let row = harness
        .state()
        .items
        .iter()
        .find(|item| item.display_name == "Multi Edit 1.wav")
        .map(|item| item.path.clone())
        .expect("the mixdown's row");
    let mut channels = 0;
    wait_until(&mut harness, "the mixdown row's channels", |h| {
        channels = h
            .state()
            .items
            .iter()
            .find(|item| item.path == row)
            .and_then(|item| item.meta.as_ref())
            .map(|meta| meta.channels)
            .unwrap_or(0);
        channels > 0
    });
    assert_eq!(channels, 6);

    // And a session keeps the outputs and the clips' channels.
    let session = dir.join("split.nwsess");
    assert!(harness.state_mut().test_save_session_to(&session));
    let mut reopened = harness_default();
    reopened.run_steps(2);
    assert!(reopened.state_mut().test_open_session_from(&session));
    reopened.run_steps(2);
    assert_eq!(
        reopened.state().test_multi_edit_track_outputs(),
        ["St", "L", "R", "C", "LFE", "Ls", "Rs"]
    );
    assert_eq!(
        reopened.state().test_multi_edit_output().as_deref(),
        Some("FL,FR,FC,LFE,BL,BR")
    );
    assert_eq!(
        reopened.state().test_multi_edit_clip_channels(),
        (0..6).map(Some).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_mono_source_offers_no_split() {
    let dir = temp_dir("split_mono");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.state_mut().test_multi_edit_drop(Some(0), 0.5, &[a.clone()]);
    wait_for_mix(&mut harness);
    harness.run_steps(2);
    let track = harness.get_by_label("Track 01").rect();
    let on_clip = egui::pos2(harness.state().test_multi_edit_x_for(1.0), track.center().y + 12.0);
    right_click_at(&mut harness, on_clip);
    let item = harness.get_by_label("Split into channels");
    assert!(item.accesskit_node().is_disabled(), "a mono source has one channel to give");
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

fn click_with(harness: &mut App, pos: egui::Pos2, modifiers: egui::Modifiers) {
    harness.hover_at(pos);
    harness.run_steps(1);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        });
        harness.run_steps(1);
    }
    harness.run_steps(1);
}

/// A file with a wave's name and no wave in it: its length is never known.
fn write_unreadable(path: &Path) {
    std::fs::write(path, b"not a wave file at all").expect("write the unreadable file");
}

/// A list holding these files, waiting only for `readable` to be measured.
fn list_waiting_for(files: &[PathBuf], readable: &[PathBuf]) -> App {
    let mut harness = harness_default();
    harness.state_mut().test_replace_with_files(files);
    harness.run_steps(2);
    let readable = readable.to_vec();
    wait_until(&mut harness, "row metadata", |h| {
        readable.iter().all(|f| h.state().test_meta_loaded(f))
    });
    harness
}

#[test]
fn up_and_down_pick_a_track_and_bring_it_into_view() {
    let dir = temp_dir("track_keys");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.set_size(egui::vec2(1280.0, 560.0));
    harness.state_mut().test_multi_edit_new();
    for _ in 0..7 {
        harness.state_mut().test_multi_edit_drop(None, 0.0, &[a.clone()]);
    }
    harness.state_mut().test_multi_edit_set_row_zoom(2.0);
    harness.run_steps(4);
    assert_eq!(harness.state().test_multi_edit_tracks().len(), 8);
    assert!(harness.state().test_multi_edit_max_scroll_y() > 0.0, "the rows overflow the view");
    focus_timeline(&mut harness);
    let selected = |h: &App| h.state().test_multi_edit_selected_track();
    let scroll_y = |h: &App| h.state().test_multi_edit_view().2;

    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(3);
    assert_eq!(selected(&harness), Some(0), "down from nothing: the first track");
    for _ in 0..10 {
        harness.key_press(egui::Key::ArrowDown);
        harness.run_steps(2);
    }
    harness.run_steps(3);
    assert_eq!(selected(&harness), Some(7), "held at the last");
    assert!(scroll_y(&harness) > 0.0, "scrolled down to it");
    for _ in 0..10 {
        harness.key_press(egui::Key::ArrowUp);
        harness.run_steps(2);
    }
    harness.run_steps(3);
    assert_eq!(selected(&harness), Some(0), "held at the first");
    assert!(scroll_y(&harness) < 1.0, "scrolled back up: {}", scroll_y(&harness));

    // From a selected clip, up and down start at its track.
    assert!(harness.state_mut().test_multi_edit_select_clip(2));
    harness.key_press(egui::Key::ArrowUp);
    harness.run_steps(3);
    assert_eq!(selected(&harness), Some(2), "the clip's track is 3; up is 2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rows_placed_before_their_lengths_are_read_close_up_when_they_are() {
    let dir = temp_dir("unread");
    let (a, b, c) = (dir.join("a.wav"), dir.join("b.wav"), dir.join("c.wav"));
    write_tone(&a, 1.0);
    write_tone(&b, 0.5);
    write_tone(&c, 2.0);
    let mut harness = list_with(&[a.clone(), b.clone(), c.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.run_steps(2);
    assert_eq!(
        harness
            .state_mut()
            .test_multi_edit_drop_unread(0, 0.5, &[a.clone(), b.clone(), c.clone()]),
        3,
        "placed without lengths"
    );
    assert_eq!(harness.state().test_multi_edit_pending_clips(), 3);
    let clips = harness.state().test_multi_edit_clips();
    assert!(clips.iter().all(|clip| clip.2 == 0.5 && clip.3 == 0.0), "starts only: {clips:?}");

    harness.run_steps(3);
    assert_eq!(harness.state().test_multi_edit_pending_clips(), 0);
    let clips = harness.state().test_multi_edit_clips();
    let got: Vec<(f64, f64)> = clips.iter().map(|clip| (clip.2, clip.3)).collect();
    let want = [(0.5, 1.0), (1.5, 0.5), (2.0, 2.0)];
    assert!(
        got.iter()
            .zip(want)
            .all(|(g, w)| (g.0 - w.0).abs() < 1e-3 && (g.1 - w.1).abs() < 1e-3),
        "back to back once read: {got:?}"
    );
    // Reading a length is not an edit: one undo takes the whole drop back.
    assert!(harness.state_mut().test_multi_edit_undo());
    assert!(harness.state().test_multi_edit_clips().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_row_whose_length_is_not_known_still_lands_as_its_start() {
    let dir = temp_dir("unknown_len");
    let (a, broken) = (dir.join("a.wav"), dir.join("broken.wav"));
    write_tone(&a, 1.0);
    write_unreadable(&broken);
    let mut harness = list_waiting_for(&[a.clone(), broken.clone()], &[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.run_steps(3);
    let track = harness.get_by_label("Track 01").rect();
    let target = egui::pos2(harness.state().test_multi_edit_x_for(2.0) + 1.0, track.center().y + 12.0);
    let row = harness.get_by_label("broken.wav").rect().center();
    drag_hold(&mut harness, row, target);
    let (_, span, count) = harness
        .state()
        .test_multi_edit_drop_preview()
        .expect("shown while held over the track");
    assert_eq!((span, count), (0.0, 1), "a start, no length");
    release(&mut harness, target);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 1, "placed, not refused: {clips:?}");
    assert!((clips[0].2 - 2.0).abs() < 0.02 && clips[0].3 == 0.0, "{clips:?}");
    harness.run_steps(20);
    assert_eq!(harness.state().test_multi_edit_pending_clips(), 1, "still waiting");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn where_clips_overlap_the_one_that_starts_later_lies_on_top() {
    let dir = temp_dir("overlap_top");
    let (a, b) = (dir.join("a.wav"), dir.join("b.wav"));
    write_tone(&a, 2.0);
    write_tone(&b, 2.0);
    let mut harness = list_with(&[a.clone(), b.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    // b goes down first, so it is first on the track; a starts earlier.
    harness.state_mut().test_multi_edit_drop(Some(0), 1.0, &[b.clone()]);
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a.clone()]);
    harness.run_steps(3);
    let track = harness.get_by_label("Track 01").rect();
    let y = track.center().y + 12.0;
    let (over, before) = (
        egui::pos2(harness.state().test_multi_edit_x_for(1.5), y),
        egui::pos2(harness.state().test_multi_edit_x_for(0.5), y),
    );
    click_at(&mut harness, over);
    assert_eq!(harness.state().test_selected_path(), Some(&b), "the later clip is on top");
    click_at(&mut harness, before);
    assert_eq!(harness.state().test_selected_path(), Some(&a));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_cut_tool_splits_a_clip_where_it_is_clicked() {
    let dir = temp_dir("cut_tool");
    let a = dir.join("a.wav");
    write_tone(&a, 2.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a.clone()]);
    harness.run_steps(3);
    focus_timeline(&mut harness);
    harness.key_press(egui::Key::C);
    harness.run_steps(2);
    assert!(harness.state().test_multi_edit_cut_tool(), "C takes the tool");

    let track = harness.get_by_label("Track 01").rect();
    let y = track.center().y + 12.0;
    let x_for = |h: &App, t: f64| h.state().test_multi_edit_x_for(t);
    // Three pixels past the 1 s grid line: snapped onto it.
    let near_line = egui::pos2(x_for(&harness, 1.0) + 3.0, y);
    click_at(&mut harness, near_line);
    let starts = |h: &App| -> Vec<(f64, f64)> {
        h.state().test_multi_edit_clips().iter().map(|c| (c.2, c.3)).collect()
    };
    let got = starts(&harness);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!((got[0].1 - 1.0).abs() < 1e-6 && (got[1].0 - 1.0).abs() < 1e-6, "split at 1 s: {got:?}");

    // With the tool a drag moves nothing.
    let from = egui::pos2(x_for(&harness, 0.5), y);
    drag(&mut harness, from, from + egui::vec2(120.0, 0.0));
    assert_eq!(starts(&harness), got, "the clips stayed put");

    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(!harness.state().test_multi_edit_cut_tool(), "Escape puts it away");
    harness.key_press(egui::Key::C);
    harness.run_steps(2);
    harness.key_press(egui::Key::C);
    harness.run_steps(2);
    assert!(!harness.state().test_multi_edit_cut_tool(), "C toggles");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn alt_click_splits_a_clip_without_the_tool() {
    let dir = temp_dir("alt_cut");
    let a = dir.join("a.wav");
    write_tone(&a, 2.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a.clone()]);
    harness.run_steps(3);
    let track = harness.get_by_label("Track 01").rect();
    let pos = egui::pos2(harness.state().test_multi_edit_x_for(1.37), track.center().y + 12.0);
    click_with(&mut harness, pos, egui::Modifiers::ALT);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 2, "{clips:?}");
    assert!((clips[1].2 - 1.37).abs() < 0.02, "where it was clicked, unsnapped: {clips:?}");
    assert!(!harness.state().test_multi_edit_cut_tool());
    assert!(harness.state_mut().test_multi_edit_undo());
    assert_eq!(harness.state().test_multi_edit_clips().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Overlapping clips, a clip waiting for its length, and the cut tool:
/// `debug/screenshot_verify/multi_edit/06_overlap.png`, `07_cut_tool.png`.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_multi_edit_overlap_and_cut() {
    let dir = temp_dir("render_overlap");
    let (long, mid, short, broken) = (
        dir.join("long.wav"),
        dir.join("mid.wav"),
        dir.join("short.wav"),
        dir.join("broken.wav"),
    );
    write_tone(&long, 6.0);
    write_tone(&mid, 3.0);
    write_tone(&short, 1.5);
    write_unreadable(&broken);
    let all = [long.clone(), mid.clone(), short.clone(), broken.clone()];
    let mut harness = list_waiting_for(&all, &all[..3]);
    harness.set_size(egui::vec2(1600.0, 700.0));
    harness.state_mut().test_multi_edit_new();
    let state = harness.state_mut();
    // Track 01: two clips crossing over 2.6 - 3.2 s.
    state.test_multi_edit_drop(Some(0), 0.2, &[mid.clone()]);
    state.test_multi_edit_drop(Some(0), 2.6, &[mid.clone()]);
    // Track 02: a short clip wholly inside a long one.
    state.test_multi_edit_drop(None, 0.5, &[long.clone()]);
    state.test_multi_edit_drop(Some(1), 2.0, &[short.clone()]);
    // Track 03: a row whose length is not known, as its start.
    state.test_multi_edit_drop(None, 1.0, &[broken.clone()]);
    state.test_multi_edit_seek(4.5);
    wait_for_mix(&mut harness);
    harness.run_steps(10);

    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("multi_edit");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    harness
        .render()
        .expect("render the overlaps")
        .save(out_dir.join("06_overlap.png"))
        .expect("save the screenshot");

    // The cut tool over the long clip.
    harness.state_mut().test_multi_edit_set_cut_tool(true);
    let track = harness.get_by_label("Track 02").rect();
    let on_long = egui::pos2(harness.state().test_multi_edit_x_for(4.8), track.center().y + 12.0);
    harness.hover_at(on_long);
    harness.run_steps(4);
    harness
        .render()
        .expect("render the cut tool")
        .save(out_dir.join("07_cut_tool.png"))
        .expect("save the screenshot");

    // A selection rectangle over both tracks, mid-drag.
    harness.state_mut().test_multi_edit_set_cut_tool(false);
    let row1 = harness.get_by_label("Track 01").rect();
    let from = egui::pos2(harness.state().test_multi_edit_x_for(0.05), row1.center().y + 14.0);
    let to = egui::pos2(harness.state().test_multi_edit_x_for(3.0), track.center().y + 14.0);
    drag_hold(&mut harness, from, to);
    harness.run_steps(2);
    harness
        .render()
        .expect("render the selection")
        .save(out_dir.join("08_marquee.png"))
        .expect("save the screenshot");
    release(&mut harness, to);
    let _ = std::fs::remove_dir_all(&dir);
}

fn close(got: &[(usize, f64)], want: &[(usize, f64)]) -> bool {
    got.len() == want.len()
        && got
            .iter()
            .zip(want)
            .all(|(g, w)| g.0 == w.0 && (g.1 - w.1).abs() < 1e-6)
}

#[test]
fn a_rectangle_dragged_over_the_tracks_selects_what_it_meets() {
    let dir = temp_dir("marquee");
    let (a, b, c) = (dir.join("a.wav"), dir.join("b.wav"), dir.join("c.wav"));
    for f in [&a, &b, &c] {
        write_tone(f, 1.0);
    }
    let mut harness = list_with(&[a.clone(), b.clone(), c.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.state_mut().test_multi_edit_drop(Some(0), 1.0, &[a.clone()]);
    harness.state_mut().test_multi_edit_drop(None, 1.5, &[b.clone()]);
    harness.state_mut().test_multi_edit_drop(Some(0), 5.0, &[c.clone()]);
    harness.run_steps(3);
    focus_timeline(&mut harness);
    let (row0, row1) = (
        harness.get_by_label("Track 01").rect(),
        harness.get_by_label("Track 02").rect(),
    );
    let x_for = |h: &App, t: f64| h.state().test_multi_edit_x_for(t);
    // From empty space before the first clip, over both tracks, short of c.
    let from = egui::pos2(x_for(&harness, 0.5), row0.center().y + 12.0);
    let to = egui::pos2(x_for(&harness, 3.0), row1.center().y + 12.0);
    drag(&mut harness, from, to);
    let selected = harness.state().test_multi_edit_selected_clips();
    assert!(close(&selected, &[(0, 1.0), (1, 1.5)]), "a and b, not c: {selected:?}");

    harness.key_press(egui::Key::Delete);
    harness.run_steps(2);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 1, "both went: {clips:?}");
    assert_eq!(clips[0].1, c);
    assert!(harness.state_mut().test_multi_edit_undo());
    assert_eq!(harness.state().test_multi_edit_clips().len(), 3, "one undo brings both back");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_and_shift_clicks_build_a_selection() {
    let dir = temp_dir("click_select");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    for start in [0.0, 2.0, 4.0] {
        harness.state_mut().test_multi_edit_drop(Some(0), start, &[a.clone()]);
    }
    harness.run_steps(3);
    let y = harness.get_by_label("Track 01").rect().center().y + 12.0;
    let at = |h: &App, t: f64| egui::pos2(h.state().test_multi_edit_x_for(t), y);
    let starts = |h: &App| -> Vec<f64> {
        h.state().test_multi_edit_selected_clips().iter().map(|s| s.1).collect()
    };
    let (p0, p2, p4) = (at(&harness, 0.5), at(&harness, 2.5), at(&harness, 4.5));
    click_at(&mut harness, p0);
    assert_eq!(starts(&harness), vec![0.0]);
    click_with(&mut harness, p2, egui::Modifiers::COMMAND);
    assert_eq!(starts(&harness), vec![0.0, 2.0], "Ctrl adds");
    click_with(&mut harness, p4, egui::Modifiers::SHIFT);
    assert_eq!(starts(&harness), vec![0.0, 2.0, 4.0], "Shift adds");
    click_with(&mut harness, p0, egui::Modifiers::COMMAND);
    assert_eq!(starts(&harness), vec![2.0, 4.0], "Ctrl on a selected clip takes it out");
    click_at(&mut harness, p0);
    assert_eq!(starts(&harness), vec![0.0], "a plain click selects it alone");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dragging_a_selected_clip_carries_the_whole_selection() {
    let dir = temp_dir("group_drag");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.state_mut().test_multi_edit_drop(Some(0), 1.0, &[a.clone()]);
    harness.state_mut().test_multi_edit_drop(None, 2.0, &[a.clone()]);
    harness.state_mut().test_multi_edit_drop(None, 8.0, &[a.clone()]);
    harness.run_steps(3);
    // a on Track 01 and b on Track 02 selected; the clip on Track 03 not.
    assert!(harness.state_mut().test_multi_edit_select_clips(&[0, 1]));
    harness.run_steps(1);
    let (row0, row1) = (
        harness.get_by_label("Track 01").rect(),
        harness.get_by_label("Track 02").rect(),
    );
    let from = egui::pos2(harness.state().test_multi_edit_x_for(1.5), row0.center().y + 12.0);
    let to = egui::pos2(harness.state().test_multi_edit_x_for(2.5), row1.center().y + 12.0);
    drag(&mut harness, from, to);
    let placed: Vec<(usize, f64)> = harness
        .state()
        .test_multi_edit_clips()
        .iter()
        .map(|c| (c.0, c.2))
        .collect();
    assert!(
        close(&placed, &[(1, 2.0), (2, 3.0), (2, 8.0)]),
        "both a second later, one track down: {placed:?}"
    );
    assert_eq!(harness.state().test_multi_edit_selected_clips().len(), 2, "still selected");
    assert!(harness.state_mut().test_multi_edit_undo());
    let placed: Vec<(usize, f64)> = harness
        .state()
        .test_multi_edit_clips()
        .iter()
        .map(|c| (c.0, c.2))
        .collect();
    assert!(close(&placed, &[(0, 1.0), (1, 2.0), (2, 8.0)]), "one undo: {placed:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ctrl_a_selects_every_clip_and_escape_lets_go() {
    let dir = temp_dir("select_all");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a.clone()]);
    harness.state_mut().test_multi_edit_drop(None, 3.0, &[a.clone()]);
    harness.run_steps(3);
    focus_timeline(&mut harness);
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run_steps(2);
    assert_eq!(harness.state().test_multi_edit_selected_clips().len(), 2);
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    assert!(harness.state().test_multi_edit_selected_clips().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_copied_group_pastes_with_its_spacing_and_its_tracks() {
    let dir = temp_dir("group_paste");
    let a = dir.join("a.wav");
    write_tone(&a, 1.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_drop(Some(0), 1.0, &[a.clone()]);
    harness.state_mut().test_multi_edit_drop(None, 1.5, &[a.clone()]);
    harness.run_steps(3);
    focus_timeline(&mut harness);
    assert!(harness.state_mut().test_multi_edit_select_clips(&[0, 1]));
    harness.event(egui::Event::Copy);
    harness.run_steps(2);
    harness.state_mut().test_multi_edit_seek(5.0);
    paste(&mut harness);
    let placed = |h: &App| -> Vec<(usize, f64)> {
        h.state().test_multi_edit_clips().iter().map(|c| (c.0, c.2)).collect()
    };
    assert!(
        close(&placed(&harness), &[(0, 1.0), (0, 5.0), (1, 1.5), (1, 5.5)]),
        "{:?}",
        placed(&harness)
    );
    let playhead = harness.state().test_multi_edit_playhead();
    assert!((playhead - 6.5).abs() < 1e-3, "to the group's end: {playhead}");
    paste(&mut harness);
    assert!(
        close(
            &placed(&harness),
            &[(0, 1.0), (0, 5.0), (0, 6.5), (1, 1.5), (1, 5.5), (1, 7.0)]
        ),
        "again, right after: {:?}",
        placed(&harness)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn alt_arrows_and_s_act_on_every_selected_clip() {
    let dir = temp_dir("group_keys");
    let a = dir.join("a.wav");
    write_tone(&a, 2.0);
    let mut harness = list_with(&[a.clone()]);
    harness.state_mut().test_multi_edit_new();
    harness.state_mut().test_multi_edit_set_zoom(80.0);
    harness.state_mut().test_multi_edit_drop(Some(0), 0.0, &[a.clone()]);
    harness.state_mut().test_multi_edit_drop(None, 0.5, &[a.clone()]);
    harness.run_steps(3);
    focus_timeline(&mut harness);
    assert!(harness.state_mut().test_multi_edit_select_clips(&[0, 1]));
    // At 80 px a second an arrow step is 1 s: both move by the primary's.
    harness.key_press_modifiers(egui::Modifiers::ALT, egui::Key::ArrowRight);
    harness.run_steps(2);
    let placed: Vec<(usize, f64)> = harness
        .state()
        .test_multi_edit_clips()
        .iter()
        .map(|c| (c.0, c.2))
        .collect();
    assert!(close(&placed, &[(0, 1.0), (1, 1.5)]), "{placed:?}");

    harness.state_mut().test_multi_edit_seek(2.0);
    harness.key_press(egui::Key::S);
    harness.run_steps(2);
    let clips = harness.state().test_multi_edit_clips();
    assert_eq!(clips.len(), 4, "both split at the playhead: {clips:?}");
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
