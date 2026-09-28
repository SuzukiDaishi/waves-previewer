//! Copy files or folders in Explorer, press Ctrl+V on the list, and they open
//! -- the file manager gesture.
//!
//! Explorer leaves a file list on the clipboard and no text, and egui reports
//! nothing for Ctrl+V then; the app sees the press through a keyboard hook
//! (`os_paste_key.rs`). These tests stand in for both halves: the clipboard's
//! file list (`test_set_clipboard_files`) and the press the hook would note
//! (`test_note_os_paste_key`). Pasted paths open the way dropped ones do.
#![cfg(feature = "kittest")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui_kittest::Harness;
use neowaves::app::SortKey;
use neowaves::kittest::harness_default;
use neowaves::WavesPreviewer;

type App = Harness<'static, WavesPreviewer>;

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let dir = std::env::temp_dir().join(format!(
        "neowaves_list_paste_{tag}_{}_{}_{}",
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

/// A short mono tone at the fixture's rate.
fn write_tone(path: &Path) {
    const FIXTURE_SR: u32 = 48_000;
    let samples: Vec<f32> = (0..2_400).map(|i| ((i as f32) * 0.05).sin() * 0.2).collect();
    neowaves::wave::export_channels_audio(&[samples], FIXTURE_SR, path).expect("write audio");
}

fn files_len(harness: &App) -> usize {
    harness.state().test_files_len()
}

/// Frames until the paste's load has finished and reported.
fn run_until_reported(harness: &mut App) -> String {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        let toasts = harness.state().test_toast_messages().join(" | ");
        if toasts.contains("Added") || toasts.contains("Nothing to add") {
            harness.run_steps(2);
            return harness.state().test_toast_messages().join(" | ");
        }
        assert!(start.elapsed() < Duration::from_secs(20), "paste never reported: {toasts:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Explorer's Ctrl+C then Ctrl+V on the list.
fn paste_from_explorer(harness: &mut App, paths: &[PathBuf]) {
    harness
        .state_mut()
        .test_set_clipboard_files(Some(paths.to_vec()));
    harness.state_mut().test_note_os_paste_key();
    harness.run_steps(1);
}

#[test]
fn files_and_a_folder_paste_into_an_empty_list() {
    let dir = temp_dir("empty");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    let folder = dir.join("takes");
    std::fs::create_dir_all(folder.join("nested")).unwrap();
    write_tone(&a);
    write_tone(&b);
    write_tone(&folder.join("c.wav"));
    write_tone(&folder.join("nested").join("d.wav"));
    // Not audio, inside the folder: skipped without a word.
    std::fs::write(folder.join("readme.txt"), b"notes").unwrap();

    let mut harness = harness_default();
    harness.run_steps(2);
    assert_eq!(files_len(&harness), 0);
    assert!(harness.state().test_selected_path().is_none(), "nothing is selected");

    paste_from_explorer(&mut harness, &[a.clone(), b.clone(), folder.clone()]);
    let report = run_until_reported(&mut harness);

    assert_eq!(files_len(&harness), 4, "two files and the folder's two, recursively");
    assert!(report.contains("Added 4 file(s)"), "{report}");
    assert!(!report.contains("skipped"), "{report}");
    assert!(
        harness.state().test_selected_path().is_some(),
        "the pasted files are selected"
    );
    assert_eq!(harness.state().test_list_paste_count(), 1);

    harness.state_mut().test_set_clipboard_files(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_press_egui_also_reports_pastes_once() {
    let dir = temp_dir("once");
    let a = dir.join("a.wav");
    write_tone(&a);
    let mut harness = harness_default();
    harness.run_steps(2);

    // A clipboard with text as well: egui sends Event::Paste *and* the hook
    // sees the press, in the same frame.
    harness
        .state_mut()
        .test_set_clipboard_files(Some(vec![a.clone()]));
    harness.state_mut().test_note_os_paste_key();
    harness.event(egui::Event::Paste(format!("file://{}", a.display())));
    harness.run_steps(1);
    run_until_reported(&mut harness);

    assert_eq!(harness.state().test_list_paste_count(), 1, "one press, one paste");
    assert_eq!(files_len(&harness), 1);

    harness.state_mut().test_set_clipboard_files(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_was_turned_away_is_reported() {
    let dir = temp_dir("report");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    let notes = dir.join("notes.txt");
    write_tone(&a);
    write_tone(&b);
    std::fs::write(&notes, b"not audio").unwrap();
    let gone = dir.join("gone.wav");

    let mut harness = harness_default();
    harness.state_mut().test_replace_with_files(&[a.clone()]);
    harness.run_steps(2);
    assert_eq!(files_len(&harness), 1);

    paste_from_explorer(&mut harness, &[a, b, notes, gone]);
    let report = run_until_reported(&mut harness);

    assert_eq!(files_len(&harness), 2);
    assert!(report.contains("Added 1 file(s)"), "{report}");
    assert!(report.contains("1 already in the list"), "{report}");
    assert!(report.contains("1 not audio"), "{report}");
    assert!(report.contains("1 not found"), "{report}");

    harness.state_mut().test_set_clipboard_files(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_column_filter_applies_to_pasted_rows_and_the_report_says_so() {
    let dir = temp_dir("filtered");
    let keep = dir.join("keep_me.wav");
    let other = dir.join("other.wav");
    write_tone(&keep);
    write_tone(&other);

    let mut harness = harness_default();
    harness.run_steps(2);
    harness
        .state_mut()
        .test_set_condition_filter(SortKey::File, "contains", "keep", "")
        .unwrap();

    paste_from_explorer(&mut harness, &[keep.clone(), other]);
    let report = run_until_reported(&mut harness);

    assert_eq!(
        harness.state().test_visible_list_paths(),
        vec![keep],
        "the filter applies to rows a paste brings in"
    );
    assert!(report.contains("Added 2 file(s)"), "{report}");
    assert!(report.contains("1 hidden"), "{report}");

    harness.state_mut().test_set_clipboard_files(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pasted_session_file_opens_the_session() {
    let dir = temp_dir("session");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    write_tone(&a);
    write_tone(&b);
    let session = dir.join("work.nwsess");

    let mut writer = harness_default();
    writer.state_mut().test_replace_with_files(&[a, b]);
    writer.run_steps(2);
    assert!(writer.state_mut().test_save_session_to(&session));
    let start = Instant::now();
    while writer.state().test_session_save_in_flight() {
        assert!(start.elapsed() < Duration::from_secs(20), "save timeout");
        writer.run_steps(1);
    }

    let mut harness = harness_default();
    harness.run_steps(2);
    paste_from_explorer(&mut harness, &[session]);
    let start = Instant::now();
    while files_len(&harness) != 2 {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the pasted session never opened"
        );
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }

    harness.state_mut().test_set_clipboard_files(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_explorer_copy_after_copying_rows_pastes_the_explorer_files() {
    let dir = temp_dir("superseded");
    let a = dir.join("a.wav");
    let b = dir.join("b.wav");
    let c = dir.join("c.wav");
    let d = dir.join("d.wav");
    for path in [&a, &b, &c, &d] {
        write_tone(path);
    }

    let mut harness = harness_default();
    // Stand in for the clipboard from the start, so the list's own copy
    // lands there and not on the clipboard of the machine running the test.
    harness.state_mut().test_set_clipboard_files(Some(Vec::new()));
    harness
        .state_mut()
        .test_replace_with_files(&[a.clone(), b.clone()]);
    harness.run_steps(2);
    assert!(harness.state_mut().test_set_list_selection(&[a, b]));

    // Ctrl+C on both rows, then Ctrl+V with nothing copied since: the list's
    // own paste, which adds a copy of each row.
    harness.event(egui::Event::Copy);
    harness.run_steps(1);
    let start = Instant::now();
    while harness.state().test_clipboard_prep_in_flight() {
        assert!(start.elapsed() < Duration::from_secs(20), "copy never finished");
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.state_mut().test_note_os_paste_key();
    harness.run_steps(2);
    assert_eq!(harness.state().test_virtual_item_count(), 2, "both rows were copied");
    assert_eq!(files_len(&harness), 4);

    // Then two files copied in Explorer: those are what Ctrl+V brings in,
    // not the rows copied earlier.
    paste_from_explorer(&mut harness, &[c, d]);
    let report = run_until_reported(&mut harness);

    assert!(report.contains("Added 2 file(s)"), "{report}");
    assert_eq!(files_len(&harness), 6);
    assert_eq!(
        harness.state().test_virtual_item_count(),
        2,
        "the earlier copy was not pasted again"
    );
    assert_eq!(harness.state().test_list_paste_count(), 2);

    harness.state_mut().test_set_clipboard_files(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_paste_pressed_elsewhere_does_not_fire_later() {
    let dir = temp_dir("stale");
    let a = dir.join("a.wav");
    write_tone(&a);
    let mut harness = harness_default();
    harness.run_steps(2);

    // Pressed while another workspace (Recording) owns the keys: not the
    // list's paste, and it must not wait for the list to come back.
    harness.state_mut().test_set_clipboard_files(Some(vec![a]));
    harness.state_mut().test_open_recording_tab();
    harness.state_mut().test_note_os_paste_key();
    harness.run_steps(2);
    harness.state_mut().test_switch_to_list();
    harness.run_steps(3);

    assert_eq!(harness.state().test_list_paste_count(), 0);
    assert_eq!(files_len(&harness), 0);

    harness.state_mut().test_set_clipboard_files(None);
    let _ = std::fs::remove_dir_all(&dir);
}
