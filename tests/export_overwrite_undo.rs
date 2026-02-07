#[cfg(feature = "kittest")]
mod export_overwrite_undo {
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
            "neowaves_export_overwrite_undo_{tag}_{}_{}_{}",
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
            left.push((t * 220.0 * std::f32::consts::TAU).sin() * 0.25);
            right.push((t * 330.0 * std::f32::consts::TAU).sin() * 0.25);
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
            let has_files = !harness.state().files.is_empty();
            if has_files {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!("scan timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_export_finish(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_export_in_progress() {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                panic!("export timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn overwrite_export_can_be_undone_from_backup() {
        let dir = make_temp_dir("overwrite_undo");
        let src = dir.join("src.wav");
        let chans = synth_stereo(48_000, 1.5);
        neowaves::wave::export_channels_audio(&chans, 48_000, &src).expect("export src wav");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);

        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.state_mut().test_set_export_first_prompt(false);
        harness.state_mut().test_set_export_save_mode_overwrite(true);
        harness.state_mut().test_set_export_backup_bak(true);
        assert!(harness.state_mut().test_set_selected_sample_rate_override(44_100));
        harness.state_mut().test_trigger_save_selected();
        wait_for_export_finish(&mut harness);
        harness.run_steps(3);

        let info_after = neowaves::audio_io::read_audio_info(&src).expect("probe overwritten wav");
        assert_eq!(info_after.sample_rate, 44_100);

        assert!(harness.state_mut().test_undo_last_overwrite_export());
        harness.run_steps(3);
        let info_undo = neowaves::audio_io::read_audio_info(&src).expect("probe restored wav");
        assert_eq!(info_undo.sample_rate, 48_000);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
