// Regressions for the "list turns red" bug: egui's debug-build warning
// paints (id-clash text + `warn_if_rect_changes_id` red outlines) must
// never fire over the file list. Covers both the general id-clash probe
// and the OS dark/light theme flip that used to swap in an unpatched
// egui style with the debug heuristic re-enabled.
#[cfg(feature = "kittest")]
mod list_id_clash_probe {
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
            "neowaves_list_id_clash_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    fn write_wav(path: &Path, sr: u32, secs: f32) {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        let mono: Vec<f32> = (0..frames)
            .map(|i| ((i as f32) / (sr as f32) * 220.0 * std::f32::consts::TAU).sin() * 0.3)
            .collect();
        neowaves::wave::export_channels_audio(&[mono], sr, path).expect("export wav fixture");
    }

    fn collect_clash_text(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => {
                let text: &str = t.galley.job.text.as_str();
                if text.contains("use of") {
                    out.push(text.to_string());
                }
            }
            egui::Shape::Vec(v) => {
                for s in v {
                    collect_clash_text(s, out);
                }
            }
            _ => {}
        }
    }

    fn clash_messages(harness: &Harness<'static, WavesPreviewer>) -> Vec<String> {
        let mut out = Vec::new();
        for cs in &harness.output().shapes {
            collect_clash_text(&cs.shape, &mut out);
        }
        out.sort();
        out.dedup();
        out
    }

    // `warn_if_rect_changes_id` false positives paint 2px pure-red
    // outlines (no text) around widget rects; count them.
    fn count_red_warning_rects(shape: &egui::Shape, out: &mut usize) {
        match shape {
            egui::Shape::Rect(rs) => {
                if rs.stroke.color == egui::Color32::RED
                    && rs.stroke.width >= 1.5
                    && rs.fill == egui::Color32::TRANSPARENT
                {
                    *out += 1;
                }
            }
            egui::Shape::Vec(v) => {
                for s in v {
                    count_red_warning_rects(s, out);
                }
            }
            _ => {}
        }
    }

    fn red_warning_rects(harness: &Harness<'static, WavesPreviewer>) -> usize {
        let mut n = 0;
        for cs in &harness.output().shapes {
            count_red_warning_rects(&cs.shape, &mut n);
        }
        n
    }

    fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().scan_in_progress && !harness.state().files.is_empty() {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "scan did not finish"
            );
        }
    }


    #[test]
    fn detector_sanity_finds_intentional_clash() {
        let mut harness = egui_kittest::Harness::new(|ctx| {
            ctx.options_mut(|o| o.warn_on_id_clash = true);
            egui::CentralPanel::default().show(ctx, |ui| {
                let id = egui::Id::new("dup");
                let r1 = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(50.0, 20.0));
                let r2 = egui::Rect::from_min_size(egui::pos2(10.0, 60.0), egui::vec2(50.0, 20.0));
                let _ = ui.interact(r1, id, egui::Sense::click());
                let _ = ui.interact(r2, id, egui::Sense::click());
            });
        });
        harness.run_steps(3);
        let mut out = Vec::new();
        for cs in &harness.output().shapes {
            collect_clash_text(&cs.shape, &mut out);
        }
        assert!(!out.is_empty(), "detector failed to see an intentional id clash");
    }

    // Regression for the "list turns red" bug: egui keeps separate
    // dark/light styles, and with the default `ThemePreference::System`
    // the active style slot follows the OS theme. The startup style patch
    // (which disables the `warn_if_rect_changes_id` debug heuristic and
    // sets the app's text styles) used to land only in the slot active at
    // startup. When the OS later reports the other theme, egui switches
    // to the unpatched slot: the app still looks dark (visuals get
    // re-applied), but the debug heuristic is back on and the virtualized
    // list paints 2px red outlines around every cell after scroll jumps.
    #[test]
    fn os_theme_flip_does_not_paint_red_debug_rects() {
        let dir = make_temp_dir("theme_flip");
        for i in 0..60 {
            write_wav(&dir.join(format!("clip_{i:03}.wav")), 24_000, 0.05);
        }
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        let mut harness = harness_with_startup(cfg);
        // The native app runs with egui's default ThemePreference::System
        // (the kittest harness pins a fixed theme for determinism), so
        // restore the real-app behavior for this regression.
        harness
            .ctx
            .options_mut(|o| o.theme_preference = egui::ThemePreference::System);
        wait_for_scan(&mut harness);
        for _ in 0..5 {
            harness.run_steps(1);
        }

        // Simulate the OS reporting the other theme mid-session; with
        // ThemePreference::System this swaps egui's active style slot.
        let start_theme = harness.ctx.theme();
        let flipped = match start_theme {
            egui::Theme::Dark => egui::Theme::Light,
            egui::Theme::Light => egui::Theme::Dark,
        };
        harness.input_mut().system_theme = Some(flipped);
        for _ in 0..5 {
            harness.run_steps(1);
        }

        // The debug heuristic must stay off in whatever slot is active.
        let warn_flag = harness
            .ctx
            .global_style()
            .debug
            .warn_if_rect_changes_id;
        assert!(
            !warn_flag,
            "warn_if_rect_changes_id re-enabled after a system theme flip \
             (style patch landed in only one theme slot)"
        );

        // Drive the virtualized list through scroll jumps that reuse row
        // rects for different rows and make sure no red debug outlines
        // are painted.
        harness.hover_at(egui::pos2(400.0, 400.0));
        harness.event(egui::Event::PointerButton {
            pos: egui::pos2(400.0, 300.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerButton {
            pos: egui::pos2(400.0, 300.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(2);
        let mut max_red = 0usize;
        for _ in 0..6 {
            harness.key_press(egui::Key::PageDown);
            harness.run_steps(1);
            max_red = max_red.max(red_warning_rects(&harness));
            harness.run_steps(1);
            max_red = max_red.max(red_warning_rects(&harness));
        }
        for _ in 0..6 {
            harness.key_press(egui::Key::PageUp);
            harness.run_steps(1);
            max_red = max_red.max(red_warning_rects(&harness));
        }
        assert_eq!(
            max_red, 0,
            "red debug outlines painted over the list after an OS theme flip"
        );
    }

    #[test]
    fn list_view_has_no_id_clashes() {
        let dir = make_temp_dir("media");
        for i in 0..40 {
            write_wav(&dir.join(format!("clip_{i:03}.wav")), 24_000, 0.05);
        }
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        let mut harness = harness_with_startup(cfg);
        harness.ctx.options_mut(|o| o.warn_on_id_clash = true);
        wait_for_scan(&mut harness);

        let mut seen: Vec<String> = Vec::new();
        let mut record = |harness: &Harness<'static, WavesPreviewer>, phase: &str| {
            for msg in clash_messages(harness) {
                seen.push(format!("[{phase}] {msg}"));
            }
        };

        for _ in 0..10 {
            harness.run_steps(1);
        }
        record(&harness, "idle");

        // Hover over rows.
        for y in [200.0, 300.0, 400.0, 500.0] {
            harness.hover_at(egui::pos2(400.0, y));
            harness.run_steps(2);
            record(&harness, "hover");
        }

        // Click a row (select + load preview).
        harness.event(egui::Event::PointerButton {
            pos: egui::pos2(400.0, 300.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerButton {
            pos: egui::pos2(400.0, 300.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        for _ in 0..8 {
            harness.run_steps(1);
            record(&harness, "click");
        }

        // Keyboard navigation.
        for _ in 0..5 {
            harness.key_press(egui::Key::ArrowDown);
            harness.run_steps(1);
            record(&harness, "arrows");
        }

        // Scroll the list.
        harness.hover_at(egui::pos2(400.0, 400.0));
        for _ in 0..6 {
            harness.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -3.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            harness.run_steps(1);
            record(&harness, "scroll");
        }
        for _ in 0..10 {
            harness.run_steps(1);
            record(&harness, "settle");
        }

        seen.sort();
        seen.dedup();
        assert!(
            seen.is_empty(),
            "egui id clashes detected in the list view:\n{}",
            seen.join("\n")
        );
    }
}
