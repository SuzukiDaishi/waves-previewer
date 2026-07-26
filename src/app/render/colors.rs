use egui::Color32;

use crate::app::types::ThemeMode;

pub const OVERLAY_COLOR: Color32 = Color32::from_rgb(80, 240, 160);
pub const OVERLAY_STROKE_BASE: f32 = 1.3;
pub const OVERLAY_STROKE_EMPH: f32 = 1.6;

/// Theme-aware UI palette for the hand-painted widgets (list selection,
/// status meters, progress bars). The editor's audio canvas deliberately
/// stays dark in both themes (DAW-style); its colors are not here.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// List multi-selection row fill (focused / unfocused list).
    pub selection_fill: Color32,
    pub selection_fill_weak: Color32,
    pub selection_stroke: Color32,
    pub selection_stroke_weak: Color32,
    /// Attention highlight (e.g. rename target row).
    pub attention_fill: Color32,
    pub attention_fill_weak: Color32,
    /// Status/inline warnings and errors.
    pub warning_text: Color32,
    pub error_text: Color32,
    /// "Playing" state accent.
    pub playing_text: Color32,
    /// Volume slider / activity bars.
    pub slider_label: Color32,
    pub slider_label_weak: Color32,
    pub slider_track: Color32,
    pub slider_fill: Color32,
    pub slider_value_text: Color32,
    pub slider_value_text_weak: Color32,
    pub slider_knob_stroke: Color32,
    pub meter_track: Color32,
    pub meter_fill: Color32,
    pub meter_peak_tick: Color32,
    pub meter_text: Color32,
    pub meter_text_outline: Color32,
}

impl Palette {
    pub fn for_theme(theme: ThemeMode) -> Self {
        match theme {
            ThemeMode::Dark => Self {
                selection_fill: Color32::from_rgba_unmultiplied(70, 170, 235, 40),
                selection_fill_weak: Color32::from_rgba_unmultiplied(70, 170, 235, 24),
                selection_stroke: Color32::from_rgba_unmultiplied(110, 205, 255, 180),
                selection_stroke_weak: Color32::from_rgba_unmultiplied(110, 205, 255, 128),
                attention_fill: Color32::from_rgba_unmultiplied(255, 210, 100, 220),
                attention_fill_weak: Color32::from_rgba_unmultiplied(255, 210, 100, 180),
                warning_text: Color32::from_rgb(255, 180, 60),
                error_text: Color32::from_rgb(220, 90, 90),
                playing_text: Color32::from_rgb(120, 220, 140),
                slider_label: Color32::from_rgb(220, 226, 232),
                slider_label_weak: Color32::from_rgb(174, 180, 188),
                slider_track: Color32::from_rgb(24, 27, 31),
                slider_fill: Color32::from_rgb(88, 196, 118),
                slider_value_text: Color32::from_rgb(130, 190, 235),
                slider_value_text_weak: Color32::from_rgb(120, 150, 165),
                slider_knob_stroke: Color32::from_rgb(70, 76, 84),
                meter_track: Color32::from_rgb(18, 18, 22),
                meter_fill: Color32::from_rgb(100, 220, 120),
                meter_peak_tick: Color32::from_rgb(255, 196, 72),
                meter_text: Color32::from_rgb(142, 224, 160),
                meter_text_outline: Color32::from_rgb(38, 52, 42),
            },
            ThemeMode::Light => Self {
                selection_fill: Color32::from_rgba_unmultiplied(30, 120, 200, 46),
                selection_fill_weak: Color32::from_rgba_unmultiplied(30, 120, 200, 26),
                selection_stroke: Color32::from_rgba_unmultiplied(20, 110, 190, 200),
                selection_stroke_weak: Color32::from_rgba_unmultiplied(20, 110, 190, 140),
                attention_fill: Color32::from_rgba_unmultiplied(235, 160, 20, 230),
                attention_fill_weak: Color32::from_rgba_unmultiplied(235, 160, 20, 190),
                warning_text: Color32::from_rgb(178, 108, 0),
                error_text: Color32::from_rgb(190, 40, 40),
                playing_text: Color32::from_rgb(20, 140, 60),
                slider_label: Color32::from_rgb(40, 46, 52),
                slider_label_weak: Color32::from_rgb(96, 104, 112),
                slider_track: Color32::from_rgb(210, 214, 220),
                slider_fill: Color32::from_rgb(52, 160, 88),
                slider_value_text: Color32::from_rgb(30, 110, 180),
                slider_value_text_weak: Color32::from_rgb(110, 130, 148),
                slider_knob_stroke: Color32::from_rgb(150, 156, 164),
                meter_track: Color32::from_rgb(215, 218, 224),
                meter_fill: Color32::from_rgb(52, 170, 92),
                meter_peak_tick: Color32::from_rgb(205, 140, 0),
                meter_text: Color32::from_rgb(22, 110, 54),
                meter_text_outline: Color32::from_rgb(232, 240, 234),
            },
        }
    }
}
