use egui::{Align, RichText, Sense};
use egui_extras::TableBuilder;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_list_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use crate::app::helpers::{
            amp_to_color, db_to_amp, db_to_color, format_duration, sortable_header,
        };
        use crate::app::types::SortKey;
        use std::path::PathBuf;

        let mut to_open: Option<PathBuf> = None;
        let text_height = egui::TextStyle::Body.resolve(ui.style()).size;
        let header_h = text_height * 1.6;
        let row_h = self.wave_row_h.max(text_height * 1.3);
        let avail_h = ui.available_height();
        let visible_rows = ((avail_h - header_h) / row_h).floor().max(1.0) as usize;
        ui.set_min_width(ui.available_width());
        let row_count = self.files.len().max(12);
        let mut dirty_paths: std::collections::HashSet<PathBuf> = self
            .tabs
            .iter()
            .filter(|t| t.dirty || t.loop_markers_dirty)
            .map(|t| t.path.clone())
            .collect();
        dirty_paths.extend(self.edited_cache.keys().cloned());

        let mut key_moved = false;
        // Keyboard navigation & per-file gain adjust in list view
        if self.active_tab.is_none() && !self.files.is_empty() {
            let pressed_down = ctx.input(|i| i.key_pressed(egui::Key::ArrowDown));
            let pressed_up = ctx.input(|i| i.key_pressed(egui::Key::ArrowUp));
            let pressed_enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
            let pressed_ctrl_a = ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::A));
            let pressed_left = ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft));
            let pressed_right = ctx.input(|i| i.key_pressed(egui::Key::ArrowRight));
            let pressed_pgdown = ctx.input(|i| i.key_pressed(egui::Key::PageDown));
            let pressed_pgup = ctx.input(|i| i.key_pressed(egui::Key::PageUp));
            let pressed_home = ctx.input(|i| i.key_pressed(egui::Key::Home));
            let pressed_end = ctx.input(|i| i.key_pressed(egui::Key::End));
            if pressed_ctrl_a {
                self.selected_multi.clear();
                for i in 0..self.files.len() {
                    self.selected_multi.insert(i);
                }
                if self.selected.is_none() {
                    self.selected = Some(0);
                }
            }
            if pressed_home || pressed_end {
                let len = self.files.len();
                let target = if pressed_home { 0 } else { len.saturating_sub(1) };
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            } else if pressed_pgdown || pressed_pgup {
                let len = self.files.len();
                let cur = self.selected.unwrap_or(0);
                let target = if pressed_pgdown {
                    (cur + visible_rows).min(len.saturating_sub(1))
                } else {
                    cur.saturating_sub(visible_rows)
                };
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            } else if pressed_down || pressed_up {
                let len = self.files.len();
                let cur = self.selected.unwrap_or(0);
                let target = if pressed_down {
                    (cur + 1).min(len.saturating_sub(1))
                } else {
                    cur.saturating_sub(1)
                };
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            }
            if pressed_enter {
                if let Some(i) = self.selected {
                    if let Some(p) = self.path_for_row(i).cloned() {
                        self.open_or_activate_tab(&p);
                    }
                }
            }

            // Per-file Gain(dB) adjust: Left/Right arrows
            if pressed_left || pressed_right {
                let mods = ctx.input(|i| i.modifiers);
                let step = if mods.ctrl {
                    3.0
                } else if mods.shift {
                    1.0
                } else {
                    0.1
                };
                let delta = if pressed_left { -step } else { step };
                let mut indices = self.selected_multi.clone();
                if indices.is_empty() {
                    if let Some(i) = self.selected {
                        indices.insert(i);
                    }
                }
                if !indices.is_empty() {
                    self.adjust_gain_for_indices(&indices, delta);
                }
            }
        }

        let mut sort_changed = false;
        let mut missing_paths: Vec<PathBuf> = Vec::new();
        let list_rect = ui.available_rect_before_wrap();
        let pointer_over_list = ui
            .input(|i| i.pointer.hover_pos())
            .map_or(false, |p| list_rect.contains(p));
        let wheel_raw = ctx.input(|i| i.raw_scroll_delta);
        if pointer_over_list && wheel_raw != egui::Vec2::ZERO {
            self.last_list_scroll_at = Some(std::time::Instant::now());
        }
        let allow_auto_scroll = self.scroll_to_selected
            && (key_moved
                || self
                    .last_list_scroll_at
                    .map_or(true, |t| t.elapsed() > std::time::Duration::from_millis(300)));
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .auto_shrink([false, true])
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(Align::Center))
            .column(egui_extras::Column::initial(200.0).resizable(true))
            .column(egui_extras::Column::initial(250.0).resizable(true))
            .column(egui_extras::Column::initial(60.0).resizable(true))
            .column(egui_extras::Column::initial(40.0).resizable(true))
            .column(egui_extras::Column::initial(70.0).resizable(true))
            .column(egui_extras::Column::initial(50.0).resizable(true))
            .column(egui_extras::Column::initial(90.0).resizable(true))
            .column(egui_extras::Column::initial(90.0).resizable(true))
            .column(egui_extras::Column::initial(80.0).resizable(true))
            .column(egui_extras::Column::initial(150.0).resizable(true))
            .column(egui_extras::Column::remainder())
            .min_scrolled_height((avail_h - header_h).max(0.0));
        if allow_auto_scroll {
            if let Some(sel) = self.selected {
                if sel < row_count {
                    table = table.scroll_to_row(sel, Some(Align::Center));
                    self.scroll_to_selected = false;
                }
            }
        }

        table
            .header(header_h, |mut header| {
                header.col(|ui| {
                    sort_changed |= sortable_header(
                        ui,
                        "File",
                        &mut self.sort_key,
                        &mut self.sort_dir,
                        SortKey::File,
                        true,
                    );
                });
                header.col(|ui| {
                    sort_changed |= sortable_header(
                        ui,
                        "Folder",
                        &mut self.sort_key,
                        &mut self.sort_dir,
                        SortKey::Folder,
                        true,
                    );
                });
                header.col(|ui| {
                    sort_changed |= sortable_header(
                        ui,
                        "Length",
                        &mut self.sort_key,
                        &mut self.sort_dir,
                        SortKey::Length,
                        true,
                    );
                });
                header.col(|ui| {
                    sort_changed |= sortable_header(
                        ui,
                        "Ch",
                        &mut self.sort_key,
                        &mut self.sort_dir,
                        SortKey::Channels,
                        true,
                    );
                });
                header.col(|ui| {
                    sort_changed |= sortable_header(
                        ui,
                        "SR",
                        &mut self.sort_key,
                        &mut self.sort_dir,
                        SortKey::SampleRate,
                        true,
                    );
                });
                header.col(|ui| {
                    sort_changed |= sortable_header(
                        ui,
                        "Bits",
                        &mut self.sort_key,
                        &mut self.sort_dir,
                        SortKey::Bits,
                        true,
                    );
                });
                header.col(|ui| {
                    sort_changed |= sortable_header(
                        ui,
                        "dBFS (Peak)",
                        &mut self.sort_key,
                        &mut self.sort_dir,
                        SortKey::Level,
                        false,
                    );
                });
                header.col(|ui| {
                    sort_changed |= sortable_header(
                        ui,
                        "LUFS (I)",
                        &mut self.sort_key,
                        &mut self.sort_dir,
                        SortKey::Lufs,
                        false,
                    );
                });
                header.col(|ui| {
                    ui.label(RichText::new("Gain (dB)").strong());
                });
                header.col(|ui| {
                    ui.label(RichText::new("Wave").strong());
                });
                header.col(|_ui| {});
            })
            .body(|body| {
                body.rows(row_h, row_count, |mut row| {
                    let row_idx = row.index();
                    if row_idx < self.files.len() {
                        let file_idx = self.files[row_idx];
                        let path_owned = match self.all_files.get(file_idx) {
                            Some(p) => p.clone(),
                            None => return,
                        };
                        if !path_owned.is_file() {
                            missing_paths.push(path_owned.clone());
                            return;
                        }
                        self.queue_meta_for_path(&path_owned, true);
                        let file_name = path_owned
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("");
                        let parent = path_owned.parent().and_then(|p| p.to_str()).unwrap_or("");
                        let is_selected = self.selected_multi.contains(&row_idx);
                        row.set_selected(is_selected);
                        let mut clicked_to_load = false;
                        let mut clicked_to_select = false;
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let mut display = file_name.to_string();
                                    if dirty_paths.contains(&path_owned) {
                                        display.push_str(" *");
                                    }
                                    if self
                                        .pending_gains
                                        .get(&path_owned)
                                        .map(|v| v.abs() > 0.0001)
                                        .unwrap_or(false)
                                    {
                                        display.push_str(" •");
                                    }
                                    let resp = ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(display)
                                                    .monospace()
                                                    .size(text_height * 1.0),
                                            )
                                            .sense(Sense::click())
                                            .truncate()
                                            .show_tooltip_when_elided(false),
                                        )
                                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                                    if resp.clicked_by(egui::PointerButton::Primary)
                                        && !resp.double_clicked()
                                    {
                                        clicked_to_load = true;
                                    }
                                    if resp.double_clicked() {
                                        clicked_to_select = true;
                                        to_open = Some(path_owned.clone());
                                    }
                                    if resp.hovered() {
                                        resp.on_hover_text(file_name);
                                    }
                                },
                            );
                        });
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let resp = ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(parent)
                                                    .monospace()
                                                    .size(text_height * 1.0),
                                            )
                                            .sense(Sense::click())
                                            .truncate()
                                            .show_tooltip_when_elided(false),
                                        )
                                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                                    if resp.clicked_by(egui::PointerButton::Primary)
                                        && !resp.double_clicked()
                                    {
                                        clicked_to_load = true;
                                    }
                                    if resp.double_clicked() {
                                        clicked_to_select = true;
                                        let _ = crate::app::helpers::open_folder_with_file_selected(
                                            &path_owned,
                                        );
                                    }
                                    if resp.hovered() {
                                        resp.on_hover_text(parent);
                                    }
                                },
                            );
                        });
                        row.col(|ui| {
                            let secs = self
                                .meta
                                .get(&path_owned)
                                .and_then(|m| m.duration_secs)
                                .unwrap_or(f32::NAN);
                            let text = if secs.is_finite() {
                                format_duration(secs)
                            } else {
                                "...".into()
                            };
                            let resp = ui
                                .add(
                                    egui::Label::new(RichText::new(text).monospace())
                                        .sense(Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if resp.clicked_by(egui::PointerButton::Primary) {
                                clicked_to_load = true;
                            }
                        });
                        row.col(|ui| {
                            let ch = self.meta.get(&path_owned).map(|m| m.channels).unwrap_or(0);
                            let resp = ui
                                .add(
                                    egui::Label::new(RichText::new(format!("{}", ch)).monospace())
                                        .sense(Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if resp.clicked_by(egui::PointerButton::Primary) {
                                clicked_to_load = true;
                            }
                        });
                        row.col(|ui| {
                            let sr = self
                                .meta
                                .get(&path_owned)
                                .map(|m| m.sample_rate)
                                .unwrap_or(0);
                            let resp = ui
                                .add(
                                    egui::Label::new(RichText::new(format!("{}", sr)).monospace())
                                        .sense(Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if resp.clicked_by(egui::PointerButton::Primary) {
                                clicked_to_load = true;
                            }
                        });
                        row.col(|ui| {
                            let bits = self
                                .meta
                                .get(&path_owned)
                                .map(|m| m.bits_per_sample)
                                .unwrap_or(0);
                            let resp = ui
                                .add(
                                    egui::Label::new(
                                        RichText::new(format!("{}", bits)).monospace(),
                                    )
                                    .sense(Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if resp.clicked_by(egui::PointerButton::Primary) {
                                clicked_to_load = true;
                            }
                        });
                        row.col(|ui| {
                            let (rect2, resp2) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_h * 0.9),
                                Sense::click(),
                            );
                            let gain_db = *self.pending_gains.get(&path_owned).unwrap_or(&0.0);
                            let orig = self.meta.get(&path_owned).and_then(|m| m.peak_db);
                            let adj = orig.map(|db| db + gain_db);
                            if let Some(db) = adj {
                                ui.painter().rect_filled(rect2, 4.0, db_to_color(db));
                            }
                            let text = adj
                                .map(|db| format!("{:.1}", db))
                                .unwrap_or_else(|| "...".into());
                            let fid = egui::TextStyle::Monospace.resolve(ui.style());
                            ui.painter().text(
                                rect2.center(),
                                egui::Align2::CENTER_CENTER,
                                text,
                                fid,
                                egui::Color32::WHITE,
                            );
                            if resp2.clicked_by(egui::PointerButton::Primary) {
                                clicked_to_load = true;
                            }
                        });
                        row.col(|ui| {
                            let base = self.meta.get(&path_owned).and_then(|m| m.lufs_i);
                            let gain_db = *self.pending_gains.get(&path_owned).unwrap_or(&0.0);
                            let eff = if let Some(v) = self.lufs_override.get(&path_owned) {
                                Some(*v)
                            } else {
                                base.map(|v| v + gain_db)
                            };
                            let (rect2, resp2) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_h * 0.9),
                                Sense::click(),
                            );
                            if let Some(db) = eff {
                                ui.painter().rect_filled(rect2, 4.0, db_to_color(db));
                            }
                            let text = eff
                                .map(|v| format!("{:.1}", v))
                                .unwrap_or_else(|| "...".into());
                            let fid = egui::TextStyle::Monospace.resolve(ui.style());
                            ui.painter().text(
                                rect2.center(),
                                egui::Align2::CENTER_CENTER,
                                text,
                                fid,
                                egui::Color32::WHITE,
                            );
                            if resp2.clicked_by(egui::PointerButton::Primary) {
                                clicked_to_load = true;
                            }
                        });
                        row.col(|ui| {
                            let old = *self.pending_gains.get(&path_owned).unwrap_or(&0.0);
                            let mut g = old;
                            let resp = ui.add(
                                egui::DragValue::new(&mut g)
                                    .range(-24.0..=24.0)
                                    .speed(0.1)
                                    .fixed_decimals(1)
                                    .suffix(" dB"),
                            );
                            if resp.changed() {
                                let new = crate::app::WavesPreviewer::clamp_gain_db(g);
                                let delta = new - old;
                                if self.selected_multi.len() > 1
                                    && self.selected_multi.contains(&row_idx)
                                {
                                    let indices = self.selected_multi.clone();
                                    self.adjust_gain_for_indices(&indices, delta);
                                } else {
                                    if new == 0.0 {
                                        self.pending_gains.remove(&path_owned);
                                    } else {
                                        self.pending_gains.insert(path_owned.clone(), new);
                                    }
                                    if self.playing_path.as_ref() == Some(&path_owned) {
                                        self.apply_effective_volume();
                                    }
                                    self.schedule_lufs_for_path(path_owned.clone());
                                }
                            }
                        });
                        row.col(|ui| {
                            let (rect2, _resp2) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_h * 0.9),
                                Sense::hover(),
                            );
                            let error_text = self
                                .meta
                                .get(&path_owned)
                                .and_then(|m| m.decode_error.as_deref());
                            let (wave_rect, error_rect) = if error_text.is_some() {
                                let err_max = (rect2.height() * 0.45).max(8.0);
                                let mut err_h = (row_h * 0.36).max(8.0);
                                if err_h > err_max {
                                    err_h = err_max;
                                }
                                let wave_h = (rect2.height() - err_h).max(1.0);
                                let wave_rect = egui::Rect::from_min_size(
                                    rect2.min,
                                    egui::vec2(rect2.width(), wave_h),
                                );
                                let error_rect = egui::Rect::from_min_size(
                                    egui::pos2(rect2.min.x, rect2.max.y - err_h),
                                    egui::vec2(rect2.width(), err_h),
                                );
                                (wave_rect, Some(error_rect))
                            } else {
                                (rect2, None)
                            };
                            if let Some(m) = self.meta.get(&path_owned) {
                                let w = wave_rect.width();
                                let h = wave_rect.height();
                                let n = m.thumb.len().max(1) as f32;
                                let gain_db = *self.pending_gains.get(&path_owned).unwrap_or(&0.0);
                                let scale = db_to_amp(gain_db);
                                for (idx, &(mn0, mx0)) in m.thumb.iter().enumerate() {
                                    let mn = (mn0 * scale).clamp(-1.0, 1.0);
                                    let mx = (mx0 * scale).clamp(-1.0, 1.0);
                                    let x = wave_rect.left() + (idx as f32 / n) * w;
                                    let y0 = wave_rect.center().y - mx * (h * 0.45);
                                    let y1 = wave_rect.center().y - mn * (h * 0.45);
                                    let a = (mn.abs().max(mx.abs())).clamp(0.0, 1.0);
                                    let col = amp_to_color(a);
                                    ui.painter().line_segment(
                                        [egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))],
                                        egui::Stroke::new(1.0, col),
                                    );
                                }
                            }
                            if let (Some(text), Some(err_rect)) = (error_text, error_rect) {
                                let text_pos = egui::pos2(err_rect.left() + 4.0, err_rect.center().y);
                                let mut font_size = text_height * 0.85;
                                if font_size < 10.0 {
                                    font_size = 10.0;
                                }
                                if font_size > err_rect.height() {
                                    font_size = err_rect.height();
                                }
                                let font = egui::FontId::proportional(font_size);
                                ui.painter().text(
                                    text_pos,
                                    egui::Align2::LEFT_CENTER,
                                    text,
                                    font,
                                    egui::Color32::from_rgb(220, 90, 90),
                                );
                            }
                        });
                        // row-level interaction (must call response() after at least one col())
                        let resp = row.response();
                        if resp.secondary_clicked() && !self.selected_multi.contains(&row_idx) {
                            let mods = ctx.input(|i| i.modifiers);
                            self.update_selection_on_click(row_idx, mods);
                        }
                        resp.context_menu(|ui| {
                            let selected = self.selected_paths();
                            let has_selection = !selected.is_empty();
                            if ui
                                .add_enabled(has_selection, egui::Button::new("Copy..."))
                                .clicked()
                            {
                                if let Some(dir) = self.pick_folder_dialog() {
                                    self.copy_paths_to_folder(&selected, &dir);
                                }
                                ui.close();
                            }
                            if selected.len() == 1 {
                                if ui.button("Rename...").clicked() {
                                    self.open_rename_dialog(selected[0].clone());
                                    ui.close();
                                }
                            }
                            if ui
                                .add_enabled(has_selection, egui::Button::new("Delete..."))
                                .clicked()
                            {
                                self.open_delete_confirm(selected.clone());
                                ui.close();
                            }
                            let has_edits = self.has_edits_for_paths(&selected);
                            if ui
                                .add_enabled(has_edits, egui::Button::new("Clear Edits"))
                                .clicked()
                            {
                                self.clear_edits_for_paths(&selected);
                                ui.close();
                            }
                        });
                        let clicked_any =
                            resp.clicked_by(egui::PointerButton::Primary) || clicked_to_load;
                        if clicked_any {
                            let mods = ctx.input(|i| i.modifiers);
                            self.update_selection_on_click(row_idx, mods);
                            self.select_and_load(row_idx, false);
                        } else if clicked_to_select {
                            self.selected = Some(row_idx);
                            self.scroll_to_selected = false;
                            self.selected_multi.clear();
                            self.selected_multi.insert(row_idx);
                            self.select_anchor = Some(row_idx);
                        }
                    } else {
                        // filler
                        for _ in 0..11 {
                            row.col(|ui| {
                                let _ = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::hover(),
                                );
                            });
                        }
                    }
                });
            });

        if !missing_paths.is_empty() {
            for p in missing_paths {
                self.remove_missing_path(&p);
            }
        }
        if sort_changed {
            self.apply_sort();
        }
        if let Some(p) = to_open.as_ref() {
            self.open_or_activate_tab(p);
        }

        // keyboard handling moved above table to allow same-frame auto-scroll
    }
}
