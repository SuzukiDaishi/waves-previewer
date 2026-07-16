#[cfg(feature = "kittest")]
mod p2_editor_ops {
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
            "neowaves_p2_editor_ops_{tag}_{}_{}_{}",
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

    fn open_editor_tab(
        tag: &str,
        channels: &[Vec<f32>],
    ) -> (Harness<'static, WavesPreviewer>, PathBuf) {
        let dir = make_temp_dir(tag);
        let src = dir.join("source.wav");
        neowaves::wave::export_channels_audio(channels, 48_000, &src).expect("export source wav");
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);
        (harness, dir)
    }

    fn tab_samples(harness: &Harness<'static, WavesPreviewer>) -> Vec<Vec<f32>> {
        let idx = harness.state().active_tab.expect("active tab");
        harness.state().tabs[idx].ch_samples.clone()
    }

    #[test]
    fn invert_polarity_negates_range_and_undoes() {
        let (mut harness, dir) = open_editor_tab("invert", &synth_stereo(48_000, 0.5));
        let before = tab_samples(&harness);

        // Whole-file invert: every sample exactly negated.
        assert!(harness
            .state_mut()
            .test_apply_invert_polarity_frac(0.0, 1.0));
        harness.run_steps(2);
        let after = tab_samples(&harness);
        assert_eq!(before.len(), after.len());
        for (b_ch, a_ch) in before.iter().zip(after.iter()) {
            assert_eq!(b_ch.len(), a_ch.len());
            for (b, a) in b_ch.iter().zip(a_ch.iter()) {
                assert_eq!(*a, -*b, "sample must be exactly negated");
            }
        }
        let tab_idx = harness.state().active_tab.unwrap();
        assert!(harness.state().tabs[tab_idx].dirty);

        // Undo restores the original samples bit-exactly.
        assert!(harness.state_mut().test_editor_undo());
        harness.run_steps(2);
        let restored = tab_samples(&harness);
        assert_eq!(before, restored);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dc_offset_removal_zeroes_mean_and_undoes() {
        // Sine + constant bias per channel.
        let mut chans = synth_stereo(48_000, 0.5);
        for v in chans[0].iter_mut() {
            *v += 0.15;
        }
        for v in chans[1].iter_mut() {
            *v -= 0.08;
        }
        let (mut harness, dir) = open_editor_tab("dc", &chans);
        let before = tab_samples(&harness);

        assert!(harness.state_mut().test_apply_remove_dc_frac(0.0, 1.0));
        harness.run_steps(2);
        let after = tab_samples(&harness);
        for ch in after.iter() {
            let mean: f64 = ch.iter().map(|&v| f64::from(v)).sum::<f64>() / ch.len() as f64;
            assert!(
                mean.abs() < 1.0e-4,
                "mean after DC removal should be ~0, got {mean}"
            );
        }
        // The AC content is preserved: after + mean == before.
        let mean0: f64 =
            before[0].iter().map(|&v| f64::from(v)).sum::<f64>() / before[0].len() as f64;
        let k = before[0].len() / 3;
        assert!((f64::from(after[0][k]) + mean0 - f64::from(before[0][k])).abs() < 1.0e-4);

        assert!(harness.state_mut().test_editor_undo());
        harness.run_steps(2);
        assert_eq!(before, tab_samples(&harness));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invert_polarity_partial_range_leaves_rest_untouched() {
        let (mut harness, dir) = open_editor_tab("invert_part", &synth_stereo(48_000, 0.5));
        let before = tab_samples(&harness);
        let len = before[0].len();
        let (s, e) = (len / 4, len / 2);

        assert!(harness
            .state_mut()
            .test_apply_invert_polarity_frac(0.25, 0.5));
        harness.run_steps(2);
        let after = tab_samples(&harness);
        for ch in 0..before.len() {
            for i in 0..len {
                if i >= s && i < e {
                    assert_eq!(after[ch][i], -before[ch][i]);
                } else {
                    assert_eq!(after[ch][i], before[ch][i]);
                }
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
