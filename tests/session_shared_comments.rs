//! Two people talking in one `.nwsess` on a file server, with no lock.
//!
//! The guarantee is narrower than the one `session_shared_editing` tests but
//! points the other way. There, the promise is that a save never silently
//! replaces somebody's work. Here, the promise is that a comment *always*
//! lands: two people posting at once must both end up in the document, and
//! neither should ever be shown a conflict prompt to resolve, because a
//! conversation merges and a document does not.
//!
//! The second thing under test is quieter and just as important. Posting
//! rewrites the shared document, so without care every colleague's comment
//! would raise "the session changed on disk, reload it" -- a warning whose
//! remedy discards unsaved edits. A change that is only a conversation must
//! not raise it.

#[cfg(feature = "kittest")]
mod session_shared_comments {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_default;
    use neowaves::WavesPreviewer;

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_shared_comments_{tag}_{}_{}_{}",
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

    /// A second person, with their own window, on the same document.
    fn second_writer(session: &Path) -> Harness<'static, WavesPreviewer> {
        let mut harness = harness_default();
        assert!(
            harness.state_mut().test_open_session_from(session),
            "the colleague opens the shared session"
        );
        for _ in 0..8 {
            harness.step();
        }
        harness
    }

    fn bodies(harness: &Harness<'static, WavesPreviewer>) -> Vec<String> {
        let mut rows = harness.state().test_comment_bodies();
        rows.sort();
        rows
    }

    #[test]
    fn a_comment_reaches_the_document_and_a_colleague_reads_it_back() {
        let dir = temp_dir("roundtrip");
        let mut mine = harness_default();
        let session = open_session_with_one_file(&mut mine, &dir, "shared.nwsess");

        mine.state_mut()
            .test_post_comment_blocking(None, "The reverb tail is long here")
            .expect("post");

        let mut theirs = second_writer(&session);
        assert_eq!(
            bodies(&theirs),
            vec!["The reverb tail is long here".to_string()],
            "a colleague opening the session sees it"
        );

        // ...and they can reply into the same document.
        let root = mine.state().test_comments()[0].0.clone();
        theirs
            .state_mut()
            .test_post_comment_blocking(Some(&root), "Shortened it")
            .expect("reply");
        mine.state_mut().test_pull_comments();
        assert_eq!(
            bodies(&mine),
            vec![
                "Shortened it".to_string(),
                "The reverb tail is long here".to_string()
            ]
        );
    }

    #[test]
    fn two_people_posting_without_reading_each_other_both_survive() {
        let dir = temp_dir("concurrent");
        let mut mine = harness_default();
        let session = open_session_with_one_file(&mut mine, &dir, "shared.nwsess");
        let mut theirs = second_writer(&session);

        // Neither refreshes before writing: each is working from the document
        // as it was when they opened it.
        mine.state_mut()
            .test_post_comment_blocking(None, "mine")
            .expect("my post");
        theirs
            .state_mut()
            .test_post_comment_blocking(None, "theirs")
            .expect("their post");

        // The document holds both, and neither writer was shown a conflict.
        assert!(
            mine.state().test_session_conflict().is_none(),
            "posting a comment must never raise the conflict prompt"
        );
        assert!(theirs.state().test_session_conflict().is_none());

        let mut reader = second_writer(&session);
        assert_eq!(
            bodies(&reader),
            vec!["mine".to_string(), "theirs".to_string()],
            "neither post was lost"
        );

        // And a third writer's post does not disturb the first two.
        reader
            .state_mut()
            .test_post_comment_blocking(None, "third")
            .expect("third post");
        assert_eq!(
            bodies(&second_writer(&session)),
            vec!["mine".to_string(), "theirs".to_string(), "third".to_string()]
        );
    }

    #[test]
    fn a_comment_never_writes_out_the_authors_unsaved_edits() {
        let dir = temp_dir("unsaved");
        let mut mine = harness_default();
        let session = open_session_with_one_file(&mut mine, &dir, "shared.nwsess");

        // Something in this window that has not been saved.
        let before = std::fs::read_to_string(&session).expect("read session");
        mine.state_mut().test_set_search_query("only in my window");
        mine.step();

        mine.state_mut()
            .test_post_comment_blocking(None, "a note for the team")
            .expect("post");

        let after = std::fs::read_to_string(&session).expect("read session");
        assert!(
            after.contains("a note for the team"),
            "the comment did reach the document"
        );
        assert!(
            !after.contains("only in my window"),
            "posting must not push the author's unsaved editing state out with it"
        );
        assert_ne!(before, after);
    }

    #[test]
    fn a_colleagues_edit_on_disk_survives_a_comment_posted_here() {
        let dir = temp_dir("preserve");
        let mut mine = harness_default();
        let session = open_session_with_one_file(&mut mine, &dir, "shared.nwsess");

        // The colleague saves something only their document has.
        let mut theirs = second_writer(&session);
        theirs.state_mut().test_set_search_query("their search");
        theirs.step();
        assert!(
            theirs.state_mut().test_save_session_to(&session),
            "their save"
        );
        theirs.step();

        // We comment without ever reloading.
        mine.state_mut()
            .test_post_comment_blocking(None, "still here")
            .expect("post");

        let after = std::fs::read_to_string(&session).expect("read session");
        assert!(
            after.contains("their search"),
            "a comment write must merge into the document on disk, not replace it"
        );
        assert!(after.contains("still here"));
    }

    #[test]
    fn a_full_save_carries_a_colleagues_comment_rather_than_dropping_it() {
        let dir = temp_dir("fullsave");
        let mut mine = harness_default();
        let session = open_session_with_one_file(&mut mine, &dir, "shared.nwsess");

        let mut theirs = second_writer(&session);
        theirs
            .state_mut()
            .test_post_comment_blocking(None, "said while you were working")
            .expect("their post");

        // We save the whole session, having never seen their comment. Their
        // post rewrote the file, so the naive compare-and-swap would refuse
        // this -- and refusing it would mean any colleague typing put
        // everyone else's Ctrl+S behind a conflict prompt whose only remedy
        // discards unsaved edits.
        assert!(
            mine.state_mut().test_save_session_to(&session),
            "a colleague's comment must not stand between the author and their save"
        );
        mine.step();

        let after = std::fs::read_to_string(&session).expect("read session");
        assert!(
            after.contains("said while you were working"),
            "a full save unions the conversation on disk instead of overwriting it"
        );
    }

    #[test]
    fn a_colleagues_real_save_still_refuses_ours() {
        let dir = temp_dir("still-refuses");
        let mut mine = harness_default();
        let session = open_session_with_one_file(&mut mine, &dir, "shared.nwsess");

        // The same shape as above, but what they changed is not a comment.
        let mut theirs = second_writer(&session);
        theirs.state_mut().test_set_search_query("their search");
        theirs.step();
        assert!(theirs.state_mut().test_save_session_to(&session), "their save");
        theirs.step();

        // The blocking save reports a conflict as an error rather than a
        // prompt -- its callers are the CLI and tests.
        assert!(
            !mine.state_mut().test_save_session_to(&session),
            "the comment exemption must not weaken the guarantee it sits next to"
        );
        assert!(
            std::fs::read_to_string(&session)
                .expect("read session")
                .contains("their search"),
            "and nothing was written over their document"
        );
    }

    #[test]
    fn a_colleagues_comment_does_not_raise_the_reload_warning() {
        let dir = temp_dir("quiet");
        let mut mine = harness_default();
        let session = open_session_with_one_file(&mut mine, &dir, "shared.nwsess");

        let mut theirs = second_writer(&session);
        theirs
            .state_mut()
            .test_post_comment_blocking(None, "just a comment")
            .expect("their post");

        // The watch noticed the file change; the pull gets to classify it.
        mine.state_mut()
            .test_report_session_changed_and_settle("tanaka");

        assert!(
            mine.state().test_session_changed_on_disk().is_none(),
            "a document that differs only in its conversation is not a reason \
             to warn about reloading -- the remedy discards unsaved edits"
        );
        assert_eq!(
            bodies(&mine),
            vec!["just a comment".to_string()],
            "their comment arrived anyway"
        );
    }

    #[test]
    fn a_colleagues_real_save_still_raises_the_reload_warning() {
        let dir = temp_dir("loud");
        let mut mine = harness_default();
        let session = open_session_with_one_file(&mut mine, &dir, "shared.nwsess");

        let mut theirs = second_writer(&session);
        theirs.state_mut().test_set_search_query("their search");
        theirs.step();
        assert!(
            theirs.state_mut().test_save_session_to(&session),
            "their save"
        );
        theirs.step();

        mine.state_mut()
            .test_report_session_changed_and_settle("tanaka");

        assert!(
            mine.state().test_session_changed_on_disk().is_some(),
            "a change beyond the conversation must still stand the warning up"
        );
    }

    #[test]
    fn a_withdrawn_comment_stays_withdrawn_when_a_stale_copy_merges_back() {
        let dir = temp_dir("tombstone");
        let mut mine = harness_default();
        let session = open_session_with_one_file(&mut mine, &dir, "shared.nwsess");

        let id = mine
            .state_mut()
            .test_post_comment_blocking(None, "posted in haste")
            .expect("post");

        // A colleague opened before the withdrawal, so their copy still has
        // the text.
        let mut theirs = second_writer(&session);
        assert_eq!(bodies(&theirs), vec!["posted in haste".to_string()]);

        assert!(mine.state_mut().test_delete_comment(&id), "withdraw");
        mine.state_mut().test_settle_comment_jobs();

        // Their next post merges their whole conversation back in.
        theirs
            .state_mut()
            .test_post_comment_blocking(None, "unrelated")
            .expect("their post");

        let reader = second_writer(&session);
        assert_eq!(
            bodies(&reader),
            vec!["unrelated".to_string()],
            "a tombstone must outlive a stale copy of the text it replaced"
        );
    }

    #[test]
    fn a_comment_written_before_the_session_has_a_file_goes_out_with_the_save() {
        let dir = temp_dir("unsaved-session");
        let audio = dir.join("source.wav");
        write_fixture(&audio);
        let mut mine = harness_default();
        mine.state_mut().test_replace_with_files(&[audio]);
        mine.step();

        // No document to append to yet.
        mine.state_mut()
            .post_comment_for_test(None, "before the first save");
        mine.step();
        assert_eq!(
            mine.state().test_comments_pending(),
            1,
            "it is queued, and counts as unsaved work"
        );

        let session = dir.join("late.nwsess");
        assert!(mine.state_mut().test_save_session_to(&session), "first save");
        mine.step();

        assert_eq!(
            mine.state().test_comments_pending(),
            0,
            "the save carried it, so nothing is left queued"
        );
        assert!(std::fs::read_to_string(&session)
            .expect("read session")
            .contains("before the first save"));
    }
}
