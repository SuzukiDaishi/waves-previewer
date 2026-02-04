#[cfg(feature = "kittest")]
mod kittest_suite {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use egui::{Key, Modifiers};
    use egui_kittest::{Harness, kittest::Queryable};
    use neowaves::{StartupConfig, WavesPreviewer};
    use neowaves::kittest::{harness_default, harness_with_startup};
    use walkdir::WalkDir;

    const DEFAULT_WAV_DIR: &str =
        "C:\\Users\\zukky\\Desktop\\TTS_Train_Pipeline\\voice_pipeline\\synth_out_raw\\wavs";

    fn wav_dir() -> PathBuf {
        let from_env = std::env::var("WAVES_PREVIEWER_TEST_WAV_DIR").ok();
        let dir = from_env.unwrap_or_else(|| DEFAULT_WAV_DIR.to_string());
        let path = PathBuf::from(dir);
        assert!(
            path.is_dir(),
            "test wav dir not found: {}",
            path.display()
        );
        path
    }

    fn harness_with_wavs(open_first: bool) -> Harness<'static, WavesPreviewer> {
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(wav_dir());
        cfg.open_first = open_first;
        harness_with_startup(cfg)
    }

    fn harness_empty() -> Harness<'static, WavesPreviewer> {
        harness_default()
    }

    fn sample_wav_files(count: usize) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in WalkDir::new(wav_dir()).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if entry.file_type().is_file() {
                let path = entry.path();
                let is_wav = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("wav"))
                    .unwrap_or(false);
                if is_wav {
                    out.push(path.to_path_buf());
                    if out.len() >= count {
                        break;
                    }
                }
            }
        }
        assert!(out.len() >= count, "not enough wavs");
        out
    }

    fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let done = {
                let state = harness.state();
                !state.scan_in_progress && !state.files.is_empty()
            };
            if done {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("scan timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_tab(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().active_tab.is_some() {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("tab open timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_tab_ready(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if let Some(idx) = harness.state().active_tab {
                if let Some(tab) = harness.state().tabs.get(idx) {
                    if !tab.loading && tab.samples_len > 0 {
                        break;
                    }
                }
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("tab decode timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_editor_apply(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_editor_apply_active() {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!("editor apply timeout");
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn ensure_editor_ready(harness: &mut Harness<'static, WavesPreviewer>) {
        if harness.state().active_tab.is_none() {
            assert!(harness.state_mut().test_open_first_tab());
            wait_for_tab(harness);
        }
        wait_for_tab_ready(harness);
    }

    fn path_for_row(state: &WavesPreviewer, row: usize) -> PathBuf {
        let id = state.files[row];
        let idx = *state.item_index.get(&id).expect("missing item id");
        state.items[idx].path.clone()
    }

    fn select_first_row(harness: &mut Harness<'static, WavesPreviewer>) -> PathBuf {
        let path = {
            let state = harness.state();
            path_for_row(state, 0)
        };
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        harness.get_by_label(label).click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_playing_path(), Some(&path));
        path
    }

    fn open_first_tab(harness: &mut Harness<'static, WavesPreviewer>) -> PathBuf {
        let path = select_first_row(harness);
        harness.key_press(Key::Enter);
        wait_for_tab(harness);
        path
    }

    fn top_menu_button<'a>(
        harness: &'a Harness<'static, WavesPreviewer>,
        label: &'a str,
    ) -> egui_kittest::Node<'a> {
        let nodes: Vec<_> = harness.query_all_by_label(label).collect();
        let node = nodes
            .into_iter()
            .min_by(|a, b| {
                a.rect()
                    .min
                    .y
                    .partial_cmp(&b.rect().min.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|| panic!("Top menu button '{label}' not found"));
        node
    }

    #[test]
    fn load_folder_shows_files() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        assert!(!harness.state().files.is_empty());
        assert!(harness.state().root.is_some());
    }

    #[test]
    fn top_menu_smoke() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        top_menu_button(&harness, "File");
        top_menu_button(&harness, "Export");
        top_menu_button(&harness, "Tools");
        top_menu_button(&harness, "List");
    }

    #[test]
    fn select_row_and_play_pause() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        let before = harness
            .state()
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        harness.key_press(Key::Space);
        harness.run_steps(2);
        let after = harness
            .state()
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_ne!(before, after);
    }

    #[test]
    fn enter_opens_editor_tab() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        harness.key_press(Key::Enter);
        wait_for_tab(&mut harness);
        assert!(harness.state().active_tab.is_some());
    }

    #[test]
    fn loop_toggle_in_editor() {
        let mut harness = harness_with_wavs(true);
        wait_for_scan(&mut harness);
        wait_for_tab(&mut harness);
        let before = format!("{:?}", harness.state().tabs[0].loop_mode);
        harness.key_press(Key::L);
        harness.run_steps(2);
        let after = format!("{:?}", harness.state().tabs[0].loop_mode);
        assert_ne!(before, after);
    }

    #[test]
    fn mode_buttons_switch() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.get_by_label("Pitch").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_mode_name(), "PitchShift");
        harness.get_by_label("Stretch").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_mode_name(), "TimeStretch");
        harness.get_by_label("Speed").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_mode_name(), "Speed");
    }

    #[test]
    fn open_first_auto_opens_tab() {
        let mut harness = harness_with_wavs(true);
        wait_for_scan(&mut harness);
        wait_for_tab(&mut harness);
        assert!(harness.state().active_tab.is_some());
    }

    #[test]
    fn search_filters_and_clears() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let initial_len = harness.state().files.len();
        let first_name = path_for_row(harness.state(), 0)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let query: String = first_name.chars().take(4).collect();
        harness
            .state_mut()
            .test_set_search_query(&query);
        harness.run_steps(2);
        let filtered_len = harness.state().files.len();
        assert!(filtered_len <= initial_len);
        if !harness.state().files.is_empty() {
            let name = path_for_row(harness.state(), 0)
                .to_string_lossy()
                .to_lowercase();
            assert!(name.contains(&query.to_lowercase()));
        }
        harness.state_mut().test_set_search_query("");
        harness.run_steps(2);
        assert_eq!(harness.state().files.len(), initial_len);
    }

    #[test]
    fn sort_header_cycles() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_cycle_sort_file();
        assert_eq!(harness.state().test_sort_key_name(), "File");
        assert_eq!(harness.state().test_sort_dir_name(), "Asc");
        harness.state_mut().test_cycle_sort_file();
        assert_eq!(harness.state().test_sort_dir_name(), "Desc");
        harness.state_mut().test_cycle_sort_file();
        assert_eq!(harness.state().test_sort_dir_name(), "None");
    }

    #[test]
    fn shift_arrow_extends_selection() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        let mut mods = Modifiers::default();
        mods.shift = true;
        harness.key_press_modifiers(mods, Key::ArrowDown);
        harness.run_steps(2);
        assert!(harness.state().selected_multi.len() >= 2);
    }

    #[test]
    fn loop_markers_set_by_keys() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.state().audio.seek_to_sample(1000);
        harness.key_press(Key::K);
        harness.run_steps(1);
        harness.state().audio.seek_to_sample(2000);
        harness.key_press(Key::P);
        harness.run_steps(2);
        let region = harness.state().tabs[0].loop_region;
        assert!(matches!(region, Some((s, e)) if e > s));
    }

    #[test]
    fn zero_cross_snap_toggles() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        open_first_tab(&mut harness);
        let before = harness.state().tabs[0].snap_zero_cross;
        harness.key_press(Key::S);
        harness.run_steps(2);
        let after = harness.state().tabs[0].snap_zero_cross;
        assert_ne!(before, after);
    }

    #[test]
    fn view_mode_buttons_switch() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        open_first_tab(&mut harness);
        harness.get_by_label("Spec").click();
        harness.run_steps(2);
        assert_eq!(format!("{:?}", harness.state().tabs[0].view_mode), "Spectrogram");
        harness.get_by_label("Mel").click();
        harness.run_steps(2);
        assert_eq!(format!("{:?}", harness.state().tabs[0].view_mode), "Mel");
        harness.get_by_label("Wave").click();
        harness.run_steps(2);
        assert_eq!(format!("{:?}", harness.state().tabs[0].view_mode), "Waveform");
    }

    #[test]
    fn loop_edit_buttons_set_region() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.state().audio.seek_to_sample(1000);
        harness.get_by_label("Set Start").click();
        harness.run_steps(2);
        harness.state().audio.seek_to_sample(2000);
        harness.get_by_label("Set End").click();
        harness.run_steps(2);
        let region = harness.state().tabs[0].loop_region;
        assert!(matches!(region, Some((s, e)) if e > s));
    }

    #[test]
    fn clear_gains_from_menu() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        harness.key_press(Key::ArrowRight);
        harness.run_steps(2);
        assert!(harness.state().test_pending_gain_count() > 0);
        harness.get_by_label("Export").click();
        harness.run_steps(1);
        harness.get_by_label("Clear All Gains").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_pending_gain_count(), 0);
    }

    #[test]
    fn add_paths_avoids_duplicates() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let before = harness.state().items.len();
        let path = harness.state().items[0].path.clone();
        let added = harness.state_mut().test_add_paths(&[path]);
        harness.run_steps(2);
        assert_eq!(added, 0);
        assert_eq!(harness.state().items.len(), before);
    }

    #[test]
    fn replace_with_files_clears_root() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let files = harness
            .state()
            .items
            .iter()
            .take(2)
            .map(|item| item.path.clone())
            .collect::<Vec<_>>();
        harness.state_mut().test_replace_with_files(&files);
        harness.run_steps(2);
        assert!(harness.state().root.is_none());
        assert_eq!(harness.state().items.len(), files.len());
    }

    #[test]
    fn gain_adjust_with_arrows() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let path = select_first_row(&mut harness);
        harness.key_press(Key::ArrowRight);
        harness.run_steps(2);
        assert!(harness.state().test_has_pending_gain(&path));
    }

    #[test]
    fn export_settings_opens() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.get_by_label("Tools").click();
        harness.run_steps(1);
        harness.get_by_label("Settings...").click();
        harness.run_steps(2);
        assert!(harness.state().test_show_export_settings());
    }

    #[test]
    fn ctrl_a_selects_all_rows() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let mut mods = Modifiers::default();
        mods.ctrl = true;
        harness.key_press_modifiers(mods, Key::A);
        harness.run_steps(2);
        let state = harness.state();
        assert_eq!(state.selected_multi.len(), state.files.len());
    }

    #[test]
    fn list_shortcut_p_toggles_auto_play() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let before = harness.state().test_auto_play_list_nav();
        harness.key_press(Key::P);
        harness.run_steps(2);
        let after = harness.state().test_auto_play_list_nav();
        assert_ne!(before, after);
    }

    #[test]
    fn list_shortcut_a_d_adjust_volume() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let base = harness.state().test_volume_db();
        harness.key_press(Key::A);
        harness.run_steps(1);
        let down = harness.state().test_volume_db();
        assert!(down < base);
        harness.key_press(Key::D);
        harness.run_steps(1);
        let up = harness.state().test_volume_db();
        assert!(up > down);
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_navigation_timing_metrics() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        let steps = 120usize;
        let start = Instant::now();
        for _ in 0..steps {
            harness.key_press(Key::ArrowDown);
            harness.run_steps(1);
        }
        let elapsed = start.elapsed();
        let per_ms = elapsed.as_secs_f64() * 1000.0 / steps as f64;
        eprintln!(
            "list_navigation_timing_metrics: steps={} total_ms={:.2} per_step_ms={:.2}",
            steps,
            elapsed.as_secs_f64() * 1000.0,
            per_ms
        );
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_select_and_load_call_timing_metrics() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let rows = harness.state().files.len();
        let steps = 120usize.min(rows.saturating_sub(1));
        let start = Instant::now();
        for i in 0..steps {
            let row = (i + 1).min(rows.saturating_sub(1));
            assert!(harness.state_mut().test_select_and_load_row(row));
        }
        let elapsed = start.elapsed();
        let per_ms = elapsed.as_secs_f64() * 1000.0 / steps.max(1) as f64;
        eprintln!(
            "list_select_and_load_call_timing_metrics: steps={} total_ms={:.2} per_call_ms={:.2}",
            steps,
            elapsed.as_secs_f64() * 1000.0,
            per_ms
        );
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_idle_frame_timing_metrics() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let steps = 120usize;
        let start = Instant::now();
        for _ in 0..steps {
            harness.run_steps(1);
        }
        let elapsed = start.elapsed();
        let per_ms = elapsed.as_secs_f64() * 1000.0 / steps as f64;
        eprintln!(
            "list_idle_frame_timing_metrics: steps={} total_ms={:.2} per_frame_ms={:.2}",
            steps,
            elapsed.as_secs_f64() * 1000.0,
            per_ms
        );
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_sync_decode_timing_reference() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let rows = harness.state().files.len();
        let steps = 32usize.min(rows.saturating_sub(1));
        let start = Instant::now();
        for i in 0..steps {
            let row = (i + 1).min(rows.saturating_sub(1));
            assert!(harness.state_mut().test_select_and_load_row(row));
            let _ = harness.state_mut().test_force_load_selected_list_preview_for_play();
        }
        let elapsed = start.elapsed();
        let per_ms = elapsed.as_secs_f64() * 1000.0 / steps.max(1) as f64;
        eprintln!(
            "list_sync_decode_timing_reference: steps={} total_ms={:.2} per_call_ms={:.2}",
            steps,
            elapsed.as_secs_f64() * 1000.0,
            per_ms
        );
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_autoplay_ready_timing_metrics() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);
        select_first_row(&mut harness);
        harness.run_steps(2);

        let rows = harness.state().files.len();
        let steps = 48usize.min(rows.saturating_sub(1));
        if steps == 0 {
            eprintln!("list_autoplay_ready_timing_metrics: skipped (not enough rows)");
            return;
        }

        let mut lat_ms: Vec<f64> = Vec::new();
        let mut timeouts = 0usize;
        for _ in 0..steps {
            harness.key_press(Key::ArrowDown);
            let start = Instant::now();
            let mut ready = false;
            for _ in 0..120 {
                harness.run_steps(1);
                let state = harness.state();
                let selected = state.test_selected_path().cloned();
                let playing = state.test_playing_path().cloned();
                if selected.is_some()
                    && selected == playing
                    && state.test_audio_is_playing()
                    && state.test_audio_has_samples()
                {
                    ready = true;
                    break;
                }
            }
            if ready {
                lat_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            } else {
                timeouts = timeouts.saturating_add(1);
            }
        }

        lat_ms.sort_by(|a, b| a.total_cmp(b));
        let avg = if lat_ms.is_empty() {
            0.0
        } else {
            lat_ms.iter().sum::<f64>() / lat_ms.len() as f64
        };
        let p95 = if lat_ms.is_empty() {
            0.0
        } else {
            lat_ms[((lat_ms.len() - 1) * 95) / 100]
        };
        let max = lat_ms.last().copied().unwrap_or(0.0);
        eprintln!(
            "list_autoplay_ready_timing_metrics: steps={} measured={} timeouts={} avg_ms={:.2} p95_ms={:.2} max_ms={:.2}",
            steps,
            lat_ms.len(),
            timeouts,
            avg,
            p95,
            max
        );
    }

    #[test]
    fn arrow_down_moves_selection() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        let before = harness.state().selected;
        harness.key_press(Key::ArrowDown);
        harness.run_steps(2);
        let after = harness.state().selected;
        assert_ne!(before, after);
    }

    #[test]
    fn choose_folder_dialog_uses_queue() {
        let mut harness = harness_empty();
        let dir = wav_dir();
        harness.state_mut().test_queue_folder_dialog(Some(dir.clone()));
        top_menu_button(&harness, "File").click();
        harness.run_steps(1);
        harness.get_by_label("Folder...").click();
        wait_for_scan(&mut harness);
        assert_eq!(harness.state().root.as_ref(), Some(&dir));
        assert!(!harness.state().files.is_empty());
    }

    #[test]
    fn choose_files_dialog_uses_queue() {
        let mut harness = harness_empty();
        let files = sample_wav_files(2);
        harness
            .state_mut()
            .test_queue_files_dialog(Some(files.clone()));
        top_menu_button(&harness, "File").click();
        harness.run_steps(1);
        harness.get_by_label("Files...").click();
        harness.run_steps(2);
        assert!(harness.state().root.is_none());
        assert_eq!(harness.state().items.len(), files.len());
    }

    #[test]
    fn drag_drop_folder_adds_files() {
        let mut harness = harness_empty();
        let dir = wav_dir();
        let added = harness.state_mut().test_simulate_drop_paths(&[dir]);
        harness.run_steps(2);
        assert!(added > 0);
        assert_eq!(harness.state().items.len(), added);
        assert!(harness.state().root.is_none());
    }

    #[test]
    fn editor_trim_reduces_length() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let before = harness.state().test_tab_samples_len();
        assert!(harness.state_mut().test_apply_trim_frac(0.1, 0.9));
        harness.run_steps(2);
        let after = harness.state().test_tab_samples_len();
        assert!(after < before);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_fade_in_out_marks_dirty() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_apply_fade_in(0.0, 0.2, neowaves::FadeShape::SCurve));
        assert!(harness
            .state_mut()
            .test_apply_fade_out(0.8, 1.0, neowaves::FadeShape::SCurve));
        harness.run_steps(2);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_gain_and_normalize() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_gain(0.2, 0.6, -6.0));
        assert!(harness
            .state_mut()
            .test_apply_normalize(0.0, 1.0, -3.0));
        harness.run_steps(2);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_reverse_marks_dirty() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_reverse(0.1, 0.4));
        harness.run_steps(2);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_markers_add_and_clear() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_add_marker_frac(0.2));
        assert!(harness.state_mut().test_add_marker_frac(0.8));
        assert!(harness.state().test_marker_count() >= 2);
        assert!(harness.state_mut().test_clear_markers());
        assert_eq!(harness.state().test_marker_count(), 0);
    }

    #[test]
    fn editor_loop_region_and_mode() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_loop_region_frac(0.2, 0.6));
        assert!(harness
            .state_mut()
            .test_set_loop_xfade_ms(40.0, neowaves::LoopXfadeShape::EqualPower));
        assert!(harness
            .state_mut()
            .test_set_loop_mode(neowaves::LoopMode::Marker));
        harness.run_steps(2);
        let region = harness.state().test_loop_region();
        assert!(matches!(region, Some((s, e)) if e > s));
    }

    #[test]
    fn editor_pitch_shift_apply() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_pitch_shift(4.0));
        wait_for_editor_apply(&mut harness);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_time_stretch_apply() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_time_stretch(1.2));
        wait_for_editor_apply(&mut harness);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_view_mode_and_overlay_toggle() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        assert!(harness.state_mut().test_set_waveform_overlay(false));
        harness.run_steps(1);
        assert_eq!(
            format!("{:?}", harness.state().tabs[harness.state().active_tab.unwrap()].view_mode),
            "Spectrogram"
        );
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Mel));
        assert!(harness.state_mut().test_set_waveform_overlay(true));
        harness.run_steps(1);
        assert_eq!(
            format!("{:?}", harness.state().tabs[harness.state().active_tab.unwrap()].view_mode),
            "Mel"
        );
    }
}
