//! The application must not be able to end up in a state only the task
//! manager can leave.
//!
//! Every job behind the modal overlay used to be cleared by exactly one
//! thing: its worker's message. A worker that returned or panicked without
//! sending left the overlay up with input blocked, the repaint cadence
//! pinned, and the quit prompt drawn underneath it -- an application that
//! looks dead, cannot be quit, and takes the unsaved work with it.
#![cfg(feature = "kittest")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use neowaves::kittest::harness_with_startup;
use neowaves::{StartupConfig, WavesPreviewer};

fn make_temp_dir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "neowaves_stability_{tag}_{}_{}_{}",
        std::process::id(),
        now_ms,
        seq
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_fixture_wav(path: &std::path::Path) {
    let sr = 48_000u32;
    let frames = sr as usize / 4;
    let mono: Vec<f32> = (0..frames)
        .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.25)
        .collect();
    neowaves::wave::export_channels_audio(&[mono], sr, path).expect("write fixture");
}

fn harness_with_files(tag: &str, count: usize) -> (PathBuf, Harness<'static, WavesPreviewer>) {
    let dir = make_temp_dir(tag);
    for idx in 0..count {
        write_fixture_wav(&dir.join(format!("row_{idx:03}.wav")));
    }
    let cfg = StartupConfig {
        open_folder: Some(dir.clone()),
        open_first: false,
        ..StartupConfig::default()
    };
    let mut harness = harness_with_startup(cfg);
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        harness.run_steps(1);
        if !harness.state().scan_in_progress && harness.state().test_files_len() >= count {
            break;
        }
        assert!(Instant::now() < deadline, "scan did not finish");
    }
    (dir, harness)
}

#[test]
fn a_save_worker_that_dies_gives_the_window_back() {
    let (dir, mut harness) = harness_with_files("dead_save", 2);

    harness
        .state_mut()
        .test_wedge_session_save_with_dead_worker();
    assert!(
        harness.state().test_busy_overlay_blocking(),
        "the wedge should start out blocking, or the test proves nothing"
    );

    harness.run_steps(2);

    assert!(
        !harness.state().test_session_save_in_flight(),
        "a save whose worker is gone is not still in flight"
    );
    assert!(
        !harness.state().test_busy_overlay_blocking(),
        "the modal overlay must not outlive the job it is waiting for"
    );
    let toasts = harness.state().test_toast_messages().join("\n");
    assert!(
        toasts.contains("Save did not finish"),
        "the user has to be told the session was not written, got: {toasts}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_export_worker_that_dies_gives_the_window_back() {
    let (dir, mut harness) = harness_with_files("dead_export", 2);

    harness.state_mut().test_wedge_export_with_dead_worker();
    harness.run_steps(2);

    assert!(
        !harness.state().test_export_in_flight(),
        "an export whose worker is gone is not still in flight"
    );
    assert!(!harness.state().test_busy_overlay_blocking());
    let toasts = harness.state().test_toast_messages().join("\n");
    assert!(toasts.contains("Export did not finish"), "got: {toasts}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_clipboard_worker_that_dies_gives_the_window_back() {
    let (dir, mut harness) = harness_with_files("dead_clipboard", 2);

    harness
        .state_mut()
        .test_wedge_clipboard_prep_with_dead_worker();
    harness.run_steps(2);

    assert!(
        !harness.state().test_clipboard_prep_in_flight(),
        "a copy whose worker is gone is not still in flight"
    );
    assert!(!harness.state().test_busy_overlay_blocking());
    let toasts = harness.state().test_toast_messages().join("\n");
    assert!(toasts.contains("Copy did not finish"), "got: {toasts}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_quit_prompt_is_answerable_while_a_job_blocks() {
    let (dir, mut harness) = harness_with_files("quit_while_busy", 2);

    // Alive, so no drain clears it: this is the wedged-but-not-dead case, the
    // one where the user's only remaining move is to close the window.
    let _tx = harness
        .state_mut()
        .test_wedge_session_save_started_secs_ago(1);
    harness.state_mut().test_set_show_quit_prompt(true);
    harness.run_steps(2);

    assert!(
        harness.state().test_busy_overlay_blocking(),
        "the job is still in flight"
    );
    // If the overlay still painted its input-blocking layer over the prompt,
    // this click would land on the overlay and the prompt would stay open.
    harness.get_by_label("Cancel").click();
    harness.run_steps(2);
    assert!(
        !harness.state().test_show_quit_prompt(),
        "the quit prompt must be clickable even while a job blocks the window"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_job_that_stops_answering_can_be_stepped_away_from() {
    let (dir, mut harness) = harness_with_files("stall_escape", 2);

    // Alive and holding the overlay, long past the point where a user would
    // conclude the application had died.
    let _tx = harness
        .state_mut()
        .test_wedge_session_save_started_secs_ago(120);
    harness.run_steps(2);

    harness.get_by_label("Stop waiting").click();
    harness.run_steps(2);

    assert!(
        harness.state().test_busy_overlay_released(),
        "the escape hatch has to actually release the overlay"
    );
    assert!(
        harness.state().test_session_save_in_flight(),
        "and it must not pretend the job ended -- it is still running"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_fresh_job_is_modal_again_after_an_escape() {
    let (dir, mut harness) = harness_with_files("release_resets", 2);

    let tx = harness
        .state_mut()
        .test_wedge_session_save_started_secs_ago(120);
    harness.run_steps(2);
    harness.get_by_label("Stop waiting").click();
    harness.run_steps(2);
    assert!(harness.state().test_busy_overlay_released());

    // The stalled job ends; a later one must block again, or one stall would
    // silently turn every future save non-modal.
    drop(tx);
    harness.run_steps(3);
    harness.state_mut().test_wedge_export_with_dead_worker();
    assert!(
        !harness.state().test_busy_overlay_released(),
        "the release is an escape from one job, not a preference"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A menu closure runs every frame it is open. Anything in it that costs the
/// size of the selection is therefore paid sixty times a second, over a
/// selection that can be the whole list -- which is a menu that never opens.
mod menus_do_not_pay_for_the_selection_every_frame {
    use super::*;

    #[test]
    fn the_list_menu_does_not_rebuild_the_selection_each_frame() {
        let (dir, mut harness) = harness_with_files("list_menu_cost", 6);
        harness.state_mut().test_list_select_all();
        harness.run_steps(2);

        harness.get_by_label("List").click();
        harness.run_steps(2);
        let paths_before = WavesPreviewer::test_selected_paths_builds();
        let summaries_before = WavesPreviewer::test_selection_summary_computes();

        // Ten more frames with the menu open and nothing else happening.
        harness.run_steps(10);

        let paths = WavesPreviewer::test_selected_paths_builds() - paths_before;
        let summaries = WavesPreviewer::test_selection_summary_computes() - summaries_before;
        assert!(
            paths <= 1,
            "the open menu rebuilt the selection {paths} times in ten frames"
        );
        assert!(
            summaries <= 1,
            "the open menu re-walked the selection {summaries} times in ten frames"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_summary_still_answers_correctly_from_its_cache() {
        let (dir, mut harness) = harness_with_files("summary_cache", 3);
        harness.state_mut().test_list_select_all();
        harness.run_steps(1);

        // A cache that returns the wrong answer is worse than the cost it
        // saves, so the assertion is on the answer, not the counter.
        assert_eq!(
            harness
                .state_mut()
                .test_selection_menu_summary_shared_status(),
            None,
            "nothing has a status yet"
        );
        let computes_before = WavesPreviewer::test_selection_summary_computes();
        for _ in 0..5 {
            let _ = harness
                .state_mut()
                .test_selection_menu_summary_shared_status();
        }
        assert_eq!(
            WavesPreviewer::test_selection_summary_computes(),
            computes_before,
            "five asks inside the cache window are one pass"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_paste_check_does_not_reach_for_the_clipboard_every_frame() {
        let (dir, mut harness) = harness_with_files("paste_probe", 2);
        harness.run_steps(1);

        // On Windows each of these opens the OS clipboard twice -- a lock
        // every other application's copy and paste needs too. The answer is
        // cached; asking a hundred times is asking once.
        let first = harness.state_mut().test_can_paste_into_list();
        for _ in 0..100 {
            assert_eq!(
                harness.state_mut().test_can_paste_into_list(),
                first,
                "the cached answer must be stable within its window"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_row_that_never_measures_does_not_hold_the_csv_export_open() {
    let (dir, mut harness) = harness_with_files("csv_unmeasurable", 1);
    // A row nothing will ever produce metadata for. A real one gets measured
    // by the pool, which is the case that already worked.
    let target = dir.join("went_away_mid_export.wav");

    harness
        .state_mut()
        .test_wedge_csv_export_on(&target, dir.join("out.csv"));
    assert!(harness.state().test_busy_overlay_blocking());

    // Each landing reports "still nothing measured", which is what an
    // unreadable file, a file that went away mid-export, or a metadata worker
    // that died looks like from here. Retried without a ceiling, this is an
    // application that never comes back.
    for _ in 0..8 {
        harness.state_mut().test_csv_export_meta_landed(&target);
        harness.run_steps(1);
    }

    assert_eq!(
        harness.state().test_csv_export_pending_len(),
        0,
        "the export must give up on a row it cannot measure"
    );
    assert!(
        !harness.state().test_busy_overlay_blocking(),
        "and stop blocking the window when it does"
    );
    let toasts = harness.state().test_toast_messages().join("\n");
    assert!(
        toasts.contains("could not be measured"),
        "a short measurement has to be said out loud, got: {toasts}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
