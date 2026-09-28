//! List sorting and filtering, and the rule under both: rows move, the
//! selection does not. The selection follows the *files* it was on through a
//! re-sort, a re-filter or a removal -- never the row numbers.
#![cfg(feature = "kittest")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use neowaves::app::{SortDir, SortKey};
use neowaves::kittest::{harness_default, harness_with_startup};
use neowaves::{StartupConfig, WavesPreviewer};

type App = Harness<'static, WavesPreviewer>;

fn harness_with_rows(count: usize) -> App {
    let mut harness = harness_with_startup(StartupConfig {
        dummy_list_count: Some(count),
        ..StartupConfig::default()
    });
    harness.run_steps(3);
    harness
}

fn row_path(harness: &App, row: usize) -> PathBuf {
    harness.state().test_visible_list_paths()[row].clone()
}

fn run_until_list_jobs_idle(harness: &mut App) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        // Covers the filter job as well as the sort job.
        if !harness.state().test_sort_job_active() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "list job timeout");
    }
    harness.run_steps(1);
}

fn sorted(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths
}

#[test]
fn a_resort_keeps_the_multi_selection_on_the_same_files() {
    let mut harness = harness_with_rows(40);
    harness
        .state_mut()
        .test_set_sort(neowaves::app::SortKey::File, neowaves::app::SortDir::Asc);
    let picked = vec![row_path(&harness, 3), row_path(&harness, 5)];
    assert!(harness.state_mut().test_set_list_selection(&picked));

    harness
        .state_mut()
        .test_set_sort(neowaves::app::SortKey::File, neowaves::app::SortDir::Desc);
    harness.run_steps(1);

    assert_eq!(
        harness.state().test_selected_multi_paths(),
        sorted(picked.clone()),
        "the highlight must stay on the files, not on rows 3 and 5"
    );
    assert_eq!(harness.state().test_selected_path(), Some(&picked[0]));
    assert_eq!(harness.state().test_anchor_path(), Some(picked[0].clone()));
}

#[test]
fn a_search_keeps_the_selection_on_matching_files() {
    let mut harness = harness_with_rows(40);
    // Row 12 matches "wav_00001" (10..19); row 30 does not.
    let kept = row_path(&harness, 12);
    let dropped = row_path(&harness, 30);
    assert!(harness
        .state_mut()
        .test_set_list_selection(&[kept.clone(), dropped.clone()]));

    harness.state_mut().test_set_search_query("wav_00001");
    harness.run_steps(1);

    assert_eq!(harness.state().test_selected_multi_paths(), vec![kept.clone()]);
    assert_eq!(harness.state().test_selected_path(), Some(&kept));
}

#[test]
fn an_async_sort_does_not_undo_a_click_made_while_it_ran() {
    let mut harness = harness_with_rows(400);
    harness.state_mut().test_pin_low_perf_tier();
    let first = row_path(&harness, 10);
    assert!(harness.state_mut().test_set_list_selection(&[first]));

    harness
        .state_mut()
        .test_request_sort(neowaves::app::SortKey::File, neowaves::app::SortDir::Desc);
    assert!(
        harness.state().test_sort_job_active(),
        "400 rows on the low tier must take the async path"
    );
    // The user picks another file before the sort lands.
    let second = row_path(&harness, 20);
    assert!(harness
        .state_mut()
        .test_set_list_selection(&[second.clone()]));

    run_until_list_jobs_idle(&mut harness);

    assert_eq!(harness.state().test_selected_path(), Some(&second));
    assert_eq!(harness.state().test_selected_multi_paths(), vec![second]);
}

#[test]
fn an_async_filter_does_not_undo_a_click_made_while_it_ran() {
    let mut harness = harness_with_rows(400);
    harness.state_mut().test_pin_low_perf_tier();
    let first = row_path(&harness, 10);
    assert!(harness.state_mut().test_set_list_selection(&[first]));

    harness.state_mut().test_apply_search_via_jobs("wav_0000");
    assert!(harness.state().test_sort_job_active());
    let second = row_path(&harness, 3);
    assert!(harness
        .state_mut()
        .test_set_list_selection(&[second.clone()]));

    run_until_list_jobs_idle(&mut harness);

    assert_eq!(harness.state().test_selected_path(), Some(&second));
}

// ---------------------------------------------------------------------------
// Column filters
// ---------------------------------------------------------------------------

fn files_len(harness: &App) -> usize {
    harness.state().test_files_len()
}

/// Clicks a column header. The File column shares its name with the File
/// menu, so of the matches take the lowest on screen: the header row sits
/// below the menu bar.
fn click_header(harness: &mut App, label: &str, secondary: bool) {
    let node = harness
        .get_all_by_label(label)
        .max_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .unwrap_or_else(|| panic!("no header labelled {label}"));
    if secondary {
        node.click_secondary();
    } else {
        node.click();
    }
}

fn unsorted(harness: &mut App) {
    harness.state_mut().test_set_sort(SortKey::File, SortDir::None);
    harness.run_steps(1);
}

#[test]
fn the_header_menu_sorts_either_way() {
    let mut harness = harness_with_rows(40);
    unsorted(&mut harness);

    click_header(&mut harness, "File", true);
    harness.run_steps(2);
    harness.get_by_label("Sort descending").click();
    harness.run_steps(2);

    assert_eq!(harness.state().test_sort_dir_name(), "Desc");
    assert!(row_path(&harness, 0).ends_with("wav_000039.wav"));
}

#[test]
fn the_value_list_filters_rows_and_the_header_menu_clears_it() {
    let mut harness = harness_with_rows(40);
    unsorted(&mut harness);

    click_header(&mut harness, "File", true);
    harness.run_steps(2);
    harness.get_by_label("Filter\u{2026}").click();
    harness.run_steps(2);
    assert!(harness.state().test_list_filter_dialog_open());

    let start = Instant::now();
    let values = loop {
        if let Some(values) = harness.state().test_list_filter_dialog_values() {
            break values;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "value list timeout");
        harness.run_steps(1);
    };
    assert_eq!(values.len(), 40, "every file name is one value");
    assert!(values.iter().all(|(_, rows)| *rows == 1));

    assert!(harness
        .state_mut()
        .test_list_filter_dialog_check_only(&["wav_000003.wav", "wav_000007.wav"]));
    harness.run_steps(1);
    harness.get_by_label("OK").click();
    harness.run_steps(2);

    assert!(!harness.state().test_list_filter_dialog_open());
    assert_eq!(files_len(&harness), 2);
    assert_eq!(harness.state().test_column_filter_count(), 1);

    click_header(&mut harness, "File", true);
    harness.run_steps(2);
    harness.get_by_label("Clear filter from \"File\"").click();
    harness.run_steps(2);
    assert_eq!(files_len(&harness), 40);
    assert_eq!(harness.state().test_column_filter_count(), 0);
}

#[test]
fn a_bad_condition_keeps_the_dialog_open_with_the_reason() {
    let mut harness = harness_with_rows(10);
    unsorted(&mut harness);
    click_header(&mut harness, "File", true);
    harness.run_steps(2);
    harness.get_by_label("Filter\u{2026}").click();
    harness.run_steps(2);

    assert!(harness
        .state_mut()
        .test_list_filter_dialog_use_condition("matches regex", "(unclosed"));
    // The modal is re-centred a frame after its content changes size.
    harness.run_steps(3);
    harness.get_by_label("OK").click();
    harness.run_steps(2);

    assert!(harness.state().test_list_filter_dialog_open());
    let error = harness.state().test_list_filter_dialog_error().unwrap_or_default();
    assert!(error.contains("regex"), "{error}");
    assert_eq!(files_len(&harness), 10, "nothing was applied");
}

#[test]
fn text_conditions_combine_with_the_search_box() {
    let mut harness = harness_with_rows(40);
    harness
        .state_mut()
        .test_set_condition_filter(SortKey::File, "begins with", "wav_00001", "")
        .unwrap();
    harness.run_steps(1);
    assert_eq!(files_len(&harness), 10);

    // Both apply: the search narrows what the column filter left.
    harness.state_mut().test_set_search_query("5");
    harness.run_steps(1);
    assert_eq!(files_len(&harness), 1);

    harness
        .state_mut()
        .test_set_condition_filter(SortKey::File, "equals", "wav_00002?.wav", "")
        .unwrap();
    harness.state_mut().test_set_search_query("");
    harness.run_steps(1);
    assert_eq!(files_len(&harness), 10, "? is a one-character wildcard");
}

#[test]
fn number_filters_rank_and_average_the_column() {
    let mut harness = harness_with_rows(10);
    for (row, gain) in [(0usize, 1.0f32), (1, 2.0), (2, 3.0), (3, 6.0)] {
        let path = row_path(&harness, row);
        harness.state_mut().test_set_pending_gain_db_for_path(&path, gain);
    }
    harness.run_steps(1);

    harness
        .state_mut()
        .test_set_condition_filter(SortKey::Gain, "top N items", "2", "")
        .unwrap();
    harness.run_steps(1);
    assert_eq!(files_len(&harness), 2, "6 dB and 3 dB");

    // Ten rows averaging 1.2 dB: 2, 3 and 6 are above it.
    harness
        .state_mut()
        .test_set_condition_filter(SortKey::Gain, "above average", "", "")
        .unwrap();
    harness.run_steps(1);
    assert_eq!(files_len(&harness), 3);

    harness
        .state_mut()
        .test_set_condition_filter(SortKey::Gain, "between", "1.5", "3")
        .unwrap();
    harness.run_steps(1);
    assert_eq!(files_len(&harness), 2);
}

#[test]
fn an_async_filter_measures_top_n_before_matching() {
    let mut harness = harness_with_rows(400);
    harness.state_mut().test_pin_low_perf_tier();
    for (row, gain) in [(5usize, 4.0f32), (50, 9.0), (300, 7.0)] {
        let path = row_path(&harness, row);
        harness.state_mut().test_set_pending_gain_db_for_path(&path, gain);
    }
    harness
        .state_mut()
        .test_set_condition_filter(SortKey::Gain, "top N items", "2", "")
        .unwrap();
    run_until_list_jobs_idle(&mut harness);
    assert_eq!(files_len(&harness), 2);
}

// ---- Filters that need real files ----

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let dir = std::env::temp_dir().join(format!(
        "neowaves_list_filter_{tag}_{}_{}_{}",
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

/// A mono tone of `frames` frames at the fixture's rate.
fn write_tone(path: &Path, frames: usize) {
    const FIXTURE_SR: u32 = 48_000;
    let samples: Vec<f32> = (0..frames)
        .map(|i| ((i as f32) * 0.05).sin() * 0.2)
        .collect();
    neowaves::wave::export_channels_audio(&[samples], FIXTURE_SR, path).expect("write audio");
}

fn settle(harness: &mut App, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        harness.step();
        if !harness.state().test_session_save_in_flight()
            && !harness.state().test_session_open_busy()
        {
            harness.run_steps(3);
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("{what} did not settle");
}

#[test]
fn column_filters_are_saved_with_the_session() {
    let dir = temp_dir("session");
    let files: Vec<PathBuf> = ["alpha.wav", "beta.wav", "gamma.wav"]
        .iter()
        .map(|name| {
            let path = dir.join(name);
            write_tone(&path, 2_400);
            path
        })
        .collect();
    let mut harness = harness_default();
    harness.state_mut().test_replace_with_files(&files);
    harness.run_steps(2);
    harness
        .state_mut()
        .test_set_condition_filter(SortKey::File, "contains", "ta", "")
        .unwrap();
    harness.run_steps(1);
    assert_eq!(files_len(&harness), 1, "beta only");

    let session = dir.join("work.nwsess");
    assert!(harness.state_mut().test_save_session_to(&session));
    settle(&mut harness, "save");

    let mut reopened = harness_default();
    assert!(reopened.state_mut().test_open_session_from(&session));
    settle(&mut reopened, "open");
    assert_eq!(reopened.state().test_column_filter_count(), 1);
    assert_eq!(files_len(&reopened), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_metadata_filter_follows_values_as_they_arrive() {
    let dir = temp_dir("meta");
    // 0.05 s and 0.5 s long.
    let short = dir.join("short.wav");
    let long = dir.join("long.wav");
    write_tone(&short, 2_400);
    write_tone(&long, 24_000);
    let mut harness = harness_default();
    harness
        .state_mut()
        .test_replace_with_files(&[short.clone(), long.clone()]);
    // Let the first read finish, so nothing already in flight can bring the
    // values back: after the clear below, only the filter's own prefetch can.
    let start = Instant::now();
    while !(harness.state().test_meta_loaded(&short) && harness.state().test_meta_loaded(&long)) {
        assert!(start.elapsed() < Duration::from_secs(20), "initial metadata timeout");
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.run_steps(2);
    // Forget what was read, so the filter starts with nothing known.
    harness.state_mut().test_clear_meta_for_path(&short);
    harness.state_mut().test_clear_meta_for_path(&long);
    harness
        .state_mut()
        .test_set_condition_filter(SortKey::Length, ">", "0.2", "")
        .unwrap();
    assert_eq!(files_len(&harness), 0, "no length known yet: a blank fails >");

    let start = Instant::now();
    while files_len(&harness) != 1 {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the long file never came back once its length was read"
        );
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(harness.state().test_visible_list_paths(), vec![long]);

    let _ = std::fs::remove_dir_all(&dir);
}
