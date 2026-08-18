//! Speaker-aware mapping from a clip's channels onto the output device's.
//!
//! The output stream is opened with whatever channel count the OS reports for
//! the device, so a stereo clip on a 7.1.4 endpoint has to be spread over 12
//! outputs. Doing that by channel *index* puts the right channel into the
//! centre, the LFE, the surrounds and the heights — the clip appears to come
//! from behind the listener. So the mapping is done by speaker *position*
//! instead: both sides are labelled with the standard WAVE layout for their
//! channel count, and a mix matrix is derived from those labels.
//!
//! The matrix is pure routing arithmetic — no filtering, no delay, no bass
//! management. Anything the output has no source for stays silent.

/// A speaker position, named after the WAVEFORMATEXTENSIBLE channel mask bits.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SpeakerPos {
    /// Front left.
    Fl,
    /// Front right.
    Fr,
    /// Front centre.
    Fc,
    /// Low-frequency effects.
    Lfe,
    /// Back (rear) left.
    Bl,
    /// Back (rear) right.
    Br,
    /// Front left of centre.
    Flc,
    /// Front right of centre.
    Frc,
    /// Back centre.
    Bc,
    /// Side left.
    Sl,
    /// Side right.
    Sr,
    /// Top front left.
    Tfl,
    /// Top front centre.
    Tfc,
    /// Top front right.
    Tfr,
    /// Top back left.
    Tbl,
    /// Top back centre.
    Tbc,
    /// Top back right.
    Tbr,
}

use SpeakerPos::*;

/// -3 dB, the usual coefficient for folding a channel into a neighbour.
const HALF_POWER: f32 = std::f32::consts::FRAC_1_SQRT_2;

const LAYOUT_1: &[SpeakerPos] = &[Fc];
const LAYOUT_2: &[SpeakerPos] = &[Fl, Fr];
const LAYOUT_3: &[SpeakerPos] = &[Fl, Fr, Fc];
const LAYOUT_4: &[SpeakerPos] = &[Fl, Fr, Bl, Br];
const LAYOUT_5: &[SpeakerPos] = &[Fl, Fr, Fc, Bl, Br];
const LAYOUT_6: &[SpeakerPos] = &[Fl, Fr, Fc, Lfe, Bl, Br];
const LAYOUT_7: &[SpeakerPos] = &[Fl, Fr, Fc, Lfe, Bc, Sl, Sr];
const LAYOUT_8: &[SpeakerPos] = &[Fl, Fr, Fc, Lfe, Bl, Br, Sl, Sr];
const LAYOUT_10: &[SpeakerPos] = &[Fl, Fr, Fc, Lfe, Bl, Br, Sl, Sr, Tfl, Tfr];
const LAYOUT_12: &[SpeakerPos] = &[Fl, Fr, Fc, Lfe, Bl, Br, Sl, Sr, Tfl, Tfr, Tbl, Tbr];

/// The standard WAVE / WASAPI channel order for a given channel count.
///
/// `None` for counts with no agreed layout (9, 11, 13+); callers fall back to
/// index arithmetic there rather than guessing at speaker positions.
pub fn standard_layout(count: usize) -> Option<&'static [SpeakerPos]> {
    match count {
        1 => Some(LAYOUT_1),
        2 => Some(LAYOUT_2),
        3 => Some(LAYOUT_3),
        4 => Some(LAYOUT_4),
        5 => Some(LAYOUT_5),
        6 => Some(LAYOUT_6),
        7 => Some(LAYOUT_7),
        8 => Some(LAYOUT_8),
        10 => Some(LAYOUT_10),
        12 => Some(LAYOUT_12),
        _ => None,
    }
}

/// How source channels reach the output.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum ChannelMapMode {
    /// Route by speaker position, folding what the output cannot reproduce.
    #[default]
    Auto,
    /// Route by channel index: source channel N to output channel N, no mixing.
    Direct,
}

impl ChannelMapMode {
    /// Wire encoding for the atomic the audio callback reads.
    pub fn to_u8(self) -> u8 {
        match self {
            ChannelMapMode::Auto => 0,
            ChannelMapMode::Direct => 1,
        }
    }

    /// Inverse of [`ChannelMapMode::to_u8`]; unknown values decode to `Auto`.
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => ChannelMapMode::Direct,
            _ => ChannelMapMode::Auto,
        }
    }
}

/// The highest source channel index the matrix can address.
///
/// Matches the width of the mute/solo masks in `SharedAudio`, and bounds the
/// per-frame scratch buffer the callback keeps on the stack.
pub const MAX_SOURCE_CHANNELS: usize = 64;

/// One output channel's recipe: the source channels mixed into it, with gains.
pub type MixRow = Vec<(u8, f32)>;

/// A source-to-output routing matrix, built once per callback invocation.
#[derive(Clone, Debug)]
pub struct ChannelMixMatrix {
    rows: Vec<MixRow>,
    used_sources: Vec<u8>,
}

impl ChannelMixMatrix {
    /// Derive the matrix for a clip of `src_channels` on a device of
    /// `out_channels`.
    pub fn build(src_channels: usize, out_channels: usize, mode: ChannelMapMode) -> Self {
        let out_channels = out_channels.max(1);
        let src_channels = src_channels.clamp(1, MAX_SOURCE_CHANNELS);
        let rows = match mode {
            ChannelMapMode::Direct => direct_rows(src_channels, out_channels),
            ChannelMapMode::Auto => auto_rows(src_channels, out_channels),
        };
        let mut used_sources = Vec::with_capacity(src_channels.min(out_channels * 2));
        for row in &rows {
            for (src, _) in row {
                if !used_sources.contains(src) {
                    used_sources.push(*src);
                }
            }
        }
        used_sources.sort_unstable();
        Self { rows, used_sources }
    }

    /// The recipe for one output channel; empty means silence.
    pub fn row(&self, out_ch: usize) -> &[(u8, f32)] {
        self.rows.get(out_ch).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every source channel the matrix reads, ascending and deduplicated.
    ///
    /// The callback only interpolates these, so a stereo clip on a 7.1.4
    /// device costs two reads per frame instead of twelve.
    pub fn used_sources(&self) -> &[u8] {
        &self.used_sources
    }

    /// Mix one already-fetched source frame into one output channel.
    pub fn mix(&self, out_ch: usize, src_frame: &[f32]) -> f32 {
        let mut sum = 0.0f32;
        for (src, gain) in self.row(out_ch) {
            sum += src_frame.get(*src as usize).copied().unwrap_or(0.0) * gain;
        }
        sum
    }
}

fn direct_rows(src_channels: usize, out_channels: usize) -> Vec<MixRow> {
    (0..out_channels)
        .map(|out_ch| {
            if out_ch < src_channels {
                vec![(out_ch as u8, 1.0)]
            } else {
                Vec::new()
            }
        })
        .collect()
}

fn auto_rows(src_channels: usize, out_channels: usize) -> Vec<MixRow> {
    let (Some(src_layout), Some(out_layout)) =
        (standard_layout(src_channels), standard_layout(out_channels))
    else {
        return legacy_fold_rows(src_channels, out_channels);
    };

    let mut rows: Vec<MixRow> = vec![Vec::new(); out_channels];
    let index_of = |pos: SpeakerPos| out_layout.iter().position(|p| *p == pos);

    // A mono clip is a centre-agnostic signal: send it to both front speakers
    // rather than to a centre channel the listener may not have.
    if src_channels == 1 {
        match (index_of(Fl), index_of(Fr)) {
            (Some(l), Some(r)) => {
                rows[l].push((0, 1.0));
                rows[r].push((0, 1.0));
            }
            _ => rows[0].push((0, 1.0)),
        }
        return rows;
    }

    for (src_ch, src_pos) in src_layout.iter().enumerate() {
        for (out_ch, gain) in targets_for(*src_pos, &index_of) {
            rows[out_ch].push((src_ch as u8, gain));
        }
    }
    rows
}

/// Where one source position lands, given which outputs exist.
///
/// A direct match wins; otherwise the position folds into its nearest
/// neighbours at -3 dB. LFE is dropped when the device has no LFE, which is
/// what ITU-R BS.775 downmixing prescribes and keeps the fold from booming.
fn targets_for(
    src_pos: SpeakerPos,
    index_of: &impl Fn(SpeakerPos) -> Option<usize>,
) -> Vec<(usize, f32)> {
    if let Some(out_ch) = index_of(src_pos) {
        return vec![(out_ch, 1.0)];
    }
    // First existing position in the chain wins, at -3 dB.
    let first_of = |chain: &[SpeakerPos]| -> Vec<(usize, f32)> {
        for pos in chain {
            if let Some(out_ch) = index_of(*pos) {
                return vec![(out_ch, HALF_POWER)];
            }
        }
        Vec::new()
    };
    // Split evenly across a left/right pair, falling back to a narrower pair.
    let split = |pairs: &[(SpeakerPos, SpeakerPos)]| -> Vec<(usize, f32)> {
        for (left, right) in pairs {
            if let (Some(l), Some(r)) = (index_of(*left), index_of(*right)) {
                return vec![(l, HALF_POWER), (r, HALF_POWER)];
            }
        }
        Vec::new()
    };

    match src_pos {
        // Centre spreads into the front pair as a phantom centre.
        Fc => split(&[(Fl, Fr)]),
        // No LFE speaker: drop it rather than fold bass into the mains.
        Lfe => Vec::new(),
        Fl => first_of(&[Flc, Fc]),
        Fr => first_of(&[Frc, Fc]),
        Flc => first_of(&[Fl, Fc]),
        Frc => first_of(&[Fr, Fc]),
        Bl => first_of(&[Sl, Fl]),
        Br => first_of(&[Sr, Fr]),
        Sl => first_of(&[Bl, Fl]),
        Sr => first_of(&[Br, Fr]),
        Bc => split(&[(Bl, Br), (Sl, Sr), (Fl, Fr)]),
        // Heights drop onto the bed speaker below them.
        Tfl => first_of(&[Fl, Sl, Bl]),
        Tfr => first_of(&[Fr, Sr, Br]),
        Tbl => first_of(&[Bl, Sl, Fl]),
        Tbr => first_of(&[Br, Sr, Fr]),
        Tfc | Tbc => split(&[(Fl, Fr)]),
    }
}

/// The pre-speaker-mapping behaviour, kept for channel counts with no standard
/// layout: outputs beyond the source repeat its last channel, and a source
/// wider than the output folds by averaging every `out_channels`-th channel.
fn legacy_fold_rows(src_channels: usize, out_channels: usize) -> Vec<MixRow> {
    (0..out_channels)
        .map(|out_ch| {
            if src_channels <= 1 {
                return vec![(0u8, 1.0)];
            }
            if src_channels <= out_channels {
                return vec![(out_ch.min(src_channels - 1) as u8, 1.0)];
            }
            let step = out_channels.max(1);
            let mut sources = Vec::new();
            let mut c = out_ch.min(step - 1);
            while c < src_channels {
                sources.push(c as u8);
                c += step;
            }
            let gain = 1.0 / sources.len().max(1) as f32;
            sources.into_iter().map(|c| (c, gain)).collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render one constant source frame through the matrix.
    fn mix_all(src: &[f32], out_channels: usize, mode: ChannelMapMode) -> Vec<f32> {
        let matrix = ChannelMixMatrix::build(src.len(), out_channels, mode);
        (0..out_channels).map(|c| matrix.mix(c, src)).collect()
    }

    fn mix_auto(src: &[f32], out_channels: usize) -> Vec<f32> {
        mix_all(src, out_channels, ChannelMapMode::Auto)
    }

    fn assert_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len(), "channel count");
        for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
            assert!((a - e).abs() < 1e-6, "ch{i}: {a} != {e} (in {actual:?})");
        }
    }

    #[test]
    fn stereo_to_stereo_is_identity() {
        assert_close(&mix_auto(&[0.25, -0.75], 2), &[0.25, -0.75]);
    }

    #[test]
    fn stereo_on_5_1_only_drives_the_front_pair() {
        // FL FR FC LFE BL BR — centre, LFE and both surrounds stay silent.
        assert_close(
            &mix_auto(&[0.25, -0.75], 6),
            &[0.25, -0.75, 0.0, 0.0, 0.0, 0.0],
        );
    }

    #[test]
    fn stereo_on_7_1_4_only_drives_the_front_pair() {
        // The regression this module exists for: previously out2..out11 all
        // carried the right channel, so a stereo clip came from behind.
        let out = mix_auto(&[0.25, -0.75], 12);
        assert_close(
            &out,
            &[
                0.25, -0.75, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            ],
        );
    }

    #[test]
    fn mono_on_5_1_drives_both_front_speakers_only() {
        assert_close(&mix_auto(&[0.5], 6), &[0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn mono_on_mono_output_stays_on_the_single_channel() {
        assert_close(&mix_auto(&[0.5], 1), &[0.5]);
    }

    #[test]
    fn five_one_to_stereo_uses_itu_coefficients() {
        // FL FR FC LFE BL BR; LFE (0.4) is dropped.
        let src = [0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        let expected_l = 0.1 + HALF_POWER * 0.3 + HALF_POWER * 0.5;
        let expected_r = 0.2 + HALF_POWER * 0.3 + HALF_POWER * 0.6;
        assert_close(&mix_auto(&src, 2), &[expected_l, expected_r]);
    }

    #[test]
    fn quad_to_stereo_folds_the_backs_into_the_fronts() {
        // FL FR BL BR
        let src = [0.2f32, 0.4, 0.6, 0.8];
        assert_close(
            &mix_auto(&src, 2),
            &[0.2 + HALF_POWER * 0.6, 0.4 + HALF_POWER * 0.8],
        );
    }

    #[test]
    fn three_channel_to_stereo_makes_a_phantom_centre() {
        // FL FR FC
        let src = [0.3f32, -0.9, 0.5];
        assert_close(
            &mix_auto(&src, 2),
            &[0.3 + HALF_POWER * 0.5, -0.9 + HALF_POWER * 0.5],
        );
    }

    #[test]
    fn five_one_on_7_1_keeps_backs_in_place_and_leaves_sides_silent() {
        // src FL FR FC LFE BL BR -> out FL FR FC LFE BL BR SL SR
        let src = [0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        assert_close(
            &mix_auto(&src, 8),
            &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.0, 0.0],
        );
    }

    #[test]
    fn seven_one_to_5_1_folds_the_sides_into_the_backs() {
        // src FL FR FC LFE BL BR SL SR -> out FL FR FC LFE BL BR
        let src = [0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
        assert_close(
            &mix_auto(&src, 6),
            &[
                0.1,
                0.2,
                0.3,
                0.4,
                0.5 + HALF_POWER * 0.7,
                0.6 + HALF_POWER * 0.8,
            ],
        );
    }

    #[test]
    fn heights_fold_onto_the_bed_speakers_below_them() {
        // src is 7.1.4, out is 7.1: TFL/TFR land on FL/FR, TBL/TBR on BL/BR.
        let src = [
            0.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0,
        ];
        let out = mix_auto(&src, 8);
        assert!((out[0] - HALF_POWER).abs() < 1e-6, "TFL -> FL: {out:?}");
        let mut src_tbl = [0.0f32; 12];
        src_tbl[10] = 1.0;
        let out = mix_auto(&src_tbl, 8);
        assert!((out[4] - HALF_POWER).abs() < 1e-6, "TBL -> BL: {out:?}");
    }

    #[test]
    fn direct_mode_maps_channel_index_to_channel_index() {
        let src = [0.1f32, 0.2];
        assert_close(
            &mix_all(&src, 6, ChannelMapMode::Direct),
            &[0.1, 0.2, 0.0, 0.0, 0.0, 0.0],
        );
        let wide = [0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        assert_close(&mix_all(&wide, 2, ChannelMapMode::Direct), &[0.1, 0.2]);
    }

    #[test]
    fn unknown_channel_counts_fall_back_to_the_legacy_index_fold() {
        // 9 has no standard layout: out N repeats the last source channel.
        let src = [0.1f32, 0.2];
        assert_close(
            &mix_auto(&src, 9),
            &[0.1, 0.2, 0.2, 0.2, 0.2, 0.2, 0.2, 0.2, 0.2],
        );
        // And a 9-channel source folds by averaging every out_channels-th.
        let src9 = [0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];
        assert_close(
            &mix_auto(&src9, 2),
            &[
                (0.1 + 0.3 + 0.5 + 0.7 + 0.9) / 5.0,
                (0.2 + 0.4 + 0.6 + 0.8) / 4.0,
            ],
        );
    }

    #[test]
    fn used_sources_lists_only_the_channels_the_matrix_reads() {
        let matrix = ChannelMixMatrix::build(2, 12, ChannelMapMode::Auto);
        assert_eq!(matrix.used_sources(), &[0, 1]);
        let matrix = ChannelMixMatrix::build(6, 2, ChannelMapMode::Auto);
        // LFE (channel 3) is dropped, so it is never interpolated.
        assert_eq!(matrix.used_sources(), &[0, 1, 2, 4, 5]);
    }

    #[test]
    fn zero_channel_counts_are_clamped_instead_of_panicking() {
        let matrix = ChannelMixMatrix::build(0, 0, ChannelMapMode::Auto);
        assert_eq!(matrix.row(0), &[(0, 1.0)]);
        assert!(matrix.row(1).is_empty());
    }

    #[test]
    fn channel_map_mode_round_trips_through_its_wire_encoding() {
        for mode in [ChannelMapMode::Auto, ChannelMapMode::Direct] {
            assert_eq!(ChannelMapMode::from_u8(mode.to_u8()), mode);
        }
        assert_eq!(ChannelMapMode::from_u8(200), ChannelMapMode::Auto);
    }
}
