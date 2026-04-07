#[cfg(feature = "kittest")]
mod export_overwrite_undo {
    use std::path::{Path, PathBuf};
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

    fn wait_for_tab_ready(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let ready = harness
                .state()
                .active_tab
                .and_then(|idx| harness.state().tabs.get(idx))
                .map(|tab| tab.samples_len > 0 && !tab.loading && !tab.ch_samples.is_empty())
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

    fn newest_file_with_ext(dir: &Path, ext: &str) -> Option<PathBuf> {
        let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
        for ent in std::fs::read_dir(dir).ok()? {
            let ent = ent.ok()?;
            let path = ent.path();
            let matches = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(ext))
                .unwrap_or(false);
            if !matches {
                continue;
            }
            let modified = ent
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            match &latest {
                Some((ts, _)) if modified <= *ts => {}
                _ => latest = Some((modified, path)),
            }
        }
        latest.map(|(_, path)| path)
    }

    fn assert_loop_close(actual: Option<(usize, usize)>, expected: (usize, usize), tol: usize) {
        let actual = actual.expect("missing loop region");
        assert!(
            actual.0.abs_diff(expected.0) <= tol && actual.1.abs_diff(expected.1) <= tol,
            "expected loop {:?} within ±{}, got {:?}",
            expected,
            tol,
            actual
        );
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
        harness
            .state_mut()
            .test_set_export_save_mode_overwrite(true);
        harness.state_mut().test_set_export_backup_bak(true);
        assert!(harness
            .state_mut()
            .test_set_selected_sample_rate_override(44_100));
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

    #[test]
    fn overwrite_export_persists_current_loop_and_markers_without_apply() {
        let dir = make_temp_dir("overwrite_current_annotations");
        let src = dir.join("src.wav");
        let chans = synth_stereo(48_000, 2.0);
        neowaves::wave::export_channels_audio(&chans, 48_000, &src).expect("export src wav");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);

        assert!(harness.state_mut().test_add_marker_frac(0.25));
        assert!(harness.state_mut().test_set_loop_region_frac(0.20, 0.60));
        harness.run_steps(2);

        let expected_loop = harness
            .state()
            .test_loop_region()
            .expect("current loop should exist");
        let out_sr = harness.state().test_audio_out_sample_rate();
        let file_sr = neowaves::audio_io::read_audio_info(&src)
            .expect("probe source wav")
            .sample_rate
            .max(1);
        let expected_file_loop =
            neowaves::wave::map_loop_markers_to_file_sr(expected_loop.0, expected_loop.1, out_sr, file_sr)
                .expect("map loop markers to file sr");

        harness.state_mut().test_set_export_first_prompt(false);
        harness
            .state_mut()
            .test_set_export_save_mode_overwrite(true);
        harness.state_mut().test_set_export_backup_bak(false);
        harness.state_mut().test_trigger_save_selected();
        wait_for_export_finish(&mut harness);
        harness.run_steps(3);

        let saved_loop =
            neowaves::loop_markers::read_loop_markers(&src).expect("saved loop markers");
        assert_eq!(
            saved_loop,
            (
                expected_file_loop.0 as u64,
                expected_file_loop.1 as u64
            )
        );
        let saved_markers =
            neowaves::markers::read_markers(&src, file_sr, file_sr).expect("read saved markers");
        assert_eq!(saved_markers.len(), 1);
        assert_eq!(saved_markers[0].label, "M01");

        assert!(harness.state_mut().test_close_active_tab());
        harness.run_steps(2);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);
        assert_loop_close(harness.state().test_loop_region(), expected_loop, 1);
        assert_eq!(harness.state().test_marker_count(), 1);

        drop(harness);

        let mut restarted = harness_with_folder(dir.clone());
        wait_for_scan(&mut restarted);
        assert!(restarted.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut restarted);
        assert_loop_close(restarted.state().test_loop_region(), expected_loop, 1);
        assert_eq!(restarted.state().test_marker_count(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_file_export_writes_current_annotations_but_keeps_source_dirty() {
        let dir = make_temp_dir("new_file_current_annotations");
        let src = dir.join("src.wav");
        let export_dir = dir.join("exports");
        std::fs::create_dir_all(&export_dir).expect("create export dir");
        let chans = synth_stereo(48_000, 2.0);
        neowaves::wave::export_channels_audio(&chans, 48_000, &src).expect("export src wav");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);

        assert!(harness.state_mut().test_add_marker_frac(0.33));
        assert!(harness.state_mut().test_set_loop_region_frac(0.15, 0.45));
        harness.run_steps(2);
        let expected_loop = harness
            .state()
            .test_loop_region()
            .expect("current loop should exist");

        harness.state_mut().test_set_export_first_prompt(false);
        harness
            .state_mut()
            .test_set_export_save_mode_overwrite(false);
        harness.state_mut().test_set_export_conflict("rename");
        harness
            .state_mut()
            .test_set_export_dest_folder(Some(&export_dir));
        harness
            .state_mut()
            .test_set_export_name_template("{name}_saved");
        harness.state_mut().test_trigger_save_selected();
        wait_for_export_finish(&mut harness);
        harness.run_steps(3);

        let out = newest_file_with_ext(&export_dir, "wav").expect("missing exported wav");
        assert_eq!(neowaves::loop_markers::read_loop_markers(&src), None);
        let exported_loop =
            neowaves::loop_markers::read_loop_markers(&out).expect("exported loop markers");
        assert!(exported_loop.1 > exported_loop.0);
        let exported_markers =
            neowaves::markers::read_markers(&out, 48_000, 48_000).expect("read exported markers");
        assert_eq!(exported_markers.len(), 1);
        assert_eq!(exported_markers[0].label, "M01");

        assert!(harness.state().test_loop_marker_dirty());
        assert!(harness.state().test_marker_dirty());
        assert_loop_close(harness.state().test_loop_region(), expected_loop, 1);

        assert!(harness.state_mut().test_close_active_tab());
        harness.run_steps(2);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);
        assert_loop_close(harness.state().test_loop_region(), expected_loop, 1);
        assert_eq!(harness.state().test_marker_count(), 1);
        assert!(harness.state().test_loop_marker_dirty());
        assert!(harness.state().test_marker_dirty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
