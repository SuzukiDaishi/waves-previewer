#[cfg(feature = "kittest")]
mod small_fix_regressions {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui::{Key, Modifiers};
    use egui_kittest::{kittest::Queryable, Harness};
    use hound::{SampleFormat, WavSpec, WavWriter};
    use neowaves::app::ToolKind;
    use neowaves::kittest::{harness_default, harness_with_startup};
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

    fn wait_for_audio_samples(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().test_audio_has_samples() {
                return;
            }
            if start.elapsed() > Duration::from_secs(8) {
                panic!("audio sample load timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_preview_tool(
        harness: &mut Harness<'static, WavesPreviewer>,
        tool: ToolKind,
        require_overlay: bool,
    ) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let tool_ok = harness.state().test_preview_audio_tool() == Some(tool);
            let overlay_ok = !require_overlay || harness.state().test_preview_overlay_tool() == Some(tool);
            if tool_ok && overlay_ok {
                return;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!(
                    "preview timeout for {:?}: audio={:?} overlay={:?}",
                    tool,
                    harness.state().test_preview_audio_tool(),
                    harness.state().test_preview_overlay_tool()
                );
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

    fn editor_canvas_pos_at_frac(
        harness: &Harness<'static, WavesPreviewer>,
        frac: f32,
    ) -> egui::Pos2 {
        const EDITOR_AMPLITUDE_NAV_RESERVED_W: f32 = 30.0;
        let inspector_rect = harness.get_by_label("Inspector").rect();
        let tab_idx = harness.state().active_tab.expect("active tab");
        let wave_w = harness.state().tabs[tab_idx].last_wave_w.max(64.0);
        let wave_right = (inspector_rect.left() - 4.0 - EDITOR_AMPLITUDE_NAV_RESERVED_W).max(48.0);
        let wave_left = (wave_right - wave_w + 8.0).max(8.0);
        let width = (wave_right - wave_left).max(64.0);
        egui::pos2(
            wave_left + width * frac.clamp(0.0, 1.0),
            inspector_rect.center().y,
        )
    }

    fn editor_shift_click_at_frac(harness: &mut Harness<'static, WavesPreviewer>, frac: f32) {
        let pos = editor_canvas_pos_at_frac(harness, frac);
        harness.hover_at(pos);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.run_steps(2);
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
    fn audio_info_wav_reports_total_frames() {
        let dir = make_temp_dir("audio_info_total_frames");
        let wav = dir.join("frames.wav");
        write_wav_32_float(&wav, 48_000, 2.0);

        let info = neowaves::audio_io::read_audio_info(&wav).expect("read audio info");
        assert_eq!(info.sample_rate, 48_000);
        assert_eq!(info.total_frames, Some(96_000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wav_proxy_preview_preserves_duration_with_reduced_frames() {
        let dir = make_temp_dir("wav_proxy_preview");
        let wav = dir.join("proxy.wav");
        write_wav_32_float(&wav, 48_000, 90.0);

        let proxy = neowaves::audio_io::build_wav_proxy_preview(&wav, 100_000)
            .expect("build wav proxy")
            .expect("wav proxy available");
        let proxy_frames = proxy.channels.first().map(|ch| ch.len()).unwrap_or(0);
        assert_eq!(proxy.total_source_frames, 4_320_000);
        assert_eq!(
            proxy.channels.len(),
            2,
            "stereo proxy should preserve channels"
        );
        assert_eq!(
            proxy.sample_rate, 48_000,
            "proxy metadata should keep the source sample rate"
        );
        assert!(
            proxy_frames < proxy.total_source_frames,
            "proxy should reduce frame count: proxy={proxy_frames} source={}",
            proxy.total_source_frames
        );
        assert!(
            proxy_frames > 0,
            "proxy should still contain waveform samples"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wav_streaming_decode_emits_progressive_chunks() {
        let dir = make_temp_dir("wav_streaming_chunks");
        let wav = dir.join("stream.wav");
        write_wav_32_float(&wav, 48_000, 12.0);

        let mut events: Vec<(usize, bool)> = Vec::new();
        neowaves::audio_io::decode_audio_multi_streaming_chunks(
            &wav,
            0.25,
            || false,
            |chunk, _sr, decoded_frames, is_final| {
                assert!(!chunk.is_empty(), "streaming chunk should include channels");
                assert!(
                    chunk[0].len() > 0,
                    "streaming chunk should include decoded samples"
                );
                events.push((decoded_frames, is_final));
                true
            },
        )
        .expect("streaming decode");

        assert!(
            events.len() > 2,
            "expected multiple progressive chunk events, got {}",
            events.len()
        );
        for pair in events.windows(2) {
            assert!(
                pair[1].0 > pair[0].0,
                "decoded frame count should increase monotonically: {:?}",
                pair
            );
        }
        assert!(events
            .last()
            .map(|(_, is_final)| *is_final)
            .unwrap_or(false));

        let _ = std::fs::remove_dir_all(&dir);
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
        let first_asc = harness.state().test_row_path(0).expect("first row asc");
        let last_asc = harness
            .state()
            .test_row_path(files_len - 1)
            .expect("last row asc");
        assert_eq!(first_asc, wav_lo);
        assert_eq!(last_asc, bad_mp3);

        harness.state_mut().test_sort_sample_rate_desc();
        harness.run_steps(2);
        let first_desc = harness.state().test_row_path(0).expect("first row desc");
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
        let immediate = harness
            .state_mut()
            .test_force_load_selected_list_preview_for_play();
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

    #[test]
    fn tab_switch_during_heavy_processing_keeps_target_audio() {
        let dir = make_temp_dir("tab_switch_heavy");
        let long = dir.join("a_long.wav");
        let short = dir.join("b_short.wav");
        write_wav_32_float(&long, 48_000, 8.0);
        write_wav_32_float(&short, 48_000, 0.7);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);

        assert!(harness.state_mut().test_open_tab_for_path(&long));
        wait_for_tab_ready(&mut harness);
        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(long.as_path())
        );

        assert!(harness.state_mut().test_open_tab_for_path(&short));
        wait_for_tab_ready(&mut harness);
        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(short.as_path())
        );

        harness.state_mut().test_set_mode_time_stretch();
        harness.state_mut().test_set_playback_rate(0.5);

        // Start heavy processing on long tab, then immediately switch back to short tab.
        assert!(harness.state_mut().test_open_tab_for_path(&long));
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(10));
        assert!(harness.state_mut().test_open_tab_for_path(&short));

        let mut ready = false;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(12) {
            harness.run_steps(1);
            let active = harness.state().test_active_tab_path();
            let playing = harness.state().test_playing_path().cloned();
            let len = harness.state().test_audio_buffer_len();
            if active.as_deref() == Some(short.as_path())
                && playing.as_deref() == Some(short.as_path())
                && len > 0
                && len < 200_000
            {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        assert!(
            ready,
            "target tab did not stabilize after heavy-processing switch (active={:?} playing={:?} len={})",
            harness.state().test_active_tab_path(),
            harness.state().test_playing_path(),
            harness.state().test_audio_buffer_len()
        );

        // Keep running to ensure no late stale result rewinds playback to the long tab.
        let soak_start = Instant::now();
        while soak_start.elapsed() < Duration::from_secs(2) {
            harness.run_steps(1);
            assert_eq!(
                harness.state().test_active_tab_path().as_deref(),
                Some(short.as_path())
            );
            assert_eq!(
                harness.state().test_playing_path().map(|p| p.as_path()),
                Some(short.as_path())
            );
            assert!(
                harness.state().test_audio_buffer_len() < 200_000,
                "audio buffer length looks like long-tab content after switch"
            );
            std::thread::sleep(Duration::from_millis(15));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_non_source_tab_keeps_playback_running() {
        let dir = make_temp_dir("close_non_source_tab");
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_wav_32_float(&a, 48_000, 3.0);
        write_wav_32_float(&b, 48_000, 3.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);

        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&b));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().audio.play();
        harness.run_steps(2);
        assert!(
            harness.state().test_audio_is_playing(),
            "playback should be active"
        );

        assert!(harness.state_mut().test_close_tab_for_path(&b));
        harness.run_steps(2);

        assert!(
            harness.state().test_audio_is_playing(),
            "closing a non-source tab should not stop playback"
        );
        assert_eq!(
            harness.state().test_playing_path().map(|p| p.as_path()),
            Some(a.as_path())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_source_tab_stops_playback() {
        let dir = make_temp_dir("close_source_tab");
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_wav_32_float(&a, 48_000, 2.0);
        write_wav_32_float(&b, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);

        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&b));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().audio.play();
        harness.run_steps(2);
        assert!(
            harness.state().test_audio_is_playing(),
            "playback should start"
        );
        assert_eq!(
            harness.state().test_playing_path().map(|p| p.as_path()),
            Some(a.as_path())
        );

        assert!(harness.state_mut().test_close_tab_for_path(&a));
        harness.run_steps(3);

        assert!(
            !harness.state().test_audio_is_playing(),
            "closing current source tab should stop playback"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secondary_drag_anchor_is_not_replaced_by_playhead() {
        let dir = make_temp_dir("right_drag_shift");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state().audio.seek_to_sample(4_000);
        assert!(harness
            .state_mut()
            .test_simulate_right_drag_from_frac(0.80, true, 0.92));
        harness.run_steps(1);

        let anchor = harness
            .state()
            .test_tab_selection_anchor()
            .expect("selection anchor");
        let selection = harness.state().test_tab_selection().expect("selection");
        assert!(selection.1 > selection.0);
        assert!(
            anchor > 20_000,
            "secondary selection anchor should come from button-down sample, not playhead: {anchor}"
        );
        assert_eq!(selection.0, anchor);
        assert_eq!(harness.state().test_tab_right_drag_mode(), None);
        assert_eq!(harness.state().test_audio_play_pos(), 4_000);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shift_click_after_shift_arrow_uses_saved_anchor() {
        let dir = make_temp_dir("shift_click_anchor");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_audio_seek_to_sample(4_000);
        harness.run_steps(1);
        harness.key_press_modifiers(Modifiers::SHIFT, Key::ArrowRight);
        harness.run_steps(2);
        let anchor = harness
            .state()
            .test_tab_selection_anchor()
            .expect("selection anchor");
        editor_shift_click_at_frac(&mut harness, 0.75);
        let selection = harness.state().test_tab_selection().expect("selection");
        assert_eq!(selection.0, anchor);
        assert!(selection.1 > selection.0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stopped_meter_does_not_show_stale_value() {
        let dir = make_temp_dir("meter_stale");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_audio_seek_to_sample(10_000);
        harness.run_steps(1);
        harness.key_press(Key::Space);
        harness.run_steps(5);
        assert!(
            harness.state().test_meter_db() > -79.9,
            "playing meter should report signal"
        );
        harness.key_press(Key::Space);
        harness.run_steps(5);
        assert!(
            harness.state().test_meter_db() <= -79.9,
            "stopped meter should reset to -inf-equivalent"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_playback_meter_reports_signal() {
        let dir = make_temp_dir("meter_list_playback");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_path(&src));
        harness.run_steps(2);

        harness.key_press(Key::Space);
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().test_audio_is_playing() && harness.state().test_meter_db() > -79.9 {
                break;
            }
            if start.elapsed() > Duration::from_secs(8) {
                panic!(
                    "list playback meter did not report signal: playing={} meter={}",
                    harness.state().test_audio_is_playing(),
                    harness.state().test_meter_db()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        harness.key_press(Key::Space);
        harness.run_steps(5);
        assert!(
            harness.state().test_meter_db() <= -79.9,
            "stopped list playback should reset meter to -inf-equivalent"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn right_drag_seek_keeps_existing_selection() {
        let dir = make_temp_dir("right_drag_seek");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_set_selection_frac(0.10, 0.25));

        let before_sel = harness.state().test_tab_selection();
        let before_pos = harness.state().test_audio_play_pos();
        assert!(harness.state_mut().test_simulate_right_drag(false, 0.75));
        harness.run_steps(1);

        let after_pos = harness.state().test_audio_play_pos();
        assert_ne!(after_pos, before_pos);
        assert_eq!(harness.state().test_tab_selection(), before_sel);
        assert_eq!(harness.state().test_tab_right_drag_mode(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_mode_rate_stays_stable_across_workspace_switch() {
        let dir = make_temp_dir("speed_rate_switch");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.37);
        harness.state_mut().test_refresh_playback_rate();
        harness.run_steps(2);
        let rate_before = harness.state().test_audio_rate();
        let expected_rate_before = harness.state().test_playback_rate()
            * (harness.state().test_playback_transport_sr() as f32
                / harness.state().test_audio_out_sample_rate() as f32);
        assert_eq!(
            harness.state().test_playback_transport_name(),
            "ExactStreamWav",
            "pristine speed-mode editor playback should keep exact-stream transport active"
        );
        assert!(
            (rate_before - expected_rate_before).abs() < 1.0e-6,
            "callback rate should follow exact-stream ratio: expected={expected_rate_before} actual={rate_before}"
        );

        harness.state_mut().test_switch_to_list();
        harness.run_steps(2);
        let rate_in_list = harness.state().test_audio_rate();
        assert!(
            (rate_in_list - rate_before).abs() < 1e-6,
            "speed rate changed after switching to list: before={rate_before} after={rate_in_list}"
        );

        assert!(harness.state_mut().test_open_tab_for_path(&src));
        harness.run_steps(4);
        let rate_after = harness.state().test_audio_rate();
        assert!(
            (rate_after - rate_before).abs() < 1e-6,
            "speed rate changed after tab/workspace switch: before={rate_before} after={rate_after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_restore_keeps_rate_for_resampled_editor_buffer() {
        let dir = make_temp_dir("preview_restore_rate");
        let src = dir.join("src_44100.wav");
        write_wav_32_float(&src, 44_100, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_path(&src));
        assert!(harness
            .state_mut()
            .test_set_selected_sample_rate_override(48_000));
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.25);
        harness.state_mut().test_refresh_playback_rate();
        harness.run_steps(2);
        let rate_before = harness.state().test_audio_rate();
        assert!((rate_before - 1.0).abs() < 1.0e-6);

        assert!(harness.state_mut().test_force_preview_restore_active_tab());
        harness.run_steps(2);
        let rate_after = harness.state().test_audio_rate();
        assert!(
            (rate_after - rate_before).abs() < 1.0e-6,
            "preview restore should keep output-buffer playback rate stable: before={rate_before} after={rate_after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_restore_allows_spec_waveform_overlay() {
        let dir = make_temp_dir("preview_spec_overlay");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_tool_gain_db(4.0));
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::app::ViewMode::Spectrogram));
        assert!(harness.state_mut().test_set_waveform_overlay(true));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        wait_for_preview_tool(&mut harness, ToolKind::Gain, true);

        assert_eq!(harness.state().test_preview_overlay_tool(), Some(ToolKind::Gain));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_preview_busy_does_not_disable_gain_preview() {
        let dir = make_temp_dir("preview_busy_scope");
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_wav_32_float(&a, 48_000, 2.5);
        write_wav_32_float(&b, 48_000, 2.5);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::PitchShift));
        assert!(harness.state_mut().test_set_tool_pitch_semitones(2.0));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        harness.run_steps(2);

        assert!(harness.state_mut().test_open_tab_for_path(&b));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_tool_gain_db(3.0));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        wait_for_preview_tool(&mut harness, ToolKind::Gain, true);

        assert_eq!(harness.state().test_preview_audio_tool(), Some(ToolKind::Gain));
        assert_eq!(harness.state().test_preview_overlay_tool(), Some(ToolKind::Gain));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_sidecar_roundtrip_keeps_editor_rate_stable() {
        let dir = make_temp_dir("session_rate_roundtrip");
        let src = dir.join("src_44100.wav");
        let sess = dir.join("roundtrip_rate.nwsess");
        write_wav_32_float(&src, 44_100, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_apply_gain(0.10, 0.30, 3.0));
        harness.run_steps(2);
        assert!(harness.state_mut().test_save_session_to(&sess));

        assert!(harness.state_mut().test_open_session_from(&sess));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.11);
        harness.state_mut().test_refresh_playback_rate();
        harness.run_steps(2);
        assert!(harness.state_mut().test_force_preview_restore_active_tab());
        harness.run_steps(2);

        let rate_after = harness.state().test_audio_rate();
        assert!(
            (rate_after - 1.0).abs() < 1.0e-6,
            "session sidecar reopen should keep callback rate fixed at unity: rate={rate_after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_preview_rate_uses_source_buffer_sample_rate() {
        let dir = make_temp_dir("list_preview_rate");
        let src = dir.join("src_44100.wav");
        write_wav_32_float(&src, 44_100, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.20);
        assert!(harness.state_mut().test_select_path(&src));
        wait_for_audio_samples(&mut harness);

        let rate_before = harness.state().test_audio_rate();
        assert_eq!(
            harness.state().test_playback_transport_name(),
            "Buffer",
            "passive list selection should keep cached buffer transport"
        );
        assert!(
            !harness.state().test_audio_is_streaming_wav(&src),
            "passive list selection should not activate exact streaming transport"
        );
        assert!(
            (rate_before - 1.0).abs() < 1.0e-6,
            "list preview callback should stay at unity: rate={rate_before}"
        );
        let rendered_len = harness.state().test_audio_buffer_len();
        assert!(
            rendered_len.abs_diff(80_000) <= 4,
            "speed preview should be fully rendered before playback: len={rendered_len}"
        );

        let _ = harness
            .state_mut()
            .test_force_load_selected_list_preview_for_play();
        wait_for_audio_samples(&mut harness);

        let rate_after = harness.state().test_audio_rate();
        let expected_rate_after = harness.state().test_playback_rate()
            * (harness.state().test_playback_transport_sr() as f32
                / harness.state().test_audio_out_sample_rate() as f32);
        assert_eq!(
            harness.state().test_playback_transport_name(),
            "ExactStreamWav",
            "explicit list play should switch eligible pristine WAV to exact-stream transport"
        );
        assert!(
            harness.state().test_audio_is_streaming_wav(&src),
            "explicit list play should activate exact streaming transport"
        );
        assert!(
            (rate_after - expected_rate_after).abs() < 1.0e-6,
            "list play callback rate should follow exact-stream ratio: expected={expected_rate_after} actual={rate_after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn master_volume_stays_realtime_without_rebuilding_prepared_buffer() {
        let mut harness = harness_default();
        harness
            .state_mut()
            .test_seed_prepared_audio_buffer(vec![0.0, 0.25, -0.25, 0.1, -0.1]);

        let before_ptr = harness.state().test_audio_buffer_ptr();
        let before_sample = harness
            .state()
            .test_audio_buffer_sample(0, 2)
            .expect("prepared buffer sample");

        harness.state_mut().test_set_volume_db(-12.0);
        harness.run_steps(2);

        let after_ptr = harness.state().test_audio_buffer_ptr();
        let after_sample = harness
            .state()
            .test_audio_buffer_sample(0, 2)
            .expect("prepared buffer sample after volume change");
        let expected_linear = 10.0f32.powf(-12.0 / 20.0);
        let actual_linear = harness.state().test_audio_output_volume_linear();

        assert_eq!(
            after_ptr, before_ptr,
            "master volume changes should not replace the prepared buffer"
        );
        assert!(
            (after_sample - before_sample).abs() < 1.0e-6,
            "master volume should remain outside offline buffer rendering"
        );
        assert!(
            (actual_linear - expected_linear).abs() < 1.0e-6,
            "callback master volume mismatch: expected={expected_linear} actual={actual_linear}"
        );
    }

    #[test]
    fn editor_stream_discards_stale_processing_result_without_rate_jump() {
        let dir = make_temp_dir("editor_processing_stale_mode");
        let src = dir.join("src_44100.wav");
        write_wav_32_float(&src, 44_100, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.0);
        harness.state_mut().test_refresh_playback_rate();
        harness.state_mut().test_request_workspace_play_toggle();
        harness.run_steps(2);
        assert!(harness.state().test_audio_is_playing());

        let rate_before = harness.state().test_audio_rate();
        let expected_rate_before = harness.state().test_playback_rate()
            * (harness.state().test_playback_transport_sr() as f32
                / harness.state().test_audio_out_sample_rate() as f32);
        assert_eq!(
            harness.state().test_playback_transport_name(),
            "ExactStreamWav",
            "eligible pristine editor playback should stay on exact-stream transport"
        );
        assert!(
            (rate_before - expected_rate_before).abs() < 1.0e-6,
            "exact-stream callback rate mismatch before stale result: expected={expected_rate_before} actual={rate_before}"
        );
        harness.state_mut().test_inject_processing_result(
            &src,
            true,
            true,
            neowaves::app::RateMode::PitchShift,
            neowaves::app::RateMode::PitchShift,
            7,
            7,
        );
        harness.run_steps(2);

        let rate_after = harness.state().test_audio_rate();
        assert!(
            (rate_after - rate_before).abs() < 1.0e-6,
            "stale editor processing result changed callback rate: before={rate_before} after={rate_after}"
        );
        assert!(
            harness.state().test_audio_is_playing(),
            "stale processing result should not stop current editor playback"
        );
        assert!(
            !harness.state().test_processing_active(),
            "stale processing state should be cleared after discard"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stream_to_buffer_rebuild_preserves_playback_timebase() {
        let dir = make_temp_dir("editor_stream_buffer_timebase");
        let src = dir.join("src_44100.wav");
        write_wav_32_float(&src, 44_100, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.0);
        harness.state_mut().test_refresh_playback_rate();

        harness.state_mut().test_playback_seek_to_source_time(1.0);
        harness.run_steps(1);
        let time_before = harness
            .state()
            .test_playback_current_source_time_sec()
            .expect("source time before rebuild");
        assert_eq!(
            harness.state().test_playback_transport_name(),
            "ExactStreamWav",
            "pristine editor playback should start on exact-stream transport"
        );

        assert!(harness
            .state_mut()
            .test_set_selected_sample_rate_override(48_000));
        harness.state_mut().test_rebuild_current_buffer_with_mode();
        harness.run_steps(2);

        let time_after = harness
            .state()
            .test_playback_current_source_time_sec()
            .expect("source time after rebuild");
        assert!(
            (time_after - time_before).abs() < 0.01,
            "stream-to-buffer fallback should preserve source time: before={time_before:.6} after={time_after:.6}"
        );
        assert_eq!(
            harness.state().test_playback_transport_name(),
            "Buffer",
            "sample-rate override should force buffer transport"
        );
        let rate_after = harness.state().test_audio_rate();
        assert!(
            (rate_after - 1.0).abs() < 1.0e-6,
            "buffer transport should run at output-sr rate after rebuild: rate={rate_after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_gain_disables_exact_stream_transport() {
        let dir = make_temp_dir("editor_exact_stream_pending_gain");
        let src = dir.join("src_44100.wav");
        write_wav_32_float(&src, 44_100, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.0);
        harness.state_mut().test_refresh_playback_rate();
        harness.run_steps(2);

        assert_eq!(
            harness.state().test_playback_transport_name(),
            "ExactStreamWav",
            "pristine editor playback should begin on exact-stream transport"
        );
        assert!(harness
            .state_mut()
            .test_set_pending_gain_db_for_current_source(3.0));
        harness.run_steps(2);

        assert_eq!(
            harness.state().test_playback_transport_name(),
            "Buffer",
            "per-file gain should force fallback to rendered buffer transport"
        );
        assert!(
            !harness.state().test_audio_is_streaming_wav(&src),
            "per-file gain should deactivate exact streaming transport"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_processing_result_job_id_mismatch_is_discarded() {
        let dir = make_temp_dir("editor_processing_job_mismatch");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_set_mode_pitch_shift();
        harness.state_mut().test_request_workspace_play_toggle();
        harness.run_steps(2);
        assert!(harness.state().test_audio_is_playing());

        let rate_before = harness.state().test_audio_rate();
        harness.state_mut().test_inject_processing_result(
            &src,
            true,
            true,
            neowaves::app::RateMode::PitchShift,
            neowaves::app::RateMode::PitchShift,
            11,
            12,
        );
        harness.run_steps(2);

        assert!(
            harness.state().test_audio_is_playing(),
            "job-id-mismatched processing result should not stop playback"
        );
        let rate_after = harness.state().test_audio_rate();
        assert!(
            (rate_after - rate_before).abs() < 1.0e-6,
            "job-id-mismatched processing result changed rate: before={rate_before} after={rate_after}"
        );
        assert!(
            !harness.state().test_processing_active(),
            "job-id-mismatched processing state should be cleared after discard"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_processing_result_does_not_leak_into_active_editor_tab() {
        let dir = make_temp_dir("editor_processing_target_mismatch");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_set_mode_pitch_shift();
        harness.state_mut().test_request_workspace_play_toggle();
        harness.run_steps(2);
        assert!(harness.state().test_audio_is_playing());

        harness.state_mut().test_inject_processing_result(
            &src,
            false,
            false,
            neowaves::app::RateMode::PitchShift,
            neowaves::app::RateMode::PitchShift,
            21,
            21,
        );
        harness.run_steps(2);

        assert!(
            harness.state().test_audio_is_playing(),
            "list-target processing result should not stop editor playback"
        );
        assert!(
            !harness.state().test_processing_active(),
            "target-mismatched processing state should be cleared after discard"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_mode_does_not_spawn_heavy_processing_for_editor_tab() {
        let dir = make_temp_dir("editor_processing_speed_noop");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        harness.state_mut().test_set_mode_speed();
        assert!(
            !harness
                .state_mut()
                .test_spawn_heavy_processing_from_active_tab(),
            "unity speed should not create heavy processing jobs for editor tabs"
        );
        assert!(
            !harness.state().test_processing_active(),
            "Speed mode should leave processing state empty"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_reopen_rebuilds_waveform_cache_without_crash() {
        let dir = make_temp_dir("waveform_cache_reopen");
        let src = dir.join("src.wav");
        let sess = dir.join("waveform_cache_roundtrip.nwsess");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);
        assert!(
            harness.state().test_active_tab_waveform_pyramid_ready(),
            "initial editor tab should build waveform pyramid"
        );
        assert!(harness.state_mut().test_save_session_to(&sess));

        assert!(harness.state_mut().test_open_session_from(&sess));
        wait_for_tab_ready(&mut harness);
        assert!(
            harness.state().test_active_tab_waveform_pyramid_ready(),
            "session reopen should rebuild waveform pyramid"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_wav_loading_progress_advances_and_waveform_updates_before_final() {
        let dir = make_temp_dir("editor_wav_progress");
        let wav = dir.join("long.wav");
        write_wav_32_float(&wav, 48_000, 90.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_path(&wav));
        assert!(harness.state_mut().test_open_tab_for_path(&wav));

        let started = Instant::now();
        let mut saw_loading_waveform = false;
        let mut saw_nonflat_loading_waveform = false;
        let mut saw_whole_timeline_while_loading = false;
        let mut saw_streaming_while_loading = false;
        let mut saw_exact_audio_ready_while_loading = false;
        let mut saw_playing_while_loading = false;
        let mut saw_final_ready = false;
        let mut max_progress = 0.0f32;
        loop {
            harness.run_steps(1);
            if harness.state().test_tab_loading() {
                if harness.state().test_active_tab_loading_waveform_ready() {
                    saw_loading_waveform = true;
                }
                if harness.state().test_active_tab_loading_waveform_nonflat() {
                    saw_nonflat_loading_waveform = true;
                }
                if harness.state().test_audio_is_streaming_wav(&wav) {
                    saw_streaming_while_loading = true;
                }
                if harness.state().test_active_editor_exact_audio_ready() {
                    saw_exact_audio_ready_while_loading = true;
                }
                if !harness.state().test_audio_is_playing() {
                    harness.state_mut().test_request_workspace_play_toggle();
                }
                if harness.state().test_audio_is_playing() {
                    saw_playing_while_loading = true;
                }
                let progress = harness.state().test_editor_decode_progress().unwrap_or(0.0);
                max_progress = max_progress.max(progress);
                let visual_len = harness.state().test_active_tab_samples_len_visual();
                let audio_len = harness.state().test_tab_samples_len();
                if visual_len > 0 && visual_len >= audio_len {
                    saw_whole_timeline_while_loading = true;
                }
            } else if harness.state().test_active_editor_exact_audio_ready() {
                saw_final_ready = true;
                break;
            }
            if started.elapsed() > Duration::from_secs(20) {
                panic!(
                    "wav editor loading timeout: waveform={saw_loading_waveform} whole_timeline={saw_whole_timeline_while_loading} streaming={saw_streaming_while_loading} exact_ready_loading={saw_exact_audio_ready_while_loading} playing_loading={saw_playing_while_loading} final_ready={saw_final_ready} max_progress={max_progress:.3}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert!(
            saw_loading_waveform,
            "loading waveform overview should update before final"
        );
        assert!(
            saw_nonflat_loading_waveform,
            "loading waveform overview should become non-flat before final"
        );
        assert!(
            saw_whole_timeline_while_loading,
            "loading state should keep a usable whole-file timeline while detail decode is still pending"
        );
        assert!(
            saw_streaming_while_loading,
            "eligible pristine WAV loading should activate exact streaming transport immediately"
        );
        assert!(
            saw_exact_audio_ready_while_loading,
            "exact-stream activation should make editor playback ready before final decode"
        );
        assert!(
            saw_playing_while_loading,
            "editor playback should be allowed during loading when exact-stream is active"
        );
        assert!(
            saw_final_ready,
            "full decode should still finish before playback becomes available"
        );
        assert!(
            max_progress > 0.20,
            "loading progress should move past initial preview region: {max_progress:.3}"
        );

        assert!(
            harness.state().test_audio_is_playing(),
            "playback started during loading should remain active after final decode"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_wav_finalizing_exact_audio_keeps_stream_rate_while_playing() {
        let dir = make_temp_dir("editor_wav_finalizing_rate");
        let wav = dir.join("long_44100.wav");
        write_wav_32_float(&wav, 44_100, 24.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_path(&wav));
        assert!(harness.state_mut().test_open_tab_for_path(&wav));

        let started = Instant::now();
        let mut saw_playing_while_loading = false;
        let mut rate_while_loading = None;
        loop {
            harness.run_steps(1);
            if harness.state().test_tab_loading() {
                if !harness.state().test_audio_is_playing() {
                    harness.state_mut().test_request_workspace_play_toggle();
                }
                saw_playing_while_loading |= harness.state().test_audio_is_playing();
                if harness.state().test_audio_is_playing() {
                    rate_while_loading = Some(harness.state().test_audio_rate());
                }
            } else if harness.state().test_active_editor_exact_audio_ready() {
                break;
            }
            if started.elapsed() > Duration::from_secs(20) {
                panic!(
                    "wav offline finalize timeout: loading={} exact_ready={} playing={} streaming={}",
                    harness.state().test_tab_loading(),
                    harness.state().test_active_editor_exact_audio_ready(),
                    harness.state().test_audio_is_playing(),
                    harness.state().test_audio_is_streaming_wav(&wav),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert!(
            saw_playing_while_loading,
            "exact-stream loading should allow playback before final decode"
        );
        let rate_before_final = rate_while_loading.expect("rate while loading");
        let rate_after_final = harness.state().test_audio_rate();
        assert!(
            harness.state().test_audio_is_streaming_wav(&wav),
            "pristine WAV should remain on exact-stream transport after final decode"
        );
        assert_eq!(
            harness.state().test_playback_transport_name(),
            "ExactStreamWav",
            "final decode should not swap live playback off exact-stream transport"
        );
        assert!(
            (rate_after_final - rate_before_final).abs() < 1.0e-6,
            "finalizing exact audio should not change callback rate mid-play: before={rate_before_final} after={rate_after_final}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_open_seeds_loading_overview_before_decode_finishes() {
        let dir = make_temp_dir("editor_loading_overview_seed");
        let wav = dir.join("seed.wav");
        write_wav_32_float(&wav, 48_000, 12.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_path(&wav));
        assert!(harness.state_mut().test_open_tab_for_path(&wav));
        harness.run_steps(1);

        assert!(
            harness.state().test_tab_loading(),
            "tab should enter loading state"
        );
        assert!(
            harness.state().test_active_tab_loading_waveform_ready(),
            "loading overview should be available immediately after open"
        );
        let start = Instant::now();
        while harness.state().test_active_tab_samples_len_visual() == 0
            && start.elapsed() < Duration::from_secs(2)
        {
            harness.run_steps(1);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            harness.state().test_active_tab_samples_len_visual() > 0,
            "visual length should become available during early loading"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spectrogram_hop_roundtrip_via_session_keeps_derived_overlap() {
        let dir = make_temp_dir("spectro_hop_roundtrip");
        let src = dir.join("src.wav");
        let sess = dir.join("hop_roundtrip.nwsess");
        write_wav_32_float(&src, 48_000, 1.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_spectro_hop_size(128);
        harness.run_steps(2);
        assert_eq!(harness.state().test_spectro_hop_size(), 128);
        assert!(
            (harness.state().test_spectro_overlap() - 0.9375).abs() < 1.0e-4,
            "overlap should be derived from hop/fft"
        );

        assert!(harness.state_mut().test_save_session_to(&sess));
        harness.state_mut().test_set_spectro_hop_size(512);
        harness.run_steps(1);
        assert_eq!(harness.state().test_spectro_hop_size(), 512);

        assert!(harness.state_mut().test_open_session_from(&sess));
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(8) {
            harness.run_steps(1);
            if harness.state().test_spectro_hop_size() == 128 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        assert_eq!(harness.state().test_spectro_hop_size(), 128);
        assert!(
            (harness.state().test_spectro_overlap() - 0.9375).abs() < 1.0e-4,
            "restored overlap should remain hop-derived"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn music_stem_preview_gain_clamps_to_plus_24_db() {
        let dir = make_temp_dir("stem_preview_gain_clamp");
        let src = dir.join("src.wav");
        write_wav_32_float(&src, 48_000, 2.0);

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&src));
        wait_for_tab_ready(&mut harness);

        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::MusicAnalyze));
        assert!(harness
            .state_mut()
            .test_set_music_analysis_result_mock(true));
        assert!(harness
            .state_mut()
            .test_set_music_preview_gains_db(80.0, 30.0, 42.0, 100.0));
        harness.run_steps(3);

        let gains = harness
            .state()
            .test_music_preview_gains_db()
            .expect("music preview gains");
        assert!(gains.0 <= 24.0 && gains.0 >= -80.0);
        assert!(gains.1 <= 24.0 && gains.1 >= -80.0);
        assert!(gains.2 <= 24.0 && gains.2 >= -80.0);
        assert!(gains.3 <= 24.0 && gains.3 >= -80.0);
        assert!(
            (gains.0 - 24.0).abs() < 1.0e-6,
            "bass should clamp to +24dB"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audio_output_device_pref_roundtrip_and_fallback() {
        let mut harness = harness_default();
        let dir = make_temp_dir("audio_output_prefs");
        let prefs = dir.join("prefs_test.txt");

        harness
            .state_mut()
            .test_set_audio_output_device_pref(Some("Dummy Output Device"));
        harness.state().test_save_prefs_to_path(&prefs);
        harness.state_mut().test_set_audio_output_device_pref(None);
        harness.state_mut().test_load_prefs_from_path(&prefs);
        assert_eq!(
            harness.state().test_audio_output_device_pref().as_deref(),
            Some("Dummy Output Device")
        );

        harness
            .state_mut()
            .test_set_audio_output_devices(vec!["Device-A".to_string()]);
        assert!(harness
            .state_mut()
            .test_apply_audio_output_device_selection(Some("Missing-Device"), false));
        assert_eq!(harness.state().test_audio_output_device_pref(), None);
        let err = harness
            .state()
            .test_audio_output_error()
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            err.contains("not available"),
            "fallback error message should mention unavailable device: {err}"
        );

        assert!(harness
            .state_mut()
            .test_apply_audio_output_device_selection(Some("Device-A"), false));
        assert_eq!(
            harness.state().test_audio_output_device_pref().as_deref(),
            Some("Device-A")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_zoom_inversion_pref_roundtrip() {
        let mut harness = harness_default();
        let dir = make_temp_dir("editor_zoom_pref");
        let prefs = dir.join("prefs_test.txt");

        harness
            .state_mut()
            .test_set_editor_pref_invert_wave_zoom_wheel(true);
        harness
            .state_mut()
            .test_set_editor_pref_horizontal_zoom_anchor("playhead");
        harness.state().test_save_prefs_to_path(&prefs);

        harness
            .state_mut()
            .test_set_editor_pref_invert_wave_zoom_wheel(false);
        harness
            .state_mut()
            .test_set_editor_pref_horizontal_zoom_anchor("pointer");
        harness.state_mut().test_load_prefs_from_path(&prefs);

        assert!(harness.state().test_editor_pref_invert_wave_zoom_wheel());
        assert_eq!(
            harness.state().test_editor_pref_horizontal_zoom_anchor(),
            "playhead"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_shift_pan_inversion_pref_roundtrip() {
        let mut harness = harness_default();
        let dir = make_temp_dir("editor_shift_pan_pref");
        let prefs = dir.join("prefs_test.txt");

        harness
            .state_mut()
            .test_set_editor_pref_invert_shift_wheel_pan(true);
        harness
            .state_mut()
            .test_set_editor_pref_pause_resume_mode("continue_from_pause");
        harness.state().test_save_prefs_to_path(&prefs);

        harness
            .state_mut()
            .test_set_editor_pref_invert_shift_wheel_pan(false);
        harness
            .state_mut()
            .test_set_editor_pref_pause_resume_mode("return_to_last_start");
        harness.state_mut().test_load_prefs_from_path(&prefs);

        assert!(harness.state().test_editor_pref_invert_shift_wheel_pan());
        assert_eq!(
            harness.state().test_editor_pref_pause_resume_mode(),
            "continue_from_pause"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
