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
    Inspection,
    BatchLoudnorm,
    Transcript,
    Music,
    VariationAudition,
    DuplicateScan,
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
                    .color(self.palette().playing_text)
                    .strong(),
            );
        }

        let total_vis = self.files.len();
        let total_all = self.items.len();
        let dirty_gains = self.pending_gain_count_throttled();
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
                Some(ToolKind::Speed) => "Previewing Speed",
                Some(ToolKind::SpectralWarp) => "Previewing Spectral Warp",
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
        if let Some(state) = &self.batch_loudnorm_state {
            let total = state.targets.len().max(1);
            let (label, pct) = match state.phase {
                crate::app::types::LoudnormPhase::Measure => {
                    let done = total - state.pending.len().min(total);
                    (
                        format!("Loudness measure: {}/{}", done, total),
                        done as f32 / total as f32,
                    )
                }
                crate::app::types::LoudnormPhase::Apply => (
                    format!("Loudness apply: {}/{}", state.apply_index, total),
                    state.apply_index as f32 / total as f32,
                ),
            };
            items.push(TopbarActivityItem {
                label,
                progress: Some(pct.clamp(0.0, 1.0)),
                show_percentage: true,
                cancel: Some(TopbarActivityCancel::BatchLoudnorm),
            });
        }
        if let Some(state) = &self.inspection_run_state {
            items.push(TopbarActivityItem {
                label: format!("Inspecting: {}/{}", state.done, state.total.max(1)),
                progress: Some((state.done as f32 / state.total.max(1) as f32).clamp(0.0, 1.0)),
                show_percentage: true,
                cancel: Some(TopbarActivityCancel::Inspection),
            });
        }
        if let Some(state) = &self.duplicate_scan_state {
            items.push(TopbarActivityItem {
                label: format!("Duplicates: {}/{}", state.done, state.total.max(1)),
                progress: Some((state.done as f32 / state.total.max(1) as f32).clamp(0.0, 1.0)),
                show_percentage: true,
                cancel: Some(TopbarActivityCancel::DuplicateScan),
            });
        }
        if let Some(state) = &self.variation_audition {
            let mode = match state.mode {
                crate::app::types::VariationAuditionMode::RoundRobin => "RR",
                crate::app::types::VariationAuditionMode::Random => "Rnd",
            };
            items.push(TopbarActivityItem {
                label: format!(
                    "Audition {}/{} ({mode})",
                    state.cursor + 1,
                    state.paths.len()
                ),
                progress: None,
                show_percentage: false,
                cancel: Some(TopbarActivityCancel::VariationAudition),
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
            TopbarActivityCancel::Inspection => {
                self.cancel_inspection_run();
            }
            TopbarActivityCancel::BatchLoudnorm => {
                self.cancel_batch_loudnorm();
            }
            TopbarActivityCancel::Transcript => self.cancel_transcript_ai_run(),
            TopbarActivityCancel::Music => self.cancel_music_analysis_run(),
            TopbarActivityCancel::VariationAudition => {
                self.cancel_variation_audition();
                self.audio.stop();
            }
            TopbarActivityCancel::DuplicateScan => self.cancel_duplicate_scan(),
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
                if !compact {
                    self.ui_topbar_loudness_readout(ui);
                }
            },
        );
    }

    /// Realtime BS.1770 readout fed by the metering thread: momentary /
    /// short-term LUFS and 4x-oversampled true peak. "-" while idle.
    fn ui_topbar_loudness_readout(&mut self, ui: &mut egui::Ui) {
        let decode = |v: i32| {
            (v != crate::audio::METER_VALUE_INVALID).then(|| v as f32 / 100.0)
        };
        let m = decode(
            self.audio
                .shared
                .lufs_m_milli
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        let s = decode(
            self.audio
                .shared
                .lufs_s_milli
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        let tp = decode(
            self.audio
                .shared
                .true_peak_db_milli
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        let text = Self::format_loudness_readout(m, s, tp);
        ui.label(egui::RichText::new(text).monospace().size(10.5).weak())
            .on_hover_text(
                "Realtime loudness of what's playing: M = momentary LUFS (400 ms), \
                 S = short-term LUFS (3 s), TP = true peak (dBTP, 4x oversampled)",
            );
        if m.is_some() || s.is_some() || tp.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    pub(crate) fn format_loudness_readout(
        m: Option<f32>,
        s: Option<f32>,
        tp: Option<f32>,
    ) -> String {
        let fmt = |v: Option<f32>| match v {
            Some(v) if v <= -99.0 => "-inf".to_string(),
            Some(v) => format!("{v:+.1}"),
            None => "-".to_string(),
        };
        format!("M {}  S {}  TP {}", fmt(m), fmt(s), fmt(tp))
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
        let palette = self.palette();
        let text_col = if response.hovered() {
            palette.slider_label
        } else {
            palette.slider_label_weak
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
        painter.rect_filled(track_rect, 3.0, palette.slider_track);
        let t = ((self.volume_db + 80.0) / 86.0).clamp(0.0, 1.0);
        let fill_rect = egui::Rect::from_min_max(
            track_rect.min,
            egui::pos2(track_rect.left() + track_rect.width() * t, track_rect.bottom()),
        );
        painter.rect_filled(fill_rect, 3.0, palette.slider_fill);
        let stroke_col = if response.has_focus() {
            palette.slider_value_text
        } else if response.hovered() {
            palette.slider_value_text_weak
        } else {
            palette.slider_knob_stroke
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
            palette.meter_text,
        );
        painter.circle_stroke(
            egui::pos2(knob_x, track_rect.center().y),
            5.0,
            egui::Stroke::new(1.0, palette.meter_text_outline),
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
        let palette = self.palette();
        let track_rect = rect.shrink(1.0);
        painter.rect_filled(track_rect, 2.0, palette.meter_track);
        let norm_of = |db: f32| ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let ch_count = self.meter_ch_db.len();
        if ch_count >= 2 {
            // Per-output-channel sub-bars (RMS fill + peak-hold tick).
            let sub_h = track_rect.height() / ch_count as f32;
            for (i, &(rms_db, _peak_db)) in self.meter_ch_db.iter().enumerate() {
                let top = track_rect.top() + sub_h * i as f32;
                let sub = egui::Rect::from_min_max(
                    egui::pos2(track_rect.left(), top + 0.5),
                    egui::pos2(track_rect.right(), top + sub_h - 0.5),
                );
                let n = norm_of(rms_db);
                if n > 0.0 {
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            sub.min,
                            egui::vec2(sub.width() * n, sub.height()),
                        ),
                        1.0,
                        palette.meter_fill,
                    );
                }
                if let Some(&hold_db) = self.meter_ch_hold_db.get(i) {
                    let hn = norm_of(hold_db);
                    if hn > 0.01 {
                        let x = sub.left() + sub.width() * hn;
                        painter.line_segment(
                            [egui::pos2(x, sub.top()), egui::pos2(x, sub.bottom())],
                            egui::Stroke::new(1.0, palette.meter_peak_tick),
                        );
                    }
                }
            }
        } else {
            let norm = norm_of(db);
            if norm > 0.0 {
                let fill = egui::Rect::from_min_size(
                    track_rect.min,
                    egui::vec2(track_rect.width() * norm, track_rect.height()),
                );
                painter.rect_filled(fill, 2.0, palette.meter_fill);
            }
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

#[cfg(test)]
mod loudness_readout_tests {
    #[test]
    fn formats_values_and_placeholders() {
        let f = crate::app::WavesPreviewer::format_loudness_readout;
        assert_eq!(f(None, None, None), "M -  S -  TP -");
        assert_eq!(
            f(Some(-23.06), Some(-22.94), Some(-1.2)),
            "M -23.1  S -22.9  TP -1.2"
        );
        assert_eq!(f(Some(-120.0), None, Some(0.0)), "M -inf  S -  TP +0.0");
    }
}
