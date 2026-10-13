//! The editor's compressor, limiter and gate, DSP and rules only. Each one
//! takes the channels an edit is in scope for together -- so a stereo link
//! can hold the image still -- and returns, beside the audio, a
//! [`DynamicsTrace`]: what came in and how much gain was taken, hop by hop.
//! The Inspector draws that trace, so the reduction on screen is the one the
//! edit applies.
//!
//! Lookahead costs no latency here: the whole buffer is in memory, so the
//! detector simply reads ahead of the sample whose gain it decides.
//! Everything streams -- nothing allocates a buffer per sample beyond the
//! output itself, because an hour-long stereo file is 345 million samples.
//!
//! The Effect Graph's compressor and gate (`wave::process_compressor_offline`,
//! `wave::process_noise_gate_offline`) are separate and unchanged: a saved
//! graph must keep rendering what it rendered.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::eq::{Section, SectionFilter, SectionKind};

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
        let hpf = (hpf_hz > 0.0).then(|| {
            SectionFilter::new(Section::design(
                SectionKind::HighPass,
                f64::from(hpf_hz).min(nyquist_guard),
                0.0,
                butterworth,
                sr,
            ))
        });
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
    let len = shortest_len(channels);
    let n = channels.len();
    let makeup = s.effective_makeup_db();
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
    let attack = coeff(s.attack_ms, sample_rate);
    let release = coeff(s.release_ms, sample_rate);
    let la = frames(s.lookahead_ms, sample_rate);
    let mix = (s.mix_pct / 100.0).clamp(0.0, 1.0);
    let link = (s.link_pct / 100.0).clamp(0.0, 1.0);
    let mut levels = vec![0.0f32; n];
    let mut env = vec![0.0f32; n];
    for i in 0..len + la {
        if i % CANCEL_STRIDE == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let lead = channels.iter().map(|c| if i < len { c[i] } else { 0.0 });
        linked_levels(&mut detectors, lead, link, &mut levels);
        for (e, level) in env.iter_mut().zip(&levels) {
            let target = s.static_gain_db(amp_to_db(*level));
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
}

impl LimiterSettings {
    pub const INPUT_GAIN_DB: std::ops::RangeInclusive<f32> = 0.0..=24.0;
    pub const CEILING_DB: std::ops::RangeInclusive<f32> = -12.0..=0.0;
    pub const RELEASE_MS: std::ops::RangeInclusive<f32> = 1.0..=1_000.0;
    pub const LOOKAHEAD_MS: std::ops::RangeInclusive<f32> = 0.5..=10.0;
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
        }
    }
}

/// Windowed-sinc taps for the inter-sample points, per phase, for samples
/// `j - HALF + 1 ..= j + HALF` around a point between `j` and `j + 1`.
fn true_peak_taps() -> [[f32; 2 * TRUE_PEAK_HALF_TAPS]; 3] {
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

/// The largest magnitude at or just after sample `j` of `x`.
#[inline]
fn peak_at(x: &[f32], j: usize, taps: Option<&[[f32; 2 * TRUE_PEAK_HALF_TAPS]; 3]>) -> f32 {
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
    let len = shortest_len(channels);
    let n = channels.len();
    let mut trace = TraceBuilder::new(len, sample_rate, s.input_gain_db);
    let mut out: Vec<Vec<f32>> = channels.iter().map(|_| Vec::with_capacity(len)).collect();
    if len == 0 {
        return Some(DynamicsOutput {
            channels: out,
            trace: trace.finish(),
        });
    }
    let gin = db_to_amp(s.input_gain_db);
    let ceiling = db_to_amp(s.ceiling_db.min(0.0));
    let window = frames(s.lookahead_ms, sample_rate).max(1);
    let release = coeff(s.release_ms, sample_rate);
    let taps = s.true_peak.then(true_peak_taps);
    let chains = if s.link { 1 } else { n };
    let mut gains: Vec<LookaheadGain> = (0..chains)
        .map(|_| LookaheadGain::new(window, release))
        .collect();
    let required = |j: usize, chain: usize| -> f32 {
        let peak = if s.link {
            channels
                .iter()
                .fold(0.0f32, |m, c| m.max(peak_at(c, j, taps.as_ref())))
        } else {
            peak_at(channels[chain], j, taps.as_ref())
        } * gin;
        if peak > ceiling {
            ceiling / peak
        } else {
            1.0
        }
    };
    for (c, g) in gains.iter_mut().enumerate() {
        let first = (0..window.min(len))
            .map(|j| required(j, c))
            .fold(1.0f32, f32::min);
        g.prime(first);
        for j in 0..(window - 1).min(len) {
            g.require(j, required(j, c));
        }
    }
    let mut chain_gain = vec![1.0f32; chains];
    for i in 0..len {
        if i % CANCEL_STRIDE == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let ahead = i + window - 1;
        for (c, g) in gains.iter_mut().enumerate() {
            if ahead < len {
                g.require(ahead, required(ahead, c));
            }
            chain_gain[c] = g.gain_at(i);
        }
        let mut peak = 0.0f32;
        let mut lowest = 1.0f32;
        for (ci, (dst, src)) in out.iter_mut().zip(channels).enumerate() {
            let g = chain_gain[if s.link { 0 } else { ci }];
            let y = (src[i] * gin * g).clamp(-ceiling, ceiling);
            dst.push(y);
            peak = peak.max(src[i].abs());
            lowest = lowest.min(g);
        }
        trace.push(peak, amp_to_db(lowest));
    }
    Some(DynamicsOutput {
        channels: out,
        trace: trace.finish(),
    })
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
        let taps = true_peak_taps();
        let ceiling = 10f32.powf(-0.9 / 20.0);
        let y = &out.channels[0];
        for j in (TRUE_PEAK_HALF_TAPS..y.len() - TRUE_PEAK_HALF_TAPS).step_by(7) {
            assert!(
                peak_at(y, j, Some(&taps)) <= ceiling,
                "inter-sample peak at {j}"
            );
        }
    }

    #[test]
    fn a_limiter_with_nothing_to_catch_changes_nothing() {
        let src = sine(440.0, 9_600, 0.2);
        let out = limit(&[&src], SR, &LimiterSettings::default(), &go()).unwrap();
        assert_eq!(out.channels[0], src);
        assert!(out.trace.max_reduction_db() < 1e-4);
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
        assert!(gate(&[&src], SR, &GateSettings::default(), &stop).is_none());
    }
}
