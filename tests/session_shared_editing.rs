//! Two people, one `.nwsess` on a file server, no lock.
//!
//! The guarantee under test is narrow and worth stating plainly: a save that
//! would replace somebody else's work does not happen silently. It either
//! commits (nothing changed underneath it), or it stops and says so, leaving
//! both the document on disk and the local edits intact.

#[cfg(feature = "kittest")]
mod session_shared_editing {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_default;
    use neowaves::WavesPreviewer;

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_shared_editing_{tag}_{}_{}_{}",
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

    fn write_fixture(path: &Path) {
        let samples: Vec<f32> = (0..480)
            .map(|i| ((i as f32) / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.2)
            .collect();
        neowaves::wave::export_channels_audio(&[samples], 48_000, path).expect("write fixture");
    }

    /// Drive frames until the background save lands, or give up.
    fn settle_save(harness: &mut Harness<'static, WavesPreviewer>) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            harness.step();
            if !harness.state().test_session_save_in_flight() {
                // One more frame so the drain's own follow-up work runs.
                harness.step();
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("session save did not finish");
    }

    fn open_session_with_one_file(
        harness: &mut Harness<'static, WavesPreviewer>,
        dir: &Path,
        name: &str,
    ) -> PathBuf {
        let audio = dir.join("source.wav");
        if !audio.is_file() {
            write_fixture(&audio);
        }
        let session = dir.join(name);
        harness.state_mut().test_replace_with_files(&[audio]);
        harness.step();
        assert!(
            harness.state_mut().test_save_session_to(&session),
            "initial save"
        );
        harness.step();
        session
    }

    /// Simulate the colleague: replace the document with a different one,
    /// stamped as their save.
    ///
    /// Edited as text rather than through the serializer, because that is
    /// exactly what a *different build* of the app writing the same file
    /// looks like from here -- and the guarantee has to hold against those
    /// too, not only against documents this binary produced.
    fn someone_else_saves(session: &Path, who: &str, revision: u64) -> String {
        let text = std::fs::read_to_string(session).expect("read session");
        let theirs: String = text
            .lines()
            .map(|line| {
                if line.starts_with("revision = ") {
                    format!("revision = {revision}")
                } else if line.starts_with("saved_by = ") {
                    format!("saved_by = \"{who}\"")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let theirs = format!("{theirs}\n");
        assert!(
            theirs.contains(&format!("revision = {revision}")),
            "the fixture must carry a revision to conflict over"
        );
        assert!(
            theirs.contains(who),
            "the fixture must name who saved it"
        );
        assert_ne!(theirs, text, "their save has to actually differ from ours");
        std::fs::write(session, &theirs).expect("their save");
        theirs
    }

    #[test]
    fn a_second_writer_gets_a_conflict_instead_of_a_silent_overwrite() {
        let dir = temp_dir("conflict");
        let mut harness = harness_default();
        let session = open_session_with_one_file(&mut harness, &dir, "shared.nwsess");

        let theirs = someone_else_saves(&session, "tanaka", 42);

        assert!(
            harness.state_mut().test_begin_session_save(&session),
            "the save starts"
        );
        settle_save(&mut harness);

        let conflict = harness
            .state()
            .test_session_conflict()
            .expect("a save over somebody else's work must be refused");
        assert!(
            conflict.contains("tanaka"),
            "the conflict must name who wrote what is on disk, got: {conflict}"
        );
        assert!(
            conflict.contains("revision 42"),
            "and which version it is, got: {conflict}"
        );
        assert_eq!(
            std::fs::read_to_string(&session).expect("read after refusal"),
            theirs,
            "their document must be untouched"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn backing_out_of_the_save_as_picker_keeps_the_conflict_prompt() {
        // Choosing "Save As..." and then cancelling the file picker answers
        // nothing: no document was written, so the question has to stay. The
        // old code cleared the conflict before the picker ran, leaving the
        // user with no prompt, no save, and no sign anything had happened.
        let dir = temp_dir("save_as_cancelled");
        let mut harness = harness_default();
        let session = open_session_with_one_file(&mut harness, &dir, "shared.nwsess");
        let theirs = someone_else_saves(&session, "tanaka", 7);

        assert!(harness.state_mut().test_begin_session_save(&session));
        settle_save(&mut harness);
        assert!(harness.state().test_session_conflict().is_some());

        // Under kittest the picker always cancels, which is the case at issue.
        assert!(harness.state_mut().test_conflict_choose_save_as());
        harness.step();

        assert!(
            harness.state().test_session_conflict().is_some(),
            "cancelling the picker must leave the conflict prompt up"
        );
        assert_eq!(
            std::fs::read_to_string(&session).expect("read"),
            theirs,
            "and must not have written anything"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn overwriting_after_a_conflict_commits_and_leaves_a_backup() {
        let dir = temp_dir("overwrite");
        let mut harness = harness_default();
        let session = open_session_with_one_file(&mut harness, &dir, "shared.nwsess");
        let theirs = someone_else_saves(&session, "tanaka", 7);

        assert!(harness.state_mut().test_begin_session_save(&session));
        settle_save(&mut harness);
        assert!(harness.state().test_session_conflict().is_some());

        // What the prompt's Overwrite button does.
        assert!(harness
            .state_mut()
            .test_begin_session_save_forced(&session));
        settle_save(&mut harness);

        assert!(
            harness.state().test_session_conflict().is_none(),
            "a forced save clears the conflict"
        );
        assert_eq!(
            harness.state().test_session_revision(),
            Some(8),
            "the revision continues from the document that was replaced"
        );
        let backup = session.with_file_name("shared.nwsess.bak");
        assert_eq!(
            std::fs::read_to_string(&backup).expect("read backup"),
            theirs,
            "the replaced document has to remain recoverable"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saving_to_a_new_path_after_a_conflict_commits_and_clears_the_warning() {
        let dir = temp_dir("save_as");
        let mut harness = harness_default();
        let session = open_session_with_one_file(&mut harness, &dir, "shared.nwsess");
        let theirs = someone_else_saves(&session, "tanaka", 3);

        assert!(harness.state_mut().test_begin_session_save(&session));
        settle_save(&mut harness);
        assert!(harness.state().test_session_conflict().is_some());

        // What the prompt's Save As button does once a path is picked.
        let mine = dir.join("mine.nwsess");
        assert!(harness.state_mut().test_begin_session_save(&mine));
        settle_save(&mut harness);

        assert!(
            harness.state().test_session_conflict().is_none(),
            "writing somewhere else resolves the conflict"
        );
        assert!(mine.is_file(), "the fork was written");
        assert_eq!(
            std::fs::read_to_string(&session).expect("read theirs"),
            theirs,
            "their document is still theirs"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_changed_on_disk_warning_stays_up_until_it_is_acted_on() {
        let dir = temp_dir("banner");
        let mut harness = harness_default();
        let session = open_session_with_one_file(&mut harness, &dir, "shared.nwsess");

        harness
            .state_mut()
            .test_report_session_changed_on_disk("revision 12, saved by tanaka");

        // A toast would have expired by now; this must not.
        for _ in 0..30 {
            harness.step();
        }
        let banner = harness
            .state()
            .test_session_changed_on_disk()
            .expect("the warning has to survive until the user acts on it");
        assert!(banner.contains("tanaka"));
        assert!(
            harness.state_mut().test_request_session_reload_prompt(),
            "the banner offers a reload"
        );

        // Saving is how the user says "mine wins"; it clears the warning.
        assert!(harness
            .state_mut()
            .test_begin_session_save_forced(&session));
        settle_save(&mut harness);
        assert!(
            harness.state().test_session_changed_on_disk().is_none(),
            "a completed save resolves the disagreement"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_that_nobody_disturbed_still_just_saves() {
        let dir = temp_dir("happy");
        let mut harness = harness_default();
        let session = open_session_with_one_file(&mut harness, &dir, "quiet.nwsess");

        assert!(harness.state_mut().test_begin_session_save(&session));
        settle_save(&mut harness);

        assert!(
            harness.state().test_session_conflict().is_none(),
            "the common case must not have become a prompt"
        );
        assert_eq!(harness.state().test_session_revision(), Some(2));

        let _ = std::fs::remove_dir_all(dir);
    }
}
