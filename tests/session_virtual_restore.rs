#[cfg(feature = "kittest")]
mod session_virtual_restore {
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
            "neowaves_session_restore_{tag}_{}_{}_{}",
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
            let done = {
                let state = harness.state();
                !state.scan_in_progress && !state.files.is_empty()
            };
            if done {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("scan timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_tab(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if let Some(idx) = harness.state().active_tab {
                if harness
                    .state()
                    .tabs
                    .get(idx)
                    .map(|t| t.samples_len > 0)
                    .unwrap_or(false)
                {
                    break;
                }
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("tab timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn session_roundtrip_restores_virtual_and_overrides() {
        let dir = make_temp_dir("roundtrip");
        let wav_path = dir.join("source.wav");
        let chans = synth_stereo(48_000, 2.5);
        neowaves::wave::export_channels_audio(&chans, 48_000, &wav_path)
            .expect("export source fixture wav");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);

        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab(&mut harness);
        assert!(harness.state_mut().test_add_trim_virtual_frac(0.15, 0.55));
        harness.run_steps(3);

        assert!(harness.state_mut().test_select_and_load_row(0));
        assert!(harness
            .state_mut()
            .test_set_selected_sample_rate_override(44_100));
        harness.state_mut().test_set_external_show_unmatched(true);
        let export_dir = dir.join("exports");
        std::fs::create_dir_all(&export_dir).expect("create export dir");
        harness
            .state_mut()
            .test_set_export_save_mode_overwrite(true);
        harness.state_mut().test_set_export_conflict("skip");
        harness.state_mut().test_set_export_backup_bak(false);
        harness
            .state_mut()
            .test_set_export_name_template("{name}_session_restore");
        harness
            .state_mut()
            .test_set_export_dest_folder(Some(&export_dir));

        let session_path = dir.join("roundtrip.nwsess");
        assert!(harness.state_mut().test_save_session_to(&session_path));

        // Mutate runtime state and ensure load restores it.
        harness.state_mut().test_set_external_show_unmatched(false);
        assert!(harness
            .state_mut()
            .test_set_selected_sample_rate_override(0));
        harness
            .state_mut()
            .test_set_export_save_mode_overwrite(false);
        harness.state_mut().test_set_export_conflict("rename");
        harness.state_mut().test_set_export_backup_bak(true);
        harness
            .state_mut()
            .test_set_export_name_template("{name}_mutated");
        harness.state_mut().test_set_export_dest_folder(None);
        harness.run_steps(2);

        assert!(harness.state_mut().test_open_session_from(&session_path));
        harness.run_steps(3);

        assert!(harness.state().test_virtual_item_count() >= 1);
        assert!(harness.state().test_sample_rate_override_count() >= 1);
        assert!(harness.state().test_external_show_unmatched());
        assert_eq!(harness.state().test_export_save_mode_name(), "Overwrite");
        assert_eq!(harness.state().test_export_conflict_name(), "Skip");
        assert!(!harness.state().test_export_backup_bak());
        assert_eq!(
            harness.state().test_export_name_template(),
            "{name}_session_restore"
        );
        assert_eq!(
            harness
                .state()
                .test_export_dest_folder()
                .map(|p| p.as_path()),
            Some(export_dir.as_path())
        );
        assert_eq!(
            harness.state().test_selected_path().map(|p| p.as_path()),
            Some(wav_path.as_path())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
