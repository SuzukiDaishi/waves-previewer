use egui::{Align2, Color32, Id, Order, RichText, Stroke};

use crate::app::assistant_ops::{AssistantDisplayMode, AssistantMessageRole};

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_gemini_assistant(&mut self, ctx: &egui::Context) {
        if !self.assistant_is_ready() {
            return;
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Escape))
            && matches!(
                self.assistant_state.display,
                AssistantDisplayMode::Overlay | AssistantDisplayMode::Pinned
            )
        {
            self.close_gemini_assistant();
        }

        let mut open_overlay = false;
        if self.assistant_state.display == AssistantDisplayMode::Rail {
            egui::Area::new(Id::new("gemini_assistant_rail"))
                .order(Order::Foreground)
                .anchor(Align2::RIGHT_CENTER, egui::vec2(-7.0, 0.0))
                .show(ctx, |ui| {
                    let count = self.assistant_state.active_jobs.len();
                    let label = if count == 0 {
                        "✦".to_string()
                    } else {
                        format!("✦\n{count}")
                    };
                    if ui
                        .add_sized([34.0, 44.0], egui::Button::new(label))
                        .on_hover_text("Open Gemini Assistant")
                        .clicked()
                    {
                        open_overlay = true;
                    }
                });
        }
        if open_overlay {
            self.assistant_state.display = AssistantDisplayMode::Overlay;
        }
        if !matches!(
            self.assistant_state.display,
            AssistantDisplayMode::Overlay | AssistantDisplayMode::Pinned
        ) {
            return;
        }

        let snapshot = self.action_context_snapshot();
        let panel_height = (ctx.content_rect().height() - 105.0).max(360.0);
        let mut close = false;
        let mut submit = false;
        let mut cancel = false;
        let mut open_settings = false;
        let mut open_review = false;
        egui::Area::new(Id::new("gemini_assistant_overlay"))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_TOP, egui::vec2(-7.0, 96.0))
            .show(ctx, |ui| {
                egui::Frame::window(ui.style())
                    .fill(Color32::from_rgb(21, 24, 30))
                    .stroke(Stroke::new(1.0, Color32::from_rgb(58, 67, 80)))
                    .show(ui, |ui| {
                        ui.set_min_size(egui::vec2(390.0, panel_height));
                        ui.set_max_width(390.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(Color32::from_rgb(74, 222, 128)));
                            ui.vertical(|ui| {
                                ui.strong("Gemini Assistant");
                                ui.small(if self.assistant_state.privacy_mode {
                                    "Private chat · store=false · Actions off"
                                } else {
                                    "Agent mode · typed Actions · R2/R3 approval"
                                });
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("×").on_hover_text("Close").clicked() {
                                        close = true;
                                    }
                                    if ui.button("⚙").on_hover_text("Settings").clicked() {
                                        open_settings = true;
                                    }
                                    if ui.button("Review").clicked() {
                                        open_review = true;
                                    }
                                },
                            );
                        });
                        ui.separator();

                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .selectable_label(
                                    self.assistant_state.include_session_context,
                                    format!(
                                        "Session · {} items",
                                        if self.assistant_state.include_session_context {
                                            snapshot.library_count
                                        } else {
                                            0
                                        }
                                    ),
                                )
                                .on_hover_text("Click to include or exclude session context")
                                .clicked()
                            {
                                self.assistant_state.include_session_context =
                                    !self.assistant_state.include_session_context;
                            }
                            let selection_label = if let Some(range) = &snapshot.editor_range {
                                format!(
                                    "Editor · {:.2}–{:.2}s",
                                    range.start_ms as f64 / 1000.0,
                                    range.end_ms as f64 / 1000.0
                                )
                            } else {
                                format!("Selection · {}", snapshot.selected_media_ids.len())
                            };
                            if ui
                                .selectable_label(
                                    self.assistant_state.include_selection_context,
                                    selection_label,
                                )
                                .on_hover_text("Click to include or exclude selection context")
                                .clicked()
                            {
                                self.assistant_state.include_selection_context =
                                    !self.assistant_state.include_selection_context;
                            }
                            ui.label(format!(
                                "Cloud audio: {}",
                                self.assistant_state.upload_policy.prefs_name()
                            ));
                            if !self.assistant_state.privacy_mode {
                                ui.label(format!(
                                    "Usage: {} tokens",
                                    self.assistant_state.total_usage_tokens()
                                ));
                            }
                        });
                        ui.separator();

                        let input_height = 106.0;
                        let trace_height = if self.assistant_state.traces.is_empty() {
                            0.0
                        } else {
                            86.0
                        };
                        let chat_height =
                            (panel_height - input_height - trace_height - 98.0).max(120.0);
                        egui::ScrollArea::vertical()
                            .id_salt("gemini_chat_scroll")
                            .stick_to_bottom(true)
                            .auto_shrink([false, false])
                            .max_height(chat_height)
                            .min_scrolled_height(chat_height)
                            .show(ui, |ui| {
                                for message in &self.assistant_state.messages {
                                    let (label, fill) = match message.role {
                                        AssistantMessageRole::User => {
                                            ("You", Color32::from_rgb(18, 85, 111))
                                        }
                                        AssistantMessageRole::Assistant => {
                                            ("Gemini", Color32::from_rgb(29, 34, 43))
                                        }
                                        AssistantMessageRole::System => {
                                            ("NeoWaves", Color32::from_rgb(43, 39, 28))
                                        }
                                    };
                                    egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(RichText::new(label).small().strong());
                                        ui.label(&message.text);
                                    });
                                    ui.add_space(5.0);
                                }
                                for partial in self.assistant_state.streaming_text.values() {
                                    egui::Frame::group(ui.style())
                                        .fill(Color32::from_rgb(29, 34, 43))
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.horizontal(|ui| {
                                                ui.label(RichText::new("Gemini").small().strong());
                                                ui.spinner();
                                            });
                                            ui.label(partial);
                                        });
                                    ui.add_space(5.0);
                                }
                                if let Some(error) = &self.assistant_state.last_error {
                                    ui.colored_label(
                                        Color32::from_rgb(251, 113, 133),
                                        format!("Error: {error}"),
                                    );
                                }
                                for job in self.assistant_state.active_jobs.values() {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(&job.label);
                                    });
                                }
                            });

                        if trace_height > 0.0 {
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), trace_height),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    egui::ScrollArea::vertical()
                                        .id_salt("gemini_trace_scroll")
                                        .auto_shrink([false, false])
                                        .max_height(trace_height)
                                        .min_scrolled_height(trace_height)
                                        .show(ui, |ui| {
                                            if !self.assistant_state.traces.is_empty() {
                                                ui.separator();
                                                egui::CollapsingHeader::new(format!(
                                                    "Action trace ({})",
                                                    self.assistant_state.traces.len()
                                                ))
                                                .default_open(false)
                                                .show(ui, |ui| {
                                                    for trace in self
                                                        .assistant_state
                                                        .traces
                                                        .iter()
                                                        .rev()
                                                        .take(12)
                                                    {
                                                        let status =
                                                            if trace.ok { "✓" } else { "!" };
                                                        ui.label(format!(
                                        "{status} {} · {} target(s) · {} ms\n{}",
                                        trace.action,
                                        trace.target_count,
                                        trace.duration_ms,
                                        trace.summary
                                    ));
                                                    }
                                                    if ui
                                                        .small_button("Copy redacted trace JSON")
                                                        .clicked()
                                                    {
                                                        ctx.copy_text(
                                                            self.assistant_state
                                                                .redacted_trace_export_json(),
                                                        );
                                                    }
                                                });
                                            }
                                        });
                                },
                            );
                        }
                        ui.separator();

                        let response = ui.add(
                            egui::TextEdit::multiline(&mut self.assistant_state.prompt)
                                .desired_rows(3)
                                .hint_text("Ask about the current list, selection, or editor…"),
                        );
                        if response.has_focus()
                            && ctx.input(|input| {
                                input.key_pressed(egui::Key::Enter) && input.modifiers.command
                            })
                        {
                            submit = true;
                        }
                        ui.horizontal(|ui| {
                            ui.small("Ctrl/Cmd+Enter to send");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if self.assistant_state.pending_chat_job.is_some() {
                                        if ui.button("Cancel").clicked() {
                                            cancel = true;
                                        }
                                    } else if ui
                                        .add_enabled(
                                            !self.assistant_state.prompt.trim().is_empty(),
                                            egui::Button::new("Send"),
                                        )
                                        .clicked()
                                    {
                                        submit = true;
                                    }
                                },
                            );
                        });
                    });
            });

        if close {
            self.close_gemini_assistant();
        }
        if open_settings {
            self.assistant_state.show_settings = true;
        }
        if open_review {
            self.assistant_state.show_review = true;
        }
        if cancel {
            self.cancel_gemini_chat();
        }
        if submit {
            self.submit_gemini_prompt();
        }
    }
}
