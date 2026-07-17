#[cfg(feature = "kittest")]
mod p4_usability {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_with_startup;
    use neowaves::{StartupConfig, WavesPreviewer};

    fn make_temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("neowaves_p4_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn tone(sr: u32, freq: f32) -> Vec<Vec<f32>> {
        vec![(0..(sr / 4) as usize)
            .map(|i| (i as f32 / sr as f32 * freq * std::f32::consts::TAU).sin() * 0.4)
            .collect()]
    }

    fn wait_until(
        harness: &mut Harness<'static, WavesPreviewer>,
        what: &str,
        mut done: impl FnMut(&Harness<'static, WavesPreviewer>) -> bool,
    ) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if done(harness) {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!("timeout waiting for {what}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn harness_with_files(tag: &str, n: usize) -> (Harness<'static, WavesPreviewer>, PathBuf) {
        let sr = 48_000u32;
        let dir = make_temp_dir(tag);
        for i in 0..n {
            neowaves::wave::export_channels_audio(
                &tone(sr, 300.0 + 50.0 * i as f32),
                sr,
                &dir.join(format!("f{i}.wav")),
            )
            .expect("export fixture");
        }
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= n);
        (harness, dir)
    }

    #[test]
    fn select_all_and_clear_selection() {
        let (mut harness, dir) = harness_with_files("selall", 4);
        harness.state_mut().test_list_select_all();
        assert_eq!(harness.state().test_selected_multi_len(), 4);
        harness.state_mut().test_list_clear_selection();
        assert_eq!(harness.state().test_selected_multi_len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn right_click_inside_multi_selection_preserves_it() {
        let (mut harness, dir) = harness_with_files("rclick", 4);
        harness.state_mut().test_list_select_all();
        assert_eq!(harness.state().test_selected_multi_len(), 4);
        // Right-click inside the selection: keep all 4 rows selected.
        harness.state_mut().test_row_secondary_click(2);
        assert_eq!(
            harness.state().test_selected_multi_len(),
            4,
            "right-click inside the selection must not collapse it"
        );
        // Right-click outside (after clearing): selects that row.
        harness.state_mut().test_list_clear_selection();
        harness.state_mut().test_row_secondary_click(1);
        assert_eq!(harness.state().selected, Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

}
