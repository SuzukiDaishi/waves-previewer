//! Edit and channel tools: Edge Fade, Insert Silence, Pencil, Trim,
//! Channel Routing and the Panner.

use egui::{Color32, RichText};

use crate::app::inspector_ops::{InspectorAction, ToolCommand};
use crate::app::types::FadeShape;
use crate::app::ui::inspector::widgets::{self, Gestures};
use crate::app::WavesPreviewer;

const FADE_SHAPES: [(FadeShape, &str); 6] = [
    (FadeShape::Linear, "Linear"),
    (FadeShape::EqualPower, "Equal"),
    (FadeShape::Cosine, "Cosine"),
    (FadeShape::SCurve, "S-Curve"),
    (FadeShape::Quadratic, "Quadratic"),
    (FadeShape::Cubic, "Cubic"),
];

fn shape_label(shape: FadeShape) -> &'static str {
    FADE_SHAPES
        .iter()
        .find(|(s, _)| *s == shape)
        .map(|(_, l)| *l)
        .unwrap_or("Linear")
}

pub(super) fn fade(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize, g: &mut Gestures) {
    let tab = &mut app.tabs[tab_idx];
    let sr = tab.buffer_sample_rate.max(1) as f32;
    let clip_ms = tab.samples_len as f32 / sr * 1000.0;
    let default_ms = (clip_ms * 0.1).min(250.0).max(1.0).min(clip_ms * 0.5);
    let edges: [(&str, Color32, bool); 2] = [
        ("Start · fade in", Color32::from_rgb(105, 195, 255), true),
        ("End · fade out", Color32::from_rgb(255, 175, 105), false),
    ];
    for (title, color, is_in) in edges {
        egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (ms, shape) = if is_in {
                    (&mut tab.tool_state.fade_in_ms, &mut tab.fade_in_shape)
                } else {
                    (&mut tab.tool_state.fade_out_ms, &mut tab.fade_out_shape)
                };
                let mut enabled = *ms > 0.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).strong().color(color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = ui.checkbox(&mut enabled, "On");
                        if g.track(&r) {
                            *ms = if enabled { default_ms } else { 0.0 };
                        }
                    });
                });
                ui.add_enabled_ui(enabled, |ui| {
                    widgets::settings_grid(ui, ("insp_fade", tab_idx, is_in), |ui| {
                        widgets::drag_row(
                            ui,
                            g,
                            "Length",
                            "How long the fade lasts",
                            ms,
                            0.0..=clip_ms.max(0.0),
                            5.0,
                            " ms",
                            1,
                        );
                        ui.label("Curve");
                        egui::ComboBox::from_id_salt(("insp_fade_shape", tab_idx, is_in))
                            .selected_text(shape_label(*shape))
                            .show_ui(ui, |ui| {
                                for (value, label) in FADE_SHAPES {
                                    ui.selectable_value(shape, value, label);
                                }
                            });
                        ui.end_row();
                    });
                });
            });
    }
    let n_in = (tab.tool_state.fade_in_ms / 1000.0 * sr).round() as usize;
    let n_out = (tab.tool_state.fade_out_ms / 1000.0 * sr).round() as usize;
    if n_in.saturating_add(n_out) > tab.samples_len {
        widgets::warn(
            ui,
            "The fades overlap; both curves multiply where they meet.",
        );
    }
    widgets::note(
        ui,
        "Or drag the blue and orange handles on the waveform. Both fades are one undo step.",
    );
}

pub(super) fn insert_silence(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
) {
    let tab = &mut app.tabs[tab_idx];
    widgets::settings_grid(ui, ("insp_silence", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Duration",
            "How much silence to insert",
            &mut tab.tool_state.insert_silence_ms,
            1.0..=600_000.0,
            10.0,
            " ms",
            0,
        );
    });
    widgets::note(
        ui,
        "Markers and loop points after the insert point move right with the audio.",
    );
}

pub(super) fn pencil(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize) {
    let tab = &app.tabs[tab_idx];
    if tab.pencil_draft.is_none() {
        widgets::note(ui, "Press Edit below to start a green draft. Until then a click on the waveform moves the playhead.");
    } else if tab.samples_per_px > 0.5 {
        widgets::warn(
            ui,
            "Zoom in further to edit points (needs more than 2 px per sample).",
        );
    } else {
        widgets::note(ui, "Drag a point to move it; drag empty space to select points, then drag one of them to move the group.");
        ui.label(
            RichText::new("Hold Ctrl and drag to draw. A plain click only selects.")
                .small()
                .color(Color32::from_rgb(90, 210, 245)),
        );
    }
    widgets::note(ui, "Works on the lane under the pointer (every channel in Mix view). Ctrl+Z undoes one move or stroke of the draft.");
}

pub(super) fn trim(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let virtual_trim = app.virtual_trim_status_for_tab(tab_idx);
    let tab = &mut app.tabs[tab_idx];
    let mode = &mut tab.tool_state.trim_mode;
    ui.horizontal(|ui| {
        ui.label("Apply does").on_hover_text(
            "Cut removes the range and closes the gap. Mute silences it, keeping the length. Trim keeps only the range.",
        );
        for (value, label) in [(2u8, "Trim"), (0, "Cut"), (1, "Mute")] {
            if ui.selectable_label(*mode == value, label).clicked() {
                *mode = value;
            }
        }
    });
    widgets::note(
        ui,
        match *mode {
            0 => "Removes the selection and closes the gap (same as Delete).",
            1 => "Silences the selection, keeping its length (same as Ctrl+M).",
            _ => "Keeps only the selection and discards the rest (same as T).",
        },
    );
    let has_range = !tab.all_selected_ranges().is_empty();
    if ui
        .add_enabled(has_range, egui::Button::new("Add Trim As Virtual"))
        .on_hover_text("Put the selection in the list as a new item, leaving this file as it is. Nothing is written to disk until you save it.")
        .clicked()
    {
        actions.push(InspectorAction::Tool(ToolCommand::TrimVirtual));
    }
    if let Some((message, progress)) = virtual_trim {
        ui.add(
            egui::ProgressBar::new(progress.clamp(0.0, 1.0))
                .desired_width(ui.available_width())
                .text(message),
        );
    }

    widgets::section(ui, "Auto Trim");
    let running = tab.auto_trim_state.as_ref().is_some_and(|s| s.running);
    let message = tab
        .auto_trim_state
        .as_ref()
        .map(|s| s.message.clone())
        .unwrap_or_default();
    let stats = tab.auto_trim_state.as_ref().and_then(|s| s.stats);
    let live_pending = tab
        .auto_trim_state
        .as_ref()
        .is_some_and(|s| s.config_dirty_at.is_some());
    let mut changed = false;
    ui.add_enabled_ui(!running, |ui| {
        let cfg = &mut tab.auto_trim_config;
        widgets::settings_grid(ui, ("insp_auto_trim", tab_idx), |ui| {
            changed |= widgets::slider_row(
                ui,
                g,
                "Noise gate",
                "How far above the noise floor a block must be to count as sound",
                &mut cfg.threshold_above_noise_db,
                0.0..=48.0,
                " dB",
                1,
                false,
            );
            changed |= widgets::slider_row(
                ui,
                g,
                "Peak gate",
                "How far below the loudest block audio may fall before it counts as silence",
                &mut cfg.threshold_below_peak_db,
                0.0..=80.0,
                " dB",
                1,
                false,
            );
            changed |= widgets::slider_row(
                ui,
                g,
                "Pre-roll",
                "Silence kept before each section",
                &mut cfg.pre_roll_secs,
                0.0..=2.0,
                " s",
                3,
                false,
            );
            changed |= widgets::slider_row(
                ui,
                g,
                "Post-roll",
                "Silence kept after each section (release tails)",
                &mut cfg.post_roll_secs,
                0.0..=2.0,
                " s",
                3,
                false,
            );
            changed |= widgets::slider_row(
                ui,
                g,
                "Gap merge",
                "Silent gaps shorter than this stay inside one section",
                &mut cfg.gap_merge_secs,
                0.0..=2.0,
                " s",
                3,
                false,
            );
            changed |= widgets::slider_row(
                ui,
                g,
                "Min active",
                "Sections shorter than this are ignored as blips",
                &mut cfg.min_active_secs,
                0.0..=1.0,
                " s",
                3,
                false,
            );
        });
    });
    if changed {
        if let Some(state) = tab.auto_trim_state.as_mut() {
            if state.result.is_some() || state.stats.is_some() {
                state.config_dirty_at = Some(std::time::Instant::now());
            }
        }
    }
    if live_pending || changed {
        ui.ctx()
            .request_repaint_after(crate::app::ui_timing::BUSY_INDICATOR_DELAY);
    }
    match stats {
        Some(stats) => {
            widgets::note(
                ui,
                &format!(
                    "noise {:.1} dB · peak {:.1} dB · threshold {:.1} dB{}",
                    stats.noise_floor_db,
                    stats.peak_db,
                    stats.threshold_db,
                    if stats.uniform_signal {
                        " (uniform: peak gate only)"
                    } else {
                        ""
                    }
                ),
            );
        }
        None => widgets::note(ui, "Run Auto Trim once to measure the levels."),
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !running && !tab.ch_samples.is_empty(),
                egui::Button::new("Auto Trim"),
            )
            .on_hover_text("Find the sections with sound and select them; changes above re-run it")
            .clicked()
        {
            actions.push(InspectorAction::Tool(ToolCommand::AutoTrimStart));
        }
        if running && ui.button("Stop").clicked() {
            actions.push(InspectorAction::Tool(ToolCommand::AutoTrimCancel));
        }
        if live_pending && !running {
            ui.label(RichText::new("updating…").weak().italics());
        }
    });
    if running {
        let progress = tab.auto_trim_state.as_ref().map_or(0.0, |s| s.progress);
        ui.add(
            egui::ProgressBar::new(progress.clamp(0.0, 1.0))
                .desired_width(ui.available_width())
                .show_percentage(),
        );
    }
    if !message.is_empty() {
        widgets::note(ui, &message);
    }
}

pub(super) fn channel_routing(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    actions: &mut Vec<InspectorAction>,
) {
    let tab = &mut app.tabs[tab_idx];
    let in_count = tab.ch_samples.len().max(1);
    if WavesPreviewer::ui_channel_routing_patchbay(ui, &mut tab.channel_routing_draft, in_count) {
        actions.push(InspectorAction::Apply);
    }
    widgets::note(ui, "The whole file; the selection and channel view are not used. One output fed by several inputs gets their average.");
}

pub(super) fn panner(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    actions: &mut Vec<InspectorAction>,
) {
    use crate::app::ui::pan_controls::{labelled_knob, pan_field, PAN_KNOB_BALANCE_SWEEP_DEG};
    use crate::panning::{PanMode, PAN_TURN_DEG};
    let view = app.panner_view(tab_idx);
    let tab = &mut app.tabs[tab_idx];
    if view.layout.is_empty() {
        widgets::note(ui, "Waiting for the audio to load.");
        return;
    }
    let mono = view.mono();
    let mode = tab.tool_state.pan_mode_for(&view.layout);
    let mut settled = false;
    ui.horizontal(|ui| {
        ui.label("Mode");
        for candidate in [PanMode::Balance, PanMode::Vbap] {
            let mut resp = ui.selectable_label(mode == candidate, candidate.label());
            if candidate == PanMode::Vbap && !mono && !view.three_d {
                resp = resp.on_hover_text(
                    "This layout has no height speakers: the turn stays on the horizontal ring, and pitch and roll do nothing.",
                );
            }
            if resp.clicked() && mode != candidate {
                tab.tool_state.pan_mode = Some(candidate);
                settled = true;
            }
        }
    });
    widgets::note(
        ui,
        if mono {
            "Mono → stereo: Apply makes the file two channels."
        } else if mode == PanMode::Balance {
            "Turns down the side the pan moves away from. Channels on the centre line and the LFE stay as they are."
        } else {
            "Turns the whole sound field and lays every channel back onto the speakers by VBAP. The LFE never moves."
        },
    );
    match mode {
        PanMode::Balance => {
            let edit = labelled_knob(
                ui,
                ui.id().with("panner_balance"),
                "Balance",
                "Drag right or up to go right; Shift is finer. Double-click: centre.",
                tab.tool_state.pan_balance * 100.0,
                -100.0..=100.0,
                100.0,
                PAN_KNOB_BALANCE_SWEEP_DEG,
                " %",
                0,
                true,
            );
            if let Some(value) = edit.value {
                tab.tool_state.pan_balance = value / 100.0;
            }
            settled |= edit.finished;
        }
        PanMode::Vbap => {
            let tilt = view.three_d && !mono;
            let knobs: [(&str, &str, f32, std::ops::RangeInclusive<f32>, bool); 3] = [
                (
                    "Yaw",
                    "Turns the field right (drag right or up) or left. Double-click: front.",
                    tab.tool_state.pan_yaw_deg,
                    -180.0..=180.0,
                    true,
                ),
                (
                    "Pitch",
                    "Lifts the front up (or tips it down). Double-click: level.",
                    tab.tool_state.pan_pitch_deg,
                    -90.0..=90.0,
                    tilt,
                ),
                (
                    "Roll",
                    "Lowers the right side (or the left). Double-click: level.",
                    tab.tool_state.pan_roll_deg,
                    -180.0..=180.0,
                    tilt,
                ),
            ];
            let mut turned = [None; 3];
            ui.horizontal_top(|ui| {
                for (i, (label, hover, value, range, enabled)) in knobs.into_iter().enumerate() {
                    let edit = labelled_knob(
                        ui,
                        ui.id().with(("panner_turn", i)),
                        label,
                        hover,
                        value,
                        range,
                        PAN_TURN_DEG,
                        PAN_TURN_DEG,
                        "\u{b0}",
                        0,
                        enabled,
                    );
                    turned[i] = edit.value;
                    settled |= edit.finished;
                }
            });
            if let Some(v) = turned[0] {
                tab.tool_state.pan_yaw_deg = v;
            }
            if let Some(v) = turned[1] {
                tab.tool_state.pan_pitch_deg = v;
            }
            if let Some(v) = turned[2] {
                tab.tool_state.pan_roll_deg = v;
            }
            if !tilt {
                widgets::note(
                    ui,
                    if mono {
                        "A mono file turns between the two speakers of the stereo pair it becomes."
                    } else {
                        "No height speakers: pitch and roll have nowhere to go."
                    },
                );
            }
            pan_field(ui, &view.speakers, &view.sources);
        }
    }
    if settled {
        actions.push(InspectorAction::Tool(ToolCommand::PannerSettled));
    }
}
