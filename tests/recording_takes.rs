//! The Recording tab's take list, and the rule under it: a take is one list
//! row for its whole life. Stopping makes it a `(virtual)` row; saving turns
//! that same row into the saved file's row -- never a virtual row plus a
//! second row for the file.
#![cfg(feature = "kittest")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    let dir = std::env::temp_dir().join(format!(
        "neowaves_recording_takes_{tag}_{}_{}_{}",
        std::process::id(),
        now_ms,
        seq
    ));
    std::fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

fn synth(sr: u32, secs: f32) -> Vec<Vec<f32>> {
    let frames = (sr as f32 * secs) as usize;
    let ch: Vec<f32> = (0..frames)
        .map(|i| (i as f32 / sr as f32 * 330.0 * std::f32::consts::TAU).sin() * 0.3)
        .collect();
    vec![ch.clone(), ch]
}

/// A finished take, in the test's own directory. Not the NeoWaves temp cache
/// `start_recording` uses: every harness sweeps that cache on startup, so a
/// test running alongside would delete this one's take while it is written.
/// Every save here names its destination, so where the take lives does not
/// matter.
fn write_take(dir: &std::path::Path, tag: &str, markers: &[usize]) -> PathBuf {
    let path = dir.join(format!("nwcache_{tag}_take.wav"));
    neowaves::wave::export_channels_audio(&synth(48_000, 3.0), 48_000, &path)
        .expect("write take");
    if !markers.is_empty() {
        let entries: Vec<_> = markers
            .iter()
            .enumerate()
            .map(|(i, &sample)| neowaves::markers::MarkerEntry {
                sample,
                label: format!("M{:02}", i + 1),
            })
            .collect();
        neowaves::markers::write_markers(&path, 48_000, 48_000, &entries)
            .expect("write take markers");
    }
    path
}

fn harness_with_folder(dir: PathBuf) -> Harness<'static, WavesPreviewer> {
    let mut cfg = StartupConfig::default();
    cfg.open_folder = Some(dir);
    cfg.open_first = false;
    harness_with_startup(cfg)
}

fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>) {
    let start = Instant::now();
    while harness.state().files.is_empty() {
        harness.run_steps(1);
        assert!(start.elapsed() < Duration::from_secs(20), "scan timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_export_finish(harness: &mut Harness<'static, WavesPreviewer>) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        if !harness.state().test_export_in_progress() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "export timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Keeps frames running long enough for the folder watcher to report the new
/// file, which is where a second row used to come from.
fn let_the_watcher_run(harness: &mut Harness<'static, WavesPreviewer>) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn wait_for_tab_ready(harness: &mut Harness<'static, WavesPreviewer>) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        let ready = harness
            .state()
            .active_tab
            .and_then(|idx| harness.state().tabs.get(idx))
            .map(|tab| tab.samples_len > 0 && !tab.loading)
            .unwrap_or(false);
        if ready {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "tab ready timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
    harness.run_steps(3);
}

fn virtual_rows(harness: &Harness<'static, WavesPreviewer>) -> usize {
    harness
        .state()
        .items
        .iter()
        .filter(|item| item.path.to_string_lossy().contains("__virtual__"))
        .count()
}

#[test]
fn stopping_a_take_adds_exactly_one_virtual_row() {
    let root = make_temp_dir("stop");
    let takes_dir = make_temp_dir("stop_takes");
    let take = write_take(&takes_dir, "stop", &[]);
    let mut harness = harness_with_folder(root.clone());
    harness.run_steps(3);

    let row = harness
        .state_mut()
        .test_finish_recording_take(&take)
        .expect("a stopped take must become a list row");
    harness.run_steps(2);

    assert_eq!(harness.state().test_rows_with_path(&row), 1);
    assert_eq!(virtual_rows(&harness), 1);
    assert_eq!(harness.state_mut().test_recording_take_states(), ["stopped"]);

    let _ = std::fs::remove_file(&take);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&takes_dir);
}

/// What the list knows about a row: channels, length, peak, loudness,
/// waveform points and marker ticks -- `None` until its metadata arrives.
fn row_meta(
    harness: &Harness<'static, WavesPreviewer>,
    row: &std::path::Path,
) -> Option<(u16, f32, bool, bool, usize, usize)> {
    let item = harness.state().items.iter().find(|item| item.path == row)?;
    let meta = item.meta.as_ref()?;
    Some((
        meta.channels,
        meta.duration_secs.unwrap_or_default(),
        meta.peak_db.is_some(),
        meta.lufs_i.is_some(),
        meta.thumb.len(),
        meta.marker_fracs.len(),
    ))
}

#[test]
fn a_stopped_take_row_shows_length_levels_and_waveform() {
    let root = make_temp_dir("row_meta");
    let takes_dir = make_temp_dir("row_meta_takes");
    let take = write_take(&takes_dir, "row_meta", &[24_000, 96_000]);
    let mut harness = harness_with_folder(root.clone());
    harness.run_steps(3);
    // Stopped from the Recording tab, as it is in use: the list is not
    // drawn, so nothing there asks for the row's metadata.
    harness.state_mut().test_open_recording_tab();
    harness.run_steps(2);

    let row = harness
        .state_mut()
        .test_finish_recording_take(&take)
        .expect("a stopped take must become a list row");
    let start = Instant::now();
    let (channels, secs, has_peak, has_lufs, thumb, markers) = loop {
        harness.run_steps(1);
        if let Some(meta) = row_meta(&harness, &row).filter(|m| m.4 > 0) {
            break meta;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the take's row never got its metadata: {:?}",
            row_meta(&harness, &row)
        );
        std::thread::sleep(Duration::from_millis(10));
    };

    assert_eq!(channels, 2);
    assert!((secs - 3.0).abs() < 0.05, "length {secs}");
    assert!(has_peak, "dBFS (Peak)");
    assert!(has_lufs, "LUFS (I)");
    assert!(thumb > 0, "waveform");
    assert_eq!(markers, 2, "the take's markers tick the waveform");

    // And the list, once in front, keeps them.
    harness.state_mut().test_switch_to_list();
    harness.run_steps(3);
    assert_eq!(
        row_meta(&harness, &row).map(|m| (m.0, m.4 > 0)),
        Some((2, true))
    );

    let _ = std::fs::remove_file(&take);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&takes_dir);
}

#[test]
fn saving_a_take_into_the_watched_folder_leaves_one_file_row() {
    let root = make_temp_dir("save_into_root");
    let takes_dir = make_temp_dir("save_into_root_takes");
    let take = write_take(&takes_dir, "save_into_root", &[]);
    let mut harness = harness_with_folder(root.clone());
    harness.run_steps(3);

    let row = harness
        .state_mut()
        .test_finish_recording_take(&take)
        .expect("take row");
    let dst = root.join("my take.wav");
    assert!(harness.state_mut().test_save_recording_take_as(0, &dst));
    wait_for_export_finish(&mut harness);
    let_the_watcher_run(&mut harness);

    assert!(dst.is_file(), "the take must be written where the dialog said");
    assert_eq!(
        harness.state().test_rows_with_path(&dst),
        1,
        "the saved take must be one row, not the virtual row plus the file"
    );
    assert_eq!(harness.state().test_rows_with_path(&row), 0);
    assert_eq!(virtual_rows(&harness), 0, "no virtual row may be left behind");
    assert!(harness.state().test_row_is_plain_file(&dst));
    assert_eq!(
        harness.state_mut().test_recording_take_states(),
        [format!("saved:{}", dst.display())]
    );

    let _ = std::fs::remove_file(&take);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&takes_dir);
}

#[test]
fn saving_over_a_listed_file_replaces_its_row() {
    let root = make_temp_dir("overwrite_listed");
    let takes_dir = make_temp_dir("overwrite_listed_takes");
    let existing = root.join("existing.wav");
    neowaves::wave::export_channels_audio(&synth(48_000, 1.0), 48_000, &existing)
        .expect("write existing file");
    let take = write_take(&takes_dir, "overwrite_listed", &[]);
    let mut harness = harness_with_folder(root.clone());
    wait_for_scan(&mut harness);
    assert_eq!(harness.state().test_rows_with_path(&existing), 1);

    harness
        .state_mut()
        .test_finish_recording_take(&take)
        .expect("take row");
    assert!(harness.state_mut().test_save_recording_take_as(0, &existing));
    wait_for_export_finish(&mut harness);
    let_the_watcher_run(&mut harness);

    assert_eq!(
        harness.state().test_rows_with_path(&existing),
        1,
        "the file's old row must give way to the saved take, not sit beside it"
    );
    assert_eq!(virtual_rows(&harness), 0);
    assert!(harness.state().test_row_is_plain_file(&existing));
    assert_eq!(harness.state().items.len(), 1);

    let _ = std::fs::remove_file(&take);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&takes_dir);
}

#[test]
fn take_markers_show_in_the_editor_and_survive_the_save() {
    let root = make_temp_dir("markers");
    let takes_dir = make_temp_dir("markers_takes");
    let export_dir = make_temp_dir("markers_out");
    let take = write_take(&takes_dir, "markers", &[24_000, 96_000]);
    let mut harness = harness_with_folder(root.clone());
    harness.run_steps(3);

    let row = harness
        .state_mut()
        .test_finish_recording_take(&take)
        .expect("take row");
    harness.state_mut().test_open_tab_for_path(&row);
    wait_for_tab_ready(&mut harness);
    assert_eq!(
        harness.state().test_active_tab_marker_count(),
        2,
        "markers dropped while recording must be on the take in the editor"
    );

    let dst = export_dir.join("marked.wav");
    assert!(harness.state_mut().test_save_recording_take_as(0, &dst));
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);
    let saved = neowaves::markers::read_markers(&dst, 48_000, 48_000).unwrap_or_default();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].sample, 24_000);
    assert_eq!(saved[1].sample, 96_000);

    let _ = std::fs::remove_file(&take);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&takes_dir);
    let _ = std::fs::remove_dir_all(&export_dir);
}

#[test]
fn take_markers_survive_a_save_without_opening_the_editor() {
    let root = make_temp_dir("markers_unopened");
    let takes_dir = make_temp_dir("markers_unopened_takes");
    let export_dir = make_temp_dir("markers_unopened_out");
    let take = write_take(&takes_dir, "markers_unopened", &[12_000]);
    let mut harness = harness_with_folder(root.clone());
    harness.run_steps(3);

    harness
        .state_mut()
        .test_finish_recording_take(&take)
        .expect("take row");
    let dst = export_dir.join("unopened.wav");
    assert!(harness.state_mut().test_save_recording_take_as(0, &dst));
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);
    let saved = neowaves::markers::read_markers(&dst, 48_000, 48_000).unwrap_or_default();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].sample, 12_000);

    let _ = std::fs::remove_file(&take);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&takes_dir);
    let _ = std::fs::remove_dir_all(&export_dir);
}

#[test]
fn discarding_a_take_and_removing_its_row_go_together() {
    let root = make_temp_dir("discard");
    let takes_dir = make_temp_dir("discard_takes");
    let first = write_take(&takes_dir, "discard_a", &[]);
    let second = write_take(&takes_dir, "discard_b", &[]);
    let mut harness = harness_with_folder(root.clone());
    harness.run_steps(3);

    let first_row = harness
        .state_mut()
        .test_finish_recording_take(&first)
        .expect("first take row");
    let second_row = harness
        .state_mut()
        .test_finish_recording_take(&second)
        .expect("second take row");
    assert_eq!(virtual_rows(&harness), 2);

    // Discarding in the Recording tab removes the list row.
    assert!(harness.state_mut().test_discard_recording_take(0));
    harness.run_steps(2);
    assert_eq!(harness.state().test_rows_with_path(&first_row), 0);
    assert_eq!(harness.state_mut().test_recording_take_states(), ["stopped"]);

    // Removing the row in the list removes the take.
    harness.state_mut().test_remove_rows(&[second_row.clone()]);
    harness.run_steps(2);
    assert_eq!(harness.state().test_rows_with_path(&second_row), 0);
    assert!(harness.state_mut().test_recording_take_states().is_empty());

    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&takes_dir);
}
