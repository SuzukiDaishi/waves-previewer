use egui::{Color32, RichText, Sense, Stroke, StrokeKind};
use std::collections::HashMap;
use std::sync::Arc;

use crate::app::helpers::db_to_color;
use crate::app::types::{
    EffectGraphCombineMode, EffectGraphDebugPreview, EffectGraphNodeData, EffectGraphNodeKind,
    EffectGraphNodeRunPhase, EffectGraphPlaybackTarget, EffectGraphPortKey, EffectGraphSeverity,
    EffectGraphSpectrumMode,
};

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
        let raw_scroll = ctx.input(|i| i.raw_scroll_delta);
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
            egui::TextStyle::Small.resolve(&painter.ctx().style()),
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
        egui::TextStyle::Small.resolve(&painter.ctx().style()),
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
            egui::TextStyle::Small.resolve(&painter.ctx().style()),
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
            egui::TextStyle::Small.resolve(&painter.ctx().style()),
            Color32::from_rgb(146, 160, 176),
        );
    }
}

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_effect_graph_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use egui::{SidePanel, TopBottomPanel};

        self.handle_effect_graph_shortcuts(ctx);
        self.ui_effect_graph_unsaved_prompt(ctx);

        TopBottomPanel::bottom("effect_graph_console_panel")
            .resizable(true)
            .default_height(self.effect_graph.bottom_panel_height)
            .show_inside(ui, |ui| {
                self.effect_graph.bottom_panel_height = ui.max_rect().height();
                self.ui_effect_graph_console(ui);
            });

        SidePanel::right("effect_graph_tester_panel")
            .resizable(true)
            .default_width(self.effect_graph.right_panel_width)
            .show_inside(ui, |ui| {
                self.effect_graph.right_panel_width = ui.max_rect().width();
                self.ui_effect_graph_tester(ui);
            });

        SidePanel::left("effect_graph_library_panel")
            .resizable(true)
            .default_width(self.effect_graph.left_panel_width)
            .show_inside(ui, |ui| {
                self.effect_graph.left_panel_width = ui.max_rect().width();
                ui.vertical(|ui| {
                    self.ui_effect_graph_templates_panel(ui);
                    ui.separator();
                    self.ui_effect_graph_node_palette(ui);
                });
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

    fn effect_graph_toggle_playback(
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
        self.playback_mark_source(
            crate::app::PlaybackSourceKind::EffectGraph,
            self.audio.shared.out_sample_rate.max(1),
        );
        self.audio.set_samples_buffer(audio);
        self.audio.play();
        self.effect_graph.tester.playback_target = Some(target);
    }

    fn effect_graph_play_button_label(
        &self,
        target: EffectGraphPlaybackTarget,
        play_label: &'static str,
    ) -> &'static str {
        if self.effect_graph_playback_active(target) {
            "Stop"
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
        let standard_kinds = [
            EffectGraphNodeKind::Input,
            EffectGraphNodeKind::Output,
            EffectGraphNodeKind::Gain,
            EffectGraphNodeKind::MonoMix,
            EffectGraphNodeKind::PitchShift,
            EffectGraphNodeKind::TimeStretch,
            EffectGraphNodeKind::Speed,
        ];
        let debug_kinds = [
            EffectGraphNodeKind::DebugWaveform,
            EffectGraphNodeKind::DebugSpectrum,
        ];
        let routing_kinds = [
            EffectGraphNodeKind::Duplicate,
            EffectGraphNodeKind::SplitChannels,
            EffectGraphNodeKind::CombineChannels,
        ];
        let canvas_world = self
            .effect_graph
            .canvas
            .last_canvas_pointer_world
            .unwrap_or([140.0, 140.0]);
        let mut add_button = |ui: &mut egui::Ui, kind: EffectGraphNodeKind| {
            let can_add = match kind {
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
            let label = EffectGraphNodeData::default_for_kind(kind).display_name();
            let resp = ui.add_enabled(can_add, egui::Button::new(label));
            if resp.clicked() {
                let _ = self.effect_graph_add_node(kind, canvas_world);
            }
            if resp.drag_started() {
                self.effect_graph.canvas.drag_palette_kind = Some(kind);
            }
        };
        ui.label(RichText::new("Effects").strong());
        for kind in standard_kinds {
            add_button(ui, kind);
        }
        ui.separator();
        ui.label(RichText::new("Debug").strong());
        ui.label(
            RichText::new("Run Test only. Batch apply skips these nodes.")
                .small()
                .color(Color32::from_rgb(140, 154, 168)),
        );
        for kind in debug_kinds {
            add_button(ui, kind);
        }
        ui.separator();
        ui.label(RichText::new("Routing").strong());
        for kind in routing_kinds {
            add_button(ui, kind);
        }
        if self.effect_graph.canvas.drag_palette_kind.is_some() {
            ui.label(RichText::new("Release over canvas to add").italics());
        }
    }

    fn ui_effect_graph_tester(&mut self, ui: &mut egui::Ui) {
        let predicted_output_summary = self.effect_graph_predicted_output_summary();
        ui.heading("Test");
        ui.label("Target audio");
        let target_edit = ui.add(egui::TextEdit::singleline(
            &mut self.effect_graph.tester.target_path_input,
        ));
        if target_edit.changed() {
            self.effect_graph.tester.target_path = None;
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
                let audio = self
                    .effect_graph
                    .tester
                    .last_input_audio
                    .clone()
                    .or_else(|| self.effect_graph_preview_input_audio().ok());
                if let Some(audio) = audio {
                    self.effect_graph_toggle_playback(EffectGraphPlaybackTarget::Input, audio);
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
        ui.horizontal(|ui| {
            ui.heading("Console");
            if ui.button("Clear").clicked() {
                self.effect_graph.console.lines.clear();
            }
        });
        egui::ScrollArea::vertical().show(ui, |ui| {
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
                    ui.label(
                        RichText::new(format!("[{}] {}", issue.code, issue.message)).color(color),
                    );
                });
            }
            for line in self.effect_graph.console.lines.iter().rev() {
                let color = match line.severity {
                    EffectGraphSeverity::Info => Color32::from_rgb(180, 200, 220),
                    EffectGraphSeverity::Warning => Color32::from_rgb(255, 210, 120),
                    EffectGraphSeverity::Error => Color32::from_rgb(240, 120, 120),
                };
                ui.horizontal(|ui| {
                    if let Some(node_id) = line.node_id.clone() {
                        if ui.small_button("Go").clicked() {
                            self.effect_graph.canvas.focus_node_id = Some(node_id);
                        }
                    }
                    ui.label(
                        RichText::new(format!(
                            "[{}] [{}] {}",
                            format_console_timestamp(line.timestamp_unix_ms),
                            line.scope,
                            line.message
                        ))
                        .color(color),
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
            let scroll = ctx.input(|i| i.raw_scroll_delta.y);
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
        let predicted_output_summary = self.effect_graph_predicted_output_summary();
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
                    .map(|port_id| (*port_id).to_string())
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
                    .map(|port_id| (*port_id).to_string())
                    .collect::<Vec<_>>()
            };
            let ordered_output_ports = node
                .data
                .output_ports()
                .iter()
                .map(|port_id| (*port_id).to_string())
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
            painter.rect_filled(rect, 10.0, Color32::from_rgb(32, 36, 42));
            painter.rect_filled(title_rect, 10.0, Color32::from_rgb(44, 50, 58));
            painter.rect_stroke(
                rect,
                10.0,
                Stroke::new(if selected { 3.0 } else { 2.0 }, border),
                StrokeKind::Outside,
            );
            painter.text(
                egui::pos2(title_rect.left() + 10.0, title_rect.center().y),
                egui::Align2::LEFT_CENTER,
                node.data.display_name(),
                egui::TextStyle::Button.resolve(ui.style()),
                Color32::WHITE,
            );
            if let Some(status) = status {
                if let Some(elapsed_ms) = status.elapsed_ms {
                    painter.text(
                        egui::pos2(title_rect.right() - 10.0, title_rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        format!("{elapsed_ms:.0} ms"),
                        egui::TextStyle::Small.resolve(ui.style()),
                        Color32::from_rgb(210, 220, 230),
                    );
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
                painter.circle_filled(*pin_pos, 6.5, Color32::from_rgb(230, 238, 246));
                let input_label = if matches!(&node.data, EffectGraphNodeData::CombineChannels)
                    && matches!(
                        combine_mode,
                        Some(EffectGraphCombineMode::Restore | EffectGraphCombineMode::Adaptive)
                    ) {
                    node_display_labels
                        .and_then(|labels| labels.get(&port_key.port_id).cloned())
                        .unwrap_or_else(|| effect_graph_port_label(&port_key.port_id))
                } else {
                    effect_graph_port_label(&port_key.port_id)
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
                painter.circle_filled(*pin_pos, 6.5, Color32::from_rgb(110, 170, 255));
                painter.text(
                    egui::pos2(pin_pos.x - 10.0, pin_pos.y),
                    egui::Align2::RIGHT_CENTER,
                    effect_graph_port_label(&port_key.port_id),
                    egui::TextStyle::Small.resolve(ui.style()),
                    Color32::from_rgb(180, 194, 208),
                );
                if matches!(&node.data, EffectGraphNodeData::SplitChannels)
                    && self.effect_graph.tester.last_input_bus.is_some()
                {
                    let port_index = effect_graph_port_sort_index(&port_key.port_id);
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
            let mut mono_mix_ignored_channels = None;
            let mut semitones = None;
            let mut rate = None;
            let mut waveform_zoom = None;
            let mut spectrum_mode = None;
            let mut spectrum_zoom = None;
            match &node.data {
                EffectGraphNodeData::Gain { gain_db: value } => gain_db = Some(*value),
                EffectGraphNodeData::MonoMix { ignored_channels } => {
                    mono_mix_ignored_channels = Some(ignored_channels.clone());
                }
                EffectGraphNodeData::PitchShift { semitones: value } => semitones = Some(*value),
                EffectGraphNodeData::TimeStretch { rate: value }
                | EffectGraphNodeData::Speed { rate: value } => rate = Some(*value),
                EffectGraphNodeData::DebugWaveform { zoom: value } => waveform_zoom = Some(*value),
                EffectGraphNodeData::DebugSpectrum { mode, zoom: value } => {
                    spectrum_mode = Some(*mode);
                    spectrum_zoom = Some(*value);
                }
                EffectGraphNodeData::Input
                | EffectGraphNodeData::Output
                | EffectGraphNodeData::Duplicate
                | EffectGraphNodeData::SplitChannels
                | EffectGraphNodeData::CombineChannels => {}
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
                                let audio = self
                                    .effect_graph
                                    .tester
                                    .last_input_audio
                                    .clone()
                                    .or_else(|| self.effect_graph_preview_input_audio().ok());
                                if let Some(audio) = audio {
                                    self.effect_graph_toggle_playback(
                                        EffectGraphPlaybackTarget::Input,
                                        audio,
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
                        | EffectGraphNodeData::Speed { .. } => {}
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
