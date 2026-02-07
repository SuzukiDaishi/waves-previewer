#[cfg(feature = "kittest")]
mod virtual_export_behavior {
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
            "neowaves_virtual_export_behavior_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn synth_stereo(sr: u32, secs: f32, freq_l: f32, freq_r: f32) -> Vec<Vec<f32>> {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for i in 0..frames {
            let t = (i as f32) / (sr as f32);
            left.push((t * freq_l * std::f32::consts::TAU).sin() * 0.30);
            right.push((t * freq_r * std::f32::consts::TAU).sin() * 0.25);
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

    fn newest_file_with_ext(dir: &Path, ext: &str) -> Option<PathBuf> {
        let mut latest: Option<(std::time::SystemTime, PathBuf)> = None;
        for ent in std::fs::read_dir(dir).ok()? {
            let ent = ent.ok()?;
            let p = ent.path();
            let matches = p
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
                _ => latest = Some((modified, p)),
            }
        }
        latest.map(|(_, p)| p)
    }

    #[test]
    fn virtual_export_works_for_wav_mp3_m4a_ogg() {
        for ext in ["wav", "mp3", "m4a", "ogg"] {
            let dir = make_temp_dir(&format!("virtual_formats_{ext}"));
            let src = dir.join("a_source.wav");
            let chans = synth_stereo(48_000, 4.0, 220.0, 440.0);
            neowaves::wave::export_channels_audio(&chans, 48_000, &src).expect("export source");
            let export_dir = dir.join("exports");
            std::fs::create_dir_all(&export_dir).expect("create export dir");

            let mut harness = harness_with_folder(dir.clone());
            wait_for_scan(&mut harness);
            assert!(harness.state_mut().test_open_first_tab());
            wait_for_tab_ready(&mut harness);
            assert!(harness.state_mut().test_add_trim_virtual_frac(0.2, 0.6));
            harness.run_steps(3);
            harness.state_mut().test_switch_to_list();

            let current = harness
                .state()
                .test_selected_path()
                .cloned()
                .expect("selected virtual path should exist");
            assert!(harness.state_mut().test_select_path(&current));
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
                .test_set_export_name_template("{name}_virt_export");
            harness
                .state_mut()
                .test_set_export_format_override(Some(ext));
            harness.state_mut().test_trigger_save_selected();
            wait_for_export_finish(&mut harness);
            harness.run_steps(2);

            let out = newest_file_with_ext(&export_dir, ext)
                .unwrap_or_else(|| panic!("missing exported .{ext}"));
            let info = neowaves::audio_io::read_audio_info(&out)
                .unwrap_or_else(|e| panic!("probe failed for {}: {e}", out.display()));
            assert!(info.sample_rate > 0 && info.channels > 0);
            let (decoded, sr) = neowaves::audio_io::decode_audio_multi(&out)
                .unwrap_or_else(|e| panic!("decode failed for {}: {e}", out.display()));
            assert!(sr > 0);
            assert!(!decoded.is_empty() && !decoded[0].is_empty());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn virtual_length_does_not_leak_to_other_file_playback() {
        let dir = make_temp_dir("virtual_length_leak");
        let src_short = dir.join("a_short.wav");
        let src_long = dir.join("b_long.wav");
        neowaves::wave::export_channels_audio(
            &synth_stereo(48_000, 4.0, 220.0, 330.0),
            48_000,
            &src_short,
        )
        .expect("export short source");
        neowaves::wave::export_channels_audio(
            &synth_stereo(48_000, 10.0, 330.0, 550.0),
            48_000,
            &src_long,
        )
        .expect("export long source");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_add_trim_virtual_frac(0.1, 0.3));
        harness.run_steps(3);
        harness.state_mut().test_switch_to_list();

        let virtual_path = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("virtual should be selected");
        assert!(harness.state_mut().test_select_path(&virtual_path));
        let _ = harness
            .state_mut()
            .test_force_load_selected_list_preview_for_play();
        harness.run_steps(3);
        let virtual_len = harness.state().test_audio_buffer_len();
        assert!(virtual_len > 0);

        assert!(harness.state_mut().test_select_path(&src_long));
        let _ = harness
            .state_mut()
            .test_force_load_selected_list_preview_for_play();
        let start = Instant::now();
        let long_len = loop {
            harness.run_steps(1);
            let len = harness.state().test_audio_buffer_len();
            if len > virtual_len * 2 {
                break len;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!(
                    "long file playback remained too short: virtual_len={} current_len={}",
                    virtual_len, len
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(long_len > virtual_len * 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
