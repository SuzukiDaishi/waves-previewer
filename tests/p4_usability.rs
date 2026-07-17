#[cfg(feature = "kittest")]
mod p4_usability {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use egui_kittest::{kittest::Queryable, Harness};
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
    fn empty_state_panel_shows_without_folder_and_hides_with_files() {
        // No folder, no items: onboarding panel with the Open Folder button.
        let mut harness = harness_with_startup(StartupConfig::default());
        harness.run_steps(3);
        let _ = harness.get_by_label("Open Folder...");
        let _ = harness.get_by_label("NeoWaves");

        // With a folder loaded the panel disappears (table renders instead).
        let (mut harness, dir) = harness_with_files("emptystate", 1);
        harness.run_steps(2);
        assert!(
            harness.query_by_label("Open Folder...").is_none(),
            "onboarding panel must hide once files are loaded"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
    fn edit_menu_undo_state_tracks_editor_edits() {
        let (mut harness, dir) = harness_with_files("undomenu", 1);
        assert!(harness.state_mut().test_open_first_tab());
        wait_until(&mut harness, "tab ready", |h| {
            h.state()
                .active_tab
                .and_then(|i| h.state().tabs.get(i))
                .map(|t| t.samples_len > 0)
                .unwrap_or(false)
        });
        assert!(
            !harness.state().test_undo_available(false),
            "fresh tab must have nothing to undo"
        );
        // A destructive edit makes Undo available; triggering the shared
        // menu/hotkey dispatch restores the buffer and enables Redo.
        let before_len = {
            let idx = harness.state().active_tab.unwrap();
            harness.state().tabs[idx].samples_len
        };
        assert!(harness.state_mut().test_apply_trim_frac(0.25, 0.75));
        harness.run_steps(2);
        assert!(harness.state().test_undo_available(false));
        assert!(harness.state_mut().test_trigger_undo_redo(false));
        harness.run_steps(2);
        let idx = harness.state().active_tab.unwrap();
        assert_eq!(harness.state().tabs[idx].samples_len, before_len);
        assert!(harness.state().test_undo_available(true), "redo available");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keymap_rebind_changes_dispatch_and_rejects_conflicts() {
        let (mut harness, dir) = harness_with_files("keymap", 1);
        assert!(harness.state_mut().test_open_first_tab());
        wait_until(&mut harness, "tab ready", |h| {
            h.state()
                .active_tab
                .and_then(|i| h.state().tabs.get(i))
                .map(|t| t.samples_len > 0)
                .unwrap_or(false)
        });
        harness.run_steps(3);
        let spp = |h: &Harness<'static, WavesPreviewer>| {
            let idx = h.state().active_tab.unwrap();
            h.state().tabs[idx].samples_per_px
        };
        // Baseline: the built-in + chord zooms in.
        let spp0 = spp(&harness);
        harness.key_press(egui::Key::Plus);
        harness.run_steps(3);
        let spp1 = spp(&harness);
        assert!(spp1 < spp0, "built-in + should zoom: {spp0} -> {spp1}");
        // Rebind zoom-in to Q: Q now zooms and the old chord is released.
        harness
            .state_mut()
            .test_keymap_assign("EditorZoomIn", "Q")
            .expect("rebind to Q");
        assert_eq!(
            harness.state().test_keymap_effective("EditorZoomIn").as_deref(),
            Some("Q")
        );
        harness.key_press(egui::Key::Q);
        harness.run_steps(3);
        let spp2 = spp(&harness);
        assert!(spp2 < spp1, "rebound Q should zoom: {spp1} -> {spp2}");
        harness.key_press(egui::Key::Plus);
        harness.run_steps(3);
        assert_eq!(spp(&harness), spp2, "old + chord must no longer zoom");
        // Conflicts: same context and overlapping Global context both refuse.
        assert!(harness
            .state_mut()
            .test_keymap_assign("EditorZoomOut", "Q")
            .is_err());
        assert!(harness
            .state_mut()
            .test_keymap_assign("EditorZoomOut", "Space")
            .is_err());
        // Re-assigning the built-in default clears the override.
        harness
            .state_mut()
            .test_keymap_assign("EditorZoomIn", "Plus")
            .expect("restore default");
        assert_eq!(harness.state().test_keymap_override_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_toolbar_click_switches_tool() {
        let (mut harness, dir) = harness_with_files("toolbar", 1);
        assert!(harness.state_mut().test_open_first_tab());
        wait_until(&mut harness, "tab ready", |h| {
            h.state()
                .active_tab
                .and_then(|i| h.state().tabs.get(i))
                .map(|t| t.samples_len > 0)
                .unwrap_or(false)
        });
        harness.run_steps(3);
        assert_eq!(
            harness.state().test_active_tool(),
            Some(neowaves::app::ToolKind::LoopEdit)
        );
        // Click the Trim scissors icon in the new toolbar.
        harness.get_by_label("✂").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_active_tool(),
            Some(neowaves::app::ToolKind::Trim)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heavy_apply_does_not_modal_block_and_discards_result_after_tab_close() {
        let (mut harness, dir) = harness_with_files("nonblock", 1);
        assert!(harness.state_mut().test_open_first_tab());
        wait_until(&mut harness, "tab ready", |h| {
            h.state()
                .active_tab
                .and_then(|i| h.state().tabs.get(i))
                .map(|t| t.samples_len > 0)
                .unwrap_or(false)
        });
        harness.run_steps(2);
        // Kick a heavy async apply; while it runs, the modal busy overlay
        // must stay down (the apply is per-tab, not app-blocking).
        assert!(harness.state_mut().test_apply_time_stretch(1.5));
        assert!(harness.state().test_editor_apply_busy());
        assert!(
            !harness.state().test_busy_overlay_blocking(),
            "editor apply must not raise the modal busy overlay"
        );
        // Close the tab before/while the worker finishes: the result must be
        // discarded (no panic, no resurrection of the tab or its audio).
        assert!(harness.state_mut().test_force_close_tab(0));
        wait_until(&mut harness, "apply state drained", |h| {
            !h.state().test_editor_apply_busy()
        });
        assert!(harness.state().tabs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_history_labels_and_multi_step_jumps() {
        let (mut harness, dir) = harness_with_files("history", 1);
        assert!(harness.state_mut().test_open_first_tab());
        wait_until(&mut harness, "tab ready", |h| {
            h.state()
                .active_tab
                .and_then(|i| h.state().tabs.get(i))
                .map(|t| t.samples_len > 0)
                .unwrap_or(false)
        });
        let len0 = {
            let idx = harness.state().active_tab.unwrap();
            harness.state().tabs[idx].samples_len
        };
        // Two labeled edits: Trim then Invert Polarity.
        assert!(harness.state_mut().test_apply_trim_frac(0.1, 0.9));
        harness.run_steps(2);
        assert!(harness.state_mut().test_apply_invert_polarity_frac(0.0, 1.0));
        harness.run_steps(2);
        let (undo, redo) = harness.state().test_undo_history_labels();
        assert_eq!(undo, vec!["Trim".to_string(), "Invert Polarity".to_string()]);
        assert!(redo.is_empty());
        // Jump two steps back in one click: original buffer restored, both
        // ops now sit in the redo (future) column with their labels.
        assert_eq!(harness.state_mut().test_undo_history_jump(false, 2), 2);
        harness.run_steps(2);
        let idx = harness.state().active_tab.unwrap();
        assert_eq!(harness.state().tabs[idx].samples_len, len0);
        let (undo, redo) = harness.state().test_undo_history_labels();
        assert!(undo.is_empty());
        assert_eq!(
            redo,
            vec!["Invert Polarity".to_string(), "Trim".to_string()],
            "redo stack keeps op labels (top = next redo)"
        );
        // Redo one step: Trim comes back.
        assert_eq!(harness.state_mut().test_undo_history_jump(true, 1), 1);
        harness.run_steps(2);
        let (undo, _) = harness.state().test_undo_history_labels();
        assert_eq!(undo, vec!["Trim".to_string()]);
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
