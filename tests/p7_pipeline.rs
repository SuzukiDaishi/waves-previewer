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
        // Move Bits before Length and re-render, the way the List Columns
        // window does. `list_column_order` is a *derived* projection of
        // `list_column_layout` (see `sanitize_list_column_layout`), so writing
        // to it moves nothing and is overwritten on the next frame -- which is
        // what this test used to do.
        assert!(harness
            .state_mut()
            .test_move_column_before(ColumnId::Bits, ColumnId::Length));
        let order = harness.state().list_column_order.clone();
        harness.run_steps(3);
        let length_x = harness.get_by_label("Length").rect().left();
        let bits_x = harness.get_by_label("Bits").rect().left();
        assert!(
            bits_x < length_x,
            "reordered: Bits ({bits_x}) must sit left of Length ({length_x})"
        );
        // Round-trip through a session file. The layout is what is written and
        // restored; the order follows from it.
        let sess = dir.join("order.nwsess");
        let layout = harness.state().test_list_column_layout();
        assert!(harness.state_mut().test_save_session_to(&sess));
        assert!(harness
            .state_mut()
            .test_move_column_before(ColumnId::Length, ColumnId::Bits));
        assert_ne!(harness.state().test_list_column_layout(), layout);
        assert!(harness.state_mut().test_open_session_from(&sess));
        harness.run_steps(3);
        assert_eq!(harness.state().test_list_column_layout(), layout);
        assert_eq!(harness.state().list_column_order, order);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_watch_applies_added_and_removed_files() {
        let sr = 48_000u32;
        let dir = make_temp_dir("watch");
        let tone: Vec<f32> = (0..(sr / 10) as usize)
            .map(|i| (i as f32 / sr as f32 * 220.0 * std::f32::consts::TAU).sin() * 0.3)
            .collect();
        neowaves::wave::export_channels_audio(&[tone.clone()].to_vec(), sr, &dir.join("a.wav"))
            .expect("export fixture");
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        harness.state_mut().test_set_watch_interval_ms(50);
        wait_until(&mut harness, "watch spawned", |h| {
            h.state().test_watch_active()
        });
        // Give the poller its baseline snapshot.
        std::thread::sleep(Duration::from_millis(150));
        harness.run_steps(2);

        // A file dropped into the folder appears in the list...
        let added = dir.join("b.wav");
        // Bypass the app's own exporter so this doesn't register as a
        // self-write (external tools are what the watch is for).
        {
            let src = dir.join("a.wav");
            std::fs::copy(&src, &added).expect("copy new file");
        }
        wait_until(&mut harness, "added file picked up", |h| {
            h.state().files.len() >= 2
        });

        // ...and deleting it removes the row again.
        std::fs::remove_file(&added).expect("remove file");
        wait_until(&mut harness, "removed file dropped", |h| {
            h.state().files.len() == 1
        });
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
        let (lead, tail) = harness
            .state()
            .test_meta_silence_ms(&path)
            .expect("silence");
        assert!(
            (lead - 100.0).abs() <= 15.0,
            "lead silence ~100 ms, got {lead}"
        );
        assert!(
            (tail - 50.0).abs() <= 15.0,
            "tail silence ~50 ms, got {tail}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn qa_columns_fill_edge_and_blank_from_full_decode() {
        let sr = 48_000u32;
        let dir = make_temp_dir("qacols");
        // 100 ms blank + 200 ms tone + 50 ms blank, with a loud *first* sample
        // so Edge Zero has something to complain about. The tail is left
        // untouched so the trailing blank is still there for Blank Pad —
        // spiking the last sample too would zero the trailing blank out.
        let mut ch = vec![0.0f32; (sr / 10) as usize];
        ch.extend(
            (0..(sr / 5) as usize)
                .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5),
        );
        ch.extend(vec![0.0f32; (sr / 20) as usize]);
        ch[0] = 0.5;
        let path = dir.join("qa.wav");
        neowaves::wave::export_channels_audio(&[ch].to_vec(), sr, &path).expect("export fixture");

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        harness.state_mut().test_set_qa_columns(true);
        wait_until(&mut harness, "qa meta", |h| {
            h.state().test_meta_blank_pad(&path).is_some()
        });

        let (first_abs, last_abs) = harness.state().test_meta_edge_abs(&path).expect("edge");
        assert!(
            first_abs > 0.4,
            "first sample kept its level, got {first_abs}"
        );
        assert!(
            last_abs < 1e-3,
            "last sample is still silent, got {last_abs}"
        );
        assert_eq!(
            harness.state().test_qa_column_is_ng("edge_zero", &path),
            Some(true),
            "a loud first sample alone must report NG"
        );
        // 0.5 peak is well under 0 dBFS.
        assert_eq!(
            harness.state().test_qa_column_is_ng("over_peak", &path),
            Some(false)
        );

        let (lead, tail, threshold) = harness.state().test_meta_blank_pad(&path).expect("blank");
        assert!((threshold - (-45.0)).abs() < 1e-3, "default -45 dBFS");
        // The single loud sample at index 0 means there is no leading blank;
        // the trailing 50 ms is intact.
        assert!(
            lead <= 1.0,
            "no leading blank past the loud first sample, got {lead}"
        );
        assert!((tail - 50.0).abs() <= 15.0, "tail blank ~50 ms, got {tail}");
        assert_eq!(
            harness.state().test_qa_column_is_ng("blank_pad", &path),
            Some(true),
            "50 ms trailing blank exceeds the 10 ms default"
        );

        // The minimum-duration knob is display-side: raising it past the
        // measured blank flips the verdict without touching the measurement.
        harness.state_mut().test_set_blank_min_ms(500.0);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_meta_blank_pad(&path),
            Some((lead, tail, threshold)),
            "changing the minimum length must not trigger a re-decode"
        );
        assert_eq!(
            harness.state().test_qa_column_is_ng("blank_pad", &path),
            Some(false)
        );

        // The threshold, by contrast, invalidates the measurement and the row
        // re-queues itself with the new value.
        harness.state_mut().test_set_blank_threshold_dbfs(-20.0);
        wait_until(&mut harness, "blank re-measure", |h| {
            h.state()
                .test_meta_blank_pad(&path)
                .map(|(_, _, th)| (th - (-20.0)).abs() < 1e-3)
                .unwrap_or(false)
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The app's exporter clamps to +/-1.0, so no fixture it writes can be over
    /// full scale. Pending gain is the reachable path — and it is the one that
    /// matters, since Over0 deliberately includes it to agree with the adjacent
    /// Peak column.
    #[test]
    fn over_peak_column_accounts_for_pending_gain() {
        let sr = 48_000u32;
        let dir = make_temp_dir("qaover");
        // -6 dBFS peak.
        let ch: Vec<f32> = (0..(sr / 10) as usize)
            .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        let path = dir.join("hot.wav");
        neowaves::wave::export_channels_audio(&[ch].to_vec(), sr, &path).expect("export fixture");

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        harness.state_mut().test_set_qa_columns(true);
        wait_until(&mut harness, "over peak meta", |h| {
            h.state().test_qa_column_is_ng("over_peak", &path).is_some()
        });
        assert_eq!(
            harness.state().test_qa_column_is_ng("over_peak", &path),
            Some(false),
            "-6 dBFS on its own is fine"
        );

        harness
            .state_mut()
            .test_set_pending_gain_db_for_path(&path, 12.0);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_qa_column_is_ng("over_peak", &path),
            Some(true),
            "-6 dBFS plus 12 dB of pending gain clips on export"
        );

        harness
            .state_mut()
            .test_set_pending_gain_db_for_path(&path, 3.0);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_qa_column_is_ng("over_peak", &path),
            Some(false),
            "backing the gain off must clear the warning without a re-decode"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
                panic!("tab ready timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Routing is the only editor tool that changes a tab's channel count, so
    /// the apply and its undo have to keep every count-derived piece of state
    /// consistent.
    #[test]
    fn channel_routing_duplicates_swaps_and_undoes() {
        use neowaves::app::ToolKind;
        let sr = 48_000u32;
        let dir = make_temp_dir("chroute");
        let mono: Vec<f32> = (0..(sr / 10) as usize)
            .map(|i| (i as f32 / sr as f32 * 220.0 * std::f32::consts::TAU).sin() * 0.4)
            .collect();
        let path = dir.join("mono.wav");
        neowaves::wave::export_channels_audio(&[mono].to_vec(), sr, &path).expect("export fixture");

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        assert!(harness.state_mut().test_open_tab_for_path(&path));
        wait_for_tab_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::ChannelRouting));

        let before = harness.state().test_tab_channel_samples().expect("samples");
        assert_eq!(before.len(), 1, "fixture is mono");

        // mono -> stereo duplicate.
        assert!(harness
            .state_mut()
            .test_set_channel_routing(&[vec![0], vec![0]]));
        assert!(harness.state_mut().test_apply_channel_routing());
        harness.run_steps(2);
        let dup = harness.state().test_tab_channel_samples().expect("samples");
        assert_eq!(dup.len(), 2, "channel count must grow to 2");
        assert_eq!(dup[0], before[0]);
        assert_eq!(dup[1], before[0]);

        // Now swap the two channels.
        assert!(harness
            .state_mut()
            .test_set_channel_routing(&[vec![1], vec![0]]));
        assert!(harness.state_mut().test_apply_channel_routing());
        harness.run_steps(2);
        let swapped = harness.state().test_tab_channel_samples().expect("samples");
        assert_eq!(swapped.len(), 2);
        assert_eq!(swapped[0], dup[1]);
        assert_eq!(swapped[1], dup[0]);

        // Undo walks the channel count back down.
        assert!(harness.state_mut().test_editor_undo());
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_tab_channel_samples().map(|c| c.len()),
            Some(2)
        );
        assert!(harness.state_mut().test_editor_undo());
        harness.run_steps(2);
        let restored = harness.state().test_tab_channel_samples().expect("samples");
        assert_eq!(restored.len(), 1, "undo must restore the mono layout");
        assert_eq!(restored[0], before[0]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Press and release must land in separate frames — `Harness::event` queues
    /// for the next frame, so sending both at once gives egui a pointer that
    /// went down and up within one frame and no click is registered.
    fn click_at(harness: &mut Harness<'static, WavesPreviewer>, pos: egui::Pos2) {
        harness.hover_at(pos);
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(2);
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(2);
    }

    /// The patchbay's grab radii are much larger than the drawn pins, and a
    /// cable is cut by clicking the cable itself. Both are driven here through
    /// real pointer events rather than the state hooks.
    #[test]
    fn channel_routing_patchbay_connects_off_centre_and_cuts_cables() {
        use neowaves::app::ToolKind;
        let sr = 48_000u32;
        let dir = make_temp_dir("chpatch");
        let ch: Vec<f32> = (0..(sr / 10) as usize)
            .map(|i| (i as f32 / sr as f32 * 330.0 * std::f32::consts::TAU).sin() * 0.3)
            .collect();
        let path = dir.join("stereo.wav");
        neowaves::wave::export_channels_audio(&[ch.clone(), ch].to_vec(), sr, &path)
            .expect("export fixture");

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        assert!(harness.state_mut().test_open_tab_for_path(&path));
        wait_for_tab_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::ChannelRouting));
        harness.run_steps(3);

        let (in_pins, _out_pins) = WavesPreviewer::test_channel_routing_pins(&harness.ctx)
            .expect("patchbay published its pin geometry");
        assert_eq!(in_pins.len(), 2);

        // Identity to start: out0<-in0, out1<-in1.
        assert_eq!(
            harness.state().test_channel_routing_sources(),
            Some(vec![vec![0], vec![1]])
        );

        // The 1280x720 test window only exposes the first pin row, so every
        // pointer event below is aimed at input 0.
        let pin = in_pins[0];
        assert!(
            pin.y + 24.0 < harness.ctx.content_rect().bottom(),
            "input 0 must be on-screen for the pointer events below"
        );

        // Aim 14 px below the pin centre — outside the 9 px halo that is drawn,
        // inside the 18 px radius that is clickable.
        assert_eq!(harness.state().test_channel_routing_connecting_from(), None);
        click_at(&mut harness, pin + egui::vec2(0.0, 14.0));
        assert_eq!(
            harness.state().test_channel_routing_connecting_from(),
            Some(0),
            "a click 14 px off the pin centre must still grab the pin"
        );
        // A click clear of every pin and cable abandons it. 22 px below the
        // row keeps it outside the 11 px cable radius while staying inside
        // both the canvas and the test window.
        click_at(&mut harness, pin + egui::vec2(120.0, 22.0));
        assert_eq!(harness.state().test_channel_routing_connecting_from(), None);

        // Cut a cable by clicking the cable. Sampled just right of the input
        // pin, where the bezier still runs flat: far enough out to be past the
        // pin's grab radius, and on-screen even in the narrow test window.
        let cut_at = pin + egui::vec2(30.0, 0.0);
        assert!(
            (cut_at - pin).length() > 18.0,
            "cut point must sit outside the pin grab radius"
        );
        click_at(&mut harness, cut_at);
        assert_eq!(
            harness.state().test_channel_routing_sources(),
            Some(vec![vec![], vec![1]]),
            "clicking a cable must cut exactly that cable"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Clicking a QA header must bring the NG rows to the top, with rows whose
    /// metadata hasn't resolved yet sorted below the OK ones.
    #[test]
    fn qa_column_sorts_ng_rows_to_the_top() {
        use neowaves::app::{SortDir, SortKey};
        let sr = 48_000u32;
        let dir = make_temp_dir("qasort");
        // "ok.wav" starts and ends on silence; "ng.wav" starts loud.
        let tone: Vec<f32> = (0..(sr / 5) as usize)
            .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        let mut ok = vec![0.0f32; 64];
        ok.extend(tone.iter().copied());
        ok.extend(vec![0.0f32; 64]);
        let mut ng = ok.clone();
        ng[0] = 0.5;
        neowaves::wave::export_channels_audio(&[ok].to_vec(), sr, &dir.join("a_ok.wav"))
            .expect("export ok fixture");
        neowaves::wave::export_channels_audio(&[ng].to_vec(), sr, &dir.join("b_ng.wav"))
            .expect("export ng fixture");

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 2);
        harness.state_mut().test_set_qa_columns(true);
        let ng_path = dir.join("b_ng.wav");
        let ok_path = dir.join("a_ok.wav");
        wait_until(&mut harness, "qa meta", |h| {
            h.state().test_qa_column_is_ng("edge_zero", &ng_path) == Some(true)
                && h.state().test_qa_column_is_ng("edge_zero", &ok_path) == Some(false)
        });

        // Ascending would put OK first; the header's first click is Desc
        // precisely so the problems surface immediately.
        harness
            .state_mut()
            .test_set_sort(SortKey::EdgeZero, SortDir::Desc);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_row_path(0),
            Some(ng_path.clone()),
            "descending Edge0 must list the NG file first"
        );

        harness
            .state_mut()
            .test_set_sort(SortKey::EdgeZero, SortDir::Asc);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_row_path(0),
            Some(ok_path),
            "ascending Edge0 must list the passing file first"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `h:mm:ss` rendering itself is covered by the unit tests on
    /// `format_duration_hms`; this checks the latch that decides which format
    /// the list uses, without needing an hour-long fixture.
    #[test]
    fn length_latch_tracks_duration_and_resets_on_reload() {
        let sr = 8_000u32;
        let dir = make_temp_dir("qalen");
        let empty_dir = make_temp_dir("qalen_empty");
        let one_sec: Vec<f32> = vec![0.1; sr as usize];
        neowaves::wave::export_channels_audio(&[one_sec].to_vec(), sr, &dir.join("short.wav"))
            .expect("export fixture");

        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir.clone());
        cfg.open_first = false;
        let mut harness = harness_with_startup(cfg);
        wait_until(&mut harness, "scan", |h| h.state().files.len() >= 1);
        wait_until(&mut harness, "duration metadata", |h| {
            h.state().test_list_max_duration_secs() > 0.5
        });
        assert!(
            !harness.state().test_list_length_uses_hours(),
            "a one-second list keeps the m:SS format"
        );

        harness
            .state_mut()
            .test_start_folder_load(empty_dir.clone());
        wait_until(&mut harness, "latch reset", |h| {
            h.state().test_list_max_duration_secs() == 0.0
        });

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty_dir);
    }
}
