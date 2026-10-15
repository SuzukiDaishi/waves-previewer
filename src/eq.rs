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

use crate::automation::{Automatable, Evaluator, Scale, Tracks, AUTOMATION_BLOCK};

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

    /// Whether the band has a Q: every shape. A cut's is its resonance
    /// (`EqBand::cut_q`), flat at 0.707.
    pub fn uses_q(self) -> bool {
        true
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
    /// A cut's resonance: 0.707 is the flat Butterworth corner, more is a
    /// peak just before the corner. Its own field rather than `q`, because
    /// a cut saved before cuts had a Q still carries the bell's `q` it was
    /// made from, and reading that as a resonance would change its sound.
    pub cut_q: f32,
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
            cut_q: BUTTERWORTH_Q as f32,
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

    /// The Q this band's shape uses: `cut_q` for a cut, `q` otherwise.
    pub fn q_value(&self) -> f32 {
        if self.kind.uses_slope() {
            self.cut_q
        } else {
            self.q
        }
    }

    /// Set the Q this band's shape uses, kept in its range.
    pub fn set_q(&mut self, q: f32) {
        if self.kind.uses_slope() {
            self.cut_q = q.clamp(
                *ParametricEqParams::CUT_Q.start(),
                *ParametricEqParams::CUT_Q.end(),
            );
        } else {
            self.q = q.clamp(*ParametricEqParams::Q.start(), *ParametricEqParams::Q.end());
        }
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
            EqKind::LowCut => cut_qs(self.slope.order(), self.cut_q)
                .map(|q| Section::design(SectionKind::HighPass, f, 0.0, q, sr))
                .collect(),
            EqKind::HighCut => cut_qs(self.slope.order(), self.cut_q)
                .map(|q| Section::design(SectionKind::LowPass, f, 0.0, q, sr))
                .collect(),
        }
    }
}

/// The Q of a second-order Butterworth section: the flat corner.
pub const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// The Q of each second-order section of an `order`-th order Butterworth
/// filter (`order` even).
fn butterworth_qs(order: usize) -> impl Iterator<Item = f64> {
    let n = order.max(2) as f64;
    (0..order.max(2) / 2).map(move |k| {
        let angle = std::f64::consts::PI * (2.0 * k as f64 + 1.0) / (2.0 * n);
        1.0 / (2.0 * angle.cos())
    })
}

/// A cut's sections with resonance `cut_q`: the Butterworth cascade with
/// its sharpest section (the last, nearest the corner) scaled by how far
/// `cut_q` is from flat. At 0.707 this is exactly the Butterworth cascade,
/// so a cut that has never been given a Q sounds as it always did; above it
/// the corner peaks, below it the corner softens, and the slope past the
/// corner is the slope either way.
fn cut_qs(order: usize, cut_q: f32) -> impl Iterator<Item = f64> {
    let sections = order.max(2) / 2;
    let q = f64::from(cut_q).max(0.05);
    // An f32 cannot hold 1/sqrt(2): the flat default is snapped to exactly
    // flat, so its sections are bit for bit the Butterworth ones.
    let scale = if (q - BUTTERWORTH_Q).abs() < 1e-6 {
        1.0
    } else {
        q / BUTTERWORTH_Q
    };
    butterworth_qs(order)
        .enumerate()
        .map(move |(k, q)| if k + 1 == sections { q * scale } else { q })
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
    /// A cut's resonance: from a soft, early roll-off to a corner peaking
    /// about 20 dB at 12 dB/oct. 0.707 is flat.
    pub const CUT_Q: std::ops::RangeInclusive<f32> = 0.4..=10.0;
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

    /// `lowest_active_hz`, counting where an automated band's corner goes
    /// and the bands only automation makes active.
    pub fn lowest_active_hz_automated(&self, tracks: &Tracks<EqParam>) -> Option<f32> {
        let swept = tracks.tracks.iter().filter_map(|(param, curve)| match param {
            EqParam::Freq(b) if self.bands.get(*b as usize).is_some_and(EqBand::is_heard) => {
                curve.min_value()
            }
            _ => None,
        });
        let automated = (0..EQ_MAX_BANDS)
            .filter(|b| is_band_automated(tracks, *b) && self.bands[*b].is_heard())
            .map(|b| self.bands[b].freq_hz);
        self.lowest_active_hz()
            .into_iter()
            .chain(swept)
            .chain(automated)
            .reduce(f32::min)
    }
}

/// An EQ setting that can follow a curve: a band's corner, gain or Q (by
/// slot), or the output gain. Not a band's shape, slope, mode or bypass
/// (switches).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EqParam {
    Freq(u8),
    Gain(u8),
    Q(u8),
    Output,
}

impl EqParam {
    /// The band slot it belongs to, if any.
    pub fn band(self) -> Option<usize> {
        match self {
            EqParam::Freq(b) | EqParam::Gain(b) | EqParam::Q(b) => Some(b as usize),
            EqParam::Output => None,
        }
    }

    /// Frequencies and Qs move by ratio, gains by dB.
    pub fn scale(self) -> Scale {
        match self {
            EqParam::Freq(_) | EqParam::Q(_) => Scale::Log,
            EqParam::Gain(_) | EqParam::Output => Scale::Linear,
        }
    }

    /// The range of the value, for a band of `kind` (a cut's Q is its
    /// resonance, with a range of its own).
    pub fn range(self, kind: EqKind) -> std::ops::RangeInclusive<f32> {
        type P = ParametricEqParams;
        match self {
            EqParam::Freq(_) => P::FREQ_HZ,
            EqParam::Gain(_) => P::GAIN_DB,
            EqParam::Q(_) if kind.uses_slope() => P::CUT_Q,
            EqParam::Q(_) => P::Q,
            EqParam::Output => P::OUTPUT_DB,
        }
    }
}

fn is_band_automated(tracks: &Tracks<EqParam>, band: usize) -> bool {
    tracks.params().any(|p| p.band() == Some(band))
}

impl Automatable for ParametricEqParams {
    type Param = EqParam;

    fn set_param(&mut self, param: EqParam, value: f32) {
        let Some(slot) = param.band() else {
            let r = EqParam::Output.range(EqKind::Bell);
            self.output_db = value.max(*r.start()).min(*r.end());
            return;
        };
        let Some(band) = self.bands.get_mut(slot) else {
            return;
        };
        let r = param.range(band.kind);
        let v = value.max(*r.start()).min(*r.end());
        match param {
            EqParam::Freq(_) => band.freq_hz = v,
            EqParam::Gain(_) => band.gain_db = v,
            EqParam::Q(_) => band.set_q(v),
            EqParam::Output => {}
        }
    }

    fn param(&self, param: EqParam) -> f32 {
        match param {
            EqParam::Output => self.output_db,
            EqParam::Freq(b) => self.bands.get(b as usize).map_or(0.0, |x| x.freq_hz),
            EqParam::Gain(b) => self.bands.get(b as usize).map_or(0.0, |x| x.gain_db),
            EqParam::Q(b) => self.bands.get(b as usize).map_or(0.0, EqBand::q_value),
        }
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

/// Run the EQ over `channels` with `tracks` moving its settings over time:
/// an automated band's sections are designed afresh every
/// `AUTOMATION_BLOCK` samples, keeping each filter's state, and the output
/// gain follows its curve. `offset` is the buffer sample `channels` start
/// at. With no tracks this is exactly [`process`].
pub fn process_automated(
    channels: &mut [Vec<f32>],
    sample_rate: u32,
    params: &ParametricEqParams,
    tracks: &Tracks<EqParam>,
    offset: usize,
) {
    if tracks.is_empty() {
        process(channels, sample_rate, params);
        return;
    }
    // A band only automation lifts off zero still has to be run.
    let active: Vec<usize> = (0..EQ_MAX_BANDS)
        .filter(|&b| {
            let band = &params.bands[b];
            band.is_heard() && (!band.is_neutral() || is_band_automated(tracks, b))
        })
        .collect();
    let run_view = |view: EqChannelMode, buf: &mut [f32]| {
        let bands: Vec<usize> = active
            .iter()
            .copied()
            .filter(|&b| params.bands[b].acts_on(view))
            .collect();
        let mut filters: Vec<Vec<SectionFilter>> = bands
            .iter()
            .map(|&b| {
                params.bands[b]
                    .sections(sample_rate)
                    .into_iter()
                    .map(SectionFilter::new)
                    .collect()
            })
            .collect();
        let mut eval = Evaluator::new(*params, tracks);
        let mut start = 0;
        while start < buf.len() {
            let end = (start + AUTOMATION_BLOCK).min(buf.len());
            let now = eval.at(offset + start);
            for (filters, &b) in filters.iter_mut().zip(&bands) {
                if is_band_automated(tracks, b) {
                    for (filter, section) in filters.iter_mut().zip(now.bands[b].sections(sample_rate)) {
                        filter.set_section(section);
                    }
                }
            }
            for v in &mut buf[start..end] {
                let mut x = f64::from(*v);
                for filter in filters.iter_mut().flatten() {
                    x = filter.tick(x);
                }
                *v = x as f32;
            }
            start = end;
        }
    };
    // The output gain, a block at a time.
    let apply_output = |buf: &mut [f32]| {
        let mut eval = Evaluator::new(*params, tracks);
        for (block, chunk) in buf.chunks_mut(AUTOMATION_BLOCK).enumerate() {
            let db = eval.at(offset + block * AUTOMATION_BLOCK).output_db;
            if db != 0.0 {
                let g = 10f32.powf(db / 20.0);
                chunk.iter_mut().for_each(|v| *v *= g);
            }
        }
    };
    if channels.len() == 2 && params.has_mid_side() {
        let len = channels[0].len().min(channels[1].len());
        let (mut mid, mut side): (Vec<f32>, Vec<f32>) = (0..len)
            .map(|i| {
                let (l, r) = (channels[0][i], channels[1][i]);
                ((l + r) * 0.5, (l - r) * 0.5)
            })
            .unzip();
        run_view(EqChannelMode::Mid, &mut mid);
        run_view(EqChannelMode::Side, &mut side);
        for i in 0..len {
            channels[0][i] = mid[i] + side[i];
            channels[1][i] = mid[i] - side[i];
        }
    } else {
        for ch in channels.iter_mut() {
            run_view(EqChannelMode::Mid, ch);
        }
    }
    for ch in channels.iter_mut() {
        apply_output(ch);
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

    /// New coefficients, the state kept: a filter whose settings move
    /// carries on from where it was instead of starting from silence.
    pub fn set_section(&mut self, section: Section) {
        self.section = section;
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
    fn a_cut_with_the_flat_q_is_the_butterworth_cut_it_always_was() {
        for slope in CutSlope::ALL {
            for kind in [EqKind::LowCut, EqKind::HighCut] {
                let band = EqBand {
                    enabled: true,
                    kind,
                    freq_hz: 700.0,
                    slope,
                    ..EqBand::default()
                };
                let section_kind = if kind == EqKind::LowCut {
                    SectionKind::HighPass
                } else {
                    SectionKind::LowPass
                };
                let butterworth: Vec<Section> = butterworth_qs(slope.order())
                    .map(|q| Section::design(section_kind, 700.0, 0.0, q, f64::from(SR)))
                    .collect();
                assert_eq!(band.sections(SR), butterworth, "{kind:?} {slope:?}");
            }
        }
    }

    #[test]
    fn a_resonant_cut_peaks_at_its_corner_and_keeps_its_slope() {
        for slope in CutSlope::ALL {
            let flat = one_band(EqBand {
                kind: EqKind::HighCut,
                freq_hz: 2_000.0,
                slope,
                ..EqBand::default()
            });
            let mut peaked = flat;
            peaked.bands[0].cut_q = 4.0;
            // Just under the corner of a high cut: flat is under 0 dB, the
            // resonant one rises well above it.
            let near = 1_900.0;
            let flat_db = response_db(&flat, EqChannelMode::Stereo, SR, near);
            let peaked_db = response_db(&peaked, EqChannelMode::Stereo, SR, near);
            assert!(flat_db < 0.0, "{slope:?}: {flat_db}");
            assert!(peaked_db > 6.0, "{slope:?}: {peaked_db}");
            // Far past the corner the slope is the slope: the two differ by
            // what the resonant section adds there, a fraction of a dB.
            let far = 16_000.0;
            let a = response_db(&flat, EqChannelMode::Stereo, SR, far);
            let b = response_db(&peaked, EqChannelMode::Stereo, SR, far);
            assert!((a - b).abs() < 3.0, "{slope:?}: {a} vs {b}");
            // And the processed audio follows the drawn curve.
            let heard = measured_gain_db(&peaked, near);
            assert!((heard - peaked_db).abs() < 0.5, "{slope:?}: heard {heard}, drawn {peaked_db}");
        }
        // A low Q softens the corner.
        let mut soft = one_band(EqBand {
            kind: EqKind::LowCut,
            freq_hz: 1_000.0,
            ..EqBand::default()
        });
        soft.bands[0].cut_q = 0.4;
        assert!(response_db(&soft, EqChannelMode::Stereo, SR, 1_000.0) < -6.0);
    }

    #[test]
    fn q_value_reads_the_field_the_shape_uses() {
        let mut band = EqBand {
            enabled: true,
            kind: EqKind::LowCut,
            ..EqBand::default()
        };
        band.set_q(3.0);
        assert_eq!(band.cut_q, 3.0);
        assert_eq!(band.q, 1.0, "a cut leaves the bell's q alone");
        assert_eq!(band.q_value(), 3.0);
        band.set_q(100.0);
        assert_eq!(band.q_value(), *ParametricEqParams::CUT_Q.end());
        band.kind = EqKind::Bell;
        assert_eq!(band.q_value(), 1.0);
    }

    fn curve(scale: Scale, points: &[(usize, f32)]) -> crate::automation::Curve {
        crate::automation::Curve::new(points.to_vec(), scale)
    }

    #[test]
    fn an_automated_gain_raises_only_the_second_half() {
        // A bell at 0 dB is neutral on its own: only its curve makes it act.
        let p = one_band(EqBand {
            kind: EqKind::Bell,
            freq_hz: 1_000.0,
            gain_db: 0.0,
            ..EqBand::default()
        });
        assert!(p.is_neutral());
        let len = SR as usize * 2;
        let src = sine(1_000.0, len, 0.1);
        let mut tracks = Tracks::none();
        tracks.push(
            EqParam::Gain(0),
            curve(Scale::Linear, &[(len / 2 - 1, 0.0), (len / 2, 12.0)]),
        );
        let mut chans = vec![src.clone()];
        process_automated(&mut chans, SR, &p, &tracks, 0);
        let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        let q = len / 4;
        let first = 20.0 * (rms(&chans[0][q..2 * q]) / rms(&src[q..2 * q])).log10();
        let second = 20.0 * (rms(&chans[0][3 * q..]) / rms(&src[3 * q..])).log10();
        assert!(first.abs() < 0.2, "{first}");
        assert!((second - 12.0).abs() < 0.3, "{second}");
    }

    #[test]
    fn a_swept_cut_stays_stable_and_ends_where_it_was_sent() {
        let p = one_band(EqBand {
            kind: EqKind::HighCut,
            freq_hz: 18_000.0,
            slope: CutSlope::Db48,
            cut_q: 3.0,
            ..EqBand::default()
        });
        let len = SR as usize * 2;
        // Noise-like: a sum of tones across the band.
        let src: Vec<f32> = (0..len)
            .map(|i| {
                [110.0f32, 830.0, 3_100.0, 9_700.0]
                    .iter()
                    .map(|f| (i as f32 / SR as f32 * f * std::f32::consts::TAU).sin() * 0.2)
                    .sum()
            })
            .collect();
        let mut tracks = Tracks::none();
        tracks.push(
            EqParam::Freq(0),
            curve(Scale::Log, &[(0, 18_000.0), (len * 3 / 4, 300.0)]),
        );
        let mut chans = vec![src.clone()];
        process_automated(&mut chans, SR, &p, &tracks, 0);
        assert!(chans[0].iter().all(|v| v.is_finite() && v.abs() < 8.0));
        // At the end the cut is at 300 Hz: the 9.7 kHz tone is gone.
        let tail = &chans[0][len * 7 / 8..];
        let mut swept_to = p;
        swept_to.bands[0].freq_hz = 300.0;
        let mut still = vec![src[len * 7 / 8..].to_vec()];
        process(&mut still, SR, &swept_to);
        let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        let a = rms(&tail[SR as usize / 10..]);
        let b = rms(&still[0][SR as usize / 10..]);
        assert!((20.0 * (a / b).log10()).abs() < 0.5, "{a} vs {b}");
        assert_eq!(p.lowest_active_hz_automated(&tracks), Some(300.0));
    }

    #[test]
    fn an_empty_track_set_is_the_plain_eq() {
        let mut p = ParametricEqParams::default();
        p.bands[1].gain_db = 6.0;
        p.output_db = -2.0;
        let src = sine(700.0, 9_600, 0.3);
        let mut plain = vec![src.clone(), src.clone()];
        process(&mut plain, SR, &p);
        let mut auto = vec![src.clone(), src];
        process_automated(&mut auto, SR, &p, &Tracks::none(), 0);
        assert_eq!(plain, auto);
    }

    #[test]
    fn eq_params_reach_every_band_field() {
        let mut p = ParametricEqParams::default();
        p.set_param(EqParam::Freq(2), 5_000.0);
        p.set_param(EqParam::Gain(2), 99.0);
        p.set_param(EqParam::Q(2), 2.0);
        p.set_param(EqParam::Output, -3.0);
        assert_eq!(p.bands[2].freq_hz, 5_000.0);
        assert_eq!(p.bands[2].gain_db, *ParametricEqParams::GAIN_DB.end());
        assert_eq!(p.param(EqParam::Q(2)), 2.0);
        assert_eq!(p.output_db, -3.0);
        p.bands[3].kind = EqKind::LowCut;
        p.set_param(EqParam::Q(3), 4.0);
        assert_eq!(p.bands[3].cut_q, 4.0);
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
        // A cut saved before cuts had a Q stays the flat Butterworth cut.
        let old_cut: EqBand =
            serde_json::from_str(r#"{"enabled":true,"kind":"low_cut","freq_hz":80.0,"q":1.0}"#)
                .unwrap();
        assert_eq!(old_cut.cut_q as f64, BUTTERWORTH_Q as f32 as f64);
    }
}
