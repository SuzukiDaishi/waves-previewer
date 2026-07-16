#[cfg(feature = "kittest")]
mod p3_spectral_tools {
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
            "neowaves_p3_spectral_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn synth_sine(sr: u32, secs: f32, freq: f32) -> Vec<Vec<f32>> {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        let ch: Vec<f32> = (0..frames)
            .map(|i| (i as f32 / sr as f32 * freq * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        vec![ch]
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

    fn wait_for_apply_done(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_editor_apply_busy() {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                panic!("editor apply timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn tab_samples(harness: &Harness<'static, WavesPreviewer>) -> Vec<Vec<f32>> {
        let idx = harness.state().active_tab.expect("active tab");
        harness.state().tabs[idx].ch_samples.clone()
    }

    fn rms(sig: &[f32]) -> f32 {
        if sig.is_empty() {
            return 0.0;
        }
        (sig.iter().map(|v| v * v).sum::<f32>() / sig.len() as f32).sqrt()
    }

    #[test]
    fn spectral_brush_apply_attenuates_and_undoes() {
        let sr = 48_000u32;
        let dir = make_temp_dir("brush");
        let src = dir.join("tone.wav");
        neowaves::wave::export_channels_audio(&synth_sine(sr, 2.0, 1_000.0), sr, &src)
            .expect("export source wav");
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);

        let before = tab_samples(&harness);
        let len = before[0].len();
        let center = len / 2;
        // Probe well inside the default 60 ms sigma (3 sigma = 180 ms).
        let probe = center - 1_200..center + 1_200;
        let rms_before = rms(&before[0][probe.clone()]);
        assert!(rms_before > 0.2, "fixture tone missing: rms {rms_before}");

        // Paint a hard cut right on the tone and apply it.
        assert!(harness
            .state_mut()
            .test_spectral_brush_stamp(0.5, 1_000.0, 40.0));
        assert!(harness.state_mut().test_spectral_brush_apply());
        wait_for_apply_done(&mut harness);

        let after = tab_samples(&harness);
        assert_eq!(after[0].len(), len, "brush must not change length");
        let rms_after = rms(&after[0][probe.clone()]);
        let drop_db = 20.0 * (rms_after / rms_before).log10();
        assert!(
            drop_db < -15.0,
            "stamp center not attenuated: {drop_db} dB ({rms_before} -> {rms_after})"
        );
        // Far from the stamp (well outside 3 sigma + FFT margins) the tone
        // is untouched.
        let far = 4_000..8_000;
        assert_eq!(before[0][far.clone()], after[0][far.clone()]);
        // Stamps are consumed by Apply.
        let tab_idx = harness.state().active_tab.unwrap();
        assert!(harness.state().tabs[tab_idx].spectral_brush_stamps.is_empty());
        assert!(harness.state().tabs[tab_idx].dirty);

        // Undo restores the original buffer bit-exactly.
        assert!(harness.state_mut().test_editor_undo());
        harness.run_steps(2);
        assert_eq!(before, tab_samples(&harness));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
