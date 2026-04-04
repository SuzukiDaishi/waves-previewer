use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Key, RichText};

use super::*;

impl WavesPreviewer {
    pub(super) fn run_frame(
        &mut self,
        ctx: &egui::Context,
        frame_started: Instant,
        had_ui_input: bool,
    ) {
        self.run_frame_pre_ui(ctx, frame_started, had_ui_input);
        let activate_path = self.run_frame_workspace(ctx);
        let activated_tab_idx = self.run_frame_activation(ctx, activate_path);
        if let Some(tab_idx) = activated_tab_idx {
            self.refresh_tool_preview_for_tab(tab_idx);
        }
        self.run_frame_overlays(ctx);
        self.run_frame_modal_windows(ctx);
        self.run_frame_finish(ctx, frame_started);
    }

    fn run_frame_pre_ui(
        &mut self,
        ctx: &egui::Context,
        frame_started: Instant,
        had_ui_input: bool,
    ) {
        if had_ui_input {
            self.debug.ui_input_started_at = Some(frame_started);
        }
        self.suppress_list_enter = false;
        if ctx.dragged_id().is_some() && !ctx.input(|i| i.pointer.any_down()) {
            if self.debug.cfg.enabled {
                self.debug_trace_input("force stop_dragging (pointer released outside)");
            }
            ctx.stop_dragging();
        }
        self.ensure_theme_visuals(ctx);
        self.tick_project_open();
        self.meter_db = self.current_output_meter_db();
        self.playback_sync_state_snapshot();
        self.apply_effective_volume();
        self.process_scan_messages();
        self.pump_list_meta_prefetch();
        self.process_ipc_requests();
        self.process_mcp_commands(ctx);
        self.apply_pending_transcript_seek();
        self.process_tool_results();
        self.process_tool_queue();
        self.apply_search_if_due();
        self.handle_screenshot_events(ctx);
        if ctx.input(|i| i.key_pressed(Key::F9)) {
            let path = self.default_screenshot_path();
            self.request_screenshot(ctx, path, false);
        }
        self.run_startup_actions(ctx);
        self.debug_tick(ctx);
        self.drain_heavy_preview_results();
        self.drain_list_preview_results();
        self.drain_list_preview_prefetch_results();
        self.drain_editor_decode();
        self.drain_heavy_overlay_results();
        self.drain_editor_apply_jobs(ctx);
        self.drain_plugin_jobs(ctx);
        self.drain_transcript_model_download_results(ctx);
        self.drain_transcript_ai_results(ctx);
        self.drain_music_model_download_results(ctx);
        self.drain_music_ai_results(ctx);
        self.drain_music_preview_results(ctx);
        self.enforce_music_stem_cache_policy();
        self.drain_meta_updates(ctx);
        self.drain_external_load_results(ctx);
        self.check_csv_export_completion();
        self.tick_bulk_resample();
        if self.bulk_resample_state.is_some() {
            ctx.request_repaint();
        }
        self.apply_spectrogram_updates(ctx);
        self.apply_feature_analysis_updates(ctx);
        self.apply_editor_viewport_render_updates(ctx);
        self.drain_export_results(ctx);
        self.drain_lufs_recalc_results();
        self.drain_effect_graph_runner(ctx);
        self.tick_playback_fx_state(ctx);
        self.pump_lufs_recalc_worker();
        self.tick_processing_state(ctx);
    }

    fn run_frame_workspace(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        self.ui_top_bar(ctx);
        self.handle_dropped_files(ctx);
        let mut activate_path: Option<PathBuf> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                let is_list = self.is_list_workspace_active();
                let list_label = if is_list {
                    RichText::new("[List]").strong()
                } else {
                    RichText::new("List")
                };
                if ui.selectable_label(is_list, list_label).clicked() {
                    if let Some(idx) = self.active_tab {
                        self.clear_preview_if_any(idx);
                    }
                    self.workspace_view = WorkspaceView::List;
                    self.pending_activate_path = None;
                    self.pending_activate_kind = None;
                    self.pending_activate_ready = false;
                    self.audio.set_loop_enabled(false);
                    self.request_list_focus(ctx);
                }
                if self.effect_graph.workspace_open {
                    ui.horizontal(|ui| {
                        let active = self.is_effect_graph_workspace_active();
                        let text = if active {
                            RichText::new("[Effect Graph]").strong()
                        } else {
                            RichText::new("Effect Graph")
                        };
                        if ui.selectable_label(active, text).clicked() {
                            self.workspace_view = WorkspaceView::EffectGraph;
                            self.effect_graph.workspace_open = true;
                            self.effect_graph.last_editor_tab = self.active_tab;
                        }
                        if ui.button("x").on_hover_text("Close").clicked() {
                            self.request_close_effect_graph_workspace();
                        }
                    });
                }
                let mut to_close: Option<usize> = None;
                let tabs_len = self.tabs.len();
                for i in 0..tabs_len {
                    let active =
                        self.workspace_view == WorkspaceView::Editor && self.active_tab == Some(i);
                    let tab = &self.tabs[i];
                    let mut display = tab.display_name.clone();
                    if tab.dirty || tab.loop_markers_dirty || tab.markers_dirty {
                        display = format!("\u{25CF} {display}");
                    }
                    let path_for_activate = tab.path.clone();
                    let text = if active {
                        RichText::new(format!("[{}]", display)).strong()
                    } else {
                        RichText::new(display)
                    };
                    ui.horizontal(|ui| {
                        if ui.selectable_label(active, text).clicked() {
                            if let Some(prev) = self.active_tab {
                                if prev != i {
                                    self.clear_preview_if_any(prev);
                                }
                            }
                            self.workspace_view = WorkspaceView::Editor;
                            self.active_tab = Some(i);
                            self.debug_mark_tab_switch_start(&path_for_activate);
                            activate_path = Some(path_for_activate.clone());
                        }
                        if ui.button("x").on_hover_text("Close").clicked() {
                            self.clear_preview_if_any(i);
                            to_close = Some(i);
                        }
                    });
                }
                if let Some(i) = to_close {
                    self.close_tab_at(i, ctx);
                }
            });
            ui.separator();
            if self.is_effect_graph_workspace_active() {
                self.ui_effect_graph_view(ui, ctx);
            } else if let Some(tab_idx) = self
                .active_tab
                .filter(|_| self.workspace_view == WorkspaceView::Editor)
            {
                self.ui_editor_view(ui, ctx, tab_idx);
            } else {
                self.ui_list_view(ui, ctx);
            }
        });
        activate_path
    }

    fn run_frame_activation(
        &mut self,
        ctx: &egui::Context,
        activate_path: Option<PathBuf>,
    ) -> Option<usize> {
        let mut activated_tab_idx: Option<usize> = None;
        if let Some(p) = activate_path {
            self.queue_tab_activation(p);
            ctx.request_repaint();
        }
        if let Some(pending) = self.pending_activate_path.clone() {
            if !self.pending_activate_ready {
                self.pending_activate_ready = true;
                ctx.request_repaint();
            } else {
                let p = pending;
                let activation_kind = self
                    .pending_activate_kind
                    .unwrap_or(PendingTabActivationKind::TabSwitch);
                self.pending_activate_path = None;
                self.pending_activate_kind = None;
                self.pending_activate_ready = false;
                self.playing_path = Some(p.clone());
                if !self.apply_dirty_tab_audio_with_mode(&p) {
                    let mut used_tab_transport = false;
                    let source_time_sec = self.playback_current_source_time_sec();
                    if let Some(idx) = self.active_tab {
                        let measure_stream_activation =
                            matches!(activation_kind, PendingTabActivationKind::InitialOpen)
                                && self
                                    .tabs
                                    .get(idx)
                                    .map(|tab| {
                                        tab.path == p && self.editor_stream_transport_eligible(tab)
                                    })
                                    .unwrap_or(false);
                        let activation_started =
                            measure_stream_activation.then(std::time::Instant::now);
                        let activated_stream =
                            self.try_activate_editor_stream_transport_for_tab(idx);
                        if let Some(started_at) = activation_started {
                            self.debug_push_editor_stream_activation_sample(
                                started_at.elapsed().as_secs_f32() * 1000.0,
                            );
                        }
                        if activated_stream {
                            used_tab_transport = true;
                            if let Some(source_time_sec) = source_time_sec {
                                self.playback_seek_to_source_time(self.mode, source_time_sec);
                            }
                        } else if let Some(tab) = self.tabs.get(idx) {
                            if tab.path == p && !tab.ch_samples.is_empty() {
                                used_tab_transport = true;
                                let channels = tab.ch_samples.clone();
                                let in_sr = tab.buffer_sample_rate.max(1);
                                if self.mode_requires_offline_processing() {
                                    self.audio.stop();
                                    self.audio.set_samples_mono(Vec::new());
                                    self.spawn_heavy_processing_from_channels(
                                        p.clone(),
                                        channels,
                                        ProcessingTarget::EditorTab(p.clone()),
                                    );
                                } else {
                                    let mut render_spec = self.offline_render_spec_for_path(&p);
                                    render_spec.master_gain_db = 0.0;
                                    render_spec.file_gain_db = 0.0;
                                    let rendered = Self::render_channels_offline_with_spec(
                                        channels,
                                        in_sr,
                                        render_spec,
                                        false,
                                    );
                                    self.audio.set_samples_channels(rendered);
                                    self.playback_mark_buffer_source(
                                        PlaybackSourceKind::EditorTab(p.clone()),
                                        self.audio.shared.out_sample_rate.max(1),
                                    );
                                    if let Some(source_time_sec) = source_time_sec {
                                        self.playback_seek_to_source_time(
                                            self.mode,
                                            source_time_sec,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    if !used_tab_transport {
                        if let Some(idx) = self.active_tab {
                            if let Some(tab) = self.tabs.get_mut(idx) {
                                if tab.path == p && !tab.loading {
                                    tab.loading = true;
                                    self.spawn_editor_decode(p.clone());
                                }
                            }
                        }
                        match self.mode {
                            RateMode::Speed | RateMode::PitchShift | RateMode::TimeStretch => {
                                self.playback_mark_buffer_source(
                                    PlaybackSourceKind::EditorTab(p.clone()),
                                    self.audio.shared.out_sample_rate.max(1),
                                );
                            }
                        }
                    }
                    if let Some(idx) = self.active_tab {
                        if let Some(tab) = self.tabs.get(idx) {
                            self.apply_loop_mode_for_tab(tab);
                        }
                    }
                    self.apply_effective_volume();
                }
                if matches!(activation_kind, PendingTabActivationKind::TabSwitch) {
                    self.debug_mark_tab_switch_interactive(&p);
                }
                activated_tab_idx = self.active_tab;
            }
        }
        activated_tab_idx
    }

    fn run_frame_overlays(&mut self, ctx: &egui::Context) {
        if let Some(tab_idx) = self.active_tab {
            self.queue_editor_analysis_for_tab(tab_idx);
        } else {
            self.ui_editor_zoo_overlay(ctx, None, ctx.content_rect());
        }
        self.ui_busy_overlay(ctx);
    }

    fn run_frame_modal_windows(&mut self, ctx: &egui::Context) {
        self.run_frame_leave_prompt(ctx);
        self.run_frame_first_save_prompt(ctx);
        self.ui_export_settings_window(ctx);
        self.ui_transcription_settings_window(ctx);
        self.ui_external_data_window(ctx);
        self.ui_transcript_window(ctx);
        self.ui_list_art_window(ctx);
        self.ui_tool_palette_window(ctx);
        self.ui_tool_confirm_dialog(ctx);
        self.run_frame_rename_dialogs(ctx);
        self.run_frame_resample_dialog(ctx);
        self.ui_debug_window(ctx);
        self.handle_global_shortcuts(ctx);
        self.handle_clipboard_hotkeys(ctx);
        self.handle_undo_redo_hotkeys(ctx);
    }

    fn run_frame_leave_prompt(&mut self, ctx: &egui::Context) {
        if !self.show_leave_prompt {
            return;
        }
        let mut open = self.show_leave_prompt;
        let mut cancel_like_close = false;
        egui::Window::new("Leave Editor?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("The waveform has been modified in memory. Leave this editor?");
                ui.horizontal(|ui| {
                    if ui.button("Leave").clicked() {
                        match self.leave_intent.take() {
                            Some(LeaveIntent::CloseTab(i)) => {
                                if i < self.tabs.len() {
                                    self.close_tab_at(i, ctx);
                                }
                            }
                            Some(LeaveIntent::ToTab(i)) => {
                                if let Some(t) = self.tabs.get(i) {
                                    self.active_tab = Some(i);
                                    self.queue_tab_activation(t.path.clone());
                                }
                                self.rebuild_current_buffer_with_mode();
                            }
                            Some(LeaveIntent::ToList) => {
                                self.active_tab = None;
                                self.audio.set_loop_enabled(false);
                                self.request_list_focus(ctx);
                            }
                            None => {}
                        }
                        self.show_leave_prompt = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.leave_intent = None;
                        cancel_like_close = true;
                    }
                });
            });
        if cancel_like_close {
            open = false;
        }
        if !open {
            self.leave_intent = None;
            self.show_leave_prompt = false;
        }
    }

    fn run_frame_first_save_prompt(&mut self, ctx: &egui::Context) {
        if !self.show_first_save_prompt {
            return;
        }
        let mut open = self.show_first_save_prompt;
        let mut close_prompt = false;
        egui::Window::new("First Export Option")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("Choose default export behavior for Ctrl+E:");
                ui.horizontal(|ui| {
                    if ui.button("Overwrite").clicked() {
                        self.export_cfg.save_mode = SaveMode::Overwrite;
                        self.export_cfg.first_prompt = false;
                        close_prompt = true;
                        self.trigger_save_selected();
                    }
                    if ui.button("New File").clicked() {
                        self.export_cfg.save_mode = SaveMode::NewFile;
                        self.export_cfg.first_prompt = false;
                        close_prompt = true;
                        self.trigger_save_selected();
                    }
                    if ui.button("Cancel").clicked() {
                        close_prompt = true;
                    }
                });
            });
        if close_prompt {
            open = false;
        }
        self.show_first_save_prompt = open;
    }

    fn run_frame_rename_dialogs(&mut self, ctx: &egui::Context) {
        if self.show_rename_dialog {
            let mut do_rename = false;
            let mut open = self.show_rename_dialog;
            let mut cancel_like_close = false;
            egui::Window::new("Rename File")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    let rename_edit_id = egui::Id::new("rename_input_text");
                    if let Some(path) = self.rename_target.as_ref() {
                        ui.label(path.display().to_string());
                    }
                    let resp =
                        ui.add(egui::TextEdit::singleline(&mut self.rename_input).id(rename_edit_id));
                    if self.rename_focus_next {
                        resp.request_focus();
                        self.rename_focus_next = false;
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        do_rename = true;
                    }
                    if let Some(err) = self.rename_error.as_ref() {
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    }
                    ui.horizontal(|ui| {
                        let can = !self.rename_input.trim().is_empty();
                        if ui.add_enabled(can, egui::Button::new("Rename")).clicked() {
                            do_rename = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_like_close = true;
                        }
                    });
                });
            if do_rename {
                let name = self.rename_input.clone();
                if let Some(path) = self.rename_target.clone() {
                    match self.rename_file_path(&path, &name) {
                        Ok(_) => {
                            self.show_rename_dialog = false;
                            self.rename_target = None;
                            self.rename_focus_next = false;
                            self.rename_error = None;
                        }
                        Err(err) => {
                            self.rename_error = Some(err);
                        }
                    }
                } else {
                    self.show_rename_dialog = false;
                }
            }
            if cancel_like_close {
                open = false;
            }
            if !open {
                self.show_rename_dialog = false;
                self.rename_target = None;
                self.rename_focus_next = false;
                self.rename_error = None;
            }
        }
        if self.show_batch_rename_dialog {
            let mut do_rename = false;
            let mut open = self.show_batch_rename_dialog;
            let mut cancel_like_close = false;
            egui::Window::new("Batch Rename")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(format!("{} files", self.batch_rename_targets.len()));
                    ui.horizontal(|ui| {
                        ui.label("Pattern:");
                        ui.text_edit_singleline(&mut self.batch_rename_pattern);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Start:");
                        ui.add(
                            egui::DragValue::new(&mut self.batch_rename_start).range(0..=1_000_000),
                        );
                        ui.label("Zero pad:");
                        ui.add(egui::DragValue::new(&mut self.batch_rename_pad).range(0..=6));
                    });
                    ui.label("Tokens: {name} (original stem), {n} (sequence)");
                    if let Some(err) = self.batch_rename_error.as_ref() {
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    }
                    let preview_count = 4usize;
                    ui.separator();
                    ui.label("Preview:");
                    for (i, src) in self.batch_rename_targets.iter().take(preview_count).enumerate()
                    {
                        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        let num = self.batch_rename_start.saturating_add(i as u32);
                        let num_str = if self.batch_rename_pad > 0 {
                            format!("{:0width$}", num, width = self.batch_rename_pad as usize)
                        } else {
                            num.to_string()
                        };
                        let mut name = self
                            .batch_rename_pattern
                            .replace("{name}", stem)
                            .replace("{n}", &num_str);
                        let has_ext = std::path::Path::new(&name).extension().is_some();
                        if !has_ext {
                            if let Some(ext) = src.extension().and_then(|s| s.to_str()) {
                                name.push('.');
                                name.push_str(ext);
                            }
                        }
                        ui.label(format!("{} -> {}", src.display(), name));
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked() {
                            do_rename = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_like_close = true;
                        }
                    });
                });
            if do_rename {
                match self.batch_rename_paths() {
                    Ok(()) => {
                        self.show_batch_rename_dialog = false;
                        self.batch_rename_targets.clear();
                        self.batch_rename_error = None;
                    }
                    Err(err) => {
                        self.batch_rename_error = Some(err);
                    }
                }
            }
            if cancel_like_close {
                open = false;
            }
            if !open {
                self.show_batch_rename_dialog = false;
                self.batch_rename_targets.clear();
                self.batch_rename_error = None;
            }
        }
    }

    fn run_frame_resample_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_resample_dialog {
            return;
        }
        let mut do_apply = false;
        let mut open = self.show_resample_dialog;
        let mut cancel_like_close = false;
        egui::Window::new("Sample Rate Convert")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("{} files", self.resample_targets.len()));
                ui.horizontal(|ui| {
                    ui.label("Target sample rate (Hz):");
                    ui.add(
                        egui::DragValue::new(&mut self.resample_target_sr)
                            .range(8000..=384_000)
                            .speed(100.0),
                    );
                });
                if let Some(err) = self.resample_error.as_ref() {
                    ui.colored_label(egui::Color32::LIGHT_RED, err);
                }
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        do_apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_like_close = true;
                    }
                });
            });
        if do_apply {
            match self.apply_resample_dialog() {
                Ok(()) => {
                    self.show_resample_dialog = false;
                    self.resample_targets.clear();
                    self.resample_error = None;
                }
                Err(err) => {
                    self.resample_error = Some(err);
                }
            }
        }
        if cancel_like_close {
            open = false;
        }
        if !open {
            self.show_resample_dialog = false;
            self.resample_targets.clear();
            self.resample_error = None;
        }
    }

    fn run_frame_finish(&mut self, ctx: &egui::Context, frame_started: Instant) {
        let playing = self
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        let fast_repaint = playing
            || self.scan_in_progress
            || self.processing.is_some()
            || self.playback_fx_state.is_some()
            || self.list_preview_rx.is_some()
            || self.list_preview_pending_path.is_some()
            || self.editor_decode_state.is_some()
            || self.heavy_preview_rx.is_some()
            || self.heavy_overlay_rx.is_some()
            || self.music_ai_state.is_some()
            || self.music_preview_state.is_some()
            || self.editor_apply_state.is_some()
            || self.plugin_process_state.is_some()
            || self.export_state.is_some()
            || self.csv_export_state.is_some()
            || self.bulk_resample_state.is_some()
            || !self.editor_feature_inflight.is_empty();
        let repaint_ms = if fast_repaint {
            16
        } else if self.zoo_enabled && self.is_list_workspace_active() {
            50
        } else if self.zoo_enabled {
            33
        } else {
            80
        };
        ctx.request_repaint_after(Duration::from_millis(repaint_ms));
        if let Some(started_at) = self.debug.ui_input_started_at.take() {
            let elapsed_ms = started_at.elapsed().as_secs_f32() * 1000.0;
            self.debug_push_ui_input_to_paint_sample(elapsed_ms);
        }
        let frame_ms = frame_started.elapsed().as_secs_f64() * 1000.0;
        self.debug.frame_last_ms = frame_ms as f32;
        self.debug.frame_sum_ms += frame_ms;
        self.debug.frame_samples = self.debug.frame_samples.saturating_add(1);
        if self.debug.frame_peak_ms < self.debug.frame_last_ms {
            self.debug.frame_peak_ms = self.debug.frame_last_ms;
        }
    }
}
