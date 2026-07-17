#[cfg(feature = "kittest")]
mod p6_metering {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_with_startup;
    use neowaves::{StartupConfig, WavesPreviewer};

    fn make_temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("neowaves_p6_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn tone(sr: u32, freq: f32, secs: f32) -> Vec<Vec<f32>> {
        vec![(0..(sr as f32 * secs) as usize)
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
    fn play_selected_together_mixes_into_one_preview_buffer() {
        let sr = 48_000u32;
        let dir = make_temp_dir("mixplay");
        // Different lengths: the mix must span the longest input.
        for (i, secs) in [0.25f32, 0.5, 0.1].iter().enumerate() {
            neowaves::wave::export_channels_audio(
                &tone(sr, 300.0 + 100.0 * i as f32, *secs),
                sr,
                &dir.join(format!("m{i}.wav")),
            )
            .expect("export fixture");
        }
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 3);

        harness.state_mut().test_list_select_all();
        assert!(harness.state_mut().test_start_mix_audition());
        wait_until(&mut harness, "mix ready", |h| {
            !h.state().test_mix_audition_pending()
        });
        let (channels, len) = harness
            .state()
            .test_audio_buffer_shape()
            .expect("mix buffer adopted");
        assert!(channels >= 1);
        let out_sr = harness.state().audio.shared.out_sample_rate.max(1) as f32;
        let expected = (out_sr * 0.5) as usize;
        assert!(
            (len as i64 - expected as i64).unsigned_abs() as usize <= expected / 50 + 64,
            "mix length should span the longest input: len={len} expected~{expected}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn play_selected_together_needs_multi_selection() {
        let sr = 48_000u32;
        let dir = make_temp_dir("mixone");
        neowaves::wave::export_channels_audio(&tone(sr, 300.0, 0.2), sr, &dir.join("a.wav"))
            .expect("export fixture");
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        harness.state_mut().test_list_select_all();
        assert!(
            !harness.state_mut().test_start_mix_audition(),
            "single selection must refuse to start"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
