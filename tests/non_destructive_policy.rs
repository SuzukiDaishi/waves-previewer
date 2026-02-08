#[cfg(feature = "kittest")]
mod non_destructive_policy {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_with_startup;
    use neowaves::{StartupConfig, WavesPreviewer};

    #[derive(Clone, Debug)]
    struct FileSnapshot {
        len: u64,
        modified: Option<SystemTime>,
    }

    fn snapshot(path: &Path) -> FileSnapshot {
        let meta = std::fs::metadata(path).expect("metadata");
        let modified = meta.modified().ok();
        FileSnapshot {
            len: meta.len(),
            modified,
        }
    }

    fn assert_unchanged(path: &Path, before: &FileSnapshot) {
        let after = snapshot(path);
        assert_eq!(
            after.len,
            before.len,
            "file size changed: {}",
            path.display()
        );
        if let (Some(b), Some(a)) = (before.modified, after.modified) {
            assert_eq!(a, b, "file modified timestamp changed: {}", path.display());
        }
    }

    fn make_temp_dir(tag: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "neowaves_non_destructive_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
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
            if !harness.state().scan_in_progress && !harness.state().files.is_empty() {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
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
                .map(|t| t.samples_len > 0 && !t.loading)
                .unwrap_or(false);
            if ready {
                break;
            }
            if start.elapsed() > Duration::from_secs(15) {
                panic!("tab ready timeout");
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
            if start.elapsed() > Duration::from_secs(20) {
                panic!("export timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn sr_and_bits_convert_are_non_destructive_until_export() {
        let dir = make_temp_dir("sr_bits");
        let path = dir.join("fixture.wav");
        let chans = synth_stereo(48_000, 2.0);
        neowaves::wave::export_channels_audio_with_depth(
            &chans,
            48_000,
            &path,
            Some(neowaves::wave::WavBitDepth::Pcm24),
        )
        .expect("write fixture");
        let before = snapshot(&path);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);

        assert!(harness
            .state_mut()
            .test_apply_selected_resample_override(44_100));
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_selected_sample_rate_override(),
            Some(44_100)
        );
        assert_eq!(
            std::fs::read_dir(&dir).expect("read dir").count(),
            1,
            "non-export operation must not create files"
        );
        assert_unchanged(&path, &before);

        assert!(harness
            .state_mut()
            .test_convert_bits_selected_to(neowaves::wave::WavBitDepth::Pcm16));
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_selected_bit_depth_override(),
            Some(neowaves::wave::WavBitDepth::Pcm16)
        );
        assert_eq!(
            std::fs::read_dir(&dir).expect("read dir").count(),
            1,
            "non-export operation must not create files"
        );
        assert_unchanged(&path, &before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_trim_as_virtual_keeps_source_file_unchanged() {
        let dir = make_temp_dir("trim_virtual");
        let path = dir.join("fixture.wav");
        let chans = synth_stereo(48_000, 3.0);
        neowaves::wave::export_channels_audio(&chans, 48_000, &path).expect("write fixture");
        let before = snapshot(&path);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);

        let virtual_before = harness.state().test_virtual_item_count();
        assert!(harness.state_mut().test_add_trim_virtual_frac(0.10, 0.40));
        harness.run_steps(6);
        assert!(
            harness.state().test_virtual_item_count() > virtual_before,
            "trim as virtual must add a virtual item"
        );
        assert_unchanged(&path, &before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_convert_sets_override_supports_undo_and_exports_audio() {
        let dir = make_temp_dir("format_override");
        let export_dir = dir.join("exports");
        std::fs::create_dir_all(&export_dir).expect("create export dir");
        let path = dir.join("fixture.wav");
        let chans = synth_stereo(48_000, 2.0);
        neowaves::wave::export_channels_audio(&chans, 48_000, &path).expect("write fixture");
        let before = snapshot(&path);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);

        assert!(harness.state_mut().test_convert_format_selected_to("mp3"));
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_selected_format_override().as_deref(),
            Some("mp3")
        );
        let display = harness
            .state()
            .test_selected_display_name()
            .expect("selected display name");
        assert!(display.to_ascii_lowercase().ends_with(".mp3"));
        assert_unchanged(&path, &before);

        assert!(harness.state_mut().test_list_undo());
        harness.run_steps(2);
        assert!(harness.state().test_selected_format_override().is_none());
        let display = harness
            .state()
            .test_selected_display_name()
            .expect("selected display name after undo");
        assert!(display.to_ascii_lowercase().ends_with(".wav"));
        assert_unchanged(&path, &before);

        assert!(harness.state_mut().test_convert_format_selected_to("mp3"));
        harness.run_steps(2);
        harness.state_mut().test_set_export_first_prompt(false);
        harness
            .state_mut()
            .test_set_export_save_mode_overwrite(false);
        harness.state_mut().test_set_export_conflict("rename");
        harness
            .state_mut()
            .test_set_export_dest_folder(Some(&export_dir));
        harness.state_mut().test_set_export_name_template("{name}_fmt");
        harness.state_mut().test_trigger_save_selected();
        wait_for_export_finish(&mut harness);

        let mut found_mp3 = None;
        for entry in std::fs::read_dir(&export_dir).expect("read export dir") {
            let path = entry.expect("entry").path();
            if path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("mp3"))
                .unwrap_or(false)
            {
                found_mp3 = Some(path);
                break;
            }
        }
        let out = found_mp3.expect("missing exported mp3");
        let info = neowaves::audio_io::read_audio_info(&out).expect("probe exported mp3");
        assert!(info.sample_rate > 0);
        assert!(info.channels > 0);
        assert!(info.duration_secs.unwrap_or(0.0) > 0.0);
        assert_unchanged(&path, &before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_apply_is_immediately_used_by_list_space_playback() {
        let dir = make_temp_dir("editor_apply_list_playback");
        let path = dir.join("fixture.wav");
        let chans = synth_stereo(48_000, 4.0);
        neowaves::wave::export_channels_audio(&chans, 48_000, &path).expect("write fixture");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_and_load_row(0));
        harness.run_steps(2);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);

        let before_len = harness.state().test_tab_samples_len();
        assert!(harness.state_mut().test_apply_trim_frac(0.0, 0.25));
        harness.run_steps(2);
        let after_len = harness.state().test_tab_samples_len();
        assert!(after_len < before_len);
        assert!(harness.state().test_tab_dirty());

        harness.state_mut().test_switch_to_list();
        harness.run_steps(1);
        assert!(harness.state_mut().test_select_path(&path));
        assert!(harness.state_mut().test_evict_selected_list_preview_cache());

        let started_immediately = harness
            .state_mut()
            .test_force_load_selected_list_preview_for_play();
        assert!(
            started_immediately,
            "dirty editor audio should be used immediately for list Space playback"
        );

        harness.run_steps(2);
        let list_len = harness.state().test_audio_buffer_len();
        assert!(list_len > 0);
        assert!(
            list_len.abs_diff(after_len) <= 2,
            "list buffer length should match applied editor length: list={} editor={}",
            list_len,
            after_len
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
