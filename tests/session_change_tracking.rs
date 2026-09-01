//! "What changed since **I** last opened this?"
//!
//! The guarantee: a file the session points at that somebody replaced while
//! you were away is reported the next time you open it, once -- and a file
//! that was merely touched, or that you changed yourself while the session
//! was open, is not.

#[cfg(feature = "kittest")]
mod session_change_tracking {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_default;
    use neowaves::WavesPreviewer;

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_change_tracking_{tag}_{}_{}_{}",
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

    /// Each test gets its own store, so they never see each other's history
    /// and never touch the developer's.
    ///
    /// The store path comes from an environment variable read when the
    /// harness is built, and the environment is process-wide -- so these
    /// tests cannot run side by side. The returned guard is held for the
    /// whole test.
    #[must_use]
    fn use_isolated_store(dir: &Path) -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NEOWAVES_SESSION_STATE", dir.join("state.sqlite3"));
        guard
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
            if !harness.state().test_session_change_check_busy()
                && !harness.state().test_session_save_in_flight()
                && !harness.state().test_session_history_busy()
                && !harness.state().test_session_open_busy()
            {
                // A couple more frames so the drain's follow-up work runs.
                harness.step();
                harness.step();
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{what} did not settle");
    }

    fn open_session(harness: &mut Harness<'static, WavesPreviewer>, session: &Path) {
        assert!(
            harness.state_mut().test_open_session_from(session),
            "open session"
        );
        settle(harness, "session open");
    }

    /// Build a session listing `files`, save it, and return its path.
    fn make_session(
        harness: &mut Harness<'static, WavesPreviewer>,
        dir: &Path,
        files: &[PathBuf],
    ) -> PathBuf {
        let session = dir.join("work.nwsess");
        harness.state_mut().test_replace_with_files(files);
        harness.step();
        assert!(
            harness.state_mut().test_save_session_to(&session),
            "initial save"
        );
        settle(harness, "initial save");
        session
    }

    #[test]
    fn the_first_open_of_a_session_reports_nothing() {
        let dir = temp_dir("first_open");
        let _store_guard = use_isolated_store(&dir);
        let audio = dir.join("a.wav");
        write_tone(&audio, 440.0);

        let mut harness = harness_default();
        assert!(
            harness.state().test_session_store_enabled(),
            "the isolated store must be in use"
        );
        let session = make_session(&mut harness, &dir, &[audio]);
        open_session(&mut harness, &session);

        assert_eq!(
            harness.state().test_session_file_changes(),
            None,
            "a session opened for the first time has nothing to compare against, \
             so every file must not be announced as new"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_somebody_replaced_is_reported_on_the_next_open() {
        let dir = temp_dir("replaced");
        let _store_guard = use_isolated_store(&dir);
        let kept = dir.join("kept.wav");
        let replaced = dir.join("replaced.wav");
        write_tone(&kept, 440.0);
        write_tone(&replaced, 440.0);

        let mut harness = harness_default();
        let session = make_session(&mut harness, &dir, &[kept.clone(), replaced.clone()]);
        open_session(&mut harness, &session);
        assert_eq!(harness.state().test_session_file_changes(), None);

        // Somebody else swaps the audio while we are away.
        write_tone(&replaced, 880.0);

        open_session(&mut harness, &session);
        let changes = harness
            .state()
            .test_session_file_changes()
            .expect("the replaced file must be reported");
        assert_eq!(
            changes,
            vec![("Changed".to_string(), "replaced.wav".to_string())],
            "only the file that actually changed"
        );
        assert!(
            harness.state().test_session_changes_since().is_some(),
            "the report must say what it is since"
        );

        // And it is not reported again: the baseline moved with it.
        open_session(&mut harness, &session);
        assert_eq!(
            harness.state().test_session_file_changes(),
            None,
            "a change already reported must not be reported forever"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_rewritten_with_the_same_bytes_is_not_reported() {
        // The whole point of hashing the second tier: re-exporting or copying
        // a file back moves its mtime without changing a sample.
        let dir = temp_dir("touched");
        let _store_guard = use_isolated_store(&dir);
        let audio = dir.join("a.wav");
        write_tone(&audio, 440.0);

        let mut harness = harness_default();
        let session = make_session(&mut harness, &dir, &[audio.clone()]);
        open_session(&mut harness, &session);

        let before = std::fs::read(&audio).expect("read");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::remove_file(&audio).expect("remove");
        std::fs::write(&audio, &before).expect("rewrite identical bytes");

        open_session(&mut harness, &session);
        assert_eq!(
            harness.state().test_session_file_changes(),
            None,
            "identical bytes must not be reported as a change"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stored_hash_survives_an_open_that_changed_nothing() {
        // The second tier can only tell "touched" from "changed" if the
        // hash it recorded is still there. An open where nothing happened
        // must not throw that away -- otherwise the guarantee holds for
        // exactly one open and then quietly turns into a false alarm.
        let dir = temp_dir("hash_survives");
        let _store_guard = use_isolated_store(&dir);
        let audio = dir.join("a.wav");
        write_tone(&audio, 440.0);

        let mut harness = harness_default();
        let session = make_session(&mut harness, &dir, &[audio.clone()]);

        // 1. First open: records the baseline, including a hash.
        open_session(&mut harness, &session);
        // 2. An open where absolutely nothing changed.
        open_session(&mut harness, &session);
        assert_eq!(harness.state().test_session_file_changes(), None);

        // 3. Rewrite with byte-identical content: mtime moves, audio does not.
        let bytes = std::fs::read(&audio).expect("read");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::remove_file(&audio).expect("remove");
        std::fs::write(&audio, &bytes).expect("rewrite identical bytes");

        open_session(&mut harness, &session);
        assert_eq!(
            harness.state().test_session_file_changes(),
            None,
            "the hash from the first open must still be there to prove the \
             bytes are unchanged; an idle open must not wipe it"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_deleted_file_is_reported_as_removed() {
        let dir = temp_dir("deleted");
        let _store_guard = use_isolated_store(&dir);
        let kept = dir.join("kept.wav");
        let doomed = dir.join("doomed.wav");
        write_tone(&kept, 440.0);
        write_tone(&doomed, 660.0);

        let mut harness = harness_default();
        let session = make_session(&mut harness, &dir, &[kept, doomed.clone()]);
        open_session(&mut harness, &session);

        std::fs::remove_file(&doomed).expect("delete");

        open_session(&mut harness, &session);
        let changes = harness
            .state()
            .test_session_file_changes()
            .expect("a deleted source must be reported");
        assert_eq!(
            changes,
            vec![("Removed".to_string(), "doomed.wav".to_string())]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_change_noticed_while_the_session_is_open_is_not_reported_again() {
        let dir = temp_dir("while_open");
        let _store_guard = use_isolated_store(&dir);
        let audio = dir.join("a.wav");
        write_tone(&audio, 440.0);

        let mut harness = harness_default();
        let session = make_session(&mut harness, &dir, &[audio.clone()]);
        open_session(&mut harness, &session);

        // Changed under our nose, and noticed at the time -- which is what
        // the folder watch does for us in the real app.
        write_tone(&audio, 880.0);
        harness
            .state_mut()
            .test_note_session_file_changed(vec![audio.clone()]);
        settle(&mut harness, "live baseline update");

        open_session(&mut harness, &session);
        assert_eq!(
            harness.state().test_session_file_changes(),
            None,
            "a change the user already watched happen must not be re-announced"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_report_stays_until_it_is_dismissed() {
        let dir = temp_dir("dismiss");
        let _store_guard = use_isolated_store(&dir);
        let audio = dir.join("a.wav");
        write_tone(&audio, 440.0);

        let mut harness = harness_default();
        let session = make_session(&mut harness, &dir, &[audio.clone()]);
        open_session(&mut harness, &session);
        write_tone(&audio, 880.0);
        open_session(&mut harness, &session);
        assert!(harness.state().test_session_file_changes().is_some());

        // A toast would have expired by now.
        for _ in 0..40 {
            harness.step();
        }
        assert!(
            harness.state().test_session_file_changes().is_some(),
            "the report has to survive until the user acts on it"
        );

        harness.state_mut().test_dismiss_session_file_changes();
        harness.step();
        assert_eq!(harness.state().test_session_file_changes(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saving_keeps_the_version_it_replaced_and_restoring_is_itself_undoable() {
        let dir = temp_dir("history");
        let _store_guard = use_isolated_store(&dir);
        let audio = dir.join("a.wav");
        write_tone(&audio, 440.0);

        let mut harness = harness_default();
        let session = make_session(&mut harness, &dir, &[audio]);
        // Two more saves, so revisions 1 and 2 end up in history.
        for _ in 0..2 {
            assert!(harness.state_mut().test_save_session_to(&session));
            settle(&mut harness, "save");
        }

        harness.state_mut().test_open_session_history();
        settle(&mut harness, "history read");
        let entries = harness.state().test_session_history();
        assert!(
            entries.len() >= 2,
            "each save that replaced a document leaves a version, got {entries:?}"
        );
        assert_eq!(
            entries.first().and_then(|e| e.0),
            Some(2),
            "newest first: the last version replaced was revision 2"
        );
        assert!(
            entries.iter().all(|e| e.2 > 0),
            "a stored version must have its bytes"
        );

        let before_restore = std::fs::read(&session).expect("read current");
        assert!(harness.state_mut().test_restore_session_history(0));
        settle(&mut harness, "restore");

        assert_ne!(
            std::fs::read(&session).expect("read restored"),
            before_restore,
            "restoring must actually put the earlier document back"
        );

        harness.state_mut().test_open_session_history();
        settle(&mut harness, "history read after restore");
        let after = harness.state().test_session_history();
        assert!(
            after.len() > entries.len(),
            "the document the restore replaced has to be recoverable too: {after:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
