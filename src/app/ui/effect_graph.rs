use egui::{Color32, Frame, RichText, Sense, Stroke, StrokeKind};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::app::helpers::db_to_color;
use crate::app::types::{
    EffectGraphBitDepth, EffectGraphCombineMode, EffectGraphDebugPreview, EffectGraphNodeCategory,
    EffectGraphNodeData, EffectGraphNodeKind, EffectGraphNodeRunPhase, EffectGraphPlaybackTarget,
    EffectGraphPortDirection, EffectGraphPortKey, EffectGraphResampleQuality, EffectGraphSeverity,
    EffectGraphSpectrumMode,
};

const EFFECT_GRAPH_MONITOR_DOWNMIX_NOTE: &str = "Preview monitor downmixes >2ch to stereo";

fn world_to_screen(
    canvas_rect: egui::Rect,
    pan: egui::Vec2,
    zoom: f32,
    world: [f32; 2],
) -> egui::Pos2 {
    canvas_rect.min + pan + egui::vec2(world[0] * zoom, world[1] * zoom)
}

fn screen_to_world(
    canvas_rect: egui::Rect,
    pan: egui::Vec2,
    zoom: f32,
    screen: egui::Pos2,
) -> [f32; 2] {
    let delta = screen - canvas_rect.min - pan;
    [delta.x / zoom.max(0.01), delta.y / zoom.max(0.01)]
}

fn cubic_point(
    p0: egui::Pos2,
    p1: egui::Pos2,
    p2: egui::Pos2,
    p3: egui::Pos2,
    t: f32,
) -> egui::Pos2 {
    let u = 1.0 - t;
    let tt = t * t;
    let uu = u * u;
    let uuu = uu * u;
    let ttt = tt * t;
    egui::pos2(
        uuu * p0.x + 3.0 * uu * t * p1.x + 3.0 * u * tt * p2.x + ttt * p3.x,
        uuu * p0.y + 3.0 * uu * t * p1.y + 3.0 * u * tt * p2.y + ttt * p3.y,
    )
}

fn format_console_timestamp(unix_ms: u64) -> String {
    let total_seconds = (unix_ms / 1000) % 86_400;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn format_time_axis_label(seconds: f32) -> String {
    if seconds < 1.0 {
        format!("{:.0} ms", seconds * 1000.0)
    } else {
        format!("{seconds:.2} s")
    }
}

fn format_frequency_axis_label(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.1} kHz", hz / 1000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

/// Pin label resolved from the node's port spec. Falls back to parsing the
/// raw port-id string for ports that no longer exist on the node (dangling
/// edges from old sessions).
fn effect_graph_pin_label(
    data: &EffectGraphNodeData,
    direction: EffectGraphPortDirection,
    port_id: &str,
) -> String {
    data.spec()
        .port(direction, port_id)
        .map(|port| port.label.to_string())
        .unwrap_or_else(|| effect_graph_port_label(port_id))
}

fn effect_graph_port_label(port_id: &str) -> String {
    if let Some(number) = port_id
        .strip_prefix("ch")
        .or_else(|| {
            let number = port_id.strip_prefix("out")?;
            (!number.is_empty()).then_some(number)
        })
        .or_else(|| port_id.strip_prefix("in"))
    {
        if number.chars().all(|ch| ch.is_ascii_digit()) {
            return number.to_string();
        }
    }
    port_id.to_string()
}

fn effect_graph_port_sort_index(port_id: &str) -> usize {
    effect_graph_port_label(port_id)
        .parse::<usize>()
        .ok()
        .and_then(|value| value.checked_sub(1))
        .unwrap_or(usize::MAX)
}

fn apply_preview_scroll(
    ctx: &egui::Context,
    response: &egui::Response,
    scroll_x: &mut f32,
    _zoom: f32,
) {
    if response.dragged_by(egui::PointerButton::Primary) {
        let drag_delta = ctx.input(|i| i.pointer.delta().x);
        if drag_delta.abs() > 0.0 {
            *scroll_x =
                (*scroll_x - drag_delta / response.rect.width().max(64.0) * 0.18).clamp(0.0, 1.0);
        }
    }
    if response.hovered() {
        let raw_scroll = ctx.input(|i| i.smooth_scroll_delta);
        let dominant_scroll = if raw_scroll.x.abs() > raw_scroll.y.abs() {
            raw_scroll.x
        } else {
            -raw_scroll.y
        };
        if dominant_scroll.abs() > 0.0 {
            *scroll_x = (*scroll_x - dominant_scroll / response.rect.width().max(64.0) * 0.012)
                .clamp(0.0, 1.0);
        }
    }
}

fn draw_waveform_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    mono: &[f32],
    sample_rate: u32,
    zoom: f32,
    scroll_x: f32,
) {
    painter.rect_filled(rect, 6.0, Color32::from_rgb(20, 24, 30));
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, Color32::from_rgb(44, 54, 68)),
        StrokeKind::Inside,
    );
    if mono.is_empty() {
        return;
    }
    let axis_h = 18.0;
    let plot_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.max.y - axis_h));
    if plot_rect.height() <= 8.0 {
        return;
    }
    let zoom = zoom.clamp(1.0, 32.0);
    let visible_len = ((mono.len() as f32) / zoom).round() as usize;
    let visible_len = visible_len.clamp(64, mono.len().max(64));
    let max_start = mono.len().saturating_sub(visible_len);
    let start = ((max_start as f32) * scroll_x.clamp(0.0, 1.0)).round() as usize;
    let end = (start + visible_len).min(mono.len());
    let bins = plot_rect.width().round().max(48.0) as usize;
    let mut minmax = Vec::new();
    crate::wave::build_minmax(&mut minmax, &mono[start..end], bins);
    if minmax.is_empty() {
        return;
    }
    let center_y = plot_rect.center().y;
    painter.line_segment(
        [
            egui::pos2(plot_rect.left(), center_y),
            egui::pos2(plot_rect.right(), center_y),
        ],
        Stroke::new(1.0, Color32::from_rgb(42, 70, 96)),
    );
    let width = plot_rect.width() / minmax.len().max(1) as f32;
    for (index, (lo, hi)) in minmax.iter().enumerate() {
        let x = plot_rect.left() + (index as f32 + 0.5) * width;
        let y0 = egui::lerp(
            plot_rect.bottom()..=plot_rect.top(),
            ((*lo).clamp(-1.0, 1.0) + 1.0) * 0.5,
        );
        let y1 = egui::lerp(
            plot_rect.bottom()..=plot_rect.top(),
            ((*hi).clamp(-1.0, 1.0) + 1.0) * 0.5,
        );
        painter.line_segment(
            [egui::pos2(x, y0), egui::pos2(x, y1)],
            Stroke::new(width.max(1.0), Color32::from_rgb(86, 214, 186)),
        );
    }

    painter.line_segment(
        [
            egui::pos2(plot_rect.left(), plot_rect.bottom()),
            egui::pos2(plot_rect.right(), plot_rect.bottom()),
        ],
        Stroke::new(1.0, Color32::from_rgb(58, 66, 78)),
    );
    let total_duration = mono.len() as f32 / sample_rate.max(1) as f32;
    let visible_duration = (end.saturating_sub(start)) as f32 / sample_rate.max(1) as f32;
    let start_time = start as f32 / sample_rate.max(1) as f32;
    let tick_count = 5usize;
    for tick in 0..tick_count {
        let frac = tick as f32 / (tick_count.saturating_sub(1).max(1)) as f32;
        let x = egui::lerp(plot_rect.left()..=plot_rect.right(), frac);
        painter.line_segment(
            [
                egui::pos2(x, plot_rect.bottom()),
                egui::pos2(x, plot_rect.bottom() + 4.0),
            ],
            Stroke::new(1.0, Color32::from_rgb(84, 96, 108)),
        );
        let label = format_time_axis_label(start_time + visible_duration * frac);
        painter.text(
            egui::pos2(x, rect.bottom() - 1.0),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::TextStyle::Small.resolve(&painter.ctx().global_style()),
            Color32::from_rgb(146, 160, 176),
        );
    }
    painter.text(
        egui::pos2(plot_rect.right() - 4.0, plot_rect.top() + 4.0),
        egui::Align2::RIGHT_TOP,
        format!(
            "visible {:.2}s / total {:.2}s",
            visible_duration, total_duration
        ),
        egui::TextStyle::Small.resolve(&painter.ctx().global_style()),
        Color32::from_rgb(118, 132, 148),
    );
}

fn draw_spectrum_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    spectrogram: &crate::app::types::SpectrogramData,
    mode: EffectGraphSpectrumMode,
    zoom: f32,
    scroll_x: f32,
) {
    painter.rect_filled(rect, 6.0, Color32::from_rgb(20, 24, 30));
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, Color32::from_rgb(44, 54, 68)),
        StrokeKind::Inside,
    );
    if spectrogram.frames == 0
        || spectrogram.bins == 0
        || rect.width() <= 2.0
        || rect.height() <= 2.0
    {
        return;
    }
    let axis_left = 48.0;
    let axis_bottom = 18.0;
    let plot_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + axis_left, rect.min.y),
        egui::pos2(rect.max.x, rect.max.y - axis_bottom),
    );
    if plot_rect.width() <= 8.0 || plot_rect.height() <= 8.0 {
        return;
    }
    let zoom = zoom.clamp(1.0, 16.0);
    let visible_frames = ((spectrogram.frames as f32) / zoom).round() as usize;
    let visible_frames = visible_frames.clamp(8, spectrogram.frames.max(8));
    let max_start_frame = spectrogram.frames.saturating_sub(visible_frames);
    let start_frame = ((max_start_frame as f32) * scroll_x.clamp(0.0, 1.0)).round() as usize;
    let end_frame = (start_frame + visible_frames).min(spectrogram.frames);
    let visible_frames = end_frame.saturating_sub(start_frame).max(1);
    let target_w = plot_rect.width().round().clamp(32.0, 220.0) as usize;
    let target_h = plot_rect.height().round().clamp(24.0, 120.0) as usize;
    let cell_w = plot_rect.width() / target_w as f32;
    let cell_h = plot_rect.height() / target_h as f32;
    let max_bin = spectrogram.bins.saturating_sub(1).max(1);
    let max_freq = (spectrogram.sample_rate.max(1) as f32 * 0.5).max(1.0);
    let mel_max = 2595.0 * (1.0 + max_freq / 700.0).log10();
    let log_min = 20.0_f32.min(max_freq).max(1.0);
    let y_from_freq = |freq: f32| -> f32 {
        let frac = match mode {
            EffectGraphSpectrumMode::Linear => (freq / max_freq).clamp(0.0, 1.0),
            EffectGraphSpectrumMode::Log => {
                if freq <= 0.0 || max_freq <= log_min {
                    0.0
                } else {
                    let f = freq.clamp(log_min, max_freq);
                    (f / log_min).ln() / (max_freq / log_min).ln()
                }
            }
            EffectGraphSpectrumMode::Mel => {
                let mel = 2595.0 * (1.0 + (freq / 700.0)).log10();
                (mel / mel_max.max(1.0)).clamp(0.0, 1.0)
            }
        };
        egui::lerp(plot_rect.bottom()..=plot_rect.top(), frac)
    };
    for x in 0..target_w {
        let frame =
            start_frame + ((x * visible_frames) / target_w).min(visible_frames.saturating_sub(1));
        let base = frame * spectrogram.bins;
        for y in 0..target_h {
            let frac = y as f32 / target_h.saturating_sub(1).max(1) as f32;
            let bin = match mode {
                EffectGraphSpectrumMode::Linear => {
                    ((frac * max_bin as f32).round() as usize).min(max_bin)
                }
                EffectGraphSpectrumMode::Log => {
                    let freq = if max_freq <= log_min {
                        frac * max_freq
                    } else {
                        let ratio = max_freq / log_min;
                        log_min * ratio.powf(frac)
                    };
                    ((freq / max_freq) * max_bin as f32).round() as usize
                }
                EffectGraphSpectrumMode::Mel => {
                    let mel = mel_max * frac;
                    let freq = 700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0);
                    ((freq / max_freq) * max_bin as f32).round() as usize
                }
            };
            let db = spectrogram
                .values_db
                .get(base + bin.min(max_bin))
                .copied()
                .unwrap_or(-120.0)
                .clamp(-120.0, 6.0);
            let x0 = plot_rect.left() + x as f32 * cell_w;
            let y0 = plot_rect.bottom() - (y as f32 + 1.0) * cell_h;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(x0, y0),
                    egui::vec2(cell_w + 0.5, cell_h + 0.5),
                ),
                0.0,
                db_to_color(db),
            );
        }
    }

    painter.line_segment(
        [
            egui::pos2(plot_rect.left(), plot_rect.bottom()),
            egui::pos2(plot_rect.right(), plot_rect.bottom()),
        ],
        Stroke::new(1.0, Color32::from_rgb(58, 66, 78)),
    );
    painter.line_segment(
        [
            egui::pos2(plot_rect.left(), plot_rect.top()),
            egui::pos2(plot_rect.left(), plot_rect.bottom()),
        ],
        Stroke::new(1.0, Color32::from_rgb(58, 66, 78)),
    );
    let frame_step_seconds = spectrogram.frame_step as f32 / spectrogram.sample_rate.max(1) as f32;
    let visible_duration = visible_frames as f32 * frame_step_seconds;
    let start_time = start_frame as f32 * frame_step_seconds;
    let tick_count = 5usize;
    for tick in 0..tick_count {
        let frac = tick as f32 / (tick_count.saturating_sub(1).max(1)) as f32;
        let x = egui::lerp(plot_rect.left()..=plot_rect.right(), frac);
        painter.line_segment(
            [
                egui::pos2(x, plot_rect.bottom()),
                egui::pos2(x, plot_rect.bottom() + 4.0),
            ],
            Stroke::new(1.0, Color32::from_rgb(84, 96, 108)),
        );
        painter.text(
            egui::pos2(x, rect.bottom() - 1.0),
            egui::Align2::CENTER_BOTTOM,
            format_time_axis_label(start_time + visible_duration * frac),
            egui::TextStyle::Small.resolve(&painter.ctx().global_style()),
            Color32::from_rgb(146, 160, 176),
        );
    }
    for freq in [
        0.0,
        max_freq * 0.25,
        max_freq * 0.5,
        max_freq * 0.75,
        max_freq,
    ] {
        let y = y_from_freq(freq.max(1.0));
        painter.line_segment(
            [
                egui::pos2(plot_rect.left() - 4.0, y),
                egui::pos2(plot_rect.left(), y),
            ],
            Stroke::new(1.0, Color32::from_rgb(84, 96, 108)),
        );
        painter.text(
            egui::pos2(plot_rect.left() - 6.0, y),
            egui::Align2::RIGHT_CENTER,
            format_frequency_axis_label(freq),
            egui::TextStyle::Small.resolve(&painter.ctx().global_style()),
            Color32::from_rgb(146, 160, 176),
        );
    }
}

impl crate::app::WavesPreviewer {
    /// Renders a plugin probe's error / generic-fallback-warning / backend-log
    /// status, shared by the EffectGraph PluginFx node and the Editor's
    /// PluginFx tool so both surface probe failures identically.
    pub(in crate::app) fn ui_plugin_probe_status(
        ui: &mut egui::Ui,
        error: Option<&str>,
        backend_note: Option<&str>,
        backend_log: Option<&str>,
    ) {
        if let Some(err) = error {
            ui.label(
                RichText::new(err)
                    .small()
                    .color(Color32::from_rgb(240, 120, 120)),
            );
        }
        if let Some(note) = backend_note {
            Frame::NONE
                .fill(Color32::from_rgb(80, 60, 20))
                .inner_margin(egui::Margin::symmetric(8, 4))
                .corner_radius(4.0)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("⚠ Generic fallback:")
                                .small()
                                .color(Color32::from_rgb(255, 200, 60)),
                        );
                        ui.label(
                            RichText::new(note.trim())
                                .small()
                                .color(Color32::from_rgb(220, 180, 100)),
                        );
                    });
                });
        }
        if let Some(log) = backend_log {
            ui.collapsing("Backend Log", |ui| {
                ui.label(
                    RichText::new(log)
                        .small()
                        .monospace()
                        .color(Color32::from_rgb(160, 176, 192)),
                );
            });
        }
    }

    pub(in crate::app) fn ui_effect_graph_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.request_plugin_scan_if_needed();
        self.handle_effect_graph_shortcuts(ctx);
        self.ui_effect_graph_unsaved_prompt(ctx);

        // Side panels first so the console only spans the canvas area, then a
        // height-clamped console under the canvas. All three are drag-resizable.
        egui::Panel::right("effect_graph_tester_panel")
            .resizable(true)
            .default_size(self.effect_graph.right_panel_width)
            .size_range(egui::Rangef::new(220.0, 520.0))
            .show_inside(ui, |ui| {
                self.effect_graph.right_panel_width = ui.max_rect().width();
                self.ui_effect_graph_tester(ui);
            });

        egui::Panel::left("effect_graph_library_panel")
            .resizable(true)
            .default_size(self.effect_graph.left_panel_width)
            .size_range(egui::Rangef::new(180.0, 460.0))
            .show_inside(ui, |ui| {
                self.effect_graph.left_panel_width = ui.max_rect().width();
                ui.vertical(|ui| {
                    self.ui_effect_graph_templates_panel(ui);
                    ui.separator();
                    self.ui_effect_graph_node_palette(ui);
                });
            });

        let console_max = (ui.available_height() * 0.45).max(120.0);
        egui::Panel::bottom("effect_graph_console_panel")
            .resizable(true)
            .default_size(self.effect_graph.bottom_panel_height.clamp(90.0, console_max))
            .size_range(egui::Rangef::new(72.0, console_max))
            .show_inside(ui, |ui| {
                self.ui_effect_graph_console(ui);
            });

        self.ui_effect_graph_canvas(ui, ctx);
    }

    fn handle_effect_graph_shortcuts(&mut self, ctx: &egui::Context) {
        if !self.is_effect_graph_workspace_active() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)) {
            self.effect_graph_remove_selected_items();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.effect_graph.canvas.connecting_from_port = None;
            self.effect_graph.canvas.drag_palette_kind = None;
            self.effect_graph.canvas.selected_edge_id = None;
            self.effect_graph.canvas.background_panning = false;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F)) {
            if let Some(node_id) = self
                .effect_graph
                .canvas
                .selected_nodes
                .iter()
                .next()
                .cloned()
            {
                self.effect_graph.canvas.focus_node_id = Some(node_id);
            }
        }
    }

    fn effect_graph_playback_active(&self, target: EffectGraphPlaybackTarget) -> bool {
        self.effect_graph.tester.playback_target == Some(target)
            && self
                .audio
                .shared
                .playing
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(in crate::app) fn effect_graph_toggle_playback(
        &mut self,
        target: EffectGraphPlaybackTarget,
        audio: Arc<crate::audio::AudioBuffer>,
    ) {
        if self.effect_graph_playback_active(target) {
            self.audio.stop();
            self.effect_graph.tester.playback_target = None;
            return;
        }
        self.audio.stop();
        self.audio.set_samples_buffer(audio);
        self.playback_mark_buffer_source(
            crate::app::PlaybackSourceKind::EffectGraph,
            self.audio.shared.out_sample_rate.max(1),
        );
        self.effect_graph.tester.playback_target = Some(target);
        if self.playback_mode_needs_fx_buffer() && !self.spawn_playback_fx_render(true) {
            return;
        }
        self.audio.play();
    }

    fn effect_graph_play_button_label(
        &self,
        target: EffectGraphPlaybackTarget,
        play_label: &'static str,
    ) -> &'static str {
        if self.effect_graph_playback_active(target) {
            "Stop"
        } else if target == EffectGraphPlaybackTarget::Input
            && self.effect_graph.input_preview_worker_state.rx.is_some()
        {
            "Loading..."
        } else {
            play_label
        }
    }

    fn ui_effect_graph_templates_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Templates");
            if ui.button("Reload").clicked() {
                self.load_effect_graph_library();
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.effect_graph.library.new_template_name)
                    .hint_text("New template name"),
            );
            if ui.button("New").clicked() {
                let name = self
                    .effect_graph
                    .library
                    .new_template_name
                    .trim()
                    .to_string();
                let chosen = if name.is_empty() { None } else { Some(name) };
                self.effect_graph_new_unsaved_template(chosen);
                self.workspace_view = crate::app::types::WorkspaceView::EffectGraph;
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                let _ = self.save_effect_graph_draft(false);
            }
            if ui.button("Save As New").clicked() {
                let _ = self.save_effect_graph_draft(true);
            }
            let can_delete = self
                .effect_graph
                .active_template_id
                .as_ref()
                .and_then(|id| self.effect_graph_entry_by_id(id))
                .is_some();
            if ui
                .add_enabled(can_delete, egui::Button::new("Delete"))
                .clicked()
            {
                if let Some(template_id) = self.effect_graph.active_template_id.clone() {
                    self.request_delete_effect_graph_template(&template_id);
                }
            }
        });
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() * 0.55).max(120.0))
            .show(ui, |ui| {
                let active = self.effect_graph.active_template_id.clone();
                let entries = self.effect_graph.library.entries.clone();
                for entry in entries {
                    let mut label = entry.name.clone();
                    if !entry.valid {
                        label.push_str(" (invalid)");
                    }
                    let selected = active.as_deref() == Some(entry.template_id.as_str());
                    let text = if entry.valid {
                        RichText::new(label)
                    } else {
                        RichText::new(label).color(Color32::from_rgb(220, 120, 120))
                    };
                    if ui.selectable_label(selected, text).clicked() {
                        self.request_switch_effect_graph_template(&entry.template_id);
                    }
                }
            });
        ui.separator();
        ui.label(
            RichText::new(format!(
                "Draft: {}{}",
                self.effect_graph.draft.name,
                if self.effect_graph.draft_dirty {
                    " *"
                } else {
                    ""
                }
            ))
            .strong(),
        );
        if self.effect_graph_has_errors() {
            ui.label(
                RichText::new("Validation errors present").color(Color32::from_rgb(230, 110, 110)),
            );
        }
    }

    fn ui_effect_graph_node_palette(&mut self, ui: &mut egui::Ui) {
        ui.heading("Nodes");
        // Palette entries derive from the node spec table; new kinds show up
        // automatically once their spec declares a category.
        let kinds_in = |category: EffectGraphNodeCategory| {
            EffectGraphNodeKind::ALL
                .into_iter()
                .filter(move |kind| kind.spec().category == category)
        };
        let canvas_world = self
            .effect_graph
            .canvas
            .last_canvas_pointer_world
            .unwrap_or([140.0, 140.0]);
        let mut add_button = |ui: &mut egui::Ui, kind: EffectGraphNodeKind| {
            let spec = kind.spec();
            let can_add = match spec.kind {
                EffectGraphNodeKind::Input => !self
                    .effect_graph
                    .draft
                    .nodes
                    .iter()
                    .any(|node| matches!(&node.data, EffectGraphNodeData::Input)),
                EffectGraphNodeKind::Output => !self
                    .effect_graph
                    .draft
                    .nodes
                    .iter()
                    .any(|node| matches!(&node.data, EffectGraphNodeData::Output)),
                _ => true,
            };
            let label = spec.display_name;
            let resp = ui.add_enabled(can_add, egui::Button::new(label));
            if resp.clicked() {
                let _ = self.effect_graph_add_node(kind, canvas_world);
            }
            if resp.drag_started() {
                self.effect_graph.canvas.drag_palette_kind = Some(kind);
            }
        };
        ui.label(RichText::new("Effects").strong());
        for kind in kinds_in(EffectGraphNodeCategory::Standard) {
            add_button(ui, kind);
        }
        ui.separator();
        ui.label(RichText::new("Debug").strong());
        ui.label(
            RichText::new("Run Test only. Batch apply skips these nodes.")
                .small()
                .color(Color32::from_rgb(140, 154, 168)),
        );
        for kind in kinds_in(EffectGraphNodeCategory::Debug) {
            add_button(ui, kind);
        }
        ui.separator();
        ui.label(RichText::new("Routing").strong());
        for kind in kinds_in(EffectGraphNodeCategory::Routing) {
            add_button(ui, kind);
        }
        if self.effect_graph.canvas.drag_palette_kind.is_some() {
            ui.label(RichText::new("Release over canvas to add").italics());
        }
    }

    fn ui_effect_graph_tester(&mut self, ui: &mut egui::Ui) {
        let predicted_output_format = self.effect_graph_predicted_output_format().ok();
        let predicted_output_summary = predicted_output_format
            .as_ref()
            .map(|predicted| predicted.summary.clone());
        let show_monitor_downmix_note = predicted_output_format
            .as_ref()
            .map(|predicted| predicted.channel_count > 2)
            .unwrap_or(false)
            || self
                .effect_graph
                .tester
                .last_output_bus
                .as_ref()
                .map(|bus| bus.channels.len() > 2)
                .unwrap_or(false);
        ui.heading("Test");
        ui.label("Target audio");
        let target_edit = ui.add(egui::TextEdit::singleline(
            &mut self.effect_graph.tester.target_path_input,
        ));
        if target_edit.changed() {
            self.effect_graph.tester.target_path = None;
            self.invalidate_effect_graph_input_preview();
            self.invalidate_effect_graph_prediction_cache();
        }
        ui.horizontal(|ui| {
            if ui.button("Use Current").clicked() {
                self.effect_graph_use_current_selection_target();
            }
            if ui.button("Browse").clicked() {
                if let Some(path) = self
                    .pick_files_dialog()
                    .and_then(|mut files| files.drain(..).next())
                {
                    self.effect_graph.tester.target_path = Some(path.clone());
                    self.effect_graph.tester.target_path_input = path.display().to_string();
                    self.invalidate_effect_graph_input_preview();
                    self.invalidate_effect_graph_prediction_cache();
                }
            }
        });
        ui.horizontal(|ui| {
            let can_run =
                !self.effect_graph_has_errors() && self.effect_graph.runner.mode.is_none();
            if ui
                .add_enabled(can_run, egui::Button::new("Run Test"))
                .clicked()
            {
                if let Err(err) = self.start_effect_graph_test_run() {
                    self.effect_graph.tester.last_error = Some(err.clone());
                    self.push_effect_graph_console(EffectGraphSeverity::Error, "test", err, None);
                }
            }
            if ui
                .add_enabled(
                    self.effect_graph.runner.mode.is_some(),
                    egui::Button::new("Stop"),
                )
                .clicked()
            {
                self.cancel_effect_graph_run();
            }
        });
        ui.horizontal(|ui| {
            let can_apply = self
                .effect_graph
                .active_template_id
                .as_ref()
                .and_then(|id| self.effect_graph_entry_by_id(id))
                .is_some();
            if ui
                .add_enabled(can_apply, egui::Button::new("Apply Selected"))
                .clicked()
            {
                if let Some(template_id) = self.effect_graph.active_template_id.clone() {
                    let selected = self.selected_paths();
                    let _ = self.apply_effect_graph_template_to_paths(&template_id, &selected);
                }
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    true,
                    egui::Button::new(self.effect_graph_play_button_label(
                        EffectGraphPlaybackTarget::Input,
                        "Play Input",
                    )),
                )
                .clicked()
            {
                if let Err(err) = self.start_effect_graph_input_preview(true) {
                    self.effect_graph.tester.last_error = Some(err.clone());
                    self.push_effect_graph_console(EffectGraphSeverity::Error, "input", err, None);
                }
            }
            if ui
                .add_enabled(
                    self.effect_graph.tester.last_output_audio.is_some(),
                    egui::Button::new(self.effect_graph_play_button_label(
                        EffectGraphPlaybackTarget::Output,
                        "Play Output",
                    )),
                )
                .clicked()
            {
                if let Some(audio) = self.effect_graph.tester.last_output_audio.clone() {
                    self.effect_graph_toggle_playback(EffectGraphPlaybackTarget::Output, audio);
                }
            }
        });
        if self.effect_graph_uses_embedded_sample() {
            ui.label(
                RichText::new(
                    "No target selected. Using embedded sample (10s chirp + white noise).",
                )
                .color(Color32::from_rgb(150, 190, 255)),
            );
        }
        if let Some(summary) = predicted_output_summary {
            ui.label(RichText::new(summary).color(Color32::from_rgb(150, 190, 255)));
        }
        if show_monitor_downmix_note {
            ui.label(
                RichText::new(EFFECT_GRAPH_MONITOR_DOWNMIX_NOTE)
                    .small()
                    .color(Color32::from_rgb(118, 132, 148)),
            );
        }
        if let Some(ms) = self.effect_graph.tester.last_run_ms {
            ui.label(format!("Last run: {ms:.1} ms"));
        }
        if !self.effect_graph.tester.last_output_summary.is_empty() {
            ui.label(format!(
                "Actual: {}",
                self.effect_graph.tester.last_output_summary
            ));
        }
        if let Some(err) = self.effect_graph.tester.last_error.as_ref() {
            ui.label(RichText::new(err).color(Color32::from_rgb(230, 110, 110)));
        }
        if let Some(path) = self.effect_graph.runner.current_path.as_ref() {
            ui.separator();
            ui.label(format!(
                "Running: {} ({}/{})",
                path.display(),
                self.effect_graph
                    .runner
                    .done
                    .min(self.effect_graph.runner.total),
                self.effect_graph.runner.total
            ));
        }
    }

    fn ui_effect_graph_console(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Console").strong());
            let issues = self.effect_graph.validation.len();
            if issues > 0 {
                ui.label(
                    RichText::new(format!("{issues} issue(s)"))
                        .color(Color32::from_rgb(255, 210, 120))
                        .small(),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Clear").clicked() {
                    self.effect_graph.console.lines.clear();
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(false)
            .show(ui, |ui| {
                for issue in self.effect_graph.validation.clone() {
                    let color = match issue.severity {
                        EffectGraphSeverity::Info => Color32::from_rgb(150, 190, 255),
                        EffectGraphSeverity::Warning => Color32::from_rgb(255, 210, 120),
                        EffectGraphSeverity::Error => Color32::from_rgb(240, 120, 120),
                    };
                    ui.horizontal(|ui| {
                        if let Some(node_id) = issue.node_id.clone() {
                            if ui.small_button("Go").clicked() {
                                self.effect_graph.canvas.focus_node_id = Some(node_id);
                            }
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("[{}] {}", issue.code, issue.message))
                                    .monospace()
                                    .color(color),
                            )
                            .truncate()
                            .show_tooltip_when_elided(true),
                        );
                    });
                }
                for line in self.effect_graph.console.lines.iter().rev() {
                    let color = match line.severity {
                        EffectGraphSeverity::Info => Color32::from_rgb(170, 182, 196),
                        EffectGraphSeverity::Warning => Color32::from_rgb(255, 210, 120),
                        EffectGraphSeverity::Error => Color32::from_rgb(240, 120, 120),
                    };
                    ui.horizontal(|ui| {
                        if let Some(node_id) = line.node_id.clone() {
                            if ui.small_button("Go").clicked() {
                                self.effect_graph.canvas.focus_node_id = Some(node_id);
                            }
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!(
                                    "{}  {:<6} {}",
                                    format_console_timestamp(line.timestamp_unix_ms),
                                    line.scope,
                                    line.message
                                ))
                                .monospace()
                                .color(color),
                            )
                            .truncate()
                            .show_tooltip_when_elided(true),
                        );
                    });
                }
            });
    }

    fn ui_effect_graph_canvas(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let (canvas_rect, canvas_resp) =
            ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        ui.set_clip_rect(canvas_rect);
        canvas_resp.context_menu(|ui| {
            if ui.button("Auto Layout").clicked() {
                self.effect_graph_tidy_layout();
                ui.close();
            }
        });
        let painter = ui.painter_at(canvas_rect);
        painter.rect_filled(canvas_rect, 10.0, Color32::from_rgb(18, 20, 24));

        let zoom = self.effect_graph.canvas.zoom.clamp(0.35, 2.0);
        let mut pan = egui::vec2(
            self.effect_graph.canvas.pan[0],
            self.effect_graph.canvas.pan[1],
        );
        let node_hit_rects: Vec<egui::Rect> = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .map(|node| {
                let min = world_to_screen(canvas_rect, pan, zoom, node.ui_pos);
                let size = egui::vec2(node.ui_size[0] * zoom, node.ui_size[1] * zoom);
                egui::Rect::from_min_size(min, size)
            })
            .collect();
        let pointer_press_origin = ctx.input(|i| i.pointer.press_origin());
        let pressed_on_node = pointer_press_origin
            .filter(|pointer| canvas_rect.contains(*pointer))
            .map(|pointer| node_hit_rects.iter().any(|rect| rect.contains(pointer)))
            .unwrap_or(false);
        if canvas_resp.drag_started_by(egui::PointerButton::Middle) {
            self.effect_graph_push_undo_snapshot();
        }
        if canvas_resp.drag_started_by(egui::PointerButton::Primary) && !pressed_on_node {
            self.effect_graph_push_undo_snapshot();
            self.effect_graph.canvas.background_panning = true;
        }
        if canvas_resp.dragged_by(egui::PointerButton::Middle) {
            pan += ctx.input(|i| i.pointer.delta());
            self.effect_graph.canvas.pan = [pan.x, pan.y];
            ctx.request_repaint();
        }
        if self.effect_graph.canvas.background_panning
            && canvas_resp.dragged_by(egui::PointerButton::Primary)
        {
            pan += ctx.input(|i| i.pointer.delta());
            self.effect_graph.canvas.pan = [pan.x, pan.y];
            ctx.request_repaint();
        }
        if self.effect_graph.canvas.background_panning && !ctx.input(|i| i.pointer.primary_down()) {
            self.effect_graph.canvas.background_panning = false;
        }
        if canvas_resp.hovered() && ctx.input(|i| i.modifiers.command) {
            let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                self.effect_graph_push_undo_snapshot();
                self.effect_graph.canvas.zoom = (zoom * (1.0 + scroll * 0.001)).clamp(0.35, 2.0);
            }
        }
        if let Some(pointer) = ctx.pointer_hover_pos() {
            if canvas_rect.contains(pointer) {
                self.effect_graph.canvas.last_canvas_pointer_world =
                    Some(screen_to_world(canvas_rect, pan, zoom, pointer));
            }
        }
        if let Some(node_id) = self.effect_graph.canvas.focus_node_id.clone() {
            if let Some(node) = self
                .effect_graph
                .draft
                .nodes
                .iter()
                .find(|node| node.id == node_id)
            {
                let center_world = [
                    node.ui_pos[0] + node.ui_size[0] * 0.5,
                    node.ui_pos[1] + node.ui_size[1] * 0.5,
                ];
                let center_screen = world_to_screen(canvas_rect, pan, zoom, center_world);
                pan += canvas_rect.center() - center_screen;
                self.effect_graph.canvas.pan = [pan.x, pan.y];
                self.effect_graph.canvas.focus_node_id = None;
            }
        }
        let input_sources = self.effect_graph_input_sources();
        let flow_hints = self.effect_graph_flow_hints();
        let combine_modes = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .filter_map(|node| {
                self.effect_graph_combine_mode_hint(node, &input_sources, &flow_hints)
                    .map(|mode| (node.id.clone(), mode))
            })
            .collect::<HashMap<_, _>>();
        let combine_slot_labels = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .map(|node| {
                (
                    node.id.clone(),
                    self.effect_graph_combine_slot_labels(node, &input_sources, &flow_hints),
                )
            })
            .collect::<HashMap<_, _>>();
        let combine_display_labels = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .map(|node| {
                (
                    node.id.clone(),
                    self.effect_graph_combine_display_labels(node, &input_sources, &flow_hints),
                )
            })
            .collect::<HashMap<_, _>>();
        let predicted_output_format = self.effect_graph_predicted_output_format().ok();
        let predicted_output_summary = predicted_output_format
            .as_ref()
            .map(|predicted| predicted.summary.clone());
        let show_monitor_downmix_note = predicted_output_format
            .as_ref()
            .map(|predicted| predicted.channel_count > 2)
            .unwrap_or(false)
            || self
                .effect_graph
                .tester
                .last_output_bus
                .as_ref()
                .map(|bus| bus.channels.len() > 2)
                .unwrap_or(false);
        let split_live_width = self
            .effect_graph
            .tester
            .last_input_bus
            .as_ref()
            .map(|bus| bus.channel_layout.declared_width.max(bus.channels.len()))
            .unwrap_or(0);

        let minor = 24.0 * zoom;
        let major = minor * 4.0;
        let mut x = canvas_rect.left() + pan.x.rem_euclid(minor);
        while x <= canvas_rect.right() {
            painter.line_segment(
                [
                    egui::pos2(x, canvas_rect.top()),
                    egui::pos2(x, canvas_rect.bottom()),
                ],
                Stroke::new(
                    if ((x - canvas_rect.left()) / major).fract().abs() < 0.01 {
                        1.2
                    } else {
                        1.0
                    },
                    if ((x - canvas_rect.left()) / major).fract().abs() < 0.01 {
                        Color32::from_rgb(40, 48, 58)
                    } else {
                        Color32::from_rgb(28, 32, 38)
                    },
                ),
            );
            x += minor;
        }
        let mut y = canvas_rect.top() + pan.y.rem_euclid(minor);
        while y <= canvas_rect.bottom() {
            painter.line_segment(
                [
                    egui::pos2(canvas_rect.left(), y),
                    egui::pos2(canvas_rect.right(), y),
                ],
                Stroke::new(
                    if ((y - canvas_rect.top()) / major).fract().abs() < 0.01 {
                        1.2
                    } else {
                        1.0
                    },
                    if ((y - canvas_rect.top()) / major).fract().abs() < 0.01 {
                        Color32::from_rgb(40, 48, 58)
                    } else {
                        Color32::from_rgb(28, 32, 38)
                    },
                ),
            );
            y += minor;
        }

        let port_positions = |node: &crate::app::types::EffectGraphNode,
                              rect: egui::Rect|
         -> (
            Vec<(EffectGraphPortKey, egui::Pos2)>,
            Vec<(EffectGraphPortKey, egui::Pos2)>,
        ) {
            let ordered_input_ports = if matches!(&node.data, EffectGraphNodeData::CombineChannels)
                && matches!(
                    combine_modes.get(&node.id),
                    Some(EffectGraphCombineMode::Restore | EffectGraphCombineMode::Adaptive)
                ) {
                let slot_labels = combine_slot_labels.get(&node.id);
                let mut ports = node
                    .data
                    .input_ports()
                    .iter()
                    .map(|port| port.id.to_string())
                    .collect::<Vec<_>>();
                ports.sort_by(|left, right| {
                    let left_slot = slot_labels.and_then(|labels| labels.get(left).copied());
                    let right_slot = slot_labels.and_then(|labels| labels.get(right).copied());
                    (
                        left_slot.is_none(),
                        left_slot.unwrap_or(usize::MAX),
                        effect_graph_port_sort_index(left),
                        left.as_str(),
                    )
                        .cmp(&(
                            right_slot.is_none(),
                            right_slot.unwrap_or(usize::MAX),
                            effect_graph_port_sort_index(right),
                            right.as_str(),
                        ))
                });
                ports
            } else {
                node.data
                    .input_ports()
                    .iter()
                    .map(|port| port.id.to_string())
                    .collect::<Vec<_>>()
            };
            let ordered_output_ports = node
                .data
                .output_ports()
                .iter()
                .map(|port| port.id.to_string())
                .collect::<Vec<_>>();
            let distribute = |ports: &[String], x: f32| -> Vec<(EffectGraphPortKey, egui::Pos2)> {
                if ports.is_empty() {
                    return Vec::new();
                }
                let top = rect.top() + 36.0;
                let bottom = rect.bottom() - 16.0;
                let usable_height = (bottom - top).max(4.0);
                let step = if ports.len() == 1 {
                    0.0
                } else {
                    usable_height / (ports.len() as f32 - 1.0)
                };
                ports
                    .iter()
                    .enumerate()
                    .map(|(index, port_id)| {
                        let y = if ports.len() == 1 {
                            rect.center().y
                        } else {
                            top + step * index as f32
                        };
                        (
                            EffectGraphPortKey {
                                node_id: node.id.clone(),
                                port_id: port_id.clone(),
                            },
                            egui::pos2(x, y),
                        )
                    })
                    .collect()
            };
            (
                distribute(&ordered_input_ports, rect.left()),
                distribute(&ordered_output_ports, rect.right()),
            )
        };

        let mut input_pins: Vec<(EffectGraphPortKey, egui::Pos2)> = Vec::new();
        let mut output_pins: Vec<(EffectGraphPortKey, egui::Pos2)> = Vec::new();
        for node in self.effect_graph.draft.nodes.iter() {
            let min = world_to_screen(canvas_rect, pan, zoom, node.ui_pos);
            let size = egui::vec2(node.ui_size[0] * zoom, node.ui_size[1] * zoom);
            let rect = egui::Rect::from_min_size(min, size);
            let (node_inputs, node_outputs) = port_positions(node, rect);
            input_pins.extend(node_inputs);
            output_pins.extend(node_outputs);
        }

        for edge in self.effect_graph.draft.edges.iter() {
            let Some((_, start)) = output_pins.iter().find(|(key, _)| {
                key.node_id == edge.from_node_id && key.port_id == edge.from_port_id
            }) else {
                continue;
            };
            let Some((_, end)) = input_pins
                .iter()
                .find(|(key, _)| key.node_id == edge.to_node_id && key.port_id == edge.to_port_id)
            else {
                continue;
            };
            let c1 = egui::pos2(start.x + 64.0, start.y);
            let c2 = egui::pos2(end.x - 64.0, end.y);
            let mut points = Vec::with_capacity(24);
            for step in 0..24 {
                points.push(cubic_point(*start, c1, c2, *end, step as f32 / 23.0));
            }
            // Dark underlay gives cables depth against the grid.
            painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(4.0, Color32::from_black_alpha(120)),
            ));
            painter.add(egui::Shape::line(
                points,
                Stroke::new(2.0, Color32::from_rgb(110, 170, 255)),
            ));
        }

        if let Some(from_port) = self.effect_graph.canvas.connecting_from_port.clone() {
            if let Some((_, start)) = output_pins.iter().find(|(key, _)| key == &from_port) {
                if let Some(pointer) = ctx.pointer_hover_pos() {
                    let c1 = egui::pos2(start.x + 64.0, start.y);
                    let c2 = egui::pos2(pointer.x - 64.0, pointer.y);
                    let mut points = Vec::with_capacity(24);
                    for step in 0..24 {
                        points.push(cubic_point(*start, c1, c2, pointer, step as f32 / 23.0));
                    }
                    painter.add(egui::Shape::line(
                        points,
                        Stroke::new(2.0, Color32::from_rgb(255, 196, 96)),
                    ));
                }
            }
        }

        let mut pending_connect: Option<(String, String, String, String)> = None;
        let mut clear_connect = false;
        let mut pending_plugin_load_from_file: Option<String> = None;
        for idx in 0..self.effect_graph.draft.nodes.len() {
            let node = self.effect_graph.draft.nodes[idx].clone();
            let min = world_to_screen(canvas_rect, pan, zoom, node.ui_pos);
            let size = egui::vec2(node.ui_size[0] * zoom, node.ui_size[1] * zoom);
            let rect = egui::Rect::from_min_size(min, size);
            let (node_input_pins, node_output_pins) = port_positions(&node, rect);
            let title_h = 28.0 * zoom.min(1.1);
            let title_rect =
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + title_h));
            let body_rect =
                egui::Rect::from_min_max(egui::pos2(rect.min.x, title_rect.max.y), rect.max);
            let selected = self.effect_graph.canvas.selected_nodes.contains(&node.id);
            let debug_preview = self.effect_graph.debug_previews.get(&node.id).cloned();
            let combine_mode = combine_modes.get(&node.id).copied();
            let node_display_labels = combine_display_labels.get(&node.id);
            let mut debug_scroll_x = self
                .effect_graph
                .debug_view_state
                .get(&node.id)
                .map(|state| state.scroll_x)
                .unwrap_or(0.0);
            let status = self.effect_graph.runner.node_status.get(&node.id);
            let accent = match &node.data {
                EffectGraphNodeData::Input => Color32::from_rgb(56, 184, 168),
                EffectGraphNodeData::Output => Color32::from_rgb(240, 184, 82),
                EffectGraphNodeData::PluginFx { .. } => Color32::from_rgb(214, 156, 98),
                EffectGraphNodeData::DebugWaveform { .. } => Color32::from_rgb(144, 206, 130),
                EffectGraphNodeData::DebugSpectrum { .. } => Color32::from_rgb(188, 142, 232),
                EffectGraphNodeData::Duplicate
                | EffectGraphNodeData::SplitChannels
                | EffectGraphNodeData::CombineChannels => Color32::from_rgb(110, 206, 180),
                _ => Color32::from_rgb(88, 164, 220),
            };
            let border = match status.map(|status| status.phase) {
                Some(EffectGraphNodeRunPhase::Running) => Color32::from_rgb(255, 196, 96),
                Some(EffectGraphNodeRunPhase::Failed) => Color32::from_rgb(232, 92, 92),
                Some(EffectGraphNodeRunPhase::Success) => Color32::from_rgb(110, 210, 144),
                _ => accent,
            };
            let corner = 8.0 * zoom.clamp(0.6, 1.2);
            // Soft drop shadow lifts cards off the grid.
            painter.rect_filled(
                rect.translate(egui::vec2(0.0, 3.0)).expand(1.5),
                corner + 2.0,
                Color32::from_black_alpha(90),
            );
            let body_fill = Color32::from_rgb(30, 33, 40);
            let header_fill = crate::app::helpers::lerp_color(
                Color32::from_rgb(36, 40, 48),
                accent,
                0.22,
            );
            painter.rect_filled(rect, corner, body_fill);
            painter.rect_filled(
                title_rect,
                egui::CornerRadius {
                    nw: corner as u8,
                    ne: corner as u8,
                    sw: 0,
                    se: 0,
                },
                header_fill,
            );
            // Accent underline separates header from body.
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 1.0, title_rect.bottom()),
                    egui::pos2(rect.right() - 1.0, title_rect.bottom()),
                ],
                Stroke::new(2.0, accent),
            );
            painter.rect_stroke(
                rect,
                corner,
                Stroke::new(if selected { 2.5 } else { 1.2 }, border),
                StrokeKind::Outside,
            );
            if selected {
                painter.rect_stroke(
                    rect.expand(3.0),
                    corner + 3.0,
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(border.r(), border.g(), border.b(), 90)),
                    StrokeKind::Outside,
                );
            }
            painter.text(
                egui::pos2(title_rect.left() + 10.0, title_rect.center().y),
                egui::Align2::LEFT_CENTER,
                node.data.display_name(),
                egui::TextStyle::Button.resolve(ui.style()),
                Color32::from_rgb(235, 240, 246),
            );
            if let Some(status) = status {
                if let Some(elapsed_ms) = status.elapsed_ms {
                    let badge_text = format!("{elapsed_ms:.0} ms");
                    let font = egui::TextStyle::Small.resolve(ui.style());
                    let galley = painter.layout_no_wrap(
                        badge_text.clone(),
                        font.clone(),
                        Color32::from_rgb(214, 224, 234),
                    );
                    let pad = egui::vec2(6.0, 2.0);
                    let badge_rect = egui::Rect::from_min_size(
                        egui::pos2(
                            title_rect.right() - galley.size().x - pad.x * 2.0 - 6.0,
                            title_rect.center().y - galley.size().y * 0.5 - pad.y,
                        ),
                        galley.size() + pad * 2.0,
                    );
                    painter.rect_filled(badge_rect, badge_rect.height() * 0.5, Color32::from_black_alpha(110));
                    painter.galley(badge_rect.min + pad, galley, Color32::from_rgb(214, 224, 234));
                }
            }

            let node_click_resp = ui.interact(
                rect,
                ui.id().with(("effect_graph_node_click", &node.id)),
                Sense::click(),
            );
            if node_click_resp.clicked() {
                self.effect_graph.canvas.selected_nodes.clear();
                self.effect_graph
                    .canvas
                    .selected_nodes
                    .insert(node.id.clone());
                self.effect_graph.canvas.selected_edge_id = None;
            }

            let title_resp = ui.interact(
                title_rect,
                ui.id().with(("effect_graph_title", &node.id)),
                Sense::click_and_drag(),
            );
            if title_resp.drag_started() {
                self.effect_graph_push_undo_snapshot();
            }
            if title_resp.dragged() {
                let delta = ctx.input(|i| i.pointer.delta()) / zoom.max(0.01);
                if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                    node_mut.ui_pos[0] += delta.x;
                    node_mut.ui_pos[1] += delta.y;
                }
                self.effect_graph.draft_dirty = true;
                ctx.request_repaint();
            }

            for (port_key, pin_pos) in node_input_pins.iter() {
                painter.circle_filled(*pin_pos, 7.0, Color32::from_rgb(18, 20, 24));
                painter.circle_filled(*pin_pos, 5.0, Color32::from_rgb(196, 212, 228));
                painter.circle_stroke(*pin_pos, 7.0, Stroke::new(1.0, Color32::from_rgb(70, 80, 92)));
                let input_label = if matches!(&node.data, EffectGraphNodeData::CombineChannels)
                    && matches!(
                        combine_mode,
                        Some(EffectGraphCombineMode::Restore | EffectGraphCombineMode::Adaptive)
                    ) {
                    node_display_labels
                        .and_then(|labels| labels.get(&port_key.port_id).cloned())
                        .unwrap_or_else(|| {
                            effect_graph_pin_label(
                                &node.data,
                                EffectGraphPortDirection::Input,
                                &port_key.port_id,
                            )
                        })
                } else {
                    effect_graph_pin_label(
                        &node.data,
                        EffectGraphPortDirection::Input,
                        &port_key.port_id,
                    )
                };
                painter.text(
                    egui::pos2(pin_pos.x + 10.0, pin_pos.y),
                    egui::Align2::LEFT_CENTER,
                    input_label,
                    egui::TextStyle::Small.resolve(ui.style()),
                    Color32::from_rgb(180, 194, 208),
                );
                let pin_rect = egui::Rect::from_center_size(*pin_pos, egui::vec2(22.0, 22.0));
                let pin_resp = ui.interact(
                    pin_rect,
                    ui.id()
                        .with(("effect_graph_in_pin", &node.id, &port_key.port_id)),
                    Sense::click(),
                );
                let pin_resp = if matches!(&node.data, EffectGraphNodeData::CombineChannels)
                    && matches!(
                        combine_mode,
                        Some(EffectGraphCombineMode::Restore | EffectGraphCombineMode::Adaptive)
                    ) {
                    pin_resp.on_hover_text(format!("Socket {}", port_key.port_id))
                } else {
                    pin_resp
                };
                if pin_resp.hovered()
                    && ctx.input(|i| i.pointer.any_released())
                    && self.effect_graph.canvas.connecting_from_port.is_some()
                {
                    if let Some(from) = self.effect_graph.canvas.connecting_from_port.clone() {
                        pending_connect = Some((
                            from.node_id,
                            from.port_id,
                            node.id.clone(),
                            port_key.port_id.clone(),
                        ));
                    }
                    clear_connect = true;
                }
            }
            for (port_key, pin_pos) in node_output_pins.iter() {
                painter.circle_filled(*pin_pos, 7.0, Color32::from_rgb(18, 20, 24));
                painter.circle_filled(*pin_pos, 5.0, Color32::from_rgb(110, 170, 255));
                painter.circle_stroke(*pin_pos, 7.0, Stroke::new(1.0, Color32::from_rgb(70, 80, 92)));
                painter.text(
                    egui::pos2(pin_pos.x - 10.0, pin_pos.y),
                    egui::Align2::RIGHT_CENTER,
                    effect_graph_pin_label(
                        &node.data,
                        EffectGraphPortDirection::Output,
                        &port_key.port_id,
                    ),
                    egui::TextStyle::Small.resolve(ui.style()),
                    Color32::from_rgb(180, 194, 208),
                );
                if matches!(&node.data, EffectGraphNodeData::SplitChannels)
                    && self.effect_graph.tester.last_input_bus.is_some()
                {
                    let port_index = node
                        .data
                        .output_ports()
                        .iter()
                        .position(|port| port.id == port_key.port_id)
                        .unwrap_or(usize::MAX);
                    if port_index != usize::MAX {
                        let (badge, color) = if port_index < split_live_width {
                            ("live", Color32::from_rgb(132, 210, 154))
                        } else {
                            ("vacant", Color32::from_rgb(132, 144, 156))
                        };
                        painter.text(
                            egui::pos2(pin_pos.x - 10.0, pin_pos.y - 12.0),
                            egui::Align2::RIGHT_CENTER,
                            badge,
                            egui::TextStyle::Small.resolve(ui.style()),
                            color,
                        );
                    }
                }
                let pin_rect = egui::Rect::from_center_size(*pin_pos, egui::vec2(22.0, 22.0));
                let pin_resp = ui.interact(
                    pin_rect,
                    ui.id()
                        .with(("effect_graph_out_pin", &node.id, &port_key.port_id)),
                    Sense::click(),
                );
                if pin_resp.clicked() {
                    self.effect_graph.canvas.connecting_from_port = Some(port_key.clone());
                }
            }

            let mut gain_db = None;
            let mut target_lufs = None;
            let mut mono_mix_ignored_channels = None;
            let mut semitones = None;
            let mut rate = None;
            let mut noise_gate = None;
            let mut eq = None;
            let mut compressor = None;
            let mut trim = None;
            let mut band_split = None;
            let mut bit_depth = None;
            let mut resampler = None;
            let mut waveform_zoom = None;
            let mut spectrum_mode = None;
            let mut spectrum_zoom = None;
            let mut plugin_config = None;
            let plugin_runtime = self
                .effect_graph
                .plugin_runtime
                .get(&node.id)
                .cloned()
                .unwrap_or_default();
            match &node.data {
                EffectGraphNodeData::Gain { gain_db: value } => gain_db = Some(*value),
                EffectGraphNodeData::Loudness { target_lufs: value } => target_lufs = Some(*value),
                EffectGraphNodeData::MonoMix { ignored_channels } => {
                    mono_mix_ignored_channels = Some(ignored_channels.clone());
                }
                EffectGraphNodeData::PitchShift { semitones: value } => semitones = Some(*value),
                EffectGraphNodeData::TimeStretch { rate: value }
                | EffectGraphNodeData::Speed { rate: value } => rate = Some(*value),
                EffectGraphNodeData::NoiseGate {
                    threshold_db,
                    attack_ms,
                    release_ms,
                } => noise_gate = Some((*threshold_db, *attack_ms, *release_ms)),
                EffectGraphNodeData::Eq {
                    low_shelf_freq_hz,
                    low_shelf_gain_db,
                    mid_freq_hz,
                    mid_gain_db,
                    mid_q,
                    high_shelf_freq_hz,
                    high_shelf_gain_db,
                } => {
                    eq = Some((
                        *low_shelf_freq_hz,
                        *low_shelf_gain_db,
                        *mid_freq_hz,
                        *mid_gain_db,
                        *mid_q,
                        *high_shelf_freq_hz,
                        *high_shelf_gain_db,
                    ))
                }
                EffectGraphNodeData::Compressor {
                    threshold_db,
                    ratio,
                    attack_ms,
                    release_ms,
                    makeup_db,
                } => compressor = Some((*threshold_db, *ratio, *attack_ms, *release_ms, *makeup_db)),
                EffectGraphNodeData::Trim {
                    threshold_below_peak_db,
                    pre_roll_ms,
                    post_roll_ms,
                } => trim = Some((*threshold_below_peak_db, *pre_roll_ms, *post_roll_ms)),
                EffectGraphNodeData::BitDepth { depth } => bit_depth = Some(*depth),
                EffectGraphNodeData::Resampler {
                    target_sample_rate,
                    quality,
                } => resampler = Some((*target_sample_rate, *quality)),
                EffectGraphNodeData::PluginFx { config } => plugin_config = Some(config.clone()),
                EffectGraphNodeData::DebugWaveform { zoom: value } => waveform_zoom = Some(*value),
                EffectGraphNodeData::DebugSpectrum { mode, zoom: value } => {
                    spectrum_mode = Some(*mode);
                    spectrum_zoom = Some(*value);
                }
                EffectGraphNodeData::BandSplit { low_hz, high_hz } => {
                    band_split = Some((*low_hz, *high_hz));
                }
                EffectGraphNodeData::Input
                | EffectGraphNodeData::Output
                | EffectGraphNodeData::Duplicate
                | EffectGraphNodeData::SplitChannels
                | EffectGraphNodeData::CombineChannels
                | EffectGraphNodeData::BandJoin
                | EffectGraphNodeData::MsSplit
                | EffectGraphNodeData::MsJoin => {}
            }
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(body_rect.shrink2(egui::vec2(8.0, 8.0))),
                |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(
                        RichText::new(Self::effect_graph_node_parameter_summary(&node.data))
                            .color(Color32::from_rgb(190, 206, 220)),
                    );
                    match &node.data {
                        EffectGraphNodeData::Input => {
                            let target_label = self.effect_graph_test_input_summary();
                            ui.label(
                                RichText::new(target_label)
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            if ui
                                .add_enabled(
                                    true,
                                    egui::Button::new(self.effect_graph_play_button_label(
                                        EffectGraphPlaybackTarget::Input,
                                        "Play Input",
                                    )),
                                )
                                .clicked()
                            {
                                if let Err(err) = self.start_effect_graph_input_preview(true) {
                                    self.effect_graph.tester.last_error = Some(err.clone());
                                    self.push_effect_graph_console(
                                        EffectGraphSeverity::Error,
                                        "input",
                                        err,
                                        None,
                                    );
                                }
                            }
                        }
                        EffectGraphNodeData::Output => {
                            if let Some(summary) = predicted_output_summary.as_ref() {
                                ui.label(
                                    RichText::new(summary.clone())
                                        .small()
                                        .color(Color32::from_rgb(150, 190, 255)),
                                );
                            }
                            let actual_summary =
                                if self.effect_graph.tester.last_output_summary.is_empty() {
                                    "Output: not rendered".to_string()
                                } else {
                                    format!(
                                        "Output: {}",
                                        self.effect_graph.tester.last_output_summary
                                    )
                                };
                            ui.label(
                                RichText::new(actual_summary)
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            if show_monitor_downmix_note {
                                ui.label(
                                    RichText::new(EFFECT_GRAPH_MONITOR_DOWNMIX_NOTE)
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                );
                            }
                            if ui
                                .add_enabled(
                                    self.effect_graph.tester.last_output_audio.is_some(),
                                    egui::Button::new(self.effect_graph_play_button_label(
                                        EffectGraphPlaybackTarget::Output,
                                        "Play Output",
                                    )),
                                )
                                .clicked()
                            {
                                if let Some(audio) =
                                    self.effect_graph.tester.last_output_audio.clone()
                                {
                                    self.effect_graph_toggle_playback(
                                        EffectGraphPlaybackTarget::Output,
                                        audio,
                                    );
                                }
                            }
                        }
                        EffectGraphNodeData::PluginFx { .. } => {
                            if let Some(config) = plugin_config.clone() {
                                let selected_plugin_key = config.plugin_key.clone();
                                let selected_plugin_name = if config.plugin_name.trim().is_empty() {
                                    selected_plugin_key
                                        .as_deref()
                                        .and_then(|key| {
                                            Path::new(key)
                                                .file_name()
                                                .and_then(|name| name.to_str())
                                        })
                                        .unwrap_or("Select plugin")
                                        .to_string()
                                } else {
                                    config.plugin_name.clone()
                                };
                                let backend_label = plugin_runtime
                                    .backend
                                    .map(|backend| match backend {
                                        crate::plugin::PluginHostBackend::Generic => "Generic",
                                        crate::plugin::PluginHostBackend::NativeVst3 => {
                                            "NativeVst3"
                                        }
                                        crate::plugin::PluginHostBackend::NativeClap => {
                                            "NativeClap"
                                        }
                                    })
                                    .unwrap_or("Not probed");
                                let gui_live = self
                                    .effect_graph
                                    .plugin_gui_state
                                    .as_ref()
                                    .map(|state| state.node_id == node.id)
                                    .unwrap_or(false);
                                let filter_lower = config.filter.trim().to_ascii_lowercase();
                                let matching_plugins = self
                                    .plugin_catalog
                                    .iter()
                                    .filter(|entry| {
                                        if filter_lower.is_empty() {
                                            true
                                        } else {
                                            entry.name.to_ascii_lowercase().contains(&filter_lower)
                                                || entry
                                                    .path
                                                    .to_string_lossy()
                                                    .to_ascii_lowercase()
                                                    .contains(&filter_lower)
                                        }
                                    })
                                    .map(|entry| {
                                        (entry.key.clone(), entry.name.clone(), entry.path.clone())
                                    })
                                    .collect::<Vec<_>>();

                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("Rescan").clicked() {
                                        self.spawn_plugin_scan();
                                    }
                                    if ui.button("Reload Params").clicked() {
                                        if let Some(plugin_key) = selected_plugin_key.clone() {
                                            self.spawn_plugin_probe_for_effect_graph_node(
                                                node.id.clone(),
                                                plugin_key,
                                            );
                                        } else {
                                            self.effect_graph
                                                .plugin_runtime
                                                .entry(node.id.clone())
                                                .or_default()
                                                .last_error =
                                                Some("plugin not selected".to_string());
                                        }
                                    }
                                    if ui
                                        .button("Load from file...")
                                        .on_hover_text(
                                            "Pick a .vst3/.clap plugin directly, without needing a prior Rescan",
                                        )
                                        .clicked()
                                    {
                                        pending_plugin_load_from_file = Some(node.id.clone());
                                    }
                                });

                                let mut filter_text = config.filter.clone();
                                let filter_resp = ui.add(
                                    egui::TextEdit::singleline(&mut filter_text)
                                        .hint_text("Filter plugins"),
                                );
                                if filter_resp.changed() {
                                    self.effect_graph_push_undo_snapshot();
                                    if let Some(node_mut) =
                                        self.effect_graph.draft.nodes.get_mut(idx)
                                    {
                                        if let EffectGraphNodeData::PluginFx { config } =
                                            &mut node_mut.data
                                        {
                                            config.filter = filter_text;
                                        }
                                    }
                                    self.effect_graph.draft_dirty = true;
                                    self.revalidate_effect_graph_draft();
                                }

                                egui::ComboBox::from_id_salt(format!(
                                    "effect_graph_plugin_picker_{idx}"
                                ))
                                .width(ui.available_width())
                                .selected_text(selected_plugin_name)
                                .show_ui(ui, |ui| {
                                    if matching_plugins.is_empty() {
                                        ui.label(
                                            RichText::new("No scanned plugins match the filter")
                                                .small()
                                                .color(Color32::from_rgb(118, 132, 148)),
                                        );
                                    }
                                    for (plugin_key, plugin_name, plugin_path) in
                                        matching_plugins.iter()
                                    {
                                        let selected = selected_plugin_key.as_deref()
                                            == Some(plugin_key.as_str());
                                        let label = format!(
                                            "{} ({})",
                                            plugin_name,
                                            Self::plugin_path_label(plugin_path),
                                        );
                                        if ui.selectable_label(selected, label).clicked() {
                                            self.effect_graph_push_undo_snapshot();
                                            self.effect_graph_select_plugin(
                                                &node.id,
                                                plugin_key.clone(),
                                                plugin_name.clone(),
                                            );
                                        }
                                    }
                                });

                                if let Some(plugin_key) = selected_plugin_key.as_deref() {
                                    ui.label(
                                        RichText::new(plugin_key)
                                            .small()
                                            .color(Color32::from_rgb(118, 132, 148)),
                                    );
                                }

                                ui.horizontal_wrapped(|ui| {
                                    let mut enabled = config.enabled;
                                    if ui.checkbox(&mut enabled, "Enable").changed() {
                                        self.effect_graph_push_undo_snapshot();
                                        if let Some(node_mut) =
                                            self.effect_graph.draft.nodes.get_mut(idx)
                                        {
                                            if let EffectGraphNodeData::PluginFx { config } =
                                                &mut node_mut.data
                                            {
                                                config.enabled = enabled;
                                            }
                                        }
                                        self.effect_graph.draft_dirty = true;
                                        self.revalidate_effect_graph_draft();
                                    }
                                    let mut bypass = config.bypass;
                                    if ui.checkbox(&mut bypass, "Bypass").changed() {
                                        self.effect_graph_push_undo_snapshot();
                                        if let Some(node_mut) =
                                            self.effect_graph.draft.nodes.get_mut(idx)
                                        {
                                            if let EffectGraphNodeData::PluginFx { config } =
                                                &mut node_mut.data
                                            {
                                                config.bypass = bypass;
                                            }
                                        }
                                        self.effect_graph.draft_dirty = true;
                                        self.revalidate_effect_graph_draft();
                                    }
                                });

                                ui.label(
                                    RichText::new(format!("Backend: {backend_label}"))
                                        .small()
                                        .color(Color32::from_rgb(150, 190, 255)),
                                );
                                ui.label(
                                    RichText::new(format!("GUI: {:?}", plugin_runtime.gui_status))
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                );

                                ui.horizontal_wrapped(|ui| {
                                    let can_open_gui = config.plugin_key.is_some()
                                        && plugin_runtime.gui_capabilities.supports_native_gui;
                                    if ui
                                        .add_enabled(
                                            can_open_gui,
                                            egui::Button::new("Open Native GUI"),
                                        )
                                        .clicked()
                                    {
                                        self.open_plugin_gui_for_effect_graph_node(&node.id);
                                    }
                                    if ui
                                        .add_enabled(gui_live, egui::Button::new("Sync GUI"))
                                        .clicked()
                                    {
                                        self.sync_plugin_gui_for_effect_graph_node(&node.id);
                                    }
                                    if ui
                                        .add_enabled(gui_live, egui::Button::new("Close GUI"))
                                        .clicked()
                                    {
                                        self.close_plugin_gui_for_effect_graph_node(&node.id);
                                    }
                                });
                                if !plugin_runtime.gui_capabilities.supports_native_gui {
                                    ui.label(
                                        RichText::new(
                                            "Probe the plugin to detect native GUI support",
                                        )
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                    );
                                }
                                Self::ui_plugin_probe_status(
                                    ui,
                                    plugin_runtime.last_error.as_deref(),
                                    plugin_runtime.last_backend_note.as_deref(),
                                    plugin_runtime.last_backend_log.as_deref(),
                                );
                                egui::CollapsingHeader::new(format!(
                                    "Parameters ({})",
                                    config.params.len()
                                ))
                                .default_open(!config.params.is_empty() && config.params.len() <= 8)
                                .show(ui, |ui| {
                                    if config.params.is_empty() {
                                        ui.label(
                                            RichText::new(
                                                "Reload Params to fetch plugin parameters",
                                            )
                                            .small()
                                            .color(Color32::from_rgb(118, 132, 148)),
                                        );
                                    }
                                    for (param_index, param) in config.params.iter().enumerate() {
                                        let mut normalized = param.normalized.clamp(0.0, 1.0);
                                        let label = if param.unit.trim().is_empty() {
                                            param.name.clone()
                                        } else {
                                            format!("{} ({})", param.name, param.unit)
                                        };
                                        let response = ui.add(
                                            egui::Slider::new(&mut normalized, 0.0..=1.0)
                                                .text(label),
                                        );
                                        let mut changed = response.changed();
                                        if ui.small_button("Default").clicked() {
                                            normalized = param.default_normalized.clamp(0.0, 1.0);
                                            changed = true;
                                        }
                                        if changed {
                                            self.effect_graph_push_undo_snapshot();
                                            if let Some(node_mut) =
                                                self.effect_graph.draft.nodes.get_mut(idx)
                                            {
                                                if let EffectGraphNodeData::PluginFx { config } =
                                                    &mut node_mut.data
                                                {
                                                    if let Some(param_mut) =
                                                        config.params.get_mut(param_index)
                                                    {
                                                        param_mut.normalized = normalized;
                                                    }
                                                }
                                            }
                                            self.effect_graph.draft_dirty = true;
                                            self.revalidate_effect_graph_draft();
                                        }
                                    }
                                });
                            }
                        }
                        EffectGraphNodeData::DebugWaveform { .. } => {
                            ui.label(
                                RichText::new("Test-run only pass-through monitor")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            if let Some(preview) = debug_preview.as_deref() {
                                if let EffectGraphDebugPreview::Waveform { mono, sample_rate } =
                                    preview
                                {
                                    ui.label(
                                        RichText::new(format!(
                                            "{sample_rate} Hz  |  drag / wheel to scroll"
                                        ))
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                    );
                                    let preview_height =
                                        (ui.available_height() - 34.0).clamp(88.0, 124.0);
                                    let (preview_rect, preview_resp) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), preview_height),
                                        Sense::click_and_drag(),
                                    );
                                    apply_preview_scroll(
                                        ctx,
                                        &preview_resp,
                                        &mut debug_scroll_x,
                                        waveform_zoom.unwrap_or(1.0),
                                    );
                                    draw_waveform_preview(
                                        ui.painter(),
                                        preview_rect,
                                        mono,
                                        *sample_rate,
                                        waveform_zoom.unwrap_or(1.0),
                                        debug_scroll_x,
                                    );
                                }
                            } else {
                                ui.label(
                                    RichText::new("Run Test to capture waveform")
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                );
                            }
                        }
                        EffectGraphNodeData::DebugSpectrum { mode, .. } => {
                            ui.label(
                                RichText::new("Test-run only pass-through monitor")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            ui.horizontal_wrapped(|ui| {
                                for option in [
                                    EffectGraphSpectrumMode::Linear,
                                    EffectGraphSpectrumMode::Log,
                                    EffectGraphSpectrumMode::Mel,
                                ] {
                                    let label = match option {
                                        EffectGraphSpectrumMode::Linear => "Linear",
                                        EffectGraphSpectrumMode::Log => "Log",
                                        EffectGraphSpectrumMode::Mel => "Mel",
                                    };
                                    let selected = *mode == option;
                                    if ui.selectable_label(selected, label).clicked() {
                                        self.effect_graph_push_undo_snapshot();
                                        if let Some(node_mut) =
                                            self.effect_graph.draft.nodes.get_mut(idx)
                                        {
                                            node_mut.data = EffectGraphNodeData::DebugSpectrum {
                                                mode: option,
                                                zoom: spectrum_zoom.unwrap_or(1.0),
                                            };
                                        }
                                        self.effect_graph.draft_dirty = true;
                                        self.revalidate_effect_graph_draft();
                                    }
                                }
                            });
                            if let Some(preview) = debug_preview.as_deref() {
                                if let EffectGraphDebugPreview::Spectrum { spectrogram } = preview {
                                    ui.label(
                                        RichText::new("drag / wheel to scroll")
                                            .small()
                                            .color(Color32::from_rgb(118, 132, 148)),
                                    );
                                    let preview_height =
                                        (ui.available_height() - 40.0).clamp(116.0, 156.0);
                                    let (preview_rect, preview_resp) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), preview_height),
                                        Sense::click_and_drag(),
                                    );
                                    apply_preview_scroll(
                                        ctx,
                                        &preview_resp,
                                        &mut debug_scroll_x,
                                        spectrum_zoom.unwrap_or(1.0),
                                    );
                                    draw_spectrum_preview(
                                        ui.painter(),
                                        preview_rect,
                                        spectrogram,
                                        spectrum_mode.unwrap_or(EffectGraphSpectrumMode::Log),
                                        spectrum_zoom.unwrap_or(1.0),
                                        debug_scroll_x,
                                    );
                                }
                            } else {
                                ui.label(
                                    RichText::new("Run Test to capture spectrum")
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                );
                            }
                        }
                        EffectGraphNodeData::Duplicate => {
                            ui.label(
                                RichText::new("Clones the incoming bus into two adaptive branches")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            ui.label(
                                RichText::new(
                                    "Combine can widen these branches into separate channels",
                                )
                                .small()
                                .color(Color32::from_rgb(118, 132, 148)),
                            );
                        }
                        EffectGraphNodeData::MonoMix { .. } => {
                            ui.label(
                                RichText::new("Downmixes all non-ignored channels to mono")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            ui.label(
                                RichText::new("Use Ignore for channels like LFE before averaging")
                                    .small()
                                    .color(Color32::from_rgb(118, 132, 148)),
                            );
                            if let Some(mut ignored_channels) = mono_mix_ignored_channels.clone() {
                                ui.separator();
                                ui.label(
                                    RichText::new("Ignore channels")
                                        .small()
                                        .color(Color32::from_rgb(160, 176, 192)),
                                );
                                let mut changed = false;
                                let channel_count = ignored_channels.len().max(8);
                                ui.scope(|ui| {
                                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 6.0);
                                    ui.style_mut().override_text_style =
                                        Some(egui::TextStyle::Small);
                                    egui::Grid::new(format!("mono_mix_ignore_grid_{idx}"))
                                        .num_columns(2)
                                        .spacing([12.0, 6.0])
                                        .show(ui, |ui| {
                                            for row in 0..channel_count.div_ceil(2) {
                                                for column in 0..2 {
                                                    let channel_index = row * 2 + column;
                                                    if channel_index >= channel_count {
                                                        ui.label("");
                                                        continue;
                                                    }
                                                    let current = ignored_channels
                                                        .get(channel_index)
                                                        .copied()
                                                        .unwrap_or(false);
                                                    let mut checked = current;
                                                    if ui
                                                        .checkbox(
                                                            &mut checked,
                                                            RichText::new(
                                                                Self::effect_graph_channel_label(
                                                                    channel_index,
                                                                ),
                                                            )
                                                            .small(),
                                                        )
                                                        .changed()
                                                    {
                                                        if channel_index >= ignored_channels.len() {
                                                            ignored_channels
                                                                .resize(channel_index + 1, false);
                                                        }
                                                        ignored_channels[channel_index] = checked;
                                                        changed = true;
                                                    }
                                                }
                                                ui.end_row();
                                            }
                                        });
                                });
                                if changed {
                                    self.effect_graph_push_undo_snapshot();
                                    if let Some(node_mut) =
                                        self.effect_graph.draft.nodes.get_mut(idx)
                                    {
                                        node_mut.data =
                                            EffectGraphNodeData::MonoMix { ignored_channels };
                                    }
                                    self.effect_graph.draft_dirty = true;
                                    self.revalidate_effect_graph_draft();
                                }
                            }
                        }
                        EffectGraphNodeData::Loudness { .. } => {
                            ui.label(
                                RichText::new("Measures integrated LUFS and applies matching gain")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            ui.label(
                                RichText::new("Format and duration stay unchanged")
                                    .small()
                                    .color(Color32::from_rgb(118, 132, 148)),
                            );
                        }
                        EffectGraphNodeData::BandSplit { .. } => {
                            ui.label(
                                RichText::new("Splits into low / mid / high frequency bands")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            ui.label(
                                RichText::new(
                                    "Complementary split: Band Join reconstructs the input exactly",
                                )
                                .small()
                                .color(Color32::from_rgb(118, 132, 148)),
                            );
                            if let Some((mut low_hz, mut high_hz)) = band_split {
                                let mut changed = false;
                                changed |= ui
                                    .add(
                                        egui::Slider::new(&mut low_hz, 20.0..=8_000.0)
                                            .logarithmic(true)
                                            .text("Low/Mid Hz"),
                                    )
                                    .on_hover_text("Crossover between the low and mid bands")
                                    .changed();
                                changed |= ui
                                    .add(
                                        egui::Slider::new(&mut high_hz, 40.0..=20_000.0)
                                            .logarithmic(true)
                                            .text("Mid/High Hz"),
                                    )
                                    .on_hover_text("Crossover between the mid and high bands")
                                    .changed();
                                if changed {
                                    if high_hz <= low_hz {
                                        high_hz = low_hz * 1.01;
                                    }
                                    self.effect_graph_push_undo_snapshot();
                                    if let Some(node_mut) =
                                        self.effect_graph.draft.nodes.get_mut(idx)
                                    {
                                        node_mut.data =
                                            EffectGraphNodeData::BandSplit { low_hz, high_hz };
                                    }
                                    self.effect_graph.draft_dirty = true;
                                    self.revalidate_effect_graph_draft();
                                }
                            }
                        }
                        EffectGraphNodeData::BandJoin => {
                            ui.label(
                                RichText::new("Sums the low / mid / high bands back together")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            ui.label(
                                RichText::new(
                                    "Straight from Band Split it returns the original audio",
                                )
                                .small()
                                .color(Color32::from_rgb(118, 132, 148)),
                            );
                        }
                        EffectGraphNodeData::MsSplit => {
                            ui.label(
                                RichText::new("Encodes stereo into mid (L+R) and side (L-R)")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            ui.label(
                                RichText::new("Mono input passes through as mid with silent side")
                                    .small()
                                    .color(Color32::from_rgb(118, 132, 148)),
                            );
                        }
                        EffectGraphNodeData::MsJoin => {
                            ui.label(
                                RichText::new("Decodes mid + side back to stereo (L/R)")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            ui.label(
                                RichText::new(
                                    "Straight from MS Split it returns the original stereo",
                                )
                                .small()
                                .color(Color32::from_rgb(118, 132, 148)),
                            );
                        }
                        EffectGraphNodeData::SplitChannels => {
                            ui.label(
                                RichText::new("Splits incoming audio into 8 routed mono outputs")
                                    .small()
                                    .color(Color32::from_rgb(160, 176, 192)),
                            );
                            ui.label(
                                RichText::new("Preserves source layout width")
                                    .small()
                                    .color(Color32::from_rgb(118, 132, 148)),
                            );
                            if split_live_width > 0 {
                                ui.label(
                                    RichText::new(format!(
                                        "Last test input: {split_live_width} ch"
                                    ))
                                    .small()
                                    .color(Color32::from_rgb(118, 132, 148)),
                                );
                            }
                        }
                        EffectGraphNodeData::CombineChannels => match combine_mode {
                            Some(EffectGraphCombineMode::Restore) => {
                                ui.label(
                                    RichText::new("Restores original channel slots")
                                        .small()
                                        .color(Color32::from_rgb(160, 176, 192)),
                                );
                                ui.label(
                                    RichText::new("Missing slots restore as silence")
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                );
                                ui.label(
                                    RichText::new("Mode: restore")
                                        .small()
                                        .color(Color32::from_rgb(110, 206, 180)),
                                );
                            }
                            Some(EffectGraphCombineMode::Adaptive) => {
                                ui.label(
                                    RichText::new("Auto widen branch outputs")
                                        .small()
                                        .color(Color32::from_rgb(160, 176, 192)),
                                );
                                ui.label(
                                    RichText::new("Duplicate branches widen instead of mixing")
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                );
                                ui.label(
                                    RichText::new("Preserves untouched slots where possible")
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                );
                                ui.label(
                                    RichText::new("Mode: adaptive")
                                        .small()
                                        .color(Color32::from_rgb(110, 170, 255)),
                                );
                            }
                            Some(EffectGraphCombineMode::Concat) => {
                                ui.label(
                                    RichText::new("Concats plain inputs in socket order")
                                        .small()
                                        .color(Color32::from_rgb(160, 176, 192)),
                                );
                                ui.label(
                                    RichText::new("Resamples to the highest input sample rate")
                                        .small()
                                        .color(Color32::from_rgb(118, 132, 148)),
                                );
                                ui.label(
                                    RichText::new("Mode: concat")
                                        .small()
                                        .color(Color32::from_rgb(110, 170, 255)),
                                );
                            }
                            Some(EffectGraphCombineMode::Mixed) => {
                                ui.label(
                                    RichText::new("Unsupported channel layout mix")
                                        .small()
                                        .color(Color32::from_rgb(240, 120, 120)),
                                );
                                ui.label(
                                    RichText::new("Adaptive combine could not infer a safe layout")
                                        .small()
                                        .color(Color32::from_rgb(180, 140, 140)),
                                );
                            }
                            None => {
                                ui.label(
                                    RichText::new("Restore width when slotted")
                                        .small()
                                        .color(Color32::from_rgb(160, 176, 192)),
                                );
                                ui.label(
                                    RichText::new(
                                        "Concat plain inputs when no slot routing is present",
                                    )
                                    .small()
                                    .color(Color32::from_rgb(118, 132, 148)),
                                );
                            }
                        },
                        EffectGraphNodeData::Gain { .. }
                        | EffectGraphNodeData::PitchShift { .. }
                        | EffectGraphNodeData::TimeStretch { .. }
                        | EffectGraphNodeData::Speed { .. }
                        | EffectGraphNodeData::NoiseGate { .. }
                        | EffectGraphNodeData::Eq { .. }
                        | EffectGraphNodeData::Compressor { .. }
                        | EffectGraphNodeData::Trim { .. }
                        | EffectGraphNodeData::BitDepth { .. }
                        | EffectGraphNodeData::Resampler { .. } => {}
                    }
                    if let Some(mut value) = waveform_zoom {
                        let response =
                            ui.add(egui::Slider::new(&mut value, 1.0..=32.0).text("Zoom"));
                        if response.changed() {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data = EffectGraphNodeData::DebugWaveform { zoom: value };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                    if let (Some(mode), Some(mut value)) = (spectrum_mode, spectrum_zoom) {
                        let response =
                            ui.add(egui::Slider::new(&mut value, 1.0..=16.0).text("Zoom"));
                        if response.changed() {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data =
                                    EffectGraphNodeData::DebugSpectrum { mode, zoom: value };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                    if let Some(mut value) = gain_db {
                        let response =
                            ui.add(egui::Slider::new(&mut value, -24.0..=24.0).text("dB"));
                        if response.changed() {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data = EffectGraphNodeData::Gain { gain_db: value };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                    if let Some(mut value) = target_lufs {
                        let response =
                            ui.add(egui::Slider::new(&mut value, -36.0..=0.0).text("Target LUFS"));
                        if response.changed() {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data =
                                    EffectGraphNodeData::Loudness { target_lufs: value };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                    if let Some(mut value) = semitones {
                        let response =
                            ui.add(egui::Slider::new(&mut value, -12.0..=12.0).text("st"));
                        if response.changed() {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data =
                                    EffectGraphNodeData::PitchShift { semitones: value };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                    if let Some(mut value) = rate {
                        let response = ui.add(egui::Slider::new(&mut value, 0.25..=4.0).text("x"));
                        if response.changed() {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data = match node_mut.data {
                                    EffectGraphNodeData::TimeStretch { .. } => {
                                        EffectGraphNodeData::TimeStretch { rate: value }
                                    }
                                    EffectGraphNodeData::Speed { .. } => {
                                        EffectGraphNodeData::Speed { rate: value }
                                    }
                                    _ => node_mut.data.clone(),
                                };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                    if let Some((mut threshold_db, mut attack_ms, mut release_ms)) = noise_gate {
                        let mut changed = false;
                        {
                            let mut plot_params = crate::wave::NoiseGateParams {
                                threshold_db,
                                attack_ms,
                                release_ms,
                            };
                            if crate::app::ui::dsp_widgets::noise_gate_plot(
                                ui,
                                egui::Id::new(("fx_gate_plot", idx)),
                                &mut plot_params,
                            ) {
                                threshold_db = plot_params.threshold_db;
                                changed = true;
                            }
                        }
                        changed |= ui
                            .add(egui::Slider::new(&mut threshold_db, -80.0..=0.0).text("Threshold dB"))
                            .on_hover_text("Signal below this level is faded toward silence")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut attack_ms, 0.1..=500.0).logarithmic(true).text("Attack ms"))
                            .on_hover_text("How fast the gate opens once the signal crosses the threshold")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut release_ms, 1.0..=2000.0).logarithmic(true).text("Release ms"))
                            .on_hover_text("How fast the gate closes once the signal drops below the threshold")
                            .changed();
                        if changed {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data = EffectGraphNodeData::NoiseGate {
                                    threshold_db,
                                    attack_ms,
                                    release_ms,
                                };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                    if let Some((
                        mut low_shelf_freq_hz,
                        mut low_shelf_gain_db,
                        mut mid_freq_hz,
                        mut mid_gain_db,
                        mut mid_q,
                        mut high_shelf_freq_hz,
                        mut high_shelf_gain_db,
                    )) = eq
                    {
                        let mut changed = false;
                        {
                            let mut plot_params = crate::wave::ThreeBandEqParams {
                                low_shelf_freq_hz,
                                low_shelf_gain_db,
                                mid_freq_hz,
                                mid_gain_db,
                                mid_q,
                                high_shelf_freq_hz,
                                high_shelf_gain_db,
                            };
                            if crate::app::ui::dsp_widgets::eq_response_plot(
                                ui,
                                egui::Id::new(("fx_eq_plot", idx)),
                                &mut plot_params,
                                48_000,
                            ) {
                                low_shelf_freq_hz = plot_params.low_shelf_freq_hz;
                                low_shelf_gain_db = plot_params.low_shelf_gain_db;
                                mid_freq_hz = plot_params.mid_freq_hz;
                                mid_gain_db = plot_params.mid_gain_db;
                                mid_q = plot_params.mid_q;
                                high_shelf_freq_hz = plot_params.high_shelf_freq_hz;
                                high_shelf_gain_db = plot_params.high_shelf_gain_db;
                                changed = true;
                            }
                        }
                        ui.label(RichText::new("Low shelf").small().weak())
                            .on_hover_text("Boosts or cuts everything below this frequency");
                        changed |= ui
                            .add(egui::Slider::new(&mut low_shelf_freq_hz, 20.0..=2000.0).logarithmic(true).text("Freq Hz"))
                            .on_hover_text("Low shelf corner frequency")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut low_shelf_gain_db, -24.0..=24.0).text("Gain dB"))
                            .on_hover_text("Low shelf gain")
                            .changed();
                        ui.label(RichText::new("Mid").small().weak())
                            .on_hover_text("Boosts or cuts a band centered on this frequency");
                        changed |= ui
                            .add(egui::Slider::new(&mut mid_freq_hz, 50.0..=12_000.0).logarithmic(true).text("Freq Hz"))
                            .on_hover_text("Mid band center frequency")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut mid_gain_db, -24.0..=24.0).text("Gain dB"))
                            .on_hover_text("Mid band gain")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut mid_q, 0.1..=10.0).logarithmic(true).text("Q"))
                            .on_hover_text("Mid band width: higher Q = narrower band")
                            .changed();
                        ui.label(RichText::new("High shelf").small().weak())
                            .on_hover_text("Boosts or cuts everything above this frequency");
                        changed |= ui
                            .add(egui::Slider::new(&mut high_shelf_freq_hz, 500.0..=20_000.0).logarithmic(true).text("Freq Hz"))
                            .on_hover_text("High shelf corner frequency")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut high_shelf_gain_db, -24.0..=24.0).text("Gain dB"))
                            .on_hover_text("High shelf gain")
                            .changed();
                        if changed {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data = EffectGraphNodeData::Eq {
                                    low_shelf_freq_hz,
                                    low_shelf_gain_db,
                                    mid_freq_hz,
                                    mid_gain_db,
                                    mid_q,
                                    high_shelf_freq_hz,
                                    high_shelf_gain_db,
                                };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                    if let Some((mut threshold_db, mut ratio, mut attack_ms, mut release_ms, mut makeup_db)) =
                        compressor
                    {
                        let mut changed = false;
                        {
                            let mut plot_params = crate::wave::CompressorParams {
                                threshold_db,
                                ratio,
                                attack_ms,
                                release_ms,
                                makeup_db,
                            };
                            if crate::app::ui::dsp_widgets::compressor_transfer_plot(
                                ui,
                                egui::Id::new(("fx_comp_plot", idx)),
                                &mut plot_params,
                            ) {
                                threshold_db = plot_params.threshold_db;
                                ratio = plot_params.ratio;
                                changed = true;
                            }
                        }
                        changed |= ui
                            .add(egui::Slider::new(&mut threshold_db, -60.0..=0.0).text("Threshold dB"))
                            .on_hover_text("Signal above this level gets compressed")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut ratio, 1.0..=20.0).text("Ratio"))
                            .on_hover_text("How strongly signal above the threshold is reduced (4:1 = 4 dB in becomes 1 dB out)")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut attack_ms, 0.1..=500.0).logarithmic(true).text("Attack ms"))
                            .on_hover_text("How fast the compressor reacts once the signal crosses the threshold")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut release_ms, 1.0..=2000.0).logarithmic(true).text("Release ms"))
                            .on_hover_text("How fast the compressor lets go once the signal drops below the threshold")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut makeup_db, 0.0..=24.0).text("Makeup dB"))
                            .on_hover_text("Gain applied after compression to restore overall level")
                            .changed();
                        if changed {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data = EffectGraphNodeData::Compressor {
                                    threshold_db,
                                    ratio,
                                    attack_ms,
                                    release_ms,
                                    makeup_db,
                                };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                    if let Some((mut threshold_below_peak_db, mut pre_roll_ms, mut post_roll_ms)) = trim {
                        let mut changed = false;
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut threshold_below_peak_db, 6.0..=90.0)
                                    .text("Threshold below peak dB"),
                            )
                            .on_hover_text("How far below the loudest point counts as silence")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut pre_roll_ms, 0.0..=1000.0).text("Pre-roll ms"))
                            .on_hover_text("Audio kept before the detected start of sound")
                            .changed();
                        changed |= ui
                            .add(egui::Slider::new(&mut post_roll_ms, 0.0..=1000.0).text("Post-roll ms"))
                            .on_hover_text("Audio kept after the detected end of sound")
                            .changed();
                        if changed {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data = EffectGraphNodeData::Trim {
                                    threshold_below_peak_db,
                                    pre_roll_ms,
                                    post_roll_ms,
                                };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                        ui.label(
                            RichText::new("Removes leading/trailing silence only")
                                .small()
                                .weak(),
                        );
                    }
                    if let Some(depth) = bit_depth {
                        ui.horizontal(|ui| {
                            ui.label("Depth")
                                .on_hover_text("Quantizes the signal to this bit depth (preview of the resolution loss)");
                            for (option, label) in [
                                (EffectGraphBitDepth::Pcm16, "16-bit"),
                                (EffectGraphBitDepth::Pcm24, "24-bit"),
                                (EffectGraphBitDepth::Float32, "32-bit float"),
                            ] {
                                if ui.selectable_label(depth == option, label).clicked() && depth != option {
                                    self.effect_graph_push_undo_snapshot();
                                    if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                        node_mut.data = EffectGraphNodeData::BitDepth { depth: option };
                                    }
                                    self.effect_graph.draft_dirty = true;
                                    self.revalidate_effect_graph_draft();
                                }
                            }
                        });
                    }
                    if let Some((mut target_sample_rate, quality)) = resampler {
                        let mut changed = false;
                        let mut sr_f = target_sample_rate as f32;
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut sr_f, 8_000.0..=192_000.0)
                                    .logarithmic(true)
                                    .text("Target Hz"),
                            )
                            .on_hover_text("Sample rate to convert to")
                            .changed();
                        target_sample_rate = sr_f.round() as u32;
                        let mut new_quality = quality;
                        ui.horizontal(|ui| {
                            ui.label("Quality")
                                .on_hover_text("Resampling quality: higher is slower but cleaner");
                            for (option, label) in [
                                (EffectGraphResampleQuality::Fast, "Fast"),
                                (EffectGraphResampleQuality::Good, "Good"),
                                (EffectGraphResampleQuality::Best, "Best"),
                            ] {
                                if ui.selectable_label(quality == option, label).clicked() {
                                    new_quality = option;
                                    changed = true;
                                }
                            }
                        });
                        if changed {
                            self.effect_graph_push_undo_snapshot();
                            if let Some(node_mut) = self.effect_graph.draft.nodes.get_mut(idx) {
                                node_mut.data = EffectGraphNodeData::Resampler {
                                    target_sample_rate,
                                    quality: new_quality,
                                };
                            }
                            self.effect_graph.draft_dirty = true;
                            self.revalidate_effect_graph_draft();
                        }
                    }
                },
            );
            if matches!(
                node.data,
                EffectGraphNodeData::DebugWaveform { .. }
                    | EffectGraphNodeData::DebugSpectrum { .. }
            ) {
                self.effect_graph
                    .debug_view_state
                    .entry(node.id.clone())
                    .or_default()
                    .scroll_x = debug_scroll_x;
            }
        }

        if let Some((from_node_id, from_port_id, to_node_id, to_port_id)) = pending_connect {
            let _ = self.effect_graph_connect_nodes(
                &from_node_id,
                &from_port_id,
                &to_node_id,
                &to_port_id,
            );
        }
        if let Some(node_id) = pending_plugin_load_from_file {
            if let Some(path) = self.pick_plugin_file_dialog() {
                if let Some(entry) = self.add_plugin_catalog_entry_from_path(path.clone()) {
                    self.effect_graph_push_undo_snapshot();
                    self.effect_graph_select_plugin(&node_id, entry.key, entry.name);
                } else {
                    self.effect_graph
                        .plugin_runtime
                        .entry(node_id.clone())
                        .or_default()
                        .last_error = Some(format!(
                        "not a recognized VST3/CLAP plugin: {}",
                        path.display()
                    ));
                }
            }
        }
        if clear_connect
            || (self.effect_graph.canvas.connecting_from_port.is_some()
                && canvas_resp.clicked_by(egui::PointerButton::Primary)
                && !pressed_on_node)
        {
            self.effect_graph.canvas.connecting_from_port = None;
        }
        if canvas_resp.clicked_by(egui::PointerButton::Primary)
            && !pressed_on_node
            && self.effect_graph.canvas.connecting_from_port.is_none()
        {
            self.effect_graph.canvas.selected_nodes.clear();
            self.effect_graph.canvas.selected_edge_id = None;
        }
        if let Some(kind) = self.effect_graph.canvas.drag_palette_kind {
            if ctx.input(|i| i.pointer.any_released()) {
                if let Some(pointer) = ctx.pointer_hover_pos() {
                    if canvas_rect.contains(pointer) {
                        let world = screen_to_world(canvas_rect, pan, zoom, pointer);
                        let _ = self.effect_graph_add_node(kind, world);
                    }
                }
                self.effect_graph.canvas.drag_palette_kind = None;
            }
        }
    }

    fn ui_effect_graph_unsaved_prompt(&mut self, ctx: &egui::Context) {
        if !self.effect_graph.show_unsaved_prompt {
            return;
        }
        let pending_action = self.effect_graph.pending_action.clone();
        egui::Window::new("Unsaved Effect Graph")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if matches!(
                    pending_action,
                    Some(crate::app::types::EffectGraphPendingAction::DeleteTemplate(
                        _
                    ))
                ) {
                    ui.label("Delete the selected template file?");
                    ui.label("The current draft will remain open as an unsaved graph.");
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            self.execute_effect_graph_pending_action(true);
                        }
                        if ui.button("Cancel").clicked() {
                            self.effect_graph.pending_action = None;
                            self.effect_graph.show_unsaved_prompt = false;
                        }
                    });
                } else {
                    ui.label("Save changes to the current effect graph?");
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            if self.save_effect_graph_draft(false).is_ok() {
                                self.execute_effect_graph_pending_action(false);
                            }
                        }
                        if ui.button("Discard").clicked() {
                            self.execute_effect_graph_pending_action(true);
                        }
                        if ui.button("Cancel").clicked() {
                            self.effect_graph.pending_action = None;
                            self.effect_graph.show_unsaved_prompt = false;
                        }
                    });
                }
            });
    }
}
