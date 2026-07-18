#[cfg(feature = "kittest")]
mod p7_pipeline {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use egui_kittest::Harness;
    use neowaves::kittest::harness_with_startup;
    use neowaves::{StartupConfig, WavesPreviewer};

    fn make_temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("neowaves_p7_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
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

    #[test]
    fn column_reorder_moves_headers_and_persists_in_session() {
        use egui_kittest::kittest::Queryable;
        use neowaves::app::ColumnId;
        let sr = 48_000u32;
        let dir = make_temp_dir("colorder");
        let tone: Vec<f32> = (0..(sr / 10) as usize)
            .map(|i| (i as f32 / sr as f32 * 330.0 * std::f32::consts::TAU).sin() * 0.3)
            .collect();
        neowaves::wave::export_channels_audio(&[tone].to_vec(), sr, &dir.join("a.wav"))
            .expect("export fixture");
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        harness.run_steps(2);
        // Default order: Length header is left of Bits.
        let length_x = harness.get_by_label("Length").rect().left();
        let bits_x = harness.get_by_label("Bits").rect().left();
        assert!(length_x < bits_x, "default order: Length left of Bits");
        // Move Bits before Length and re-render.
        let mut order = harness.state().list_column_order.clone();
        let bi = order.iter().position(|c| *c == ColumnId::Bits).unwrap();
        let li = order.iter().position(|c| *c == ColumnId::Length).unwrap();
        let bits_col = order.remove(bi);
        order.insert(li, bits_col);
        harness.state_mut().list_column_order = order.clone();
        harness.run_steps(3);
        let length_x = harness.get_by_label("Length").rect().left();
        let bits_x = harness.get_by_label("Bits").rect().left();
        assert!(
            bits_x < length_x,
            "reordered: Bits ({bits_x}) must sit left of Length ({length_x})"
        );
        // Round-trip through a session file.
        let sess = dir.join("order.nwsess");
        assert!(harness.state_mut().test_save_session_to(&sess));
        harness.state_mut().list_column_order = ColumnId::ALL.to_vec();
        assert!(harness.state_mut().test_open_session_from(&sess));
        harness.run_steps(3);
        assert_eq!(harness.state().list_column_order, order);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn silence_columns_fill_lead_and_tail_ms() {
        let sr = 48_000u32;
        let dir = make_temp_dir("silcols");
        // 100 ms silence + 200 ms tone + 50 ms silence.
        let mut ch = vec![0.0f32; (sr / 10) as usize];
        ch.extend(
            (0..(sr / 5) as usize)
                .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5),
        );
        ch.extend(vec![0.0f32; (sr / 20) as usize]);
        let path = dir.join("padded.wav");
        neowaves::wave::export_channels_audio(&[ch].to_vec(), sr, &path).expect("export fixture");

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        harness.state_mut().test_set_silence_columns(true);
        // Rendering the visible row queues the full-decode metadata job.
        wait_until(&mut harness, "silence meta", |h| {
            h.state().test_meta_silence_ms(&path).is_some()
        });
        let (lead, tail) = harness.state().test_meta_silence_ms(&path).expect("silence");
        assert!(
            (lead - 100.0).abs() <= 15.0,
            "lead silence ~100 ms, got {lead}"
        );
        assert!((tail - 50.0).abs() <= 15.0, "tail silence ~50 ms, got {tail}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
