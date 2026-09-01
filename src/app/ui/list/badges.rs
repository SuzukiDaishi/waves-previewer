use std::borrow::Cow;

use egui::Color32;

use crate::app::WavesPreviewer;

impl WavesPreviewer {
    pub(in crate::app) fn list_type_sort_key(item: &crate::app::types::MediaItem) -> Cow<'_, str> {
        match item.source {
            crate::app::types::MediaSource::Virtual => Cow::Borrowed("vir"),
            crate::app::types::MediaSource::External => Cow::Borrowed("ext"),
            crate::app::types::MediaSource::File => item
                .path
                .extension()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Borrowed("file")),
        }
    }

    pub(super) fn list_type_badge_for_item(
        item: &crate::app::types::MediaItem,
    ) -> (String, String, Color32, Color32) {
        let (label, tooltip, fill, stroke) = match item.source {
            crate::app::types::MediaSource::Virtual => (
                "VIR".to_string(),
                "Virtual audio".to_string(),
                Color32::from_rgb(112, 78, 32),
                Color32::from_rgb(224, 178, 110),
            ),
            crate::app::types::MediaSource::External => (
                "EXT".to_string(),
                "External row".to_string(),
                Color32::from_rgb(68, 78, 98),
                Color32::from_rgb(158, 176, 205),
            ),
            crate::app::types::MediaSource::File => {
                let ext = item
                    .path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default();
                match ext.as_str() {
                    "wav" => (
                        "WAV".to_string(),
                        "WAV file".to_string(),
                        Color32::from_rgb(48, 96, 168),
                        Color32::from_rgb(120, 182, 255),
                    ),
                    "aiff" | "aif" => (
                        "AIFF".to_string(),
                        "AIFF file".to_string(),
                        Color32::from_rgb(88, 70, 150),
                        Color32::from_rgb(176, 156, 244),
                    ),
                    "flac" => (
                        "FLAC".to_string(),
                        "FLAC file".to_string(),
                        Color32::from_rgb(34, 116, 116),
                        Color32::from_rgb(110, 214, 214),
                    ),
                    "mp3" => (
                        "MP3".to_string(),
                        "MP3 file".to_string(),
                        Color32::from_rgb(146, 94, 34),
                        Color32::from_rgb(236, 178, 92),
                    ),
                    "m4a" => (
                        "M4A".to_string(),
                        "M4A file".to_string(),
                        Color32::from_rgb(40, 128, 92),
                        Color32::from_rgb(118, 226, 174),
                    ),
                    "ogg" => (
                        "OGG".to_string(),
                        "OGG file".to_string(),
                        Color32::from_rgb(88, 106, 128),
                        Color32::from_rgb(172, 198, 228),
                    ),
                    // Video containers get a violet family of their own so a
                    // row that only plays its audio track reads as different
                    // at a glance from a plain audio row.
                    "mp4" | "m4v" => (
                        "MP4".to_string(),
                        "MP4 video (audio track only)".to_string(),
                        Color32::from_rgb(112, 62, 148),
                        Color32::from_rgb(206, 156, 246),
                    ),
                    "mov" => (
                        "MOV".to_string(),
                        "QuickTime video (audio track only)".to_string(),
                        Color32::from_rgb(112, 62, 148),
                        Color32::from_rgb(206, 156, 246),
                    ),
                    "3gp" | "3g2" => (
                        "3GP".to_string(),
                        "3GPP video (audio track only)".to_string(),
                        Color32::from_rgb(112, 62, 148),
                        Color32::from_rgb(206, 156, 246),
                    ),
                    _ => {
                        let upper = if ext.is_empty() {
                            "FILE".to_string()
                        } else {
                            ext.to_ascii_uppercase().chars().take(4).collect()
                        };
                        (
                            upper,
                            if ext.is_empty() {
                                "File".to_string()
                            } else {
                                format!(".{ext} file")
                            },
                            Color32::from_rgb(84, 88, 98),
                            Color32::from_rgb(182, 188, 202),
                        )
                    }
                }
            }
        };

        let stroke = if matches!(item.status, crate::app::types::MediaStatus::DecodeFailed(_)) {
            Color32::from_rgb(220, 110, 110)
        } else {
            stroke
        };
        (label, tooltip, fill, stroke)
    }

    pub(super) fn paint_list_type_badge(
        ui: &egui::Ui,
        rect: egui::Rect,
        text_height: f32,
        label: &str,
        fill: Color32,
        stroke: Color32,
    ) {
        Self::paint_list_badge(
            ui,
            rect,
            text_height,
            label,
            fill,
            stroke,
            Color32::WHITE,
            true,
        );
    }

    /// Shared pill used by the Type column and by the Status/Tags columns.
    /// The type badges are fixed-width codes and read better monospaced;
    /// status and tag labels are prose the user typed, so they take the
    /// proportional font and are elided when the column is too narrow.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_list_badge(
        ui: &egui::Ui,
        rect: egui::Rect,
        text_height: f32,
        label: &str,
        fill: Color32,
        stroke: Color32,
        text: Color32,
        monospace: bool,
    ) {
        ui.painter().rect_filled(rect, 5.0, fill);
        ui.painter().rect_stroke(
            rect,
            5.0,
            egui::Stroke::new(1.0, stroke),
            egui::StrokeKind::Outside,
        );
        let size = (text_height * 0.88).max(9.0);
        let fid = if monospace {
            egui::FontId::monospace(size)
        } else {
            egui::FontId::proportional(size)
        };
        let galley =
            ui.painter()
                .layout(label.to_string(), fid, text, (rect.width() - 8.0).max(1.0));
        ui.painter()
            .galley(rect.center() - galley.size() * 0.5, galley, text);
    }

    /// Width one badge needs for `label`, capped so a long label cannot push
    /// the rest of the row out of view.
    pub(super) fn list_badge_width(ui: &egui::Ui, text_height: f32, label: &str) -> f32 {
        let size = (text_height * 0.88).max(9.0);
        let galley = ui.painter().layout_no_wrap(
            label.to_string(),
            egui::FontId::proportional(size),
            Color32::WHITE,
        );
        (galley.size().x + 12.0).clamp(20.0, 160.0)
    }
}
