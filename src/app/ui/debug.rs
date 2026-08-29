use egui::{Color32, RichText, Sense, Stroke, Vec2};

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_debug_window(&mut self, ctx: &egui::Context) {
        if !self.debug.cfg.enabled {
            return;
        }
        let mut open = self.debug.show_window;
        let scroll_target = self.begin_floating_scroll_surface("debug_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let shown = egui::Window::new("Debug")
            .open(&mut open)
            .resizable(true)
            .default_width(720.0)
            .min_width(520.0)
            .show(ctx, |ui| {
                // Diagnostics exist to be pasted into a bug report, so
                // this window opts back into the label selection the rest of
                // the app turns off.
                ui.style_mut().interaction.selectable_labels = true;
                ui.style_mut().interaction.multi_widget_text_select = true;
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
                egui::CollapsingHeader::new("Frame Profiler")
                    .default_open(true)
                    .show(ui, |ui| self.ui_frame_profiler(ui));
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
                        let wants_kb = ctx.egui_wants_keyboard_input();
                        let wants_ptr = ctx.egui_wants_pointer_input();
                        let pos = ctx.input(|i| i.pointer.hover_pos());
                        let pos_text = pos
                            .map(|p| format!("{:.1},{:.1}", p.x, p.y))
                            .unwrap_or_else(|| "(none)".to_string());
                        ui.monospace(format!("raw.focused: {}", self.debug.last_raw_focused));
                        ui.monospace(format!("raw.events_len: {}", self.debug.last_events_len));
                        ui.monospace(format!("wants_keyboard_input: {wants_kb}"));
                        ui.monospace(format!("wants_pointer_input: {wants_ptr}"));
                        ui.monospace(format!("suppress_list_enter: {}", self.suppress_list_enter));
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
                                ui.ctx()
                                    .send_cmd(egui::output::OutputCommand::CopyText(buf));
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
                                self.paste_clipboard_to_list(None);
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
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        self.debug.show_window = open;
    }

    fn ui_frame_profiler(&mut self, ui: &mut egui::Ui) {
        let profiler = &mut self.debug.frame_profiler;
        let mut clear = false;
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut profiler.enabled, "Record")
                .on_hover_text("Collect samples only while this Debug window is visible");
            ui.checkbox(&mut profiler.paused, "Hold")
                .on_hover_text("Freeze the current graphs for inspection");
            if ui.button("Clear").clicked() {
                clear = true;
            }
            ui.label(
                RichText::new(format!("{} / 600 frames", profiler.samples().len()))
                    .weak()
                    .monospace(),
            );
        });
        if clear {
            profiler.clear();
        }

        ui.label(
            RichText::new(
                "FPS is the real app-frame cadence. UI Thread Time measures CPU-side update and \n\
                 paint-command preparation; GPU present time is not available from egui.",
            )
            .small()
            .weak(),
        );

        let samples: Vec<_> = profiler.samples().iter().cloned().collect();
        if samples.len() < 2 {
            ui.add_space(8.0);
            ui.label("Collecting frame samples...");
            if profiler.enabled && !profiler.paused {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(16));
            }
            return;
        }

        let valid_intervals: Vec<f32> = samples
            .iter()
            .filter_map(|sample| (sample.interval_ms > 0.0).then_some(sample.interval_ms))
            .collect();
        let app_times: Vec<f32> = samples.iter().map(|sample| sample.app_ms).collect();
        let average_interval =
            valid_intervals.iter().sum::<f32>() / valid_intervals.len().max(1) as f32;
        let cadence_fps = if average_interval > 0.0 {
            1_000.0 / average_interval
        } else {
            0.0
        };
        let p99_interval = percentile(&valid_intervals, 0.99);
        let low_fps = if p99_interval > 0.0 {
            1_000.0 / p99_interval
        } else {
            0.0
        };
        let deferred_frames = samples
            .iter()
            .filter(|sample| sample.deferred_count > 0)
            .count();
        ui.horizontal_wrapped(|ui| {
            metric_label(ui, "FPS avg", cadence_fps, "");
            metric_label(ui, "1% low", low_fps, "");
            metric_label(ui, "UI p95", percentile(&app_times, 0.95), " ms");
            metric_label(ui, "UI max", percentile(&app_times, 1.0), " ms");
            ui.monospace(format!("Deferred {deferred_frames}/{}", samples.len()));
        });

        ui.add_space(4.0);
        ui.label(RichText::new("Actual FPS").strong());
        draw_fps_history(ui, &samples);
        ui.add_space(4.0);
        ui.label(RichText::new("UI Thread Time").strong());
        draw_cpu_history(ui, &samples);

        ui.horizontal_wrapped(|ui| {
            for phase in crate::app::frame_profiler::FramePhase::ALL {
                ui.colored_label(phase_color(phase), "■");
                ui.label(phase.label());
            }
            ui.colored_label(Color32::from_gray(115), "■");
            ui.label("Other / framework");
        });

        ui.add_space(6.0);
        ui.label(RichText::new("Top UI-thread blockers (recent P95)").strong());
        ui.label(
            RichText::new(
                "Red can miss a 60 FPS frame by itself; yellow consumes at least 4 ms. \n\
                 Aggregate phase rows are included so uninstrumented UI work stays visible.",
            )
            .small()
            .weak(),
        );
        let summaries = profiler.stage_summaries();
        egui::Grid::new("frame_profiler_stage_grid")
            .num_columns(6)
            .striped(true)
            .spacing(Vec2::new(12.0, 3.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Stage").strong());
                ui.label(RichText::new("Last").strong());
                ui.label(RichText::new("Avg").strong());
                ui.label(RichText::new("P95").strong());
                ui.label(RichText::new("Max").strong());
                ui.label(RichText::new("N").strong());
                ui.end_row();
                for stage in summaries.iter().take(16) {
                    let color = blocker_color(stage.p95_ms);
                    let aggregate = crate::app::frame_profiler::FramePhase::ALL
                        .iter()
                        .any(|phase| phase.label() == stage.name);
                    let name = RichText::new(stage.name).color(color);
                    ui.label(if aggregate { name.strong() } else { name });
                    ui.monospace(format!("{:.2}", stage.last_ms));
                    ui.monospace(format!("{:.2}", stage.average_ms));
                    ui.monospace(format!("{:.2}", stage.p95_ms));
                    ui.monospace(format!("{:.2}", stage.max_ms));
                    ui.monospace(stage.samples.to_string());
                    ui.end_row();
                }
            });
    }
}

fn metric_label(ui: &mut egui::Ui, name: &str, value: f32, suffix: &str) {
    let color = if name.contains("FPS") || name.contains("low") {
        if value >= 55.0 {
            Color32::from_rgb(90, 210, 125)
        } else if value >= 30.0 {
            Color32::from_rgb(235, 190, 70)
        } else {
            Color32::from_rgb(240, 90, 90)
        }
    } else {
        blocker_color(value)
    };
    ui.label(RichText::new(format!("{name} {value:.1}{suffix}")).color(color));
}

fn percentile(values: &[f32], percentile: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let index =
        ((sorted.len().saturating_sub(1)) as f32 * percentile.clamp(0.0, 1.0)).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn phase_color(phase: crate::app::frame_profiler::FramePhase) -> Color32 {
    use crate::app::frame_profiler::FramePhase;
    match phase {
        FramePhase::PreUi => Color32::from_rgb(73, 157, 255),
        FramePhase::Workspace => Color32::from_rgb(76, 206, 139),
        FramePhase::Activation => Color32::from_rgb(238, 177, 65),
        FramePhase::Overlays => Color32::from_rgb(187, 113, 235),
        FramePhase::Windows => Color32::from_rgb(234, 100, 132),
        FramePhase::Finish => Color32::from_rgb(77, 204, 212),
    }
}

fn blocker_color(ms: f32) -> Color32 {
    if ms >= 16.67 {
        Color32::from_rgb(245, 90, 90)
    } else if ms >= 4.0 {
        Color32::from_rgb(235, 190, 70)
    } else {
        Color32::from_rgb(145, 215, 165)
    }
}

fn chart_frame(ui: &mut egui::Ui, height: f32) -> (egui::Response, egui::Painter, egui::Rect) {
    let width = ui.available_width().max(260.0);
    let (response, painter) = ui.allocate_painter(Vec2::new(width, height), Sense::hover());
    let rect = response.rect;
    painter.rect_filled(rect, 4.0, Color32::from_rgb(16, 19, 24));
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, Color32::from_gray(55)),
        egui::StrokeKind::Inside,
    );
    (response, painter, rect.shrink2(Vec2::new(8.0, 7.0)))
}

fn draw_fps_history(ui: &mut egui::Ui, samples: &[crate::app::frame_profiler::FramePerfSample]) {
    let (response, painter, rect) = chart_frame(ui, 120.0);
    let max_points = (rect.width() / 1.5).floor().max(2.0) as usize;
    let samples = &samples[samples.len().saturating_sub(max_points)..];
    let map_y = |fps: f32| rect.bottom() - (fps.clamp(0.0, 120.0) / 120.0) * rect.height();
    for (fps, color) in [
        (30.0, Color32::from_rgb(125, 85, 55)),
        (60.0, Color32::from_rgb(55, 105, 75)),
    ] {
        let y = map_y(fps);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(1.0, color),
        );
        painter.text(
            egui::pos2(rect.left() + 3.0, y - 2.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{fps:.0}"),
            egui::FontId::monospace(9.0),
            Color32::from_gray(155),
        );
    }
    if samples.len() >= 2 {
        let x_step = rect.width() / (samples.len() - 1) as f32;
        for index in 1..samples.len() {
            let previous = samples[index - 1].fps();
            let current = samples[index].fps();
            let color = if current >= 55.0 {
                Color32::from_rgb(90, 220, 130)
            } else if current >= 30.0 {
                Color32::from_rgb(240, 190, 65)
            } else {
                Color32::from_rgb(245, 80, 85)
            };
            painter.line_segment(
                [
                    egui::pos2(rect.left() + (index - 1) as f32 * x_step, map_y(previous)),
                    egui::pos2(rect.left() + index as f32 * x_step, map_y(current)),
                ],
                Stroke::new(1.5, color),
            );
        }
    }
    if let Some(pos) = response.hover_pos() {
        let fraction = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let index = (fraction * samples.len().saturating_sub(1) as f32).round() as usize;
        if let Some(sample) = samples.get(index) {
            let x = rect.left()
                + index as f32 / samples.len().saturating_sub(1).max(1) as f32 * rect.width();
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(1.0, Color32::WHITE),
            );
            response.clone().on_hover_text(format!(
                "FPS {:.1}\nCadence {:.2} ms\nApp UI {:.2} ms\nDeferred drains {}",
                sample.fps(),
                sample.interval_ms,
                sample.app_ms,
                sample.deferred_count
            ));
        }
    }
}

fn draw_cpu_history(ui: &mut egui::Ui, samples: &[crate::app::frame_profiler::FramePerfSample]) {
    let (response, painter, rect) = chart_frame(ui, 145.0);
    let max_bars = (rect.width() / 2.0).floor().max(2.0) as usize;
    let samples = &samples[samples.len().saturating_sub(max_bars)..];
    let peak = samples
        .iter()
        .map(|sample| sample.app_ms)
        .fold(0.0_f32, f32::max);
    let scale_ms = if peak <= 33.34 {
        33.34
    } else if peak <= 50.0 {
        50.0
    } else if peak <= 100.0 {
        100.0
    } else if peak <= 200.0 {
        200.0
    } else {
        (peak / 100.0).ceil() * 100.0
    };
    let map_y = |ms: f32| rect.bottom() - (ms.clamp(0.0, scale_ms) / scale_ms) * rect.height();
    for (ms, color) in [
        (16.67, Color32::from_rgb(60, 115, 78)),
        (33.33, Color32::from_rgb(125, 85, 55)),
    ] {
        if ms <= scale_ms {
            let y = map_y(ms);
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                Stroke::new(1.0, color),
            );
            painter.text(
                egui::pos2(rect.left() + 3.0, y - 2.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{ms:.1} ms"),
                egui::FontId::monospace(9.0),
                Color32::from_gray(155),
            );
        }
    }
    let bar_step = rect.width() / samples.len().max(1) as f32;
    let bar_width = (bar_step - 0.35).max(0.7);
    for (index, sample) in samples.iter().enumerate() {
        let x0 = rect.left() + index as f32 * bar_step;
        let mut stacked_ms = 0.0;
        for phase in crate::app::frame_profiler::FramePhase::ALL {
            let value = sample.phases_ms[phase.index()];
            let y0 = map_y(stacked_ms);
            stacked_ms += value;
            let y1 = map_y(stacked_ms);
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, y1), egui::pos2(x0 + bar_width, y0)),
                0.0,
                phase_color(phase),
            );
        }
        if sample.other_ms > 0.0 {
            let y0 = map_y(stacked_ms);
            stacked_ms += sample.other_ms;
            let y1 = map_y(stacked_ms);
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, y1), egui::pos2(x0 + bar_width, y0)),
                0.0,
                Color32::from_gray(115),
            );
        }
        if sample.deferred_count > 0 {
            painter.circle_filled(
                egui::pos2(x0 + bar_width * 0.5, rect.top() + 4.0),
                1.7,
                Color32::from_rgb(250, 85, 85),
            );
        }
    }
    if let Some(pos) = response.hover_pos() {
        let index = (((pos.x - rect.left()) / bar_step).floor() as usize)
            .min(samples.len().saturating_sub(1));
        if let Some(sample) = samples.get(index) {
            let mut text = format!(
                "App UI {:.2} ms (accounted {:.2} ms)\n",
                sample.app_ms,
                sample.accounted_ms()
            );
            for phase in crate::app::frame_profiler::FramePhase::ALL {
                text.push_str(&format!(
                    "{}: {:.2} ms\n",
                    phase.label(),
                    sample.phases_ms[phase.index()]
                ));
            }
            text.push_str(&format!(
                "Other / framework: {:.2} ms\nDeferred drains: {}",
                sample.other_ms, sample.deferred_count
            ));
            response.clone().on_hover_text(text);
        }
    }
}
