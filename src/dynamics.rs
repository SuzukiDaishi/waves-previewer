//! The editor's compressor, limiter, clipper and gate, DSP and rules only. Each one
//! takes the channels an edit is in scope for together -- so a stereo link
//! can hold the image still -- and returns, beside the audio, a
//! [`DynamicsTrace`]: what came in and how much gain was taken, hop by hop.
//! The Inspector draws that trace, so the reduction on screen is the one the
//! edit applies.
//!
//! Lookahead costs no latency here: the whole buffer is in memory, so the
//! detector simply reads ahead of the sample whose gain it decides.
//!
//! The limiter and the clipper can run oversampled (`crate::oversample`):
//! the same code reads the buffer at `factor` times its rate and writes
//! through the filter back down. Off, it reads the buffer itself and renders
//! exactly what it always did.
//! Everything streams -- nothing allocates a buffer per sample beyond the
//! output itself, because an hour-long stereo file is 345 million samples.
//!
//! The Effect Graph's compressor and gate (`wave::process_compressor_offline`,
//! `wave::process_noise_gate_offline`) are separate and unchanged: a saved
//! graph must keep rendering what it rendered.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::automation::{Automatable, Evaluator, Scale, Tracks, AUTOMATION_BLOCK};
use crate::eq::{Section, SectionFilter, SectionKind};
use crate::oversample::{Oversampler, Oversampling, UpStream};

/// The trace's resolution: 5 ms is finer than any drawn pixel at the zoom a
/// whole selection is seen at.
const TRACE_HOP_SECS: f64 = 0.005;
/// At most this many trace points, however long the audio: a longer edit
/// gets a coarser hop rather than a trace that outgrows what it describes.
const TRACE_MAX_POINTS: usize = 65_536;
/// Samples between cancel checks.
const CANCEL_STRIDE: usize = 1 << 15;
/// The floor every level is measured against, in dBFS.
const LEVEL_FLOOR_DB: f32 = -120.0;
/// How long a gate's peak detector holds a peak: long enough that a low
/// tone's troughs do not close it, short enough to follow a word's end.
const GATE_PEAK_RELEASE_MS: f32 = 10.0;
/// Inter-sample peak estimation: taps either side of the point.
const TRUE_PEAK_HALF_TAPS: usize = 8;
/// Points estimated between two samples (4x oversampling).
const TRUE_PEAK_PHASES: [f64; 3] = [0.25, 0.5, 0.75];

/// How a detector measures level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detection {
    /// Every sample's magnitude: reacts to transients.
    #[default]
    Peak,
    /// Average power over a window: reacts to loudness.
    Rms,
}

impl Detection {
    pub fn label(self) -> &'static str {
        match self {
            Detection::Peak => "Peak",
            Detection::Rms => "RMS",
        }
    }
}

#[inline]
fn db_to_amp(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
fn amp_to_db(amp: f32) -> f32 {
    if amp <= 0.0 {
        LEVEL_FLOOR_DB
    } else {
        (20.0 * amp.log10()).max(LEVEL_FLOOR_DB)
    }
}

/// Per-sample one-pole coefficient for a time constant of `ms`.
fn coeff(ms: f32, sample_rate: u32) -> f32 {
    let samples = ms.max(0.001) * 0.001 * sample_rate.max(1) as f32;
    (-1.0 / samples.max(1e-3)).exp()
}

fn frames(ms: f32, sample_rate: u32) -> usize {
    crate::sample_rate::frames_for_secs(f64::from(ms.max(0.0)) / 1000.0, sample_rate)
}

// ---------------------------------------------------------------------------
// Trace

/// What a dynamics processor did, hop by hop: the loudest input sample of
/// each hop (dBFS) and the deepest gain change in it (dB, <= 0).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DynamicsTrace {
    /// Sample (in the processed buffer, or the tab once placed) of the first hop.
    pub start: usize,
    pub hop: usize,
    pub input_db: Vec<f32>,
    pub gain_db: Vec<f32>,
    /// Added on the way out (makeup, a limiter's input gain): the output of
    /// a hop is `input + gain + output_offset_db`.
    pub output_offset_db: f32,
}

impl DynamicsTrace {
    /// The hop length for `len` samples at `sample_rate`.
    pub fn hop_for(len: usize, sample_rate: u32) -> usize {
        crate::sample_rate::frames_for_secs(TRACE_HOP_SECS, sample_rate)
            .max(len.div_ceil(TRACE_MAX_POINTS))
            .max(1)
    }

    /// The part covering samples `[from, to)` of this trace.
    pub fn crop(&self, from: usize, to: usize) -> Self {
        let hop = self.hop.max(1);
        let a = (from.saturating_sub(self.start) / hop).min(self.input_db.len());
        let b = (to.saturating_sub(self.start).div_ceil(hop)).clamp(a, self.input_db.len());
        Self {
            start: self.start + a * hop,
            hop,
            input_db: self.input_db[a..b].to_vec(),
            gain_db: self.gain_db[a..b].to_vec(),
            output_offset_db: self.output_offset_db,
        }
    }

    /// The hop holding `sample`, if the trace covers it.
    pub fn index_at(&self, sample: usize) -> Option<usize> {
        let rel = sample.checked_sub(self.start)?;
        let idx = rel / self.hop.max(1);
        (idx < self.gain_db.len()).then_some(idx)
    }

    pub fn gain_db_at(&self, sample: usize) -> Option<f32> {
        self.index_at(sample).map(|i| self.gain_db[i])
    }

    pub fn input_db_at(&self, sample: usize) -> Option<f32> {
        self.index_at(sample).map(|i| self.input_db[i])
    }

    /// The deepest reduction anywhere, as a positive number of dB.
    pub fn max_reduction_db(&self) -> f32 {
        -self.gain_db.iter().copied().fold(0.0f32, f32::min)
    }

    /// One past the last sample the trace covers.
    pub fn end(&self) -> usize {
        self.start + self.gain_db.len() * self.hop.max(1)
    }
}

/// Collects a trace one sample at a time.
struct TraceBuilder {
    hop: usize,
    count: usize,
    peak: f32,
    gain_db: f32,
    trace: DynamicsTrace,
}

impl TraceBuilder {
    fn new(len: usize, sample_rate: u32, output_offset_db: f32) -> Self {
        let hop = DynamicsTrace::hop_for(len, sample_rate);
        Self {
            hop,
            count: 0,
            peak: 0.0,
            gain_db: 0.0,
            trace: DynamicsTrace {
                start: 0,
                hop,
                input_db: Vec::with_capacity(len / hop + 1),
                gain_db: Vec::with_capacity(len / hop + 1),
                output_offset_db,
            },
        }
    }

    #[inline]
    fn push(&mut self, input_peak: f32, gain_db: f32) {
        self.peak = self.peak.max(input_peak);
        self.gain_db = self.gain_db.min(gain_db);
        self.count += 1;
        if self.count == self.hop {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.count == 0 {
            return;
        }
        self.trace.input_db.push(amp_to_db(self.peak));
        self.trace.gain_db.push(self.gain_db);
        self.count = 0;
        self.peak = 0.0;
        self.gain_db = 0.0;
    }

    fn finish(mut self) -> DynamicsTrace {
        self.flush();
        self.trace
    }
}

/// The processed channels and what was done to them.
#[derive(Clone, Debug, Default)]
pub struct DynamicsOutput {
    pub channels: Vec<Vec<f32>>,
    pub trace: DynamicsTrace,
}

/// The input side of a trace alone: each hop's loudest sample, and no gain
/// change. What the dynamics tools draw before anything has been rendered.
pub fn level_trace(channels: &[&[f32]], sample_rate: u32) -> DynamicsTrace {
    let len = shortest_len(channels);
    let mut trace = TraceBuilder::new(len, sample_rate, 0.0);
    for i in 0..len {
        let peak = channels.iter().fold(0.0f32, |m, c| m.max(c[i].abs()));
        trace.push(peak, 0.0);
    }
    trace.finish()
}

// ---------------------------------------------------------------------------
// Detector

/// The sidechain of one channel: optional key filters, then a level.
struct Detector {
    hpf: Option<SectionFilter>,
    lpf: Option<SectionFilter>,
    detection: Detection,
    rms_coeff: f32,
    peak_coeff: f32,
    state: f32,
}

impl Detector {
    fn new(
        sample_rate: u32,
        detection: Detection,
        rms_ms: f32,
        peak_release_ms: Option<f32>,
        hpf_hz: f32,
        lpf_hz: f32,
    ) -> Self {
        let sr = f64::from(sample_rate.max(1));
        let nyquist_guard = sr * 0.45;
        let butterworth = std::f64::consts::FRAC_1_SQRT_2;
        let hpf = key_hpf(sample_rate, hpf_hz).map(SectionFilter::new);
        let lpf = (lpf_hz > 0.0 && f64::from(lpf_hz) < nyquist_guard).then(|| {
            SectionFilter::new(Section::design(
                SectionKind::LowPass,
                f64::from(lpf_hz),
                0.0,
                butterworth,
                sr,
            ))
        });
        Self {
            hpf,
            lpf,
            detection,
            rms_coeff: coeff(rms_ms, sample_rate),
            peak_coeff: peak_release_ms.map_or(0.0, |ms| coeff(ms, sample_rate)),
            state: 0.0,
        }
    }

    /// Follow a new RMS window and key high-pass, keeping the detector's
    /// state: what an automated compressor does between blocks.
    fn retune(&mut self, sample_rate: u32, rms_ms: f32, hpf_hz: f32) {
        self.rms_coeff = coeff(rms_ms, sample_rate);
        match (key_hpf(sample_rate, hpf_hz), self.hpf.as_mut()) {
            (Some(section), Some(filter)) => filter.set_section(section),
            (Some(section), None) => self.hpf = Some(SectionFilter::new(section)),
            (None, _) => self.hpf = None,
        }
    }

    /// The level (linear) after feeding `x`.
    #[inline]
    fn level(&mut self, x: f32) -> f32 {
        let mut k = f64::from(x);
        if let Some(f) = self.hpf.as_mut() {
            k = f.tick(k);
        }
        if let Some(f) = self.lpf.as_mut() {
            k = f.tick(k);
        }
        let k = k as f32;
        match self.detection {
            Detection::Peak => {
                let a = k.abs();
                self.state = if a >= self.state {
                    a
                } else {
                    a + self.peak_coeff * (self.state - a)
                };
                self.state
            }
            Detection::Rms => {
                let p = k * k;
                self.state = p + self.rms_coeff * (self.state - p);
                self.state.max(0.0).sqrt()
            }
        }
    }
}

/// A key high-pass at `hpf_hz`, or none at 0: Butterworth, kept under the
/// rate's Nyquist guard.
fn key_hpf(sample_rate: u32, hpf_hz: f32) -> Option<Section> {
    let sr = f64::from(sample_rate.max(1));
    (hpf_hz > 0.0).then(|| {
        Section::design(
            SectionKind::HighPass,
            f64::from(hpf_hz).min(sr * 0.45),
            0.0,
            std::f64::consts::FRAC_1_SQRT_2,
            sr,
        )
    })
}

/// Every channel's detected level for one sample, linked by `link` (0..1):
/// at 1 every channel sees the loudest, at 0 each sees its own.
#[inline]
fn linked_levels(
    detectors: &mut [Detector],
    inputs: impl Iterator<Item = f32>,
    link: f32,
    out: &mut [f32],
) {
    let mut loudest = 0.0f32;
    for ((det, x), slot) in detectors.iter_mut().zip(inputs).zip(out.iter_mut()) {
        *slot = det.level(x);
        loudest = loudest.max(*slot);
    }
    if link > 0.0 {
        for slot in out.iter_mut() {
            *slot = *slot * (1.0 - link) + loudest * link;
        }
    }
}

fn shortest_len(channels: &[&[f32]]) -> usize {
    channels.iter().map(|c| c.len()).min().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Compressor

/// A feed-forward compressor with a soft knee, lookahead, parallel mix and a
/// sidechain high-pass.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompressorSettings {
    pub threshold_db: f32,
    pub ratio: f32,
    /// Width of the soft knee around the threshold; 0 is a hard knee.
    pub knee_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub lookahead_ms: f32,
    pub makeup_db: f32,
    /// Replace `makeup_db` with half the reduction a full-scale signal gets.
    pub auto_makeup: bool,
    /// Processed share of the output (parallel compression below 100).
    pub mix_pct: f32,
    /// The most the gain is ever reduced, in dB.
    pub range_db: f32,
    pub detection: Detection,
    pub rms_ms: f32,
    /// How much the channels share one gain (100: the image never moves).
    pub link_pct: f32,
    /// Sidechain high-pass, so the bass does not pump the rest; 0 is off.
    pub sidechain_hpf_hz: f32,
}

impl CompressorSettings {
    pub const THRESHOLD_DB: std::ops::RangeInclusive<f32> = -60.0..=0.0;
    /// 1:1 is off; 20:1 is a limiter in all but name.
    pub const RATIO: std::ops::RangeInclusive<f32> = 1.0..=20.0;
    pub const KNEE_DB: std::ops::RangeInclusive<f32> = 0.0..=24.0;
    pub const ATTACK_MS: std::ops::RangeInclusive<f32> = 0.05..=500.0;
    pub const RELEASE_MS: std::ops::RangeInclusive<f32> = 1.0..=3_000.0;
    pub const LOOKAHEAD_MS: std::ops::RangeInclusive<f32> = 0.0..=10.0;
    pub const MAKEUP_DB: std::ops::RangeInclusive<f32> = -12.0..=24.0;
    pub const MIX_PCT: std::ops::RangeInclusive<f32> = 0.0..=100.0;
    pub const RANGE_DB: std::ops::RangeInclusive<f32> = 0.0..=60.0;
    pub const RMS_MS: std::ops::RangeInclusive<f32> = 1.0..=300.0;
    pub const LINK_PCT: std::ops::RangeInclusive<f32> = 0.0..=100.0;
    pub const SIDECHAIN_HPF_HZ: std::ops::RangeInclusive<f32> = 0.0..=500.0;

    /// The gain change (dB, <= 0) for a detected level of `level_db`.
    pub fn static_gain_db(&self, level_db: f32) -> f32 {
        let t = self.threshold_db;
        let r = self.ratio.max(1.0);
        let w = self.knee_db.max(0.0);
        let over = level_db - t;
        let out = if w > 0.0 && 2.0 * over.abs() <= w {
            level_db + (1.0 / r - 1.0) * (over + w * 0.5).powi(2) / (2.0 * w)
        } else if over > 0.0 {
            t + over / r
        } else {
            level_db
        };
        (out - level_db).clamp(-self.range_db.max(0.0), 0.0)
    }

    /// The makeup actually applied.
    pub fn effective_makeup_db(&self) -> f32 {
        if self.auto_makeup {
            -0.5 * self.static_gain_db(0.0)
        } else {
            self.makeup_db
        }
    }

    /// Output level for an input level: the transfer curve.
    pub fn transfer_db(&self, level_db: f32) -> f32 {
        level_db + self.static_gain_db(level_db) + self.effective_makeup_db()
    }

    /// Changes nothing whatever comes in.
    pub fn is_neutral(&self) -> bool {
        ((self.ratio - 1.0).abs() < 1e-3 && self.effective_makeup_db().abs() < 1e-3)
            || self.mix_pct <= 0.0
    }
}

/// A compressor setting that can follow a curve. Not the lookahead (it is
/// the delay between the detector and the output), and not the detection
/// mode or auto makeup (switches).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompParam {
    Threshold,
    Ratio,
    Knee,
    Attack,
    Release,
    Makeup,
    Mix,
    Range,
    Link,
    SidechainHpf,
    RmsWindow,
}

impl CompParam {
    pub const ALL: [CompParam; 11] = [
        CompParam::Threshold,
        CompParam::Ratio,
        CompParam::Knee,
        CompParam::Attack,
        CompParam::Release,
        CompParam::Makeup,
        CompParam::Mix,
        CompParam::Range,
        CompParam::Link,
        CompParam::SidechainHpf,
        CompParam::RmsWindow,
    ];

    pub fn range(self) -> std::ops::RangeInclusive<f32> {
        type C = CompressorSettings;
        match self {
            CompParam::Threshold => C::THRESHOLD_DB,
            CompParam::Ratio => C::RATIO,
            CompParam::Knee => C::KNEE_DB,
            CompParam::Attack => C::ATTACK_MS,
            CompParam::Release => C::RELEASE_MS,
            CompParam::Makeup => C::MAKEUP_DB,
            CompParam::Mix => C::MIX_PCT,
            CompParam::Range => C::RANGE_DB,
            CompParam::Link => C::LINK_PCT,
            CompParam::SidechainHpf => C::SIDECHAIN_HPF_HZ,
            CompParam::RmsWindow => C::RMS_MS,
        }
    }

    /// Times and the ratio move by ratio; levels, shares and the key filter
    /// (which runs from 0, off) move by amount.
    pub fn scale(self) -> Scale {
        match self {
            CompParam::Ratio | CompParam::Attack | CompParam::Release | CompParam::RmsWindow => {
                Scale::Log
            }
            _ => Scale::Linear,
        }
    }
}

impl Automatable for CompressorSettings {
    type Param = CompParam;

    fn set_param(&mut self, param: CompParam, value: f32) {
        let range = param.range();
        let v = value.max(*range.start()).min(*range.end());
        match param {
            CompParam::Threshold => self.threshold_db = v,
            CompParam::Ratio => self.ratio = v,
            CompParam::Knee => self.knee_db = v,
            CompParam::Attack => self.attack_ms = v,
            CompParam::Release => self.release_ms = v,
            CompParam::Makeup => self.makeup_db = v,
            CompParam::Mix => self.mix_pct = v,
            CompParam::Range => self.range_db = v,
            CompParam::Link => self.link_pct = v,
            CompParam::SidechainHpf => self.sidechain_hpf_hz = v,
            CompParam::RmsWindow => self.rms_ms = v,
        }
    }

    fn param(&self, param: CompParam) -> f32 {
        match param {
            CompParam::Threshold => self.threshold_db,
            CompParam::Ratio => self.ratio,
            CompParam::Knee => self.knee_db,
            CompParam::Attack => self.attack_ms,
            CompParam::Release => self.release_ms,
            CompParam::Makeup => self.makeup_db,
            CompParam::Mix => self.mix_pct,
            CompParam::Range => self.range_db,
            CompParam::Link => self.link_pct,
            CompParam::SidechainHpf => self.sidechain_hpf_hz,
            CompParam::RmsWindow => self.rms_ms,
        }
    }
}

impl Default for CompressorSettings {
    /// Gentle bus settings that audibly work without pumping.
    fn default() -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 3.0,
            knee_db: 6.0,
            attack_ms: 10.0,
            release_ms: 150.0,
            lookahead_ms: 0.0,
            makeup_db: 0.0,
            auto_makeup: false,
            mix_pct: 100.0,
            range_db: 60.0,
            detection: Detection::Peak,
            rms_ms: 10.0,
            link_pct: 100.0,
            sidechain_hpf_hz: 0.0,
        }
    }
}

/// Compress `channels` together. `None` when cancelled.
pub fn compress(
    channels: &[&[f32]],
    sample_rate: u32,
    s: &CompressorSettings,
    cancel: &AtomicBool,
) -> Option<DynamicsOutput> {
    compress_automated(channels, sample_rate, s, &Tracks::none(), 0, cancel)
}

/// Compress `channels` together with `tracks` moving the settings over
/// time. `offset` is the buffer sample `channels` start at: the curves'
/// points are buffer samples. With no tracks this is exactly [`compress`].
/// `None` when cancelled.
pub fn compress_automated(
    channels: &[&[f32]],
    sample_rate: u32,
    s: &CompressorSettings,
    tracks: &Tracks<CompParam>,
    offset: usize,
    cancel: &AtomicBool,
) -> Option<DynamicsOutput> {
    let len = shortest_len(channels);
    let n = channels.len();
    // The settings in force; an automated compressor moves them a block at
    // a time, and the trace keeps the static makeup as its offset.
    let mut cur = *s;
    let mut makeup = cur.effective_makeup_db();
    let mut trace = TraceBuilder::new(len, sample_rate, makeup);
    let mut out: Vec<Vec<f32>> = channels.iter().map(|_| Vec::with_capacity(len)).collect();
    if len == 0 {
        return Some(DynamicsOutput {
            channels: out,
            trace: trace.finish(),
        });
    }
    let mut detectors: Vec<Detector> = (0..n)
        .map(|_| {
            Detector::new(
                sample_rate,
                s.detection,
                s.rms_ms,
                None,
                s.sidechain_hpf_hz,
                0.0,
            )
        })
        .collect();
    let mut attack = coeff(s.attack_ms, sample_rate);
    let mut release = coeff(s.release_ms, sample_rate);
    let la = frames(s.lookahead_ms, sample_rate);
    let mut mix = (s.mix_pct / 100.0).clamp(0.0, 1.0);
    let mut link = (s.link_pct / 100.0).clamp(0.0, 1.0);
    let mut levels = vec![0.0f32; n];
    let mut env = vec![0.0f32; n];
    let mut eval = (!tracks.is_empty()).then(|| Evaluator::new(*s, tracks));
    for i in 0..len + la {
        if i % CANCEL_STRIDE == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        if let Some(eval) = eval.as_mut().filter(|_| i % AUTOMATION_BLOCK == 0) {
            let next = eval.at(offset + i);
            if next != cur {
                if next.attack_ms != cur.attack_ms {
                    attack = coeff(next.attack_ms, sample_rate);
                }
                if next.release_ms != cur.release_ms {
                    release = coeff(next.release_ms, sample_rate);
                }
                if next.rms_ms != cur.rms_ms || next.sidechain_hpf_hz != cur.sidechain_hpf_hz {
                    for det in detectors.iter_mut() {
                        det.retune(sample_rate, next.rms_ms, next.sidechain_hpf_hz);
                    }
                }
                mix = (next.mix_pct / 100.0).clamp(0.0, 1.0);
                link = (next.link_pct / 100.0).clamp(0.0, 1.0);
                makeup = next.effective_makeup_db();
                cur = next;
            }
        }
        let lead = channels.iter().map(|c| if i < len { c[i] } else { 0.0 });
        linked_levels(&mut detectors, lead, link, &mut levels);
        for (e, level) in env.iter_mut().zip(&levels) {
            let target = cur.static_gain_db(amp_to_db(*level));
            let c = if target < *e { attack } else { release };
            *e = target + c * (*e - target);
        }
        if i < la {
            continue;
        }
        let at = i - la;
        let mut peak = 0.0f32;
        let mut deepest = 0.0f32;
        for ((dst, src), e) in out.iter_mut().zip(channels).zip(&env) {
            let x = src[at];
            let g = db_to_amp(*e + makeup);
            dst.push(x * (1.0 - mix) + x * g * mix);
            peak = peak.max(x.abs());
            deepest = deepest.min(*e);
        }
        trace.push(peak, deepest);
    }
    Some(DynamicsOutput {
        channels: out,
        trace: trace.finish(),
    })
}

// ---------------------------------------------------------------------------
// Limiter

/// A lookahead brickwall limiter.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimiterSettings {
    /// Gain before limiting: how hard the audio is pushed into the ceiling.
    pub input_gain_db: f32,
    pub ceiling_db: f32,
    pub release_ms: f32,
    pub lookahead_ms: f32,
    /// Keep the peaks between samples under the ceiling too (4x estimate).
    pub true_peak: bool,
    /// One gain for every channel, so a peak in one does not move the image.
    pub link: bool,
    /// Run at this many times the rate, so fast gain changes and the clamp
    /// at the ceiling make no aliasing. Off renders what the limiter always
    /// did.
    pub oversampling: Oversampling,
}

impl LimiterSettings {
    /// Up to +48 dB: far enough to crush a whole programme flat against the
    /// ceiling as an effect, not only to shave its peaks.
    pub const INPUT_GAIN_DB: std::ops::RangeInclusive<f32> = 0.0..=48.0;
    /// Down to -30 dB, so the ceiling can also hold a part well under full
    /// scale (a stem, a bed under dialogue).
    pub const CEILING_DB: std::ops::RangeInclusive<f32> = -30.0..=0.0;

    /// The reduction a full GR scale should show for these settings: the
    /// usual 24 dB, or 48 dB once the limiter is driven past what that holds.
    pub fn gr_scale_db(&self) -> f32 {
        if self.input_gain_db - self.ceiling_db > 24.0 {
            48.0
        } else {
            24.0
        }
    }
    pub const RELEASE_MS: std::ops::RangeInclusive<f32> = 1.0..=1_000.0;
    pub const LOOKAHEAD_MS: std::ops::RangeInclusive<f32> = 0.5..=10.0;
}

/// A limiter setting that can follow a curve. Not the lookahead (the
/// window length), true peak, link or oversampling (switches).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LimiterParam {
    InputGain,
    Ceiling,
    Release,
}

impl LimiterParam {
    pub const ALL: [LimiterParam; 3] = [
        LimiterParam::InputGain,
        LimiterParam::Ceiling,
        LimiterParam::Release,
    ];

    pub fn range(self) -> std::ops::RangeInclusive<f32> {
        match self {
            LimiterParam::InputGain => LimiterSettings::INPUT_GAIN_DB,
            LimiterParam::Ceiling => LimiterSettings::CEILING_DB,
            LimiterParam::Release => LimiterSettings::RELEASE_MS,
        }
    }

    pub fn scale(self) -> Scale {
        match self {
            LimiterParam::Release => Scale::Log,
            _ => Scale::Linear,
        }
    }
}

impl Automatable for LimiterSettings {
    type Param = LimiterParam;

    fn set_param(&mut self, param: LimiterParam, value: f32) {
        let range = param.range();
        let v = value.max(*range.start()).min(*range.end());
        match param {
            LimiterParam::InputGain => self.input_gain_db = v,
            LimiterParam::Ceiling => self.ceiling_db = v,
            LimiterParam::Release => self.release_ms = v,
        }
    }

    fn param(&self, param: LimiterParam) -> f32 {
        match param {
            LimiterParam::InputGain => self.input_gain_db,
            LimiterParam::Ceiling => self.ceiling_db,
            LimiterParam::Release => self.release_ms,
        }
    }
}

/// Where the frames a limiter or clipper works on sit in the buffer: the
/// segment starts at buffer sample `offset`, and frame `j` belongs to its
/// sample `(j - pad) / factor` (its first, for the frames before it).
/// Plain frames are the samples: `factor` 1, `pad` 0.
#[derive(Clone, Copy, Debug)]
struct Clock {
    offset: usize,
    factor: usize,
    pad: usize,
}

impl Clock {
    fn plain(offset: usize) -> Self {
        Self {
            offset,
            factor: 1,
            pad: 0,
        }
    }

    fn oversampled(offset: usize, os: &Oversampler) -> Self {
        Self {
            offset,
            factor: os.factor(),
            pad: os.pad(),
        }
    }

    /// The automation block frame `j` falls in.
    #[inline]
    fn block(&self, j: usize) -> usize {
        j.saturating_sub(self.pad) / self.factor / AUTOMATION_BLOCK
    }

    /// The buffer sample a block's settings are evaluated at.
    fn block_start(&self, block: usize) -> usize {
        self.offset + block * AUTOMATION_BLOCK
    }
}

/// A limiter's input gain, ceiling (both linear) and release coefficient at
/// a frame, re-evaluated a block of buffer samples at a time when automated.
struct LimiterAt<'a> {
    eval: Option<Evaluator<'a, LimiterSettings>>,
    clock: Clock,
    /// The frames' rate, which the release is timed in.
    rate: u32,
    block: usize,
    gin: f32,
    ceiling: f32,
    release: f32,
}

impl<'a> LimiterAt<'a> {
    fn new(s: &LimiterSettings, tracks: &'a Tracks<LimiterParam>, clock: Clock, rate: u32) -> Self {
        Self {
            eval: (!tracks.is_empty()).then(|| Evaluator::new(*s, tracks)),
            clock,
            rate,
            block: usize::MAX,
            gin: db_to_amp(s.input_gain_db),
            ceiling: db_to_amp(s.ceiling_db.min(0.0)),
            release: coeff(s.release_ms, rate),
        }
    }

    #[inline]
    fn at(&mut self, j: usize) -> (f32, f32, f32) {
        if let Some(eval) = self.eval.as_mut() {
            let block = self.clock.block(j);
            if block != self.block {
                self.block = block;
                let now = eval.at(self.clock.block_start(block));
                self.gin = db_to_amp(now.input_gain_db);
                self.ceiling = db_to_amp(now.ceiling_db.min(0.0));
                self.release = coeff(now.release_ms, self.rate);
            }
        }
        (self.gin, self.ceiling, self.release)
    }
}

impl Default for LimiterSettings {
    /// A master limiter: -1 dBTP leaves room for lossy encoders.
    fn default() -> Self {
        Self {
            input_gain_db: 0.0,
            ceiling_db: -1.0,
            release_ms: 50.0,
            lookahead_ms: 5.0,
            true_peak: true,
            link: true,
            oversampling: Oversampling::Off,
        }
    }
}

/// What a limiter or clipper reads, frame by frame: the buffer itself, or
/// the buffer oversampled (`UpStream`). One code path for both, so with
/// oversampling off a tool renders exactly what it rendered before there
/// was any.
trait Frames {
    fn channels(&self) -> usize;
    fn len(&self) -> usize;
    /// Frame `j < len` of channel `c`.
    fn at(&mut self, c: usize, j: usize) -> f32;
}

/// The buffer's own samples.
struct Plain<'a> {
    channels: &'a [&'a [f32]],
    len: usize,
}

impl<'a> Plain<'a> {
    fn new(channels: &'a [&'a [f32]]) -> Self {
        Self {
            channels,
            len: shortest_len(channels),
        }
    }
}

impl Frames for Plain<'_> {
    fn channels(&self) -> usize {
        self.channels.len()
    }

    fn len(&self) -> usize {
        self.len
    }

    #[inline]
    fn at(&mut self, c: usize, j: usize) -> f32 {
        self.channels[c][j]
    }
}

impl Frames for UpStream<'_> {
    fn channels(&self) -> usize {
        UpStream::channels(self)
    }

    fn len(&self) -> usize {
        UpStream::len(self)
    }

    #[inline]
    fn at(&mut self, c: usize, j: usize) -> f32 {
        UpStream::at(self, c, j)
    }
}

type TruePeakTaps = [[f32; 2 * TRUE_PEAK_HALF_TAPS]; 3];

/// Windowed-sinc taps for the inter-sample points, per phase, for samples
/// `j - HALF + 1 ..= j + HALF` around a point between `j` and `j + 1`.
fn true_peak_taps() -> TruePeakTaps {
    let mut taps = [[0.0f32; 2 * TRUE_PEAK_HALF_TAPS]; 3];
    let half = TRUE_PEAK_HALF_TAPS as f64;
    for (p, frac) in TRUE_PEAK_PHASES.iter().enumerate() {
        for (k, tap) in taps[p].iter_mut().enumerate() {
            let offset = k as f64 - (half - 1.0); // sample j + offset
            let t = frac - offset;
            let sinc = if t.abs() < 1e-9 {
                1.0
            } else {
                (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
            };
            let window = 0.5 + 0.5 * (std::f64::consts::PI * t / (half + 1.0)).cos();
            *tap = (sinc * window) as f32;
        }
    }
    taps
}

/// The largest magnitude at or just after frame `j` of channel `c`.
#[inline]
fn peak_at<F: Frames>(src: &mut F, c: usize, j: usize, taps: Option<&TruePeakTaps>) -> f32 {
    let mut peak = src.at(c, j).abs();
    if let Some(taps) = taps {
        let len = src.len();
        let first = j as isize - (TRUE_PEAK_HALF_TAPS as isize - 1);
        for phase in taps {
            let mut acc = 0.0f32;
            for (k, tap) in phase.iter().enumerate() {
                let idx = first + k as isize;
                if idx >= 0 && (idx as usize) < len {
                    acc += src.at(c, idx as usize) * tap;
                }
            }
            peak = peak.max(acc.abs());
        }
    }
    peak
}

/// The gain frame `j` needs so its peak, driven by `gin`, stays under
/// `ceiling`: every channel's peak when linked, else `chain`'s own.
#[inline]
fn limiter_required<F: Frames>(
    src: &mut F,
    link: bool,
    chain: usize,
    j: usize,
    taps: Option<&TruePeakTaps>,
    gin: f32,
    ceiling: f32,
) -> f32 {
    let peak = if link {
        let mut m = 0.0f32;
        for c in 0..src.channels() {
            m = m.max(peak_at(src, c, j, taps));
        }
        m
    } else {
        peak_at(src, chain, j, taps)
    } * gin;
    if peak > ceiling {
        ceiling / peak
    } else {
        1.0
    }
}

/// The gain one limiter chain follows: the smallest requirement in the
/// lookahead window, averaged over the window so it arrives as a ramp that
/// is complete when the peak is, then released exponentially.
struct LookaheadGain {
    window: usize,
    queue: VecDeque<(usize, f32)>,
    ring: Vec<f32>,
    sum: f64,
    pos: usize,
    gain: f32,
    release: f32,
}

impl LookaheadGain {
    fn new(window: usize, release: f32) -> Self {
        let window = window.max(1);
        Self {
            window,
            queue: VecDeque::with_capacity(window + 1),
            ring: vec![1.0; window],
            sum: window as f64,
            pos: 0,
            gain: 1.0,
            release,
        }
    }

    /// Start already at `gain`: offline there is no "before", so the first
    /// window's requirement holds from the first sample instead of being
    /// ramped into from unity (which let the opening peaks through).
    fn prime(&mut self, gain: f32) {
        self.ring.fill(gain);
        self.sum = f64::from(gain) * self.window as f64;
        self.gain = gain;
    }

    /// The gain sample `idx` will need.
    fn require(&mut self, idx: usize, required: f32) {
        while self.queue.back().is_some_and(|(_, r)| *r >= required) {
            self.queue.pop_back();
        }
        self.queue.push_back((idx, required));
    }

    /// The gain for sample `i` (call with consecutive `i`).
    fn gain_at(&mut self, i: usize) -> f32 {
        while self.queue.front().is_some_and(|(idx, _)| *idx < i) {
            self.queue.pop_front();
        }
        let m = self.queue.front().map_or(1.0, |(_, r)| *r);
        let slot = self.pos % self.window;
        self.sum += f64::from(m) - f64::from(self.ring[slot]);
        self.ring[slot] = m;
        self.pos += 1;
        let s = (self.sum / self.window as f64) as f32;
        self.gain = if s < self.gain {
            s
        } else {
            s + self.release * (self.gain - s)
        };
        self.gain
    }
}

/// Limit `channels` together. `None` when cancelled.
pub fn limit(
    channels: &[&[f32]],
    sample_rate: u32,
    s: &LimiterSettings,
    cancel: &AtomicBool,
) -> Option<DynamicsOutput> {
    limit_automated(channels, sample_rate, s, &Tracks::none(), 0, cancel)
}

/// Limit `channels` together with `tracks` moving the input gain, ceiling
/// and release over time. A peak is judged by the settings at the peak
/// (which the lookahead reaches before the output does); the output is
/// held to the ceiling in force where it is written. `offset` is the
/// buffer sample `channels` start at. With no tracks this is exactly
/// [`limit`]. `None` when cancelled.
pub fn limit_automated(
    channels: &[&[f32]],
    sample_rate: u32,
    s: &LimiterSettings,
    tracks: &Tracks<LimiterParam>,
    offset: usize,
    cancel: &AtomicBool,
) -> Option<DynamicsOutput> {
    let len = shortest_len(channels);
    let mut trace = TraceBuilder::new(len, sample_rate, s.input_gain_db);
    let mut out: Vec<Vec<f32>> = channels.iter().map(|_| Vec::with_capacity(len)).collect();
    if len == 0 {
        return Some(DynamicsOutput {
            channels: out,
            trace: trace.finish(),
        });
    }
    let input_peak = |i: usize| channels.iter().fold(0.0f32, |m, c| m.max(c[i].abs()));
    match Oversampler::new(s.oversampling) {
        None => {
            let mut src = Plain::new(channels);
            let clock = Clock::plain(offset);
            let done = limit_frames(&mut src, sample_rate, s, tracks, clock, cancel, |i, frame, lowest| {
                for (dst, y) in out.iter_mut().zip(frame) {
                    dst.push(*y);
                }
                trace.push(input_peak(i), amp_to_db(lowest));
            });
            if !done {
                return None;
            }
        }
        Some(os) => {
            let factor = os.factor();
            let rate = sample_rate.saturating_mul(factor as u32);
            // Reads reach from the true-peak taps behind the frame being
            // written to those after the end of the lookahead.
            let reach = frames(s.lookahead_ms, rate).max(1) + 2 * TRUE_PEAK_HALF_TAPS;
            let mut src = UpStream::new(&os, channels, len, reach);
            let clock = Clock::oversampled(offset, &os);
            let mut downs: Vec<_> = channels.iter().map(|_| os.down_stream()).collect();
            let mut lowest_here = 1.0f32;
            let done = limit_frames(&mut src, rate, s, tracks, clock, cancel, |k, frame, lowest| {
                // A buffer sample's frames make its trace point.
                if let Some(m) = k.checked_sub(clock.pad).filter(|m| *m < len * factor) {
                    lowest_here = lowest_here.min(lowest);
                    if m % factor == factor - 1 {
                        trace.push(input_peak(m / factor), amp_to_db(lowest_here));
                        lowest_here = 1.0;
                    }
                }
                for ((down, dst), y) in downs.iter_mut().zip(out.iter_mut()).zip(frame) {
                    if let Some(v) = down.push(*y) {
                        dst.push(v);
                    }
                }
            });
            if !done {
                return None;
            }
            // A brickwall: the filter back down rings a hair past what it
            // was given, so the ceiling is held once more at the buffer's rate.
            let mut at = LimiterAt::new(s, tracks, Clock::plain(offset), sample_rate);
            for i in 0..len {
                let (_, ceiling, _) = at.at(i);
                for dst in out.iter_mut() {
                    dst[i] = dst[i].clamp(-ceiling, ceiling);
                }
            }
        }
    }
    Some(DynamicsOutput {
        channels: out,
        trace: trace.finish(),
    })
}

/// The limiter over `src`: `emit(j, outputs, lowest gain)` for every frame
/// in order. `rate` is the frames' rate (the lookahead and the release are
/// times). `false` when cancelled.
fn limit_frames<F: Frames>(
    src: &mut F,
    rate: u32,
    s: &LimiterSettings,
    tracks: &Tracks<LimiterParam>,
    clock: Clock,
    cancel: &AtomicBool,
    mut emit: impl FnMut(usize, &[f32], f32),
) -> bool {
    let len = src.len();
    let n = src.channels();
    let window = frames(s.lookahead_ms, rate).max(1);
    let taps = s.true_peak.then(true_peak_taps);
    let taps = taps.as_ref();
    let chains = if s.link { 1 } else { n };
    // The settings where a requirement is decided, and where the output is
    // written: the same until something is automated.
    let mut at_peak = LimiterAt::new(s, tracks, clock, rate);
    let mut at_out = LimiterAt::new(s, tracks, clock, rate);
    let mut gains: Vec<LookaheadGain> = (0..chains)
        .map(|_| LookaheadGain::new(window, at_out.release))
        .collect();
    for (c, g) in gains.iter_mut().enumerate() {
        let mut first = 1.0f32;
        for j in 0..window.min(len) {
            let (gin, ceiling, _) = at_peak.at(j);
            first = first.min(limiter_required(src, s.link, c, j, taps, gin, ceiling));
        }
        g.prime(first);
        for j in 0..(window - 1).min(len) {
            let (gin, ceiling, _) = at_peak.at(j);
            g.require(j, limiter_required(src, s.link, c, j, taps, gin, ceiling));
        }
    }
    let mut chain_gain = vec![1.0f32; chains];
    let mut frame = vec![0.0f32; n];
    for i in 0..len {
        if i % CANCEL_STRIDE == 0 && cancel.load(Ordering::Relaxed) {
            return false;
        }
        let ahead = i + window - 1;
        let peak_settings = (ahead < len).then(|| at_peak.at(ahead));
        let (gin, ceiling, release) = at_out.at(i);
        for (c, g) in gains.iter_mut().enumerate() {
            if let Some((ahead_gin, ahead_ceiling, _)) = peak_settings {
                let r = limiter_required(src, s.link, c, ahead, taps, ahead_gin, ahead_ceiling);
                g.require(ahead, r);
            }
            g.release = release;
            chain_gain[c] = g.gain_at(i);
        }
        let mut lowest = 1.0f32;
        for (ci, y) in frame.iter_mut().enumerate() {
            let g = chain_gain[if s.link { 0 } else { ci }];
            *y = (src.at(ci, i) * gin * g).clamp(-ceiling, ceiling);
            lowest = lowest.min(g);
        }
        emit(i, &frame, lowest);
    }
    true
}

// ---------------------------------------------------------------------------
// Clipper

/// A clipper: whatever passes the ceiling is cut to it on the spot -- no
/// gain riding, no lookahead, no release -- below an optional soft knee.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipperSettings {
    /// Gain before clipping: how hard the audio is pushed into the ceiling.
    pub input_gain_db: f32,
    pub ceiling_db: f32,
    /// How far under the ceiling the curve starts to bend; 0 cuts hard.
    pub knee_db: f32,
    /// Clip at this many times the rate, so the harmonics of the cut
    /// corners are filtered out instead of folding back as aliasing.
    pub oversampling: Oversampling,
    /// After oversampling, keep every sample under the ceiling. The filter
    /// back down rounds the cut corners and overshoots by up to about a
    /// decibel; a fast limiter turns that down (`catch_overshoot`).
    pub hard_ceiling: bool,
}

impl ClipperSettings {
    /// Up to +24 dB: a clipper driven further is a square wave.
    pub const INPUT_GAIN_DB: std::ops::RangeInclusive<f32> = 0.0..=24.0;
    /// The limiter's range.
    pub const CEILING_DB: std::ops::RangeInclusive<f32> = -30.0..=0.0;
    pub const KNEE_DB: std::ops::RangeInclusive<f32> = 0.0..=12.0;

    /// The output level (dBFS) of a steady input at `in_db`, before the
    /// input gain.
    pub fn transfer_db(&self, in_db: f32) -> f32 {
        let (gin, knee_start, ceiling) = clipper_levels(self);
        amp_to_db(clip_sample(db_to_amp(in_db) * gin, knee_start, ceiling))
    }

    /// The reduction a full GR scale shows: the drive's whole range.
    pub fn gr_scale_db(&self) -> f32 {
        24.0
    }
}

impl Default for ClipperSettings {
    /// A clean safety clipper: hard at -1 dB, at 4x, never over the ceiling.
    fn default() -> Self {
        Self {
            input_gain_db: 0.0,
            ceiling_db: -1.0,
            knee_db: 0.0,
            oversampling: Oversampling::X4,
            hard_ceiling: true,
        }
    }
}

/// A clipper setting that can follow a curve. Not the oversampling or the
/// hard ceiling (switches).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClipperParam {
    InputGain,
    Ceiling,
    Knee,
}

impl ClipperParam {
    pub const ALL: [ClipperParam; 3] = [
        ClipperParam::InputGain,
        ClipperParam::Ceiling,
        ClipperParam::Knee,
    ];

    pub fn range(self) -> std::ops::RangeInclusive<f32> {
        match self {
            ClipperParam::InputGain => ClipperSettings::INPUT_GAIN_DB,
            ClipperParam::Ceiling => ClipperSettings::CEILING_DB,
            ClipperParam::Knee => ClipperSettings::KNEE_DB,
        }
    }

    pub fn scale(self) -> Scale {
        Scale::Linear
    }
}

impl Automatable for ClipperSettings {
    type Param = ClipperParam;

    fn set_param(&mut self, param: ClipperParam, value: f32) {
        let range = param.range();
        let v = value.max(*range.start()).min(*range.end());
        match param {
            ClipperParam::InputGain => self.input_gain_db = v,
            ClipperParam::Ceiling => self.ceiling_db = v,
            ClipperParam::Knee => self.knee_db = v,
        }
    }

    fn param(&self, param: ClipperParam) -> f32 {
        match param {
            ClipperParam::InputGain => self.input_gain_db,
            ClipperParam::Ceiling => self.ceiling_db,
            ClipperParam::Knee => self.knee_db,
        }
    }
}

/// The input gain, where the knee starts and the ceiling, all linear.
fn clipper_levels(s: &ClipperSettings) -> (f32, f32, f32) {
    let ceiling = db_to_amp(s.ceiling_db.min(0.0));
    (
        db_to_amp(s.input_gain_db),
        ceiling * db_to_amp(-s.knee_db.max(0.0)),
        ceiling,
    )
}

/// The clipper's curve: `u` as it is up to `knee_start`, then bent towards
/// `ceiling` -- flat against it at once where the two meet (a hard clip),
/// otherwise along a tanh, which leaves the knee at the slope it came in
/// with and only ever approaches the ceiling.
#[inline]
pub fn clip_sample(u: f32, knee_start: f32, ceiling: f32) -> f32 {
    let a = u.abs();
    if a <= knee_start {
        return u;
    }
    let span = ceiling - knee_start;
    let y = if span > 0.0 {
        knee_start + span * ((a - knee_start) / span).tanh()
    } else {
        ceiling
    };
    y.min(ceiling).copysign(u)
}

/// A clipper's levels at a frame, re-evaluated a block of buffer samples
/// at a time when automated.
struct ClipperAt<'a> {
    eval: Option<Evaluator<'a, ClipperSettings>>,
    clock: Clock,
    block: usize,
    levels: (f32, f32, f32),
}

impl<'a> ClipperAt<'a> {
    fn new(s: &ClipperSettings, tracks: &'a Tracks<ClipperParam>, clock: Clock) -> Self {
        Self {
            eval: (!tracks.is_empty()).then(|| Evaluator::new(*s, tracks)),
            clock,
            block: usize::MAX,
            levels: clipper_levels(s),
        }
    }

    /// (input gain, knee start, ceiling) at frame `j`.
    #[inline]
    fn at(&mut self, j: usize) -> (f32, f32, f32) {
        if let Some(eval) = self.eval.as_mut() {
            let block = self.clock.block(j);
            if block != self.block {
                self.block = block;
                self.levels = clipper_levels(&eval.at(self.clock.block_start(block)));
            }
        }
        self.levels
    }
}

/// Clip `channels`. `None` when cancelled.
pub fn clip(
    channels: &[&[f32]],
    sample_rate: u32,
    s: &ClipperSettings,
    cancel: &AtomicBool,
) -> Option<DynamicsOutput> {
    clip_automated(channels, sample_rate, s, &Tracks::none(), 0, cancel)
}

/// Clip `channels` with `tracks` moving the input gain, ceiling and knee
/// over time; `offset` is the buffer sample `channels` start at. The trace
/// says, hop by hop, how far the loudest output came under the loudest
/// driven input -- what the clipper took, the filters' overshoot included.
/// `None` when cancelled.
pub fn clip_automated(
    channels: &[&[f32]],
    sample_rate: u32,
    s: &ClipperSettings,
    tracks: &Tracks<ClipperParam>,
    offset: usize,
    cancel: &AtomicBool,
) -> Option<DynamicsOutput> {
    let len = shortest_len(channels);
    let mut out: Vec<Vec<f32>> = channels.iter().map(|_| Vec::with_capacity(len)).collect();
    if len > 0 {
        let done = match Oversampler::new(s.oversampling) {
            None => {
                let mut src = Plain::new(channels);
                clip_frames(&mut src, s, tracks, Clock::plain(offset), cancel, |_, frame| {
                    for (dst, y) in out.iter_mut().zip(frame) {
                        dst.push(*y);
                    }
                })
            }
            Some(os) => {
                // A clip reads only the frame it writes.
                let mut src = UpStream::new(&os, channels, len, 1);
                let mut downs: Vec<_> = channels.iter().map(|_| os.down_stream()).collect();
                let clock = Clock::oversampled(offset, &os);
                let done = clip_frames(&mut src, s, tracks, clock, cancel, |_, frame| {
                    for ((down, dst), y) in downs.iter_mut().zip(out.iter_mut()).zip(frame) {
                        if let Some(v) = down.push(*y) {
                            dst.push(v);
                        }
                    }
                });
                if done && s.hard_ceiling {
                    out = catch_overshoot(&out, sample_rate, s, tracks, offset, cancel)?;
                }
                done
            }
        };
        if !done {
            return None;
        }
    }
    let trace = clipper_trace(channels, &out, sample_rate, s, tracks, offset);
    Some(DynamicsOutput {
        channels: out,
        trace,
    })
}

/// The hard ceiling's limiter reads this far ahead: the overshoot is a
/// ripple at the cut corners, a fraction of a millisecond wide...
const HARD_CEILING_LOOKAHEAD_MS: f32 = 1.0;
/// ...and lets go this slowly, so a run of clipped crests is held one steady
/// fraction of a decibel down instead of being turned down and up at their
/// rate, which would put back the aliasing oversampling took out.
const HARD_CEILING_RELEASE_MS: f32 = 20.0;

/// An oversampled clip's output held under its ceiling. Clamping the
/// overshoot would clip again at the buffer's rate and fold much of what
/// oversampling removed back in; a fast linked limiter turns down only
/// around it, smoothly, and leaves every other sample as it is. `None` when
/// cancelled.
fn catch_overshoot(
    out: &[Vec<f32>],
    sample_rate: u32,
    s: &ClipperSettings,
    tracks: &Tracks<ClipperParam>,
    offset: usize,
    cancel: &AtomicBool,
) -> Option<Vec<Vec<f32>>> {
    let catch = LimiterSettings {
        input_gain_db: 0.0,
        ceiling_db: s.ceiling_db,
        release_ms: HARD_CEILING_RELEASE_MS,
        lookahead_ms: HARD_CEILING_LOOKAHEAD_MS,
        true_peak: false,
        link: true,
        oversampling: Oversampling::Off,
    };
    let mut ceiling = Tracks::none();
    if let Some(curve) = tracks.curve(ClipperParam::Ceiling) {
        ceiling.push(LimiterParam::Ceiling, curve.clone());
    }
    let refs: Vec<&[f32]> = out.iter().map(Vec::as_slice).collect();
    let mut caught: Vec<Vec<f32>> = out.iter().map(|c| Vec::with_capacity(c.len())).collect();
    let mut src = Plain::new(&refs);
    let done = limit_frames(&mut src, sample_rate, &catch, &ceiling, Clock::plain(offset), cancel, |_, frame, _| {
        for (dst, y) in caught.iter_mut().zip(frame) {
            dst.push(*y);
        }
    });
    done.then_some(caught)
}

/// The clipper over `src`: `emit(j, outputs)` for every frame in order.
/// `false` when cancelled.
fn clip_frames<F: Frames>(
    src: &mut F,
    s: &ClipperSettings,
    tracks: &Tracks<ClipperParam>,
    clock: Clock,
    cancel: &AtomicBool,
    mut emit: impl FnMut(usize, &[f32]),
) -> bool {
    let len = src.len();
    let mut at = ClipperAt::new(s, tracks, clock);
    let mut frame = vec![0.0f32; src.channels()];
    for j in 0..len {
        if j % CANCEL_STRIDE == 0 && cancel.load(Ordering::Relaxed) {
            return false;
        }
        let (gin, knee_start, ceiling) = at.at(j);
        for (c, y) in frame.iter_mut().enumerate() {
            *y = clip_sample(src.at(c, j) * gin, knee_start, ceiling);
        }
        emit(j, &frame);
    }
    true
}

/// A clipper's trace, measured from what it wrote: per hop, the loudest
/// input sample, and the loudest output against the loudest input times the
/// gain in force there (0 dB when nothing was cut).
fn clipper_trace(
    src: &[&[f32]],
    out: &[Vec<f32>],
    sample_rate: u32,
    s: &ClipperSettings,
    tracks: &Tracks<ClipperParam>,
    offset: usize,
) -> DynamicsTrace {
    let len = out.first().map_or(0, Vec::len);
    let hop = DynamicsTrace::hop_for(len, sample_rate);
    let mut trace = DynamicsTrace {
        start: 0,
        hop,
        input_db: Vec::with_capacity(len / hop + 1),
        gain_db: Vec::with_capacity(len / hop + 1),
        output_offset_db: s.input_gain_db,
    };
    let mut at = ClipperAt::new(s, tracks, Clock::plain(offset));
    let mut i = 0;
    while i < len {
        let end = (i + hop).min(len);
        let (mut peak, mut driven, mut loudest) = (0.0f32, 0.0f32, 0.0f32);
        for j in i..end {
            let (gin, _, _) = at.at(j);
            for (x, y) in src.iter().zip(out) {
                let a = x[j].abs();
                peak = peak.max(a);
                driven = driven.max(a * gin);
                loudest = loudest.max(y[j].abs());
            }
        }
        trace.input_db.push(amp_to_db(peak));
        trace.gain_db.push(if driven > 0.0 {
            (amp_to_db(loudest) - amp_to_db(driven)).min(0.0)
        } else {
            0.0
        });
        i = end;
    }
    trace
}

// ---------------------------------------------------------------------------
// Gate

/// A gate with hysteresis, hold, a floor and a key filter.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GateSettings {
    /// Opens at this level.
    pub threshold_db: f32,
    /// Closes this far below the threshold, so a level hovering at the
    /// threshold does not chatter.
    pub hysteresis_db: f32,
    /// Attenuation while closed (the floor), in dB, <= 0.
    pub range_db: f32,
    pub attack_ms: f32,
    /// How long it stays open after the level drops.
    pub hold_ms: f32,
    pub release_ms: f32,
    /// Open this early, so an onset is not clipped.
    pub lookahead_ms: f32,
    pub detection: Detection,
    pub rms_ms: f32,
    pub link_pct: f32,
    /// Key filters: listen to a band only (0 is off).
    pub key_hpf_hz: f32,
    pub key_lpf_hz: f32,
}

impl GateSettings {
    pub const THRESHOLD_DB: std::ops::RangeInclusive<f32> = -80.0..=0.0;
    pub const HYSTERESIS_DB: std::ops::RangeInclusive<f32> = 0.0..=20.0;
    /// -90 dB is silence for any practical purpose.
    pub const RANGE_DB: std::ops::RangeInclusive<f32> = -90.0..=0.0;
    pub const ATTACK_MS: std::ops::RangeInclusive<f32> = 0.05..=500.0;
    pub const HOLD_MS: std::ops::RangeInclusive<f32> = 0.0..=2_000.0;
    pub const RELEASE_MS: std::ops::RangeInclusive<f32> = 1.0..=3_000.0;
    pub const LOOKAHEAD_MS: std::ops::RangeInclusive<f32> = 0.0..=10.0;
    pub const RMS_MS: std::ops::RangeInclusive<f32> = CompressorSettings::RMS_MS;
    pub const LINK_PCT: std::ops::RangeInclusive<f32> = 0.0..=100.0;
    pub const KEY_HPF_HZ: std::ops::RangeInclusive<f32> = 0.0..=4_000.0;
    pub const KEY_LPF_HZ: std::ops::RangeInclusive<f32> = 0.0..=crate::sample_rate::AUDIBLE_HIGH_HZ;

    /// The level it closes at.
    pub fn close_db(&self) -> f32 {
        self.threshold_db - self.hysteresis_db.max(0.0)
    }
}

impl Default for GateSettings {
    fn default() -> Self {
        Self {
            // Well under speech, well over a quiet room.
            threshold_db: -40.0,
            hysteresis_db: 3.0,
            range_db: -80.0,
            // Fast enough not to blunt a consonant.
            attack_ms: 2.0,
            hold_ms: 20.0,
            // Slow enough not to chop a word's tail.
            release_ms: 100.0,
            lookahead_ms: 0.0,
            detection: Detection::Peak,
            rms_ms: 10.0,
            link_pct: 100.0,
            key_hpf_hz: 0.0,
            key_lpf_hz: 0.0,
        }
    }
}

/// Gate `channels` together. `None` when cancelled.
pub fn gate(
    channels: &[&[f32]],
    sample_rate: u32,
    s: &GateSettings,
    cancel: &AtomicBool,
) -> Option<DynamicsOutput> {
    let len = shortest_len(channels);
    let n = channels.len();
    let mut trace = TraceBuilder::new(len, sample_rate, 0.0);
    let mut out: Vec<Vec<f32>> = channels.iter().map(|_| Vec::with_capacity(len)).collect();
    if len == 0 {
        return Some(DynamicsOutput {
            channels: out,
            trace: trace.finish(),
        });
    }
    let mut detectors: Vec<Detector> = (0..n)
        .map(|_| {
            Detector::new(
                sample_rate,
                s.detection,
                s.rms_ms,
                Some(GATE_PEAK_RELEASE_MS),
                s.key_hpf_hz,
                s.key_lpf_hz,
            )
        })
        .collect();
    let floor = db_to_amp(s.range_db.min(0.0));
    let open_at = s.threshold_db;
    let close_at = s.close_db();
    let attack = coeff(s.attack_ms, sample_rate);
    let release = coeff(s.release_ms, sample_rate);
    let hold = frames(s.hold_ms, sample_rate);
    let la = frames(s.lookahead_ms, sample_rate);
    let link = (s.link_pct / 100.0).clamp(0.0, 1.0);
    let mut levels = vec![0.0f32; n];
    let mut open = vec![false; n];
    let mut held = vec![0usize; n];
    let mut gain = vec![floor; n];
    for i in 0..len + la {
        if i % CANCEL_STRIDE == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let lead = channels.iter().map(|c| if i < len { c[i] } else { 0.0 });
        linked_levels(&mut detectors, lead, link, &mut levels);
        for c in 0..n {
            let level_db = amp_to_db(levels[c]);
            if !open[c] && level_db >= open_at {
                open[c] = true;
                held[c] = hold;
            } else if open[c] {
                if level_db >= close_at {
                    held[c] = hold;
                } else if held[c] > 0 {
                    held[c] -= 1;
                } else {
                    open[c] = false;
                }
            }
            let target = if open[c] { 1.0 } else { floor };
            let k = if target > gain[c] { attack } else { release };
            gain[c] = target + k * (gain[c] - target);
        }
        if i < la {
            continue;
        }
        let at = i - la;
        let mut peak = 0.0f32;
        let mut lowest = 1.0f32;
        for ((dst, src), g) in out.iter_mut().zip(channels).zip(&gain) {
            dst.push(src[at] * g);
            peak = peak.max(src[at].abs());
            lowest = lowest.min(*g);
        }
        trace.push(peak, amp_to_db(lowest));
    }
    Some(DynamicsOutput {
        channels: out,
        trace: trace.finish(),
    })
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

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|x| x * x).sum::<f32>() / v.len().max(1) as f32).sqrt()
    }

    fn peak(v: &[f32]) -> f32 {
        v.iter().fold(0.0f32, |m, x| m.max(x.abs()))
    }

    fn go() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn the_soft_knee_is_continuous_and_meets_the_hard_curve() {
        let s = CompressorSettings {
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 10.0,
            ..CompressorSettings::default()
        };
        let mut prev = s.transfer_db(-60.0);
        let mut level = -60.0f32;
        while level < 0.0 {
            level += 0.05;
            let now = s.transfer_db(level);
            assert!(
                (now - prev).abs() < 0.06,
                "jump at {level}: {prev} -> {now}"
            );
            prev = now;
        }
        // Outside the knee it is the hard curve.
        assert!((s.static_gain_db(-30.0)).abs() < 1e-6);
        assert!(
            (s.static_gain_db(-4.0) - (-12.0)).abs() < 1e-4,
            "{}",
            s.static_gain_db(-4.0)
        );
    }

    #[test]
    fn a_steady_tone_is_reduced_by_the_ratio() {
        let s = CompressorSettings {
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 0.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            detection: Detection::Rms,
            rms_ms: 20.0,
            ..CompressorSettings::default()
        };
        // A sine at -4 dB RMS: 16 dB over, so 12 dB of reduction.
        let amp = 10f32.powf(-4.0 / 20.0) * std::f32::consts::SQRT_2;
        let src = sine(500.0, SR as usize, amp);
        let out = compress(&[&src], SR, &s, &go()).unwrap();
        let tail = SR as usize / 2;
        let change = 20.0 * (rms(&out.channels[0][tail..]) / rms(&src[tail..])).log10();
        assert!((change + 12.0).abs() < 1.0, "{change}");
        assert!((out.trace.max_reduction_db() - 12.0).abs() < 1.5);
    }

    #[test]
    fn a_dry_mix_is_bit_identical() {
        let s = CompressorSettings {
            mix_pct: 0.0,
            threshold_db: -40.0,
            ..CompressorSettings::default()
        };
        assert!(s.is_neutral());
        let src = sine(300.0, 9_600, 0.8);
        let out = compress(&[&src], SR, &s, &go()).unwrap();
        assert_eq!(out.channels[0], src);
    }

    #[test]
    fn auto_makeup_restores_half_the_full_scale_reduction() {
        let s = CompressorSettings {
            threshold_db: -18.0,
            ratio: 3.0,
            knee_db: 0.0,
            auto_makeup: true,
            makeup_db: 99.0,
            ..CompressorSettings::default()
        };
        assert!((s.effective_makeup_db() - 6.0).abs() < 1e-4);
    }

    #[test]
    fn lookahead_catches_the_onset_a_slow_attack_misses() {
        let mut src = vec![0.0f32; SR as usize / 10];
        src.extend(sine(1_000.0, SR as usize / 10, 0.9));
        let base = CompressorSettings {
            threshold_db: -30.0,
            ratio: 20.0,
            knee_db: 0.0,
            attack_ms: 2.0,
            ..CompressorSettings::default()
        };
        let onset = SR as usize / 10;
        let window = onset..onset + 96;
        let plain = compress(&[&src], SR, &base, &go()).unwrap();
        let ahead = compress(
            &[&src],
            SR,
            &CompressorSettings {
                lookahead_ms: 5.0,
                ..base
            },
            &go(),
        )
        .unwrap();
        assert!(peak(&ahead.channels[0][window.clone()]) < peak(&plain.channels[0][window]) * 0.7);
    }

    #[test]
    fn a_linked_compressor_holds_the_image() {
        let s = CompressorSettings {
            threshold_db: -30.0,
            ratio: 8.0,
            link_pct: 100.0,
            ..CompressorSettings::default()
        };
        let left = sine(400.0, SR as usize / 2, 0.9);
        let right: Vec<f32> = left.iter().map(|v| v * 0.1).collect();
        let out = compress(&[&left, &right], SR, &s, &go()).unwrap();
        let tail = SR as usize / 4;
        let ratio_in = rms(&left[tail..]) / rms(&right[tail..]);
        let ratio_out = rms(&out.channels[0][tail..]) / rms(&out.channels[1][tail..]);
        assert!(
            (ratio_in - ratio_out).abs() < 0.01 * ratio_in,
            "{ratio_in} vs {ratio_out}"
        );
    }

    #[test]
    fn the_limiter_keeps_every_sample_under_the_ceiling() {
        let mut src = sine(997.0, SR as usize, 0.5);
        for (i, v) in src.iter_mut().enumerate() {
            if i % 4_800 < 40 {
                *v *= 3.5; // transients well over full scale
            }
        }
        let s = LimiterSettings {
            input_gain_db: 6.0,
            ceiling_db: -1.0,
            true_peak: false,
            ..LimiterSettings::default()
        };
        let out = limit(&[&src], SR, &s, &go()).unwrap();
        let ceiling = 10f32.powf(-1.0 / 20.0);
        assert!(peak(&out.channels[0]) <= ceiling + 1e-6);
        assert!(out.trace.max_reduction_db() > 6.0);
        // ...and releases back up between transients.
        let quiet = &out.trace.gain_db[out.trace.gain_db.len() / 2..];
        assert!(quiet.iter().any(|g| *g > -6.5), "never released");
    }

    #[test]
    fn true_peak_mode_keeps_the_peaks_between_samples_down_too() {
        // A tone at a quarter of the rate, phased so every sample misses the crest.
        let src: Vec<f32> = (0..SR as usize / 2)
            .map(|i| ((i as f32 + 0.5) * std::f32::consts::FRAC_PI_2).sin() * 0.99)
            .collect();
        let s = LimiterSettings {
            ceiling_db: -1.0,
            true_peak: true,
            ..LimiterSettings::default()
        };
        let out = limit(&[&src], SR, &s, &go()).unwrap();
        let ceiling = 10f32.powf(-0.9 / 20.0);
        assert!(true_peak(&out.channels[0]) <= ceiling);
    }

    /// The highest inter-sample peak of `y` (the 4x estimate), away from
    /// its ends.
    fn true_peak(y: &[f32]) -> f32 {
        let taps = true_peak_taps();
        let chans = [y];
        let mut frames = Plain::new(&chans);
        (TRUE_PEAK_HALF_TAPS..y.len() - TRUE_PEAK_HALF_TAPS)
            .map(|j| peak_at(&mut frames, 0, j, Some(&taps)))
            .fold(0.0f32, f32::max)
    }

    /// The limiter as it was before oversampling: reading the buffer
    /// directly. The plain path must render exactly this.
    fn reference_limit(
        channels: &[&[f32]],
        sample_rate: u32,
        s: &LimiterSettings,
        tracks: &Tracks<LimiterParam>,
    ) -> Vec<Vec<f32>> {
        fn peak_at_slice(x: &[f32], j: usize, taps: Option<&TruePeakTaps>) -> f32 {
            let mut peak = x[j].abs();
            if let Some(taps) = taps {
                let first = j as isize - (TRUE_PEAK_HALF_TAPS as isize - 1);
                for phase in taps {
                    let mut acc = 0.0f32;
                    for (k, tap) in phase.iter().enumerate() {
                        let idx = first + k as isize;
                        if idx >= 0 && (idx as usize) < x.len() {
                            acc += x[idx as usize] * tap;
                        }
                    }
                    peak = peak.max(acc.abs());
                }
            }
            peak
        }
        let len = shortest_len(channels);
        let n = channels.len();
        let mut out: Vec<Vec<f32>> = channels.iter().map(|_| Vec::with_capacity(len)).collect();
        let window = frames(s.lookahead_ms, sample_rate).max(1);
        let taps = s.true_peak.then(true_peak_taps);
        let chains = if s.link { 1 } else { n };
        let mut at_peak = LimiterAt::new(s, tracks, Clock::plain(0), sample_rate);
        let mut at_out = LimiterAt::new(s, tracks, Clock::plain(0), sample_rate);
        let mut gains: Vec<LookaheadGain> = (0..chains)
            .map(|_| LookaheadGain::new(window, at_out.release))
            .collect();
        let required = |j: usize, chain: usize, gin: f32, ceiling: f32| -> f32 {
            let peak = if s.link {
                channels
                    .iter()
                    .fold(0.0f32, |m, c| m.max(peak_at_slice(c, j, taps.as_ref())))
            } else {
                peak_at_slice(channels[chain], j, taps.as_ref())
            } * gin;
            if peak > ceiling {
                ceiling / peak
            } else {
                1.0
            }
        };
        for (c, g) in gains.iter_mut().enumerate() {
            let first = (0..window.min(len))
                .map(|j| {
                    let (gin, ceiling, _) = at_peak.at(j);
                    required(j, c, gin, ceiling)
                })
                .fold(1.0f32, f32::min);
            g.prime(first);
            for j in 0..(window - 1).min(len) {
                let (gin, ceiling, _) = at_peak.at(j);
                g.require(j, required(j, c, gin, ceiling));
            }
        }
        let mut chain_gain = vec![1.0f32; chains];
        for i in 0..len {
            let ahead = i + window - 1;
            let peak_settings = (ahead < len).then(|| at_peak.at(ahead));
            let (gin, ceiling, release) = at_out.at(i);
            for (c, g) in gains.iter_mut().enumerate() {
                if let Some((ahead_gin, ahead_ceiling, _)) = peak_settings {
                    g.require(ahead, required(ahead, c, ahead_gin, ahead_ceiling));
                }
                g.release = release;
                chain_gain[c] = g.gain_at(i);
            }
            for (ci, (dst, src)) in out.iter_mut().zip(channels).enumerate() {
                let g = chain_gain[if s.link { 0 } else { ci }];
                dst.push((src[i] * gin * g).clamp(-ceiling, ceiling));
            }
        }
        out
    }

    #[test]
    fn the_plain_limiter_renders_exactly_what_it_did_before_oversampling() {
        let mut left = sine(997.0, SR as usize / 2, 0.6);
        for (i, v) in left.iter_mut().enumerate() {
            if i % 3_000 < 30 {
                *v *= 2.5;
            }
        }
        let right = sine(331.0, SR as usize / 2, 0.9);
        let mut tracks = Tracks::none();
        tracks.push(
            LimiterParam::Ceiling,
            curve(Scale::Linear, &[(0, -1.0), (SR as usize / 4, -9.0)]),
        );
        tracks.push(LimiterParam::Release, curve(Scale::Log, &[(0, 10.0), (20_000, 200.0)]));
        let base = LimiterSettings {
            input_gain_db: 9.0,
            ..LimiterSettings::default()
        };
        for (name, s, tracks) in [
            ("default", base, Tracks::none()),
            ("sample peak", LimiterSettings { true_peak: false, ..base }, Tracks::none()),
            ("unlinked", LimiterSettings { link: false, ..base }, Tracks::none()),
            ("automated", base, tracks),
        ] {
            let chans = [left.as_slice(), right.as_slice()];
            let got = limit_automated(&chans, SR, &s, &tracks, 0, &go()).unwrap();
            assert_eq!(got.channels, reference_limit(&chans, SR, &s, &tracks), "{name}");
        }
    }

    /// The limiter at every oversampling factor.
    const FACTORS: [Oversampling; 4] = [
        Oversampling::X2,
        Oversampling::X4,
        Oversampling::X8,
        Oversampling::X16,
    ];

    #[test]
    fn an_oversampled_limiter_holds_the_ceiling_at_and_between_samples() {
        // A tone at a quarter of the rate, phased so every sample misses
        // the crest, pushed 6 dB into the ceiling.
        let src: Vec<f32> = (0..SR as usize / 4)
            .map(|i| ((i as f32 + 0.5) * std::f32::consts::FRAC_PI_2).sin() * 0.7)
            .collect();
        let ceiling = 10f32.powf(-1.0 / 20.0);
        for os in FACTORS {
            let s = LimiterSettings {
                input_gain_db: 6.0,
                oversampling: os,
                ..LimiterSettings::default()
            };
            let out = limit(&[&src], SR, &s, &go()).unwrap();
            let y = &out.channels[0];
            assert_eq!(y.len(), src.len());
            assert!(peak(y) <= ceiling + 1e-6, "{os:?}: sample peak {}", peak(y));
            let over_db = 20.0 * (true_peak(y) / ceiling).log10();
            assert!(over_db < 0.1, "{os:?}: inter-sample peak {over_db:.3} dB over");
            // The crest is 0.7 between samples: pushed to 1.4 against 0.89.
            assert!(out.trace.max_reduction_db() > 3.5, "{os:?}");
        }
    }

    #[test]
    fn an_oversampled_limiter_with_nothing_to_catch_gives_the_audio_back() {
        let src = sine(1_000.0, SR as usize / 4, 0.3);
        for os in FACTORS {
            let s = LimiterSettings {
                oversampling: os,
                ..LimiterSettings::default()
            };
            let out = limit(&[&src], SR, &s, &go()).unwrap();
            assert!(out.trace.max_reduction_db() < 1e-4, "{os:?}");
            let y = &out.channels[0];
            let worst = src[100..src.len() - 100]
                .iter()
                .zip(&y[100..src.len() - 100])
                .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(worst < 1e-4, "{os:?}: {worst}");
        }
    }

    /// Where the energy of `x` sits, in samples.
    fn energy_centre(x: &[f32]) -> f64 {
        let (mut m, mut e) = (0.0f64, 0.0f64);
        for (i, v) in x.iter().enumerate() {
            let p = f64::from(*v).powi(2);
            m += i as f64 * p;
            e += p;
        }
        m / e.max(1e-30)
    }

    /// A 2 ms burst of 3 kHz centred on `at`, in `len` samples of silence.
    fn burst(len: usize, at: usize, amp: f32) -> Vec<f32> {
        let half = 48;
        (0..len)
            .map(|i| {
                let d = i as f32 - at as f32;
                if d.abs() >= half as f32 {
                    return 0.0;
                }
                let env = 0.5 * (1.0 + (std::f32::consts::PI * d / half as f32).cos());
                env * (d / SR as f32 * 3_000.0 * std::f32::consts::TAU).sin() * amp
            })
            .collect()
    }

    #[test]
    fn oversampling_moves_nothing_in_time() {
        // Against the same tool at the buffer's rate: the filters add no
        // delay, so the burst comes out where it did (within half a sample).
        let src = burst(SR as usize / 10, 2_400, 0.9);
        let lim = LimiterSettings {
            input_gain_db: 12.0,
            ..LimiterSettings::default()
        };
        // The clip alone: the hard ceiling's limiter rides the level.
        let clipper = ClipperSettings {
            input_gain_db: 12.0,
            oversampling: Oversampling::Off,
            hard_ceiling: false,
            ..ClipperSettings::default()
        };
        let lim_at = energy_centre(&limit(&[&src], SR, &lim, &go()).unwrap().channels[0]);
        let clip_at = energy_centre(&clip(&[&src], SR, &clipper, &go()).unwrap().channels[0]);
        assert!((clip_at - energy_centre(&src)).abs() < 0.5, "a symmetric clip of a symmetric burst");
        for os in FACTORS {
            let s = LimiterSettings {
                oversampling: os,
                ..lim
            };
            let y = &limit(&[&src], SR, &s, &go()).unwrap().channels[0];
            assert!((energy_centre(y) - lim_at).abs() < 0.5, "limiter {os:?}");
            let s = ClipperSettings {
                oversampling: os,
                ..clipper
            };
            let y = &clip(&[&src], SR, &s, &go()).unwrap().channels[0];
            assert!((energy_centre(y) - clip_at).abs() < 0.5, "clipper {os:?}");
        }
    }

    #[test]
    fn an_oversampled_ceiling_follows_its_curve() {
        let len = SR as usize;
        let src = sine(997.0, len, 0.9);
        let mut tracks = Tracks::none();
        tracks.push(
            LimiterParam::Ceiling,
            curve(Scale::Linear, &[(len / 2 - 1, -1.0), (len / 2, -20.0)]),
        );
        let s = LimiterSettings {
            oversampling: Oversampling::X4,
            ..LimiterSettings::default()
        };
        let y = &limit_automated(&[&src], SR, &s, &tracks, 0, &go()).unwrap().channels[0];
        assert!(peak(&y[..len / 2]) <= 10f32.powf(-1.0 / 20.0) + 1e-6);
        let settle = len / 2 + SR as usize / 10;
        assert!(peak(&y[settle..]) <= 10f32.powf(-20.0 / 20.0) + 1e-6);
        assert!(peak(&y[settle..]) > 10f32.powf(-21.0 / 20.0), "held at it");
    }

    // ---- Clipper ----------------------------------------------------------

    #[test]
    fn a_hard_clip_cuts_at_the_ceiling_and_leaves_the_rest() {
        let src = sine(441.0, SR as usize / 4, 0.9);
        let s = ClipperSettings {
            ceiling_db: -6.0,
            oversampling: Oversampling::Off,
            ..ClipperSettings::default()
        };
        let out = clip(&[&src], SR, &s, &go()).unwrap();
        let ceiling = 10f32.powf(-6.0 / 20.0);
        let y = &out.channels[0];
        assert!((peak(y) - ceiling).abs() < 1e-6);
        for (a, b) in src.iter().zip(y) {
            if a.abs() <= ceiling {
                assert_eq!(a, b);
            }
        }
        // 0.9 against 0.5: about 5 dB taken off the crests.
        let taken = out.trace.max_reduction_db();
        assert!((taken - 20.0 * (0.9 / ceiling).log10()).abs() < 0.1, "{taken}");
    }

    #[test]
    fn the_knee_bends_smoothly_up_to_the_ceiling_without_reaching_it() {
        let ceiling = 0.5f32;
        let knee_start = ceiling * 10f32.powf(-6.0 / 20.0);
        let mut prev = 0.0f32;
        for i in 1..4_000 {
            let u = i as f32 / 1_000.0;
            let y = clip_sample(u, knee_start, ceiling);
            if u <= knee_start {
                assert_eq!(y, u);
            }
            assert!(y <= ceiling, "{u} -> {y}");
            assert!(y >= prev, "falls at {u}");
            assert!(y - prev <= 1.0 / 1_000.0 + 1e-6, "steeper than 1:1 at {u}");
            assert_eq!(clip_sample(-u, knee_start, ceiling), -y);
            prev = y;
        }
        assert!(prev > ceiling * 0.99, "approaches the ceiling");
    }

    #[test]
    fn a_clipper_with_nothing_to_clip_changes_nothing() {
        let src = sine(440.0, 9_600, 0.2);
        let off = ClipperSettings {
            oversampling: Oversampling::Off,
            ..ClipperSettings::default()
        };
        let out = clip(&[&src], SR, &off, &go()).unwrap();
        assert_eq!(out.channels[0], src);
        assert!(out.trace.max_reduction_db() < 1e-4);
        let out = clip(&[&src], SR, &ClipperSettings::default(), &go()).unwrap();
        assert!(out.trace.max_reduction_db() < 1e-3, "{}", out.trace.max_reduction_db());
    }

    /// The power of `x` at `hz` (Hann-windowed Goertzel).
    fn power_at(x: &[f32], hz: f64) -> f64 {
        let n = x.len() as f64;
        let w = std::f64::consts::TAU * hz / f64::from(SR);
        let k = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for (i, v) in x.iter().enumerate() {
            let win = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n).cos();
            let s0 = f64::from(*v) * win + k * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        s1 * s1 + s2 * s2 - k * s1 * s2
    }

    /// How loud the folded-back harmonics of a clipped 4.1 kHz tone are
    /// against the tone, in dB. Its odd harmonics from the 7th land, at
    /// 48 kHz, on 19.3, 11.1, 2.9, 5.3 and 13.5 kHz -- none of them a harmonic.
    fn aliasing_db(s: &ClipperSettings) -> f64 {
        let src = sine(4_100.0, SR as usize, 0.9);
        let y = &clip(&[&src], SR, s, &go()).unwrap().channels[0];
        let folded: f64 = [19_300.0, 11_100.0, 2_900.0, 5_300.0, 13_500.0]
            .iter()
            .map(|hz| power_at(y, *hz))
            .sum();
        10.0 * (folded / power_at(y, 4_100.0)).log10()
    }

    #[test]
    fn oversampled_clipping_folds_far_less_back() {
        let plain = ClipperSettings {
            ceiling_db: -6.0,
            oversampling: Oversampling::Off,
            ..ClipperSettings::default()
        };
        let off = aliasing_db(&plain);
        let x8 = aliasing_db(&ClipperSettings {
            oversampling: Oversampling::X8,
            hard_ceiling: false,
            ..plain
        });
        let x8_hard = aliasing_db(&ClipperSettings {
            oversampling: Oversampling::X8,
            hard_ceiling: true,
            ..plain
        });
        eprintln!("aliasing: off {off:.1} dB, 8x {x8:.1} dB, 8x hard ceiling {x8_hard:.1} dB");
        assert!(off > -40.0, "the plain clip should alias: {off:.1} dB");
        assert!(x8 < off - 40.0, "8x: {x8:.1} dB against {off:.1} dB");
        // The hard ceiling's limiter rides the level smoothly: clamping
        // instead brought this back to about -45 dB.
        assert!(x8_hard < off - 50.0, "8x hard ceiling: {x8_hard:.1} dB against {off:.1} dB");
    }

    #[test]
    fn the_hard_ceiling_trims_what_the_filter_rounds_over() {
        let src = sine(1_000.0, SR as usize / 4, 0.9);
        let ceiling = 10f32.powf(-6.0 / 20.0);
        let s = ClipperSettings {
            input_gain_db: 12.0,
            ceiling_db: -6.0,
            oversampling: Oversampling::X4,
            hard_ceiling: false,
            ..ClipperSettings::default()
        };
        let soft = clip(&[&src], SR, &s, &go()).unwrap();
        assert!(peak(&soft.channels[0]) > ceiling * 1.01, "a band-limited square overshoots");
        let hard = clip(
            &[&src],
            SR,
            &ClipperSettings {
                hard_ceiling: true,
                ..s
            },
            &go(),
        )
        .unwrap();
        assert!(peak(&hard.channels[0]) <= ceiling + 1e-6);
        // Turned down only as far as the overshoot needs.
        let held = 20.0 * (rms(&hard.channels[0][4_800..]) / rms(&soft.channels[0][4_800..])).log10();
        assert!(held > -1.5 && held < 0.0, "{held:.2} dB");
    }

    #[test]
    fn a_clipper_curve_moves_the_ceiling_where_it_says() {
        let len = SR as usize;
        let src = sine(997.0, len, 0.9);
        let s = ClipperSettings {
            oversampling: Oversampling::Off,
            ..ClipperSettings::default()
        };
        let plain = clip(&[&src], SR, &s, &go()).unwrap();
        let mut tracks = Tracks::none();
        tracks.push(ClipperParam::Ceiling, curve(Scale::Linear, &[(0, s.ceiling_db)]));
        let same = clip_automated(&[&src], SR, &s, &tracks, 0, &go()).unwrap();
        assert_eq!(plain.channels, same.channels, "a constant curve is the setting");
        let mut tracks = Tracks::none();
        tracks.push(
            ClipperParam::Ceiling,
            curve(Scale::Linear, &[(len / 2 - 1, -1.0), (len / 2, -12.0)]),
        );
        let y = &clip_automated(&[&src], SR, &s, &tracks, 0, &go()).unwrap().channels[0];
        assert!((peak(&y[..len / 2 - AUTOMATION_BLOCK]) - 10f32.powf(-1.0 / 20.0)).abs() < 1e-6);
        assert!((peak(&y[len / 2 + AUTOMATION_BLOCK..]) - 10f32.powf(-12.0 / 20.0)).abs() < 1e-6);
    }

    /// What each factor costs: ten seconds of stereo through the limiter
    /// (true peak on) and the clipper. Run with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn oversampling_cost() {
        let len = SR as usize * 10;
        let left = sine(997.0, len, 0.9);
        let right = sine(331.0, len, 0.9);
        let chans = [left.as_slice(), right.as_slice()];
        for os in Oversampling::ALL {
            let lim = LimiterSettings {
                input_gain_db: 6.0,
                oversampling: os,
                ..LimiterSettings::default()
            };
            let t = std::time::Instant::now();
            limit(&chans, SR, &lim, &go()).unwrap();
            let lim_secs = t.elapsed().as_secs_f64();
            let clipper = ClipperSettings {
                input_gain_db: 6.0,
                oversampling: os,
                ..ClipperSettings::default()
            };
            let t = std::time::Instant::now();
            clip(&chans, SR, &clipper, &go()).unwrap();
            let clip_secs = t.elapsed().as_secs_f64();
            eprintln!(
                "{:>4}: limiter {lim_secs:.2} s, clipper {clip_secs:.2} s per 10 s of stereo",
                os.label()
            );
        }
    }

    #[test]
    fn the_transfer_curve_is_the_clip() {
        let s = ClipperSettings {
            input_gain_db: 6.0,
            ceiling_db: -3.0,
            knee_db: 0.0,
            ..ClipperSettings::default()
        };
        assert!((s.transfer_db(-20.0) - -14.0).abs() < 1e-4);
        assert!((s.transfer_db(-6.0) - -3.0).abs() < 1e-4);
        assert!((s.transfer_db(0.0) - -3.0).abs() < 1e-4);
        let soft = ClipperSettings { knee_db: 6.0, ..s };
        assert!(soft.transfer_db(0.0) < -3.0);
        assert!((soft.transfer_db(-20.0) - -14.0).abs() < 1e-4, "below the knee");
    }

    #[test]
    fn a_limiter_with_nothing_to_catch_changes_nothing() {
        let src = sine(440.0, 9_600, 0.2);
        let out = limit(&[&src], SR, &LimiterSettings::default(), &go()).unwrap();
        assert_eq!(out.channels[0], src);
        assert!(out.trace.max_reduction_db() < 1e-4);
    }

    fn curve(scale: Scale, points: &[(usize, f32)]) -> crate::automation::Curve {
        crate::automation::Curve::new(points.to_vec(), scale)
    }

    #[test]
    fn a_constant_curve_is_the_static_setting() {
        let src = sine(500.0, SR as usize / 2, 0.8);
        let s = CompressorSettings {
            threshold_db: -24.0,
            ..CompressorSettings::default()
        };
        let plain = compress(&[&src], SR, &s, &go()).unwrap();
        let mut tracks = Tracks::none();
        tracks.push(CompParam::Threshold, curve(Scale::Linear, &[(0, -24.0)]));
        tracks.push(CompParam::Attack, curve(Scale::Log, &[(0, s.attack_ms)]));
        let auto = compress_automated(&[&src], SR, &s, &tracks, 0, &go()).unwrap();
        for (a, b) in plain.channels[0].iter().zip(&auto.channels[0]) {
            assert!((a - b).abs() < 1e-6);
        }
        let lim = LimiterSettings {
            input_gain_db: 12.0,
            ..LimiterSettings::default()
        };
        let plain = limit(&[&src], SR, &lim, &go()).unwrap();
        let mut tracks = Tracks::none();
        tracks.push(LimiterParam::Ceiling, curve(Scale::Linear, &[(0, -1.0)]));
        let auto = limit_automated(&[&src], SR, &lim, &tracks, 0, &go()).unwrap();
        for (a, b) in plain.channels[0].iter().zip(&auto.channels[0]) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn a_falling_threshold_compresses_the_second_half_harder() {
        let len = SR as usize * 2;
        let src = sine(300.0, len, 0.5);
        let s = CompressorSettings {
            threshold_db: -6.0,
            ratio: 8.0,
            knee_db: 0.0,
            ..CompressorSettings::default()
        };
        // -6 dB for the first second, then a step down to -36 dB.
        let mut tracks = Tracks::none();
        tracks.push(
            CompParam::Threshold,
            curve(Scale::Linear, &[(len / 2 - 1, -6.0), (len / 2, -36.0)]),
        );
        let out = compress_automated(&[&src], SR, &s, &tracks, 0, &go()).unwrap();
        let q = len / 4;
        let first = rms(&out.channels[0][q..2 * q]);
        let second = rms(&out.channels[0][3 * q..]);
        assert!(20.0 * (second / first).log10() < -15.0, "{first} {second}");
        // The curve's points are buffer samples: rendered from half way in,
        // the same audio starts already past the step.
        let tail = &src[len / 2..];
        let late = compress_automated(&[tail], SR, &s, &tracks, len / 2, &go()).unwrap();
        let late_rms = rms(&late.channels[0][q..]);
        assert!((20.0 * (late_rms / second).log10()).abs() < 0.5, "{late_rms} vs {second}");
    }

    #[test]
    fn an_automated_ceiling_holds_every_stretch_under_its_own_level() {
        let len = SR as usize * 2;
        let src = sine(997.0, len, 0.9);
        let s = LimiterSettings {
            ceiling_db: -1.0,
            true_peak: false,
            ..LimiterSettings::default()
        };
        let mut tracks = Tracks::none();
        tracks.push(
            LimiterParam::Ceiling,
            curve(Scale::Linear, &[(0, -1.0), (len / 2, -1.0), (len / 2 + 1, -20.0)]),
        );
        tracks.push(LimiterParam::InputGain, curve(Scale::Linear, &[(0, 0.0), (len, 12.0)]));
        let out = limit_automated(&[&src], SR, &s, &tracks, 0, &go()).unwrap();
        let y = &out.channels[0];
        assert!(peak(&y[..len / 2]) <= 10f32.powf(-1.0 / 20.0) + 1e-6);
        // Past the step plus a lookahead, under the new ceiling.
        let settle = len / 2 + SR as usize / 10;
        assert!(peak(&y[settle..]) <= 10f32.powf(-20.0 / 20.0) + 1e-6);
        assert!(peak(&y[settle..]) > 10f32.powf(-21.0 / 20.0), "held at it");
    }

    #[test]
    fn the_deepest_settings_hold_a_quiet_tone_flat_at_a_low_ceiling() {
        let s = LimiterSettings {
            input_gain_db: *LimiterSettings::INPUT_GAIN_DB.end(),
            ceiling_db: *LimiterSettings::CEILING_DB.start(),
            ..LimiterSettings::default()
        };
        assert_eq!((s.input_gain_db, s.ceiling_db), (48.0, -30.0));
        assert_eq!(s.gr_scale_db(), 48.0);
        // A tone at -40 dBFS, pushed 48 dB: 18 dB over a -30 dB ceiling.
        let src = sine(220.0, SR as usize, 10f32.powf(-40.0 / 20.0));
        let out = limit(&[&src], SR, &s, &go()).unwrap();
        let ceiling = 10f32.powf(-30.0 / 20.0);
        assert!(peak(&out.channels[0]) <= ceiling + 1e-6);
        let tail = &out.channels[0][SR as usize / 2..];
        assert!(peak(tail) > ceiling * 0.9, "held at the ceiling, not under it");
        assert!(out.trace.max_reduction_db() > 15.0);
        assert_eq!(LimiterSettings::default().gr_scale_db(), 24.0);
    }

    #[test]
    fn hysteresis_stops_a_hovering_level_from_chattering() {
        // A tone wobbling 1 dB either side of the threshold.
        let len = SR as usize;
        let src: Vec<f32> = (0..len)
            .map(|i| {
                let t = i as f32 / SR as f32;
                let wobble_db = (t * 15.0 * std::f32::consts::TAU).sin();
                let amp = 10f32.powf((-30.0 + wobble_db) / 20.0);
                (t * 1_000.0 * std::f32::consts::TAU).sin() * amp
            })
            .collect();
        let count_closings = |s: &GateSettings| {
            let out = gate(&[&src], SR, s, &go()).unwrap();
            out.trace
                .gain_db
                .windows(2)
                .filter(|w| w[0] > -6.0 && w[1] <= -6.0)
                .count()
        };
        let chattery = GateSettings {
            threshold_db: -30.0,
            hysteresis_db: 0.0,
            hold_ms: 0.0,
            attack_ms: 0.1,
            release_ms: 1.0,
            ..GateSettings::default()
        };
        let steady = GateSettings {
            hysteresis_db: 4.0,
            ..chattery
        };
        assert!(count_closings(&chattery) > 5);
        assert_eq!(count_closings(&steady), 0);
    }

    #[test]
    fn the_range_is_a_floor_not_silence() {
        let src = sine(500.0, SR as usize / 2, 0.001); // -60 dBFS
        let s = GateSettings {
            threshold_db: -20.0,
            range_db: -12.0,
            ..GateSettings::default()
        };
        let out = gate(&[&src], SR, &s, &go()).unwrap();
        let tail = src.len() / 2;
        let change = 20.0 * (rms(&out.channels[0][tail..]) / rms(&src[tail..])).log10();
        assert!((change + 12.0).abs() < 0.2, "{change}");
    }

    #[test]
    fn hold_keeps_the_gate_open_after_the_sound_stops() {
        let mut src = sine(500.0, SR as usize / 10, 0.5);
        src.extend(sine(500.0, SR as usize / 5, 0.001));
        let stop = SR as usize / 10;
        let s = GateSettings {
            threshold_db: -20.0,
            hold_ms: 50.0,
            release_ms: 1.0,
            ..GateSettings::default()
        };
        let out = gate(&[&src], SR, &s, &go()).unwrap();
        let held = &out.channels[0][stop + 480..stop + 1_920]; // 10-40 ms after
        let closed = &out.channels[0][stop + 4_800..stop + 6_000]; // 100 ms after
        assert!(rms(held) > 0.0005, "closed too early");
        assert!(rms(closed) < 0.00005, "never closed");
    }

    #[test]
    fn the_trace_says_what_the_audio_got() {
        let src = sine(700.0, SR as usize, 0.9);
        let s = CompressorSettings {
            threshold_db: -24.0,
            ratio: 6.0,
            ..CompressorSettings::default()
        };
        let out = compress(&[&src], SR, &s, &go()).unwrap();
        let t = &out.trace;
        let at = SR as usize * 3 / 4;
        let i = t.index_at(at).unwrap();
        let hop = &out.channels[0][t.start + i * t.hop..t.start + (i + 1) * t.hop];
        let src_hop = &src[t.start + i * t.hop..t.start + (i + 1) * t.hop];
        let measured = 20.0 * (peak(hop) / peak(src_hop)).log10();
        assert!(
            (measured - t.gain_db[i]).abs() < 1.0,
            "{measured} vs {}",
            t.gain_db[i]
        );
        let cropped = t.crop(SR as usize / 2, SR as usize);
        assert!(cropped.start >= SR as usize / 2 - t.hop && cropped.end() >= SR as usize);
    }

    #[test]
    fn a_level_trace_reports_each_hop_s_peak() {
        let mut src = vec![0.0f32; SR as usize / 2];
        src[1_000] = 0.5;
        let t = level_trace(&[&src], SR);
        let i = t.index_at(1_000).unwrap();
        assert!((t.input_db[i] - 20.0 * 0.5f32.log10()).abs() < 1e-4);
        assert!(t.gain_db.iter().all(|g| *g == 0.0));
        assert!(t.input_db[i + 1] <= LEVEL_FLOOR_DB);
    }

    #[test]
    fn a_cancelled_run_stops() {
        let src = sine(700.0, SR as usize, 0.9);
        let stop = AtomicBool::new(true);
        assert!(compress(&[&src], SR, &CompressorSettings::default(), &stop).is_none());
        assert!(limit(&[&src], SR, &LimiterSettings::default(), &stop).is_none());
        let oversampled = LimiterSettings {
            oversampling: Oversampling::X4,
            ..LimiterSettings::default()
        };
        assert!(limit(&[&src], SR, &oversampled, &stop).is_none());
        assert!(clip(&[&src], SR, &ClipperSettings::default(), &stop).is_none());
        assert!(gate(&[&src], SR, &GateSettings::default(), &stop).is_none());
    }
}
