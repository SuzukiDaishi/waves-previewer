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

    #[test]
    fn spectral_heal_rebuilds_dropout_and_undoes() {
        let sr = 48_000u32;
        let dir = make_temp_dir("heal");
        let src = dir.join("tone.wav");
        // Tone with a 50 ms hole punched in the middle.
        let mut chans = synth_sine(sr, 2.0, 1_000.0);
        let len = chans[0].len();
        let hole = len / 2..len / 2 + (sr as f32 * 0.05) as usize;
        for v in &mut chans[0][hole.clone()] {
            *v = 0.0;
        }
        neowaves::wave::export_channels_audio(&chans, sr, &src).expect("export source wav");
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);

        let before = tab_samples(&harness);
        let hole_rms = rms(&before[0][hole.clone()]);
        assert!(hole_rms < 1e-3, "fixture hole not silent: {hole_rms}");

        // Select just around the hole and heal it.
        let sel_s = (hole.start - 256) as f32 / len as f32;
        let sel_e = (hole.end + 256) as f32 / len as f32;
        assert!(harness.state_mut().test_set_selection_frac(sel_s, sel_e));
        assert!(harness.state_mut().test_spectral_heal_apply());
        wait_for_apply_done(&mut harness);

        let after = tab_samples(&harness);
        assert_eq!(after[0].len(), len, "heal must not change length");
        let healed_rms = rms(&after[0][hole.clone()]);
        assert!(
            healed_rms > 0.2,
            "hole not rebuilt from context: rms {healed_rms}"
        );
        // Audio far outside the selection is untouched.
        let far = 4_000..8_000;
        assert_eq!(before[0][far.clone()], after[0][far.clone()]);
        let tab_idx = harness.state().active_tab.unwrap();
        assert!(harness.state().tabs[tab_idx].dirty);

        // Undo restores the damaged original bit-exactly.
        assert!(harness.state_mut().test_editor_undo());
        harness.run_steps(2);
        assert_eq!(before, tab_samples(&harness));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn declick_scan_and_apply_repairs_clicks() {
        let sr = 48_000u32;
        let dir = make_temp_dir("declick");
        let src = dir.join("clicky.wav");
        let mut chans = synth_sine(sr, 1.0, 440.0);
        let clicks = [10_000usize, 20_000, 30_000];
        for &pos in &clicks {
            chans[0][pos] = 1.0;
        }
        neowaves::wave::export_channels_audio(&chans, sr, &src).expect("export source wav");
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);

        let before = tab_samples(&harness);
        // Scan finds the injected clicks and stores marker spans.
        let found = harness.state_mut().test_declick_scan();
        assert!(found >= clicks.len(), "scan found only {found} clicks");
        let tab_idx = harness.state().active_tab.unwrap();
        {
            let scan = harness.state().tabs[tab_idx]
                .declick_scan
                .as_ref()
                .expect("scan stored");
            for &pos in &clicks {
                assert!(
                    scan.spans.iter().any(|&(s, e)| pos >= s && pos < e),
                    "click at {pos} missing from scan spans"
                );
            }
        }

        // Apply repairs them through the async pipeline (undoable).
        assert!(harness.state_mut().test_declick_apply());
        wait_for_apply_done(&mut harness);
        let after = tab_samples(&harness);
        for &pos in &clicks {
            assert!(
                after[0][pos].abs() < 0.6,
                "click at {pos} not repaired: {}",
                after[0][pos]
            );
        }
        // Apply invalidates the scan markers.
        let tab_idx = harness.state().active_tab.unwrap();
        assert!(harness.state().tabs[tab_idx].declick_scan.is_none());

        assert!(harness.state_mut().test_editor_undo());
        harness.run_steps(2);
        assert_eq!(before, tab_samples(&harness));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn denoise_learn_and_apply_reduces_noise_floor() {
        let sr = 48_000u32;
        let dir = make_temp_dir("denoise");
        let src = dir.join("noisy.wav");
        // First half: noise only. Second half: noise + tone.
        let len = (sr * 2) as usize;
        let noise_amp = 10f32.powf(-30.0 / 20.0);
        let mut state = 99u64;
        let mut ch: Vec<f32> = (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (((state >> 33) as f32 / (u32::MAX >> 1) as f32) * 2.0 - 1.0) * noise_amp
            })
            .collect();
        for (i, v) in ch.iter_mut().enumerate().skip(len / 2) {
            *v += (i as f32 / sr as f32 * 1_000.0 * std::f32::consts::TAU).sin() * 0.25;
        }
        neowaves::wave::export_channels_audio(&[ch], sr, &src).expect("export source wav");
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);

        let before = tab_samples(&harness);
        // Learn from the noise-only first half.
        assert!(harness.state_mut().test_set_selection_frac(0.02, 0.48));
        assert!(harness.state_mut().test_denoise_learn(), "learn failed");
        // Clear the selection so Apply covers the whole file.
        assert!(harness.state_mut().test_set_selection_frac(0.0, 0.0) || true);
        let tab_idx = harness.state().active_tab.unwrap();
        harness.state_mut().tabs[tab_idx].selection = None;

        assert!(harness.state_mut().test_denoise_apply(), "apply not queued");
        wait_for_apply_done(&mut harness);
        let after = tab_samples(&harness);
        let probe = 10_000..len / 2 - 10_000;
        let drop_db = 20.0
            * (rms(&after[0][probe.clone()]) / rms(&before[0][probe.clone()]).max(1e-12)).log10();
        assert!(drop_db < -8.0, "noise floor only dropped {drop_db} dB");
        // Undo restores the noisy original.
        assert!(harness.state_mut().test_editor_undo());
        harness.run_steps(2);
        assert_eq!(before, tab_samples(&harness));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
