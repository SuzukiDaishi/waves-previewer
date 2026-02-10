#[cfg(feature = "kittest")]
mod workflow_bulk_operations {
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
            "neowaves_workflow_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn synth_stereo(sr: u32, secs: f32, f1: f32, f2: f32) -> Vec<Vec<f32>> {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for i in 0..frames {
            let t = (i as f32) / (sr as f32);
            left.push((t * f1 * std::f32::consts::TAU).sin() * 0.25);
            right.push((t * f2 * std::f32::consts::TAU).sin() * 0.20);
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
            if start.elapsed() > Duration::from_secs(15) {
                panic!("scan timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_tab_ready(harness: &mut Harness<'static, WavesPreviewer>, path: &Path) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let loading = harness.state().test_tab_loading();
            let samples_len = harness.state().test_tab_samples_len();
            let ready = !loading && samples_len > 0;
            if ready {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                let active_path = harness
                    .state()
                    .test_active_tab_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(none)".to_string());
                panic!(
                    "tab ready timeout: path={} active={} loading={} samples_len={}",
                    path.display(),
                    active_path,
                    loading,
                    samples_len
                );
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
            if start.elapsed() > Duration::from_secs(40) {
                panic!("export timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_editor_apply(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_editor_apply_active() {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                panic!("editor apply timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_sample_rate_overrides(
        harness: &mut Harness<'static, WavesPreviewer>,
        expected: usize,
    ) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().test_sample_rate_override_count() >= expected {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!("sample rate override timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn assert_decode_ok(path: &Path) {
        let info = neowaves::audio_io::read_audio_info(path)
            .unwrap_or_else(|e| panic!("probe failed for {}: {e}", path.display()));
        assert!(info.sample_rate > 0 && info.channels > 0);
        let (decoded, sr) = neowaves::audio_io::decode_audio_multi(path)
            .unwrap_or_else(|e| panic!("decode failed for {}: {e}", path.display()));
        assert!(sr > 0);
        assert!(!decoded.is_empty() && !decoded[0].is_empty());
    }

    fn seed_sources(dir: &Path) -> Vec<PathBuf> {
        let sr = 44_100;
        let secs = 0.35;
        let chans = synth_stereo(sr, secs, 220.0, 440.0);
        let mut paths = Vec::new();
        for ext in ["wav", "mp3", "m4a", "ogg"] {
            let path = dir.join(format!("src_{ext}.{ext}"));
            neowaves::wave::export_channels_audio(&chans, sr, &path)
                .unwrap_or_else(|e| panic!("seed export {ext} failed: {e}"));
            paths.push(path);
        }
        paths
    }

    fn collect_outputs(dir: &Path, ext: &str, suffix: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).expect("read export dir") {
            let path = entry.expect("entry").path();
            let matches_ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(ext))
                .unwrap_or(false);
            let matches_suffix = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.contains(suffix))
                .unwrap_or(false);
            if matches_ext && matches_suffix {
                out.push(path);
            }
        }
        out
    }

    #[test]
    fn bulk_format_convert_all_formats() {
        let dir = make_temp_dir("bulk_convert");
        let sources = seed_sources(&dir);
        let export_dir = dir.join("exports");
        std::fs::create_dir_all(&export_dir).expect("create export dir");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_paths_multi(&sources));
        harness.run_steps(2);

        for target in ["wav", "mp3", "m4a", "ogg"] {
            assert!(harness.state_mut().test_select_paths_multi(&sources));
            assert!(harness.state_mut().test_convert_format_selected_to(target));
            harness.run_steps(2);
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
                .test_set_export_name_template(&format!("{{name}}_wf_{target}"));
            harness
                .state_mut()
                .test_set_export_format_override(Some(target));
            harness.state_mut().test_trigger_save_selected();
            wait_for_export_finish(&mut harness);
            harness.run_steps(2);

            let suffix = format!("_wf_{target}");
            let outputs = collect_outputs(&export_dir, target, &suffix);
            let expected = sources
                .iter()
                .filter(|p| {
                    p.extension()
                        .and_then(|s| s.to_str())
                        .map(|s| !s.eq_ignore_ascii_case(target))
                        .unwrap_or(true)
                })
                .count();
            assert!(
                outputs.len() >= expected,
                "missing outputs for {target}: got={}, expected>={}",
                outputs.len(),
                expected
            );
            for path in outputs {
                assert_decode_ok(&path);
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bulk_resample_all_formats_to_fixed_sr() {
        let dir = make_temp_dir("bulk_resample");
        let sources = seed_sources(&dir);
        let export_dir = dir.join("exports");
        std::fs::create_dir_all(&export_dir).expect("create export dir");
        let target_sr = 48_000;

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_paths_multi(&sources));
        harness.run_steps(2);

        assert!(harness
            .state_mut()
            .test_apply_selected_resample_override(target_sr));
        wait_for_sample_rate_overrides(&mut harness, sources.len());

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
            .test_set_export_name_template("{name}_wf_sr");
        harness.state_mut().test_set_export_format_override(None);
        harness.state_mut().test_trigger_save_selected();
        wait_for_export_finish(&mut harness);
        harness.run_steps(2);

        for ext in ["wav", "mp3", "m4a", "ogg"] {
            let outputs = collect_outputs(&export_dir, ext, "_wf_sr");
            for path in outputs {
                let info = neowaves::audio_io::read_audio_info(&path)
                    .unwrap_or_else(|e| panic!("probe failed for {}: {e}", path.display()));
                assert_eq!(
                    info.sample_rate, target_sr,
                    "resample output sr mismatch: {}",
                    path.display()
                );
                assert_decode_ok(&path);
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_fx_apply_and_export_all_formats() {
        let dir = make_temp_dir("editor_fx_export");
        let sources = seed_sources(&dir);
        let export_dir = dir.join("exports");
        std::fs::create_dir_all(&export_dir).expect("create export dir");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);

        for path in &sources {
            assert!(harness.state_mut().test_select_path(path));
            assert!(harness.state_mut().test_open_tab_for_path(path));
            wait_for_tab_ready(&mut harness, path);

            let before_len = harness.state().test_tab_samples_len();
            assert!(harness
                .state_mut()
                .test_apply_gain(0.10, 0.90, -6.0));
            assert!(harness.state_mut().test_apply_pitch_shift(3.0));
            wait_for_editor_apply(&mut harness);
            assert!(harness.state_mut().test_apply_time_stretch(1.2));
            wait_for_editor_apply(&mut harness);
            let after_len = harness.state().test_tab_samples_len();
            assert!(
                before_len != after_len,
                "time stretch should change length"
            );

            harness.state_mut().test_switch_to_list();
            assert!(harness.state_mut().test_select_path(path));

            for target in ["wav", "mp3", "m4a", "ogg"] {
                assert!(harness.state_mut().test_select_path(path));
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
                    .test_set_export_name_template(&format!("{{name}}_wf_fx_{target}"));
                harness
                    .state_mut()
                    .test_set_export_format_override(Some(target));
                harness.state_mut().test_trigger_save_selected();
                wait_for_export_finish(&mut harness);
                harness.run_steps(2);

                let suffix = format!("_wf_fx_{target}");
                let outputs = collect_outputs(&export_dir, target, &suffix);
                assert!(!outputs.is_empty(), "missing fx outputs for {target}");
                for out in outputs {
                    assert_decode_ok(&out);
                }
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dsp_fx_direct_export_smoke() {
        let dir = make_temp_dir("dsp_fx_direct");
        let sr = 48_000;
        let chans = synth_stereo(sr, 0.30, 330.0, 550.0);
        let mut mono = Vec::with_capacity(chans[0].len());
        for i in 0..chans[0].len() {
            let l = chans[0][i];
            let r = chans[1][i];
            mono.push((l + r) * 0.5);
        }
        let pitched = neowaves::wave::process_pitchshift_offline(&mono, sr, sr, 2.0);
        let stretched = neowaves::wave::process_timestretch_offline(&pitched, sr, sr, 1.1);
        let out = vec![stretched.clone(), stretched.clone()];

        for ext in ["wav", "mp3", "m4a", "ogg"] {
            let path = dir.join(format!("dsp_fx.{ext}"));
            neowaves::wave::export_channels_audio(&out, sr, &path)
                .unwrap_or_else(|e| panic!("dsp export {ext} failed: {e}"));
            assert_decode_ok(&path);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
