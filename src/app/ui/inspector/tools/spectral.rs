//! Spectral Warp and Spectral Brush, edits painted on the spectrogram, and
//! Spectral Mute and Heal, edits to a time selection or one band of it.

use crate::app::spectral_ops::HEAL_MAX_SELECTION_SECS;
use crate::app::types::{ToolKind, ToolState};
use crate::app::ui::editor::{WARP_FREQ_RADIUS_HZ, WARP_TIME_RADIUS_MS};
use crate::app::ui::inspector::widgets::{self, Gestures};
use crate::app::WavesPreviewer;

pub(super) fn warp(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize, g: &mut Gestures) {
    let tab = &mut app.tabs[tab_idx];
    let r = ui
        .checkbox(&mut tab.spectral_warp_edit, "Edit on the spectrogram")
        .on_hover_text(
            "While on, dragging on the spectrogram pushes frequencies instead of selecting",
        );
    if r.changed() {
        tab.spectral_warp_drag = None;
        if tab.spectral_warp_edit {
            // Warp and brush gestures both own the pointer: one at a time.
            tab.spectral_brush_edit = false;
            tab.spectral_brush_last = None;
        }
    }
    let defaults = ToolState::default_values();
    let st = &mut tab.tool_state;
    if !st.warp_time_radius_ms.is_finite() || st.warp_time_radius_ms <= 0.0 {
        st.warp_time_radius_ms = defaults.warp_time_radius_ms;
    }
    if !st.warp_freq_radius_hz.is_finite() || st.warp_freq_radius_hz <= 0.0 {
        st.warp_freq_radius_hz = defaults.warp_freq_radius_hz;
    }
    widgets::settings_grid(ui, ("insp_warp", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Time radius",
            "How far in time each stroke reaches (Gaussian)",
            &mut st.warp_time_radius_ms,
            WARP_TIME_RADIUS_MS,
            5.0,
            " ms",
            0,
        );
        widgets::drag_row(
            ui,
            g,
            "Freq radius",
            "How far in frequency each stroke reaches (Gaussian)",
            &mut st.warp_freq_radius_hz,
            WARP_FREQ_RADIUS_HZ,
            10.0,
            " Hz",
            0,
        );
    });
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{} stroke(s)", tab.spectral_warp_points.len())).weak(),
        );
        if !tab.spectral_warp_points.is_empty() && ui.button("Clear").clicked() {
            tab.spectral_warp_points.clear();
            tab.spectral_warp_drag = None;
        }
    });
    widgets::note(
        ui,
        "Turn on \"Waveform overlay\" above to see the result in green.",
    );
}

pub(super) fn brush(app: &mut WavesPreviewer, ui: &mut egui::Ui, tab_idx: usize, g: &mut Gestures) {
    let tab = &mut app.tabs[tab_idx];
    let r = ui
        .checkbox(&mut tab.spectral_brush_edit, "Paint on the spectrogram")
        .on_hover_text(
            "While on, dragging on the spectrogram paints attenuation instead of selecting",
        );
    if r.changed() {
        tab.spectral_brush_last = None;
        if tab.spectral_brush_edit {
            tab.spectral_warp_edit = false;
            tab.spectral_warp_drag = None;
        }
    }
    let st = &mut tab.tool_state;
    widgets::settings_grid(ui, ("insp_brush", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Strength",
            "How much each stamp cuts at its centre (taken when painted)",
            &mut st.brush_cut_db,
            3.0..=80.0,
            1.0,
            " dB",
            0,
        );
        widgets::drag_row(
            ui,
            g,
            "Time radius",
            "How far in time each stamp reaches (taken when painted)",
            &mut st.brush_time_radius_ms,
            5.0..=1000.0,
            5.0,
            " ms",
            0,
        );
        widgets::drag_row(
            ui,
            g,
            "Freq radius",
            "How far in frequency each stamp reaches (taken when painted)",
            &mut st.brush_freq_radius_hz,
            20.0..=4000.0,
            10.0,
            " Hz",
            0,
        );
    });
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{} stamp(s)", tab.spectral_brush_stamps.len())).weak(),
        );
        if !tab.spectral_brush_stamps.is_empty() && ui.button("Clear").clicked() {
            tab.spectral_brush_stamps.clear();
            tab.spectral_brush_last = None;
        }
    });
    widgets::note(
        ui,
        "Strokes stack (80 dB at most). Turn on \"Waveform overlay\" to see the result in green.",
    );
}

pub(super) fn mute_or_heal(
    app: &mut WavesPreviewer,
    ui: &mut egui::Ui,
    tab_idx: usize,
    g: &mut Gestures,
    tool: ToolKind,
) {
    let tab = &mut app.tabs[tab_idx];
    let heal = tool == ToolKind::Heal;
    let sr = tab.buffer_sample_rate.max(1) as f32;
    let selection = WavesPreviewer::editor_selected_range(tab);
    match (selection, tab.freq_selection) {
        (None, _) => widgets::warn(ui, "Select a time range first"),
        (Some(_), Some((lo, hi))) => widgets::note(
            ui,
            &format!(
                "Band {lo:.0}–{hi:.0} Hz: only this band is {}.",
                if heal { "rebuilt" } else { "removed" }
            ),
        ),
        (Some(_), None) => widgets::note(
            ui,
            if heal {
                "No band: every frequency is rebuilt. In a spectrogram view, drag up or down as well to pick one."
            } else {
                "No band: every frequency is muted. In a spectrogram view, drag up or down as well to pick one."
            },
        ),
    }
    if let (true, Some((s, e))) = (heal, selection) {
        if (e - s) as f32 / sr > HEAL_MAX_SELECTION_SECS {
            widgets::warn(
                ui,
                &format!(
                    "Heal works on at most {HEAL_MAX_SELECTION_SECS:.0} s at a time: select less"
                ),
            );
        }
    }
    let st = &mut tab.tool_state;
    widgets::settings_grid(ui, ("insp_mute_heal", tab_idx), |ui| {
        widgets::drag_row(
            ui,
            g,
            "Time fade",
            "How long the edit fades in and out at the selection's edges",
            &mut st.spectral_time_fade_ms,
            0.0..=200.0,
            0.5,
            " ms",
            1,
        );
        widgets::drag_row(
            ui,
            g,
            "Band edge",
            "How wide the transition is at the band's edges (with a band selected)",
            &mut st.spectral_freq_fade_hz,
            0.0..=2000.0,
            5.0,
            " Hz",
            0,
        );
    });
    widgets::note(
        ui,
        "The fades are shared with Play Selection and spectral copy / paste.",
    );
}
