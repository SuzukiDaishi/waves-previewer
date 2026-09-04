#[cfg(feature = "kittest")]
mod ui_focus_input_regressions {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui::{Key, Modifiers, MouseWheelUnit};
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

    fn click_at(harness: &mut Harness<'static, WavesPreviewer>, pos: egui::Pos2) {
        harness.hover_at(pos);
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
    }

    fn wheel_at(harness: &mut Harness<'static, WavesPreviewer>, pos: egui::Pos2, delta_y: f32) {
        harness.hover_at(pos);
        harness.event(egui::Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: egui::vec2(0.0, delta_y),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(3);
    }

    fn settings_scroll_offset(harness: &Harness<'static, WavesPreviewer>) -> f32 {
        harness.ctx.data(|data| {
            data.get_temp::<f32>(egui::Id::new("test_settings_scroll_offset"))
                .unwrap_or(0.0)
        })
    }

    fn settings_window_rect(harness: &Harness<'static, WavesPreviewer>) -> egui::Rect {
        harness.ctx.data(|data| {
            data.get_temp::<egui::Rect>(egui::Id::new("test_settings_window_rect"))
                .expect("Settings window rect")
        })
    }

    fn effect_graph_canvas_rect(harness: &Harness<'static, WavesPreviewer>) -> egui::Rect {
        harness.ctx.data(|data| {
            data.get_temp::<egui::Rect>(egui::Id::new("test_effect_graph_canvas_rect"))
                .expect("Effect Graph canvas rect")
        })
    }

    fn command_wheel_at(
        harness: &mut Harness<'static, WavesPreviewer>,
        pos: egui::Pos2,
        delta_y: f32,
    ) {
        harness.hover_at(pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, delta_y),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::COMMAND,
            },
            Modifiers::COMMAND,
        );
        harness.run_steps(3);
    }

    fn wait_for_editor_ready(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().test_is_editor_workspace_active()
                && !harness.state().test_tab_loading()
                && harness.state().test_tab_samples_len() > 0
            {
                return;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("editor decode timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn lowest_node_by_value<'a>(
        harness: &'a Harness<'static, WavesPreviewer>,
        value: &'a str,
    ) -> egui_kittest::Node<'a> {
        harness
            .query_all_by_value(value)
            .max_by(|a, b| {
                a.rect()
                    .min
                    .y
                    .partial_cmp(&b.rect().min.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|| panic!("node not found for value: {value}"))
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
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_list_gain_column_visible(true);
        harness.state_mut().test_move_list_gain_column_first();
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);

        {
            let gain_node = lowest_node_by_value(&harness, "0.0 dB");
            gain_node.click();
        }
        harness.run_steps(2);
        for _ in 0..8 {
            harness.key_press(Key::Backspace);
        }
        harness.event(egui::Event::Text("-6.0".to_owned()));
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
    fn wheel_follows_the_pointer_between_settings_and_list() {
        let dir = make_temp_dir("scroll_surface_focus");
        for i in 0..40 {
            let wav = dir.join(format!("scroll_focus_{i:02}.wav"));
            write_fixture_wav(&wav, 48_000, 0.05);
        }

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        harness.run_steps(3);
        assert_eq!(harness.state().test_ui_input_focus_name(), "list");

        harness.state_mut().test_set_show_export_settings(true);
        harness.run_steps(3);
        assert!(
            harness
                .state()
                .test_ui_input_focus_is_floating("settings_window"),
            "a newly opened Settings window must receive scroll focus"
        );

        let list_before = harness.state().test_list_scroll_row();
        let settings_before = settings_scroll_offset(&harness);
        let settings_pos = egui::pos2(640.0, 360.0);
        wheel_at(&mut harness, settings_pos, -240.0);
        let settings_after = settings_scroll_offset(&harness);
        assert!(
            settings_after > settings_before + 1.0,
            "Settings should consume its focused wheel input: {settings_before} -> {settings_after}"
        );
        assert_eq!(
            harness.state().test_list_scroll_row(),
            list_before,
            "the List must not scroll behind Settings"
        );

        // The wheel follows the pointer: moving onto the uncovered List
        // scrolls it without needing a click first.
        let uncovered_list_pos = egui::pos2(1180.0, 300.0);
        wheel_at(&mut harness, uncovered_list_pos, -180.0);
        let list_after = harness.state().test_list_scroll_row();
        assert!(
            list_after > list_before,
            "hovering the List should be enough to scroll it: {list_before} -> {list_after}"
        );
        assert_eq!(
            settings_scroll_offset(&harness),
            settings_after,
            "Settings must not scroll while the pointer is over the List"
        );

        // The click is not swallowed: it still selects the background surface.
        click_at(&mut harness, uncovered_list_pos);
        assert_eq!(harness.state().test_ui_input_focus_name(), "list");

        // Moving back over Settings hands the wheel straight back, and the
        // List behind it stays put -- this is the reported bug.
        wheel_at(&mut harness, settings_pos, -180.0);
        assert!(
            settings_scroll_offset(&harness) > settings_after + 1.0,
            "hovering Settings should scroll Settings"
        );
        assert_eq!(
            harness.state().test_list_scroll_row(),
            list_after,
            "the List must not scroll behind Settings, even after being clicked"
        );

        harness.state_mut().test_set_show_export_settings(false);
        harness.run_steps(2);
        assert_eq!(harness.state().test_ui_input_focus_name(), "list");

        // Multiple floating windows use MRU restoration rather than falling
        // through to whichever happens to render first.
        harness.state_mut().test_set_show_export_settings(true);
        harness.run_steps(2);
        harness
            .state_mut()
            .test_set_show_transcription_settings(true);
        harness.run_steps(2);
        assert!(harness
            .state()
            .test_ui_input_focus_is_floating("transcription_settings_window"));
        harness
            .state_mut()
            .test_set_show_transcription_settings(false);
        harness.run_steps(2);
        assert!(harness
            .state()
            .test_ui_input_focus_is_floating("settings_window"));
        harness.state_mut().test_set_show_export_settings(false);
        harness.run_steps(2);
        assert_eq!(harness.state().test_ui_input_focus_name(), "list");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_wheel_zoom_requires_editor_scroll_focus() {
        let dir = make_temp_dir("editor_scroll_focus");
        let wav = dir.join("editor_scroll_focus.wav");
        write_fixture_wav(&wav, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&wav));
        wait_for_editor_ready(&mut harness);
        harness.run_steps(3);
        assert_eq!(harness.state().test_ui_input_focus_name(), "editor");

        let before = harness
            .state()
            .test_tab_samples_per_px()
            .expect("editor zoom before");

        harness.state_mut().test_set_show_export_settings(true);
        harness.run_steps(3);
        assert!(harness
            .state()
            .test_ui_input_focus_is_floating("settings_window"));
        let settings_rect = settings_window_rect(&harness);
        let inspector_left = harness
            .state()
            .test_editor_inspector_rect()
            .expect("editor inspector rect")
            .left();
        let exposed_x = if settings_rect.left() > 80.0 {
            settings_rect.left() - 40.0
        } else {
            (settings_rect.right() + 40.0).min(inspector_left - 80.0)
        };
        let canvas_pos = egui::pos2(exposed_x.max(40.0), settings_rect.center().y);
        command_wheel_at(&mut harness, canvas_pos, 120.0);
        let blocked = harness
            .state()
            .test_tab_samples_per_px()
            .expect("editor zoom while settings focused");
        assert_eq!(blocked, before, "inactive Editor must ignore wheel zoom");

        click_at(&mut harness, canvas_pos);
        assert_eq!(
            harness.state().test_ui_input_focus_name(),
            "editor",
            "click at {canvas_pos:?} should be outside Settings {settings_rect:?} and before inspector x={inspector_left}"
        );
        command_wheel_at(&mut harness, canvas_pos, 120.0);
        let focused = harness
            .state()
            .test_tab_samples_per_px()
            .expect("editor zoom after focus");
        assert_ne!(focused, blocked, "clicked Editor should accept wheel zoom");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn effect_graph_wheel_zoom_requires_effect_graph_scroll_focus() {
        let dir = make_temp_dir("effect_graph_scroll_focus");
        let wav = dir.join("effect_graph_scroll_focus.wav");
        write_fixture_wav(&wav, 48_000, 0.2);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        harness.state_mut().test_open_effect_graph_workspace();
        harness.run_steps(3);
        assert_eq!(harness.state().test_ui_input_focus_name(), "effect_graph");
        let before = harness.state().test_effect_graph_canvas_zoom();

        harness.state_mut().test_set_show_export_settings(true);
        harness.run_steps(3);
        let settings_rect = settings_window_rect(&harness);
        let canvas_rect = effect_graph_canvas_rect(&harness);
        let exposed_x = if canvas_rect.left() + 20.0 < settings_rect.left() {
            (canvas_rect.left() + settings_rect.left()) * 0.5
        } else {
            (settings_rect.right() + canvas_rect.right()) * 0.5
        };
        let canvas_pos = egui::pos2(exposed_x, canvas_rect.center().y);
        assert!(
            canvas_rect.contains(canvas_pos) && !settings_rect.contains(canvas_pos),
            "test requires an exposed Effect Graph canvas point: canvas={canvas_rect:?}, settings={settings_rect:?}, point={canvas_pos:?}"
        );

        command_wheel_at(&mut harness, canvas_pos, 120.0);
        assert_eq!(
            harness.state().test_effect_graph_canvas_zoom(),
            before,
            "inactive Effect Graph must ignore wheel zoom"
        );

        click_at(&mut harness, canvas_pos);
        assert_eq!(harness.state().test_ui_input_focus_name(), "effect_graph");
        command_wheel_at(&mut harness, canvas_pos, 120.0);
        assert_ne!(
            harness.state().test_effect_graph_canvas_zoom(),
            before,
            "clicked Effect Graph should accept wheel zoom"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A caret in a dialog owns Ctrl+A, Delete and Space. The list behind it
    /// must not select all its rows, delete them, or start playback.
    #[test]
    fn text_edit_focus_scopes_keys_to_the_field() {
        let dir = make_temp_dir("scoped_keys");
        for i in 0..4 {
            let wav = dir.join(format!("scoped_keys_{i}.wav"));
            write_fixture_wav(&wav, 48_000, 0.3);
        }

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);

        let files_before = harness.state().test_files_len();
        let selected_before = harness.state().test_selected_multi_len();
        assert!(
            selected_before < files_before,
            "precondition: not everything is selected yet"
        );

        harness
            .state_mut()
            .test_set_export_name_template("scoped_keys_token");
        harness.state_mut().test_set_show_export_settings(true);
        harness.run_steps(3);
        {
            let template_node = text_input_by_value(&harness, "scoped_keys_token");
            template_node.click();
        }
        harness.run_steps(2);
        assert!(
            harness.state().test_input_scope_text_editing(),
            "clicking the Settings text field must put a caret in it"
        );
        assert!(
            !harness.state().test_list_owns_surface_keys(),
            "the List must not own surface keys while a dialog field has the caret"
        );

        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_selected_multi_len(),
            selected_before,
            "Ctrl+A belongs to the focused text field, not the list behind it"
        );

        harness.key_press(Key::Delete);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_files_len(),
            files_before,
            "Delete must not remove list rows while a dialog field has the caret"
        );

        assert!(!harness.state().test_audio_is_playing());
        harness.key_press(Key::Space);
        harness.run_steps(2);
        assert!(
            !harness.state().test_audio_is_playing(),
            "Space types a space in the field; it must not toggle playback"
        );

        // Closing the dialog hands the keys back to the list.
        harness.state_mut().test_set_show_export_settings(false);
        harness.run_steps(3);
        assert!(harness.state().test_list_owns_surface_keys());
        harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_selected_multi_len(),
            files_before,
            "Ctrl+A selects the whole list again once the dialog is closed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dialog with no text field at all still owns the unmodified keys while
    /// it is the surface the user is in.
    #[test]
    fn open_dialog_owns_surface_keys_without_a_text_field() {
        let dir = make_temp_dir("dialog_owns_keys");
        for i in 0..4 {
            let wav = dir.join(format!("dialog_owns_{i}.wav"));
            write_fixture_wav(&wav, 48_000, 0.3);
        }

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);
        let files_before = harness.state().test_files_len();

        harness.state_mut().test_set_show_list_columns_window(true);
        harness.run_steps(3);
        assert!(
            !harness.state().test_list_owns_surface_keys(),
            "an open dialog owns the frame's surface keys"
        );

        harness.key_press(Key::Delete);
        harness.run_steps(2);
        assert_eq!(harness.state().test_files_len(), files_before);

        harness.state_mut().test_set_show_list_columns_window(false);
        harness.run_steps(3);
        assert!(harness.state().test_list_owns_surface_keys());

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

    /// Deliver several presses of one key inside a single frame, the way
    /// auto-repeat stacks up while a frame runs long.
    ///
    /// `Harness::key_press` cannot: it queues events, and the harness gives
    /// every queued event a frame of its own -- which is exactly the case
    /// that never had the bug.
    fn press_key_times_in_one_frame(
        harness: &mut Harness<'static, WavesPreviewer>,
        key: Key,
        times: usize,
    ) {
        for _ in 0..times {
            harness.input_mut().events.push(egui::Event::Key {
                key,
                pressed: true,
                modifiers: Modifiers::default(),
                repeat: true,
                physical_key: None,
            });
        }
        harness.step();
    }

    fn list_of_fixtures(tag: &str, count: usize) -> (PathBuf, Harness<'static, WavesPreviewer>) {
        let dir = make_temp_dir(tag);
        for i in 0..count {
            write_fixture_wav(&dir.join(format!("row_{i:02}.wav")), 48_000, 0.2);
        }
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);
        (dir, harness)
    }

    #[test]
    fn a_held_arrow_does_not_lose_rows_to_a_slow_frame() {
        let (dir, mut harness) = list_of_fixtures("arrow_repeat", 8);

        // Three repeats in one frame: what the keyboard produced while the
        // previous frame was busy. Acting on one and dropping the rest is
        // what makes a held arrow stall on a row.
        press_key_times_in_one_frame(&mut harness, Key::ArrowDown, 3);
        assert_eq!(
            harness.state().test_selected_row(),
            Some(3),
            "a frame carrying three repeats moves three rows"
        );

        press_key_times_in_one_frame(&mut harness, Key::ArrowUp, 2);
        assert_eq!(harness.state().test_selected_row(), Some(1));

        // The ends still clamp rather than wrapping or overshooting.
        press_key_times_in_one_frame(&mut harness, Key::ArrowUp, 12);
        assert_eq!(harness.state().test_selected_row(), Some(0));
        press_key_times_in_one_frame(&mut harness, Key::ArrowDown, 40);
        assert_eq!(harness.state().test_selected_row(), Some(7));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn focus_that_left_the_list_under_an_arrow_comes_straight_back() {
        let (dir, mut harness) = list_of_fixtures("arrow_focus_guard", 5);

        press_key_times_in_one_frame(&mut harness, Key::ArrowDown, 1);
        assert_eq!(harness.state().test_selected_row(), Some(1));

        // What egui's own arrow-key focus navigation does when the list's
        // lock filter is not yet in place: it resolves after the list has
        // drawn, and focus lands on a widget outside it. A caret there owns
        // every key the list needs, and nothing in the list may ask for the
        // focus back -- the row stops moving until it is clicked.
        let ctx = harness.ctx.clone();
        harness.state_mut().test_move_focus_to_search_box(&ctx);
        harness.step();

        assert!(
            harness.state().test_list_widget_has_focus(&ctx),
            "a focus that moved with no pointer press belongs back in the list"
        );
        press_key_times_in_one_frame(&mut harness, Key::ArrowDown, 1);
        assert_eq!(
            harness.state().test_selected_row(),
            Some(2),
            "and the next arrow moves the selection, not a caret"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_chord_that_moves_focus_on_purpose_keeps_it() {
        let (dir, mut harness) = list_of_fixtures("arrow_then_search", 5);
        let ctx = harness.ctx.clone();

        press_key_times_in_one_frame(&mut harness, Key::ArrowDown, 1);
        // Ctrl+F is the search box, one frame after an arrow as much as at
        // any other time: taking focus back from a chord the user typed
        // would be the same bug from the other side.
        harness.key_press_modifiers(Modifiers::COMMAND, Key::F);
        harness.run_steps(2);

        assert!(
            !harness.state().test_list_widget_has_focus(&ctx),
            "the search box asked for focus and should still have it"
        );

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

    /// A topbar DragValue in text-entry mode owns its arrows; the list behind
    /// it must not move too. Leaving the field hands them straight back, so
    /// the list never ends up permanently unnavigable.
    #[test]
    fn dragvalue_text_entry_owns_arrows_then_returns_them() {
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
        assert!(
            harness.state().test_input_scope_text_editing(),
            "a clicked DragValue is a live text entry in egui"
        );

        harness.key_press(Key::ArrowDown);
        harness.run_steps(2);
        let during = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected during edit");
        assert_eq!(
            during, before,
            "ArrowDown belongs to the field being edited, not to the list behind it"
        );

        // Escape leaves the field and returns keyboard ownership to the list.
        harness.key_press(Key::Escape);
        harness.run_steps(2);
        assert!(!harness.state().test_input_scope_text_editing());
        assert!(harness.state().test_list_owns_surface_keys());

        harness.key_press(Key::ArrowDown);
        harness.run_steps(2);
        let after = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected after");
        assert_ne!(
            after, before,
            "ArrowDown should move the list again once the field is left"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Click the topbar volume fader -- which is how it takes focus -- and then
    /// put the monitor at `db`.
    ///
    /// The whole allocated rect is the control, so the click that focuses it
    /// also writes the volume from its own position. The level under test is
    /// set afterwards so the key being exercised is the only thing that moved
    /// it.
    fn focus_volume_fader_at(harness: &mut Harness<'static, WavesPreviewer>, db: f32) {
        let rect = harness
            .state()
            .test_topbar_volume_rect()
            .expect("volume control rect");
        click_at(harness, egui::pos2(rect.left() + 6.0, rect.center().y));
        harness.state_mut().test_set_volume_db(db);
        harness.run_steps(2);
        assert_eq!(harness.state().test_volume_db(), db);
    }

    /// The everyday gesture, guarded because the fader now reads its position
    /// from `Response::interact_pointer_pos` rather than the global pointer
    /// state, and is interacted under a fixed id rather than an allocated one.
    #[test]
    fn the_volume_fader_still_follows_a_pointer_drag() {
        let dir = make_temp_dir("volume_drag");
        let wav = dir.join("volume_drag.wav");
        write_fixture_wav(&wav, 48_000, 0.5);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_volume_db(-60.0);
        harness.run_steps(2);

        let rect = harness
            .state()
            .test_topbar_volume_rect()
            .expect("volume control rect");
        let y = rect.center().y;
        // Press near the left of the track and drag right: the taper puts unity
        // at 90% of the travel, so this has to climb a long way without
        // reaching the top.
        harness.hover_at(egui::pos2(rect.left() + 70.0, y));
        harness.event(egui::Event::PointerButton {
            pos: egui::pos2(rect.left() + 70.0, y),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
        let after_press = harness.state().test_volume_db();

        harness.hover_at(egui::pos2(rect.right() - 70.0, y));
        harness.run_steps(2);
        let dragged = harness.state().test_volume_db();
        harness.event(egui::Event::PointerButton {
            pos: egui::pos2(rect.right() - 70.0, y),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);

        assert!(
            dragged > after_press,
            "dragging right must raise the monitor: {after_press} -> {dragged}"
        );
        let expected_gain = 10f32.powf(dragged / 20.0);
        assert!(
            (harness.state().test_audio_output_volume_linear() - expected_gain).abs() < 1.0e-3,
            "the dragged level has to reach the engine, not just the readout"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Space is the transport key. egui turns it into a click on whatever
    /// focused widget senses clicks, so a focused volume fader used to read it
    /// as a click and write the monitor level from wherever the mouse was
    /// resting -- on top of starting playback.
    #[test]
    fn space_moves_the_transport_and_not_a_focused_volume_fader() {
        let dir = make_temp_dir("volume_space");
        let wav = dir.join("volume_space.wav");
        write_fixture_wav(&wav, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);

        focus_volume_fader_at(&mut harness, -12.0);

        let playing_before = harness.state().test_audio_is_playing();
        harness.key_press(Key::Space);
        harness.run_steps(3);

        assert_eq!(
            harness.state().test_volume_db(),
            -12.0,
            "Space must not reach the volume fader, focused or not"
        );
        assert_ne!(
            harness.state().test_audio_is_playing(),
            playing_before,
            "Space still belongs to the transport while the fader has focus"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The editor reads its seek arrows from `key_down`, a held-key set the
    /// fader's `consume_key` never touches, so a focused fader has to be asked
    /// about rather than merely out-consumed.
    #[test]
    fn a_focused_volume_fader_keeps_the_arrows_from_the_editor() {
        let dir = make_temp_dir("volume_arrows_editor");
        let wav = dir.join("volume_arrows_editor.wav");
        write_fixture_wav(&wav, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&wav));
        wait_for_editor_ready(&mut harness);

        focus_volume_fader_at(&mut harness, -12.0);
        let playhead_before = harness.state().test_playhead_display_now();

        harness.key_press(Key::ArrowRight);
        harness.run_steps(3);

        assert_eq!(
            harness.state().test_volume_db(),
            -11.0,
            "Right should step the focused fader by 1 dB"
        );
        assert_eq!(
            harness.state().test_playhead_display_now(),
            playhead_before,
            "the editor must not seek on an arrow the volume fader owns"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The list's arrows adjust gain, and it counts them against the raw event
    /// log as a fallback for presses another widget consumed -- which is
    /// exactly what the fader does with them.
    #[test]
    fn a_focused_volume_fader_keeps_the_arrows_from_the_list() {
        let dir = make_temp_dir("volume_arrows_list");
        let wav = dir.join("volume_arrows_list.wav");
        write_fixture_wav(&wav, 48_000, 0.5);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);

        focus_volume_fader_at(&mut harness, -12.0);
        let gain_before = harness.state().test_pending_gain_db(&wav);

        harness.key_press(Key::ArrowRight);
        harness.run_steps(3);

        assert_eq!(
            harness.state().test_volume_db(),
            -11.0,
            "Right should step the focused fader by 1 dB"
        );
        assert_eq!(
            harness.state().test_pending_gain_db(&wav),
            gain_before,
            "the row's gain must not move on an arrow the volume fader owns"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
