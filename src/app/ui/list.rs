use egui::{Align, Color32, RichText, Sense};
use egui_extras::TableBuilder;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_list_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use crate::app::helpers::{
            amp_to_color, db_to_amp, db_to_color, format_duration, format_system_time_local,
            highlight_text_job, sortable_header,
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
        let cols = self.list_columns;
        let external_cols = if cols.external {
            self.external_visible_columns.clone()
        } else {
            Vec::new()
        };
        let list_rect = ui.available_rect_before_wrap();
        let pointer_over_list = ui
            .input(|i| i.pointer.hover_pos())
            .map_or(false, |p| list_rect.contains(p));
        if self.debug.cfg.enabled {
            self.debug.last_pointer_over_list = pointer_over_list;
        }
        let list_focus_id = crate::app::WavesPreviewer::list_focus_id();
        let focus_resp = ui.interact(list_rect, list_focus_id, Sense::click());
        if self.list_has_focus
            && !ctx.memory(|m| m.has_focus(list_focus_id))
            && !ctx.wants_keyboard_input()
        {
            ctx.memory_mut(|m| m.request_focus(list_focus_id));
        }
        if focus_resp.clicked() {
            ctx.memory_mut(|m| m.request_focus(list_focus_id));
            self.search_has_focus = false;
        }
        let mut list_has_focus =
            ctx.memory(|m| m.has_focus(list_focus_id)) || self.list_has_focus;
        if !list_has_focus
            && self.active_tab.is_none()
            && self.selected.is_some()
            && !self.search_has_focus
            && !ctx.wants_keyboard_input()
        {
            ctx.memory_mut(|m| m.request_focus(list_focus_id));
            list_has_focus = true;
            self.list_has_focus = true;
        }
        let mut key_moved = false;
        // Keyboard navigation & per-file gain adjust in list view
        let wants_kb = ctx.wants_keyboard_input();
        let mut list_key_intent = false;
        if self.active_tab.is_none() && !self.files.is_empty() && !wants_kb {
            list_key_intent = ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowDown)
                    || i.key_pressed(egui::Key::ArrowUp)
                    || i.key_pressed(egui::Key::Enter)
                    || i.key_pressed(egui::Key::ArrowLeft)
                    || i.key_pressed(egui::Key::ArrowRight)
                    || i.key_pressed(egui::Key::PageDown)
                    || i.key_pressed(egui::Key::PageUp)
                    || i.key_pressed(egui::Key::Home)
                    || i.key_pressed(egui::Key::End)
                    || i.key_pressed(egui::Key::Delete)
                    || ((i.modifiers.ctrl || i.modifiers.command)
                        && i.key_pressed(egui::Key::A))
            });
            if !list_has_focus
                && list_key_intent
                && (pointer_over_list || self.selected.is_some() || self.list_has_focus)
            {
                ctx.memory_mut(|m| m.request_focus(list_focus_id));
                list_has_focus = true;
                self.list_has_focus = true;
            }
        }
        let allow_list_keys = list_has_focus && !wants_kb;
        if self.active_tab.is_none() && !self.files.is_empty() && list_key_intent && !allow_list_keys
        {
            if self.debug.cfg.enabled {
                self.debug_trace_input(format!(
                    "list keys blocked (list_focus={} wants_kb={})",
                    list_has_focus, wants_kb
                ));
            }
        }
        let pressed_down = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
        } else {
            false
        };
        let pressed_up = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
        } else {
            false
        };
        let pressed_enter = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        } else {
            false
        };
        let pressed_ctrl_a = if allow_list_keys {
            ctx.input_mut(|i| {
                i.consume_key(
                    egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
                    egui::Key::A,
                )
            })
        } else {
            false
        };
        let pressed_left = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft))
        } else {
            false
        };
        let pressed_right = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight))
        } else {
            false
        };
        let pressed_pgdown = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown))
        } else {
            false
        };
        let pressed_pgup = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp))
        } else {
            false
        };
        let pressed_home = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Home))
        } else {
            false
        };
        let pressed_end = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::End))
        } else {
            false
        };
        let pressed_delete = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
        } else {
            false
        };
        if self.active_tab.is_none() && !self.files.is_empty() && allow_list_keys {
            if pressed_ctrl_a
                || pressed_home
                || pressed_end
                || pressed_pgdown
                || pressed_pgup
                || pressed_down
                || pressed_up
                || pressed_enter
                || pressed_delete
                || pressed_left
                || pressed_right
            {
                ctx.memory_mut(|m| m.request_focus(list_focus_id));
                list_has_focus = true;
                self.search_has_focus = false;
            }
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
                let target = if pressed_home {
                    0
                } else {
                    len.saturating_sub(1)
                };
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
            if pressed_enter && !self.suppress_list_enter {
                let selected = self.selected_paths();
                if !selected.is_empty() {
                    self.open_paths_in_tabs(&selected);
                }
            }
            if pressed_delete {
                let selected = self.selected_paths();
                if !selected.is_empty() {
                    self.remove_paths_from_list_with_undo(&selected);
                }
            }
            if key_moved && self.auto_play_list_nav {
                self.request_list_autoplay();
            }

            // Per-file Gain(dB) adjust: Left/Right arrows
            if pressed_left || pressed_right {
                let mods = ctx.input(|i| i.modifiers);
                let step = if mods.shift { 0.1 } else { 1.0 };
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
        let wheel_raw = ctx.input(|i| i.raw_scroll_delta);
        if pointer_over_list && wheel_raw != egui::Vec2::ZERO {
            self.last_list_scroll_at = Some(std::time::Instant::now());
        }
        let allow_auto_scroll = self.scroll_to_selected
            && (key_moved
                || self.last_list_scroll_at.map_or(true, |t| {
                    t.elapsed() > std::time::Duration::from_millis(300)
                }));
        let header_dirty = self
            .tabs
            .iter()
            .any(|t| t.dirty || t.loop_markers_dirty || t.markers_dirty)
            || self
                .edited_cache
                .values()
                .any(|c| c.dirty || c.loop_markers_dirty || c.markers_dirty)
            || self
                .items
                .iter()
                .any(|item| item.pending_gain_db.abs() > 0.0001)
            || !self.sample_rate_override.is_empty();
        let mut filler_cols = 0usize;
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .auto_shrink([false, true])
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(Align::Center));
        if cols.edited {
            table = table.column(egui_extras::Column::initial(30.0).resizable(false)); // Status column
            filler_cols += 1;
        }

        if cols.file {
            table = table.column(egui_extras::Column::initial(200.0).resizable(true));
            filler_cols += 1;
        }
        if cols.folder {
            table = table.column(egui_extras::Column::initial(250.0).resizable(true));
            filler_cols += 1;
        }
        if cols.transcript {
            table = table.column(egui_extras::Column::initial(280.0).resizable(true));
            filler_cols += 1;
        }
        if cols.external {
            for _ in 0..external_cols.len() {
                table = table.column(egui_extras::Column::initial(140.0).resizable(true));
                filler_cols += 1;
            }
        }
        if cols.length {
            table = table.column(egui_extras::Column::initial(60.0).resizable(true));
            filler_cols += 1;
        }
        if cols.channels {
            table = table.column(egui_extras::Column::initial(40.0).resizable(true));
            filler_cols += 1;
        }
        if cols.sample_rate {
            table = table.column(egui_extras::Column::initial(70.0).resizable(true));
            filler_cols += 1;
        }
        if cols.bits {
            table = table.column(egui_extras::Column::initial(50.0).resizable(true));
            filler_cols += 1;
        }
        if cols.bit_rate {
            table = table.column(egui_extras::Column::initial(70.0).resizable(true));
            filler_cols += 1;
        }
        if cols.peak {
            table = table.column(egui_extras::Column::initial(90.0).resizable(true));
            filler_cols += 1;
        }
        if cols.lufs {
            table = table.column(egui_extras::Column::initial(90.0).resizable(true));
            filler_cols += 1;
        }
        if cols.bpm {
            table = table.column(egui_extras::Column::initial(70.0).resizable(true));
            filler_cols += 1;
        }
        if cols.created_at {
            table = table.column(egui_extras::Column::initial(120.0).resizable(true));
            filler_cols += 1;
        }
        if cols.modified_at {
            table = table.column(egui_extras::Column::initial(120.0).resizable(true));
            filler_cols += 1;
        }
        if cols.gain {
            table = table.column(egui_extras::Column::initial(80.0).resizable(true));
            filler_cols += 1;
        }
        if cols.wave {
            table = table.column(egui_extras::Column::initial(150.0).resizable(true));
            filler_cols += 1;
        }
        table = table
            .column(egui_extras::Column::remainder())
            .min_scrolled_height((avail_h - header_h).max(0.0));
        filler_cols += 1;
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
                if cols.edited {
                    header.col(|ui| {
                        let mut dot = RichText::new("\u{25CF}");
                        if header_dirty {
                            dot = dot.color(Color32::from_rgb(255, 180, 60));
                        } else {
                            dot = dot.weak();
                        }
                        ui.label(dot);
                    });
                }

                if cols.file {
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
                }
                if cols.folder {
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
                }
                if cols.transcript {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Transcript",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Transcript,
                            true,
                        );
                    });
                }
                if cols.external {
                    for (idx, name) in external_cols.iter().enumerate() {
                        header.col(|ui| {
                            sort_changed |= sortable_header(
                                ui,
                                name,
                                &mut self.sort_key,
                                &mut self.sort_dir,
                                SortKey::External(idx),
                                true,
                            );
                        });
                    }
                }
                if cols.length {
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
                }
                if cols.channels {
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
                }
                if cols.sample_rate {
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
                }
                if cols.bits {
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
                }
                if cols.bit_rate {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Bitrate",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::BitRate,
                            true,
                        );
                    });
                }
                if cols.peak {
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
                }
                if cols.lufs {
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
                }
                if cols.bpm {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "BPM",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Bpm,
                            false,
                        );
                    });
                }
                if cols.created_at {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Created",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::CreatedAt,
                            true,
                        );
                    });
                }
                if cols.modified_at {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Modified",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::ModifiedAt,
                            true,
                        );
                    });
                }
                if cols.gain {
                    header.col(|ui| {
                        ui.label(RichText::new("Gain (dB)").strong());
                    });
                }
                if cols.wave {
                    header.col(|ui| {
                        ui.label(RichText::new("Wave").strong());
                    });
                }
                header.col(|_ui| {});
            })
            .body(|body| {
                body.rows(row_h, row_count, |mut row| {
                    let row_idx = row.index();
                    if row_idx < self.files.len() {
                        let id = self.files[row_idx];
                        let (path_owned, file_name, parent, is_virtual) = match self.item_for_id(id) {
                            Some(item) => (
                                item.path.clone(),
                                item.display_name.clone(),
                                item.display_folder.clone(),
                                item.source == crate::app::types::MediaSource::Virtual,
                            ),
                            None => return,
                        };
                        if !is_virtual && !path_owned.is_file() {
                            missing_paths.push(path_owned.clone());
                            return;
                        }
                        if !is_virtual {
                            self.queue_meta_for_path(&path_owned, true);
                            self.queue_transcript_for_path(&path_owned, true);
                        }
                        let Some(item) = self.item_for_id(id) else {
                            return;
                        };
                        let is_selected = self.selected_multi.contains(&row_idx);
                        row.set_selected(is_selected);
                        let mut clicked_to_load = false;
                        let mut clicked_to_select = false;
                        let is_dirty = self.has_edits_for_paths(&[path_owned.clone()]);
                        if cols.edited {
                            row.col(|ui| {
                                if is_dirty {
                                    ui.label(
                                        RichText::new("\u{25CF}")
                                            .color(Color32::from_rgb(255, 180, 60))
                                            .size(text_height * 1.05),
                                    );
                                }
                            });
                        }
                        if cols.file {
                            row.col(|ui| {
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        let display = file_name.clone();
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
                                            resp.on_hover_text(&file_name);
                                        }
                                    },
                                );
                            });
                        }
                        if cols.folder {
                            row.col(|ui| {
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        let resp = ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(parent.as_str())
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
                                            if !is_virtual {
                                                let _ = crate::app::helpers::open_folder_with_file_selected(
                                                    &path_owned,
                                                );
                                            }
                                        }
                                        if resp.hovered() {
                                            resp.on_hover_text(&parent);
                                        }
                                    },
                                );
                            });
                        }
                        if cols.transcript {
                            row.col(|ui| {
                                let transcript_text = item
                                    .transcript
                                    .as_ref()
                                    .map(|t| t.full_text.as_str())
                                    .unwrap_or("");
                                let display = if transcript_text.is_empty()
                                    && self.transcript_inflight.contains(&path_owned)
                                {
                                    "..."
                                } else {
                                    transcript_text
                                };
                                let label = if let Some(job) = highlight_text_job(
                                    display,
                                    &self.search_query,
                                    self.search_use_regex,
                                    ui.style(),
                                ) {
                                    egui::Label::new(job).sense(Sense::click()).truncate()
                                } else {
                                    egui::Label::new(
                                        RichText::new(display).size(text_height * 0.95),
                                    )
                                    .sense(Sense::click())
                                    .truncate()
                                };
                                let resp = ui
                                    .add(label.show_tooltip_when_elided(false))
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked_by(egui::PointerButton::Primary)
                                    && !resp.double_clicked()
                                {
                                    clicked_to_load = true;
                                }
                                if resp.hovered() && !transcript_text.is_empty() {
                                    resp.on_hover_text(transcript_text);
                                }
                            });
                        }
                        if cols.external {
                            for name in external_cols.iter() {
                                row.col(|ui| {
                                    let value = item
                                        .external
                                        .get(name)
                                        .map(|v| v.as_str())
                                        .unwrap_or("");
                                    let resp = ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(value).size(text_height * 0.95),
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
                                    if resp.hovered() && !value.is_empty() {
                                        resp.on_hover_text(value);
                                    }
                                });
                            }
                        }
                        if cols.length {
                            row.col(|ui| {
                                let secs = self
                                    .meta_for_path(&path_owned)
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
                        }
                        if cols.channels {
                            row.col(|ui| {
                                let ch = self
                                    .meta_for_path(&path_owned)
                                    .map(|m| m.channels)
                                    .filter(|v| *v > 0);
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(
                                                ch.map(|v| format!("{v}"))
                                                    .unwrap_or_else(|| "-".into()),
                                            )
                                            .monospace(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.sample_rate {
                            row.col(|ui| {
                                let sr = self.effective_sample_rate_for_path(&path_owned);
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(
                                                sr.map(|v| format!("{v}"))
                                                    .unwrap_or_else(|| "-".into()),
                                            )
                                            .monospace(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.bits {
                            row.col(|ui| {
                                let bits = self
                                    .meta_for_path(&path_owned)
                                    .map(|m| m.bits_per_sample)
                                    .filter(|v| *v > 0);
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(
                                                bits.map(|v| format!("{v}"))
                                                    .unwrap_or_else(|| "-".into()),
                                            )
                                            .monospace(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.bit_rate {
                            row.col(|ui| {
                                let br = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.bit_rate_bps)
                                    .filter(|v| *v > 0);
                                let text = br
                                    .map(|v| format!("{:.0}k", (v as f32) / 1000.0))
                                    .unwrap_or_else(|| "-".into());
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
                        }
                        if cols.peak {
                            row.col(|ui| {
                                let (rect2, resp2) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::click(),
                                );
                                let gain_db = self.pending_gain_db_for_path(&path_owned);
                                let orig = self.meta_for_path(&path_owned).and_then(|m| m.peak_db);
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
                        }
                        if cols.lufs {
                            row.col(|ui| {
                                let base = self.meta_for_path(&path_owned).and_then(|m| m.lufs_i);
                                let gain_db = self.pending_gain_db_for_path(&path_owned);
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
                        }
                        if cols.bpm {
                            row.col(|ui| {
                                let bpm = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.bpm)
                                    .filter(|v| v.is_finite() && *v > 0.0);
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(
                                                bpm.map(|v| format!("{:.2}", v))
                                                    .unwrap_or_else(|| "-".into()),
                                            )
                                            .monospace(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.created_at {
                            row.col(|ui| {
                                let text = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.created_at)
                                    .map(format_system_time_local)
                                    .unwrap_or_else(|| "-".into());
                                let resp = ui
                                    .add(
                                        egui::Label::new(RichText::new(text).monospace())
                                            .sense(Sense::click())
                                            .truncate(),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.modified_at {
                            row.col(|ui| {
                                let text = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.modified_at)
                                    .map(format_system_time_local)
                                    .unwrap_or_else(|| "-".into());
                                let resp = ui
                                    .add(
                                        egui::Label::new(RichText::new(text).monospace())
                                            .sense(Sense::click())
                                            .truncate(),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.gain {
                            row.col(|ui| {
                                let old = self.pending_gain_db_for_path(&path_owned);
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
                                        let path_list = vec![path_owned.clone()];
                                        let before = self.capture_list_selection_snapshot();
                                        let before_items =
                                            self.capture_list_undo_items_by_paths(&path_list);
                                        self.set_pending_gain_db_for_path(&path_owned, new);
                                        if self.playing_path.as_ref() == Some(&path_owned) {
                                            self.apply_effective_volume();
                                        }
                                        self.schedule_lufs_for_path(path_owned.clone());
                                        self.record_list_update_from_paths(
                                            &path_list,
                                            before_items,
                                            before,
                                        );
                                    }
                                }
                            });
                        }
                        if cols.wave {
                            row.col(|ui| {
                                let (rect2, _resp2) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::hover(),
                                );
                                let error_text = self
                                    .meta_for_path(&path_owned)
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
                                if let Some(m) = self.meta_for_path(&path_owned) {
                                    let w = wave_rect.width();
                                    let h = wave_rect.height();
                                    let n = m.thumb.len().max(1) as f32;
                                    let gain_db = self.pending_gain_db_for_path(&path_owned);
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
                                    let text_pos =
                                        egui::pos2(err_rect.left() + 4.0, err_rect.center().y);
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
                        }
                        row.col(|_ui| {});
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
                                .add_enabled(has_selection, egui::Button::new("Copy to Clipboard"))
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
                            let real_selected = self.selected_real_paths();
                            if real_selected.len() == 1 {
                                if ui.button("Rename...").clicked() {
                                    self.open_rename_dialog(real_selected[0].clone());
                                    ui.close();
                                }
                            }
                            if ui
                                .add_enabled(has_selection, egui::Button::new("Remove from List"))
                                .clicked()
                            {
                                self.remove_paths_from_list_with_undo(&selected);
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
                            if ui
                                .add_enabled(has_selection, egui::Button::new("Sample Rate Convert..."))
                                .clicked()
                            {
                                self.open_resample_dialog(selected.clone());
                                ui.close();
                            }
                        });
                        let clicked_any =
                            resp.clicked_by(egui::PointerButton::Primary) || clicked_to_load;
                        if clicked_any {
                            let mods = ctx.input(|i| i.modifiers);
                            self.update_selection_on_click(row_idx, mods);
                            self.select_and_load(row_idx, false);
                            if self.auto_play_list_nav {
                                self.request_list_autoplay();
                            }
                            ctx.memory_mut(|m| m.request_focus(list_focus_id));
                            list_has_focus = true;
                            self.search_has_focus = false;
                        } else if clicked_to_select {
                            self.selected = Some(row_idx);
                            self.scroll_to_selected = false;
                            self.selected_multi.clear();
                            self.selected_multi.insert(row_idx);
                            self.select_anchor = Some(row_idx);
                            ctx.memory_mut(|m| m.request_focus(list_focus_id));
                            list_has_focus = true;
                            self.search_has_focus = false;
                        }
                    } else {
                        // filler
                        for _ in 0..filler_cols {
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
        self.list_has_focus = list_has_focus;

        // keyboard handling moved above table to allow same-frame auto-scroll
    }
}
