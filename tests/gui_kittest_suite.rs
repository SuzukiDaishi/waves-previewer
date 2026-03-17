#[cfg(feature = "kittest")]
mod kittest_suite {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui::{Key, Modifiers, MouseWheelUnit};
    use egui_kittest::{kittest::Queryable, Harness};
    use neowaves::app::ToolKind;
    use neowaves::kittest::{harness_default, harness_with_startup};
    use neowaves::{StartupConfig, WavesPreviewer};
    use walkdir::WalkDir;

    const DEFAULT_WAV_DIR: &str = "test_samples";
    const SCAN_TIMEOUT: Duration = Duration::from_secs(30);
    const TAB_READY_TIMEOUT: Duration = Duration::from_secs(30);

    fn source_wav_dir() -> PathBuf {
        let from_env = std::env::var("WAVES_PREVIEWER_TEST_WAV_DIR").ok();
        let path = from_env
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_WAV_DIR));
        assert!(path.is_dir(), "test wav dir not found: {}", path.display());
        path
    }

    fn wav_dir() -> PathBuf {
        static FIXTURE_DIR: OnceLock<PathBuf> = OnceLock::new();
        FIXTURE_DIR
            .get_or_init(|| {
                let src = source_wav_dir();
                let dst = make_temp_dir("kittest_media");
                for entry in WalkDir::new(&src).follow_links(false) {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => continue,
                    };
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let Ok(rel) = entry.path().strip_prefix(&src) else {
                        continue;
                    };
                    let out = dst.join(rel);
                    if let Some(parent) = out.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::copy(entry.path(), out);
                }
                maybe_generate_extra_formats(&dst);
                dst
            })
            .clone()
    }

    fn has_file_ext(dir: &Path, ext: &str) -> bool {
        for entry in WalkDir::new(dir).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let matches = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(ext))
                .unwrap_or(false);
            if matches {
                return true;
            }
        }
        false
    }

    fn first_wav_file(dir: &Path) -> Option<PathBuf> {
        for entry in WalkDir::new(dir).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let is_wav = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("wav"))
                .unwrap_or(false);
            if is_wav {
                return Some(entry.into_path());
            }
        }
        None
    }

    fn try_ffmpeg_convert(src: &Path, dst: &Path) -> bool {
        Command::new("ffmpeg")
            .arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(src)
            .arg(dst)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn try_sox_convert(src: &Path, dst: &Path) -> bool {
        Command::new("sox")
            .arg(src)
            .arg(dst)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn try_internal_convert(src: &Path, dst: &Path) -> bool {
        match neowaves::audio_io::decode_audio_multi(src) {
            Ok((chans, sr)) => neowaves::wave::export_channels_audio(&chans, sr, dst).is_ok(),
            Err(_) => false,
        }
    }

    fn maybe_generate_extra_formats(dir: &Path) {
        let Some(seed) = first_wav_file(dir) else {
            return;
        };
        for ext in ["mp3", "m4a", "ogg"] {
            if has_file_ext(dir, ext) {
                continue;
            }
            let out = dir.join(format!("generated_fixture.{ext}"));
            let ok = try_ffmpeg_convert(&seed, &out)
                || ((ext == "mp3" || ext == "ogg") && try_sox_convert(&seed, &out))
                || try_internal_convert(&seed, &out);
            if !ok {
                eprintln!(
                    "warning: could not generate {} fixture from {}",
                    ext,
                    seed.display()
                );
            }
        }
    }

    fn harness_with_wavs(open_first: bool) -> Harness<'static, WavesPreviewer> {
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(wav_dir());
        cfg.open_first = open_first;
        harness_with_startup(cfg)
    }

    fn make_temp_dir(tag: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "neowaves_kittest_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn synth_stereo(sr: u32, secs: f32) -> Vec<Vec<f32>> {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for i in 0..frames {
            let t = (i as f32) / (sr as f32);
            left.push((t * 220.0 * std::f32::consts::TAU).sin() * 0.30);
            right.push((t * 440.0 * std::f32::consts::TAU).sin() * 0.25);
        }
        vec![left, right]
    }

    fn build_format_fixtures(dir: &Path, secs: f32) -> Vec<PathBuf> {
        let sr = 44_100;
        let chans = synth_stereo(sr, secs);
        let mut out = Vec::new();
        for ext in ["wav", "mp3", "m4a", "ogg"] {
            let path = dir.join(format!("fixture_{ext}.{ext}"));
            neowaves::wave::export_channels_audio(&chans, sr, &path)
                .unwrap_or_else(|e| panic!("export {ext} failed: {e}"));
            out.push(path);
        }
        out
    }

    fn harness_with_folder(dir: PathBuf) -> Harness<'static, WavesPreviewer> {
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        cfg.open_first = false;
        harness_with_startup(cfg)
    }

    fn harness_with_editor_fixture() -> Harness<'static, WavesPreviewer> {
        let dir = make_temp_dir("editor_fixture");
        let sr = 48_000;
        let chans = synth_stereo(sr, 3.0);
        let path = dir.join("editor_fixture.wav");
        neowaves::wave::export_channels_audio(&chans, sr, &path)
            .unwrap_or_else(|e| panic!("export editor fixture failed: {e}"));
        harness_with_folder(dir)
    }

    fn audio_buffer_len(state: &WavesPreviewer) -> usize {
        state
            .audio
            .shared
            .samples
            .load()
            .as_ref()
            .map(|b| b.len())
            .unwrap_or(0)
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
            let (done, has_files) = {
                let state = harness.state();
                (!state.scan_in_progress, !state.files.is_empty())
            };
            // Most UI tests only need the list to become usable.
            if (done && has_files) || (has_files && start.elapsed() > Duration::from_secs(5)) {
                break;
            }
            if start.elapsed() > SCAN_TIMEOUT {
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
                    if tab.samples_len > 0
                        && (!tab.loading || harness.state().test_audio_has_samples())
                    {
                        break;
                    }
                }
            }
            if start.elapsed() > TAB_READY_TIMEOUT {
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
        let label = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
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

    fn editor_canvas_hover_pos(harness: &Harness<'static, WavesPreviewer>) -> egui::Pos2 {
        let inspector_rect = harness.get_by_label("Inspector").rect();
        egui::pos2(
            (inspector_rect.left() - 220.0).max(40.0),
            inspector_rect.center().y,
        )
    }

    fn editor_zoom_in_once(harness: &mut Harness<'static, WavesPreviewer>) {
        let hover_pos = editor_canvas_hover_pos(harness);
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: Modifiers::COMMAND,
            },
            Modifiers::COMMAND,
        );
        harness.run_steps(3);
    }

    fn editor_shift_pan_once(harness: &mut Harness<'static, WavesPreviewer>) {
        let hover_pos = editor_canvas_hover_pos(harness);
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.run_steps(3);
    }

    fn editor_wave_width(harness: &Harness<'static, WavesPreviewer>) -> f32 {
        let inspector_rect = harness.get_by_label("Inspector").rect();
        (inspector_rect.left() - 40.0).max(64.0)
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
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        top_menu_button(&harness, "File");
        top_menu_button(&harness, "Export");
        top_menu_button(&harness, "Tools");
        top_menu_button(&harness, "List");
    }

    #[test]
    fn inspector_panel_visible_when_editor_open() {
        let mut harness = harness_with_wavs(true);
        wait_for_scan(&mut harness);
        wait_for_tab(&mut harness);
        let inspector_nodes: Vec<_> = harness.query_all_by_label("Inspector").collect();
        assert!(!inspector_nodes.is_empty(), "Inspector heading not found");
    }

    #[test]
    fn inspector_tool_combo_reachable() {
        let mut harness = harness_with_wavs(true);
        wait_for_scan(&mut harness);
        wait_for_tab(&mut harness);

        let tool_nodes: Vec<_> = harness.query_all_by_label("Tool").collect();
        assert!(!tool_nodes.is_empty(), "Inspector tool row not found");

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Reverse));
        harness.run_steps(1);
        assert_eq!(harness.state().test_active_tool(), Some(ToolKind::Reverse));
    }

    #[test]
    fn select_row_and_play_pause() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        let before = harness
            .state()
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        harness.key_press(Key::Space);
        let start = Instant::now();
        let mut ever_toggled = false;
        loop {
            harness.run_steps(1);
            let after = harness
                .state()
                .audio
                .shared
                .playing
                .load(std::sync::atomic::Ordering::Relaxed);
            if after != before {
                ever_toggled = true;
                break;
            }
            if start.elapsed() > Duration::from_secs(8) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ever_toggled);
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
        harness.state_mut().test_set_search_query(&query);
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
        let mut harness = harness_with_editor_fixture();
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
        harness.key_press(Key::R);
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
        assert_eq!(
            format!("{:?}", harness.state().tabs[0].view_mode),
            "Spectrogram"
        );
        harness.get_by_label("Mel").click();
        harness.run_steps(2);
        assert_eq!(format!("{:?}", harness.state().tabs[0].view_mode), "Mel");
        harness.get_by_label("Wave").click();
        harness.run_steps(2);
        assert_eq!(
            format!("{:?}", harness.state().tabs[0].view_mode),
            "Waveform"
        );
    }

    #[test]
    fn loop_edit_buttons_set_region() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_loop_region_frac(0.2, 0.6));
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
    fn list_playback_continuity_for_formats() {
        let dir = make_temp_dir("list_play_formats");
        let formats = build_format_fixtures(&dir, 4.0);
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(
            harness.state().files.len() >= formats.len(),
            "expected at least {} files in list",
            formats.len()
        );

        for row in 0..formats.len() {
            harness.state_mut().audio.stop();
            assert!(
                harness.state_mut().test_select_and_load_row(row),
                "failed to select row {row}"
            );
            let selected = harness
                .state()
                .test_selected_path()
                .cloned()
                .expect("selected path");
            let _ = harness
                .state_mut()
                .test_force_load_selected_list_preview_for_play();

            let mut ready = false;
            for _ in 0..200 {
                harness.run_steps(1);
                let state = harness.state();
                let selected_matches = state
                    .test_playing_path()
                    .map(|p| p == &selected)
                    .unwrap_or(false);
                if selected_matches
                    && state.test_audio_has_samples()
                    && state.test_audio_is_playing()
                {
                    ready = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                ready,
                "playback did not start in time for {}",
                selected.display()
            );

            let info = neowaves::audio_io::read_audio_info(&selected).ok();
            let sr = info.map(|i| i.sample_rate).unwrap_or(0);
            let initial_len = audio_buffer_len(harness.state());
            let mut max_len = initial_len;
            for _ in 0..160 {
                harness.run_steps(1);
                let len = audio_buffer_len(harness.state());
                if len > max_len {
                    max_len = len;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let already_long = sr > 0 && initial_len >= (sr as f32 * 3.0) as usize;
            assert!(
                max_len > initial_len || already_long,
                "list preview buffer did not grow for {} (initial={} max={} sr={})",
                selected.display(),
                initial_len,
                max_len,
                sr
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
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
            let _ = harness
                .state_mut()
                .test_force_load_selected_list_preview_for_play();
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
        harness
            .state_mut()
            .test_queue_folder_dialog(Some(dir.clone()));
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
        let mut harness = harness_with_editor_fixture();
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
        let mut harness = harness_with_editor_fixture();
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
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_gain(0.2, 0.6, -6.0));
        assert!(harness.state_mut().test_apply_normalize(0.0, 1.0, -3.0));
        harness.run_steps(2);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_reverse_marks_dirty() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_reverse(0.1, 0.4));
        harness.run_steps(2);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_markers_add_and_clear() {
        let mut harness = harness_with_editor_fixture();
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
        let mut harness = harness_with_editor_fixture();
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
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_pitch_shift(4.0));
        wait_for_editor_apply(&mut harness);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_time_stretch_apply() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_time_stretch(1.2));
        wait_for_editor_apply(&mut harness);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_view_mode_and_overlay_toggle() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        assert!(harness.state_mut().test_set_waveform_overlay(false));
        harness.run_steps(1);
        assert_eq!(
            format!(
                "{:?}",
                harness.state().tabs[harness.state().active_tab.unwrap()].view_mode
            ),
            "Spectrogram"
        );
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Mel));
        assert!(harness.state_mut().test_set_waveform_overlay(true));
        harness.run_steps(1);
        assert_eq!(
            format!(
                "{:?}",
                harness.state().tabs[harness.state().active_tab.unwrap()].view_mode
            ),
            "Mel"
        );
    }

    #[test]
    fn loop_inspector_shows_three_windows() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.state().audio.seek_to_sample(1200);
        harness.key_press(Key::K);
        harness.run_steps(1);
        harness.state().audio.seek_to_sample(7200);
        harness.key_press(Key::P);
        harness.run_steps(3);

        assert!(!harness
            .query_all_by_label("Pre-Loop window")
            .collect::<Vec<_>>()
            .is_empty());
        assert!(!harness
            .query_all_by_label("Seam preview")
            .collect::<Vec<_>>()
            .is_empty());
        assert!(!harness
            .query_all_by_label("Post-Loop window")
            .collect::<Vec<_>>()
            .is_empty());
    }

    #[test]
    fn editor_ctrl_wheel_zoom_in_changes_samples_per_px() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let spp_before = harness.state().tabs[tab_idx].samples_per_px;
        assert!(spp_before > 0.0, "samples_per_px should be initialized");

        editor_zoom_in_once(&mut harness);

        let spp_after = harness.state().tabs[tab_idx].samples_per_px;
        assert!(
            spp_after < spp_before,
            "ctrl+wheel zoom in should reduce samples_per_px: before={spp_before} after={spp_after}"
        );
    }

    #[test]
    fn editor_shift_wheel_pan_changes_view_offset() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        editor_zoom_in_once(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before = harness.state().tabs[tab_idx].view_offset;
        editor_shift_pan_once(&mut harness);

        let after = harness.state().tabs[tab_idx].view_offset;
        assert_ne!(after, before, "Shift+wheel should pan the editor view");
    }

    #[test]
    fn editor_zoom_then_pan_then_zoom_preserves_anchor_reasonably() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        editor_zoom_in_once(&mut harness);
        editor_shift_pan_once(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before_second_zoom_spp = harness.state().tabs[tab_idx].samples_per_px;
        let view_before_second_zoom = harness.state().tabs[tab_idx].view_offset as i64;
        let visible_before_second_zoom =
            (before_second_zoom_spp * editor_wave_width(&harness)).round() as i64;

        editor_zoom_in_once(&mut harness);

        let after_second_zoom = &harness.state().tabs[tab_idx];
        let delta = (after_second_zoom.view_offset as i64 - view_before_second_zoom).abs();
        assert!(
            after_second_zoom.samples_per_px < before_second_zoom_spp,
            "second zoom should still zoom in"
        );
        assert!(
            delta < visible_before_second_zoom.max(256),
            "zoom after pan should keep anchor reasonably stable: delta={delta} visible={visible_before_second_zoom}"
        );
    }

    #[test]
    fn editor_middle_drag_pan_changes_view_offset() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        editor_zoom_in_once(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before = harness.state().tabs[tab_idx].view_offset;
        let start = editor_canvas_hover_pos(&harness);
        let end = egui::pos2(start.x + 140.0, start.y);
        harness.hover_at(start);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Middle,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.event_modifiers(egui::Event::PointerMoved(end), Modifiers::NONE);
        harness.run_steps(2);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Middle,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(2);

        let after = harness.state().tabs[tab_idx].view_offset;
        assert_ne!(after, before, "Middle drag should pan the editor view");
    }

    #[test]
    fn editor_waveform_overlay_in_spec_mode_survives_zoom_and_pan() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        assert!(harness.state_mut().test_set_waveform_overlay(true));
        harness.run_steps(3);

        editor_zoom_in_once(&mut harness);
        editor_shift_pan_once(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        assert_eq!(
            harness.state().tabs[tab_idx].view_mode,
            neowaves::ViewMode::Spectrogram
        );
        assert!(harness.state().tabs[tab_idx].show_waveform_overlay);
        assert!(
            harness.state().test_active_tab_waveform_pyramid_ready(),
            "waveform cache should remain ready in spectrogram overlay mode"
        );
    }

    #[test]
    fn editor_channel_view_switch_all_custom_mixdown_keeps_waveform_visible() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        assert!(harness.state_mut().test_set_channel_view_all());
        harness.run_steps(3);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        assert!(harness.state_mut().test_set_channel_view_custom(vec![0]));
        harness.run_steps(3);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        assert!(harness.state_mut().test_set_channel_view_mixdown());
        harness.run_steps(3);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());
        assert!(
            harness.state().test_tab_samples_len() > 0,
            "waveform should remain renderable across channel view switches"
        );
    }

    #[test]
    fn editor_undo_redo_keeps_waveform_cache_renderable() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        assert!(harness.state_mut().test_apply_reverse(0.1, 0.4));
        harness.run_steps(3);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run_steps(3);
        assert!(
            harness.state().test_active_tab_waveform_pyramid_ready(),
            "undo should keep waveform cache renderable"
        );

        harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        harness.run_steps(3);
        assert!(
            harness.state().test_active_tab_waveform_pyramid_ready(),
            "redo should keep waveform cache renderable"
        );
    }

    #[test]
    fn editor_waveform_lod_counters_cover_raw_visible_and_pyramid() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(3);

        let (_, _, pyramid_before) = harness.state().test_waveform_lod_counts();
        harness.run_steps(2);
        let (_, _, pyramid_after) = harness.state().test_waveform_lod_counts();
        assert!(
            pyramid_after > pyramid_before,
            "fit-whole editor view should use pyramid LOD"
        );

        let visible_before = harness.state().test_waveform_lod_counts().1;
        for _ in 0..4 {
            editor_zoom_in_once(&mut harness);
        }
        harness.run_steps(2);
        let visible_after = harness.state().test_waveform_lod_counts().1;
        assert!(
            visible_after > visible_before,
            "mid zoom should use visible-range min/max LOD"
        );

        let raw_before = harness.state().test_waveform_lod_counts().0;
        for _ in 0..12 {
            editor_zoom_in_once(&mut harness);
        }
        harness.run_steps(2);
        let raw_after = harness.state().test_waveform_lod_counts().0;
        assert!(raw_after > raw_before, "deep zoom should use raw LOD");

        let summary = harness.state().test_debug_summary_text();
        assert!(summary.contains("waveform_render_ms:"));
        assert!(summary.contains("waveform_query_ms:"));
        assert!(summary.contains("waveform_draw_ms:"));
        assert!(summary.contains("waveform_lod_counts:"));
    }

    #[test]
    fn trim_set_add_virtual_keeps_editor_waveform_playback_source() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let source_path = harness.state().tabs[tab_idx].path.clone();
        let source_len = harness.state().tabs[tab_idx].samples_len;
        let virtual_before = harness.state().test_virtual_item_count();

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Trim));
        assert!(harness.state_mut().test_set_selection_frac(0.20, 0.60));
        harness.run_steps(2);

        harness.get_by_label("Set").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().tabs[tab_idx].preview_audio_tool,
            Some(ToolKind::Trim),
            "trim preview should be armed after Set"
        );

        harness.get_by_label("Add Trim As Virtual").click();
        harness.run_steps(3);
        assert!(
            harness.state().test_virtual_item_count() > virtual_before,
            "Add Trim As Virtual should create a new virtual item"
        );
        assert_eq!(
            harness.state().test_active_tab_path(),
            Some(source_path.clone()),
            "active editor tab should remain on source waveform"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].preview_audio_tool,
            None,
            "trim preview should be cleared after creating virtual item"
        );

        harness.key_press(Key::Space);
        harness.run_steps(3);
        assert!(
            harness.state().test_audio_is_playing(),
            "space should start playback in editor"
        );
        assert_eq!(
            audio_buffer_len(harness.state()),
            source_len,
            "editor playback should use visible source waveform after Add Trim As Virtual"
        );
        assert_eq!(
            harness.state().test_playing_path().cloned(),
            Some(source_path),
            "playing path should remain source tab path"
        );
    }

    #[test]
    fn topbar_playing_indicator_tracks_playback_state() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        harness.run_steps(2);
        assert!(
            harness
                .query_all_by_label("Playing")
                .collect::<Vec<_>>()
                .is_empty(),
            "Playing indicator should be hidden while stopped"
        );

        harness.state_mut().audio.play();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            harness.run_steps(1);
            if harness.state().test_audio_is_playing() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(harness.state().test_audio_is_playing());
        assert!(
            !harness
                .query_all_by_label("Playing")
                .collect::<Vec<_>>()
                .is_empty(),
            "Playing indicator should be visible while playing"
        );

        harness.state_mut().audio.stop();
        harness.run_steps(3);
        assert!(
            harness
                .query_all_by_label("Playing")
                .collect::<Vec<_>>()
                .is_empty(),
            "Playing indicator should hide after stop"
        );
    }

    #[test]
    fn list_context_effect_graph_open_sets_target_path() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        let path = select_first_row(&mut harness);
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .expect("file name")
            .to_string();

        harness.get_by_label(&label).click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Effect Graph ⏵").click();
        harness.run_steps(1);
        harness.get_by_label("Open").click();
        harness.run_steps(3);

        assert!(harness.state().test_effect_graph_workspace_open());
        assert_eq!(harness.state().test_effect_graph_target_path(), Some(path));
    }

    #[test]
    fn spectrogram_hop_ui_shows_derived_overlap() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_show_export_settings(true);
        harness.state_mut().test_set_spectro_hop_size(128);
        harness.run_steps(3);

        harness.get_by_label("Hop Size:");
        harness.get_by_label("Overlap: 93.8% (derived)");
    }

    #[test]
    fn settings_output_device_controls_visible() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_show_export_settings(true);
        harness.run_steps(3);

        harness.get_by_label("Audio Output:");
        harness.get_by_label("Refresh");
    }

    #[test]
    fn music_stem_preview_gain_clamps_to_plus_24_in_editor_ui() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::MusicAnalyze));
        assert!(harness
            .state_mut()
            .test_set_music_analysis_result_mock(true));
        assert!(harness
            .state_mut()
            .test_set_music_preview_gains_db(77.0, 33.0, 48.0, 60.0));
        harness.run_steps(3);

        let gains = harness
            .state()
            .test_music_preview_gains_db()
            .expect("music preview gains");
        assert!(gains.0 <= 24.0 && gains.0 >= -80.0);
        assert!(gains.1 <= 24.0 && gains.1 >= -80.0);
        assert!(gains.2 <= 24.0 && gains.2 >= -80.0);
        assert!(gains.3 <= 24.0 && gains.3 >= -80.0);
        assert!((gains.0 - 24.0).abs() < 1.0e-6);
    }

    #[test]
    fn model_download_progress_labels_show_n_over_n() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness
            .state_mut()
            .test_set_mock_transcript_model_download_progress(3, 7);
        harness
            .state_mut()
            .test_set_mock_music_model_download_progress(5, 9);
        harness.run_steps(2);

        harness.get_by_label("Downloading transcript model... 3/7");
        harness.get_by_label("Downloading music analyze model... 5/9");
        harness
            .state_mut()
            .test_clear_mock_model_download_progress();
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_saves_editor_screenshot_png() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);

        let image = harness
            .render()
            .expect("kittest render should produce an image");
        assert!(image.width() >= 640);
        assert!(image.height() >= 360);

        let dir = make_temp_dir("kittest_render_shot");
        let out = dir.join("editor_kittest_render.png");
        image
            .save(&out)
            .unwrap_or_else(|e| panic!("save kittest render png failed: {e}"));
        let size = std::fs::metadata(&out).expect("png metadata").len();
        assert!(size > 1024, "rendered png looks too small: {size} bytes");
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_zoom_ctrl_wheel_saves_before_after_screenshots() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);

        let before = harness
            .render()
            .expect("kittest render should produce pre-zoom image");
        let inspector_rect = harness.get_by_label("Inspector").rect();
        let hover_pos = egui::pos2(
            (inspector_rect.left() - 220.0).max(40.0),
            inspector_rect.center().y,
        );
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: Modifiers::COMMAND,
            },
            Modifiers::COMMAND,
        );
        harness.run_steps(3);
        let after = harness
            .render()
            .expect("kittest render should produce post-zoom image");
        assert_eq!(before.width(), after.width());
        assert_eq!(before.height(), after.height());

        let changed_pixels = before
            .pixels()
            .zip(after.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            changed_pixels > 1024,
            "zoom render difference too small: {changed_pixels} changed pixels"
        );

        let dir = make_temp_dir("kittest_zoom_ctrl_wheel");
        let before_out = dir.join("zoom_before.png");
        let after_out = dir.join("zoom_after.png");
        before
            .save(&before_out)
            .unwrap_or_else(|e| panic!("save pre-zoom png failed: {e}"));
        after
            .save(&after_out)
            .unwrap_or_else(|e| panic!("save post-zoom png failed: {e}"));
        assert!(std::fs::metadata(&before_out).is_ok());
        assert!(std::fs::metadata(&after_out).is_ok());
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_pan_changes_waveform_position_png() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        editor_zoom_in_once(&mut harness);
        harness.run_steps(2);

        let before = harness.render().expect("pre-pan render");
        editor_shift_pan_once(&mut harness);
        let after = harness.render().expect("post-pan render");

        let changed_pixels = before
            .pixels()
            .zip(after.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            changed_pixels > 1024,
            "pan diff too small: {changed_pixels}"
        );

        let dir = make_temp_dir("kittest_pan_shift_wheel");
        let before_out = dir.join("pan_before.png");
        let after_out = dir.join("pan_after.png");
        before.save(&before_out).expect("save pan before");
        after.save(&after_out).expect("save pan after");
        assert!(std::fs::metadata(&before_out).is_ok());
        assert!(std::fs::metadata(&after_out).is_ok());
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_channel_view_all_vs_mixdown_differs_png() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        let mixdown = harness.render().expect("mixdown render");
        assert!(harness.state_mut().test_set_channel_view_all());
        harness.run_steps(3);
        let all = harness.render().expect("all-channels render");

        let changed_pixels = mixdown
            .pixels()
            .zip(all.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            changed_pixels > 2048,
            "channel view render difference too small: {changed_pixels}"
        );

        let dir = make_temp_dir("kittest_channel_view_modes");
        let mixdown_out = dir.join("mixdown.png");
        let all_out = dir.join("all_channels.png");
        mixdown.save(&mixdown_out).expect("save mixdown");
        all.save(&all_out).expect("save all");
        assert!(std::fs::metadata(&mixdown_out).is_ok());
        assert!(std::fs::metadata(&all_out).is_ok());
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_waveform_overlay_spec_zoom_png() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        assert!(harness.state_mut().test_set_waveform_overlay(true));
        harness.run_steps(3);

        let before = harness.render().expect("spec overlay pre-zoom render");
        editor_zoom_in_once(&mut harness);
        editor_shift_pan_once(&mut harness);
        let after = harness.render().expect("spec overlay post-zoom render");

        let changed_pixels = before
            .pixels()
            .zip(after.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            changed_pixels > 1024,
            "spec overlay zoom/pan diff too small: {changed_pixels}"
        );

        let dir = make_temp_dir("kittest_spec_overlay_zoom");
        let before_out = dir.join("spec_overlay_before.png");
        let after_out = dir.join("spec_overlay_after.png");
        before.save(&before_out).expect("save spec overlay before");
        after.save(&after_out).expect("save spec overlay after");
        assert!(std::fs::metadata(&before_out).is_ok());
        assert!(std::fs::metadata(&after_out).is_ok());
    }
}
