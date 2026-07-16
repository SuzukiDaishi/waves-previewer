#[cfg(feature = "kittest")]
mod batch_qa_loudnorm {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
            "neowaves_batch_qa_loudnorm_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn tone(sr: u32, secs: f32, amp: f32) -> Vec<f32> {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        (0..frames)
            .map(|i| ((i as f32 / sr as f32) * 440.0 * std::f32::consts::TAU).sin() * amp)
            .collect()
    }

    fn harness_with_folder(dir: PathBuf) -> Harness<'static, WavesPreviewer> {
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        cfg.open_first = false;
        harness_with_startup(cfg)
    }

    fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>, count: usize) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().files.len() >= count {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!("scan timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn run_loudnorm(
        harness: &mut Harness<'static, WavesPreviewer>,
        paths: Vec<PathBuf>,
        target: f32,
    ) {
        harness
            .state_mut()
            .test_begin_batch_loudnorm(paths, target);
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_batch_loudnorm_active() {
                break;
            }
            if start.elapsed() > Duration::from_secs(60) {
                panic!("loudnorm batch timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn file_hashes(paths: &[PathBuf]) -> Vec<u64> {
        use std::hash::{Hash, Hasher};
        paths
            .iter()
            .map(|p| {
                let bytes = std::fs::read(p).expect("read file");
                let mut h = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut h);
                h.finish()
            })
            .collect()
    }

    #[test]
    fn loudnorm_sets_pending_gains_without_writing_files() {
        let dir = make_temp_dir("run");
        let sr = 48_000u32;
        let names = ["loud.wav", "mid.wav", "quiet.wav"];
        for (name, amp) in names.iter().zip([0.5f32, 0.1, 0.02]) {
            neowaves::wave::export_channels_audio(&[tone(sr, 0.5, amp)], sr, &dir.join(name))
                .expect("export");
        }
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness, 3);
        let paths = harness.state().test_visible_list_paths();
        let hashes_before = file_hashes(&paths);

        run_loudnorm(&mut harness, paths.clone(), -14.0);

        // Non-destructive: file bytes untouched.
        assert_eq!(file_hashes(&paths), hashes_before, "no audio writes");

        // Every file received a pending gain; quieter files get more gain.
        let gain = |name: &str| {
            let p = paths.iter().find(|p| p.ends_with(name)).expect("path");
            harness.state().test_pending_gain_db(p)
        };
        let (g_loud, g_mid, g_quiet) = (gain("loud.wav"), gain("mid.wav"), gain("quiet.wav"));
        assert!(g_loud.abs() > 0.05, "loud gain {g_loud}");
        assert!(
            g_quiet > g_mid && g_mid > g_loud,
            "gain ordering: quiet {g_quiet} > mid {g_mid} > loud {g_loud}"
        );
        let toasts = harness.state().test_toast_messages();
        assert!(
            toasts.iter().any(|m| m.contains("3 gains set")),
            "completion toast: {toasts:?}"
        );

        // One list undo restores all pending gains.
        assert!(harness.state_mut().test_list_undo_once());
        harness.run_steps(2);
        for p in &paths {
            assert_eq!(
                harness.state().test_pending_gain_db(p),
                0.0,
                "undo restores {p:?}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loudnorm_second_run_is_idempotent() {
        let dir = make_temp_dir("idem");
        let sr = 48_000u32;
        for (name, amp) in ["a.wav", "b.wav", "c.wav"].iter().zip([0.5f32, 0.1, 0.02]) {
            neowaves::wave::export_channels_audio(&[tone(sr, 0.5, amp)], sr, &dir.join(name))
                .expect("export");
        }
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness, 3);
        let paths = harness.state().test_visible_list_paths();

        run_loudnorm(&mut harness, paths.clone(), -14.0);
        run_loudnorm(&mut harness, paths.clone(), -14.0);

        let toasts = harness.state().test_toast_messages();
        assert!(
            toasts.iter().any(|m| m.contains("3 already on target")),
            "second run should skip everything: {toasts:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
