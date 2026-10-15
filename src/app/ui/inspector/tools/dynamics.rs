//! The EQ and the dynamics tools: graphs over the audio they act on, then
//! the settings. The graphs come from `plots`; the audio behind them from
//! the tool session -- the source measured when the tool opened
//! (`source_analysis`), the render of the settings (the preview's
//! `analysis`), and what is playing (`live`).

use egui::{RichText, Vec2};

use crate::app::inspector_ops::{InspectorAction, ToolCommand};
use crate::app::tool_render::{RenderAnalysis, RenderTarget};
use crate::app::ui::inspector::plots::{self, HistoryLine, HistoryView, PlotEdit, Transfer};
use crate::app::ui::inspector::widgets::{self, Gestures};
use crate::app::WavesPreviewer;
use crate::app::tool_automation::{self, AutoParam};
use crate::app::types::ToolState;
use crate::automation::Curve;
use crate::dynamics::{
    CompParam, CompressorSettings, Detection, DynamicsTrace, GateSettings, LimiterParam,
    LimiterSettings,
};
use crate::eq::{CutSlope, EqBand, EqChannelMode, EqKind, EqParam, ParametricEqParams};

/// The meters' width beside a graph.
const METERS_W: f32 = 66.0;
/// Graph heights.
const EQ_PLOT_H: f32 = 210.0;
const TRANSFER_H: f32 = 170.0;
const HISTORY_H: f32 = 96.0;
/// Colours of the time graph's lines.
const THRESHOLD_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 170, 70);
const CLOSE_COLOR: egui::Color32 = egui::Color32::from_rgb(246, 208, 80);
const CEILING_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 104, 104);

fn note_plot(g: &mut Gestures, edit: PlotEdit) {
    g.began |= edit.began;
    g.ended |= edit.ended;
}

/// The target's span in tab samples.
fn target_range(app: &WavesPreviewer, tab_idx: usize) -> (usize, usize) {
    let tool = app.tabs[tab_idx].active_tool;
    match app.tool_target_for_tab(tab_idx, tool) {
        RenderTarget::Range(a, b) => (a.min(b), a.max(b)),
        _ => (0, app.tabs[tab_idx].samples_len),
    }
}

/// The render of the current settings, when it is what is heard.
fn rendered(app: &WavesPreviewer, tab_idx: usize) -> Option<&RenderAnalysis> {
    let session = &app.tabs[tab_idx].tool_session;
    session
        .preview
        .as_ref()
        .and_then(|p| p.output.analysis.as_deref())
}

fn rendered_trace(app: &WavesPreviewer, tab_idx: usize) -> Option<DynamicsTrace> {
    match rendered(app, tab_idx) {
        Some(RenderAnalysis::Dynamics(t)) => Some(t.clone()),
        _ => None,
    }
}

fn source_levels(app: &WavesPreviewer, tab_idx: usize) -> Option<DynamicsTrace> {
    app.tabs[tab_idx]
        .tool_session
        .source_analysis
        .as_ref()
        .and_then(|a| a.levels.clone())
}

/// The preset menu of the active tool.
fn preset_menu(
    app: &WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    actions: &mut Vec<InspectorAction>,
) {
    let tool = app.tabs[tab_idx].active_tool;
    let presets = crate::app::tool_presets::presets(tool);
    if presets.is_empty() {
        return;
    }
    egui::ComboBox::from_id_salt(("tool_preset", tab_idx, tool))
        .selected_text("Preset...")
        .width(130.0)
        .show_ui(ui, |ui| {
            for (i, preset) in presets.iter().enumerate() {
                if ui.selectable_label(false, preset.name).clicked() {
                    actions.push(InspectorAction::Tool(ToolCommand::Preset(i)));
                }
            }
        });
}

/// A frequency where 0 means the filter is off.
#[allow(clippy::too_many_arguments)]
fn hz_or_off_row(
    ui: &mut egui::Ui,
    g: &mut Gestures,
    label: &str,
    hover: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) {
    ui.label(label).on_hover_text(hover);
    let r = ui
        .add(
            egui::DragValue::new(value)
                .range(range)
                .speed(2.0)
                .custom_formatter(|v, _| {
                    if v <= 0.0 {
                        "Off".to_string()
                    } else {
                        format!("{v:.0} Hz")
                    }
                }),
        )
        .on_hover_text(hover);
    g.track(&r);
    ui.end_row();
}

fn detection_row(ui: &mut egui::Ui, g: &mut Gestures, detection: &mut Detection) {
    ui.label("Detect")
        .on_hover_text("Peak reacts to transients; RMS to loudness");
    ui.horizontal(|ui| {
        for d in [Detection::Peak, Detection::Rms] {
            if ui.selectable_label(*detection == d, d.label()).clicked() && *detection != d {
                *detection = d;
                g.began = true;
                g.ended = true;
            }
        }
    });
    ui.end_row();
}

/// Seek to a sample of the tab's source.
fn seek(actions: &mut Vec<InspectorAction>, edit: &PlotEdit) {
    if let Some(sample) = edit.seek {
        actions.push(InspectorAction::Tool(ToolCommand::Seek(sample)));
    }
}

/// What the panels need to know about automation, read before the tab's
/// settings are borrowed: which settings follow a curve, and all settings
/// as they are at the playhead.
struct AutoView {
    automated: Vec<AutoParam>,
    at: ToolState,
}

impl AutoView {
    fn new(app: &WavesPreviewer, tab_idx: usize) -> Self {
        let playhead = app.automation_playhead(tab_idx);
        let tab = &app.tabs[tab_idx];
        Self {
            automated: tab
                .automation
                .iter()
                .filter(|l| l.is_active())
                .map(|l| l.param)
                .collect(),
            at: tool_automation::tool_state_at(tab, playhead),
        }
    }

    fn is(&self, param: AutoParam) -> bool {
        self.automated.contains(&param)
    }

    /// The read-only row of an automated setting.
    fn row(&self, ui: &mut egui::Ui, param: AutoParam, label: &str) {
        widgets::automated_row(ui, label, param.format(param.static_value(&self.at)));
    }

    /// The curve a setting follows, for the time graph.
    fn curve(&self, tab: &crate::app::types::EditorTab, param: AutoParam) -> Option<Curve> {
        tool_automation::lane(tab, param)
            .filter(|l| l.is_active())
            .map(|l| Curve::new(l.points.clone(), param.scale()))
    }
}

/// The Automation section of the EQ, Compressor and Limiter: which setting
/// the waveform's curve edits, whether that setting follows it, and what
/// else of this tool does. `eq_band` is the EQ band picked in its panel.
fn automation_section(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize, g: &mut Gestures, eq_band: usize) {
    let tab = &mut app.tabs[tab_idx];
    let tool = tab.active_tool;
    let params = AutoParam::for_tool(tool, eq_band, &tab.tool_state);
    if params.is_empty() {
        return;
    }
    widgets::section(ui, "Automation");
    let mut picked = tab
        .automation_selected
        .filter(|p| params.contains(p))
        .unwrap_or(params[0]);
    let name = |p: AutoParam, tab: &crate::app::types::EditorTab| {
        let mut text = p.label();
        if tool_automation::is_automated(tab, p) {
            text.push_str("  \u{25CF}");
        }
        text
    };
    ui.horizontal(|ui| {
        ui.label("Setting").on_hover_text("The setting the curve on the waveform draws");
        egui::ComboBox::from_id_salt(("automation_param", tab_idx, tool))
            .selected_text(name(picked, tab))
            .width(160.0)
            .show_ui(ui, |ui| {
                for p in &params {
                    if ui.selectable_label(picked == *p, name(*p, tab)).clicked() {
                        picked = *p;
                    }
                }
            });
    });
    if tab.automation_selected != Some(picked) {
        tab.automation_selected = Some(picked);
        tab.automation_drag = None;
    }
    let mut on = tool_automation::lane(tab, picked).is_some_and(|l| l.enabled);
    if ui
        .checkbox(&mut on, "Curve on the waveform")
        .on_hover_text("The setting follows a curve you draw over the audio instead of one value")
        .changed()
    {
        tool_automation::lane_mut(tab, picked).enabled = on;
        tab.automation_drag = None;
        g.began = true;
        g.ended = true;
    }
    if on {
        let count = tool_automation::lane(tab, picked).map_or(0, |l| l.points.len());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{count} point(s)")).weak());
            if ui
                .add_enabled(count > 0, egui::Button::new("Clear points"))
                .clicked()
            {
                tool_automation::lane_mut(tab, picked).points.clear();
                tab.automation_drag = None;
                g.began = true;
                g.ended = true;
            }
        });
        widgets::note(ui, CURVE_HINT);
        if !picked.has_effect(&tab.tool_state) {
            widgets::warn(ui, "This setting does nothing as the tool is set now (a cut has no gain; Auto makeup and Peak detection ignore theirs).");
        }
    }
    // Everything else of this tool that follows a curve, to pick or clear.
    let others: Vec<(AutoParam, usize)> = tab
        .automation
        .iter()
        .filter(|l| l.param.tool() == tool && l.is_active())
        .map(|l| (l.param, l.points.len()))
        .collect();
    if !others.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Following a curve:").small().weak());
            for (p, n) in &others {
                if ui
                    .small_button(format!("{} ({n})", p.label()))
                    .on_hover_text("Edit this curve on the waveform")
                    .clicked()
                {
                    tab.automation_selected = Some(*p);
                    tab.automation_drag = None;
                }
            }
            if ui
                .small_button("Clear all")
                .on_hover_text("Every curve of this tool goes; its settings hold their values again")
                .clicked()
            {
                tool_automation::clear_tool(tab, tool);
                g.began = true;
                g.ended = true;
            }
        });
    }
}

const CURVE_HINT: &str = "Double-click the waveform to add a point, drag a point to move it, double-click it to remove it. Before the first point and after the last the setting holds.";

// ---------------------------------------------------------------------------
// EQ

pub(super) fn eq(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let now = ui.input(|i| i.time);
    app.update_tool_live_meters(tab_idx, now);
    let show_id = egui::Id::new(("eq_show", app.tabs[tab_idx].tab_id));
    let (show_pre, show_post, show_live): (bool, bool, bool) =
        ui.data_mut(|d| *d.get_temp_mut_or(show_id, (true, true, true)));
    let post = match rendered(app, tab_idx) {
        Some(RenderAnalysis::Spectrum(s)) if show_post => Some(s.clone()),
        _ => None,
    };
    // A band slot that was emptied (however) leaves no curves behind for
    // the next band put there to inherit.
    tool_automation::prune_unused_eq_bands(&mut app.tabs[tab_idx]);
    let auto = AutoView::new(app, tab_idx);
    let tab = &mut app.tabs[tab_idx];
    let pre = tab
        .tool_session
        .source_analysis
        .as_ref()
        .and_then(|a| a.spectrum.clone())
        .filter(|_| show_pre);
    let live = (show_live && tab.tool_session.live.playing)
        .then(|| tab.tool_session.live.spectrum_db.clone());
    let stereo = tab.ch_samples.len() == 2;
    let sel_id = egui::Id::new(("eq_selected", tab.tab_id));
    let mut selected: usize = ui.data_mut(|d| *d.get_temp_mut_or(sel_id, 1));
    if !tab.tool_state.eq.bands.get(selected).is_some_and(|b| b.enabled) {
        selected = tab
            .tool_state
            .eq
            .bands
            .iter()
            .position(|b| b.enabled)
            .unwrap_or(0);
    }
    let view = plots::EqView {
        sample_rate: tab.buffer_sample_rate.max(1),
        pre: pre.as_deref(),
        post: post.as_deref(),
        live: live.as_deref(),
        stereo,
    };
    let width = ui.available_width();
    // The graph shows the EQ at the playhead; what is dragged there goes
    // back into the settings unless a curve owns it.
    let before = auto.at;
    let mut shown = before;
    let edit = plots::eq_plot(
        ui,
        egui::Id::new(("eq_plot", tab.tab_id)),
        &mut shown.eq,
        &mut selected,
        &view,
        Vec2::new(width, EQ_PLOT_H),
    );
    note_plot(g, edit);
    if shown.eq != before.eq {
        tool_automation::merge_unautomated(tab, &before, &shown);
        tool_automation::prune_unused_eq_bands(tab);
    }
    let params = &mut tab.tool_state.eq;

    // The bands, as chips; then the one picked.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for i in 0..params.bands.len() {
            let b = params.bands[i];
            if !b.enabled {
                continue;
            }
            let color = plots::BAND_COLORS[i];
            let text = RichText::new(format!(" {} ", i + 1))
                .color(if b.bypassed {
                    color.gamma_multiply(0.5)
                } else {
                    color
                })
                .strong();
            if ui
                .selectable_label(selected == i, text)
                .on_hover_text(format!("{} at {:.0} Hz", b.kind.label(), b.freq_hz))
                .clicked()
            {
                selected = i;
            }
        }
        if let Some(slot) = params.free_slot() {
            if ui
                .small_button("+ Band")
                .on_hover_text("Or double-click the graph")
                .clicked()
            {
                params.bands[slot] = EqBand {
                    enabled: true,
                    ..EqBand::default()
                };
                selected = slot;
                g.began = true;
                g.ended = true;
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(sel_id, selected));
    let slot = selected as u8;
    let mut deleted = false;
    if let Some(b) = params.bands.get_mut(selected).filter(|b| b.enabled) {
        widgets::settings_grid(ui, ("insp_eq_band", tab_idx), |ui| {
            ui.label("Shape");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("eq_kind", tab_idx))
                    .selected_text(b.kind.label())
                    .width(96.0)
                    .show_ui(ui, |ui| {
                        for kind in EqKind::ALL {
                            if ui.selectable_label(b.kind == kind, kind.label()).clicked()
                                && b.kind != kind
                            {
                                b.kind = kind;
                                g.began = true;
                                g.ended = true;
                            }
                        }
                    });
                let mut on = !b.bypassed;
                if ui
                    .checkbox(&mut on, "On")
                    .on_hover_text("Alt+click its handle")
                    .changed()
                {
                    b.bypassed = !on;
                }
            });
            ui.end_row();
            let freq = AutoParam::Eq(EqParam::Freq(slot));
            if auto.is(freq) {
                auto.row(ui, freq, "Frequency");
            } else {
                widgets::drag_row(
                    ui,
                    g,
                    "Frequency",
                    "The band's centre or corner",
                    &mut b.freq_hz,
                    ParametricEqParams::FREQ_HZ,
                    2.0,
                    " Hz",
                    0,
                );
            }
            if b.kind.uses_gain() {
                let gain = AutoParam::Eq(EqParam::Gain(slot));
                if auto.is(gain) {
                    auto.row(ui, gain, "Gain");
                } else {
                    widgets::drag_row(
                        ui,
                        g,
                        "Gain",
                        "Boost or cut",
                        &mut b.gain_db,
                        ParametricEqParams::GAIN_DB,
                        0.1,
                        " dB",
                        1,
                    );
                }
            }
            let q = AutoParam::Eq(EqParam::Q(slot));
            if auto.is(q) {
                auto.row(ui, q, "Q");
            } else if b.kind.uses_slope() {
                widgets::drag_row(
                    ui,
                    g,
                    "Q",
                    "Resonance at the corner: 0.71 is flat, higher peaks before the cut (drag the handle up or down)",
                    &mut b.cut_q,
                    ParametricEqParams::CUT_Q,
                    0.02,
                    "",
                    2,
                );
            } else if b.kind.uses_q() {
                widgets::drag_row(
                    ui,
                    g,
                    "Q",
                    "Width: higher is narrower (scroll on the handle)",
                    &mut b.q,
                    ParametricEqParams::Q,
                    0.02,
                    "",
                    2,
                );
            }
            if b.kind.uses_slope() {
                ui.label("Slope");
                ui.horizontal(|ui| {
                    for slope in CutSlope::ALL {
                        let text = slope.label().trim_end_matches(" dB/oct");
                        if ui
                            .selectable_label(b.slope == slope, text)
                            .on_hover_text(slope.label())
                            .clicked()
                            && b.slope != slope
                        {
                            b.slope = slope;
                            g.began = true;
                            g.ended = true;
                        }
                    }
                });
                ui.end_row();
            }
            ui.label("Channels").on_hover_text(
                "Work on both channels, or only on what they share (Mid) or what differs (Side)",
            );
            ui.add_enabled_ui(stereo, |ui| {
                ui.horizontal(|ui| {
                    for mode in EqChannelMode::ALL {
                        if ui.selectable_label(b.mode == mode, mode.label()).clicked()
                            && b.mode != mode
                        {
                            b.mode = mode;
                            g.began = true;
                            g.ended = true;
                        }
                    }
                });
            });
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.small_button("Delete band").clicked() {
                *b = EqBand::default();
                deleted = true;
                g.began = true;
                g.ended = true;
            }
        });
        if !stereo && b.mode != EqChannelMode::Stereo {
            widgets::note(ui, "Mid and Side need a stereo pair: here a Mid band works on every channel and a Side band does nothing.");
        }
    }
    widgets::settings_grid(ui, ("insp_eq_out", tab_idx), |ui| {
        let output = AutoParam::Eq(EqParam::Output);
        if auto.is(output) {
            auto.row(ui, output, "Output");
        } else {
            widgets::drag_row(
                ui,
                g,
                "Output",
                "Gain after every band",
                &mut params.output_db,
                ParametricEqParams::OUTPUT_DB,
                0.1,
                " dB",
                1,
            );
        }
    });
    if deleted {
        tool_automation::clear_eq_band(tab, selected);
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Show").small().weak());
        let (mut a, mut b, mut c) = (show_pre, show_post, show_live);
        let changed = ui
            .checkbox(&mut a, "Before")
            .on_hover_text("The source's spectrum over the target (grey)")
            .changed()
            | ui.checkbox(&mut b, "After")
                .on_hover_text("The spectrum these settings make (blue)")
                .changed()
            | ui.checkbox(&mut c, "Live")
                .on_hover_text("What is playing (cyan)")
                .changed();
        if changed {
            ui.data_mut(|d| d.insert_temp(show_id, (a, b, c)));
        }
    });
    if !auto.automated.is_empty() {
        widgets::note(ui, "The graph shows the EQ at the playhead.");
    }
    automation_section(app, ui, tab_idx, g, selected);
    preset_menu(app, ui, tab_idx, actions);
}

// ---------------------------------------------------------------------------
// Compressor

pub(super) fn compressor(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let now = ui.input(|i| i.time);
    app.update_tool_live_meters(tab_idx, now);
    let range = target_range(app, tab_idx);
    let levels = source_levels(app, tab_idx);
    let result = rendered_trace(app, tab_idx);
    let auto = AutoView::new(app, tab_idx);
    let tab = &mut app.tabs[tab_idx];
    let live = tab.tool_session.live.clone();
    let id = tab.tab_id;
    let width = ui.available_width();
    // The graphs show the compressor at the playhead; what is dragged there
    // goes back into the settings unless a curve owns it.
    let before = auto.at;
    let mut shown = before;
    ui.horizontal(|ui| {
        let edit = plots::transfer_plot(
            ui,
            egui::Id::new(("comp_transfer", id)),
            Transfer::Compressor(&mut shown.compressor),
            levels.as_ref().or(result.as_ref()),
            live.playing.then_some(live.in_db),
            Vec2::new((width - METERS_W - 6.0).max(120.0), TRANSFER_H),
        );
        note_plot(g, edit);
        plots::meters(
            ui,
            &live,
            None,
            plots::METER_GR_SCALE_DB,
            Vec2::new(METERS_W, TRANSFER_H),
        );
    });
    let threshold = AutoParam::Comp(CompParam::Threshold);
    let lines = [HistoryLine {
        db: shown.compressor.threshold_db,
        color: THRESHOLD_COLOR,
        label: "Threshold",
        draggable: !auto.is(threshold),
        curve: auto.curve(tab, threshold),
    }];
    let (edit, moved) = plots::history_plot(
        ui,
        egui::Id::new(("comp_history", id)),
        &HistoryView {
            range,
            levels: levels.as_ref(),
            result: result.as_ref(),
            lines: &lines,
            playhead: live.source_pos,
            shade_closed: false,
            gain_scale_db: 24.0,
        },
        Vec2::new(width, HISTORY_H),
    );
    note_plot(g, edit);
    seek(actions, &edit);
    if let Some((_, db)) = moved {
        shown.compressor.threshold_db = db.clamp(
            *CompressorSettings::THRESHOLD_DB.start(),
            *CompressorSettings::THRESHOLD_DB.end(),
        );
    }
    if shown != before {
        tool_automation::merge_unautomated(tab, &before, &shown);
    }
    ui.horizontal(|ui| {
        match result.as_ref() {
            Some(t) => ui.label(
                RichText::new(format!("Max reduction {:.1} dB", t.max_reduction_db())).small(),
            ),
            None => ui.label(
                RichText::new("Change a setting to hear and see the reduction")
                    .small()
                    .weak(),
            ),
        };
    });

    let s = &mut tab.tool_state.compressor;
    let p = |c: CompParam| AutoParam::Comp(c);
    widgets::section(ui, "Main");
    widgets::settings_grid(ui, ("insp_comp_main", tab_idx), |ui| {
        if auto.is(threshold) {
            auto.row(ui, threshold, "Threshold");
        } else {
            widgets::drag_row(
                ui,
                g,
                "Threshold",
                "Level above which it compresses",
                &mut s.threshold_db,
                CompressorSettings::THRESHOLD_DB,
                0.2,
                " dB",
                1,
            );
        }
        if auto.is(p(CompParam::Ratio)) {
            auto.row(ui, p(CompParam::Ratio), "Ratio");
        } else {
            widgets::slider_row(
                ui,
                g,
                "Ratio",
                "4:1 turns 4 dB over the threshold into 1 dB",
                &mut s.ratio,
                CompressorSettings::RATIO,
                ":1",
                1,
                true,
            );
        }
        if auto.is(p(CompParam::Knee)) {
            auto.row(ui, p(CompParam::Knee), "Knee");
        } else {
            widgets::drag_row(
                ui,
                g,
                "Knee",
                "How gradually it starts around the threshold (0: at once)",
                &mut s.knee_db,
                CompressorSettings::KNEE_DB,
                0.1,
                " dB",
                1,
            );
        }
        ui.label("Makeup").on_hover_text("Gain after compression");
        ui.horizontal(|ui| {
            if auto.is(p(CompParam::Makeup)) && !s.auto_makeup {
                let at = p(CompParam::Makeup).static_value(&auto.at);
                ui.label(
                    RichText::new(format!("{}  \u{223F} curve", p(CompParam::Makeup).format(at)))
                        .color(widgets::AUTOMATED_COLOR)
                        .monospace(),
                );
            } else {
                ui.add_enabled_ui(!s.auto_makeup, |ui| {
                    let r = ui.add(
                        egui::DragValue::new(&mut s.makeup_db)
                            .range(CompressorSettings::MAKEUP_DB)
                            .speed(0.1)
                            .suffix(" dB")
                            .fixed_decimals(1),
                    );
                    g.track(&r);
                });
            }
            ui.checkbox(&mut s.auto_makeup, "Auto")
                .on_hover_text(format!(
                    "Half the reduction a full-scale signal gets: {:+.1} dB now",
                    s.effective_makeup_db()
                ));
        });
        ui.end_row();
    });
    widgets::section(ui, "Timing");
    widgets::settings_grid(ui, ("insp_comp_time", tab_idx), |ui| {
        if auto.is(p(CompParam::Attack)) {
            auto.row(ui, p(CompParam::Attack), "Attack");
        } else {
            widgets::slider_row(
                ui,
                g,
                "Attack",
                "How fast it reacts above the threshold",
                &mut s.attack_ms,
                CompressorSettings::ATTACK_MS,
                " ms",
                1,
                true,
            );
        }
        if auto.is(p(CompParam::Release)) {
            auto.row(ui, p(CompParam::Release), "Release");
        } else {
            widgets::slider_row(
                ui,
                g,
                "Release",
                "How fast it lets go",
                &mut s.release_ms,
                CompressorSettings::RELEASE_MS,
                " ms",
                0,
                true,
            );
        }
        widgets::drag_row(
            ui,
            g,
            "Lookahead",
            "React this early, so transients are caught",
            &mut s.lookahead_ms,
            CompressorSettings::LOOKAHEAD_MS,
            0.05,
            " ms",
            1,
        );
    });
    widgets::section(ui, "Detector");
    widgets::settings_grid(ui, ("insp_comp_det", tab_idx), |ui| {
        detection_row(ui, g, &mut s.detection);
        if s.detection == Detection::Rms {
            if auto.is(p(CompParam::RmsWindow)) {
                auto.row(ui, p(CompParam::RmsWindow), "RMS window");
            } else {
                widgets::drag_row(
                    ui,
                    g,
                    "RMS window",
                    "How long the loudness is averaged over",
                    &mut s.rms_ms,
                    CompressorSettings::RMS_MS,
                    0.5,
                    " ms",
                    0,
                );
            }
        }
        if auto.is(p(CompParam::Link)) {
            auto.row(ui, p(CompParam::Link), "Stereo link");
        } else {
            widgets::slider_row(
                ui,
                g,
                "Stereo link",
                "100%: one gain for every channel, so the image holds still",
                &mut s.link_pct,
                CompressorSettings::LINK_PCT,
                " %",
                0,
                false,
            );
        }
        if auto.is(p(CompParam::SidechainHpf)) {
            auto.row(ui, p(CompParam::SidechainHpf), "Sidechain HPF");
        } else {
            hz_or_off_row(
                ui,
                g,
                "Sidechain HPF",
                "Keep the bass from driving the compressor",
                &mut s.sidechain_hpf_hz,
                CompressorSettings::SIDECHAIN_HPF_HZ,
            );
        }
    });
    widgets::section(ui, "Output");
    widgets::settings_grid(ui, ("insp_comp_out", tab_idx), |ui| {
        if auto.is(p(CompParam::Mix)) {
            auto.row(ui, p(CompParam::Mix), "Mix");
        } else {
            widgets::slider_row(
                ui,
                g,
                "Mix",
                "Below 100%: blend the dry signal back in (parallel compression)",
                &mut s.mix_pct,
                CompressorSettings::MIX_PCT,
                " %",
                0,
                false,
            );
        }
        if auto.is(p(CompParam::Range)) {
            auto.row(ui, p(CompParam::Range), "Range");
        } else {
            widgets::drag_row(
                ui,
                g,
                "Range",
                "The most it ever reduces",
                &mut s.range_db,
                CompressorSettings::RANGE_DB,
                0.2,
                " dB",
                1,
            );
        }
    });
    automation_section(app, ui, tab_idx, g, 0);
    preset_menu(app, ui, tab_idx, actions);
}

// ---------------------------------------------------------------------------
// Limiter

pub(super) fn limiter(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let now = ui.input(|i| i.time);
    app.update_tool_live_meters(tab_idx, now);
    let range = target_range(app, tab_idx);
    let levels = source_levels(app, tab_idx);
    let result = rendered_trace(app, tab_idx);
    let auto = AutoView::new(app, tab_idx);
    let tab = &mut app.tabs[tab_idx];
    let live = tab.tool_session.live.clone();
    let id = tab.tab_id;
    let width = ui.available_width();
    let before = auto.at;
    let mut shown = before;
    let ceiling = AutoParam::Limiter(LimiterParam::Ceiling);
    let lines = [HistoryLine {
        db: shown.limiter.ceiling_db,
        color: CEILING_COLOR,
        label: "Ceiling",
        draggable: !auto.is(ceiling),
        curve: auto.curve(tab, ceiling),
    }];
    // The GR scale follows the deepest the limiter is driven anywhere.
    let gr_scale = tab.tool_state.limiter.gr_scale_db().max(shown.limiter.gr_scale_db());
    let mut moved = None;
    ui.horizontal(|ui| {
        let (edit, m) = plots::history_plot(
            ui,
            egui::Id::new(("lim_history", id)),
            &HistoryView {
                range,
                levels: levels.as_ref(),
                result: result.as_ref(),
                lines: &lines,
                playhead: live.source_pos,
                shade_closed: false,
                gain_scale_db: gr_scale,
            },
            Vec2::new((width - METERS_W - 6.0).max(120.0), TRANSFER_H),
        );
        note_plot(g, edit);
        seek(actions, &edit);
        moved = m;
        plots::meters(
            ui,
            &live,
            Some(shown.limiter.ceiling_db),
            gr_scale,
            Vec2::new(METERS_W, TRANSFER_H),
        );
    });
    if let Some((_, db)) = moved {
        shown.limiter.ceiling_db = db.clamp(
            *LimiterSettings::CEILING_DB.start(),
            *LimiterSettings::CEILING_DB.end(),
        );
    }
    if shown != before {
        tool_automation::merge_unautomated(tab, &before, &shown);
    }
    let s = &mut tab.tool_state.limiter;
    match result.as_ref() {
        Some(t) => {
            let out_peak = t
                .input_db
                .iter()
                .zip(&t.gain_db)
                .map(|(i, gr)| i + gr + t.output_offset_db)
                .fold(f32::NEG_INFINITY, f32::max)
                .min(shown.limiter.ceiling_db);
            ui.label(
                RichText::new(format!(
                    "Max reduction {:.1} dB · output peak {:.1} dB{}",
                    t.max_reduction_db(),
                    out_peak,
                    if s.true_peak { "TP" } else { "FS" }
                ))
                .small(),
            );
        }
        None => {
            widgets::note(
                ui,
                "Nothing over the ceiling yet: raise Input or lower the Ceiling to hear it work.",
            );
        }
    }
    let p = |l: LimiterParam| AutoParam::Limiter(l);
    widgets::settings_grid(ui, ("insp_limiter", tab_idx), |ui| {
        if auto.is(p(LimiterParam::InputGain)) {
            auto.row(ui, p(LimiterParam::InputGain), "Input");
        } else {
            widgets::drag_row(
                ui,
                g,
                "Input",
                "Gain before the limiter: how hard the audio is pushed into the ceiling",
                &mut s.input_gain_db,
                LimiterSettings::INPUT_GAIN_DB,
                0.1,
                " dB",
                1,
            );
        }
        if auto.is(ceiling) {
            auto.row(ui, ceiling, "Ceiling");
        } else {
            widgets::drag_row(
                ui,
                g,
                "Ceiling",
                "No peak goes above this",
                &mut s.ceiling_db,
                LimiterSettings::CEILING_DB,
                0.05,
                " dB",
                2,
            );
        }
        if auto.is(p(LimiterParam::Release)) {
            auto.row(ui, p(LimiterParam::Release), "Release");
        } else {
            widgets::slider_row(
                ui,
                g,
                "Release",
                "How fast the gain comes back up",
                &mut s.release_ms,
                LimiterSettings::RELEASE_MS,
                " ms",
                0,
                true,
            );
        }
        widgets::drag_row(
            ui,
            g,
            "Lookahead",
            "How early it starts turning down before a peak",
            &mut s.lookahead_ms,
            LimiterSettings::LOOKAHEAD_MS,
            0.05,
            " ms",
            1,
        );
        widgets::check_row(ui, g, &mut s.true_peak, "True peak", "Also keep the peaks between samples under the ceiling (what a D/A converter or an encoder sees)");
        widgets::check_row(
            ui,
            g,
            &mut s.link,
            "Link channels",
            "One gain for every channel, so a peak in one does not move the image",
        );
    });
    automation_section(app, ui, tab_idx, g, 0);
    preset_menu(app, ui, tab_idx, actions);
}

// ---------------------------------------------------------------------------
// Gate

pub(super) fn gate(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let now = ui.input(|i| i.time);
    app.update_tool_live_meters(tab_idx, now);
    let range = target_range(app, tab_idx);
    let levels = source_levels(app, tab_idx);
    let result = rendered_trace(app, tab_idx);
    let tab = &mut app.tabs[tab_idx];
    let live = tab.tool_session.live.clone();
    let id = tab.tab_id;
    let s = &mut tab.tool_state.gate;
    let width = ui.available_width();
    ui.horizontal(|ui| {
        let edit = plots::transfer_plot(
            ui,
            egui::Id::new(("gate_transfer", id)),
            Transfer::Gate(&mut *s),
            levels.as_ref().or(result.as_ref()),
            live.playing.then_some(live.in_db),
            Vec2::new((width - METERS_W - 6.0).max(120.0), TRANSFER_H),
        );
        note_plot(g, edit);
        plots::meters(
            ui,
            &live,
            None,
            plots::METER_GR_SCALE_DB,
            Vec2::new(METERS_W, TRANSFER_H),
        );
    });
    let lines = [
        HistoryLine {
            db: s.threshold_db,
            color: THRESHOLD_COLOR,
            label: "Open",
            draggable: true,
            curve: None,
        },
        HistoryLine {
            db: s.close_db(),
            color: CLOSE_COLOR,
            label: "Close",
            draggable: false,
            curve: None,
        },
    ];
    let (edit, moved) = plots::history_plot(
        ui,
        egui::Id::new(("gate_history", id)),
        &HistoryView {
            range,
            levels: levels.as_ref(),
            result: result.as_ref(),
            lines: &lines,
            playhead: live.source_pos,
            shade_closed: true,
            gain_scale_db: (-s.range_db).max(12.0),
        },
        Vec2::new(width, HISTORY_H),
    );
    note_plot(g, edit);
    seek(actions, &edit);
    if let Some((_, db)) = moved {
        s.threshold_db = db.clamp(
            *GateSettings::THRESHOLD_DB.start(),
            *GateSettings::THRESHOLD_DB.end(),
        );
    }
    widgets::section(ui, "Main");
    widgets::settings_grid(ui, ("insp_gate_main", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Threshold",
            "Opens when the level reaches this",
            &mut s.threshold_db,
            GateSettings::THRESHOLD_DB,
            0.2,
            " dB",
            1,
        );
        widgets::drag_row(
            ui,
            g,
            "Hysteresis",
            "Closes this far below the threshold, so a level hovering there does not chatter",
            &mut s.hysteresis_db,
            GateSettings::HYSTERESIS_DB,
            0.1,
            " dB",
            1,
        );
        ui.label("Range")
            .on_hover_text("How far it turns down when closed");
        let r = ui.add(
            egui::DragValue::new(&mut s.range_db)
                .range(GateSettings::RANGE_DB)
                .speed(0.2)
                .custom_formatter(|v, _| {
                    if v <= f64::from(*GateSettings::RANGE_DB.start()) {
                        "Silence".to_string()
                    } else {
                        format!("{v:.0} dB")
                    }
                }),
        );
        g.track(&r);
        ui.end_row();
    });
    widgets::section(ui, "Timing");
    widgets::settings_grid(ui, ("insp_gate_time", tab_idx), |ui| {
        widgets::slider_row(
            ui,
            g,
            "Attack",
            "How fast it opens",
            &mut s.attack_ms,
            GateSettings::ATTACK_MS,
            " ms",
            2,
            true,
        );
        widgets::drag_row(
            ui,
            g,
            "Hold",
            "How long it stays open after the level drops",
            &mut s.hold_ms,
            GateSettings::HOLD_MS,
            0.5,
            " ms",
            0,
        );
        widgets::slider_row(
            ui,
            g,
            "Release",
            "How fast it closes",
            &mut s.release_ms,
            GateSettings::RELEASE_MS,
            " ms",
            0,
            true,
        );
        widgets::drag_row(
            ui,
            g,
            "Lookahead",
            "Open this early, so an onset is not clipped",
            &mut s.lookahead_ms,
            GateSettings::LOOKAHEAD_MS,
            0.05,
            " ms",
            1,
        );
    });
    widgets::section(ui, "Detector");
    widgets::settings_grid(ui, ("insp_gate_det", tab_idx), |ui| {
        detection_row(ui, g, &mut s.detection);
        if s.detection == Detection::Rms {
            widgets::drag_row(
                ui,
                g,
                "RMS window",
                "How long the level is averaged over",
                &mut s.rms_ms,
                GateSettings::RMS_MS,
                0.5,
                " ms",
                0,
            );
        }
        widgets::slider_row(
            ui,
            g,
            "Stereo link",
            "100%: every channel opens together",
            &mut s.link_pct,
            GateSettings::LINK_PCT,
            " %",
            0,
            false,
        );
        hz_or_off_row(
            ui,
            g,
            "Key HPF",
            "Listen above this only (keep rumble from opening it)",
            &mut s.key_hpf_hz,
            GateSettings::KEY_HPF_HZ,
        );
        hz_or_off_row(
            ui,
            g,
            "Key LPF",
            "Listen below this only (keep hiss from opening it)",
            &mut s.key_lpf_hz,
            GateSettings::KEY_LPF_HZ,
        );
    });
    preset_menu(app, ui, tab_idx, actions);
}
