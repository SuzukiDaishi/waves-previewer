#[cfg(feature = "kittest")]
mod batch_qa_inspection {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui_kittest::Harness;
    use neowaves::app::inspection::{InspectionConfig, IssueSeverity};
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
            "neowaves_batch_qa_inspection_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn tone(sr: u32, secs: f32, amp: f32) -> Vec<f32> {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        (0..frames)
            .map(|i| ((i as f32 / sr as f32) * 440.0 * std::f32::consts::TAU).sin() * amp)
            .collect()
    }

    /// Fixture set exercising each check:
    /// - hot.wav: near-full-scale tone -> true-peak warning
    /// - quiet_silence.wav: 500 ms leading silence + very quiet tone -> silence + LUFS warnings
    /// - bad_loop.wav: valid audio, loop end beyond the file -> loop Error
    /// - clean.wav: moderate tone, no silence padding, valid loop
    fn write_fixtures(dir: &PathBuf) {
        let sr = 48_000u32;
        neowaves::wave::export_channels_audio(&[tone(sr, 0.4, 0.99)], sr, &dir.join("hot.wav"))
            .expect("hot");
        let mut quiet = vec![0.0f32; (sr / 2) as usize];
        quiet.extend(tone(sr, 0.4, 0.01));
        neowaves::wave::export_channels_audio(&[quiet], sr, &dir.join("quiet_silence.wav"))
            .expect("quiet");
        let bad_loop = dir.join("bad_loop.wav");
        neowaves::wave::export_channels_audio(&[tone(sr, 0.4, 0.4)], sr, &bad_loop).expect("bad");
        neowaves::loop_markers::write_loop_markers(&bad_loop, Some((0, 10_000_000)))
            .expect("write bad loop");
        let clean = dir.join("clean.wav");
        neowaves::wave::export_channels_audio(&[tone(sr, 0.4, 0.4)], sr, &clean).expect("clean");
        neowaves::loop_markers::write_loop_markers(&clean, Some((1000, 10_000)))
            .expect("write clean loop");
    }

    fn harness_with_folder(dir: PathBuf) -> Harness<'static, WavesPreviewer> {
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        cfg.open_first = false;
        harness_with_startup(cfg)
    }

    fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>, count: usize) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().files.len() >= count {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!("scan timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn qa_cfg() -> InspectionConfig {
        InspectionConfig {
            // Ignore loudness on 3 of 4 fixtures by widening the window is
            // fiddly; instead use defaults but a very loose target picked so
            // only the extremely quiet file trips the check.
            target_lufs: -14.0,
            lufs_tolerance_lu: 12.0,
            ..Default::default()
        }
    }

    fn run_inspection(
        harness: &mut Harness<'static, WavesPreviewer>,
        cfg: InspectionConfig,
    ) {
        let paths: Vec<PathBuf> = harness.state().test_visible_list_paths();
        harness.state_mut().test_set_inspection_cfg(cfg);
        harness.state_mut().test_begin_inspection_run(paths);
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_inspection_run_active() {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                panic!("inspection run timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn inspection_flags_expected_files() {
        let dir = make_temp_dir("run");
        write_fixtures(&dir);
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness, 4);

        run_inspection(&mut harness, qa_cfg());

        assert_eq!(harness.state().test_inspection_report_rows(), 4);
        assert_eq!(harness.state().test_inspection_report_cancelled(), Some(false));

        let hot = harness
            .state()
            .test_inspection_row_for_file("hot.wav")
            .expect("hot row");
        assert_eq!(hot.severity, Some(IssueSeverity::Warning), "{hot:?}");
        assert!(hot
            .issues
            .iter()
            .any(|i| format!("{:?}", i.kind).contains("TruePeak")));

        let quiet = harness
            .state()
            .test_inspection_row_for_file("quiet_silence.wav")
            .expect("quiet row");
        assert_eq!(quiet.severity, Some(IssueSeverity::Warning));
        assert!(quiet
            .issues
            .iter()
            .any(|i| format!("{:?}", i.kind).contains("LeadingSilence")));
        assert!(quiet.leading_silence_ms.unwrap_or(0.0) > 400.0);

        let bad = harness
            .state()
            .test_inspection_row_for_file("bad_loop.wav")
            .expect("bad row");
        assert_eq!(bad.severity, Some(IssueSeverity::Error), "{bad:?}");
        assert!(bad
            .issues
            .iter()
            .any(|i| format!("{:?}", i.kind).contains("LoopInvalid")));

        let clean = harness
            .state()
            .test_inspection_row_for_file("clean.wav")
            .expect("clean row");
        assert_eq!(clean.severity, None, "clean must pass: {clean:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inspection_cancel_produces_partial_report() {
        let dir = make_temp_dir("cancel");
        write_fixtures(&dir);
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness, 4);

        let paths: Vec<PathBuf> = harness.state().test_visible_list_paths();
        harness.state_mut().test_set_inspection_cfg(qa_cfg());
        harness.state_mut().test_begin_inspection_run(paths);
        harness.state_mut().test_cancel_inspection_run();

        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_inspection_run_active() {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                panic!("cancelled inspection never finished");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(harness.state().test_inspection_report_cancelled(), Some(true));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
