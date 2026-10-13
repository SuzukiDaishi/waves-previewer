//! The editor's parametric EQ, DSP and rules only: up to eight bands, each a
//! bell, a shelf, a cut (12-48 dB/oct) or a notch, working on the stereo
//! pair or on its mid or its side. `tool_render` runs it; the Inspector's EQ
//! tool draws its response with [`response_db`], from the very sections
//! [`process`] runs, so the curve on screen is the filter that is applied.
//!
//! Sections are designed and run in f64. A 20 Hz low cut at 96 kHz puts its
//! poles so close to the unit circle that f32 coefficients ring and drift --
//! which is also why this does not share `wave::Biquad`, the f32 filter of
//! the Effect Graph's fixed three-band EQ.

use serde::{Deserialize, Serialize};

/// How many bands one EQ holds.
pub const EQ_MAX_BANDS: usize = 8;

/// A gain this small is no gain: below what any display rounds to.
const NEUTRAL_DB: f32 = 0.01;

/// Highest corner a section is designed at, as a fraction of the rate: the
/// RBJ designs fall apart at Nyquist.
const MAX_CORNER_FRACTION: f64 = 0.49;

/// The shape of one band.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqKind {
    #[default]
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
}

impl EqKind {
    pub const ALL: [EqKind; 6] = [
        EqKind::Bell,
        EqKind::LowShelf,
        EqKind::HighShelf,
        EqKind::LowCut,
        EqKind::HighCut,
        EqKind::Notch,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EqKind::Bell => "Bell",
            EqKind::LowShelf => "Low Shelf",
            EqKind::HighShelf => "High Shelf",
            EqKind::LowCut => "Low Cut",
            EqKind::HighCut => "High Cut",
            EqKind::Notch => "Notch",
        }
    }

    /// Whether the band's gain means anything (a cut or a notch has none).
    pub fn uses_gain(self) -> bool {
        matches!(self, EqKind::Bell | EqKind::LowShelf | EqKind::HighShelf)
    }

    /// Whether the band's Q means anything (a cut is Butterworth).
    pub fn uses_q(self) -> bool {
        !self.uses_slope()
    }

    /// Whether the band has a slope (the cuts).
    pub fn uses_slope(self) -> bool {
        matches!(self, EqKind::LowCut | EqKind::HighCut)
    }
}

/// How steeply a cut falls away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutSlope {
    Db12,
    #[default]
    Db24,
    Db36,
    Db48,
}

impl CutSlope {
    pub const ALL: [CutSlope; 4] = [
        CutSlope::Db12,
        CutSlope::Db24,
        CutSlope::Db36,
        CutSlope::Db48,
    ];

    /// Filter order: 6 dB/oct per order.
    pub fn order(self) -> usize {
        match self {
            CutSlope::Db12 => 2,
            CutSlope::Db24 => 4,
            CutSlope::Db36 => 6,
            CutSlope::Db48 => 8,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CutSlope::Db12 => "12 dB/oct",
            CutSlope::Db24 => "24 dB/oct",
            CutSlope::Db36 => "36 dB/oct",
            CutSlope::Db48 => "48 dB/oct",
        }
    }
}

/// Which part of a stereo pair a band works on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqChannelMode {
    #[default]
    Stereo,
    Mid,
    Side,
}

impl EqChannelMode {
    pub const ALL: [EqChannelMode; 3] = [
        EqChannelMode::Stereo,
        EqChannelMode::Mid,
        EqChannelMode::Side,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EqChannelMode::Stereo => "Stereo",
            EqChannelMode::Mid => "Mid",
            EqChannelMode::Side => "Side",
        }
    }
}

/// One band of the EQ.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqBand {
    /// The slot is in use (a band the user made). `false` is a free slot.
    pub enabled: bool,
    /// In use but switched off for now: kept, drawn hollow, not heard.
    pub bypassed: bool,
    pub kind: EqKind,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
    pub slope: CutSlope,
    pub mode: EqChannelMode,
}

impl Default for EqBand {
    /// An unused slot: a flat bell at 1 kHz, switched off.
    fn default() -> Self {
        Self {
            enabled: false,
            bypassed: false,
            kind: EqKind::Bell,
            freq_hz: 1_000.0,
            gain_db: 0.0,
            q: 1.0,
            slope: CutSlope::Db24,
            mode: EqChannelMode::Stereo,
        }
    }
}

impl EqBand {
    /// A band that changes nothing: unused, bypassed, or a bell or shelf at 0 dB.
    pub fn is_neutral(&self) -> bool {
        !self.is_heard() || (self.kind.uses_gain() && self.gain_db.abs() < NEUTRAL_DB)
    }

    /// In use and not bypassed.
    pub fn is_heard(&self) -> bool {
        self.enabled && !self.bypassed
    }

    /// Whether this band acts on `view`'s signal: a stereo band acts on all.
    pub fn acts_on(&self, view: EqChannelMode) -> bool {
        self.mode == EqChannelMode::Stereo || self.mode == view
    }

    /// The sections that make this band at `sample_rate`.
    pub fn sections(&self, sample_rate: u32) -> Vec<Section> {
        let sr = f64::from(sample_rate.max(1));
        let f = f64::from(self.freq_hz).clamp(1.0, sr * MAX_CORNER_FRACTION);
        let g = f64::from(self.gain_db);
        let q = f64::from(self.q).max(0.05);
        match self.kind {
            EqKind::Bell => vec![Section::design(SectionKind::Peak, f, g, q, sr)],
            EqKind::LowShelf => vec![Section::design(SectionKind::LowShelf, f, g, q, sr)],
            EqKind::HighShelf => vec![Section::design(SectionKind::HighShelf, f, g, q, sr)],
            EqKind::Notch => vec![Section::design(SectionKind::Notch, f, 0.0, q, sr)],
            EqKind::LowCut => butterworth_qs(self.slope.order())
                .map(|q| Section::design(SectionKind::HighPass, f, 0.0, q, sr))
                .collect(),
            EqKind::HighCut => butterworth_qs(self.slope.order())
                .map(|q| Section::design(SectionKind::LowPass, f, 0.0, q, sr))
                .collect(),
        }
    }
}

/// The Q of each second-order section of an `order`-th order Butterworth
/// filter (`order` even).
fn butterworth_qs(order: usize) -> impl Iterator<Item = f64> {
    let n = order.max(2) as f64;
    (0..order.max(2) / 2).map(move |k| {
        let angle = std::f64::consts::PI * (2.0 * k as f64 + 1.0) / (2.0 * n);
        1.0 / (2.0 * angle.cos())
    })
}

/// The whole EQ.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParametricEqParams {
    pub bands: [EqBand; EQ_MAX_BANDS],
    /// Gain after every band.
    pub output_db: f32,
}

impl ParametricEqParams {
    /// Band corners: the audible range.
    pub const FREQ_HZ: std::ops::RangeInclusive<f32> =
        crate::sample_rate::AUDIBLE_LOW_HZ..=crate::sample_rate::AUDIBLE_HIGH_HZ;
    /// Largest boost or cut of a band.
    pub const GAIN_DB: std::ops::RangeInclusive<f32> = -24.0..=24.0;
    /// From a broad tilt to a surgical notch.
    pub const Q: std::ops::RangeInclusive<f32> = 0.1..=18.0;
    pub const OUTPUT_DB: std::ops::RangeInclusive<f32> = -24.0..=24.0;

    /// Changes nothing: every band neutral and no output gain.
    pub fn is_neutral(&self) -> bool {
        self.bands.iter().all(EqBand::is_neutral) && self.output_db.abs() < NEUTRAL_DB
    }

    /// Whether any band works on the mid or the side alone.
    pub fn has_mid_side(&self) -> bool {
        self.bands
            .iter()
            .any(|b| b.is_heard() && b.mode != EqChannelMode::Stereo)
    }

    /// The lowest corner of a band that does something: how far a filter
    /// rings, and so how much audio a range needs on either side.
    pub fn lowest_active_hz(&self) -> Option<f32> {
        self.bands
            .iter()
            .filter(|b| !b.is_neutral())
            .map(|b| b.freq_hz)
            .reduce(f32::min)
    }

    /// The first unused slot.
    pub fn free_slot(&self) -> Option<usize> {
        self.bands.iter().position(|b| !b.enabled)
    }
}

impl Default for ParametricEqParams {
    /// Flat, with three bands where a console has them -- a low shelf under
    /// the body of a voice, a bell at 1 kHz, a high shelf at the start of
    /// "air" -- so there is something to grab.
    fn default() -> Self {
        let mut bands = [EqBand::default(); EQ_MAX_BANDS];
        bands[0] = EqBand {
            enabled: true,
            kind: EqKind::LowShelf,
            freq_hz: 120.0,
            q: std::f32::consts::FRAC_1_SQRT_2,
            ..EqBand::default()
        };
        bands[1] = EqBand {
            enabled: true,
            ..EqBand::default()
        };
        bands[2] = EqBand {
            enabled: true,
            kind: EqKind::HighShelf,
            freq_hz: 8_000.0,
            q: std::f32::consts::FRAC_1_SQRT_2,
            ..EqBand::default()
        };
        Self {
            bands,
            output_db: 0.0,
        }
    }
}

/// One band's magnitude response at `freq_hz`, in dB.
pub fn band_response_db(band: &EqBand, sample_rate: u32, freq_hz: f32) -> f32 {
    if !band.is_heard() {
        return 0.0;
    }
    let w = angular(sample_rate, freq_hz);
    band.sections(sample_rate)
        .iter()
        .map(|s| s.magnitude_db(w))
        .sum::<f64>() as f32
}

/// The EQ's response on `view`'s signal at `freq_hz`, in dB, output gain
/// included. The Stereo view is what a band set to Stereo does to both
/// channels; Mid and Side add the bands set to them.
pub fn response_db(
    params: &ParametricEqParams,
    view: EqChannelMode,
    sample_rate: u32,
    freq_hz: f32,
) -> f32 {
    let bands: f32 = params
        .bands
        .iter()
        .filter(|b| b.is_heard() && b.acts_on(view))
        .map(|b| band_response_db(b, sample_rate, freq_hz))
        .sum();
    bands + params.output_db
}

fn angular(sample_rate: u32, freq_hz: f32) -> f64 {
    let sr = f64::from(sample_rate.max(1));
    2.0 * std::f64::consts::PI * (f64::from(freq_hz).clamp(1.0, sr * 0.499) / sr)
}

/// Run the EQ over `channels` (the ones the edit is in scope for), in place.
/// A stereo pair is processed as mid and side, so a band can work on either;
/// any other layout has no side to speak of, and a Mid band then works on
/// every channel while a Side band does nothing.
pub fn process(channels: &mut [Vec<f32>], sample_rate: u32, params: &ParametricEqParams) {
    let active: Vec<&EqBand> = params.bands.iter().filter(|b| !b.is_neutral()).collect();
    let out_gain = 10f32.powf(params.output_db / 20.0);
    let chain = |view: EqChannelMode| -> Vec<Section> {
        active
            .iter()
            .filter(|b| b.acts_on(view))
            .flat_map(|b| b.sections(sample_rate))
            .collect()
    };
    if channels.len() == 2 && params.has_mid_side() {
        // Linear filters commute, and the same filter on L and R is the same
        // filter on M and S: so the stereo bands go on both, the rest on one.
        let len = channels[0].len().min(channels[1].len());
        let (mut mid, mut side): (Vec<f32>, Vec<f32>) = (0..len)
            .map(|i| {
                let (l, r) = (channels[0][i], channels[1][i]);
                ((l + r) * 0.5, (l - r) * 0.5)
            })
            .unzip();
        run(&chain(EqChannelMode::Mid), &mut mid);
        run(&chain(EqChannelMode::Side), &mut side);
        for i in 0..len {
            channels[0][i] = (mid[i] + side[i]) * out_gain;
            channels[1][i] = (mid[i] - side[i]) * out_gain;
        }
        return;
    }
    let sections = chain(EqChannelMode::Mid);
    for ch in channels.iter_mut() {
        run(&sections, ch);
        if out_gain != 1.0 {
            ch.iter_mut().for_each(|v| *v *= out_gain);
        }
    }
}

/// Run a cascade of sections over `buf` in place.
pub fn run(sections: &[Section], buf: &mut [f32]) {
    for section in sections {
        let mut filter = SectionFilter::new(*section);
        for v in buf.iter_mut() {
            *v = filter.tick(f64::from(*v)) as f32;
        }
    }
}

/// The RBJ cookbook shapes a section can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
}

/// A second-order section, normalised (`a0 = 1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Section {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Section {
    /// RBJ Audio EQ Cookbook coefficients. `gain_db` is used by the peak and
    /// the shelves only.
    pub fn design(kind: SectionKind, freq_hz: f64, gain_db: f64, q: f64, sample_rate: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq_hz / sample_rate.max(1.0);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q.max(1e-3));
        let (b0, b1, b2, a0, a1, a2) = match kind {
            SectionKind::Peak => (
                1.0 + alpha * a,
                -2.0 * cos_w0,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos_w0,
                1.0 - alpha / a,
            ),
            SectionKind::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos_w0 + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
                    a * ((a + 1.0) - (a - 1.0) * cos_w0 - k),
                    (a + 1.0) + (a - 1.0) * cos_w0 + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
                    (a + 1.0) + (a - 1.0) * cos_w0 - k,
                )
            }
            SectionKind::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos_w0 + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
                    a * ((a + 1.0) + (a - 1.0) * cos_w0 - k),
                    (a + 1.0) - (a - 1.0) * cos_w0 + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
                    (a + 1.0) - (a - 1.0) * cos_w0 - k,
                )
            }
            SectionKind::LowPass => (
                (1.0 - cos_w0) * 0.5,
                1.0 - cos_w0,
                (1.0 - cos_w0) * 0.5,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            SectionKind::HighPass => (
                (1.0 + cos_w0) * 0.5,
                -(1.0 + cos_w0),
                (1.0 + cos_w0) * 0.5,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
            SectionKind::Notch => (
                1.0,
                -2.0 * cos_w0,
                1.0,
                1.0 + alpha,
                -2.0 * cos_w0,
                1.0 - alpha,
            ),
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// |H(e^jw)| in dB at normalised angular frequency `w`.
    pub fn magnitude_db(&self, w: f64) -> f64 {
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -(self.b1 * s1 + self.b2 * s2);
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -(self.a1 * s1 + self.a2 * s2);
        let num = (num_re * num_re + num_im * num_im).max(1e-30);
        let den = (den_re * den_re + den_im * den_im).max(1e-30);
        10.0 * (num / den).log10()
    }
}

/// A section with its state: transposed direct form II, in f64.
#[derive(Clone, Copy, Debug)]
pub struct SectionFilter {
    section: Section,
    s1: f64,
    s2: f64,
}

impl SectionFilter {
    pub fn new(section: Section) -> Self {
        Self {
            section,
            s1: 0.0,
            s2: 0.0,
        }
    }

    #[inline]
    pub fn tick(&mut self, x: f64) -> f64 {
        let s = &self.section;
        let y = s.b0 * x + self.s1;
        self.s1 = s.b1 * x - s.a1 * y + self.s2;
        self.s2 = s.b2 * x - s.a2 * y;
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(freq: f32, len: usize, amp: f32) -> Vec<f32> {
        (0..len)
            .map(|i| (i as f32 / SR as f32 * freq * std::f32::consts::TAU).sin() * amp)
            .collect()
    }

    /// Steady-state level of a processed sine, in dB relative to its input.
    fn measured_gain_db(params: &ParametricEqParams, freq: f32) -> f32 {
        let src = sine(freq, SR as usize, 0.25);
        let mut chans = vec![src.clone()];
        process(&mut chans, SR, params);
        let tail = SR as usize / 2;
        let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        20.0 * (rms(&chans[0][tail..]) / rms(&src[tail..])).log10()
    }

    fn one_band(band: EqBand) -> ParametricEqParams {
        let mut p = ParametricEqParams {
            bands: [EqBand::default(); EQ_MAX_BANDS],
            output_db: 0.0,
        };
        p.bands[0] = EqBand {
            enabled: true,
            ..band
        };
        p
    }

    #[test]
    fn the_default_is_flat_but_has_three_bands_to_grab() {
        let p = ParametricEqParams::default();
        assert!(p.is_neutral());
        assert_eq!(p.bands.iter().filter(|b| b.enabled).count(), 3);
        let src = sine(440.0, 4_800, 0.5);
        let mut chans = vec![src.clone()];
        process(&mut chans, SR, &p);
        assert_eq!(chans[0], src, "neutral bands are skipped, not run");
    }

    #[test]
    fn a_bell_reaches_its_gain_at_its_centre() {
        let p = one_band(EqBand {
            kind: EqKind::Bell,
            freq_hz: 1_000.0,
            gain_db: 9.0,
            q: 1.0,
            ..EqBand::default()
        });
        assert!((response_db(&p, EqChannelMode::Stereo, SR, 1_000.0) - 9.0).abs() < 0.05);
        assert!((measured_gain_db(&p, 1_000.0) - 9.0).abs() < 0.2);
        assert!(measured_gain_db(&p, 100.0).abs() < 0.5);
    }

    #[test]
    fn a_24_db_cut_falls_48_db_two_octaves_down() {
        let p = one_band(EqBand {
            kind: EqKind::LowCut,
            freq_hz: 1_000.0,
            slope: CutSlope::Db24,
            ..EqBand::default()
        });
        let at_corner = response_db(&p, EqChannelMode::Stereo, SR, 1_000.0);
        assert!(
            (at_corner + 3.01).abs() < 0.1,
            "Butterworth corner is -3 dB: {at_corner}"
        );
        let two_down = response_db(&p, EqChannelMode::Stereo, SR, 250.0);
        assert!((two_down + 48.0).abs() < 1.0, "{two_down}");
        let measured = measured_gain_db(&p, 250.0);
        assert!(
            (measured - two_down).abs() < 1.0,
            "{measured} vs {two_down}"
        );
        // Steeper slopes fall faster.
        let mut steep = p;
        steep.bands[0].slope = CutSlope::Db48;
        assert!(response_db(&steep, EqChannelMode::Stereo, SR, 250.0) < two_down - 40.0);
    }

    #[test]
    fn a_notch_removes_its_frequency() {
        let p = one_band(EqBand {
            kind: EqKind::Notch,
            freq_hz: 1_000.0,
            q: 4.0,
            ..EqBand::default()
        });
        assert!(measured_gain_db(&p, 1_000.0) < -30.0);
        assert!(measured_gain_db(&p, 4_000.0).abs() < 0.3);
    }

    #[test]
    fn the_drawn_response_is_the_processed_one() {
        let mut p = ParametricEqParams::default();
        p.bands[0].gain_db = -6.0;
        p.bands[1].gain_db = 4.0;
        p.bands[2].gain_db = 3.0;
        p.output_db = -1.5;
        for freq in [60.0, 300.0, 1_000.0, 3_000.0, 12_000.0] {
            let drawn = response_db(&p, EqChannelMode::Stereo, SR, freq);
            let heard = measured_gain_db(&p, freq);
            assert!(
                (drawn - heard).abs() < 0.3,
                "{freq} Hz: drawn {drawn}, heard {heard}"
            );
        }
    }

    #[test]
    fn a_side_band_leaves_a_centred_signal_alone() {
        let p = one_band(EqBand {
            kind: EqKind::Bell,
            freq_hz: 440.0,
            gain_db: 12.0,
            mode: EqChannelMode::Side,
            ..EqBand::default()
        });
        let src = sine(440.0, 9_600, 0.3);
        let mut chans = vec![src.clone(), src.clone()];
        process(&mut chans, SR, &p);
        for (a, b) in chans[0].iter().zip(&src) {
            assert!((a - b).abs() < 1e-5);
        }
        // ...and works on what differs between the channels.
        let mut wide = vec![src.clone(), src.iter().map(|v| -v).collect()];
        process(&mut wide, SR, &p);
        let peak = wide[0][4_800..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.3 * 3.0, "side boosted: {peak}");
    }

    #[test]
    fn a_mid_band_on_mono_works_and_a_side_band_does_nothing() {
        let mut p = one_band(EqBand {
            kind: EqKind::Bell,
            freq_hz: 440.0,
            gain_db: 6.0,
            mode: EqChannelMode::Mid,
            ..EqBand::default()
        });
        assert!((measured_gain_db(&p, 440.0) - 6.0).abs() < 0.3);
        p.bands[0].mode = EqChannelMode::Side;
        assert!(measured_gain_db(&p, 440.0).abs() < 0.01);
    }

    #[test]
    fn a_bypassed_band_is_kept_but_not_heard() {
        let mut p = one_band(EqBand {
            kind: EqKind::Bell,
            freq_hz: 1_000.0,
            gain_db: 9.0,
            ..EqBand::default()
        });
        p.bands[0].bypassed = true;
        assert!(p.is_neutral());
        assert_eq!(p.free_slot(), Some(1), "the slot stays taken");
        assert!(response_db(&p, EqChannelMode::Stereo, SR, 1_000.0).abs() < 1e-6);
    }

    #[test]
    fn output_gain_applies_last() {
        let mut p = ParametricEqParams::default();
        p.output_db = -6.0;
        assert!(!p.is_neutral());
        assert!((measured_gain_db(&p, 1_000.0) + 6.0).abs() < 0.05);
    }

    #[test]
    fn a_session_round_trip_keeps_every_band() {
        let mut p = ParametricEqParams::default();
        p.bands[5] = EqBand {
            enabled: true,
            kind: EqKind::HighCut,
            freq_hz: 9_000.0,
            slope: CutSlope::Db48,
            mode: EqChannelMode::Side,
            ..EqBand::default()
        };
        let text = serde_json::to_string(&p).unwrap();
        let back: ParametricEqParams = serde_json::from_str(&text).unwrap();
        assert_eq!(back, p);
        // A document from before a field existed takes its default.
        let partial: EqBand = serde_json::from_str(r#"{"enabled":true,"freq_hz":200.0}"#).unwrap();
        assert_eq!(partial.kind, EqKind::Bell);
        assert_eq!(partial.freq_hz, 200.0);
    }
}
