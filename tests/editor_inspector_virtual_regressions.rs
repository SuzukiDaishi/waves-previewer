#[cfg(feature = "kittest")]
mod editor_inspector_virtual_regressions {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui_kittest::Harness;
    use neowaves::app::ToolKind;
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
            "neowaves_editor_inspector_reg_{tag}_{}_{}_{}",
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

    fn write_wav(path: &Path, sr: u32, secs: f32) {
        let chans = synth_stereo(sr, secs);
        neowaves::wave::export_channels_audio(&chans, sr, path).expect("export wav fixture");
    }

    fn harness_with_folder(dir: PathBuf) -> Harness<'static, WavesPreviewer> {
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        cfg.open_first = false;
        harness_with_startup(cfg)
    }

    fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().scan_in_progress && !harness.state().files.is_empty() {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("scan timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_tab_ready(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let ready = harness
                .state()
                .active_tab
                .and_then(|idx| harness.state().tabs.get(idx))
                .map(|tab| tab.samples_len > 0 && !tab.loading)
                .unwrap_or(false);
            if ready && harness.state().test_audio_buffer_len() > 0 {
                break;
            }
            if start.elapsed() > Duration::from_secs(15) {
                panic!("tab ready timeout");
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
            if start.elapsed() > Duration::from_secs(15) {
                panic!("editor apply timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_active_playing(harness: &mut Harness<'static, WavesPreviewer>, path: &Path) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let active_ok = harness.state().test_active_tab_path().as_deref() == Some(path);
            let playing_ok = harness.state().test_playing_path().map(|p| p.as_path()) == Some(path);
            if active_ok && playing_ok && harness.state().test_audio_buffer_len() > 0 {
                break;
            }
            if start.elapsed() > Duration::from_secs(12) {
                panic!(
                    "active/playing path timeout: expected={}, active={:?}, playing={:?}",
                    path.display(),
                    harness.state().test_active_tab_path(),
                    harness.state().test_playing_path()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn select_and_open_tab(harness: &mut Harness<'static, WavesPreviewer>, path: &Path) {
        assert!(harness.state_mut().test_select_path(path));
        harness.run_steps(1);
        assert!(harness.state_mut().test_open_tab_for_path(path));
    }

    #[test]
    fn virtual_trim_then_tab_switch_keeps_play_target() {
        let dir = make_temp_dir("virtual_switch");
        let source = dir.join("source.wav");
        write_wav(&source, 48_000, 3.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        select_and_open_tab(&mut harness, &source);
        wait_for_tab_ready(&mut harness);
        wait_for_active_playing(&mut harness, &source);
        let source_len_before = harness.state().test_tab_samples_len();
        assert!(source_len_before > 0);

        assert!(harness.state_mut().test_add_trim_virtual_frac(0.20, 0.60));
        harness.run_steps(3);
        harness.state_mut().test_switch_to_list();
        harness.run_steps(2);
        let virtual_path = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("virtual path selected");

        select_and_open_tab(&mut harness, &virtual_path);
        wait_for_tab_ready(&mut harness);
        wait_for_active_playing(&mut harness, &virtual_path);
        let virtual_len = harness.state().test_tab_samples_len();
        assert!(virtual_len > 0 && virtual_len < source_len_before);

        select_and_open_tab(&mut harness, &source);
        wait_for_tab_ready(&mut harness);
        wait_for_active_playing(&mut harness, &source);
        let source_len_after = harness.state().test_tab_samples_len();
        assert_eq!(source_len_after, source_len_before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn virtual_chain_create_from_virtual_keeps_source_immutable() {
        let dir = make_temp_dir("virtual_chain");
        let source = dir.join("source.wav");
        write_wav(&source, 48_000, 4.0);
        let source_size_before = std::fs::metadata(&source).expect("source metadata").len();

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&source));
        wait_for_tab_ready(&mut harness);
        let source_len = harness.state().test_tab_samples_len();
        let virtual_before = harness.state().test_virtual_item_count();

        assert!(harness.state_mut().test_add_trim_virtual_frac(0.05, 0.85));
        harness.run_steps(2);
        harness.state_mut().test_switch_to_list();
        harness.run_steps(2);
        let virtual_1 = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("virtual_1 path");

        assert!(harness.state_mut().test_open_tab_for_path(&virtual_1));
        wait_for_tab_ready(&mut harness);
        let virtual_1_len = harness.state().test_tab_samples_len();

        assert!(harness.state_mut().test_add_trim_virtual_frac(0.25, 0.75));
        harness.run_steps(2);
        harness.state_mut().test_switch_to_list();
        harness.run_steps(2);
        let virtual_2 = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("virtual_2 path");

        assert!(harness.state_mut().test_open_tab_for_path(&virtual_2));
        wait_for_tab_ready(&mut harness);
        let virtual_2_len = harness.state().test_tab_samples_len();

        assert!(harness.state().test_virtual_item_count() >= virtual_before + 2);
        assert!(virtual_1_len < source_len);
        assert!(virtual_2_len < virtual_1_len);
        assert_eq!(
            std::fs::metadata(&source)
                .expect("source metadata after chain")
                .len(),
            source_size_before
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn virtual_open_close_reopen_uses_cached_edit_state() {
        let dir = make_temp_dir("virtual_reopen");
        let source = dir.join("source.wav");
        write_wav(&source, 48_000, 3.5);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&source));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_add_trim_virtual_frac(0.20, 0.80));
        harness.run_steps(2);
        harness.state_mut().test_switch_to_list();
        harness.run_steps(2);
        let virtual_path = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("virtual path");

        assert!(harness.state_mut().test_open_tab_for_path(&virtual_path));
        wait_for_tab_ready(&mut harness);
        let len_before_close = harness.state().test_tab_samples_len();
        assert!(harness.state_mut().test_apply_reverse(0.10, 0.90));
        wait_for_editor_apply(&mut harness);
        harness.run_steps(2);
        assert!(harness.state().test_tab_dirty());

        assert!(harness.state_mut().test_close_active_tab());
        harness.run_steps(2);
        assert!(harness.state_mut().test_open_tab_for_path(&virtual_path));
        wait_for_tab_ready(&mut harness);

        assert!(harness.state().test_tab_dirty());
        assert_eq!(harness.state().test_tab_samples_len(), len_before_close);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inspector_tool_state_is_tab_local() {
        let dir = make_temp_dir("tool_local");
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_wav(&a, 48_000, 1.8);
        write_wav(&b, 48_000, 1.2);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);

        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Reverse));

        assert!(harness.state_mut().test_open_tab_for_path(&b));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Trim));

        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        assert_eq!(harness.state().test_active_tool(), Some(ToolKind::Reverse));

        assert!(harness.state_mut().test_open_tab_for_path(&b));
        wait_for_tab_ready(&mut harness);
        assert_eq!(harness.state().test_active_tool(), Some(ToolKind::Trim));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inspector_apply_path_sets_dirty_and_audio_updates() {
        let dir = make_temp_dir("inspector_apply");
        let source = dir.join("source.wav");
        write_wav(&source, 48_000, 5.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&source));
        wait_for_tab_ready(&mut harness);

        let tab_len_before = harness.state().test_tab_samples_len();
        let audio_len_before = harness.state().test_audio_buffer_len();
        assert!(tab_len_before > 0);
        assert!(audio_len_before > 0);

        assert!(harness.state_mut().test_apply_trim_frac(0.10, 0.70));
        wait_for_editor_apply(&mut harness);
        harness.run_steps(2);

        let tab_len_after = harness.state().test_tab_samples_len();
        let audio_len_after = harness.state().test_audio_buffer_len();
        assert!(harness.state().test_tab_dirty());
        assert!(tab_len_after < tab_len_before);
        assert!(audio_len_after < audio_len_before);
        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(source.as_path())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
