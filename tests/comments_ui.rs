//! The comments window, driven the way a person drives it.
//!
//! The persistence rules have their own tests in `session_shared_comments`.
//! What is checked here is the surface: that the menu opens the window, that
//! the composer posts into the right thread, that only an author may rewrite
//! their own words while anyone may settle a thread, and that the filters
//! hide threads without hiding the replies that explain them.

#[cfg(feature = "kittest")]
mod comments_ui {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use neowaves::kittest::harness_default;
    use neowaves::WavesPreviewer;

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_comments_ui_{tag}_{}_{}_{}",
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

    fn open_saved_session(tag: &str) -> (Harness<'static, WavesPreviewer>, PathBuf) {
        let dir = temp_dir(tag);
        let audio = dir.join("source.wav");
        write_fixture(&audio);
        let session = dir.join("shared.nwsess");
        let mut harness = harness_default();
        harness.state_mut().test_replace_with_files(&[audio]);
        harness.step();
        assert!(
            harness.state_mut().test_save_session_to(&session),
            "initial save"
        );
        harness.run_steps(2);
        (harness, session)
    }

    /// The topmost node with this label is the menu bar; anything in a
    /// floating window sits below it.
    fn top_menu_button<'a>(
        harness: &'a Harness<'static, WavesPreviewer>,
        label: &'a str,
    ) -> egui_kittest::Node<'a> {
        harness
            .query_all_by_label(label)
            .min_by(|a, b| {
                a.rect()
                    .min
                    .y
                    .partial_cmp(&b.rect().min.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|| panic!("no menu button labelled {label}"))
    }

    fn post(harness: &mut Harness<'static, WavesPreviewer>, body: &str) {
        harness.state_mut().test_set_comment_draft(body);
        harness.run_steps(2);
        harness.get_by_label("Post").click();
        harness.run_steps(2);
        harness.state_mut().test_settle_comment_jobs();
        harness.run_steps(2);
    }

    #[test]
    fn the_file_menu_opens_the_window_and_reads_the_document_on_the_way_in() {
        let (mut harness, session) = open_saved_session("menu");

        // A colleague said something while this window was elsewhere.
        {
            let mut theirs = harness_default();
            assert!(theirs.state_mut().test_open_session_from(&session));
            theirs.run_steps(8);
            theirs
                .state_mut()
                .test_post_comment_blocking(None, "said before you looked")
                .expect("their post");
        }

        top_menu_button(&harness, "File").click();
        harness.run_steps(1);
        harness.get_by_label("Comments...").click();
        harness.run_steps(2);
        harness.state_mut().test_settle_comment_jobs();
        harness.run_steps(2);

        assert!(harness.state().test_comments_window_open());
        assert_eq!(
            harness.state().test_comment_bodies(),
            vec!["said before you looked".to_string()],
            "opening the window reads the document, so a colleague's last few \
             minutes are already there"
        );
    }

    #[test]
    fn the_composer_posts_a_thread_and_then_replies_into_it() {
        let (mut harness, _session) = open_saved_session("compose");
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);

        post(&mut harness, "The reverb tail is long");
        assert_eq!(
            harness.state().test_comment_threads(),
            vec![("The reverb tail is long".to_string(), 0, false)]
        );

        // Reply, the way the Reply button sets it up.
        let root = harness.state().test_comments()[0].0.clone();
        harness.state_mut().test_set_comment_reply_to(Some(&root));
        post(&mut harness, "Shortened it");

        let threads = harness.state().test_comment_threads();
        assert_eq!(threads.len(), 1, "the reply belongs under its root");
        assert_eq!(threads[0].1, 1);

        // Posting clears the reply target, so the next comment is a thread of
        // its own rather than silently continuing somebody else's.
        post(&mut harness, "Unrelated");
        assert_eq!(harness.state().test_comment_threads().len(), 2);
    }

    #[test]
    fn only_the_author_may_rewrite_or_withdraw_their_own_words() {
        let (mut harness, session) = open_saved_session("authorship");
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);
        post(&mut harness, "mine");

        let mine = harness.state().test_comments()[0].0.clone();
        assert!(harness.state_mut().test_edit_comment(&mine, "mine, revised"));
        harness.state_mut().test_settle_comment_jobs();
        assert_eq!(
            harness.state().test_comment_bodies(),
            vec!["mine, revised".to_string()]
        );

        // Somebody else's comment is not ours to touch.
        let theirs = harness
            .state_mut()
            .test_post_comment_as("tanaka", None, "theirs")
            .expect("their post");
        assert!(
            !harness.state_mut().test_edit_comment(&theirs, "not yours"),
            "a colleague's comment must not be rewritable from here"
        );
        assert!(!harness.state_mut().test_delete_comment(&theirs));

        // But settling the thread is anyone's to do: a thread is the team's.
        assert!(harness.state_mut().test_set_thread_resolved(&theirs, true));
        harness.state_mut().test_settle_comment_jobs();
        assert!(std::fs::read_to_string(&session)
            .expect("read session")
            .contains("resolved_by"));
    }

    #[test]
    fn a_filter_hides_a_thread_but_never_a_reply_from_the_thread_it_answers() {
        let (mut harness, _session) = open_saved_session("filters");
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);

        post(&mut harness, "open question");
        let open_root = harness.state().test_comments()[0].0.clone();
        post(&mut harness, "settled question");
        let settled_root = harness
            .state()
            .test_comments()
            .iter()
            .find(|c| c.2 == "settled question")
            .expect("second thread")
            .0
            .clone();
        harness
            .state_mut()
            .test_set_comment_reply_to(Some(&settled_root));
        post(&mut harness, "answered here");
        assert!(harness
            .state_mut()
            .test_set_thread_resolved(&settled_root, true));
        harness.state_mut().test_settle_comment_jobs();
        harness.run_steps(2);

        // Both threads are still there; the filter only decides what is drawn.
        let threads = harness.state().test_comment_threads();
        assert_eq!(threads.len(), 2);
        assert!(threads.iter().any(|(body, replies, resolved)| {
            body == "settled question" && *replies == 1 && *resolved
        }));
        assert!(threads
            .iter()
            .any(|(body, _, resolved)| body == "open question" && !*resolved));
        assert_eq!(open_root.len(), 32);
    }

    #[test]
    fn detaching_and_docking_keeps_the_same_conversation() {
        let (mut harness, _session) = open_saved_session("detach");
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);
        post(&mut harness, "before detaching");

        harness.get_by_label("⧉").click();
        harness.run_steps(2);
        assert!(harness.state().test_comments_detached());
        assert!(
            harness.state().test_comments_window_open(),
            "detaching moves the panel, it does not close it"
        );
        assert_eq!(
            harness.state().test_comment_bodies(),
            vec!["before detaching".to_string()]
        );
    }

    #[test]
    fn a_withdrawn_comment_leaves_its_replies_readable() {
        let (mut harness, _session) = open_saved_session("withdraw");
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);

        post(&mut harness, "posted in haste");
        let root = harness.state().test_comments()[0].0.clone();
        harness.state_mut().test_set_comment_reply_to(Some(&root));
        post(&mut harness, "but this answer still matters");

        assert!(harness.state_mut().test_delete_comment(&root));
        harness.state_mut().test_settle_comment_jobs();
        harness.run_steps(2);

        let threads = harness.state().test_comment_threads();
        assert_eq!(threads.len(), 1, "the tombstone keeps the thread standing");
        assert_eq!(
            threads[0].1, 1,
            "withdrawing a root must not take its replies down with it"
        );
        assert_eq!(
            harness.state().test_comment_bodies(),
            vec!["but this answer still matters".to_string()]
        );
    }
}
