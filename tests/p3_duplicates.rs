#[cfg(feature = "kittest")]
mod p3_duplicates {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_with_startup;
    use neowaves::{StartupConfig, WavesPreviewer};

    fn make_temp_dir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("neowaves_p3_duplicates_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn content(sr: u32, seed: u64, secs: f32) -> Vec<Vec<f32>> {
        let len = (sr as f32 * secs) as usize;
        let mut state = seed.max(1);
        let ch: Vec<f32> = (0..len)
            .map(|i| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let noise = (((state >> 33) as f32 / (u32::MAX >> 1) as f32) * 2.0 - 1.0) * 0.05;
                let t = i as f32 / sr as f32;
                let f = 300.0 + 40.0 * (seed % 7) as f32 + 900.0 * t;
                (t * f * std::f32::consts::TAU).sin() * 0.4 + noise
            })
            .collect();
        vec![ch]
    }

    fn wait_until(
        harness: &mut Harness<'static, WavesPreviewer>,
        what: &str,
        mut done: impl FnMut(&Harness<'static, WavesPreviewer>) -> bool,
    ) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if done(harness) {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                panic!("timeout waiting for {what}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn duplicate_scan_groups_copies_and_gain_variants() {
        let sr = 48_000u32;
        let dir = make_temp_dir();
        let base = content(sr, 5, 1.0);
        // original + exact copy + gain variant + unrelated file
        neowaves::wave::export_channels_audio(&base, sr, &dir.join("a_orig.wav")).unwrap();
        neowaves::wave::export_channels_audio(&base, sr, &dir.join("b_copy.wav")).unwrap();
        let quieter: Vec<Vec<f32>> =
            vec![base[0].iter().map(|v| v * 0.6).collect::<Vec<f32>>()];
        neowaves::wave::export_channels_audio(&quieter, sr, &dir.join("c_quiet.wav")).unwrap();
        neowaves::wave::export_channels_audio(&content(sr, 11, 1.0), sr, &dir.join("d_other.wav"))
            .unwrap();

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 4);

        assert!(harness.state_mut().test_start_duplicate_scan());
        wait_until(&mut harness, "duplicate scan", |h| {
            !h.state().test_duplicate_scan_active()
        });

        let groups = harness.state().test_duplicate_groups();
        assert_eq!(groups.len(), 1, "one merged group expected: {groups:?}");
        let (exact, paths) = &groups[0];
        let names: Vec<String> = paths
            .iter()
            .filter_map(|p| p.file_name().and_then(|s| s.to_str()).map(String::from))
            .collect();
        assert!(names.contains(&"a_orig.wav".to_string()), "{names:?}");
        assert!(names.contains(&"b_copy.wav".to_string()), "{names:?}");
        assert!(names.contains(&"c_quiet.wav".to_string()), "{names:?}");
        assert!(
            !names.contains(&"d_other.wav".to_string()),
            "unrelated file joined the group: {names:?}"
        );
        // The gain variant makes the merged group non-exact.
        assert!(!exact);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
