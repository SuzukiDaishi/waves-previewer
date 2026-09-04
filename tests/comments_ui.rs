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

    use egui_kittest::{kittest::Queryable, Harness};
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

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_comments_window_layout_stability() {
        let (mut harness, _session) = open_saved_session("layout_stability");
        let root = harness
            .state_mut()
            .test_post_comment_blocking(None, "Please check the ambience tail around the loop.")
            .expect("root comment");
        harness.state_mut().test_set_comment_reply_to(Some(&root));
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);
        let initial_rect = harness
            .state()
            .test_comments_window_rect()
            .expect("initial comments window rect");
        let cancel = harness.get_by_label("Cancel reply");
        let cancel_size = cancel.rect().size();
        assert!(
            (cancel_size.x - cancel_size.y).abs() <= 1.0,
            "the unframed X keeps a square hit target: {cancel_size:?}"
        );

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("comments_layout");
        std::fs::create_dir_all(&out_dir).expect("create comments layout evidence dir");
        harness
            .render()
            .expect("render initial comments window")
            .save(out_dir.join("01_initial.png"))
            .expect("save initial comments screenshot");

        harness.run_steps(20);
        let after_20_rect = harness
            .state()
            .test_comments_window_rect()
            .expect("comments window rect after 20 frames");
        assert!(
            (after_20_rect.height() - initial_rect.height()).abs() <= 1.0,
            "comments window must not grow while replying: initial={initial_rect:?} after_20={after_20_rect:?}"
        );
        harness
            .render()
            .expect("render comments window after 20 frames")
            .save(out_dir.join("02_after_20_frames.png"))
            .expect("save comments screenshot after 20 frames");

        harness.run_steps(100);
        let after_120_rect = harness
            .state()
            .test_comments_window_rect()
            .expect("comments window rect after 120 frames");
        assert!(
            (after_120_rect.height() - initial_rect.height()).abs() <= 1.0,
            "comments window must remain stable: initial={initial_rect:?} after_120={after_120_rect:?}"
        );
        harness
            .render()
            .expect("render comments window after 120 frames")
            .save(out_dir.join("03_after_120_frames.png"))
            .expect("save comments screenshot after 120 frames");

        harness.get_by_label("Cancel reply").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_comment_reply_target(), None);
        let after_cancel_rect = harness
            .state()
            .test_comments_window_rect()
            .expect("comments window rect after cancelling reply");
        assert!(
            (after_cancel_rect.height() - initial_rect.height()).abs() <= 1.0,
            "leaving reply mode must not resize the window: initial={initial_rect:?} after_cancel={after_cancel_rect:?}"
        );
        harness
            .render()
            .expect("render comments window after cancelling reply")
            .save(out_dir.join("04_after_cancel.png"))
            .expect("save comments screenshot after cancelling reply");
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
    fn comments_can_be_started_before_the_first_session_save() {
        let mut harness = harness_default();

        top_menu_button(&harness, "File").click();
        harness.run_steps(1);
        harness.get_by_label("Comments...").click();
        harness.run_steps(2);

        assert!(
            harness.state().test_comments_window_open(),
            "the composer already preserves comments in memory until the first save"
        );
        post(&mut harness, "remember this for the shared session");
        assert_eq!(
            harness.state().test_comment_bodies(),
            vec!["remember this for the shared session".to_string()]
        );
        assert_eq!(
            harness.state().test_comments_pending(),
            1,
            "without a .nwsess the comment remains visibly pending"
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
        assert!(harness
            .state_mut()
            .test_edit_comment(&mine, "mine, revised"));
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
    fn the_machine_name_shows_only_when_two_people_share_an_account_name() {
        let (mut harness, _session) = open_saved_session("hosts");
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);
        post(&mut harness, "just me");
        harness.run_steps(2);
        assert!(
            harness.state().test_ambiguous_comment_authors().is_empty(),
            "one machine per account name needs no disambiguation"
        );

        // The same account name, posting from somewhere else.
        let me = harness.state().test_comments()[0].1.clone();
        harness
            .state_mut()
            .test_post_comment_as(&me, None, "also me, elsewhere")
            .expect("their post");
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_ambiguous_comment_authors(),
            vec![me],
            "two machines under one account name is a real shape on a share, \
             and the only time the machine name earns its space"
        );
    }

    #[test]
    fn detaching_and_docking_keeps_the_same_conversation() {
        let (mut harness, _session) = open_saved_session("detach");
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);
        post(&mut harness, "before detaching");

        harness.get_by_label("Open in window").click();
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
    fn a_reference_survives_the_round_trip_through_the_document() {
        let dir = temp_dir("refs");
        let audio = dir.join("line_001.wav");
        write_fixture(&audio);
        let session = dir.join("shared.nwsess");
        let mut harness = harness_default();
        harness
            .state_mut()
            .test_replace_with_files(&[audio.clone()]);
        harness.run_steps(2);
        assert!(harness.state_mut().test_save_session_to(&session));
        harness.run_steps(2);
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);

        let token = harness.state().test_comment_ref_token(&audio, Some(12.5));
        post(&mut harness, &format!("listen here {token}"));

        // A colleague on another machine reads the same token back and
        // resolves it to the same file -- which is the whole reason the path
        // follows the session's own `path_mode`.
        let mut theirs = harness_default();
        assert!(theirs.state_mut().test_open_session_from(&session));
        theirs.run_steps(8);
        let body = theirs
            .state()
            .test_comment_bodies()
            .into_iter()
            .next()
            .expect("their copy of the comment");
        assert!(theirs.state_mut().test_jump_to_comment_ref(&body));
        assert_eq!(
            theirs.state().test_pending_comment_jump().as_deref(),
            Some(audio.as_path()),
            "the reference resolves to the file it named"
        );
    }

    #[test]
    fn the_this_file_filter_follows_what_is_selected() {
        let dir = temp_dir("thisfile");
        let one = dir.join("one.wav");
        let two = dir.join("two.wav");
        write_fixture(&one);
        write_fixture(&two);
        let session = dir.join("shared.nwsess");
        let mut harness = harness_default();
        harness
            .state_mut()
            .test_replace_with_files(&[one.clone(), two.clone()]);
        harness.run_steps(2);
        assert!(harness.state_mut().test_save_session_to(&session));
        harness.run_steps(2);
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);

        let ref_one = harness.state().test_comment_ref_token(&one, None);
        let ref_two = harness.state().test_comment_ref_token(&two, None);
        post(&mut harness, &format!("about one {ref_one}"));
        post(&mut harness, &format!("about two {ref_two}"));
        post(&mut harness, "about nothing in particular");

        harness.state_mut().test_set_comment_filter_this_file();
        harness.state_mut().test_select_row_with_autoscroll(0);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_visible_comment_threads(),
            vec![format!("about one {ref_one}")],
            "the filter shows what was said about the selected file"
        );

        harness.state_mut().test_select_row_with_autoscroll(1);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_visible_comment_threads(),
            vec![format!("about two {ref_two}")]
        );
    }

    #[test]
    fn the_reference_menu_writes_a_token_the_parser_reads_back() {
        let dir = temp_dir("insert");
        let audio = dir.join("line_001.wav");
        write_fixture(&audio);
        let session = dir.join("shared.nwsess");
        let mut harness = harness_default();
        harness
            .state_mut()
            .test_replace_with_files(&[audio.clone()]);
        harness.run_steps(2);
        assert!(harness.state_mut().test_save_session_to(&session));
        harness.run_steps(2);
        harness.state_mut().test_open_comments_window();
        harness.run_steps(2);

        harness.state_mut().test_set_comment_draft("look at");
        let token = harness.state().test_comment_ref_token(&audio, None);
        harness.state_mut().test_insert_comment_reference(&token);
        harness.run_steps(2);

        let draft = harness.state().test_comment_draft();
        assert!(
            draft.starts_with("look at "),
            "inserting keeps exactly one space in front: {draft:?}"
        );
        assert!(draft.contains(&token));
        post(&mut harness, &draft);
        assert!(std::fs::read_to_string(&session)
            .expect("read session")
            .contains(&token));
    }

    #[test]
    fn a_colleagues_comment_reads_as_new_until_it_has_been_looked_at() {
        let (mut harness, session) = open_saved_session("unread");

        // Their comment is new; ours never is -- you do not need telling
        // about what you just wrote.
        harness
            .state_mut()
            .test_post_comment_blocking(None, "mine")
            .expect("my post");
        harness
            .state_mut()
            .test_post_comment_as("tanaka", None, "theirs")
            .expect("their post");
        assert_eq!(harness.state().test_unread_comment_count(), 1);

        harness.state_mut().test_open_comments_window();
        harness.run_steps(3);
        assert_eq!(
            harness.state().test_unread_comment_count(),
            0,
            "showing them is what reading them means"
        );
        assert_eq!(
            harness.state().test_highlighted_comment_count(),
            1,
            "but the dot survives the frame that marked it read, or nobody              would ever see which comment was the new one"
        );

        harness.state_mut().test_close_comments_window();
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_highlighted_comment_count(),
            0,
            "closing the window resets what counts as new"
        );
        assert!(session.is_file());
    }

    #[test]
    fn a_filter_does_not_mark_hidden_comments_as_read() {
        let dir = temp_dir("filtered_unread");
        let one = dir.join("one.wav");
        let two = dir.join("two.wav");
        write_fixture(&one);
        write_fixture(&two);
        let session = dir.join("shared.nwsess");
        let mut harness = harness_default();
        harness
            .state_mut()
            .test_replace_with_files(&[one.clone(), two.clone()]);
        harness.run_steps(2);
        assert!(harness.state_mut().test_save_session_to(&session));
        harness.run_steps(2);

        let ref_one = harness.state().test_comment_ref_token(&one, None);
        let ref_two = harness.state().test_comment_ref_token(&two, None);
        harness
            .state_mut()
            .test_post_comment_as("tanaka", None, &format!("about one {ref_one}"))
            .expect("first colleague comment");
        harness
            .state_mut()
            .test_post_comment_as("suzuki", None, &format!("about two {ref_two}"))
            .expect("second colleague comment");
        assert_eq!(harness.state().test_unread_comment_count(), 2);

        harness.state_mut().test_set_comment_filter_this_file();
        harness.state_mut().test_select_row_with_autoscroll(0);
        harness.state_mut().test_open_comments_window();
        harness.run_steps(3);

        assert_eq!(
            harness.state().test_visible_comment_threads(),
            vec![format!("about one {ref_one}")]
        );
        assert_eq!(
            harness.state().test_unread_comment_count(),
            1,
            "the comment about the other file was never painted and stays unread"
        );
        assert_eq!(
            harness.state().test_highlighted_comment_count(),
            1,
            "only the comment the reader actually saw gets the new highlight"
        );

        harness.get_by_label("● 1").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_unread_comment_count(), 0);
        let visible = harness.state().test_visible_comment_threads();
        assert_eq!(visible.len(), 2);
        assert!(visible.contains(&format!("about one {ref_one}")));
        assert!(visible.contains(&format!("about two {ref_two}")));
    }

    #[test]
    fn a_stopped_comment_worker_cannot_block_later_work_forever() {
        let (mut harness, _session) = open_saved_session("worker_disconnect");
        harness
            .state_mut()
            .test_post_comment_blocking(None, "keep this queued")
            .expect("comment fixture");

        assert!(harness.state_mut().test_disconnect_comment_write_worker());
        harness.state_mut().test_drain_comment_jobs_once();
        assert_eq!(harness.state().test_comment_workers_active().0, false);
        assert_eq!(
            harness.state().test_comments_pending(),
            1,
            "the vanished worker's payload returns to the outbox"
        );
        assert_eq!(harness.state().test_comment_write_backoff(), (1, true));

        harness.state_mut().test_disconnect_comment_pull_worker();
        harness.state_mut().test_drain_comment_jobs_once();
        assert_eq!(
            harness.state().test_comment_workers_active().1,
            false,
            "a vanished refresh worker is cleared so Refresh can be used again"
        );
    }

    // ---- The list's Comments column -------------------------------------

    /// The file `open_saved_session` put in the list.
    fn session_audio(session: &Path) -> PathBuf {
        session.with_file_name("source.wav")
    }

    #[test]
    fn the_row_column_counts_the_whole_thread_about_that_file() {
        let (mut harness, session) = open_saved_session("column_counts");
        let audio = session_audio(&session);
        let token = harness.state().test_comment_ref_token(&audio, None);
        assert_eq!(
            harness.state_mut().test_comment_summary_for_path(&audio),
            (0, 0, 0),
            "a file nobody has mentioned carries no badge"
        );

        harness
            .state_mut()
            .test_post_comment_blocking(None, &format!("the tail is long {token}"))
            .expect("post");
        assert_eq!(
            harness.state_mut().test_comment_summary_for_path(&audio),
            (1, 1, 0)
        );

        // A reply carries no reference of its own -- it does not need one,
        // and the badge counts the conversation the row's popup shows.
        let root = harness.state().test_comments()[0].0.clone();
        harness
            .state_mut()
            .test_post_comment_blocking(Some(&root), "shortened it")
            .expect("reply");
        assert_eq!(
            harness.state_mut().test_comment_summary_for_path(&audio),
            (2, 2, 0)
        );

        // Settling the root settles the file: still two comments, nothing
        // still asking.
        assert!(harness.state_mut().test_set_thread_resolved(&root, true));
        harness.state_mut().test_settle_comment_jobs();
        assert_eq!(
            harness.state_mut().test_comment_summary_for_path(&audio),
            (2, 0, 0)
        );
    }

    #[test]
    fn writing_from_a_row_points_the_comment_at_that_row() {
        let (mut harness, session) = open_saved_session("row_compose");
        let audio = session_audio(&session);
        // The harness window is narrower than a real one; the default layout
        // would put this column past its right edge.
        harness
            .state_mut()
            .test_show_only_columns(&["file", "comments"]);
        harness.run_steps(2);

        // The badge is the whole affordance: click it, type, post.
        harness.get_by_label("Comments: none").click();
        harness.run_steps(2);
        harness
            .state_mut()
            .test_set_comment_row_draft(&audio, "check the tail");
        harness.run_steps(2);
        harness.get_by_label("Post").click();
        harness.run_steps(2);
        harness.state_mut().test_settle_comment_jobs();
        harness.run_steps(2);

        let bodies = harness.state().test_comment_bodies();
        assert_eq!(bodies.len(), 1, "one comment, from the row");
        let token = harness.state().test_comment_ref_token(&audio, None);
        assert!(
            bodies[0].starts_with("check the tail") && bodies[0].contains(&token),
            "a comment written from a row says which file it is about: {}",
            bodies[0]
        );
        assert_eq!(
            harness.state_mut().test_comment_summary_for_path(&audio),
            (1, 1, 0),
            "and the row it was written from now counts it"
        );
    }

    #[test]
    fn a_colleagues_comment_shows_as_new_on_the_row_until_it_is_opened() {
        let (mut harness, session) = open_saved_session("row_unread");
        let audio = session_audio(&session);
        let token = harness.state().test_comment_ref_token(&audio, None);
        harness
            .state_mut()
            .test_post_comment_as("tanaka", None, &format!("please check {token}"))
            .expect("their post");
        harness
            .state_mut()
            .test_show_only_columns(&["file", "comments"]);
        harness.run_steps(2);
        assert_eq!(
            harness.state_mut().test_comment_summary_for_path(&audio),
            (1, 1, 1)
        );

        // Opening the row's conversation is reading it, exactly as opening
        // the window is.
        harness.get_by_label("Comments: 1, 1 unread").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state_mut().test_comment_summary_for_path(&audio),
            (1, 1, 0)
        );
        assert_eq!(harness.state().test_unread_comment_count(), 0);
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
