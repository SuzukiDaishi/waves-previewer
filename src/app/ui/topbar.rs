use crate::app::types::RateMode;
use egui::{Align, Color32, Key, RichText, Sense};

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.menu_button("Choose", |ui| {
                    if ui.button("Folder...").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.root = Some(dir);
                            self.rescan();
                        }
                        ui.close_menu();
                    }
                    if ui.button("Files...").clicked() {
                        if let Some(files) = rfd::FileDialog::new()
                            .add_filter("WAV", &["wav"])
                            .pick_files()
                        {
                            self.replace_with_files(&files);
                            self.after_add_refresh();
                        }
                        ui.close_menu();
                    }
                });
                ui.menu_button("Export", |ui| {
                    if ui.button("Apply Gains (new files)").clicked() {
                        self.spawn_export_gains(false);
                        ui.close_menu();
                    }
                    if ui.button("Clear All Gains").clicked() {
                        self.pending_gains.clear();
                        self.lufs_override.clear();
                        self.lufs_recalc_deadline.clear();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Save Selected (Ctrl+S)").clicked() {
                        self.trigger_save_selected();
                        ui.close_menu();
                    }
                    if ui.button("Settings...").clicked() {
                        self.show_export_settings = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button("Tools", |ui| {
                    if ui.button("Screenshot (F9)").clicked() {
                        let path = self.default_screenshot_path();
                        self.request_screenshot(ctx, path, false);
                        ui.close_menu();
                    }
                    if ui.button("Open First in Editor").clicked() {
                        self.open_first_in_list();
                        ui.close_menu();
                    }
                    if ui.button("Debug Window (F12)").clicked() {
                        self.debug.cfg.enabled = true;
                        self.debug.show_window = !self.debug.show_window;
                        ui.close_menu();
                    }
                    if ui.button("Run Checks").clicked() {
                        self.debug.cfg.enabled = true;
                        self.debug_check_invariants();
                        ui.close_menu();
                    }
                });
                // Files total + loading indicator
                let total_vis = self.files.len();
                let total_all = self.all_files.len();
                if total_all > 0 {
                    let scanning = self.scan_in_progress;
                    let loading = scanning || !self.meta_inflight.is_empty();
                    let label = if self.search_query.is_empty() {
                        if loading {
                            format!("Files: {} ⏳", total_all)
                        } else {
                            format!("Files: {}", total_all)
                        }
                    } else {
                        if loading {
                            format!("Files: {} / {} ⏳", total_vis, total_all)
                        } else {
                            format!("Files: {} / {}", total_vis, total_all)
                        }
                    };
                    ui.label(RichText::new(label).monospace());
                }
                ui.separator();
                let dirty_gains = self
                    .pending_gains
                    .iter()
                    .filter(|(_, v)| v.abs() > 0.0001)
                    .count();
                if dirty_gains > 0 {
                    ui.label(RichText::new(format!("Unsaved Gains: {}", dirty_gains)).weak());
                }
                ui.separator();
                ui.label("Volume (dB)");
                if ui
                    .add(egui::Slider::new(&mut self.volume_db, -80.0..=6.0))
                    .changed()
                {
                    self.apply_effective_volume();
                }
                ui.separator();
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
                                    .clamp_range(0.25..=4.0)
                                    .speed(0.05)
                                    .fixed_decimals(2)
                                    .suffix(" x"),
                            );
                            if resp.changed() {
                                self.audio.set_rate(self.playback_rate);
                            }
                        }
                        RateMode::PitchShift => {
                            let resp = ui.add(
                                egui::DragValue::new(&mut self.pitch_semitones)
                                    .clamp_range(-12.0..=12.0)
                                    .speed(0.1)
                                    .fixed_decimals(1)
                                    .suffix(" st"),
                            );
                            if resp.changed() {
                                self.audio.set_rate(1.0);
                                self.rebuild_current_buffer_with_mode();
                            }
                        }
                        RateMode::TimeStretch => {
                            let resp = ui.add(
                                egui::DragValue::new(&mut self.playback_rate)
                                    .clamp_range(0.25..=4.0)
                                    .speed(0.05)
                                    .fixed_decimals(2)
                                    .suffix(" x"),
                            );
                            if resp.changed() {
                                self.audio.set_rate(1.0);
                                self.rebuild_current_buffer_with_mode();
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
                if ui.button(play_text).clicked() {
                    self.audio.toggle_play();
                }
                ui.separator();
                // Search bar
                let te = egui::TextEdit::singleline(&mut self.search_query).hint_text("Search...");
                let resp = ui.add(te);
                if resp.changed() {
                    self.schedule_search_refresh();
                }
                if resp.has_focus() && ctx.input(|i| i.key_pressed(Key::Enter)) {
                    self.apply_filter_from_search();
                    self.apply_sort();
                    self.search_dirty = false;
                    self.search_deadline = None;
                }
                if !self.search_query.is_empty() {
                    if ui.button("x").on_hover_text("Clear").clicked() {
                        self.search_query.clear();
                        self.apply_filter_from_search();
                        self.apply_sort();
                        self.search_dirty = false;
                        self.search_deadline = None;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let db = self.meter_db;
                    let bar_w = 200.0;
                    let bar_h = 16.0;
                    let (rect, painter) =
                        ui.allocate_painter(egui::vec2(bar_w, bar_h), Sense::hover());
                    painter.rect_stroke(rect.rect, 2.0, egui::Stroke::new(1.0, Color32::GRAY));
                    let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                    let fill =
                        egui::Rect::from_min_size(rect.rect.min, egui::vec2(bar_w * norm, bar_h));
                    painter.rect_filled(fill, 0.0, Color32::from_rgb(100, 220, 120));
                    ui.label(RichText::new(format!("{db:.1} dBFS")).monospace());
                });
            });
        });
    }
}
