//! Geometry for the video frame shown in the editor's mini meter strip.
//!
//! Kept free of painting (and of `WavesPreviewer`) for the same reason
//! `mini_meter.rs` is kept free of egui: the width budget is the fiddly part
//! of adding a fifth panel to a strip that is between 52 and 198 px tall, and
//! it is worth being able to test the boundaries directly.
//!
//! The strip reads `[VIDEO][SCOPE][SPECTRUM][STEREO][PEAK]`. The video column
//! is taken off the left first and the existing four-panel arithmetic then
//! runs unchanged on what is left — which is also what gives the strip the
//! right priority for free: `SCOPE` tests the *remaining* width against its
//! own threshold, so it is the panel that disappears when the two no longer
//! fit together.

use egui::{pos2, vec2, Rect};

/// Below this strip width even the video column is dropped: at that size the
/// spectrum is down to a couple of hundred pixels and is the more useful
/// thing to keep.
pub const VIDEO_MIN_STRIP_WIDTH: f32 = 300.0;
/// Most of the strip the video column may claim.
pub const VIDEO_MAX_WIDTH_FRACTION: f32 = 0.32;
/// Narrowest the column may be drawn. A portrait phone clip is much taller
/// than it is wide and would otherwise shrink to a sliver.
pub const VIDEO_MIN_WIDTH: f32 = 56.0;

/// Width of the video column inside a strip of `inner_w` x `inner_h`.
///
/// Returns 0.0 when the strip is too narrow to carry one, which is the signal
/// not to draw or reserve the panel at all.
pub fn video_column_width(inner_w: f32, inner_h: f32, aspect: f32) -> f32 {
    if !inner_w.is_finite() || !inner_h.is_finite() || inner_w < VIDEO_MIN_STRIP_WIDTH {
        return 0.0;
    }
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let natural = inner_h.max(1.0) * aspect;
    let ceiling = (inner_w * VIDEO_MAX_WIDTH_FRACTION).max(VIDEO_MIN_WIDTH);
    natural.clamp(VIDEO_MIN_WIDTH, ceiling)
}

/// The largest rect of the given aspect that fits inside `panel`, centred.
///
/// The frame is letterboxed rather than stretched: a 16:9 picture in a tall
/// narrow column and a 9:16 picture in a wide one both keep their shape.
pub fn letterbox_rect(panel: Rect, aspect: f32) -> Rect {
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let (pw, ph) = (panel.width().max(0.0), panel.height().max(0.0));
    if pw <= 0.0 || ph <= 0.0 {
        return Rect::from_min_size(panel.min, vec2(0.0, 0.0));
    }
    let (w, h) = if pw / ph > aspect {
        (ph * aspect, ph)
    } else {
        (pw, pw / aspect)
    };
    let center = panel.center();
    Rect::from_min_size(pos2(center.x - w * 0.5, center.y - h * 0.5), vec2(w, h))
}

/// The pixel box a decoder should scale frames into for a column of this size.
///
/// Rounded up to a multiple of `QUANTUM` so nudging the window by a pixel does
/// not invalidate every cached frame, and multiplied by the display scale so a
/// HiDPI screen gets a sharp picture rather than an upscaled one.
pub fn frame_box_px(column: Rect, aspect: f32, pixels_per_point: f32) -> (u32, u32) {
    const QUANTUM: f32 = 32.0;
    let fitted = letterbox_rect(column, aspect);
    let scale = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let quantize = |v: f32| -> u32 {
        let px = (v * scale).max(1.0);
        (((px / QUANTUM).ceil()) * QUANTUM) as u32
    };
    (quantize(fitted.width()), quantize(fitted.height()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: f32 = 16.0 / 9.0;
    const TALL: f32 = 9.0 / 16.0;

    /// The four-panel arithmetic `draw_editor_mini_meter` runs on whatever
    /// width is left. Mirrored here so the priority between the two panels can
    /// be asserted without a UI harness.
    fn scope_shows(remaining_w: f32) -> bool {
        remaining_w >= 430.0
    }

    #[test]
    fn a_narrow_strip_carries_no_video_column() {
        assert_eq!(video_column_width(299.0, 198.0, WIDE), 0.0);
        assert_eq!(video_column_width(0.0, 198.0, WIDE), 0.0);
        assert!(video_column_width(300.0, 198.0, WIDE) > 0.0);
    }

    #[test]
    fn the_column_never_takes_more_than_its_share() {
        for w in [300.0f32, 600.0, 900.0, 1400.0, 2400.0] {
            for h in [52.0f32, 100.0, 198.0] {
                for aspect in [WIDE, TALL, 2.39, 1.0] {
                    let col = video_column_width(w, h, aspect);
                    assert!(
                        col <= (w * VIDEO_MAX_WIDTH_FRACTION).max(VIDEO_MIN_WIDTH) + 0.001,
                        "column {col} too wide for strip {w}"
                    );
                    assert!(col >= VIDEO_MIN_WIDTH - 0.001, "column {col} too narrow");
                }
            }
        }
    }

    #[test]
    fn scope_is_the_panel_that_yields_when_both_cannot_fit() {
        // At every strip width, if the scope is showing alongside the video
        // column then the video column is showing too — i.e. the video panel
        // is never the one dropped first.
        const GAP: f32 = 8.0;
        for w in (300..2000).step_by(17).map(|v| v as f32) {
            let col = video_column_width(w, 198.0, WIDE);
            let remaining = if col > 0.0 { w - col - GAP } else { w };
            if scope_shows(remaining) {
                assert!(col > 0.0, "scope kept its slot while video lost one at {w}");
            }
        }
    }

    #[test]
    fn a_tall_strip_gives_a_wide_column_and_a_short_one_a_narrow_column() {
        let tall = video_column_width(1400.0, 198.0, WIDE);
        let short = video_column_width(1400.0, 52.0, WIDE);
        assert!(tall > short, "a taller strip should show a wider frame");
        assert!((tall - 198.0 * WIDE).abs() < 0.5);
        assert!((short - 52.0 * WIDE).abs() < 0.5);
    }

    #[test]
    fn portrait_video_gets_a_narrow_column_but_not_a_sliver() {
        let col = video_column_width(1400.0, 198.0, TALL);
        assert!((col - 198.0 * TALL).abs() < 0.5);
        // At a short strip the natural width falls under the floor.
        assert_eq!(video_column_width(1400.0, 52.0, TALL), VIDEO_MIN_WIDTH);
    }

    #[test]
    fn letterboxing_preserves_aspect_inside_the_panel() {
        let panel = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 100.0));
        let fitted = letterbox_rect(panel, WIDE);
        assert!(panel.contains_rect(fitted));
        assert!((fitted.width() / fitted.height() - WIDE).abs() < 1e-3);
        // Centred in the panel.
        assert!((fitted.center() - panel.center()).length() < 1e-3);

        let tall_fit = letterbox_rect(panel, TALL);
        assert!(panel.contains_rect(tall_fit));
        assert!((tall_fit.width() / tall_fit.height() - TALL).abs() < 1e-3);
        assert!((tall_fit.height() - panel.height()).abs() < 1e-3);
    }

    #[test]
    fn a_degenerate_panel_or_aspect_does_not_panic() {
        let empty = Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 0.0));
        assert_eq!(letterbox_rect(empty, WIDE).width(), 0.0);
        let panel = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 50.0));
        let fitted = letterbox_rect(panel, f32::NAN);
        assert!(fitted.width() > 0.0);
        assert!(video_column_width(f32::NAN, 100.0, WIDE) == 0.0);
    }

    #[test]
    fn the_frame_box_is_quantized_so_a_pixel_of_resize_does_not_reset_the_cache() {
        let a = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 120.0));
        let b = Rect::from_min_size(pos2(0.0, 0.0), vec2(201.0, 120.0));
        assert_eq!(frame_box_px(a, WIDE, 1.0), frame_box_px(b, WIDE, 1.0));
        // And a HiDPI screen asks for more pixels, not the same ones scaled up.
        let one = frame_box_px(a, WIDE, 1.0);
        let two = frame_box_px(a, WIDE, 2.0);
        assert!(two.0 > one.0 && two.1 > one.1);
    }
}
