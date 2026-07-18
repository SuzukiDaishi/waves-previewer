#[cfg(feature = "kittest")]
mod p7_pipeline {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_with_startup;
    use neowaves::{StartupConfig, WavesPreviewer};

    fn make_temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("neowaves_p7_{tag}_{}", std::process::id()));
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
    fn silence_columns_fill_lead_and_tail_ms() {
        let sr = 48_000u32;
        let dir = make_temp_dir("silcols");
        // 100 ms silence + 200 ms tone + 50 ms silence.
        let mut ch = vec![0.0f32; (sr / 10) as usize];
        ch.extend(
            (0..(sr / 5) as usize)
                .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5),
        );
        ch.extend(vec![0.0f32; (sr / 20) as usize]);
        let path = dir.join("padded.wav");
        neowaves::wave::export_channels_audio(&[ch].to_vec(), sr, &path).expect("export fixture");

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        harness.state_mut().test_set_silence_columns(true);
        // Rendering the visible row queues the full-decode metadata job.
        wait_until(&mut harness, "silence meta", |h| {
            h.state().test_meta_silence_ms(&path).is_some()
        });
        let (lead, tail) = harness.state().test_meta_silence_ms(&path).expect("silence");
        assert!(
            (lead - 100.0).abs() <= 15.0,
            "lead silence ~100 ms, got {lead}"
        );
        assert!((tail - 50.0).abs() <= 15.0, "tail silence ~50 ms, got {tail}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
