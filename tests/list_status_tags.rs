//! Per-row workflow labels: the Status column and the Tags column.
//!
//! The guarantees these cover:
//! - a default status is stamped on rows as they enter the list;
//! - a row the user deliberately set back to "no status" stays that way
//!   across a save and reopen, instead of being re-stamped with the default;
//! - the palette travels in the `.nwsess`, so a session opened somewhere with
//!   different app preferences still shows its own labels and colors;
//! - assigning and clearing a label is one undoable step, deleting a
//!   definition included.

#[cfg(feature = "kittest")]
mod list_status_tags {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use neowaves::kittest::harness_default;
    use neowaves::WavesPreviewer;

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_status_tags_{tag}_{}_{}_{}",
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

    fn write_tone(path: &Path, freq: f32) {
        let samples: Vec<f32> = (0..2400)
            .map(|i| ((i as f32) / 48_000.0 * freq * std::f32::consts::TAU).sin() * 0.2)
            .collect();
        neowaves::wave::export_channels_audio(&[samples], 48_000, path).expect("write audio");
    }

    fn settle(harness: &mut Harness<'static, WavesPreviewer>, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            harness.step();
            if !harness.state().test_session_save_in_flight()
                && !harness.state().test_session_open_busy()
            {
                harness.step();
                harness.step();
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{what} did not settle");
    }

    /// Two audio files in a fresh directory, already loaded into the list.
    fn harness_with_two_files(
        tag: &str,
    ) -> (PathBuf, Vec<PathBuf>, Harness<'static, WavesPreviewer>) {
        let dir = temp_dir(tag);
        let files: Vec<PathBuf> = ["a.wav", "b.wav"]
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let path = dir.join(name);
                write_tone(&path, 440.0 + index as f32 * 110.0);
                path
            })
            .collect();
        let mut harness = harness_default();
        harness.state_mut().test_replace_with_files(&files);
        harness.step();
        (dir, files, harness)
    }

    #[test]
    fn the_default_status_is_stamped_on_rows_as_they_are_added() {
        let (dir, _files, mut harness) = harness_with_two_files("default_stamp");

        let wip = harness.state_mut().test_add_status("WIP", [212, 152, 56]);
        harness.state_mut().test_set_default_status(Some(&wip));

        // Rows already in the list keep whatever they had; the default only
        // applies to rows arriving after it was set.
        let fresh = dir.join("c.wav");
        write_tone(&fresh, 660.0);
        harness.state_mut().test_add_paths(&[fresh.clone()]);
        harness.step();

        assert_eq!(
            harness.state().test_status_for_path(&fresh).as_deref(),
            Some(wip.as_str()),
            "a newly added row takes the default status"
        );
    }

    #[test]
    fn a_row_set_back_to_no_status_does_not_come_back_wearing_the_default() {
        let (dir, files, mut harness) = harness_with_two_files("explicit_none");
        let session = dir.join("work.nwsess");

        let wip = harness.state_mut().test_add_status("WIP", [212, 152, 56]);
        harness.state_mut().test_set_default_status(Some(&wip));
        harness
            .state_mut()
            .test_set_status_for_paths(&files, Some(&wip));
        // One row is deliberately cleared.
        harness
            .state_mut()
            .test_set_status_for_paths(&files[1..], None);
        harness.step();

        assert!(harness.state_mut().test_save_session_to(&session));
        settle(&mut harness, "save");

        let mut reopened = harness_default();
        assert!(reopened.state_mut().test_open_session_from(&session));
        settle(&mut reopened, "open");

        assert_eq!(
            reopened.state().test_status_for_path(&files[0]).as_deref(),
            Some(wip.as_str())
        );
        assert_eq!(
            reopened.state().test_status_for_path(&files[1]),
            None,
            "a row saved with no status must not be re-stamped with the default on reopen"
        );
        assert_eq!(
            reopened.state().test_default_status().as_deref(),
            Some(wip.as_str()),
            "the session carries which status is the default"
        );
    }

    #[test]
    fn the_palette_travels_in_the_session_so_a_shared_one_reads_correctly() {
        let (dir, files, mut harness) = harness_with_two_files("palette_travels");
        let session = dir.join("work.nwsess");

        let review = harness
            .state_mut()
            .test_add_status("Needs Review", [78, 132, 210]);
        let foley = harness.state_mut().test_add_tag("Foley", [76, 160, 96]);
        harness
            .state_mut()
            .test_set_status_for_paths(&files[..1], Some(&review));
        harness
            .state_mut()
            .test_set_tag_for_paths(&files[..1], &foley, true);
        harness.step();
        assert!(harness.state_mut().test_save_session_to(&session));
        settle(&mut harness, "save");

        // A second app that never saw this palette in its own preferences.
        let mut elsewhere = harness_default();
        assert!(elsewhere.state_mut().test_open_session_from(&session));
        settle(&mut elsewhere, "open");

        assert_eq!(
            elsewhere.state().test_status_for_path(&files[0]).as_deref(),
            Some(review.as_str())
        );
        assert_eq!(
            elsewhere.state().test_status_label(&review),
            "Needs Review",
            "the label comes from the session, not from this machine's prefs"
        );
        assert_eq!(elsewhere.state().test_tags_for_path(&files[0]), [foley]);
    }

    #[test]
    fn assigning_a_status_is_undoable() {
        let (_dir, files, mut harness) = harness_with_two_files("undo_assign");
        let ok = harness.state_mut().test_add_status("OK", [76, 160, 96]);

        harness
            .state_mut()
            .test_set_status_for_paths(&files, Some(&ok));
        harness.step();
        assert_eq!(
            harness.state().test_status_for_path(&files[0]).as_deref(),
            Some(ok.as_str())
        );

        // The status is the only field that moved, so this is exactly the
        // case a fingerprint that omits it would drop on the floor.
        assert!(
            harness.state().test_undo_available(false),
            "a status-only change has to reach the undo stack"
        );
        assert!(harness.state_mut().test_trigger_undo_redo(false));
        harness.step();
        assert_eq!(harness.state().test_status_for_path(&files[0]), None);
    }

    #[test]
    fn deleting_a_status_takes_it_off_the_rows_that_used_it() {
        let (_dir, files, mut harness) = harness_with_two_files("delete_def");
        let ng = harness.state_mut().test_add_status("NG", [196, 74, 74]);
        harness
            .state_mut()
            .test_set_status_for_paths(&files, Some(&ng));
        harness.step();

        harness.state_mut().test_remove_status_def(&ng);
        harness.step();

        assert!(
            !harness.state().test_status_ids().contains(&ng),
            "the definition is gone"
        );
        for path in &files {
            assert_eq!(
                harness.state().test_status_for_path(path),
                None,
                "no row may keep pointing at a definition that no longer exists"
            );
        }
        assert!(harness.state_mut().test_trigger_undo_redo(false));
        harness.step();
        assert_eq!(
            harness.state().test_status_for_path(&files[0]).as_deref(),
            Some(ng.as_str()),
            "undo puts the assignments back"
        );
    }

    /// A `.nwsess` from before statuses existed: no palette, no assignments.
    fn write_legacy_session(path: &Path, audio: &Path) {
        std::fs::write(
            path,
            format!(
                r#"version = 2
name = "legacy"
path_mode = "absolute"
base_dir = "{dir}"
active_tab = 0
tabs = []

[list]
files = ["{audio}"]

[app]
theme = "dark"
sort_key = "File"
sort_dir = "Asc"
search_query = ""
search_regex = false

[app.list_columns]
file = true
folder = false
transcript = false
external = false
length = true
ch = true
sr = true
bits = true
peak = false
lufs = false
gain = false
wave = true

[spectrogram]
fft_size = 2048
window = "hann"
overlap = 0.75
max_frames = 512
scale = "log"
mel_scale = "linear"
db_floor = -80.0
max_freq_hz = 20000.0
show_note_labels = false
"#,
                dir = audio.parent().unwrap().display(),
                audio = audio.display(),
            ),
        )
        .expect("write legacy session");
    }

    #[test]
    fn opening_a_session_from_before_statuses_existed_keeps_your_own_palette() {
        let (dir, files, mut harness) = harness_with_two_files("legacy_session");
        let mine = harness.state_mut().test_add_status("Mine", [1, 2, 3]);
        harness.state_mut().test_set_default_status(Some(&mine));
        let before = harness.state().test_status_ids();

        let legacy = dir.join("legacy.nwsess");
        write_legacy_session(&legacy, &files[0]);
        assert!(harness.state_mut().test_open_session_from(&legacy));
        settle(&mut harness, "open legacy");

        // The session says nothing about statuses, so it must not be read as
        // saying "there are none" -- that would wipe the palette the user
        // built, and the next save_prefs would make it permanent.
        assert_eq!(harness.state().test_status_ids(), before);
        assert_eq!(
            harness.state().test_default_status().as_deref(),
            Some(mine.as_str())
        );
        assert_eq!(harness.state().test_status_for_path(&files[0]), None);
    }

    #[test]
    fn the_manager_window_lays_out_both_tabs() {
        let (_dir, files, mut harness) = harness_with_two_files("manager_window");
        let ng = harness.state_mut().test_add_status("NG", [196, 74, 74]);
        harness.state_mut().test_add_tag("Foley", [78, 132, 210]);
        harness
            .state_mut()
            .test_set_status_for_paths(&files, Some(&ng));

        harness.state_mut().test_open_status_tags_window(false);
        harness.step();
        harness.step();
        // Laying the window out at all is most of the check -- egui panics on
        // a duplicate widget id, which is exactly what a per-row editor gets
        // wrong first.
        harness.get_by_label("Statuses");
        harness.get_by_label("+ Add Status");
        harness.get_by_label("Save as global default");
        // The label itself lives in a TextEdit, so assert on the id line the
        // row prints under it -- and on it appearing exactly once, which is
        // what a mis-keyed row loop would get wrong.
        assert_eq!(
            harness.query_all_by_label(&format!("id: {ng}")).count(),
            1,
            "each definition is listed once"
        );

        harness.state_mut().test_set_status_tags_window_tab(true);
        harness.step();
        harness.step();
        harness.get_by_label("+ Add Tag");
        harness.get_by_label("id: foley");
    }

    #[test]
    fn a_tag_toggles_across_the_whole_selection() {
        let (_dir, files, mut harness) = harness_with_two_files("tag_bulk");
        let foley = harness.state_mut().test_add_tag("Foley", [78, 132, 210]);
        let loop_tag = harness.state_mut().test_add_tag("Loop", [156, 106, 200]);

        harness
            .state_mut()
            .test_set_tag_for_paths(&files, &foley, true);
        harness
            .state_mut()
            .test_set_tag_for_paths(&files[..1], &loop_tag, true);
        harness.step();

        assert_eq!(
            harness.state().test_tags_for_path(&files[0]),
            [foley.clone(), loop_tag]
        );
        assert_eq!(
            harness.state().test_tags_for_path(&files[1]),
            [foley.clone()]
        );

        harness
            .state_mut()
            .test_set_tag_for_paths(&files, &foley, false);
        harness.step();
        assert!(harness.state().test_tags_for_path(&files[1]).is_empty());
    }
}
