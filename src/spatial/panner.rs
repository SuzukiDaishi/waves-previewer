//! The monitoring panner: a Cartesian position onto the 7.1.4 virtual bed.
//!
//! Separable and pairwise, in the room's own coordinates: the height picks
//! between the ear layer and the upper layer, Y picks between the two rows
//! of speakers that bracket it inside each layer, and X between the two
//! speakers that bracket it inside each row. Every stage is a sine/cosine
//! pair, so the gains' squares always sum to one and an object keeps its
//! level wherever it goes.
//!
//! This is a preview panner. It is not Dolby's renderer and not the
//! ITU-R BS.2127 object renderer, and size, divergence, zones and screen
//! references are not rendered (see `docs/SPATIAL_AUDIO_SPEC.md`). What it
//! guarantees is that a sound placed at a speaker comes out of that speaker
//! alone and moves smoothly between them.

use crate::audio_channels::{standard_layout, SpeakerPos};

use super::coords;

/// Channels of the virtual bed: 7.1.4 in WAVE order (`LAYOUT_12`).
pub const BED_CHANNELS: usize = 12;
/// Room in the gain array past the bed, so the mixing loop can run over a
/// power of two.
pub const GAIN_SLOTS: usize = 16;
pub type Gains = [f32; GAIN_SLOTS];

const FL: usize = 0;
const FR: usize = 1;
const FC: usize = 2;
const LFE: usize = 3;
const BL: usize = 4;
const BR: usize = 5;
const SL: usize = 6;
const SR: usize = 7;
const TFL: usize = 8;
const TFR: usize = 9;
const TBL: usize = 10;
const TBR: usize = 11;

/// The bed's speakers, in channel order.
pub fn bed_layout() -> &'static [SpeakerPos] {
    standard_layout(BED_CHANNELS).expect("12 channels have a standard layout")
}

/// Where `v` falls between the bracketing pair of `points` (ascending), as
/// (lower index, upper index, lower weight, upper weight).
fn pair(points: &[f32], v: f32) -> (usize, usize, f32, f32) {
    let last = points.len() - 1;
    let v = v.clamp(points[0], points[last]);
    let mut i = 0;
    while i + 1 < last && v > points[i + 1] {
        i += 1;
    }
    let span = points[i + 1] - points[i];
    let t = if span > 0.0 {
        (v - points[i]) / span
    } else {
        0.0
    };
    let angle = t * std::f32::consts::FRAC_PI_2;
    (i, i + 1, angle.cos(), angle.sin())
}

/// Spread `weight` over one row of speakers at `xs` by `x`.
fn pan_row(gains: &mut Gains, xs: &[f32], speakers: &[usize], x: f32, weight: f32) {
    let (lo, hi, wl, wh) = pair(xs, x);
    gains[speakers[lo]] += weight * wl;
    gains[speakers[hi]] += weight * wh;
}

/// Gains onto the bed for a point at ADM Cartesian `pos`. Below the ear
/// layer there are no speakers, so negative Z stays at ear level.
pub fn pan_allocentric(pos: [f32; 3]) -> Gains {
    let mut gains = [0.0; GAIN_SLOTS];
    let [x, y, z] = pos;
    let (_, _, ear, top) = pair(&[0.0, 1.0], z.clamp(0.0, 1.0));

    if ear > 0.0 {
        // Rows of the ear layer, back to front.
        let (lo, hi, wl, wh) = pair(&[-1.0, 0.0, 1.0], y);
        for (row, weight) in [(lo, wl), (hi, wh)] {
            let weight = ear * weight;
            if weight == 0.0 {
                continue;
            }
            match row {
                0 => pan_row(&mut gains, &[-1.0, 1.0], &[BL, BR], x, weight),
                1 => pan_row(&mut gains, &[-1.0, 1.0], &[SL, SR], x, weight),
                _ => pan_row(&mut gains, &[-1.0, 0.0, 1.0], &[FL, FC, FR], x, weight),
            }
        }
    }
    if top > 0.0 {
        let (lo, hi, wl, wh) = pair(&[-1.0, 1.0], y);
        for (row, weight) in [(lo, wl), (hi, wh)] {
            let weight = top * weight;
            if weight == 0.0 {
                continue;
            }
            match row {
                0 => pan_row(&mut gains, &[-1.0, 1.0], &[TBL, TBR], x, weight),
                _ => pan_row(&mut gains, &[-1.0, 1.0], &[TFL, TFR], x, weight),
            }
        }
    }
    gains
}

/// Gains for a bed channel: its own speaker when the bed has one, the LFE
/// channel for an LFE, otherwise panned at its nominal position.
pub fn bed_channel_gains(speaker: Option<SpeakerPos>, nominal_cart: [f32; 3]) -> Gains {
    let mut gains = [0.0; GAIN_SLOTS];
    if let Some(speaker) = speaker {
        if speaker.is_lfe() {
            gains[LFE] = 1.0;
            return gains;
        }
        if let Some(index) = bed_layout().iter().position(|p| *p == speaker) {
            gains[index] = 1.0;
            return gains;
        }
        if let Some(cart) = coords::speaker_cart(speaker) {
            return pan_allocentric(cart);
        }
    }
    pan_allocentric(nominal_cart)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn power(g: &Gains) -> f32 {
        g.iter().map(|v| v * v).sum()
    }

    #[test]
    fn a_point_at_a_speaker_plays_from_that_speaker_alone() {
        for speaker in bed_layout() {
            let Some(cart) = coords::speaker_cart(*speaker) else {
                continue;
            };
            let gains = pan_allocentric(cart);
            let index = bed_layout().iter().position(|p| p == speaker).unwrap();
            assert!((gains[index] - 1.0).abs() < 1e-5, "{speaker:?}: {gains:?}");
            assert!((power(&gains) - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn the_ceiling_centre_splits_over_the_four_tops() {
        let gains = pan_allocentric([0.0, 0.0, 1.0]);
        for index in [TFL, TFR, TBL, TBR] {
            assert!((gains[index] - 0.5).abs() < 1e-5, "{gains:?}");
        }
        assert!(gains[..TFL].iter().all(|g| g.abs() < 1e-6));
    }

    #[test]
    fn power_is_kept_everywhere_in_the_room() {
        for x in [-1.0f32, -0.7, 0.0, 0.3, 1.0] {
            for y in [-1.0f32, -0.5, 0.0, 0.4, 1.0] {
                for z in [-0.5f32, 0.0, 0.25, 0.8, 1.0] {
                    let gains = pan_allocentric([x, y, z]);
                    assert!((power(&gains) - 1.0).abs() < 1e-4, "{x} {y} {z}");
                    assert_eq!(gains[LFE], 0.0, "nothing pans into the LFE");
                }
            }
        }
    }

    #[test]
    fn a_bed_channel_takes_its_own_speaker_or_is_panned() {
        let lfe = bed_channel_gains(Some(SpeakerPos::Lfe), [0.0; 3]);
        assert_eq!(lfe[LFE], 1.0);
        let lss = bed_channel_gains(Some(SpeakerPos::Sl), [0.0; 3]);
        assert_eq!(lss[SL], 1.0);
        // Top side left is not in 7.1.4: half to the front top, half back.
        let tsl = bed_channel_gains(Some(SpeakerPos::Tsl), [0.0; 3]);
        assert!(
            (tsl[TFL] - tsl[TBL]).abs() < 1e-5 && tsl[TFL] > 0.6,
            "{tsl:?}"
        );
        // No speaker of ours: its nominal position.
        let bottom = bed_channel_gains(None, [0.0, 1.0, -1.0]);
        assert!((bottom[FC] - 1.0).abs() < 1e-5);
    }
}
