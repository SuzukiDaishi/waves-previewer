//! Apple ProRes decoding, via the pure-Rust `oxideav-prores`.
//!
//! ProRes is intra-only: every sample is a whole picture that decodes on its
//! own. So unlike the H.264 backends there is no GOP to walk from a keyframe
//! and no reorder queue -- a seek just moves the cursor to the sample showing
//! at that time, and the next call decodes exactly that sample.
//!
//! The crate hands back 8-bit planar Y'CbCr, plus an alpha plane when the
//! stream codes one (4444 exported from a motion-graphics tool usually does).
//! Colour conversion and alpha compositing happen here; rotation and the
//! shrink to the panel reuse the same box filter as OpenH264.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;

use super::container::{VideoCodec, VideoContainer, VideoStreamInfo};
use super::frame::{fit_within, rgba_to_color_image, VideoFrame};
use super::VideoDecoder;

/// Checkerboard behind transparent pixels, as on-screen pixels per cell.
///
/// Sized after the shrink to the panel, so a 4K and a 720p overlay show the
/// same pattern. Dark greys rather than the usual light pair: the panel sits
/// in a dark UI, and graphics overlays are mostly white lyric text, which a
/// light checkerboard would swallow.
const CHECKER_CELL_PX: f64 = 8.0;
const CHECKER_LIGHT: u8 = 58;
const CHECKER_DARK: u8 = 40;

pub struct ProResDecoder {
    container: VideoContainer,
    /// Index position (presentation order) of the next sample to decode.
    next_pos: usize,
    /// Reused across frames so playback does not allocate 8 MB per picture.
    rgba_scratch: Vec<u8>,
}

impl ProResDecoder {
    pub fn open(container: VideoContainer) -> Result<Self> {
        if container.info.codec != VideoCodec::ProRes {
            anyhow::bail!(
                "the ProRes decoder cannot read this track, which is {}",
                container.info.codec_label
            );
        }
        Ok(Self {
            container,
            next_pos: 0,
            rgba_scratch: Vec::new(),
        })
    }
}

impl VideoDecoder for ProResDecoder {
    fn info(&self) -> &VideoStreamInfo {
        &self.container.info
    }

    fn seek(&mut self, secs: f64, _max_forward_walk: usize) -> Result<()> {
        // Every sample decodes on its own, so there is nothing to walk.
        if let Some(position) = self.container.index.position_at_or_before(secs) {
            self.next_pos = position;
        }
        Ok(())
    }

    fn next_frame(
        &mut self,
        box_px: (u32, u32),
        cancel: &AtomicBool,
    ) -> Result<Option<VideoFrame>> {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let position = self.next_pos;
        let Some(pts_secs) = self.container.index.pts_secs(position) else {
            return Ok(None);
        };
        let Some(bytes) = self.container.read_sample_at(position)? else {
            return Ok(None);
        };
        self.next_pos += 1;

        let (header, _) = oxideav_prores::frame::parse_frame(&bytes)
            .map_err(|e| anyhow::anyhow!("prores frame header: {e}"))?;
        let matrix = YCbCrMatrix::for_stream(header.matrix_coefficients, header.height);
        let decoded = oxideav_prores::decoder::decode_packet(&bytes, None)
            .map_err(|e| anyhow::anyhow!("prores decode: {e}"))?;
        let planes: Vec<&[u8]> = decoded.planes.iter().map(|p| p.data.as_slice()).collect();
        let strides: Vec<usize> = decoded.planes.iter().map(|p| p.stride).collect();
        let Some((w, h)) = planar_to_rgba(
            &planes,
            &strides,
            matrix,
            self.checker_cell(box_px),
            &mut self.rgba_scratch,
        ) else {
            anyhow::bail!("prores: decoded planes do not describe one picture");
        };
        let Some(image) = rgba_to_color_image(
            &self.rgba_scratch,
            w,
            h,
            self.container.info.rotation,
            box_px.0,
            box_px.1,
        ) else {
            return Ok(None);
        };
        Ok(Some(VideoFrame {
            pts_secs,
            image: Arc::new(image),
        }))
    }
}

impl ProResDecoder {
    /// Checkerboard cell in source pixels, so that it lands at about
    /// [`CHECKER_CELL_PX`] once the frame is shrunk into `box_px`.
    fn checker_cell(&self, box_px: (u32, u32)) -> usize {
        let info = &self.container.info;
        let (disp_w, disp_h) = (info.display_width, info.display_height);
        let (dst_w, _) = fit_within(disp_w, disp_h, box_px.0, box_px.1);
        let scale = disp_w.max(1) as f64 / dst_w.max(1) as f64;
        ((CHECKER_CELL_PX * scale).round() as usize).max(1)
    }
}

/// Fixed-point (x65536) coefficients for video-range Y'CbCr to RGB.
#[derive(Clone, Copy, Debug, PartialEq)]
struct YCbCrMatrix {
    y: i32,
    cr_r: i32,
    cb_g: i32,
    cr_g: i32,
    cb_b: i32,
}

impl YCbCrMatrix {
    /// The matrix a stream declares (ITU-T H.273 `matrix_coefficients`, as
    /// RDD 36 carries it), or the conventional one for its size when it
    /// declares none -- which is common: graphics exports often leave the
    /// colour description unspecified.
    fn for_stream(matrix_coefficients: u8, height: u16) -> Self {
        match matrix_coefficients {
            1 => Self::from_weights(0.2126, 0.0722),
            5 | 6 => Self::from_weights(0.299, 0.114),
            9 => Self::from_weights(0.2627, 0.0593),
            _ if height > 576 => Self::from_weights(0.2126, 0.0722),
            _ => Self::from_weights(0.299, 0.114),
        }
    }

    fn from_weights(kr: f64, kb: f64) -> Self {
        let kg = 1.0 - kr - kb;
        // Video range: luma spans 16..=235 and chroma 16..=240 in 8 bits.
        let y_scale = 255.0 / 219.0;
        let c_scale = 255.0 / 224.0;
        let fixed = |v: f64| (v * 65536.0).round() as i32;
        Self {
            y: fixed(y_scale),
            cr_r: fixed(2.0 * (1.0 - kr) * c_scale),
            cb_g: fixed(2.0 * kb * (1.0 - kb) / kg * c_scale),
            cr_g: fixed(2.0 * kr * (1.0 - kr) / kg * c_scale),
            cb_b: fixed(2.0 * (1.0 - kb) * c_scale),
        }
    }

    fn to_rgb(self, y: u8, cb: u8, cr: u8) -> [u8; 3] {
        let y = self.y * (i32::from(y) - 16) + 32768;
        let cb = i32::from(cb) - 128;
        let cr = i32::from(cr) - 128;
        let clamp = |v: i32| (v >> 16).clamp(0, 255) as u8;
        [
            clamp(y + self.cr_r * cr),
            clamp(y - self.cb_g * cb - self.cr_g * cr),
            clamp(y + self.cb_b * cb),
        ]
    }
}

/// Convert the decoder's planes (Y, Cb, Cr and optionally A, one byte per
/// sample) into packed RGBA in `out`, compositing any alpha over a
/// checkerboard of `checker_cell` source pixels. Returns the picture size, or
/// `None` when the planes do not fit together.
///
/// 4:2:2 is recognised by its chroma planes being half the luma width
/// (rounded up), which is how the crate crops them.
fn planar_to_rgba(
    planes: &[&[u8]],
    strides: &[usize],
    matrix: YCbCrMatrix,
    checker_cell: usize,
    out: &mut Vec<u8>,
) -> Option<(u32, u32)> {
    if planes.len() < 3 || strides.len() != planes.len() {
        return None;
    }
    let w = strides[0];
    if w == 0 {
        return None;
    }
    let h = planes[0].len() / w;
    let c_w = strides[1];
    let chroma_shift = if c_w == w {
        0
    } else if c_w == w.div_ceil(2) {
        1
    } else {
        return None;
    };
    if h == 0
        || strides[2] != c_w
        || planes[1].len() < c_w * h
        || planes[2].len() < c_w * h
    {
        return None;
    }
    let alpha = match (planes.get(3), strides.get(3)) {
        (Some(plane), Some(&stride)) if stride == w && plane.len() >= w * h => Some(*plane),
        (Some(_), _) => return None,
        _ => None,
    };

    let checker_cell = checker_cell.max(1);
    out.clear();
    out.resize(w * h * 4, 255);
    for row in 0..h {
        let y_row = &planes[0][row * w..row * w + w];
        let cb_row = &planes[1][row * c_w..row * c_w + c_w];
        let cr_row = &planes[2][row * c_w..row * c_w + c_w];
        let a_row = alpha.map(|a| &a[row * w..row * w + w]);
        let out_row = &mut out[row * w * 4..(row + 1) * w * 4];
        let checker_row = row / checker_cell;
        for x in 0..w {
            let cx = x >> chroma_shift;
            let mut rgb = matrix.to_rgb(y_row[x], cb_row[cx], cr_row[cx]);
            if let Some(a_row) = a_row {
                let a = u32::from(a_row[x]);
                if a < 255 {
                    let back = if (checker_row + x / checker_cell) % 2 == 0 {
                        CHECKER_LIGHT
                    } else {
                        CHECKER_DARK
                    };
                    let back = u32::from(back) * (255 - a);
                    for channel in &mut rgb {
                        *channel = ((u32::from(*channel) * a + back + 127) / 255) as u8;
                    }
                }
            }
            out_row[x * 4..x * 4 + 3].copy_from_slice(&rgb);
        }
    }
    Some((w as u32, h as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bt709() -> YCbCrMatrix {
        YCbCrMatrix::for_stream(1, 1080)
    }

    #[test]
    fn video_range_black_and_white_reach_the_ends_of_rgb() {
        assert_eq!(bt709().to_rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(bt709().to_rgb(235, 128, 128), [255, 255, 255]);
    }

    #[test]
    fn an_undeclared_matrix_follows_the_picture_size() {
        assert_eq!(YCbCrMatrix::for_stream(2, 1080), bt709());
        assert_eq!(
            YCbCrMatrix::for_stream(2, 480),
            YCbCrMatrix::for_stream(6, 480)
        );
        assert_ne!(YCbCrMatrix::for_stream(2, 480), bt709());
    }

    #[test]
    fn transparent_pixels_show_the_checkerboard_and_opaque_ones_do_not() {
        // 4x1, 4:4:4, mid grey luma; alpha opaque, opaque, clear, clear.
        let y = [126u8; 4];
        let c = [128u8; 4];
        let a = [255u8, 255, 0, 0];
        let mut out = Vec::new();
        let size = planar_to_rgba(&[&y, &c, &c, &a], &[4, 4, 4, 4], bt709(), 2, &mut out);
        assert_eq!(size, Some((4, 1)));
        let grey = bt709().to_rgb(126, 128, 128)[0];
        assert_eq!(out[0], grey);
        assert_eq!(out[4], grey);
        // Cell size 2: pixels 2 and 3 share the second cell, the dark one.
        assert_eq!(&out[8..11], &[CHECKER_DARK; 3]);
        assert_eq!(&out[12..15], &[CHECKER_DARK; 3]);
    }

    #[test]
    fn half_width_chroma_is_read_as_422() {
        // 3x1 luma with 2 chroma samples (ceil(3/2)): red, then blue.
        let y = [81u8, 81, 41];
        let cb = [90u8, 240];
        let cr = [240u8, 110];
        let mut out = Vec::new();
        let size = planar_to_rgba(&[&y, &cb, &cr], &[3, 2, 2], bt709(), 8, &mut out);
        assert_eq!(size, Some((3, 1)));
        // The first two pixels share chroma sample 0, the third has its own.
        assert_eq!(&out[0..3], &out[4..7]);
        assert!(out[0] > 200 && out[2] < 60, "expected red, got {:?}", &out[0..3]);
        assert!(out[10] > 200, "expected blue, got {:?}", &out[8..11]);
    }

    #[test]
    fn planes_that_do_not_fit_together_are_refused() {
        let mut out = Vec::new();
        // Chroma neither full nor half width.
        assert!(planar_to_rgba(&[&[0; 8], &[0; 3], &[0; 3]], &[4, 3, 3], bt709(), 8, &mut out)
            .is_none());
        // Chroma plane too short for the luma height.
        assert!(planar_to_rgba(&[&[0; 8], &[0; 4], &[0; 4]], &[4, 4, 4], bt709(), 8, &mut out)
            .is_none());
        // An alpha plane with the wrong stride.
        assert!(planar_to_rgba(
            &[&[0; 4], &[0; 4], &[0; 4], &[0; 2]],
            &[4, 4, 4, 2],
            bt709(),
            8,
            &mut out
        )
        .is_none());
        assert!(planar_to_rgba(&[&[0; 4]], &[4], bt709(), 8, &mut out).is_none());
    }
}
