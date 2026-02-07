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
        assert_eq!(after.len, before.len, "file size changed: {}", path.display());
        if let (Some(b), Some(a)) = (before.modified, after.modified) {
            assert_eq!(
                a, b,
                "file modified timestamp changed: {}",
                path.display()
            );
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

        assert!(harness.state_mut().test_apply_selected_resample_override(44_100));
        harness.run_steps(2);
        assert_eq!(harness.state().test_selected_sample_rate_override(), Some(44_100));
        assert_eq!(
            std::fs::read_dir(&dir).expect("read dir").count(),
            1,
            "non-export operation must not create files"
        );
        assert_unchanged(&path, &before);

        assert!(
            harness
                .state_mut()
                .test_convert_bits_selected_to(neowaves::wave::WavBitDepth::Pcm16)
        );
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
}
