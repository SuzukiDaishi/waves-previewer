//! Loop points, markers and notes. Edits to these are kept as they are
//! made -- no Apply -- and one gesture (a drag, a typed value, a button) is
//! one undo step. Only the Loop tool's audio operations (baking the seam,
//! writing the loop out) go through the footer's Preview / Apply.

use egui::{Color32, RichText};

use crate::app::inspector_ops::{InspectorAction, ToolCommand};
use crate::app::ui::inspector::widgets::{self, Gestures};
use crate::app::WavesPreviewer;

/// How much audio either side of the loop jump the seam check shows.
const SEAM_WINDOW_MS_DEFAULT: f32 = 40.0;

pub(super) fn loop_edit(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let sr_u32 = app.tabs[tab_idx].buffer_sample_rate.max(1);
    let tab = &mut app.tabs[tab_idx];
    let sr = sr_u32 as f32;
    let len = tab.samples_len;
    let len_secs = len as f32 / sr;

    widgets::section(ui, "Loop range");
    let mut region = tab.loop_region.filter(|(a, b)| b > a);
    let mut loop_changed = false;
    let mut began = false;
    let mut ended = false;
    widgets::settings_grid(ui, ("insp_loop_range", tab_idx), |ui| {
        let (mut start, mut end) = region
            .map(|(a, b)| (a as f32 / sr, b as f32 / sr))
            .unwrap_or((0.0, 0.0));
        ui.label("Start");
        let r1 = ui.add_enabled(
            region.is_some(),
            egui::DragValue::new(&mut start)
                .range(0.0..=len_secs)
                .speed(0.001)
                .fixed_decimals(3)
                .suffix(" s"),
        );
        ui.end_row();
        ui.label("End");
        let r2 = ui.add_enabled(
            region.is_some(),
            egui::DragValue::new(&mut end)
                .range(0.0..=len_secs)
                .speed(0.001)
                .fixed_decimals(3)
                .suffix(" s"),
        );
        ui.end_row();
        for r in [&r1, &r2] {
            began |= r.drag_started() || r.gained_focus();
            ended |= r.drag_stopped() || r.lost_focus();
        }
        if r1.changed() || r2.changed() {
            let a = ((start.min(end)) * sr).round() as usize;
            let b = ((start.max(end)) * sr).round() as usize;
            if b > a {
                region = Some((a.min(len), b.min(len)));
                loop_changed = true;
            }
        }
    });
    let selection = WavesPreviewer::editor_selected_range(tab);
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(selection.is_some(), egui::Button::new("Set from selection"))
            .on_hover_text("Make the selection the loop")
            .clicked()
        {
            region = selection;
            loop_changed = true;
            began = true;
            ended = true;
        }
        if ui
            .add_enabled(region.is_some(), egui::Button::new("Clear"))
            .clicked()
        {
            region = None;
            loop_changed = true;
            began = true;
            ended = true;
        }
    });
    if region.is_none() {
        widgets::note(ui, "No loop yet: select a range and Set it, press K / P at the playhead, or run Auto Detect.");
    }

    widgets::section(ui, "Seam");
    let mut xfade_ms = tab.loop_xfade_samples as f32 / sr * 1000.0;
    let mut uses_dip = WavesPreviewer::loop_xfade_uses_through_zero(tab.loop_xfade_shape);
    let mut equal = matches!(
        tab.loop_xfade_shape,
        crate::app::types::LoopXfadeShape::EqualPower
            | crate::app::types::LoopXfadeShape::EqualPowerDip
    );
    let mut seam_changed = false;
    widgets::settings_grid(ui, ("insp_loop_seam", tab_idx), |ui| {
        ui.label("Crossfade")
            .on_hover_text("How long the loop's end blends into its start when it plays around");
        let r = ui.add(
            egui::DragValue::new(&mut xfade_ms)
                .range(0.0..=5000.0)
                .speed(1.0)
                .fixed_decimals(1)
                .suffix(" ms"),
        );
        began |= r.drag_started() || r.gained_focus();
        ended |= r.drag_stopped() || r.lost_focus();
        seam_changed |= r.changed();
        ui.end_row();
        ui.label("Mode");
        ui.horizontal(|ui| {
            for (value, label) in [(false, "Crossfade"), (true, "Fade to 0")] {
                if ui.selectable_label(uses_dip == value, label).clicked() && uses_dip != value {
                    uses_dip = value;
                    seam_changed = true;
                    began = true;
                    ended = true;
                }
            }
        });
        ui.end_row();
        ui.label("Shape");
        ui.horizontal(|ui| {
            for (value, label) in [(false, "Linear"), (true, "Equal power")] {
                if ui.selectable_label(equal == value, label).clicked() && equal != value {
                    equal = value;
                    seam_changed = true;
                    began = true;
                    ended = true;
                }
            }
        });
        ui.end_row();
    });

    // The edits are applied to the tab now, inside one undo step per gesture.
    if began {
        WavesPreviewer::annotation_begin(tab, "Loop Edit");
    }
    if loop_changed {
        tab.loop_region = region;
        if region.is_some() {
            tab.loop_mode = crate::app::types::LoopMode::Marker;
        }
    }
    if seam_changed {
        use crate::app::types::LoopXfadeShape as S;
        tab.loop_xfade_samples = ((xfade_ms.max(0.0) / 1000.0) * sr).round() as usize;
        tab.loop_xfade_shape = match (uses_dip, equal) {
            (false, false) => S::Linear,
            (false, true) => S::EqualPower,
            (true, false) => S::LinearDip,
            (true, true) => S::EqualPowerDip,
        };
    }
    if loop_changed || seam_changed {
        WavesPreviewer::editor_sync_annotation_layers(tab);
        actions.push(InspectorAction::Tool(ToolCommand::LoopChanged));
    }
    if ended {
        actions.push(InspectorAction::Tool(ToolCommand::SettleAnnotations));
    }

    let seam_window_id = egui::Id::new(("loop_seam_window_ms", tab.tab_id));
    let mut seam_window_ms: f32 = ui
        .ctx()
        .data_mut(|d| *d.get_temp_mut_or(seam_window_id, SEAM_WINDOW_MS_DEFAULT));
    ui.horizontal(|ui| {
        ui.label(RichText::new("Seam check").small())
            .on_hover_text("The end of the loop (left) running into its start (right)");
        ui.spacing_mut().slider_width = (ui.available_width() - 60.0).clamp(60.0, 180.0);
        if ui
            .add(
                egui::Slider::new(&mut seam_window_ms, 2.0..=250.0)
                    .suffix(" ms")
                    .logarithmic(true)
                    .fixed_decimals(0),
            )
            .changed()
        {
            ui.ctx()
                .data_mut(|d| d.insert_temp(seam_window_id, seam_window_ms));
        }
    });
    if let Some(preview) =
        WavesPreviewer::build_loop_seam_preview(tab, sr_u32, seam_window_ms / 1000.0)
    {
        WavesPreviewer::draw_loop_seam_join(ui, &preview, seam_window_ms);
    }

    widgets::section(ui, "Apply does");
    ui.horizontal_wrapped(|ui| {
        let unroll = &mut tab.tool_state.loop_apply_unroll;
        if ui
            .selectable_label(!*unroll, "Bake the seam")
            .on_hover_text("Write the crossfade into the audio at the loop points, so any player loops it smoothly")
            .clicked()
        {
            *unroll = false;
        }
        if ui
            .selectable_label(*unroll, "Write the loop out")
            .on_hover_text("Repeat the loop in the file, making it longer")
            .clicked()
        {
            *unroll = true;
        }
    });
    if tab.tool_state.loop_apply_unroll {
        widgets::settings_grid(ui, ("insp_loop_repeat", tab_idx), |ui| {
            ui.label("Times");
            let mut repeat = tab.tool_state.loop_repeat.max(2);
            let r = ui.add(egui::DragValue::new(&mut repeat).range(2..=128).speed(0.1));
            if g.track(&r) {
                tab.tool_state.loop_repeat = repeat;
            }
            ui.end_row();
        });
    }

    egui::CollapsingHeader::new("Auto Detect")
        .id_salt(("insp_loop_detect", tab_idx))
        .default_open(tab.loop_detect_state.is_some())
        .show(ui, |ui| {
            let state = tab.loop_detect_state.as_ref();
            let running = state.is_some_and(|s| s.running);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        !running && !tab.ch_samples.is_empty(),
                        egui::Button::new("Auto Detect"),
                    )
                    .on_hover_text("Find candidate loop points")
                    .clicked()
                {
                    actions.push(InspectorAction::Tool(ToolCommand::LoopDetectStart));
                }
                if running && ui.button("Stop").clicked() {
                    actions.push(InspectorAction::Tool(ToolCommand::LoopDetectCancel));
                }
            });
            if let Some(state) = state {
                if running {
                    ui.add(
                        egui::ProgressBar::new(state.progress.clamp(0.0, 1.0))
                            .desired_width(ui.available_width())
                            .show_percentage(),
                    );
                }
                if !state.message.is_empty() {
                    widgets::note(ui, &state.message);
                }
                for (ci, c) in state.candidates.iter().enumerate().take(3) {
                    let quality = match c.confidence.label() {
                        "High" => Color32::from_rgb(120, 220, 140),
                        "Medium" => Color32::from_rgb(240, 200, 90),
                        _ => Color32::from_rgb(170, 170, 180),
                    };
                    let text = format!(
                        "{:.2} │ {:.2}s → {:.2}s ({:.2}s)",
                        c.score,
                        c.start as f32 / sr,
                        c.end as f32 / sr,
                        c.end.saturating_sub(c.start) as f32 / sr
                    );
                    let resp = ui.add_sized(
                        [ui.available_width(), 18.0],
                        egui::Button::selectable(
                            ci == state.selected_idx,
                            RichText::new(text).monospace().color(quality),
                        ),
                    );
                    if resp
                        .on_hover_text(format!(
                            "{} confidence — click to use it",
                            c.confidence.label()
                        ))
                        .clicked()
                    {
                        actions.push(InspectorAction::Tool(ToolCommand::LoopDetectApply(ci)));
                    }
                }
            }
        });
}

pub(super) fn markers(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    actions: &mut Vec<InspectorAction>,
) {
    let tab = &mut app.tabs[tab_idx];
    let sr = tab.buffer_sample_rate.max(1) as f32;
    let len = tab.samples_len;
    let len_secs = len as f32 / sr;
    ui.horizontal_wrapped(|ui| {
        if ui
            .button("Add at playhead")
            .on_hover_text("Same as M")
            .clicked()
        {
            actions.push(InspectorAction::Tool(ToolCommand::AddMarkerAtPlayhead));
        }
        if ui
            .add_enabled(!tab.markers.is_empty(), egui::Button::new("Clear all"))
            .clicked()
        {
            WavesPreviewer::annotation_begin(tab, "Clear Markers");
            tab.markers.clear();
            WavesPreviewer::editor_sync_annotation_layers(tab);
            actions.push(InspectorAction::Tool(ToolCommand::SettleAnnotations));
        }
        ui.label(RichText::new(format!("{} marker(s)", tab.markers.len())).weak());
    });
    if tab.markers.is_empty() {
        widgets::note(ui, "Markers are saved into the file (and the session).");
        return;
    }
    let mut local = tab.markers.clone();
    let mut remove: Option<usize> = None;
    let mut changed = false;
    let mut resort = false;
    let mut began = false;
    let mut ended = false;
    let mut seek: Option<usize> = None;
    egui::Grid::new(("insp_markers", tab_idx))
        .num_columns(4)
        .spacing(egui::vec2(6.0, 4.0))
        .striped(true)
        .show(ui, |ui| {
            for (idx, m) in local.iter_mut().enumerate() {
                let r_label = ui.add(egui::TextEdit::singleline(&mut m.label).desired_width(72.0));
                let mut secs = (m.sample as f32 / sr).min(len_secs);
                let r_time = ui.add(
                    egui::DragValue::new(&mut secs)
                        .range(0.0..=len_secs)
                        .speed(0.001)
                        .fixed_decimals(3)
                        .suffix(" s"),
                );
                for r in [&r_label, &r_time] {
                    began |= r.drag_started() || r.gained_focus();
                    ended |= r.drag_stopped() || r.lost_focus();
                }
                changed |= r_label.changed();
                if r_time.changed() {
                    m.sample = ((secs.max(0.0)) * sr).round() as usize;
                    m.sample = m.sample.min(len);
                    changed = true;
                    resort = true;
                }
                if ui.small_button("⏵").on_hover_text("Go there").clicked() {
                    seek = Some(m.sample);
                }
                if ui.small_button("✕").on_hover_text("Delete").clicked() {
                    remove = Some(idx);
                }
                ui.end_row();
            }
        });
    if let Some(idx) = remove {
        local.remove(idx);
        changed = true;
        began = true;
        ended = true;
    }
    if resort {
        local.sort_by_key(|m| m.sample);
    }
    if began {
        WavesPreviewer::annotation_begin(tab, "Edit Markers");
    }
    if changed {
        tab.markers = local;
        WavesPreviewer::editor_sync_annotation_layers(tab);
    }
    if ended {
        actions.push(InspectorAction::Tool(ToolCommand::SettleAnnotations));
    }
    if let Some(sample) = seek {
        actions.push(InspectorAction::Tool(ToolCommand::Seek(sample)));
    }
}

pub(super) fn note(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize) {
    let playhead = app.editor_playhead_display_sample(tab_idx);
    let tab = &mut app.tabs[tab_idx];
    let sr = tab.buffer_sample_rate.max(1) as f32;
    let mut seek = None;
    let changed = WavesPreviewer::ui_editor_note_panel(ui, tab, playhead, sr, &mut seek);
    if changed {
        app.sync_editor_notes_from_tab(tab_idx);
    }
    if let Some(sample) = seek {
        app.editor_seek_display_sample(tab_idx, sample);
    }
}
