use crate::app::types::{RateMode, ToolKind};
use egui::{Align, Color32, Key, RichText, Sense};
use std::time::Duration;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.menu_button("File", |ui| {
                        if ui.button("Project Open...").clicked() {
                            if let Some(path) = self.pick_project_open_dialog() {
                                self.queue_project_open(path);
                            }
                            ui.close();
                        }
                        let has_project = self.project_path.is_some();
                        if ui
                            .add_enabled(has_project, egui::Button::new("Project Save"))
                            .clicked()
                        {
                            if let Err(err) = self.save_project() {
                                self.debug_log(format!("project save error: {err}"));
                            }
                            ui.close();
                        }
                        if ui.button("Project Save As...").clicked() {
                            if let Some(mut path) = self.pick_project_save_dialog() {
                                let needs_ext = path
                                    .extension()
                                    .and_then(|s| s.to_str())
                                    .map(|s| !s.eq_ignore_ascii_case("nwproj"))
                                    .unwrap_or(true);
                                if needs_ext {
                                    path.set_extension("nwproj");
                                }
                                if let Err(err) = self.save_project_as(path) {
                                    self.debug_log(format!("project save error: {err}"));
                                }
                            }
                            ui.close();
                        }
                        if ui.button("Project Close").clicked() {
                            self.close_project();
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Folder...").clicked() {
                            if let Some(dir) = self.pick_folder_dialog() {
                                self.root = Some(dir);
                                self.rescan();
                            }
                            ui.close();
                        }
                        if ui.button("Files...").clicked() {
                            if let Some(files) = self.pick_files_dialog() {
                                self.replace_with_files(&files);
                                self.after_add_refresh();
                            }
                            ui.close();
                        }
                    });
                    ui.menu_button("Export", |ui| {
                        if ui.button("Apply Gains (new files)").clicked() {
                            self.spawn_export_gains(false);
                            ui.close();
                        }
                        if ui.button("Clear All Gains").clicked() {
                            self.clear_all_pending_gains();
                            self.lufs_override.clear();
                            self.lufs_recalc_deadline.clear();
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Save Selected (Ctrl+S)").clicked() {
                            self.trigger_save_selected();
                            ui.close();
                        }
                    });
                    ui.menu_button("List", |ui| {
                        if ui.button("Open First in Editor").clicked() {
                            self.open_first_in_list();
                            ui.close();
                        }
                        ui.separator();
                        let selected = self.selected_paths();
                        let real_selected = self.selected_real_paths();
                        let has_selection = !selected.is_empty();
                        let has_real_selection = !real_selected.is_empty();
                        if ui
                            .add_enabled(
                                has_selection,
                                egui::Button::new("Copy Selected to Clipboard"),
                            )
                            .clicked()
                        {
                            self.copy_selected_to_clipboard();
                            ui.close();
                        }
                        let can_paste = self
                            .clipboard_payload
                            .as_ref()
                            .map(|p| !p.items.is_empty())
                            .unwrap_or(false)
                            || !self.get_clipboard_files().is_empty();
                        if ui
                            .add_enabled(can_paste, egui::Button::new("Paste"))
                            .clicked()
                        {
                            self.paste_clipboard_to_list();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                has_real_selection,
                                egui::Button::new("Rename Selected..."),
                            )
                            .clicked()
                        {
                            if real_selected.len() == 1 {
                                self.open_rename_dialog(real_selected[0].clone());
                            } else {
                                self.open_batch_rename_dialog(real_selected.clone());
                            }
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                has_selection,
                                egui::Button::new("Remove Selected from List"),
                            )
                            .clicked()
                        {
                            self.remove_paths_from_list(&selected);
                            ui.close();
                        }
                        let has_edits = self.has_edits_for_paths(&selected);
                        if ui
                            .add_enabled(has_edits, egui::Button::new("Clear Edits for Selected"))
                            .clicked()
                        {
                            self.clear_edits_for_paths(&selected);
                            ui.close();
                        }
                    });
                    ui.menu_button("Tools", |ui| {
                        if ui.button("Command Palette...").clicked() {
                            self.show_tool_palette = true;
                            ui.close();
                        }
                        if ui.button("Settings...").clicked() {
                            self.show_export_settings = true;
                            ui.close();
                        }
                        ui.separator();
                        let mcp_on = self.mcp_cmd_rx.is_some();
                        if !mcp_on {
                            if ui.button("Start MCP (stdio)").clicked() {
                                self.start_mcp_from_ui();
                                ui.close();
                            }
                            if ui.button("Start MCP (HTTP)").clicked() {
                                self.start_mcp_http_from_ui();
                                ui.close();
                            }
                        } else {
                            ui.label(
                                RichText::new("MCP: On").color(Color32::from_rgb(120, 220, 140)),
                            );
                        }
                        ui.separator();
                        if ui.button("External Data...").clicked() {
                            self.show_external_dialog = true;
                            ui.close();
                        }
                        if ui.button("Transcript Window...").clicked() {
                            self.show_transcript_window = true;
                            ui.close();
                        }
                        if ui.button("Screenshot (F9)").clicked() {
                            let path = self.default_screenshot_path();
                            self.request_screenshot(ctx, path, false);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Debug Window (F12)").clicked() {
                            self.debug.cfg.enabled = true;
                            self.debug.show_window = !self.debug.show_window;
                            ui.close();
                        }
                        if ui.button("Run Checks").clicked() {
                            self.debug.cfg.enabled = true;
                            self.debug_check_invariants();
                            ui.close();
                        }
                    });
                    ui.separator();
                    ui.label("Volume (dB)");
                    if ui
                        .add(egui::Slider::new(&mut self.volume_db, -80.0..=6.0))
                        .changed()
                    {
                        self.apply_effective_volume();
                    }
                    let total_vis = self.files.len();
                    let total_all = self.items.len();
                    let dirty_gains = self.pending_gain_count();
                    let has_status = total_all > 0 || dirty_gains > 0;
                    if has_status {
                        ui.separator();
                        if total_all > 0 {
                            let scanning = self.scan_in_progress;
                            let loading = scanning || !self.meta_inflight.is_empty();
                            let label = if self.search_query.is_empty() {
                                if loading {
                                    format!("Files: {} ?", total_all)
                                } else {
                                    format!("Files: {}", total_all)
                                }
                            } else {
                                if loading {
                                    format!("Files: {} / {} ?", total_vis, total_all)
                                } else {
                                    format!("Files: {} / {}", total_vis, total_all)
                                }
                            };
                            ui.label(RichText::new(label).monospace());
                        }
                        if dirty_gains > 0 {
                            ui.separator();
                            ui.label(
                                RichText::new(format!("Unsaved Gains: {}", dirty_gains)).weak(),
                            );
                        }
                    }
                    let show_activity = self.scan_in_progress
                        || self.processing.is_some()
                        || self.editor_decode_state.is_some()
                        || self.heavy_preview_rx.is_some()
                        || self.heavy_overlay_rx.is_some()
                        || self.editor_apply_state.is_some()
                        || self.export_state.is_some()
                        || !self.spectro_inflight.is_empty()
                        || self.project_open_state.is_some();
                    if show_activity {
                        ui.separator();
                        ui.horizontal_wrapped(|ui| {
                            if self.scan_in_progress {
                                let elapsed = self
                                    .scan_started_at
                                    .map(|t| t.elapsed().as_secs_f32())
                                    .unwrap_or(0.0);
                                ui.add(egui::Spinner::new());
                                ui.label(
                                    RichText::new(format!(
                                        "Scanning: {} files ({:.1}s)",
                                        self.scan_found_count, elapsed
                                    ))
                                    .weak(),
                                );
                            }
                            if let Some(proc) = &self.processing {
                                if proc.started_at.elapsed() >= Duration::from_millis(120) {
                                    ui.add(egui::Spinner::new());
                                    ui.label(RichText::new(proc.msg.as_str()).weak());
                                    if ui.button("Cancel").clicked() {
                                        self.cancel_processing();
                                    }
                                }
                            }
                            if let Some(state) = &self.editor_decode_state {
                                if state.started_at.elapsed() >= Duration::from_millis(120) {
                                    let (msg, progress) = if state.partial_ready {
                                        ("Loading full audio", 0.65f32)
                                    } else {
                                        ("Decoding preview", 0.25f32)
                                    };
                                    ui.add(egui::Spinner::new());
                                    ui.label(RichText::new(msg).weak());
                                    ui.add(
                                        egui::ProgressBar::new(progress)
                                            .desired_width(60.0)
                                            .show_percentage(),
                                    );
                                    if ui.button("Cancel").clicked() {
                                        self.cancel_editor_decode();
                                    }
                                }
                            }
                            if let Some(t) = &self.heavy_preview_tool {
                                ui.add(egui::Spinner::new());
                                let msg = match t {
                                    ToolKind::PitchShift => "Previewing PitchShift",
                                    ToolKind::TimeStretch => "Previewing TimeStretch",
                                    _ => "Previewing",
                                };
                                ui.label(RichText::new(msg).weak());
                                if ui.button("Cancel").clicked() {
                                    self.cancel_heavy_preview();
                                }
                            } else if self.heavy_preview_rx.is_some()
                                || self.heavy_overlay_rx.is_some()
                            {
                                ui.add(egui::Spinner::new());
                                ui.label(RichText::new("Previewing...").weak());
                                if ui.button("Cancel").clicked() {
                                    self.cancel_heavy_preview();
                                }
                            }
                            if let Some(apply) = &self.editor_apply_state {
                                ui.add(egui::Spinner::new());
                                ui.label(RichText::new(apply.msg.as_str()).weak());
                                if ui.button("Cancel").clicked() {
                                    self.cancel_editor_apply();
                                }
                            }
                            if let Some(exp) = &self.export_state {
                                ui.add(egui::Spinner::new());
                                ui.label(RichText::new(exp.msg.as_str()).weak());
                            }
                            if !self.spectro_inflight.is_empty() {
                                ui.add(egui::Spinner::new());
                                let mut done = 0usize;
                                let mut total = 0usize;
                                for progress in self.spectro_progress.values() {
                                    done = done.saturating_add(progress.done_tiles);
                                    total = total.saturating_add(progress.total_tiles);
                                }
                                let label = if total > 0 {
                                    let pct =
                                        ((done as f32 / total as f32) * 100.0).clamp(0.0, 100.0);
                                    format!(
                                        "Spectrogram: {} ({pct:.0}%)",
                                        self.spectro_inflight.len()
                                    )
                                } else {
                                    format!("Spectrogram: {}", self.spectro_inflight.len())
                                };
                                ui.label(RichText::new(label).weak());
                                if ui.button("Cancel").clicked() {
                                    self.cancel_all_spectrograms();
                                }
                            }
                            if let Some(state) = &self.project_open_state {
                                let elapsed = state.started_at.elapsed().as_secs_f32();
                                ui.add(egui::Spinner::new());
                                ui.label(
                                    RichText::new(format!(
                                        "Opening project... ({elapsed:.1}s)"
                                    ))
                                    .weak(),
                                );
                            }
                        });
                    }
                    ui.separator();
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let db = self.meter_db;
                        let bar_w = 200.0;
                        let bar_h = 16.0;
                        let (rect, painter) =
                            ui.allocate_painter(egui::vec2(bar_w, bar_h), Sense::hover());
                        painter.rect_stroke(
                            rect.rect,
                            2.0,
                            egui::Stroke::new(1.0, Color32::GRAY),
                            egui::StrokeKind::Inside,
                        );
                        let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                        let fill = egui::Rect::from_min_size(
                            rect.rect.min,
                            egui::vec2(bar_w * norm, bar_h),
                        );
                        painter.rect_filled(fill, 0.0, Color32::from_rgb(100, 220, 120));
                        let db_label = if db <= -79.9 {
                            "-inf dBFS".to_string()
                        } else {
                            format!("{db:.1} dBFS")
                        };
                        ui.label(RichText::new(db_label).monospace());
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    // Mode: segmented + compact numeric control (DragValue)
                    ui.scope(|ui| {
                        let s = ui.style_mut();
                        s.spacing.item_spacing.x = 6.0;
                        s.spacing.button_padding = egui::vec2(4.0, 2.0);
                        ui.label("Mode");
                        let prev_mode = self.mode;
                        for (m, label) in [
                            (RateMode::Speed, "Speed"),
                            (RateMode::PitchShift, "Pitch"),
                            (RateMode::TimeStretch, "Stretch"),
                        ] {
                            if ui.selectable_label(self.mode == m, label).clicked() {
                                self.mode = m;
                            }
                        }
                        if self.mode != prev_mode {
                            match self.mode {
                                RateMode::Speed => {
                                    self.audio.set_rate(self.playback_rate);
                                }
                                _ => {
                                    self.audio.set_rate(1.0);
                                    self.rebuild_current_buffer_with_mode();
                                }
                            }
                        }
                        match self.mode {
                            RateMode::Speed => {
                                let resp = ui.add(
                                    egui::DragValue::new(&mut self.playback_rate)
                                        .range(0.25..=4.0)
                                        .speed(0.05)
                                        .fixed_decimals(2)
                                        .suffix(" x"),
                                );
                                if resp.changed() {
                                    self.audio.set_rate(self.playback_rate);
                                }
                                if resp.drag_stopped() {
                                    resp.surrender_focus();
                                }
                                if resp.has_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
                                    self.suppress_list_enter = true;
                                    resp.surrender_focus();
                                }
                            }
                            RateMode::PitchShift => {
                                let resp = ui.add(
                                    egui::DragValue::new(&mut self.pitch_semitones)
                                        .range(-12.0..=12.0)
                                        .speed(0.1)
                                        .fixed_decimals(1)
                                        .suffix(" st"),
                                );
                                if resp.changed() {
                                    self.audio.set_rate(1.0);
                                    self.rebuild_current_buffer_with_mode();
                                }
                                if resp.drag_stopped() {
                                    resp.surrender_focus();
                                }
                                if resp.has_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
                                    self.suppress_list_enter = true;
                                    resp.surrender_focus();
                                }
                            }
                            RateMode::TimeStretch => {
                                let resp = ui.add(
                                    egui::DragValue::new(&mut self.playback_rate)
                                        .range(0.25..=4.0)
                                        .speed(0.05)
                                        .fixed_decimals(2)
                                        .suffix(" x"),
                                );
                                if resp.changed() {
                                    self.audio.set_rate(1.0);
                                    self.rebuild_current_buffer_with_mode();
                                }
                                if resp.drag_stopped() {
                                    resp.surrender_focus();
                                }
                                if resp.has_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
                                    self.suppress_list_enter = true;
                                    resp.surrender_focus();
                                }
                            }
                        }
                    });
                    ui.separator();
                    let play_text = if self
                        .audio
                        .shared
                        .playing
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        "Pause (Space)"
                    } else {
                        "Play (Space)"
                    };
                    if ui
                        .add_sized(egui::vec2(110.0, 22.0), egui::Button::new(play_text))
                        .clicked()
                    {
                        self.audio.toggle_play();
                    }
                    ui.checkbox(&mut self.auto_play_list_nav, "Auto Play");
                    ui.separator();
                    let regex_changed = ui.checkbox(&mut self.search_use_regex, "Regex").changed();
                    let te =
                        egui::TextEdit::singleline(&mut self.search_query).hint_text("Search...");
                    let resp = ui.add(te);
                    if resp.changed() {
                        self.schedule_search_refresh();
                    }
                    if regex_changed {
                        self.apply_filter_from_search();
                        if self.sort_dir != crate::app::types::SortDir::None {
                            self.apply_sort();
                        }
                        self.search_dirty = false;
                        self.search_deadline = None;
                    }
                    if resp.has_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
                        self.apply_filter_from_search();
                        if self.sort_dir != crate::app::types::SortDir::None {
                            self.apply_sort();
                        }
                        self.search_dirty = false;
                        self.search_deadline = None;
                    }
                    if !self.search_query.is_empty() {
                        if ui.button("x").on_hover_text("Clear").clicked() {
                            self.search_query.clear();
                            self.apply_filter_from_search();
                            if self.sort_dir != crate::app::types::SortDir::None {
                                self.apply_sort();
                            }
                            self.search_dirty = false;
                            self.search_deadline = None;
                        }
                    }
                });
            });
        });
    }
}
