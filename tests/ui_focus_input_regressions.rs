#[cfg(feature = "kittest")]
mod ui_focus_input_regressions {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui::{Key, Modifiers};
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use neowaves::app::RateMode;
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
            "neowaves_ui_focus_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn harness_with_folder(dir: PathBuf) -> Harness<'static, WavesPreviewer> {
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        cfg.open_first = false;
        harness_with_startup(cfg)
    }

    fn write_fixture_wav(path: &std::path::Path, sr: u32, secs: f32) {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        let mut l = Vec::with_capacity(frames);
        let mut r = Vec::with_capacity(frames);
        for i in 0..frames {
            let t = i as f32 / sr as f32;
            l.push((t * 220.0 * std::f32::consts::TAU).sin() * 0.25);
            r.push((t * 440.0 * std::f32::consts::TAU).sin() * 0.20);
        }
        neowaves::wave::export_channels_audio(&[l, r], sr, path).expect("export fixture wav");
    }

    fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().scan_in_progress && !harness.state().files.is_empty() {
                return;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("scan timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn top_menu_button<'a>(
        harness: &'a Harness<'static, WavesPreviewer>,
        label: &'a str,
    ) -> egui_kittest::Node<'a> {
        let nodes: Vec<_> = harness.query_all_by_label(label).collect();
        nodes
            .into_iter()
            .min_by(|a, b| {
                a.rect()
                    .min
                    .y
                    .partial_cmp(&b.rect().min.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|| panic!("node not found: {label}"))
    }

    fn text_input_by_value<'a>(
        harness: &'a Harness<'static, WavesPreviewer>,
        value: &'a str,
    ) -> egui_kittest::Node<'a> {
        harness
            .query_all_by_value(value)
            .find(|node| node.accesskit_node().role() == egui::accesskit::Role::TextInput)
            .unwrap_or_else(|| panic!("text input not found for value: {value}"))
    }

    #[test]
    fn topbar_speed_dragvalue_accepts_text_input() {
        let dir = make_temp_dir("topbar_rate");
        let wav = dir.join("rate_input.wav");
        write_fixture_wav(&wav, 48_000, 0.6);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.0);
        harness.run_steps(2);

        {
            let rate_node = harness.get_by_value("1.00 x");
            rate_node.click();
        }
        harness.run_steps(1);
        for _ in 0..8 {
            harness.key_press(Key::Backspace);
        }
        {
            let rate_node = harness.get_by_value("1.00 x");
            rate_node.type_text("1.75");
        }
        harness.key_press(Key::Enter);
        harness.run_steps(3);

        let actual = harness.state().test_playback_rate();
        assert!(
            (actual - 1.75).abs() < 0.05,
            "playback_rate should be text-editable: got {actual}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn topbar_playback_mode_reset_restores_speed_rate() {
        let mut harness = harness_with_startup(StartupConfig::default());
        harness.state_mut().test_set_rate_mode(RateMode::Speed);
        harness.state_mut().test_set_playback_rate(1.5);

        assert!(harness.state().test_topbar_playback_mode_reset_enabled());
        assert!(harness.state_mut().test_reset_topbar_playback_mode_value());
        let actual = harness.state().test_playback_rate();
        assert!(
            (actual - 1.0).abs() <= 0.0001,
            "speed reset should restore 1.00x: got {actual}"
        );
        assert!(!harness.state().test_topbar_playback_mode_reset_enabled());
    }

    #[test]
    fn topbar_playback_mode_reset_restores_stretch_rate() {
        let mut harness = harness_with_startup(StartupConfig::default());
        harness
            .state_mut()
            .test_set_rate_mode(RateMode::TimeStretch);
        harness.state_mut().test_set_playback_rate(0.5);

        assert!(harness.state().test_topbar_playback_mode_reset_enabled());
        assert!(harness.state_mut().test_reset_topbar_playback_mode_value());
        let actual = harness.state().test_playback_rate();
        assert!(
            (actual - 1.0).abs() <= 0.0001,
            "stretch reset should restore 1.00x: got {actual}"
        );
        assert!(!harness.state().test_topbar_playback_mode_reset_enabled());
    }

    #[test]
    fn topbar_playback_mode_reset_restores_pitch_semitones() {
        let mut harness = harness_with_startup(StartupConfig::default());
        harness.state_mut().test_set_rate_mode(RateMode::PitchShift);
        harness.state_mut().test_set_pitch_semitones(5.0);

        assert!(harness.state().test_topbar_playback_mode_reset_enabled());
        assert!(harness.state_mut().test_reset_topbar_playback_mode_value());
        let actual = harness.state().test_pitch_semitones();
        assert!(
            actual.abs() <= 0.0001,
            "pitch reset should restore 0.0 st: got {actual}"
        );
        assert!(!harness.state().test_topbar_playback_mode_reset_enabled());
    }

    #[test]
    fn topbar_playback_mode_reset_disabled_at_defaults() {
        let mut harness = harness_with_startup(StartupConfig::default());

        harness.state_mut().test_set_rate_mode(RateMode::Speed);
        harness.state_mut().test_set_playback_rate(1.0);
        assert!(!harness.state().test_topbar_playback_mode_reset_enabled());
        assert!(!harness.state_mut().test_reset_topbar_playback_mode_value());

        harness
            .state_mut()
            .test_set_rate_mode(RateMode::TimeStretch);
        harness.state_mut().test_set_playback_rate(1.0);
        assert!(!harness.state().test_topbar_playback_mode_reset_enabled());
        assert!(!harness.state_mut().test_reset_topbar_playback_mode_value());

        harness.state_mut().test_set_rate_mode(RateMode::PitchShift);
        harness.state_mut().test_set_pitch_semitones(0.0);
        assert!(!harness.state().test_topbar_playback_mode_reset_enabled());
        assert!(!harness.state_mut().test_reset_topbar_playback_mode_value());
    }

    #[test]
    fn list_gain_dragvalue_accepts_text_input() {
        let dir = make_temp_dir("list_gain");
        let wav = dir.join("gain_input.wav");
        write_fixture_wav(&wav, 48_000, 0.6);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_list_gain_column_visible(true);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);

        {
            let gain_node = harness.get_by_value("0.0 dB");
            gain_node.click();
        }
        harness.run_steps(1);
        for _ in 0..8 {
            harness.key_press(Key::Backspace);
        }
        {
            let gain_node = harness.get_by_value("0.0 dB");
            gain_node.type_text("-6.0");
        }
        harness.key_press(Key::Enter);
        harness.run_steps(3);

        let actual = harness
            .state()
            .test_selected_pending_gain_db()
            .expect("selected gain");
        assert!(
            (actual - (-6.0)).abs() < 0.2,
            "list gain should be text-editable: got {actual}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_text_inputs_accept_typing() {
        let dir = make_temp_dir("settings_text");
        let wav = dir.join("settings_input.wav");
        write_fixture_wav(&wav, 48_000, 0.6);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);

        harness
            .state_mut()
            .test_set_export_name_template("focus_template_token");
        harness.run_steps(2);

        top_menu_button(&harness, "Tools").click();
        harness.run_steps(1);
        harness.get_by_label("Settings...").click();
        harness.run_steps(2);
        assert!(harness.state().test_show_export_settings());

        {
            let template_node = text_input_by_value(&harness, "focus_template_token");
            template_node.click();
        }
        harness.run_steps(1);
        {
            let template_node = text_input_by_value(&harness, "focus_template_token");
            template_node.type_text("_ok");
        }
        harness.run_steps(2);
        assert!(
            harness.state().test_export_name_template().contains("_ok"),
            "name template should accept text typing"
        );

        // Close settings first; topbar menus are intentionally de-prioritized while dialogs are open.
        harness.state_mut().test_set_show_export_settings(false);
        harness.run_steps(1);
        assert!(!harness.state().test_show_export_settings());

        harness
            .state_mut()
            .test_set_show_transcription_settings(true);
        harness.run_steps(2);
        assert!(harness.state().test_show_transcription_settings());

        // Ctrl+F should still move focus to the search box after closing dialogs.
        harness
            .state_mut()
            .test_set_show_transcription_settings(false);
        harness.run_steps(1);
        harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
        harness.run_steps(1);
        assert!(harness
            .ctx
            .memory(|m| m.has_focus(egui::Id::new("search_box"))));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_ctrl_a_selects_all_rows() {
        let dir = make_temp_dir("ctrl_a");
        for i in 0..3 {
            let wav = dir.join(format!("ctrl_a_{i}.wav"));
            write_fixture_wav(&wav, 48_000, 0.4 + i as f32 * 0.1);
        }

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);

        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.run_steps(2);

        let total = harness.state().test_files_len();
        let selected = harness.state().test_selected_multi_len();
        assert_eq!(selected, total, "Ctrl+A should select all list rows");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_arrow_navigation_recovers_after_topbar_focus() {
        let dir = make_temp_dir("arrow_focus_recover");
        for i in 0..3 {
            let wav = dir.join(format!("arrow_focus_{i}.wav"));
            write_fixture_wav(&wav, 48_000, 0.3 + i as f32 * 0.1);
        }

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);
        let before = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected before");

        // Reproduce: focus moves away from list to a topbar widget.
        harness.get_by_label("Speed").click();
        harness.run_steps(1);

        harness.key_press(Key::ArrowDown);
        harness.run_steps(2);
        harness.key_press(Key::ArrowDown);
        harness.run_steps(2);

        let after = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected after");
        assert_ne!(after, before, "ArrowDown should move list selection");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_arrow_navigation_recovers_after_dragvalue_text_focus() {
        let dir = make_temp_dir("arrow_dragvalue_recover");
        for i in 0..3 {
            let wav = dir.join(format!("arrow_dragvalue_{i}.wav"));
            write_fixture_wav(&wav, 48_000, 0.3 + i as f32 * 0.1);
        }

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);
        let before = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected before");

        // Focus topbar DragValue text entry.
        {
            let rate_node = harness.get_by_value("1.00 x");
            rate_node.click();
        }
        harness.run_steps(1);
        harness.key_press(Key::Backspace);
        harness.run_steps(1);

        harness.key_press(Key::ArrowDown);
        harness.run_steps(2);

        let after = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected after");
        assert_ne!(
            after, before,
            "ArrowDown should move selection even after DragValue text focus"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
