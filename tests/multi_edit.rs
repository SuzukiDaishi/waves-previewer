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
    assert!(harness.query_by_label("[Multi Edit 1]").is_some(), "its tab");
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
    harness.event(egui::Event::PointerButton {
        pos: to,
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

/// The timeline laid out like the design sketch, saved for a look:
/// `debug/screenshot_verify/multi_edit/01_timeline.png`.
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
    state.test_multi_edit_drop(Some(0), 0.2, &[files[0].clone()]);
    state.test_multi_edit_drop(None, 5.0, &[files[1].clone()]);
    state.test_multi_edit_drop(None, 2.5, &[files[2].clone()]);
    state.test_multi_edit_set_fades(2, 0.6, 1.2);
    for (secs, db) in [(1.0, -12.0), (2.0, 0.0), (3.0, 0.0), (4.0, -9.0), (5.0, -3.0), (8.0, -6.0)] {
        state.test_multi_edit_add_lane_point(2, "Gain", secs, db);
    }
    for (secs, st) in [(1.5, -6.0), (3.0, 5.0), (3.5, -8.0), (6.0, 7.0), (9.0, 0.0)] {
        state.test_multi_edit_add_lane_point(2, "Pitch", secs, st);
    }
    state.test_multi_edit_drop(None, 6.0, &[video.clone()]);
    state.test_multi_edit_set_fades(3, 0.4, 1.5);
    for (secs, db) in [(6.5, -20.0), (7.5, 0.0), (8.5, -6.0), (10.0, 0.0)] {
        state.test_multi_edit_add_lane_point(3, "Gain", secs, db);
    }
    state.test_multi_edit_seek(3.2);
    wait_for_mix(&mut harness);
    harness.run_steps(10);

    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("multi_edit");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    harness
        .render()
        .expect("render the timeline")
        .save(out_dir.join("01_timeline.png"))
        .expect("save the timeline screenshot");

    // The playhead over the video clip: the preview shows its picture.
    harness.state_mut().test_multi_edit_seek(8.5);
    harness.run_steps(2);
    wait_until(&mut harness, "a preview frame", |h| {
        h.state().test_multi_edit_video_shown_pts().is_some()
    });
    harness.run_steps(4);
    harness
        .render()
        .expect("render the preview")
        .save(out_dir.join("02_video_preview.png"))
        .expect("save the preview screenshot");
    let _ = std::fs::remove_dir_all(&dir);
}
