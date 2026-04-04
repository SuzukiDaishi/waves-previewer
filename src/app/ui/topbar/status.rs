use std::time::Duration;

use egui::{Align, Color32, RichText, Sense};

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

impl WavesPreviewer {
    pub(super) fn ui_topbar_status_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            self.ui_topbar_primary_status(ui, ctx);
            self.ui_topbar_activity_status(ui);
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                self.ui_topbar_output_meter(ui);
            });
        });
        ui.add_space(4.0);
    }

    fn ui_topbar_primary_status(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.playback_session.is_playing {
            ui.label(
                RichText::new("Playing")
                    .color(Color32::from_rgb(120, 220, 140))
                    .strong(),
            );
        }
        ui.label("Volume (dB)");
        let vol_resp = ui.add(egui::Slider::new(&mut self.volume_db, -80.0..=6.0));
        if vol_resp.changed() {
            self.apply_effective_volume();
        }
        let vol_up = if vol_resp.has_focus() && self.is_list_workspace_active() {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
        } else {
            false
        };
        let vol_down = if vol_resp.has_focus() && self.is_list_workspace_active() {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
        } else {
            false
        };
        if vol_up || vol_down {
            let delta = if vol_down { 1 } else { -1 };
            self.ui_topbar_release_focus_to_list(ctx, &vol_resp, Some(delta));
        }
        if vol_resp.has_focus()
            && ctx.input(|i| {
                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)
            })
        {
            self.ui_topbar_release_focus_to_list(ctx, &vol_resp, None);
        }

        let total_vis = self.files.len();
        let total_all = self.items.len();
        let dirty_gains = self.pending_gain_count();
        let sort_loading_visible = self.ui_topbar_sort_loading_visible();
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
                if sort_loading_visible {
                    ui.add(egui::Spinner::new());
                    ui.label(
                        RichText::new(format!("Sorting... ({:.0} ms)", self.sort_loading_last_ms))
                            .weak(),
                    );
                }
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

    fn ui_topbar_activity_status(&mut self, ui: &mut egui::Ui) {
        let sort_loading_visible = self.ui_topbar_sort_loading_visible();
        let list_loading = self.topbar_list_loading_status();
        let effect_graph_apply = self.topbar_effect_graph_apply_status();
        let show_activity = self.scan_in_progress
            || self.processing.is_some()
            || self.editor_decode_state.is_some()
            || self.heavy_preview_rx.is_some()
            || self.heavy_overlay_rx.is_some()
            || self.music_preview_state.is_some()
            || self.editor_apply_state.is_some()
            || self.transcript_ai_state.is_some()
            || self.transcript_model_download_state.is_some()
            || self.music_ai_state.is_some()
            || self.music_model_download_state.is_some()
            || self.export_state.is_some()
            || self.csv_export_state.is_some()
            || self.total_editor_analysis_inflight() > 0
            || self.project_open_state.is_some()
            || self.bulk_resample_state.is_some()
            || list_loading.visible
            || effect_graph_apply.visible
            || sort_loading_visible;
        if !show_activity {
            return;
        }

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            self.ui_topbar_scan_activity(ui);
            self.ui_topbar_sort_activity(ui, sort_loading_visible);
            self.ui_topbar_processing_activity(ui);
            self.ui_topbar_editor_decode_activity(ui);
            self.ui_topbar_list_loading_activity(ui, &list_loading);
            self.ui_topbar_effect_graph_activity(ui, &effect_graph_apply);
            self.ui_topbar_preview_activity(ui);
            self.ui_topbar_apply_and_export_activity(ui);
            self.ui_topbar_bulk_resample_activity(ui);
            self.ui_topbar_transcript_activity(ui);
            self.ui_topbar_music_activity(ui);
            self.ui_topbar_editor_analysis_activity(ui);
            self.ui_topbar_project_open_activity(ui);
        });
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
            visible: self.effect_graph.runner.mode == Some(EffectGraphRunMode::ApplyToListSelection)
                && elapsed >= Duration::from_millis(120),
        }
    }

    fn ui_topbar_scan_activity(&mut self, ui: &mut egui::Ui) {
        if !self.scan_in_progress {
            return;
        }
        let elapsed = self
            .scan_started_at
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);
        ui.add(egui::Spinner::new());
        ui.label(
            RichText::new(format!("Scanning: {} files ({:.1}s)", self.scan_found_count, elapsed))
                .weak(),
        );
    }

    fn ui_topbar_sort_activity(&mut self, ui: &mut egui::Ui, visible: bool) {
        if !visible {
            return;
        }
        ui.add(egui::Spinner::new());
        ui.label(RichText::new(format!("Sorting... ({:.0} ms)", self.sort_loading_last_ms)).weak());
    }

    fn ui_topbar_processing_activity(&mut self, ui: &mut egui::Ui) {
        let Some(proc) = &self.processing else {
            return;
        };
        if proc.started_at.elapsed() < Duration::from_millis(120) {
            return;
        }
        ui.add(egui::Spinner::new());
        ui.label(RichText::new(proc.msg.as_str()).weak());
        if ui.button("Cancel").clicked() {
            self.cancel_processing();
        }
    }

    fn ui_topbar_editor_decode_activity(&mut self, ui: &mut egui::Ui) {
        if !self
            .editor_decode_state
            .as_ref()
            .map(|state| state.started_at.elapsed() >= Duration::from_millis(120))
            .unwrap_or(false)
        {
            return;
        }
        if let Some(status) = self.editor_decode_ui_status(None) {
            ui.add(egui::Spinner::new());
            ui.label(RichText::new(status.message).weak());
            let mut bar = egui::ProgressBar::new(status.progress).desired_width(60.0);
            if status.show_percentage {
                bar = bar.show_percentage();
            }
            ui.add(bar);
            if ui.button("Cancel").clicked() {
                self.cancel_editor_decode();
            }
        }
    }

    fn ui_topbar_list_loading_activity(&mut self, ui: &mut egui::Ui, status: &ListLoadingStatus) {
        if !status.visible {
            return;
        }
        let elapsed = status.elapsed.as_secs_f32();
        ui.add(egui::Spinner::new());
        let label = if elapsed >= 0.1 {
            format!("Loading audio... ({elapsed:.1}s)")
        } else {
            "Loading audio...".to_string()
        };
        ui.label(RichText::new(label).weak());
    }

    fn ui_topbar_effect_graph_activity(
        &mut self,
        ui: &mut egui::Ui,
        status: &EffectGraphApplyStatus,
    ) {
        if !status.visible {
            return;
        }
        let elapsed = status.elapsed.as_secs_f32();
        let total = self.effect_graph.runner.total.max(1);
        let done = self.effect_graph.runner.done.min(total);
        let template_name = self
            .effect_graph
            .runner
            .template_stamp
            .as_ref()
            .map(|stamp| stamp.template_name.as_str())
            .unwrap_or("Effect Graph");
        let current_label = self
            .effect_graph
            .runner
            .current_path
            .as_ref()
            .and_then(|path| path.file_name().and_then(|name| name.to_str()))
            .map(|name| format!(": {name}"))
            .unwrap_or_default();
        ui.add(egui::Spinner::new());
        ui.label(
            RichText::new(format!(
                "Effect Graph {template_name}: {done}/{total} ({elapsed:.1}s){current_label}"
            ))
            .weak(),
        );
        ui.add(
            egui::ProgressBar::new(done as f32 / total as f32)
                .desired_width(80.0)
                .show_percentage(),
        );
        if ui.button("Cancel").clicked() {
            self.cancel_effect_graph_run();
        }
    }

    fn ui_topbar_preview_activity(&mut self, ui: &mut egui::Ui) {
        if let Some(tool) = &self.heavy_preview_expected_tool {
            ui.add(egui::Spinner::new());
            let message = match tool {
                ToolKind::PitchShift => "Previewing PitchShift",
                ToolKind::TimeStretch => "Previewing TimeStretch",
                _ => "Previewing",
            };
            ui.label(RichText::new(message).weak());
            if ui.button("Cancel").clicked() {
                self.cancel_heavy_preview();
            }
            return;
        }
        if self.heavy_preview_rx.is_some() || self.heavy_overlay_rx.is_some() {
            ui.add(egui::Spinner::new());
            ui.label(RichText::new("Previewing...").weak());
            if ui.button("Cancel").clicked() {
                self.cancel_heavy_preview();
            }
            return;
        }
        if self.music_preview_state.is_some() {
            ui.add(egui::Spinner::new());
            ui.label(RichText::new("Previewing Music Analyze...").weak());
            if ui.button("Cancel").clicked() {
                self.cancel_music_preview_run();
            }
        }
    }

    fn ui_topbar_apply_and_export_activity(&mut self, ui: &mut egui::Ui) {
        if let Some(apply) = &self.editor_apply_state {
            ui.add(egui::Spinner::new());
            ui.label(RichText::new(apply.msg.as_str()).weak());
            if ui.button("Cancel").clicked() {
                self.cancel_editor_apply();
            }
        } else if let Some(state) = &self.plugin_process_state {
            ui.add(egui::Spinner::new());
            let message = if state.is_apply {
                "Applying Plugin FX..."
            } else {
                "Previewing Plugin FX..."
            };
            ui.label(RichText::new(message).weak());
            if ui.button("Cancel").clicked() {
                self.cancel_plugin_process();
            }
        }
        if let Some(export) = &self.export_state {
            ui.add(egui::Spinner::new());
            ui.label(RichText::new(export.msg.as_str()).weak());
        }
        if let Some(csv) = &self.csv_export_state {
            ui.add(egui::Spinner::new());
            if csv.total > 0 {
                let elapsed = csv.started_at.elapsed().as_secs_f32();
                let pct = (csv.done as f32 / csv.total as f32).clamp(0.0, 1.0);
                ui.label(
                    RichText::new(format!(
                        "CSV: {}/{} ({:.0}%, {:.1}s)",
                        csv.done,
                        csv.total,
                        pct * 100.0,
                        elapsed
                    ))
                    .weak(),
                );
            } else {
                ui.label(RichText::new("CSV: preparing").weak());
            }
        }
    }

    fn ui_topbar_bulk_resample_activity(&mut self, ui: &mut egui::Ui) {
        let Some(state) = &mut self.bulk_resample_state else {
            return;
        };
        let total = state.targets.len().max(1);
        let (label, pct) = if state.finalizing {
            let pct = (state.after_index as f32 / total as f32).clamp(0.0, 1.0);
            (format!("Resample finalize: {}/{}", state.after_index, total), pct)
        } else {
            let pct = (state.index as f32 / total as f32).clamp(0.0, 1.0);
            (format!("Resample: {}/{}", state.index, total), pct)
        };
        ui.add(egui::Spinner::new());
        ui.label(RichText::new(label).weak());
        ui.add(egui::ProgressBar::new(pct).desired_width(60.0).show_percentage());
        if ui.button("Cancel").clicked() {
            state.cancel_requested = true;
        }
    }

    fn ui_topbar_transcript_activity(&mut self, ui: &mut egui::Ui) {
        if let Some(state) = &self.transcript_ai_state {
            ui.add(egui::Spinner::new());
            let elapsed = state.started_at.elapsed().as_secs_f32();
            let canceling = state
                .cancel_requested
                .load(std::sync::atomic::Ordering::Relaxed);
            let remaining = state.total.saturating_sub(state.done);
            let label = if state.total > 0 {
                let prefix = if canceling {
                    "Transcribing (canceling)"
                } else {
                    "Transcribing"
                };
                format!(
                    "{prefix}: {}/{} ({:.1}s) candidates:{} skip:{} rem:{}",
                    state.done,
                    state.total,
                    elapsed,
                    state.process_total,
                    state.skipped_total,
                    remaining
                )
            } else {
                format!("Transcribing... ({elapsed:.1}s)")
            };
            ui.label(RichText::new(label).weak());
            if ui.button("Cancel").clicked() {
                self.cancel_transcript_ai_run();
            }
        }
        if let Some(state) = &self.transcript_model_download_state {
            ui.add(egui::Spinner::new());
            let total = state.total.max(1);
            let done = state.done.min(total);
            ui.label(RichText::new(format!("Downloading transcript model... {done}/{total}")).weak());
            ui.add(
                egui::ProgressBar::new(done as f32 / total as f32)
                    .desired_width(80.0)
                    .show_percentage(),
            );
        }
        if let Some(err) = &self.transcript_ai_last_error {
            ui.label(
                RichText::new(format!("Transcript: {err}"))
                    .color(Color32::from_rgb(255, 120, 120)),
            );
        }
    }

    fn ui_topbar_music_activity(&mut self, ui: &mut egui::Ui) {
        if let Some(state) = &self.music_ai_state {
            ui.add(egui::Spinner::new());
            let elapsed = state.started_at.elapsed().as_secs_f32();
            let canceling = state
                .cancel_requested
                .load(std::sync::atomic::Ordering::Relaxed);
            let prefix = if canceling {
                "Music Analyze (canceling)"
            } else {
                "Music Analyze"
            };
            ui.label(
                RichText::new(format!("{prefix}: {}/{} ({elapsed:.1}s)", state.done, state.total))
                    .weak(),
            );
            if ui.button("Cancel").clicked() {
                self.cancel_music_analysis_run();
            }
        }
        if let Some(state) = &self.music_model_download_state {
            ui.add(egui::Spinner::new());
            let total = state.total.max(1);
            let done = state.done.min(total);
            ui.label(
                RichText::new(format!("Downloading music analyze model... {done}/{total}")).weak(),
            );
            ui.add(
                egui::ProgressBar::new(done as f32 / total as f32)
                    .desired_width(80.0)
                    .show_percentage(),
            );
        }
        if let Some(err) = &self.music_ai_last_error {
            ui.label(
                RichText::new(format!("Music Analyze: {err}"))
                    .color(Color32::from_rgb(255, 120, 120)),
            );
        }
    }

    fn ui_topbar_editor_analysis_activity(&mut self, ui: &mut egui::Ui) {
        if self.total_editor_analysis_inflight() == 0 {
            return;
        }
        ui.add(egui::Spinner::new());
        let (done, total) = self.total_editor_analysis_progress();
        let label = if total > 0 {
            let pct = ((done as f32 / total as f32) * 100.0).clamp(0.0, 100.0);
            format!("Analysis: {} ({pct:.0}%)", self.total_editor_analysis_inflight())
        } else {
            format!("Analysis: {}", self.total_editor_analysis_inflight())
        };
        ui.label(RichText::new(label).weak());
        if ui.button("Cancel").clicked() {
            self.cancel_all_editor_analyses();
        }
    }

    fn ui_topbar_project_open_activity(&mut self, ui: &mut egui::Ui) {
        let Some(state) = &self.project_open_state else {
            return;
        };
        let elapsed = state.started_at.elapsed().as_secs_f32();
        ui.add(egui::Spinner::new());
        ui.label(RichText::new(format!("Opening session... ({elapsed:.1}s)")).weak());
    }

    fn ui_topbar_output_meter(&mut self, ui: &mut egui::Ui) {
        let db = self.meter_db;
        let bar_w = 200.0;
        let bar_h = 16.0;
        let (rect, painter) = ui.allocate_painter(egui::vec2(bar_w, bar_h), Sense::hover());
        painter.rect_stroke(
            rect.rect,
            2.0,
            egui::Stroke::new(1.0, Color32::GRAY),
            egui::StrokeKind::Inside,
        );
        let norm = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let fill = egui::Rect::from_min_size(rect.rect.min, egui::vec2(bar_w * norm, bar_h));
        painter.rect_filled(fill, 0.0, Color32::from_rgb(100, 220, 120));
        let db_label = if db <= -79.9 {
            "-inf dBFS".to_string()
        } else {
            format!("{db:.1} dBFS")
        };
        ui.label(RichText::new(db_label).monospace());
    }
}
