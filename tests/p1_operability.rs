#[cfg(feature = "kittest")]
mod p1_operability {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui::Key;
    use egui_kittest::Harness;
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
            "neowaves_p1_operability_{tag}_{}_{}_{}",
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
            if !harness.state().files.is_empty() {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
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
                .map(|tab| {
                    tab.samples_len > 0
                        && (!tab.loading || harness.state().test_audio_has_samples())
                })
                .unwrap_or(false);
            if ready {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!("tab ready timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn open_editor_tab(tag: &str) -> (Harness<'static, WavesPreviewer>, PathBuf) {
        let dir = make_temp_dir(tag);
        let src = dir.join("source.wav");
        neowaves::wave::export_channels_audio(&synth_stereo(48_000, 2.0), 48_000, &src)
            .expect("export source wav");
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);
        (harness, dir)
    }

    #[test]
    fn shortcuts_window_lists_all_contexts() {
        use egui_kittest::kittest::Queryable;
        let mut harness = harness_with_startup(StartupConfig::default());
        harness.run_steps(1);

        harness.state_mut().test_set_shortcuts_window_open(true);
        harness.run_steps(2);

        assert!(harness.query_by_label("Keyboard Shortcuts").is_some());
        // One representative row per context group.
        assert!(harness.query_by_label("Focus the search box").is_some());
        assert!(harness
            .query_by_label("Toggle auto-play on navigation")
            .is_some());
        assert!(harness
            .query_by_label("Set loop start at the playhead")
            .is_some());
    }

    #[test]
    fn destructive_keys_show_undo_toast() {
        let (mut harness, dir) = open_editor_tab("ct_toast");

        assert!(harness.state_mut().test_set_selection_frac(0.4, 0.6));
        harness.run_steps(1);
        harness.key_press(Key::T);
        harness.run_steps(2);
        let toasts = harness.state().test_toast_messages();
        assert!(
            toasts
                .iter()
                .any(|m| m.contains("Trimmed to selection (Ctrl+Z to undo)")),
            "expected trim toast, got {toasts:?}"
        );

        assert!(harness.state_mut().test_set_selection_frac(0.1, 0.2));
        harness.run_steps(1);
        harness.key_press(Key::C);
        harness.run_steps(2);
        let toasts = harness.state().test_toast_messages();
        assert!(
            toasts
                .iter()
                .any(|m| m.contains("Deleted selection (Ctrl+Z to undo)")),
            "expected delete toast, got {toasts:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
