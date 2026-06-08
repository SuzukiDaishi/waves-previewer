use std::time::Duration;

use egui::{Align, Color32, RichText};

use crate::app::types::{EffectGraphRunMode, ToolKind};
use crate::app::WavesPreviewer;

struct EffectGraphApplyStatus {
    elapsed: Duration,
    visible: bool,
}

struct ListLoadingStatus {
    elapsed: Duration,
    visible: bool,
}

#[derive(Clone, Copy)]
enum TopbarActivityCancel {
    Processing,
    EditorDecode,
    EffectGraph,
    HeavyPreview,
    MusicPreview,
    EditorApply,
    PluginProcess,
    BulkResample,
    Transcript,
    Music,
    EditorAnalysis,
}

struct TopbarActivityItem {
    label: String,
    progress: Option<f32>,
    show_percentage: bool,
    cancel: Option<TopbarActivityCancel>,
}

impl WavesPreviewer {
    pub(super) fn ui_topbar_status_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let available_w = ui.available_width();
        let compact = available_w < 760.0;
        ui.horizontal(|ui| {
            let right_w = if compact { 280.0_f32 } else { 450.0_f32 }.min(ui.available_width());
            let status_w = if compact {
                (ui.available_width() - right_w).max(180.0)
            } else {
                (ui.available_width() - right_w).max(220.0)
            };
            ui.allocate_ui_with_layout(
                egui::vec2(status_w, 28.0),
                egui::Layout::left_to_right(Align::Center),
                |ui| {
                    self.ui_topbar_primary_status(ui);
                    self.ui_topbar_activity_slot(ui);
                },
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                self.ui_topbar_meter_group(ui, ctx, compact);
            });
        });
        ui.add_space(4.0);
    }

    fn ui_topbar_primary_status(&mut self, ui: &mut egui::Ui) {
        if self.playback_session.is_playing {
            ui.label(
                RichText::new("Playing")
                    .color(Color32::from_rgb(120, 220, 140))
                    .strong(),
            );
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
                } else if loading {
                    format!("Files: {} / {} ?", total_vis, total_all)
                } else {
                    format!("Files: {} / {}", total_vis, total_all)
                };
                ui.label(RichText::new(label).monospace());
            }
            if dirty_gains > 0 {
                ui.separator();
                ui.label(RichText::new(format!("Unsaved Gains: {}", dirty_gains)).weak());
            }
        }
    }

    fn ui_topbar_sort_loading_visible(&mut self) -> bool {
        let now = std::time::Instant::now();
        let visible = self.sort_loading_started_at.is_some()
            || self
                .sort_loading_hold_until
                .map(|until| until > now)
                .unwrap_or(false)
            || (self.sort_key_uses_meta() && !self.meta_inflight.is_empty())
            || (self.sort_key_uses_transcript() && !self.transcript_inflight.is_empty());
        if !visible && self.sort_loading_hold_until.is_some() {
            self.sort_loading_hold_until = None;
        }
        visible
    }

    fn ui_topbar_activity_slot(&mut self, ui: &mut egui::Ui) {
        let items = self.topbar_activity_items();
        ui.separator();
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width().max(180.0), 26.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_height(26.0);
                let Some(item) = items.first() else {
                    ui.label(RichText::new("Ready").weak());
                    return;
                };
                if item.progress.is_none() {
                    ui.add(egui::Spinner::new());
                }
                ui.add(
                    egui::Label::new(RichText::new(item.label.as_str()).weak())
                        .truncate()
                        .show_tooltip_when_elided(true),
                );
                if let Some(progress) = item.progress {
                    let mut bar = egui::ProgressBar::new(progress.clamp(0.0, 1.0))
                        .desired_width(96.0);
                    if item.show_percentage {
                        bar = bar.show_percentage();
                    }
                    ui.add(bar);
                }
                if items.len() > 1 {
                    ui.label(RichText::new(format!("+{}", items.len() - 1)).weak());
                }
                if let Some(cancel) = item.cancel {
                    if ui.button("Cancel").clicked() {
                        self.handle_topbar_activity_cancel(cancel);
                    }
                }
            },
        );
    }

    fn topbar_activity_items(&mut self) -> Vec<TopbarActivityItem> {
        let sort_loading_visible = self.ui_topbar_sort_loading_visible();
        let list_loading = self.topbar_list_loading_status();
        let effect_graph_apply = self.topbar_effect_graph_apply_status();
        let mut items = Vec::new();

        if let Some(label) = self.topbar_scan_activity_text() {
            items.push(TopbarActivityItem {
                label,
                progress: None,
                show_percentage: false,
                cancel: None,
            });
        }
        if let Some(status) = self
            .editor_decode_state
            .as_ref()
            .filter(|state| state.started_at.elapsed() >= Duration::from_millis(120))
            .and_then(|_| self.editor_decode_ui_status(None))
        {
            items.push(TopbarActivityItem {
                label: status.message,
                progress: Some(status.progress),
                show_percentage: status.show_percentage,
                cancel: Some(TopbarActivityCancel::EditorDecode),
            });
        }
        if let Some(apply) = &self.editor_apply_state {
            items.push(TopbarActivityItem {
                label: apply.msg.clone(),
                progress: None,
                show_percentage: false,
                cancel: Some(TopbarActivityCancel::EditorApply),
            });
        }
        if let Some(state) = &self.plugin_process_state {
            items.push(TopbarActivityItem {
                label: if state.is_apply {
                    "Applying Plugin FX...".to_string()
                } else {
                    "Previewing Plugin FX...".to_string()
                },
                progress: None,
                show_percentage: false,
                cancel: Some(TopbarActivityCancel::PluginProcess),
            });
        }
        if let Some(proc) = &self.processing {
            if proc.started_at.elapsed() >= Duration::from_millis(120) {
                items.push(TopbarActivityItem {
                    label: proc.msg.clone(),
                    progress: None,
                    show_percentage: false,
                    cancel: Some(TopbarActivityCancel::Processing),
                });
            }
        }
        if effect_graph_apply.visible {
            let total = self.effect_graph.runner.total.max(1);
            let done = self.effect_graph.runner.done.min(total);
            let elapsed = effect_graph_apply.elapsed.as_secs_f32();
            let template_name = self
                .effect_graph
                .runner
                .template_stamp
                .as_ref()
                .map(|stamp| stamp.template_name.as_str())
                .unwrap_or("Effect Graph");
            items.push(TopbarActivityItem {
                label: format!("Effect Graph {template_name}: {done}/{total} ({elapsed:.1}s)"),
                progress: Some(done as f32 / total as f32),
                show_percentage: true,
                cancel: Some(TopbarActivityCancel::EffectGraph),
            });
        }
        if self.heavy_preview_expected_tool.is_some()
            || self.heavy_preview_rx.is_some()
            || self.heavy_overlay_rx.is_some()
        {
            let label = match self.heavy_preview_expected_tool {
                Some(ToolKind::PitchShift) => "Previewing PitchShift",
                Some(ToolKind::TimeStretch) => "Previewing TimeStretch",
                _ => "Previewing",
            };
            items.push(TopbarActivityItem {
                label: label.to_string(),
                progress: None,
                show_percentage: false,
                cancel: Some(TopbarActivityCancel::HeavyPreview),
            });
        }
        if self.music_preview_state.is_some() {
            items.push(TopbarActivityItem {
                label: "Previewing Music Analyze...".to_string(),
                progress: None,
                show_percentage: false,
                cancel: Some(TopbarActivityCancel::MusicPreview),
            });
        }
        if let Some(state) = &self.bulk_resample_state {
            let total = state.targets.len().max(1);
            let (label, pct) = if state.finalizing {
                (
                    format!("Resample finalize: {}/{}", state.after_index, total),
                    (state.after_index as f32 / total as f32).clamp(0.0, 1.0),
                )
            } else {
                (
                    format!("Resample: {}/{}", state.index, total),
                    (state.index as f32 / total as f32).clamp(0.0, 1.0),
                )
            };
            items.push(TopbarActivityItem {
                label,
                progress: Some(pct),
                show_percentage: true,
                cancel: Some(TopbarActivityCancel::BulkResample),
            });
        }
        if let Some(state) = &self.transcript_ai_state {
            let total = state.total.max(1);
            let done = state.done.min(total);
            let elapsed = state.started_at.elapsed().as_secs_f32();
            let remaining = state.total.saturating_sub(state.done);
            items.push(TopbarActivityItem {
                label: format!(
                    "Transcribing: {done}/{total} ({elapsed:.1}s) candidates:{} skip:{} rem:{}",
                    state.process_total, state.skipped_total, remaining
                ),
                progress: Some(done as f32 / total as f32),
                show_percentage: true,
                cancel: Some(TopbarActivityCancel::Transcript),
            });
        }
        if let Some(state) = &self.transcript_model_download_state {
            let total = state.total.max(1);
            let done = state.done.min(total);
            items.push(TopbarActivityItem {
                label: format!("Downloading transcript model... {done}/{total}"),
                progress: Some(done as f32 / total as f32),
                show_percentage: true,
                cancel: None,
            });
        }
        if let Some(state) = &self.music_ai_state {
            let total = state.total.max(1);
            let done = state.done.min(total);
            let elapsed = state.started_at.elapsed().as_secs_f32();
            items.push(TopbarActivityItem {
                label: format!("Music Analyze: {done}/{total} ({elapsed:.1}s)"),
                progress: Some(done as f32 / total as f32),
                show_percentage: true,
                cancel: Some(TopbarActivityCancel::Music),
            });
        }
        if let Some(state) = &self.music_model_download_state {
            let total = state.total.max(1);
            let done = state.done.min(total);
            items.push(TopbarActivityItem {
                label: format!("Downloading music model... {done}/{total}"),
                progress: Some(done as f32 / total as f32),
                show_percentage: true,
                cancel: None,
            });
        }
        if self.total_editor_analysis_inflight() > 0 {
            let (done, total) = self.total_editor_analysis_progress();
            items.push(TopbarActivityItem {
                label: if total > 0 {
                    format!("Analysis: {done}/{total}")
                } else {
                    format!("Analysis: {}", self.total_editor_analysis_inflight())
                },
                progress: (total > 0).then_some(done as f32 / total as f32),
                show_percentage: true,
                cancel: Some(TopbarActivityCancel::EditorAnalysis),
            });
        }
        if let Some(state) = &self.project_open_state {
            let elapsed = state.started_at.elapsed().as_secs_f32();
            items.push(TopbarActivityItem {
                label: format!("Opening session... ({elapsed:.1}s)"),
                progress: None,
                show_percentage: false,
                cancel: None,
            });
        }
        if let Some(export) = &self.export_state {
            items.push(TopbarActivityItem {
                label: export.msg.clone(),
                progress: None,
                show_percentage: false,
                cancel: None,
            });
        }
        if let Some(csv) = &self.csv_export_state {
            let total = csv.total.max(1);
            let done = csv.done.min(total);
            let elapsed = csv.started_at.elapsed().as_secs_f32();
            items.push(TopbarActivityItem {
                label: format!("CSV: {done}/{total} ({elapsed:.1}s)"),
                progress: Some(done as f32 / total as f32),
                show_percentage: true,
                cancel: None,
            });
        }
        if list_loading.visible {
            let elapsed = list_loading.elapsed.as_secs_f32();
            items.push(TopbarActivityItem {
                label: format!("Loading audio... ({elapsed:.1}s)"),
                progress: None,
                show_percentage: false,
                cancel: None,
            });
        }
        if sort_loading_visible
            && (self.sort_loading_started_at.is_some() || self.sort_loading_last_ms >= 50.0)
        {
            items.push(TopbarActivityItem {
                label: format!("Sorting... ({:.0} ms)", self.sort_loading_last_ms),
                progress: None,
                show_percentage: false,
                cancel: None,
            });
        }
        items
    }

    fn handle_topbar_activity_cancel(&mut self, cancel: TopbarActivityCancel) {
        match cancel {
            TopbarActivityCancel::Processing => self.cancel_processing(),
            TopbarActivityCancel::EditorDecode => self.cancel_editor_decode(),
            TopbarActivityCancel::EffectGraph => self.cancel_effect_graph_run(),
            TopbarActivityCancel::HeavyPreview => self.cancel_heavy_preview(),
            TopbarActivityCancel::MusicPreview => self.cancel_music_preview_run(),
            TopbarActivityCancel::EditorApply => self.cancel_editor_apply(),
            TopbarActivityCancel::PluginProcess => self.cancel_plugin_process(),
            TopbarActivityCancel::BulkResample => {
                if let Some(state) = &mut self.bulk_resample_state {
                    state.cancel_requested = true;
                }
            }
            TopbarActivityCancel::Transcript => self.cancel_transcript_ai_run(),
            TopbarActivityCancel::Music => self.cancel_music_analysis_run(),
            TopbarActivityCancel::EditorAnalysis => self.cancel_all_editor_analyses(),
        }
    }

    fn topbar_list_loading_status(&self) -> ListLoadingStatus {
        let loading = self.is_list_workspace_active()
            && (self.list_play_pending
                || self.list_preview_pending_path.is_some()
                || (self.list_preview_rx.is_some()
                    && self.playing_path.is_some()
                    && !self
                        .audio
                        .shared
                        .playing
                        .load(std::sync::atomic::Ordering::Relaxed)));
        let elapsed = self
            .debug
            .list_select_started_at
            .map(|t| t.elapsed())
            .unwrap_or_default();
        ListLoadingStatus {
            elapsed,
            visible: loading && elapsed >= Duration::from_millis(120),
        }
    }

    fn topbar_effect_graph_apply_status(&self) -> EffectGraphApplyStatus {
        let elapsed = self
            .effect_graph
            .runner
            .started_at
            .map(|t| t.elapsed())
            .unwrap_or_default();
        EffectGraphApplyStatus {
            elapsed,
            visible: self.effect_graph.runner.mode
                == Some(EffectGraphRunMode::ApplyToListSelection)
                && elapsed >= Duration::from_millis(120),
        }
    }

    fn ui_topbar_meter_group(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, compact: bool) {
        let group_w = if compact { 270.0 } else { 440.0 };
        ui.allocate_ui_with_layout(
            egui::vec2(group_w, 26.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                self.ui_topbar_volume_control(ui, ctx, compact);
                ui.separator();
                self.ui_topbar_output_meter(ui, compact);
            },
        );
    }

    fn ui_topbar_volume_control(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, compact: bool) {
        let width = if compact { 128.0 } else { 210.0 };
        let height = 22.0;
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
        self.topbar_volume_rect = Some(rect);
        if response.clicked() {
            response.request_focus();
        }

        let track_left = rect.left() + if compact { 28.0 } else { 62.0 };
        let track_right = rect.right() - if compact { 42.0 } else { 58.0 };
        let track_rect = egui::Rect::from_min_max(
            egui::pos2(track_left, rect.center().y - 4.0),
            egui::pos2(track_right.max(track_left + 24.0), rect.center().y + 4.0),
        );
        let mut changed = false;
        if (response.dragged() || response.clicked())
            && ctx.input(|i| i.pointer.interact_pos()).is_some()
        {
            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                let t = ((pos.x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0);
                let next = -80.0 + t * 86.0;
                if (next - self.volume_db).abs() >= 0.05 {
                    self.volume_db = next.clamp(-80.0, 6.0);
                    changed = true;
                }
            }
        }
        if response.has_focus() {
            let left = ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)
            });
            let right = ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)
            });
            if left || right {
                let delta = if right { 1.0 } else { -1.0 };
                let next = (self.volume_db + delta).clamp(-80.0, 6.0);
                if (next - self.volume_db).abs() >= f32::EPSILON {
                    self.volume_db = next;
                    changed = true;
                }
            }
            let nav_up = self.is_list_workspace_active()
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
            let nav_down = self.is_list_workspace_active()
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
            if nav_up || nav_down {
                let delta = if nav_down { 1 } else { -1 };
                self.ui_topbar_release_focus_to_list(ctx, &response, Some(delta));
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
                self.ui_topbar_release_focus_to_list(ctx, &response, None);
            }
        }
        if changed {
            self.apply_effective_volume();
        }

        let painter = ui.painter_at(rect);
        let text_col = if response.hovered() {
            Color32::from_rgb(220, 226, 232)
        } else {
            Color32::from_rgb(174, 180, 188)
        };
        let body_font = egui::TextStyle::Body.resolve(ui.style());
        let mono_font = egui::TextStyle::Monospace.resolve(ui.style());
        painter.text(
            egui::pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            if compact { "Vol" } else { "Volume" },
            body_font.clone(),
            text_col,
        );
        painter.rect_filled(track_rect, 3.0, Color32::from_rgb(24, 27, 31));
        let t = ((self.volume_db + 80.0) / 86.0).clamp(0.0, 1.0);
        let fill_rect = egui::Rect::from_min_max(
            track_rect.min,
            egui::pos2(track_rect.left() + track_rect.width() * t, track_rect.bottom()),
        );
        painter.rect_filled(fill_rect, 3.0, Color32::from_rgb(88, 196, 118));
        let stroke_col = if response.has_focus() {
            Color32::from_rgb(130, 190, 235)
        } else if response.hovered() {
            Color32::from_rgb(120, 150, 165)
        } else {
            Color32::from_rgb(70, 76, 84)
        };
        painter.rect_stroke(
            track_rect,
            3.0,
            egui::Stroke::new(1.0, stroke_col),
            egui::StrokeKind::Inside,
        );
        let knob_x = track_rect.left() + track_rect.width() * t;
        painter.circle_filled(
            egui::pos2(knob_x, track_rect.center().y),
            5.0,
            Color32::from_rgb(142, 224, 160),
        );
        painter.circle_stroke(
            egui::pos2(knob_x, track_rect.center().y),
            5.0,
            egui::Stroke::new(1.0, Color32::from_rgb(38, 52, 42)),
        );
        painter.text(
            egui::pos2(rect.right(), rect.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{:.0} dB", self.volume_db),
            mono_font,
            text_col,
        );
    }

    fn ui_topbar_output_meter(&mut self, ui: &mut egui::Ui, compact: bool) {
        ui.push_id("topbar_output_meter", |ui| {
            self.ui_topbar_output_meter_inner(ui, compact);
        });
    }

    fn ui_topbar_output_meter_inner(&mut self, ui: &mut egui::Ui, compact: bool) {
        let db = self.meter_db;
        let bar_w = if compact { 90.0 } else { 150.0 };
        let bar_h = 16.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::empty());
        self.topbar_output_meter_rect = Some(rect);
        let painter = ui.painter_at(rect);
        let track_rect = rect.shrink(1.0);
        painter.rect_filled(track_rect, 2.0, Color32::from_rgb(18, 18, 22));
        let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        if norm > 0.0 {
            let fill = egui::Rect::from_min_size(
                track_rect.min,
                egui::vec2(track_rect.width() * norm, track_rect.height()),
            );
            painter.rect_filled(fill, 2.0, Color32::from_rgb(100, 220, 120));
        }
        painter.rect_stroke(
            track_rect,
            2.0,
            egui::Stroke::new(1.0, Color32::GRAY),
            egui::StrokeKind::Inside,
        );
        let db_label = if db <= -79.9 {
            if compact {
                "-inf".to_string()
            } else {
                "-inf dBFS".to_string()
            }
        } else if compact {
            format!("{db:.0} dB")
        } else {
            format!("{db:.1} dBFS")
        };
        ui.label(RichText::new(db_label).monospace());
    }
}
