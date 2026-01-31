use egui::RichText;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_debug_window(&mut self, ctx: &egui::Context) {
        if !self.debug.cfg.enabled {
            return;
        }
        let mut open = self.debug.show_window;
        egui::Window::new("Debug")
            .open(&mut open)
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Screenshot").clicked() {
                        let path = self.default_screenshot_path();
                        self.request_screenshot(ctx, path, false);
                    }
                    if ui.button("Copy Summary").clicked() {
                        let summary = self.debug_summary();
                        ctx.copy_text(summary);
                    }
                    if ui.button("Save Summary").clicked() {
                        let path = self.default_debug_summary_path();
                        self.save_debug_summary(path);
                    }
                    if ui.button("Run Checks").clicked() {
                        self.debug_check_invariants();
                    }
                });
                ui.separator();
                egui::CollapsingHeader::new("Summary")
                    .default_open(true)
                    .show(ui, |ui| {
                        let summary = self.debug_summary();
                        for line in summary.lines() {
                            ui.monospace(line);
                        }
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("External Merge Test").clicked() {
                                self.debug_start_external_merge_test(6, 6);
                            }
                        });
                    });
                ui.separator();
                egui::CollapsingHeader::new("Input")
                    .default_open(true)
                    .show(ui, |ui| {
                        let mods = ctx.input(|i| i.modifiers);
                        let wants_kb = ctx.wants_keyboard_input();
                        let wants_ptr = ctx.wants_pointer_input();
                        let pos = ctx.input(|i| i.pointer.hover_pos());
                        let pos_text = pos
                            .map(|p| format!("{:.1},{:.1}", p.x, p.y))
                            .unwrap_or_else(|| "(none)".to_string());
                        ui.monospace(format!("raw.focused: {}", self.debug.last_raw_focused));
                        ui.monospace(format!("raw.events_len: {}", self.debug.last_events_len));
                        ui.monospace(format!("wants_keyboard_input: {wants_kb}"));
                        ui.monospace(format!("wants_pointer_input: {wants_ptr}"));
                        ui.monospace(format!(
                            "suppress_list_enter: {}",
                            self.suppress_list_enter
                        ));
                        ui.monospace(format!(
                            "mods: ctrl:{} shift:{} alt:{} command:{}",
                            mods.ctrl, mods.shift, mods.alt, mods.command
                        ));
                        ui.monospace(format!("pointer: {pos_text}"));
                        ui.monospace(format!(
                            "pointer_over_list: {}",
                            self.debug.last_pointer_over_list
                        ));
                        ui.monospace(format!(
                            "list_has_focus: {} search_has_focus: {}",
                            self.list_has_focus, self.search_has_focus
                        ));
                        ui.monospace(format!(
                            "ctrl_down:{} c_pressed:{} v_pressed:{} c_down:{} v_down:{}",
                            self.debug.last_ctrl_down,
                            self.debug.last_key_c_pressed,
                            self.debug.last_key_v_pressed,
                            self.debug.last_key_c_down,
                            self.debug.last_key_v_down
                        ));
                        ui.monospace(format!(
                            "clip_edge: c:{} v:{}",
                            self.clipboard_c_was_down, self.clipboard_v_was_down
                        ));
                        if let Some(hotkey) = self.debug.last_hotkey.as_ref() {
                            let ago = self
                                .debug
                                .last_hotkey_at
                                .map(|t| t.elapsed().as_secs_f32())
                                .unwrap_or(0.0);
                            ui.monospace(format!("last_hotkey: {hotkey} ({ago:.2}s ago)"));
                        }
                        ui.separator();
                        ui.checkbox(&mut self.debug.input_trace_enabled, "Trace hotkeys");
                        ui.checkbox(&mut self.debug.event_trace_enabled, "Trace raw events");
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Trace max");
                            ui.add(
                                egui::DragValue::new(&mut self.debug.input_trace_max)
                                    .range(10..=2000),
                            );
                            let has_trace = !self.debug.input_trace.is_empty();
                            if ui
                                .add_enabled(has_trace, egui::Button::new("Copy trace"))
                                .on_hover_text("Copy Trace hotkeys lines to clipboard")
                                .clicked()
                            {
                                let mut buf = String::new();
                                for line in &self.debug.input_trace {
                                    buf.push_str(line);
                                    buf.push('\n');
                                }
                                ui.ctx().send_cmd(egui::output::OutputCommand::CopyText(buf));
                            }
                            if ui.button("Clear trace").clicked() {
                                self.debug.input_trace.clear();
                            }
                        });
                        egui::ScrollArea::vertical()
                            .max_height(140.0)
                            .show(ui, |ui| {
                                for line in &self.debug.input_trace {
                                    ui.monospace(line);
                                }
                            });
                        if self.debug.event_trace_enabled {
                            ui.separator();
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Event trace max");
                                ui.add(
                                    egui::DragValue::new(&mut self.debug.event_trace_max)
                                        .range(10..=2000),
                                );
                                if ui.button("Clear events").clicked() {
                                    self.debug.event_trace.clear();
                                }
                            });
                            egui::ScrollArea::vertical()
                                .max_height(140.0)
                                .show(ui, |ui| {
                                    for line in &self.debug.event_trace {
                                        ui.monospace(line);
                                    }
                                });
                        }
                    });
                ui.separator();
                egui::CollapsingHeader::new("Clipboard")
                    .default_open(true)
                    .show(ui, |ui| {
                        let payload_count = self
                            .clipboard_payload
                            .as_ref()
                            .map(|p| p.items.len())
                            .unwrap_or(0);
                        ui.monospace(format!("payload_items: {payload_count}"));
                        if let Some(payload) = self.clipboard_payload.as_ref() {
                            if let Some(item) = payload.items.first() {
                                ui.monospace(format!("first_item: {}", item.display_name));
                            }
                        }
                        let os_files = self.get_clipboard_files();
                        ui.monospace(format!("os_clipboard_files: {}", os_files.len()));
                        if let Some(t) = self.debug.last_copy_at {
                            ui.monospace(format!(
                                "last_copy: {:.2}s ago (items={})",
                                t.elapsed().as_secs_f32(),
                                self.debug.last_copy_count
                            ));
                        }
                        if let Some(t) = self.debug.last_paste_at {
                            let src = self.debug.last_paste_source.as_deref().unwrap_or("unknown");
                            ui.monospace(format!(
                                "last_paste: {:.2}s ago (items={}, source={})",
                                t.elapsed().as_secs_f32(),
                                self.debug.last_paste_count,
                                src
                            ));
                        }
                        ui.separator();
                        ui.monospace(format!(
                            "clip_allow:{} wants_kb:{} ctrl:{}",
                            self.debug.last_clip_allow,
                            self.debug.last_clip_wants_kb,
                            self.debug.last_clip_ctrl
                        ));
                        ui.monospace(format!(
                            "clip_events: copy:{} paste:{}",
                            self.debug.last_clip_event_copy, self.debug.last_clip_event_paste
                        ));
                        ui.monospace(format!(
                            "clip_raw_keys: c:{} v:{}",
                            self.debug.last_clip_raw_key_c, self.debug.last_clip_raw_key_v
                        ));
                        ui.monospace(format!(
                            "clip_os_keys: ctrl:{} c:{} v:{}",
                            self.debug.last_clip_os_ctrl,
                            self.debug.last_clip_os_key_c,
                            self.debug.last_clip_os_key_v
                        ));
                        ui.monospace(format!(
                            "clip_consumed: copy:{} paste:{}",
                            self.debug.last_clip_consumed_copy, self.debug.last_clip_consumed_paste
                        ));
                        ui.monospace(format!(
                            "clip_triggers: copy:{} paste:{}",
                            self.debug.last_clip_copy_trigger, self.debug.last_clip_paste_trigger
                        ));
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Copy selection").clicked() {
                                self.copy_selected_to_clipboard();
                            }
                            if ui.button("Paste").clicked() {
                                self.paste_clipboard_to_list();
                            }
                        });
                    });
                ui.separator();
                egui::CollapsingHeader::new("Selection")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.monospace(format!("selected_row: {:?}", self.selected));
                        ui.monospace(format!("selected_multi: {}", self.selected_multi.len()));
                        let selected_ids = self.selected_item_ids();
                        ui.monospace(format!("selected_item_ids: {}", selected_ids.len()));
                        if let Some(id) = selected_ids.first().copied() {
                            ui.monospace(format!("selected_item_id: {id:?}"));
                            let item_idx = self.item_index.get(&id).copied();
                            ui.monospace(format!("item_index_hit: {}", item_idx.is_some()));
                            let item_found = self.item_for_id(id).is_some();
                            ui.monospace(format!("item_for_id_found: {item_found}"));
                        }
                        if let Some(row) = self.selected {
                            let file_id = self.files.get(row).copied();
                            ui.monospace(format!("selected_row_file_id: {file_id:?}"));
                        }
                        if let Some(path) = self.selected_path_buf() {
                            ui.monospace(format!("selected_path: {}", path.display()));
                        }
                        let active_tab = self
                            .active_tab
                            .and_then(|i| self.tabs.get(i))
                            .map(|t| t.display_name.clone())
                            .unwrap_or_else(|| "(none)".to_string());
                        ui.monospace(format!("active_tab: {active_tab}"));
                    });
                ui.separator();
                egui::CollapsingHeader::new("Processing")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.monospace(format!(
                            "processing: {}",
                            self.processing
                                .as_ref()
                                .map(|p| p.msg.as_str())
                                .unwrap_or("none")
                        ));
                        if let Some(p) = self.processing.as_ref() {
                            let elapsed = p.started_at.elapsed().as_secs_f32();
                            ui.monospace(format!("processing_elapsed: {elapsed:.2}s"));
                            ui.monospace(format!("autoplay_when_ready: {}", p.autoplay_when_ready));
                        }
                        ui.monospace(format!(
                            "editor_apply_state: {}",
                            self.editor_apply_state.is_some()
                        ));
                        ui.monospace(format!(
                            "editor_decode_state: {}",
                            self.editor_decode_state.is_some()
                        ));
                        if let Some(state) = self.editor_decode_state.as_ref() {
                            let elapsed = state.started_at.elapsed().as_secs_f32();
                            ui.monospace(format!("decode_path: {}", state.path.display()));
                            ui.monospace(format!("decode_elapsed: {elapsed:.2}s"));
                            ui.monospace(format!("decode_partial_ready: {}", state.partial_ready));
                        }
                        ui.monospace(format!("export_state: {}", self.export_state.is_some()));
                    });
                ui.separator();
                egui::CollapsingHeader::new("Search")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.monospace(format!("query: {}", self.search_query));
                        ui.monospace(format!("regex: {}", self.search_use_regex));
                        ui.monospace(format!("search_dirty: {}", self.search_dirty));
                        let deadline = self.search_deadline.map(|d| {
                            d.saturating_duration_since(std::time::Instant::now())
                                .as_millis()
                        });
                        ui.monospace(format!(
                            "search_deadline_ms: {}",
                            deadline
                                .map(|d| d.to_string())
                                .unwrap_or_else(|| "none".to_string())
                        ));
                    });
                ui.separator();
                egui::CollapsingHeader::new("List Perf")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Dummy files");
                            ui.add(
                                egui::DragValue::new(&mut self.debug.dummy_list_count)
                                    .range(0..=1_000_000)
                                    .speed(5000),
                            );
                            if ui.button("Populate").clicked() {
                                let count = self.debug.dummy_list_count as usize;
                                self.populate_dummy_list(count);
                            }
                        });
                    });
                ui.checkbox(&mut self.debug.overlay_trace, "Overlay trace logs");
                ui.separator();
                egui::CollapsingHeader::new("Logs")
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Logs").strong());
                            if ui.button("Clear").clicked() {
                                self.debug.logs.clear();
                            }
                        });
                        egui::ScrollArea::vertical()
                            .max_height(220.0)
                            .show(ui, |ui| {
                                for line in &self.debug.logs {
                                    ui.monospace(line);
                                }
                            });
                    });
                if let Some(auto) = &self.debug.auto {
                    ui.separator();
                    ui.label(format!("auto-run steps: {}", auto.steps.len()));
                }
            });
        self.debug.show_window = open;
    }
}
