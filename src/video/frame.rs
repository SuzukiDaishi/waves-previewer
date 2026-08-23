//! A decoded video frame on its way to the editor's meter strip.
//!
//! Frames are downscaled and rotated **here, on the decode worker**, before a
//! single byte reaches the UI thread. The panel they land in is at most a few
//! hundred pixels wide, so handing the UI a 4K RGBA buffer (33 MB) to shrink
//! per repaint would be the one thing guaranteed to stall the frame loop.

use std::sync::Arc;

/// Clockwise display rotation from the track's transform matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rotation {
    #[default]
    None,
    Cw90,
    Cw180,
    Cw270,
}

impl Rotation {
    /// Rotation carried by an ISO-BMFF `tkhd` transformation matrix.
    ///
    /// The matrix is 3x3 in 16.16 fixed point, of which only `a`, `b`, `c` and
    /// `d` matter for the four right-angle cases a camera actually writes.
    /// Anything else (a shear, a flip, an arbitrary affine) is rare enough that
    /// treating it as unrotated beats guessing.
    pub fn from_matrix(a: i32, b: i32, c: i32, d: i32) -> Self {
        const ONE: i32 = 0x0001_0000;
        let sign = |v: i32| -> i32 {
            if v > ONE / 2 {
                1
            } else if v < -ONE / 2 {
                -1
            } else {
                0
            }
        };
        match (sign(a), sign(b), sign(c), sign(d)) {
            (1, 0, 0, 1) => Rotation::None,
            (0, 1, -1, 0) => Rotation::Cw90,
            (-1, 0, 0, -1) => Rotation::Cw180,
            (0, -1, 1, 0) => Rotation::Cw270,
            _ => Rotation::None,
        }
    }

    pub fn degrees(self) -> u16 {
        match self {
            Rotation::None => 0,
            Rotation::Cw90 => 90,
            Rotation::Cw180 => 180,
            Rotation::Cw270 => 270,
        }
    }

    /// Whether this rotation swaps width and height.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Cw90 | Rotation::Cw270)
    }

    /// The stored dimensions as they should be displayed.
    pub fn apply_to_size(self, w: u32, h: u32) -> (u32, u32) {
        if self.swaps_axes() {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// One decoded frame, already display-oriented and panel-sized.
#[derive(Clone)]
pub struct VideoFrame {
    /// Presentation time in seconds from the start of the movie.
    pub pts_secs: f64,
    pub image: Arc<egui::ColorImage>,
}

/// Fit `(src_w, src_h)` inside `(box_w, box_h)` without distorting it, never
/// scaling up. Returns at least 1x1 so the result is always a drawable image.
pub fn fit_within(src_w: u32, src_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 || box_w == 0 || box_h == 0 {
        return (1, 1);
    }
    if src_w <= box_w && src_h <= box_h {
        return (src_w, src_h);
    }
    let scale = (box_w as f64 / src_w as f64).min(box_h as f64 / src_h as f64);
    let w = ((src_w as f64 * scale).round() as u32).max(1);
    let h = ((src_h as f64 * scale).round() as u32).max(1);
    (w, h)
}

/// Rotate and box-filter an RGBA buffer into a `ColorImage` of at most
/// `box_w` x `box_h`.
///
/// A box filter (average of the source pixels each destination pixel covers)
/// rather than anything fancier: at these reduction ratios it is visually
/// indistinguishable from Lanczos and several times cheaper, and this runs
/// once per displayed frame during playback.
pub fn rgba_to_color_image(
    rgba: &[u8],
    src_w: u32,
    src_h: u32,
    rotation: Rotation,
    box_w: u32,
    box_h: u32,
) -> Option<egui::ColorImage> {
    if src_w == 0 || src_h == 0 {
        return None;
    }
    let expected = (src_w as usize)
        .checked_mul(src_h as usize)?
        .checked_mul(4)?;
    if rgba.len() < expected {
        return None;
    }
    let (disp_w, disp_h) = rotation.apply_to_size(src_w, src_h);
    let (dst_w, dst_h) = fit_within(disp_w, disp_h, box_w, box_h);

    let mut pixels = Vec::with_capacity((dst_w as usize) * (dst_h as usize));
    let sx_step = disp_w as f64 / dst_w as f64;
    let sy_step = disp_h as f64 / dst_h as f64;
    for dy in 0..dst_h {
        let y0 = (dy as f64 * sy_step) as u32;
        let y1 = (((dy + 1) as f64 * sy_step).ceil() as u32)
            .min(disp_h)
            .max(y0 + 1);
        for dx in 0..dst_w {
            let x0 = (dx as f64 * sx_step) as u32;
            let x1 = (((dx + 1) as f64 * sx_step).ceil() as u32)
                .min(disp_w)
                .max(x0 + 1);
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            let mut n = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    // Map the display-space pixel back to storage space; the
                    // rotation is applied by this index remap alone, so no
                    // intermediate rotated buffer is ever allocated.
                    let (sx, sy) = match rotation {
                        Rotation::None => (x, y),
                        Rotation::Cw90 => (y, disp_w.saturating_sub(1).saturating_sub(x)),
                        Rotation::Cw180 => (
                            disp_w.saturating_sub(1).saturating_sub(x),
                            disp_h.saturating_sub(1).saturating_sub(y),
                        ),
                        Rotation::Cw270 => (disp_h.saturating_sub(1).saturating_sub(y), x),
                    };
                    if sx >= src_w || sy >= src_h {
                        continue;
                    }
                    let idx = ((sy as usize) * (src_w as usize) + sx as usize) * 4;
                    r += rgba[idx] as u32;
                    g += rgba[idx + 1] as u32;
                    b += rgba[idx + 2] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            pixels.push(egui::Color32::from_rgb(
                (r / n) as u8,
                (g / n) as u8,
                (b / n) as u8,
            ));
        }
    }
    Some(egui::ColorImage {
        size: [dst_w as usize, dst_h as usize],
        pixels,
        source_size: egui::vec2(dst_w as f32, dst_h as f32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: i32 = 0x0001_0000;

    #[test]
    fn identity_matrix_is_unrotated() {
        assert_eq!(Rotation::from_matrix(ONE, 0, 0, ONE), Rotation::None);
    }

    #[test]
    fn the_three_right_angle_matrices_are_recognised() {
        assert_eq!(Rotation::from_matrix(0, ONE, -ONE, 0), Rotation::Cw90);
        assert_eq!(Rotation::from_matrix(-ONE, 0, 0, -ONE), Rotation::Cw180);
        assert_eq!(Rotation::from_matrix(0, -ONE, ONE, 0), Rotation::Cw270);
    }

    #[test]
    fn an_arbitrary_affine_falls_back_to_unrotated() {
        assert_eq!(
            Rotation::from_matrix(ONE / 2, ONE / 3, 0, ONE),
            Rotation::None
        );
        assert_eq!(Rotation::from_matrix(0, 0, 0, 0), Rotation::None);
    }

    #[test]
    fn quarter_turns_swap_the_display_size() {
        assert_eq!(Rotation::None.apply_to_size(1920, 1080), (1920, 1080));
        assert_eq!(Rotation::Cw90.apply_to_size(1920, 1080), (1080, 1920));
        assert_eq!(Rotation::Cw180.apply_to_size(1920, 1080), (1920, 1080));
        assert_eq!(Rotation::Cw270.apply_to_size(1920, 1080), (1080, 1920));
    }

    #[test]
    fn fit_preserves_aspect_and_never_upscales() {
        assert_eq!(fit_within(1920, 1080, 352, 198), (352, 198));
        assert_eq!(fit_within(1920, 1080, 352, 1000), (352, 198));
        assert_eq!(fit_within(1080, 1920, 352, 198), (111, 198));
        // Already smaller than the box: left alone.
        assert_eq!(fit_within(64, 48, 352, 198), (64, 48));
        // Degenerate inputs stay drawable.
        assert_eq!(fit_within(0, 0, 10, 10), (1, 1));
        assert_eq!(fit_within(10, 10, 0, 0), (1, 1));
    }

    fn solid(w: u32, h: u32, color: [u8; 4]) -> Vec<u8> {
        color
            .iter()
            .copied()
            .cycle()
            .take((w as usize) * (h as usize) * 4)
            .collect()
    }

    #[test]
    fn a_solid_frame_survives_downscaling_unchanged_in_colour() {
        let rgba = solid(64, 64, [10, 200, 30, 255]);
        let img = rgba_to_color_image(&rgba, 64, 64, Rotation::None, 16, 16).unwrap();
        assert_eq!(img.size, [16, 16]);
        assert!(img
            .pixels
            .iter()
            .all(|p| *p == egui::Color32::from_rgb(10, 200, 30)));
    }

    #[test]
    fn rotation_moves_the_marked_corner() {
        // 2x1 frame: left pixel red, right pixel blue.
        let rgba = vec![255, 0, 0, 255, 0, 0, 255, 255];
        let up = rgba_to_color_image(&rgba, 2, 1, Rotation::None, 64, 64).unwrap();
        assert_eq!(up.size, [2, 1]);
        assert_eq!(up.pixels[0], egui::Color32::from_rgb(255, 0, 0));

        // Turned 90 degrees clockwise the frame becomes 1x2 with red on top.
        let cw = rgba_to_color_image(&rgba, 2, 1, Rotation::Cw90, 64, 64).unwrap();
        assert_eq!(cw.size, [1, 2]);
        assert_eq!(cw.pixels[0], egui::Color32::from_rgb(255, 0, 0));
        assert_eq!(cw.pixels[1], egui::Color32::from_rgb(0, 0, 255));

        // And 180 degrees just swaps them in place.
        let half = rgba_to_color_image(&rgba, 2, 1, Rotation::Cw180, 64, 64).unwrap();
        assert_eq!(half.size, [2, 1]);
        assert_eq!(half.pixels[0], egui::Color32::from_rgb(0, 0, 255));
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_read_past() {
        let rgba = vec![0u8; 10];
        assert!(rgba_to_color_image(&rgba, 64, 64, Rotation::None, 16, 16).is_none());
        assert!(rgba_to_color_image(&[], 0, 0, Rotation::None, 16, 16).is_none());
    }
}
