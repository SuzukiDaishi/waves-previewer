#[cfg(feature = "kittest")]
mod p3_audition {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_with_startup;
    use neowaves::{StartupConfig, WavesPreviewer};

    fn make_temp_dir() -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("neowaves_p3_audition_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn synth(sr: u32, freq: f32) -> Vec<Vec<f32>> {
        let frames = (sr / 4) as usize; // 250 ms
        vec![(0..frames)
            .map(|i| (i as f32 / sr as f32 * freq * std::f32::consts::TAU).sin() * 0.4)
            .collect()]
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
    fn variation_audition_advances_round_robin_and_stops_on_user_stop() {
        let sr = 48_000u32;
        let dir = make_temp_dir();
        let paths: Vec<PathBuf> = (0..3)
            .map(|i| {
                let p = dir.join(format!("var_{i}.wav"));
                neowaves::wave::export_channels_audio(&synth(sr, 300.0 + 100.0 * i as f32), sr, &p)
                    .expect("export fixture");
                p
            })
            .collect();
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 3);

        assert!(harness.state_mut().test_set_list_multi_selection(&paths));
        assert!(harness.state_mut().test_start_variation_audition(false));
        assert_eq!(
            harness.state().test_variation_audition_cursor(),
            Some((0, 1)),
            "audition starts on the first selected row"
        );
        // Wait for the first item's audio to be loaded so the natural-end
        // simulation sees a real buffer.
        wait_until(&mut harness, "first item audio", |h| {
            h.state().test_audio_has_samples()
        });

        // Natural end -> next item, cycling in order.
        assert!(harness.state_mut().test_variation_simulate_natural_end());
        assert_eq!(
            harness.state().test_variation_audition_cursor(),
            Some((1, 2))
        );
        wait_until(&mut harness, "second item audio", |h| {
            h.state().test_audio_has_samples()
        });
        assert!(harness.state_mut().test_variation_simulate_natural_end());
        assert_eq!(
            harness.state().test_variation_audition_cursor(),
            Some((2, 3))
        );
        wait_until(&mut harness, "third item audio", |h| {
            h.state().test_audio_has_samples()
        });
        assert!(harness.state_mut().test_variation_simulate_natural_end());
        assert_eq!(
            harness.state().test_variation_audition_cursor(),
            Some((0, 4)),
            "round-robin wraps back to the first item"
        );

        // A stop mid-file (user stop) ends the audition.
        wait_until(&mut harness, "wrapped item audio", |h| {
            h.state().test_audio_has_samples()
        });
        assert!(harness.state_mut().test_variation_simulate_user_stop());
        assert_eq!(harness.state().test_variation_audition_cursor(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
