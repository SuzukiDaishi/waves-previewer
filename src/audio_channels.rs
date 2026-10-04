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
//!
//! Either side's labels can also be given explicitly: a file whose channels
//! are in Film order, or whose WAVE header carries a channel mask, or a
//! device whose outputs feed speakers in an order of the user's choosing
//! (see `docs/MULTICHANNEL_SPEC.md`).

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
    /// Top centre, straight overhead.
    Tc,
    /// Front left wide (no channel-mask bit).
    Wl,
    /// Front right wide (no channel-mask bit).
    Wr,
    /// Top side left -- Dolby's "top middle" (no channel-mask bit).
    Tsl,
    /// Top side right (no channel-mask bit).
    Tsr,
}

use SpeakerPos::*;

impl SpeakerPos {
    /// Every position, in the order a layout menu lists them.
    pub const ALL: [SpeakerPos; 22] = [
        Fl, Fr, Fc, Lfe, Sl, Sr, Bl, Br, Bc, Flc, Frc, Wl, Wr, Tfl, Tfr, Tfc, Tsl, Tsr, Tbl, Tbr,
        Tbc, Tc,
    ];

    /// The WAVEFORMATEXTENSIBLE channel-mask bit, where the format has one.
    /// Channels are stored in the order of their bits.
    pub fn mask_bit(self) -> Option<u32> {
        Some(match self {
            Fl => 0x1,
            Fr => 0x2,
            Fc => 0x4,
            Lfe => 0x8,
            Bl => 0x10,
            Br => 0x20,
            Flc => 0x40,
            Frc => 0x80,
            Bc => 0x100,
            Sl => 0x200,
            Sr => 0x400,
            Tc => 0x800,
            Tfl => 0x1000,
            Tfc => 0x2000,
            Tfr => 0x4000,
            Tbl => 0x8000,
            Tbc => 0x10000,
            Tbr => 0x20000,
            Wl | Wr | Tsl | Tsr => return None,
        })
    }

    /// A stable name for storing a layout in prefs and sessions.
    pub fn key(self) -> &'static str {
        match self {
            Fl => "FL",
            Fr => "FR",
            Fc => "FC",
            Lfe => "LFE",
            Bl => "BL",
            Br => "BR",
            Flc => "FLC",
            Frc => "FRC",
            Bc => "BC",
            Sl => "SL",
            Sr => "SR",
            Tc => "TC",
            Tfl => "TFL",
            Tfc => "TFC",
            Tfr => "TFR",
            Tbl => "TBL",
            Tbc => "TBC",
            Tbr => "TBR",
            Wl => "WL",
            Wr => "WR",
            Tsl => "TSL",
            Tsr => "TSR",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|pos| pos.key().eq_ignore_ascii_case(key.trim()))
    }

    pub fn is_lfe(self) -> bool {
        self == Lfe
    }

    /// Above ear level.
    pub fn is_height(self) -> bool {
        matches!(self, Tfl | Tfr | Tfc | Tsl | Tsr | Tbl | Tbr | Tbc | Tc)
    }

    /// What the speaker is called in `layout`. "Ls" is the back pair of a
    /// 5.1 and the side pair of a 7.1, so the surround names depend on which
    /// pairs the layout has.
    pub fn label(self, layout: &[Option<SpeakerPos>]) -> &'static str {
        let has = |pos: SpeakerPos| layout.contains(&Some(pos));
        match self {
            Fl => "L",
            Fr => "R",
            Fc => "C",
            Lfe => "LFE",
            Flc => "Lc",
            Frc => "Rc",
            Bc => "Cs",
            Wl => "Lw",
            Wr => "Rw",
            Tc => "Tc",
            Tfl => "Ltf",
            Tfr => "Rtf",
            Tfc => "Tfc",
            Tsl => "Ltm",
            Tsr => "Rtm",
            Tbl => "Ltr",
            Tbr => "Rtr",
            Tbc => "Tbc",
            Bl if has(Sl) || has(Sr) => "Lrs",
            Br if has(Sl) || has(Sr) => "Rrs",
            Bl => "Ls",
            Br => "Rs",
            Sl if has(Bl) || has(Br) => "Lss",
            Sr if has(Bl) || has(Br) => "Rss",
            Sl => "Ls",
            Sr => "Rs",
        }
    }

    /// Where the speaker stands, as (azimuth, elevation) in degrees:
    /// azimuth 0 straight ahead, positive to the right, 180 behind;
    /// elevation 0 at the ears, 90 overhead. ITU-R BS.2051 / Dolby angles.
    /// A lone surround pair sits at 110 degrees, as in 5.1; with both pairs
    /// the sides are at 90 and the backs at 135. LFE has no direction and
    /// reports straight ahead.
    pub fn direction(self, layout: &[Option<SpeakerPos>]) -> (f32, f32) {
        let has = |pos: SpeakerPos| layout.contains(&Some(pos));
        match self {
            Fl => (-30.0, 0.0),
            Fr => (30.0, 0.0),
            Fc | Lfe => (0.0, 0.0),
            Flc => (-15.0, 0.0),
            Frc => (15.0, 0.0),
            Wl => (-60.0, 0.0),
            Wr => (60.0, 0.0),
            Bc => (180.0, 0.0),
            Bl if has(Sl) || has(Sr) => (-135.0, 0.0),
            Br if has(Sl) || has(Sr) => (135.0, 0.0),
            Bl => (-110.0, 0.0),
            Br => (110.0, 0.0),
            Sl if has(Bl) || has(Br) => (-90.0, 0.0),
            Sr if has(Bl) || has(Br) => (90.0, 0.0),
            Sl => (-110.0, 0.0),
            Sr => (110.0, 0.0),
            Tfl => (-45.0, 45.0),
            Tfr => (45.0, 45.0),
            Tfc => (0.0, 45.0),
            Tsl => (-90.0, 45.0),
            Tsr => (90.0, 45.0),
            Tbl => (-135.0, 45.0),
            Tbr => (135.0, 45.0),
            Tbc => (180.0, 45.0),
            Tc => (0.0, 90.0),
        }
    }
}

/// The positions a WAVEFORMATEXTENSIBLE channel mask names, in the order the
/// channels are stored (ascending bit). `None` when its bits do not number
/// `count`, or name a position the format reserves but this app does not.
pub fn layout_from_mask(mask: u32, count: usize) -> Option<Vec<SpeakerPos>> {
    if mask == 0 || mask.count_ones() as usize != count {
        return None;
    }
    (0..32)
        .map(|bit| 1u32 << bit)
        .filter(|bit| mask & bit != 0)
        .map(|bit| SpeakerPos::ALL.into_iter().find(|pos| pos.mask_bit() == Some(bit)))
        .collect()
}

/// The WAVE channel mask that says `layout`, when one can: every channel a
/// speaker with a mask bit, in rising bit order -- a mask only says which
/// speakers are present, and the channels come in bit order. `None` for a
/// layout no mask can say (Film order, a channel with no speaker).
pub fn mask_for_layout(layout: &[Option<SpeakerPos>]) -> Option<u32> {
    let mut mask = 0u32;
    for pos in layout {
        let bit = (*pos)?.mask_bit()?;
        if bit <= mask {
            return None;
        }
        mask |= bit;
    }
    (mask != 0).then_some(mask)
}

/// A named channel order the layout editor offers.
#[derive(Debug)]
pub struct LayoutPreset {
    pub name: &'static str,
    pub speakers: &'static [SpeakerPos],
}

/// The layouts on offer, grouped by channel count.
pub const PRESETS: &[LayoutPreset] = &[
    LayoutPreset { name: "Mono", speakers: LAYOUT_1 },
    LayoutPreset { name: "Stereo", speakers: LAYOUT_2 },
    LayoutPreset { name: "LCR (WAV)", speakers: LAYOUT_3 },
    LayoutPreset { name: "2.1 (WAV)", speakers: &[Fl, Fr, Lfe] },
    LayoutPreset { name: "Quad (WAV)", speakers: LAYOUT_4 },
    LayoutPreset { name: "5.0 (WAV)", speakers: LAYOUT_5 },
    LayoutPreset { name: "5.1 WAV / SMPTE", speakers: LAYOUT_6 },
    LayoutPreset { name: "5.1 Film / Pro Tools", speakers: &[Fl, Fc, Fr, Bl, Br, Lfe] },
    LayoutPreset { name: "5.1 DTS", speakers: &[Fl, Fr, Bl, Br, Fc, Lfe] },
    LayoutPreset { name: "6.1 (WAV)", speakers: LAYOUT_7 },
    LayoutPreset { name: "7.1 WAV / SMPTE", speakers: LAYOUT_8 },
    LayoutPreset { name: "7.1 Film / Pro Tools", speakers: &[Fl, Fc, Fr, Sl, Sr, Bl, Br, Lfe] },
    LayoutPreset { name: "5.1.2 (WAV)", speakers: &[Fl, Fr, Fc, Lfe, Bl, Br, Tsl, Tsr] },
    LayoutPreset { name: "7.1.2 (WAV)", speakers: LAYOUT_10 },
    LayoutPreset { name: "5.1.4 (WAV)", speakers: &[Fl, Fr, Fc, Lfe, Bl, Br, Tfl, Tfr, Tbl, Tbr] },
    LayoutPreset { name: "7.1.4 WAV / SMPTE", speakers: LAYOUT_12 },
    LayoutPreset {
        name: "7.1.4 Pro Tools",
        speakers: &[Fl, Fc, Fr, Sl, Sr, Bl, Br, Lfe, Tfl, Tfr, Tbl, Tbr],
    },
    LayoutPreset {
        name: "9.1.6",
        speakers: &[Fl, Fr, Fc, Lfe, Bl, Br, Sl, Sr, Wl, Wr, Tfl, Tfr, Tsl, Tsr, Tbl, Tbr],
    },
];

/// The presets with `count` channels.
pub fn presets_for(count: usize) -> impl Iterator<Item = &'static LayoutPreset> {
    PRESETS.iter().filter(move |preset| preset.speakers.len() == count)
}

/// One speaker per channel, or none for a channel that feeds no speaker.
pub type Layout = Vec<Option<SpeakerPos>>;

/// The standard layout for `count` channels, as a [`Layout`].
pub fn standard_layout_vec(count: usize) -> Option<Layout> {
    standard_layout(count).map(|layout| layout.iter().copied().map(Some).collect())
}

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
    /// Source channels that are the LFE (bit N = channel N), which loudness
    /// leaves out: see [`ChannelMixMatrix::mix_loudness`].
    lfe_sources: u64,
}

impl ChannelMixMatrix {
    /// Derive the matrix for a clip of `src_channels` on a device of
    /// `out_channels`, both in the standard layout for their count.
    pub fn build(src_channels: usize, out_channels: usize, mode: ChannelMapMode) -> Self {
        Self::build_with_layouts(src_channels, None, out_channels, None, mode)
    }

    /// As [`ChannelMixMatrix::build`], with either side's speakers given:
    /// `src_layout` for the clip, `out_layout` for the device. A layout that
    /// does not have one entry per channel is ignored for the standard one.
    pub fn build_with_layouts(
        src_channels: usize,
        src_layout: Option<&[Option<SpeakerPos>]>,
        out_channels: usize,
        out_layout: Option<&[Option<SpeakerPos>]>,
        mode: ChannelMapMode,
    ) -> Self {
        let out_channels = out_channels.max(1);
        let src_channels = src_channels.clamp(1, MAX_SOURCE_CHANNELS);
        let rows = match mode {
            ChannelMapMode::Direct => direct_rows(src_channels, out_channels),
            ChannelMapMode::Auto => auto_rows(src_channels, src_layout, out_channels, out_layout),
        };
        let lfe_sources = src_layout
            .filter(|layout| layout.len() == src_channels)
            .map(<[Option<SpeakerPos>]>::to_vec)
            .or_else(|| standard_layout_vec(src_channels))
            .map_or(0, |layout| {
                layout
                    .iter()
                    .enumerate()
                    .filter(|(_, pos)| **pos == Some(Lfe))
                    .fold(0u64, |mask, (ch, _)| mask | 1u64 << ch)
            });
        let mut used_sources = Vec::with_capacity(src_channels.min(out_channels * 2));
        for row in &rows {
            for (src, _) in row {
                if !used_sources.contains(src) {
                    used_sources.push(*src);
                }
            }
        }
        used_sources.sort_unstable();
        Self {
            rows,
            used_sources,
            lfe_sources,
        }
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

    /// As [`ChannelMixMatrix::mix`], without the LFE: what the loudness
    /// meter reads. ITU-R BS.1770 leaves the LFE out of loudness, and a
    /// stereo fold now carries it in the front pair (see `targets_for`), so
    /// the meter takes it out again rather than read the bass as programme.
    pub fn mix_loudness(&self, out_ch: usize, src_frame: &[f32]) -> f32 {
        let mut sum = 0.0f32;
        for (src, gain) in self.row(out_ch) {
            if (self.lfe_sources >> src) & 1 == 1 {
                continue;
            }
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

fn auto_rows(
    src_channels: usize,
    src_layout: Option<&[Option<SpeakerPos>]>,
    out_channels: usize,
    out_layout: Option<&[Option<SpeakerPos>]>,
) -> Vec<MixRow> {
    let given = |layout: Option<&[Option<SpeakerPos>]>, count: usize| {
        layout
            .filter(|layout| layout.len() == count)
            .map(<[Option<SpeakerPos>]>::to_vec)
            .or_else(|| standard_layout_vec(count))
    };
    let (Some(src_layout), Some(out_layout)) =
        (given(src_layout, src_channels), given(out_layout, out_channels))
    else {
        return legacy_fold_rows(src_channels, out_channels);
    };

    let mut rows: Vec<MixRow> = vec![Vec::new(); out_channels];
    let index_of = |pos: SpeakerPos| out_layout.iter().position(|p| *p == Some(pos));

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
        // A channel assigned to no speaker is not heard.
        let Some(src_pos) = src_pos else {
            continue;
        };
        for (out_ch, gain) in targets_for(*src_pos, &index_of) {
            rows[out_ch].push((src_ch as u8, gain));
        }
    }
    rows
}

/// Where one source position lands, given which outputs exist.
///
/// A direct match wins; otherwise the position folds into its nearest
/// neighbours at -3 dB. With no LFE output the LFE folds exactly where the
/// centre does -- a phantom centre on a stereo device. ITU-R BS.775 would
/// drop it, but on headphones or a pair of monitors that loses everything
/// the LFE alone carries; the loudness meter still leaves it out
/// ([`ChannelMixMatrix::mix_loudness`]).
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
        // No LFE speaker: wherever the centre goes, at the centre's gains.
        Lfe => targets_for(Fc, index_of),
        Fl => first_of(&[Flc, Fc]),
        Fr => first_of(&[Frc, Fc]),
        Flc => first_of(&[Fl, Fc]),
        Frc => first_of(&[Fr, Fc]),
        Bl => first_of(&[Sl, Fl]),
        Br => first_of(&[Sr, Fr]),
        Sl => first_of(&[Bl, Fl]),
        Sr => first_of(&[Br, Fr]),
        Bc => split(&[(Bl, Br), (Sl, Sr), (Fl, Fr)]),
        Wl => first_of(&[Fl, Sl]),
        Wr => first_of(&[Fr, Sr]),
        // Heights move to the nearest height speaker there is, else drop
        // onto the bed speaker below them.
        Tfl => first_of(&[Tsl, Fl, Sl, Bl]),
        Tfr => first_of(&[Tsr, Fr, Sr, Br]),
        Tsl => first_of(&[Tfl, Tbl, Sl, Fl, Bl]),
        Tsr => first_of(&[Tfr, Tbr, Sr, Fr, Br]),
        Tbl => first_of(&[Tsl, Bl, Sl, Fl]),
        Tbr => first_of(&[Tsr, Br, Sr, Fr]),
        Tfc | Tbc | Tc => split(&[(Tfl, Tfr), (Tsl, Tsr), (Fl, Fr)]),
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

    #[test]
    fn two_one_is_offered_for_three_channels_and_read_from_its_mask() {
        let names: Vec<&str> = presets_for(3).map(|p| p.name).collect();
        assert_eq!(names, ["LCR (WAV)", "2.1 (WAV)"]);
        // FL | FR | LFE, as Windows' KSAUDIO_SPEAKER_2POINT1.
        assert_eq!(layout_from_mask(0x0B, 3), Some(vec![Fl, Fr, Lfe]));
        // A stereo device hears the LFE in both sides, like a centre.
        let src = [0.0f32, 0.0, 0.8];
        let matrix = ChannelMixMatrix::build_with_layouts(
            3,
            Some(&[Some(Fl), Some(Fr), Some(Lfe)]),
            2,
            None,
            ChannelMapMode::Auto,
        );
        assert_close(
            &[matrix.mix(0, &src), matrix.mix(1, &src)],
            &[0.8 * HALF_POWER, 0.8 * HALF_POWER],
        );
    }

    #[test]
    fn a_layout_has_a_mask_only_in_bit_order() {
        let some = |layout: &[SpeakerPos]| layout.iter().copied().map(Some).collect::<Vec<_>>();
        assert_eq!(mask_for_layout(&some(&[Fl, Fr, Lfe])), Some(0x0B));
        assert_eq!(mask_for_layout(&some(&[Fl, Fr, Bl, Br])), Some(0x33));
        assert_eq!(mask_for_layout(&some(LAYOUT_8)), Some(0x63F), "7.1 in WAV order");
        assert_eq!(mask_for_layout(&some(&[Fl, Fc, Fr])), None, "Film order");
        assert_eq!(mask_for_layout(&[Some(Fl), None, Some(Fr)]), None);
        assert_eq!(mask_for_layout(&[]), None);
    }

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
    fn five_one_to_stereo_folds_the_lfe_like_the_centre() {
        // FL FR FC LFE BL BR: the centre and the LFE both make a phantom
        // centre at -3 dB a side; the surrounds fold at -3 dB.
        let src = [0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6];
        let expected_l = 0.1 + HALF_POWER * 0.3 + HALF_POWER * 0.4 + HALF_POWER * 0.5;
        let expected_r = 0.2 + HALF_POWER * 0.3 + HALF_POWER * 0.4 + HALF_POWER * 0.6;
        assert_close(&mix_auto(&src, 2), &[expected_l, expected_r]);
        // The loudness meter's view leaves the LFE out (ITU-R BS.1770).
        let matrix = ChannelMixMatrix::build(6, 2, ChannelMapMode::Auto);
        assert_close(
            &[matrix.mix_loudness(0, &src), matrix.mix_loudness(1, &src)],
            &[
                0.1 + HALF_POWER * 0.3 + HALF_POWER * 0.5,
                0.2 + HALF_POWER * 0.3 + HALF_POWER * 0.6,
            ],
        );
    }

    #[test]
    fn the_lfe_takes_the_centre_speaker_when_there_is_one_and_its_own_first() {
        // LFE alone, 5.1 onto an LCR device: straight into the centre.
        let src = [0.0f32, 0.0, 0.0, 0.8, 0.0, 0.0];
        assert_close(&mix_auto(&src, 3), &[0.0, 0.0, 0.8]);
        // Onto a 5.1 device it keeps its own output.
        assert_close(&mix_auto(&src, 6), &[0.0, 0.0, 0.0, 0.8, 0.0, 0.0]);
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
        // Every 5.1 channel reaches the pair, the LFE included.
        assert_eq!(matrix.used_sources(), &[0, 1, 2, 3, 4, 5]);
        // A 7.1.4 source on a 7.1.4 device reads all twelve; on stereo too.
        let matrix = ChannelMixMatrix::build(12, 2, ChannelMapMode::Auto);
        assert_eq!(matrix.used_sources().len(), 12);
    }

    #[test]
    fn zero_channel_counts_are_clamped_instead_of_panicking() {
        let matrix = ChannelMixMatrix::build(0, 0, ChannelMapMode::Auto);
        assert_eq!(matrix.row(0), &[(0, 1.0)]);
        assert!(matrix.row(1).is_empty());
    }

    #[test]
    fn a_7_1_4_channel_mask_names_the_standard_layout() {
        let layout = layout_from_mask(0x2D63F, 12).expect("a 7.1.4 mask");
        assert_eq!(layout, LAYOUT_12.to_vec());
        assert_eq!(layout_from_mask(0x2D63F, 10), None, "bits that do not number the channels");
        assert_eq!(layout_from_mask(0, 12), None, "no mask at all");
        assert_eq!(layout_from_mask(0x4_0000, 1), None, "a bit the format reserves");
    }

    #[test]
    fn surround_names_and_angles_follow_the_pairs_the_layout_has() {
        let five_one = standard_layout_vec(6).expect("5.1");
        assert_eq!(Bl.label(&five_one), "Ls");
        assert_eq!(Bl.direction(&five_one), (-110.0, 0.0));
        let seven_one = standard_layout_vec(8).expect("7.1");
        assert_eq!((Bl.label(&seven_one), Sl.label(&seven_one)), ("Lrs", "Lss"));
        assert_eq!((Bl.direction(&seven_one).0, Sl.direction(&seven_one).0), (-135.0, -90.0));
        assert!(Tfl.is_height() && !Fl.is_height() && Lfe.is_lfe());
        for pos in SpeakerPos::ALL {
            assert_eq!(SpeakerPos::from_key(pos.key()), Some(pos));
        }
    }

    #[test]
    fn a_film_order_file_reaches_the_right_speakers() {
        // L C R Ls Rs LFE onto a WAV-order 5.1 device: L R C LFE Ls Rs.
        let film: Layout = presets_for(6)
            .find(|p| p.name.contains("Film"))
            .expect("a Film preset")
            .speakers
            .iter()
            .copied()
            .map(Some)
            .collect();
        let matrix = ChannelMixMatrix::build_with_layouts(6, Some(&film), 6, None, ChannelMapMode::Auto);
        let src = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let out: Vec<f32> = (0..6).map(|c| matrix.mix(c, &src)).collect();
        assert_close(&out, &[0.1, 0.3, 0.2, 0.6, 0.4, 0.5]);
    }

    #[test]
    fn a_channel_on_no_speaker_is_silent_and_outputs_follow_their_own_layout() {
        let src: Layout = vec![Some(Fl), None];
        let out: Layout = vec![None, Some(Fl), Some(Fr)];
        let matrix = ChannelMixMatrix::build_with_layouts(2, Some(&src), 3, Some(&out), ChannelMapMode::Auto);
        let mixed: Vec<f32> = (0..3).map(|c| matrix.mix(c, &[0.5, 0.9])).collect();
        assert_close(&mixed, &[0.0, 0.5, 0.0]);
        // A layout of the wrong length is ignored for the standard one.
        let wrong: Layout = vec![Some(Fr)];
        let matrix = ChannelMixMatrix::build_with_layouts(2, Some(&wrong), 2, None, ChannelMapMode::Auto);
        assert_close(&[matrix.mix(0, &[0.25, 0.75]), matrix.mix(1, &[0.25, 0.75])], &[0.25, 0.75]);
    }

    #[test]
    fn presets_are_sized_and_named_once_per_count() {
        assert!(presets_for(12).count() >= 2);
        assert!(presets_for(16).any(|p| p.name == "9.1.6"));
        for preset in PRESETS {
            assert_eq!(
                PRESETS.iter().filter(|p| p.name == preset.name).count(),
                1,
                "{}",
                preset.name
            );
        }
    }

    #[test]
    fn channel_map_mode_round_trips_through_its_wire_encoding() {
        for mode in [ChannelMapMode::Auto, ChannelMapMode::Direct] {
            assert_eq!(ChannelMapMode::from_u8(mode.to_u8()), mode);
        }
        assert_eq!(ChannelMapMode::from_u8(200), ChannelMapMode::Auto);
    }
}
