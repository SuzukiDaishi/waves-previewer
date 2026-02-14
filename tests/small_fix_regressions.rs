#[cfg(feature = "kittest")]
mod small_fix_regressions {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui_kittest::Harness;
    use hound::{SampleFormat, WavSpec, WavWriter};
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
            "neowaves_small_fix_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
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
                return;
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
                return;
            }
            if start.elapsed() > Duration::from_secs(15) {
                panic!("tab timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_bits_label(harness: &mut Harness<'static, WavesPreviewer>) -> String {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if let Some(label) = harness.state().test_selected_bits_label() {
                if !label.is_empty() && label != "-" {
                    return label;
                }
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("bits label timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn write_wav_32_int(path: &Path, sr: u32, secs: f32) {
        let spec = WavSpec {
            channels: 2,
            sample_rate: sr,
            bits_per_sample: 32,
            sample_format: SampleFormat::Int,
        };
        let mut writer = WavWriter::create(path, spec).expect("create wav int32");
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        for i in 0..frames {
            let t = (i as f32) / (sr as f32);
            let l = ((t * 220.0 * std::f32::consts::TAU).sin() * (i32::MAX as f32) * 0.25) as i32;
            let r = ((t * 440.0 * std::f32::consts::TAU).sin() * (i32::MAX as f32) * 0.20) as i32;
            writer.write_sample(l).expect("write left");
            writer.write_sample(r).expect("write right");
        }
        writer.finalize().expect("finalize int32");
    }

    fn write_wav_32_float(path: &Path, sr: u32, secs: f32) {
        let spec = WavSpec {
            channels: 2,
            sample_rate: sr,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut writer = WavWriter::create(path, spec).expect("create wav float32");
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        for i in 0..frames {
            let t = (i as f32) / (sr as f32);
            let l = (t * 220.0 * std::f32::consts::TAU).sin() * 0.25;
            let r = (t * 440.0 * std::f32::consts::TAU).sin() * 0.20;
            writer.write_sample(l).expect("write left");
            writer.write_sample(r).expect("write right");
        }
        writer.finalize().expect("finalize float32");
    }

    #[test]
    fn list_bits_shows_32i_and_32f() {
        let dir = make_temp_dir("bits");
        let wav_i = dir.join("int32.wav");
        let wav_f = dir.join("float32.wav");
        write_wav_32_int(&wav_i, 48_000, 0.5);
        write_wav_32_float(&wav_f, 48_000, 0.5);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);

        assert!(harness.state_mut().test_select_path(&wav_i));
        let label_i = wait_for_bits_label(&mut harness);
        assert_eq!(label_i, "32i");

        assert!(harness.state_mut().test_select_path(&wav_f));
        let label_f = wait_for_bits_label(&mut harness);
        assert_eq!(label_f, "32f");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trim_virtual_keeps_editor_context() {
        let dir = make_temp_dir("trim_ctx");
        let src = dir.join("src.wav");
        neowaves::wave::export_channels_audio(
            &vec![vec![0.0f32; 48_000 * 2], vec![0.0f32; 48_000 * 2]],
            48_000,
            &src,
        )
        .expect("export source");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);
        let tab_path_before = harness.state().test_active_tab_path().expect("tab path");
        let tab_len_before = harness
            .state()
            .active_tab
            .and_then(|i| harness.state().tabs.get(i))
            .map(|t| t.samples_len)
            .expect("tab len");
        let audio_len_before = harness.state().test_audio_buffer_len();
        let virtual_before = harness.state().test_virtual_item_count();

        assert!(harness.state_mut().test_add_trim_virtual_frac(0.10, 0.40));
        harness.run_steps(3);

        let tab_path_after = harness
            .state()
            .test_active_tab_path()
            .expect("tab path after");
        let tab_len_after = harness
            .state()
            .active_tab
            .and_then(|i| harness.state().tabs.get(i))
            .map(|t| t.samples_len)
            .expect("tab len after");
        let audio_len_after = harness.state().test_audio_buffer_len();
        let virtual_after = harness.state().test_virtual_item_count();

        assert_eq!(tab_path_after, tab_path_before);
        assert_eq!(tab_len_after, tab_len_before);
        assert_eq!(audio_len_after, audio_len_before);
        assert!(virtual_after > virtual_before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_virtual_works_and_extension_is_locked() {
        let dir = make_temp_dir("rename");
        let src = dir.join("source.wav");
        neowaves::wave::export_channels_audio(
            &vec![vec![0.0f32; 48_000], vec![0.0f32; 48_000]],
            48_000,
            &src,
        )
        .expect("export source");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_first_tab());
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_add_trim_virtual_frac(0.20, 0.60));
        harness.run_steps(3);
        harness.state_mut().test_switch_to_list();
        harness.run_steps(2);

        let virtual_path_before = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("virtual selected");
        assert!(harness
            .state_mut()
            .test_rename_selected_to("renamed_virtual.mp3"));
        harness.run_steps(1);
        let virtual_path_after = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("virtual selected after");
        let virtual_name = harness
            .state()
            .test_selected_display_name()
            .expect("virtual display name");
        assert_eq!(virtual_path_after, virtual_path_before);
        assert!(virtual_name.starts_with("renamed_virtual"));
        assert!(virtual_name.ends_with(".wav"));
        assert!(!virtual_name.ends_with(".mp3"));

        assert!(harness.state_mut().test_select_path(&src));
        assert!(harness
            .state_mut()
            .test_rename_selected_to("renamed_real.mp3"));
        harness.run_steps(1);
        let renamed_real = dir.join("renamed_real.wav");
        assert!(renamed_real.exists(), "renamed real file should exist");
        assert!(!src.exists(), "old real file should be renamed away");
        let selected_real = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected real path");
        assert_eq!(selected_real, renamed_real);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sort_unknown_numeric_values_are_always_bottom() {
        let dir = make_temp_dir("sort_unknown");
        let wav_lo = dir.join("a_22050.wav");
        let wav_hi = dir.join("b_48000.wav");
        let bad_mp3 = dir.join("z_unknown.mp3");
        write_wav_32_float(&wav_lo, 22_050, 0.25);
        write_wav_32_float(&wav_hi, 48_000, 0.25);
        std::fs::write(&bad_mp3, b"not-a-real-mp3").expect("write fake mp3");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        harness.state_mut().test_sort_sample_rate_asc();
        harness.run_steps(2);
        let files_len = harness.state().files.len();
        assert!(files_len >= 3, "expected all fixture files in list");
        let first_asc = harness
            .state()
            .test_row_path(0)
            .expect("first row asc");
        let last_asc = harness
            .state()
            .test_row_path(files_len - 1)
            .expect("last row asc");
        assert_eq!(first_asc, wav_lo);
        assert_eq!(last_asc, bad_mp3);

        harness.state_mut().test_sort_sample_rate_desc();
        harness.run_steps(2);
        let first_desc = harness
            .state()
            .test_row_path(0)
            .expect("first row desc");
        let last_desc = harness
            .state()
            .test_row_path(files_len - 1)
            .expect("last row desc");
        assert_eq!(first_desc, wav_hi);
        assert_eq!(last_desc, bad_mp3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn normal_space_play_uses_pitch_or_stretch_processing_path() {
        let dir = make_temp_dir("normal_play_mode");
        let src = dir.join("mode_check.wav");
        write_wav_32_float(&src, 48_000, 0.5);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_path(&src));
        harness.run_steps(2);

        harness.state_mut().test_set_mode_pitch_shift();
        harness.state_mut().test_set_pitch_semitones(5.0);
        let immediate = harness.state_mut().test_force_load_selected_list_preview_for_play();
        assert!(
            !immediate,
            "pitch/time mode should enqueue heavy processing for normal list play"
        );
        assert!(
            harness.state().test_processing_autoplay_when_ready(),
            "processing state should keep autoplay intent for normal list play"
        );

        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().test_audio_is_playing() {
                break;
            }
            if start.elapsed() > Duration::from_secs(8) {
                panic!("normal list play did not auto-start after heavy processing");
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
