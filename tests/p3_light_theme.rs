#[cfg(feature = "kittest")]
mod p3_light_theme {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_with_startup;
    use neowaves::{StartupConfig, WavesPreviewer};

    fn make_temp_dir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("neowaves_p3_light_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
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
            if start.elapsed() > Duration::from_secs(20) {
                panic!("timeout waiting for {what}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn light_theme_renders_list_and_editor_without_panic() {
        let sr = 48_000u32;
        let dir = make_temp_dir();
        let tone: Vec<f32> = (0..sr as usize / 2)
            .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.4)
            .collect();
        neowaves::wave::export_channels_audio(&[tone], sr, &dir.join("tone.wav")).unwrap();

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| !h.state().files.is_empty());

        // Flip to Light and render the list for several frames.
        harness.state_mut().test_set_theme_light(true);
        harness.run_steps(5);
        assert!(harness.state().test_theme_is_light());

        // Open the editor and keep rendering in Light.
        assert!(harness.state_mut().test_open_first_tab());
        wait_until(&mut harness, "tab ready", |h| {
            h.state()
                .active_tab
                .and_then(|i| h.state().tabs.get(i))
                .map(|t| t.samples_len > 0)
                .unwrap_or(false)
        });
        harness.run_steps(5);

        // And back to Dark.
        harness.state_mut().test_set_theme_light(false);
        harness.run_steps(3);
        assert!(!harness.state().test_theme_is_light());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
