//! Level, time/pitch, dynamics and repair settings: tools whose whole
//! state is a handful of numbers.

use crate::app::inspector_ops::{InspectorAction, ToolCommand};
use crate::app::types::ToolKind;
use crate::app::ui::inspector::widgets::{self, Gestures};
use crate::app::WavesPreviewer;

const CURVE_HINT: &str = "Double-click empty space on the waveform to add a point, drag a point to move it, double-click a point to remove it.";

pub(super) fn gain(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize, g: &mut Gestures) {
    let tab = &mut app.tabs[tab_idx];
    widgets::settings_grid(ui, ("insp_gain", tab_idx), |ui| {
        let curve = tab.gain_env_enabled;
        widgets::drag_row(
            ui,
            g,
            if curve { "Outside points" } else { "Gain" },
            "Raise (+) or lower (-) the level",
            &mut tab.tool_state.gain_db,
            -24.0..=24.0,
            0.1,
            " dB",
            1,
        );
        widgets::check_row(
            ui,
            g,
            &mut tab.gain_env_enabled,
            "Gain curve (draw on waveform)",
            "Shape the gain over time with points on the waveform",
        );
    });
    if tab.gain_env_enabled {
        widgets::note(ui, CURVE_HINT);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{} point(s)", tab.gain_env_points.len())).weak());
            if ui
                .add_enabled(
                    !tab.gain_env_points.is_empty(),
                    egui::Button::new("Clear points"),
                )
                .clicked()
            {
                tab.gain_env_points.clear();
                tab.gain_env_drag = None;
            }
        });
    }
}

pub(super) fn normalize(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
) {
    let tab = &mut app.tabs[tab_idx];
    widgets::settings_grid(ui, ("insp_normalize", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Peak target",
            "The loudest sample of the target ends up at this level",
            &mut tab.tool_state.normalize_target_db,
            -24.0..=0.0,
            0.1,
            " dBFS",
            1,
        );
    });
}

pub(super) fn loudness(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
) {
    let tab = &mut app.tabs[tab_idx];
    widgets::settings_grid(ui, ("insp_loudness", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Target",
            "Integrated loudness (ITU-R BS.1770) of the whole file after the change",
            &mut tab.tool_state.loudness_target_lufs,
            -36.0..=0.0,
            0.1,
            " LUFS",
            1,
        );
    });
}

pub(super) fn invert(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
) {
    let tab = &mut app.tabs[tab_idx];
    widgets::settings_grid(ui, ("insp_invert", tab_idx), |ui| {
        widgets::check_row(
            ui,
            g,
            &mut tab.tool_state.invert_smooth_boundaries,
            "Smooth boundaries (~2 ms)",
            "Ramp the flip where a partial range meets untouched audio, so it does not click",
        );
    });
}

pub(super) fn dc_offset(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize) {
    let tab = &app.tabs[tab_idx];
    widgets::note(
        ui,
        "Subtracts each channel's average over the target, so the waveform centres on zero.",
    );
    if tab.loading {
        return;
    }
    // Measured on a stride: an exact mean of a long clip on every frame
    // would be a pass over the whole buffer per repaint.
    let stride = (tab.samples_len / 200_000).max(1);
    let means: Vec<String> = tab
        .ch_samples
        .iter()
        .take(8)
        .map(|ch| {
            let (sum, n) = ch
                .iter()
                .step_by(stride)
                .fold((0.0f64, 0usize), |(s, n), v| (s + f64::from(*v), n + 1));
            let mean = if n == 0 { 0.0 } else { sum / n as f64 };
            format!("{:+.4}", mean)
        })
        .collect();
    if !means.is_empty() {
        widgets::note(
            ui,
            &format!("Current offset per channel: {}", means.join("  ")),
        );
    }
}

pub(super) fn reverse(ui: &mut egui::Ui) {
    widgets::note(
        ui,
        "No settings. A partial range is joined with short crossfades so its edges do not click.",
    );
}

pub(super) fn pitch(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize, g: &mut Gestures) {
    let tab = &mut app.tabs[tab_idx];
    widgets::settings_grid(ui, ("insp_pitch", tab_idx), |ui| {
        let curve = tab.pitch_env_enabled;
        widgets::drag_row(
            ui,
            g,
            if curve { "Outside points" } else { "Semitones" },
            "Up (+) or down (-), length unchanged",
            &mut tab.tool_state.pitch_semitones,
            -12.0..=12.0,
            0.05,
            " st",
            2,
        );
        widgets::check_row(
            ui,
            g,
            &mut tab.pitch_env_enabled,
            "Pitch curve (draw on waveform)",
            "Bend the pitch over time with points on the waveform",
        );
    });
    if tab.pitch_env_enabled {
        widgets::note(ui, CURVE_HINT);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} point(s)", tab.pitch_env_points.len())).weak(),
            );
            if ui
                .add_enabled(
                    !tab.pitch_env_points.is_empty(),
                    egui::Button::new("Clear points"),
                )
                .clicked()
            {
                tab.pitch_env_points.clear();
                tab.pitch_env_drag = None;
            }
        });
    } else {
        widgets::note(ui, "Or drag the horizontal pitch line on the waveform.");
    }
}

pub(super) fn rate(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    tool: ToolKind,
) {
    let tab = &mut app.tabs[tab_idx];
    let value = if tool == ToolKind::Speed {
        &mut tab.tool_state.speed_rate
    } else {
        &mut tab.tool_state.stretch_rate
    };
    widgets::settings_grid(ui, ("insp_rate", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Rate",
            "2.00 plays twice as fast (half the length); 0.50 half as fast",
            value,
            0.25..=4.0,
            0.01,
            "×",
            2,
        );
    });
    if tool == ToolKind::TimeStretch {
        widgets::note(
            ui,
            "Or drag the grip at the top of a selection edge on the waveform.",
        );
    } else {
        widgets::note(ui, "Like tape: the pitch moves with the speed.");
    }
}

pub(super) fn eq(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize, g: &mut Gestures) {
    use crate::wave::ThreeBandEqParams as P;
    let tab = &mut app.tabs[tab_idx];
    let st = &mut tab.tool_state;
    let mut params = P {
        low_shelf_freq_hz: st.eq_low_shelf_freq_hz,
        low_shelf_gain_db: st.eq_low_shelf_gain_db,
        mid_freq_hz: st.eq_mid_freq_hz,
        mid_gain_db: st.eq_mid_gain_db,
        mid_q: st.eq_mid_q,
        high_shelf_freq_hz: st.eq_high_shelf_freq_hz,
        high_shelf_gain_db: st.eq_high_shelf_gain_db,
    };
    if crate::app::ui::dsp_widgets::eq_response_plot(
        ui,
        egui::Id::new(("eq_plot", tab_idx)),
        &mut params,
        tab.buffer_sample_rate.max(1),
        true,
    ) {
        g.began |= ui.input(|i| i.pointer.primary_down());
    }
    widgets::settings_grid(ui, ("insp_eq", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Low shelf",
            "Corner of the low shelf",
            &mut params.low_shelf_freq_hz,
            P::LOW_SHELF_HZ,
            1.0,
            " Hz",
            0,
        );
        widgets::drag_row(
            ui,
            g,
            "  gain",
            "Boost or cut below the corner",
            &mut params.low_shelf_gain_db,
            P::GAIN_DB,
            0.1,
            " dB",
            1,
        );
        widgets::drag_row(
            ui,
            g,
            "Mid",
            "Centre of the mid band",
            &mut params.mid_freq_hz,
            P::MID_HZ,
            5.0,
            " Hz",
            0,
        );
        widgets::drag_row(
            ui,
            g,
            "  gain",
            "Boost or cut around the centre",
            &mut params.mid_gain_db,
            P::GAIN_DB,
            0.1,
            " dB",
            1,
        );
        widgets::drag_row(
            ui,
            g,
            "  Q",
            "Width of the mid band: higher is narrower",
            &mut params.mid_q,
            P::MID_Q,
            0.05,
            "",
            2,
        );
        widgets::drag_row(
            ui,
            g,
            "High shelf",
            "Corner of the high shelf",
            &mut params.high_shelf_freq_hz,
            P::HIGH_SHELF_HZ,
            10.0,
            " Hz",
            0,
        );
        widgets::drag_row(
            ui,
            g,
            "  gain",
            "Boost or cut above the corner",
            &mut params.high_shelf_gain_db,
            P::GAIN_DB,
            0.1,
            " dB",
            1,
        );
    });
    st.eq_low_shelf_freq_hz = params.low_shelf_freq_hz;
    st.eq_low_shelf_gain_db = params.low_shelf_gain_db;
    st.eq_mid_freq_hz = params.mid_freq_hz;
    st.eq_mid_gain_db = params.mid_gain_db;
    st.eq_mid_q = params.mid_q;
    st.eq_high_shelf_freq_hz = params.high_shelf_freq_hz;
    st.eq_high_shelf_gain_db = params.high_shelf_gain_db;
}

pub(super) fn compressor(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
) {
    use crate::wave::{CompressorParams as P, NoiseGateParams as G};
    let st = &mut app.tabs[tab_idx].tool_state;
    let mut params = P {
        threshold_db: st.compressor_threshold_db,
        ratio: st.compressor_ratio,
        attack_ms: st.compressor_attack_ms,
        release_ms: st.compressor_release_ms,
        makeup_db: st.compressor_makeup_db,
    };
    if crate::app::ui::dsp_widgets::compressor_transfer_plot(
        ui,
        egui::Id::new(("comp_plot", tab_idx)),
        &mut params,
    ) {
        g.began |= ui.input(|i| i.pointer.primary_down());
    }
    widgets::settings_grid(ui, ("insp_comp", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Threshold",
            "Signal above this level is compressed",
            &mut params.threshold_db,
            P::THRESHOLD_DB,
            0.5,
            " dB",
            1,
        );
        widgets::drag_row(
            ui,
            g,
            "Ratio",
            "4:1 turns 4 dB over the threshold into 1 dB",
            &mut params.ratio,
            P::RATIO,
            0.1,
            ":1",
            1,
        );
        widgets::drag_row(
            ui,
            g,
            "Attack",
            "How fast it reacts above the threshold",
            &mut params.attack_ms,
            G::ATTACK_MS,
            0.1,
            " ms",
            1,
        );
        widgets::drag_row(
            ui,
            g,
            "Release",
            "How fast it lets go below the threshold",
            &mut params.release_ms,
            G::RELEASE_MS,
            1.0,
            " ms",
            0,
        );
        widgets::drag_row(
            ui,
            g,
            "Makeup",
            "Gain after compression to restore the level",
            &mut params.makeup_db,
            P::MAKEUP_DB,
            0.1,
            " dB",
            1,
        );
    });
    st.compressor_threshold_db = params.threshold_db;
    st.compressor_ratio = params.ratio;
    st.compressor_attack_ms = params.attack_ms;
    st.compressor_release_ms = params.release_ms;
    st.compressor_makeup_db = params.makeup_db;
}

pub(super) fn noise_gate(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
) {
    use crate::wave::NoiseGateParams as P;
    let st = &mut app.tabs[tab_idx].tool_state;
    let mut params = P {
        threshold_db: st.noise_gate_threshold_db,
        attack_ms: st.noise_gate_attack_ms,
        release_ms: st.noise_gate_release_ms,
    };
    if crate::app::ui::dsp_widgets::noise_gate_plot(
        ui,
        egui::Id::new(("gate_plot", tab_idx)),
        &mut params,
    ) {
        g.began |= ui.input(|i| i.pointer.primary_down());
    }
    widgets::settings_grid(ui, ("insp_gate", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Threshold",
            "Signal below this level is faded toward silence",
            &mut params.threshold_db,
            P::THRESHOLD_DB,
            0.5,
            " dB",
            1,
        );
        widgets::drag_row(
            ui,
            g,
            "Attack",
            "How fast the gate opens",
            &mut params.attack_ms,
            P::ATTACK_MS,
            0.1,
            " ms",
            1,
        );
        widgets::drag_row(
            ui,
            g,
            "Release",
            "How fast the gate closes",
            &mut params.release_ms,
            P::RELEASE_MS,
            1.0,
            " ms",
            0,
        );
    });
    st.noise_gate_threshold_db = params.threshold_db;
    st.noise_gate_attack_ms = params.attack_ms;
    st.noise_gate_release_ms = params.release_ms;
}

pub(super) fn declick(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
    tool: ToolKind,
) {
    let tab = &mut app.tabs[tab_idx];
    let declip = tool == ToolKind::DeClip;
    let changed = widgets::settings_grid(ui, ("insp_declick", tab_idx), |ui| {
        let value = if declip {
            &mut tab.tool_state.declip_sensitivity
        } else {
            &mut tab.tool_state.declick_sensitivity
        };
        widgets::slider_row(
            ui,
            g,
            "Sensitivity",
            if declip {
                "Higher catches rails further below the peak (0 = only hard full-scale clipping)"
            } else {
                "Higher finds smaller clicks"
            },
            value,
            0.0..=1.0,
            "",
            2,
            false,
        )
    });
    if changed {
        // A scan answers for one sensitivity.
        tab.declick_scan = None;
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(!tab.loading, egui::Button::new("Scan"))
            .on_hover_text("Mark what would be repaired in red on the waveform")
            .clicked()
        {
            actions.push(InspectorAction::Tool(if declip {
                ToolCommand::DeclipScan
            } else {
                ToolCommand::DeclickScan
            }));
        }
        if let Some(scan) = &tab.declick_scan {
            let noun = if declip { "clipped run(s)" } else { "click(s)" };
            ui.label(egui::RichText::new(format!("{} {noun} found", scan.spans.len())).weak());
            if ui.small_button("Clear").clicked() {
                actions.push(InspectorAction::Tool(ToolCommand::ClearScan));
            }
        }
    });
    if declip {
        widgets::note(
            ui,
            "The rebuilt crests may go past the rail; lower the gain afterwards if needed.",
        );
    }
}

pub(super) fn dehum(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let tab = &mut app.tabs[tab_idx];
    let st = &mut tab.tool_state;
    widgets::settings_grid(ui, ("insp_dehum", tab_idx), |ui| {
        ui.label("Fundamental")
            .on_hover_text("The hum's base frequency (usually 50 or 60 Hz)");
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::DragValue::new(&mut st.dehum_hz)
                    .range(20.0..=400.0)
                    .speed(0.1)
                    .suffix(" Hz")
                    .fixed_decimals(1),
            );
            g.track(&r);
            if ui
                .add_enabled(!tab.loading, egui::Button::new("Detect"))
                .on_hover_text(
                    "Find the hum line between 45 and 65 Hz (in the selection, when there is one)",
                )
                .clicked()
            {
                actions.push(InspectorAction::Tool(ToolCommand::DehumDetect));
            }
        });
        ui.end_row();
        ui.label("Harmonics")
            .on_hover_text("How many multiples of the fundamental to notch");
        let mut harmonics = st.dehum_harmonics.clamp(1, 16);
        let r = ui.add(egui::Slider::new(&mut harmonics, 1..=16));
        if g.track(&r) {
            st.dehum_harmonics = harmonics;
        }
        ui.end_row();
        widgets::slider_row(
            ui,
            g,
            "Width (Q)",
            "Higher Q: narrower notches",
            &mut st.dehum_q,
            5.0..=100.0,
            "",
            0,
            true,
        );
        widgets::slider_row(
            ui,
            g,
            "Depth",
            "How deep each notch cuts",
            &mut st.dehum_depth_db,
            3.0..=80.0,
            " dB",
            0,
            false,
        );
    });
}

pub(super) fn denoise(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    actions: &mut Vec<InspectorAction>,
) {
    let ready = WavesPreviewer::editor_denoise_ready(&app.tabs[tab_idx]);
    let tab = &mut app.tabs[tab_idx];
    match &tab.noise_profile {
        Some(p) if ready => widgets::note(
            ui,
            &format!(
                "Profile learned from {:.2}–{:.2} s",
                p.learned_from_ms.0 / 1000.0,
                p.learned_from_ms.1 / 1000.0
            ),
        ),
        Some(_) => widgets::warn(
            ui,
            "The profile is from another sample rate — learn it again",
        ),
        None => widgets::warn(ui, "Select a stretch of noise only, then Learn"),
    }
    let has_selection = WavesPreviewer::editor_selected_range(tab).is_some();
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                has_selection && !tab.loading,
                egui::Button::new("Learn from Selection"),
            )
            .clicked()
        {
            actions.push(InspectorAction::Tool(ToolCommand::DenoiseLearn));
        }
        if tab.noise_profile.is_some() && ui.button("Forget").clicked() {
            actions.push(InspectorAction::Tool(ToolCommand::DenoiseForget));
        }
    });
    widgets::settings_grid(ui, ("insp_denoise", tab_idx), |ui| {
        widgets::slider_row(
            ui,
            g,
            "Reduction",
            "The most a noisy bin is turned down",
            &mut tab.tool_state.denoise_reduction_db,
            0.0..=40.0,
            " dB",
            0,
            false,
        );
        widgets::slider_row(
            ui,
            g,
            "Strength",
            "Over-subtraction: higher removes more, with more artefacts",
            &mut tab.tool_state.denoise_strength,
            1.0..=4.0,
            "",
            1,
            false,
        );
    });
    if has_selection {
        widgets::note(ui, "Learn uses the selection; Apply then works on the selection too (edges crossfaded). Clear the selection to reduce the whole file.");
    }
}
