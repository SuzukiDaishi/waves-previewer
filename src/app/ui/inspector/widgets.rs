//! The Inspector's building blocks: one look for every tool's settings, and
//! the gesture bookkeeping the draft history needs.

use std::ops::RangeInclusive;

use egui::{Color32, RichText};

/// Which gestures on settings began or ended this frame. One gesture (a
/// drag, a focused text entry) becomes one draft step; see
/// `tool_session::ToolSession::begin_gesture`.
#[derive(Default, Debug, Clone, Copy)]
pub(in crate::app) struct Gestures {
    pub began: bool,
    pub ended: bool,
}

impl Gestures {
    /// Note what a settings widget did. Returns whether its value changed.
    pub fn track(&mut self, response: &egui::Response) -> bool {
        if response.drag_started() || response.gained_focus() {
            self.began = true;
        }
        if response.drag_stopped() || response.lost_focus() {
            self.ended = true;
        }
        response.changed()
    }
}

/// A two-column grid: labels on the left, controls on the right.
pub(in crate::app) fn settings_grid<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Grid::new(id_salt)
        .num_columns(2)
        .spacing(egui::vec2(10.0, 6.0))
        .min_col_width(72.0)
        .show(ui, add)
        .inner
}

/// A labelled number in the settings grid.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn drag_row(
    ui: &mut egui::Ui,
    g: &mut Gestures,
    label: &str,
    hover: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    speed: f64,
    suffix: &str,
    decimals: usize,
) -> bool {
    if !value.is_finite() {
        *value = *range.start();
    }
    ui.label(label).on_hover_text(hover);
    let response = ui
        .add(
            egui::DragValue::new(value)
                .range(range)
                .speed(speed)
                .suffix(suffix)
                .fixed_decimals(decimals),
        )
        .on_hover_text(hover);
    ui.end_row();
    g.track(&response)
}

/// A labelled slider in the settings grid.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn slider_row(
    ui: &mut egui::Ui,
    g: &mut Gestures,
    label: &str,
    hover: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    suffix: &str,
    decimals: usize,
    logarithmic: bool,
) -> bool {
    if !value.is_finite() {
        *value = *range.start();
    }
    ui.label(label).on_hover_text(hover);
    let response = ui
        .add(
            egui::Slider::new(value, range)
                .suffix(suffix)
                .fixed_decimals(decimals)
                .logarithmic(logarithmic),
        )
        .on_hover_text(hover);
    ui.end_row();
    g.track(&response)
}

/// A checkbox spanning the grid row.
pub(in crate::app) fn check_row(
    ui: &mut egui::Ui,
    g: &mut Gestures,
    value: &mut bool,
    label: &str,
    hover: &str,
) -> bool {
    ui.label("");
    let response = ui.checkbox(value, label).on_hover_text(hover);
    ui.end_row();
    g.track(&response)
}

/// The colour a setting that follows its curve is shown in.
pub(in crate::app) const AUTOMATED_COLOR: Color32 = Color32::from_rgb(120, 200, 255);

/// A setting that follows its curve on the waveform: its value at the
/// playhead, read-only, in place of its control.
pub(in crate::app) fn automated_row(ui: &mut egui::Ui, label: &str, value: String) {
    let hover = "Follows its curve on the waveform (see Automation below). Switch the curve off to set it here.";
    ui.label(label).on_hover_text(hover);
    ui.label(
        RichText::new(format!("{value}  \u{223F} curve"))
            .color(AUTOMATED_COLOR)
            .monospace(),
    )
    .on_hover_text(hover);
    ui.end_row();
}

/// A small heading between groups of settings.
pub(in crate::app) fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(3.0, 14.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 1.0, Color32::from_rgb(70, 150, 235));
        ui.label(RichText::new(title).strong());
    });
}

/// A quiet line of explanation.
pub(in crate::app) fn note(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).weak().small());
}

/// A warning line.
pub(in crate::app) fn warn(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .small()
            .color(Color32::from_rgb(235, 190, 95)),
    );
}
