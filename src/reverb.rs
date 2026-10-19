//! The editor's convolution reverb, DSP and rules only: the settings
//! (`ReverbSettings`, the ones that can follow a curve in `ReverbParam`), the
//! IR as the settings shape it, how the target's channels meet the IR's, a
//! uniformly partitioned FFT convolution of whole buffers, and the wet
//! signal's filters, width and mix with their automation.
//!
//! Only the target's audio is sent into the reverb. Its tail is what
//! `ReverbTail` says: rung on over what follows (and, at the file's end,
//! past it), rung on over what follows with the length kept, or stopped at
//! the target's end. The dry signal outside the target is never touched.
//!
//! Everything is offline: the whole buffer is in memory, so nothing waits
//! on a block and nothing moves in time.

use std::sync::atomic::{AtomicBool, Ordering};

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;
use serde::{Deserialize, Serialize};

use crate::automation::{Automatable, Evaluator, Scale, Tracks, AUTOMATION_BLOCK};
use crate::eq::{Section, SectionFilter, SectionKind};
use crate::impulse::{ImpulseResponse, IrId};

/// Partition lengths of the convolution: long enough that an IR of a few
/// seconds is a dozen partitions, short enough that a short one is not
/// mostly zeros.
const MIN_PARTITION: usize = 1_024;
const MAX_PARTITION: usize = 32_768;
/// About this many partitions per IR, between those bounds.
const PARTITIONS_PER_IR: usize = 16;
/// Blocks between cancel checks are cheap; frames between them in the mix.
const CANCEL_STRIDE: usize = 1 << 15;
/// `Cut`: the wet fades out over this much before the target's end.
const CUT_FADE_SECS: f64 = 0.1;
/// `Overlap`: a tail the file's end cuts off fades out over this much.
const OVERLAP_FADE_SECS: f64 = 0.05;
/// `Extend`: the tail is kept until it falls this far under its peak, where
/// stopping is inaudible -- the file does not grow by seconds of nothing.
const EXTEND_FLOOR_DB: f32 = -90.0;
/// A Length below 100% fades the IR out over this share of what is kept.
const LENGTH_FADE_SHARE: f64 = 0.1;
/// The low and high cuts on the wet: 12 dB/oct Butterworth.
const CUT_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[inline]
fn db_to_amp(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// What happens to the reverb's tail after the target ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReverbTail {
    /// Rings on over what follows; past the end of the file, the file grows
    /// to hold it.
    #[default]
    Extend,
    /// Rings on over what follows; the file keeps its length, and a tail the
    /// end cuts off fades out there.
    Overlap,
    /// Fades out at the target's end: nothing outside the target changes.
    Cut,
}

impl ReverbTail {
    pub const ALL: [ReverbTail; 3] = [ReverbTail::Extend, ReverbTail::Overlap, ReverbTail::Cut];

    pub fn label(self) -> &'static str {
        match self {
            ReverbTail::Extend => "Ring on",
            ReverbTail::Overlap => "Keep length",
            ReverbTail::Cut => "Stop at the end",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            ReverbTail::Extend => "The tail rings on over what follows; at the end of the file, the file grows to hold it",
            ReverbTail::Overlap => "The tail rings on over what follows; the file keeps its length (a tail the end cuts off fades out)",
            ReverbTail::Cut => "The tail fades out at the end of the target: nothing outside it changes",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReverbSettings {
    /// The impulse response; `None` is the default one
    /// (`crate::impulse::DEFAULT_IR`).
    pub ir: Option<IrId>,
    /// How much of the target is the reverb: 0 dry, 100 wet only.
    pub mix_pct: f32,
    /// Gain on what the target becomes (on the tail past it too).
    pub output_db: f32,
    /// The wet's stereo width: 0 mono, 100 as the IR has it, 200 wider.
    pub width_pct: f32,
    /// High-pass on the wet; 0 is off.
    pub low_cut_hz: f32,
    /// Low-pass on the wet; 0 is off.
    pub high_cut_hz: f32,
    pub predelay_ms: f32,
    /// How much of the IR is used, faded out at the cut.
    pub length_pct: f32,
    pub reverse: bool,
    /// Scale the IR so white noise comes out as loud as it went in: IRs are
    /// recorded at any level, and this keeps switching between them from
    /// jumping in loudness.
    pub normalize: bool,
    pub tail: ReverbTail,
}

impl ReverbSettings {
    pub const MIX_PCT: std::ops::RangeInclusive<f32> = 0.0..=100.0;
    pub const OUTPUT_DB: std::ops::RangeInclusive<f32> = -24.0..=12.0;
    pub const WIDTH_PCT: std::ops::RangeInclusive<f32> = 0.0..=200.0;
    pub const LOW_CUT_HZ: std::ops::RangeInclusive<f32> = 20.0..=2_000.0;
    pub const HIGH_CUT_HZ: std::ops::RangeInclusive<f32> = 1_000.0..=20_000.0;
    pub const PREDELAY_MS: std::ops::RangeInclusive<f32> = 0.0..=500.0;
    pub const LENGTH_PCT: std::ops::RangeInclusive<f32> = 5.0..=100.0;

    /// The IR in use: the chosen one, or the default.
    pub fn ir_or_default(&self) -> IrId {
        self.ir.unwrap_or_else(IrId::default_ir)
    }

    /// Settings that change nothing: no wet, no gain.
    pub fn is_neutral(&self) -> bool {
        self.mix_pct <= 0.0 && self.output_db.abs() < 1e-4
    }
}

impl Default for ReverbSettings {
    fn default() -> Self {
        Self {
            ir: None,
            mix_pct: 30.0,
            output_db: 0.0,
            width_pct: 100.0,
            low_cut_hz: 0.0,
            high_cut_hz: 0.0,
            predelay_ms: 0.0,
            length_pct: 100.0,
            reverse: false,
            normalize: true,
            tail: ReverbTail::Extend,
        }
    }
}

/// A reverb setting that can follow a curve: what acts on the wet after the
/// convolution. Not the IR, pre-delay, length, reverse or normalize, which
/// change the IR itself, nor the tail mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReverbParam {
    Mix,
    Output,
    Width,
    LowCut,
    HighCut,
}

impl ReverbParam {
    pub const ALL: [ReverbParam; 5] = [
        ReverbParam::Mix,
        ReverbParam::Output,
        ReverbParam::Width,
        ReverbParam::LowCut,
        ReverbParam::HighCut,
    ];

    pub fn range(self) -> std::ops::RangeInclusive<f32> {
        match self {
            ReverbParam::Mix => ReverbSettings::MIX_PCT,
            ReverbParam::Output => ReverbSettings::OUTPUT_DB,
            ReverbParam::Width => ReverbSettings::WIDTH_PCT,
            ReverbParam::LowCut => ReverbSettings::LOW_CUT_HZ,
            ReverbParam::HighCut => ReverbSettings::HIGH_CUT_HZ,
        }
    }

    pub fn scale(self) -> Scale {
        match self {
            ReverbParam::LowCut | ReverbParam::HighCut => Scale::Log,
            _ => Scale::Linear,
        }
    }
}

impl Automatable for ReverbSettings {
    type Param = ReverbParam;

    fn set_param(&mut self, param: ReverbParam, value: f32) {
        let range = param.range();
        let v = value.max(*range.start()).min(*range.end());
        match param {
            ReverbParam::Mix => self.mix_pct = v,
            ReverbParam::Output => self.output_db = v,
            ReverbParam::Width => self.width_pct = v,
            ReverbParam::LowCut => self.low_cut_hz = v,
            ReverbParam::HighCut => self.high_cut_hz = v,
        }
    }

    fn param(&self, param: ReverbParam) -> f32 {
        match param {
            ReverbParam::Mix => self.mix_pct,
            ReverbParam::Output => self.output_db,
            ReverbParam::Width => self.width_pct,
            ReverbParam::LowCut => self.low_cut_hz,
            ReverbParam::HighCut => self.high_cut_hz,
        }
    }
}

// ---------------------------------------------------------------------------
// The IR as the settings use it

/// The IR's channels cut to `length_pct` (fading out over the last tenth of
/// what is kept) and reversed if asked, and how many frames of them come
/// before time zero (none once reversed: the IR then simply plays from its
/// first frame).
fn shape(ir: &ImpulseResponse, s: &ReverbSettings) -> (Vec<Vec<f32>>, usize) {
    let lead = ir.lead;
    let len = ir.frames();
    let share = f64::from(
        s.length_pct
            .clamp(*ReverbSettings::LENGTH_PCT.start(), 100.0),
    ) / 100.0;
    let keep = ((len as f64 * share).round() as usize)
        .clamp(1, len.max(1))
        .min(len);
    let fade = if keep < len {
        ((keep as f64 * LENGTH_FADE_SHARE) as usize).max(1)
    } else {
        0
    };
    let channels = ir
        .channels
        .iter()
        .map(|c| {
            let mut v = c[..lead + keep].to_vec();
            for i in 0..fade {
                let t = (i + 1) as f64 / (fade + 1) as f64;
                v[lead + keep - fade + i] *= (0.5 + 0.5 * (std::f64::consts::PI * t).cos()) as f32;
            }
            if s.reverse {
                v.reverse();
            }
            v
        })
        .collect();
    (channels, if s.reverse { 0 } else { lead })
}

/// How the target's channels meet the IR's.
#[derive(Debug, PartialEq)]
enum Routing {
    /// Every channel through the same IR: a mono one, or the IR folded to
    /// mono.
    Mono(Vec<f32>),
    /// A stereo pair, each side through its own: L through IR 0, R through 1.
    Stereo([Vec<f32>; 2]),
    /// A stereo pair through all four paths of a true-stereo IR, in the
    /// order its four channels are written: L to L, L to R, R to L, R to R.
    TrueStereo([Vec<f32>; 4]),
}

fn route(channels_in: usize, mut ir: Vec<Vec<f32>>) -> Routing {
    let fold = |parts: &[Vec<f32>], scale: f32| -> Vec<f32> {
        let len = parts.iter().map(Vec::len).max().unwrap_or(0);
        let mut out = vec![0.0f32; len];
        for p in parts {
            for (o, v) in out.iter_mut().zip(p) {
                *o += v * scale;
            }
        }
        out
    };
    match (channels_in, ir.len()) {
        (_, 0) => Routing::Mono(Vec::new()),
        (_, 1) => Routing::Mono(ir.remove(0)),
        (2, 2) => {
            let r = ir.remove(1);
            Routing::Stereo([ir.remove(0), r])
        }
        (2, 4) => {
            let mut it = ir.into_iter();
            let mut next = || it.next().unwrap_or_default();
            Routing::TrueStereo([next(), next(), next(), next()])
        }
        // A mono input feeds both of a true-stereo IR's inputs: each output
        // is the sum of two paths, and the two outputs are averaged.
        (_, 4) => Routing::Mono(fold(&ir, 0.5)),
        (_, n) => Routing::Mono(fold(&ir, 1.0 / n as f32)),
    }
}

fn energy(v: &[f32]) -> f64 {
    v.iter().map(|x| f64::from(*x) * f64::from(*x)).sum()
}

/// The gain that makes white noise come out as loud as it goes in, averaged
/// over the outputs. 1 for a silent IR.
fn normalize_gain(routing: &Routing) -> f32 {
    let e = match routing {
        Routing::Mono(h) => energy(h),
        Routing::Stereo([l, r]) => (energy(l) + energy(r)) / 2.0,
        Routing::TrueStereo([ll, lr, rl, rr]) => {
            (energy(ll) + energy(rl) + energy(lr) + energy(rr)) / 2.0
        }
    };
    if e > 0.0 {
        (1.0 / e.sqrt()) as f32
    } else {
        1.0
    }
}

// ---------------------------------------------------------------------------
// Convolution

/// The partition length for an IR of `ir_len` frames.
fn partition_len(ir_len: usize) -> usize {
    (ir_len / PARTITIONS_PER_IR)
        .max(1)
        .next_power_of_two()
        .clamp(MIN_PARTITION, MAX_PARTITION)
}

/// One path of a convolution: input `from` through IR `ir` into output `to`.
#[derive(Clone, Copy, Debug)]
struct Path {
    from: usize,
    ir: usize,
    to: usize,
}

/// `inputs` through `paths` into `outputs` outputs of `out_len` frames,
/// by uniformly partitioned overlap-save: every input block is transformed
/// once and multiplied with every partition of every IR it feeds. `None`
/// when cancelled.
fn convolve(
    inputs: &[&[f32]],
    irs: &[&[f32]],
    paths: &[Path],
    outputs: usize,
    out_len: usize,
    cancel: &AtomicBool,
) -> Option<Vec<Vec<f32>>> {
    let ir_len = irs.iter().map(|h| h.len()).max().unwrap_or(0);
    let mut out = vec![vec![0.0f32; out_len]; outputs];
    if ir_len == 0 || out_len == 0 {
        return Some(out);
    }
    let p = partition_len(ir_len);
    let n = 2 * p;
    let bins = p + 1;
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);
    let mut time = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut iscratch = ifft.make_scratch_vec();
    // Each IR cut into partitions and transformed, 1/n folded in (realfft
    // does not normalise).
    let norm = 1.0 / n as f32;
    let parts: Vec<Vec<Vec<Complex<f32>>>> = irs
        .iter()
        .map(|h| {
            h.chunks(p)
                .map(|chunk| {
                    time.fill(0.0);
                    for (t, v) in time.iter_mut().zip(chunk) {
                        *t = v * norm;
                    }
                    let _ = fft.process_with_scratch(&mut time, &mut spec, &mut scratch);
                    spec.clone()
                })
                .collect()
        })
        .collect();
    let k_max = parts.iter().map(Vec::len).max().unwrap_or(1).max(1);
    // The frequency-domain delay line of each input: its last `k_max` blocks.
    let mut fdl = vec![vec![vec![Complex::new(0.0f32, 0.0); bins]; k_max]; inputs.len()];
    let mut prev = vec![vec![0.0f32; p]; inputs.len()];
    let mut acc = vec![Complex::new(0.0f32, 0.0); bins];
    let blocks = out_len.div_ceil(p);
    for b in 0..blocks {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let slot = b % k_max;
        for (i, x) in inputs.iter().enumerate() {
            let start = (b * p).min(x.len());
            let end = ((b + 1) * p).min(x.len());
            time[..p].copy_from_slice(&prev[i]);
            time[p..].fill(0.0);
            time[p..p + (end - start)].copy_from_slice(&x[start..end]);
            prev[i].copy_from_slice(&time[p..]);
            let _ = fft.process_with_scratch(&mut time, &mut fdl[i][slot], &mut scratch);
        }
        for (o, dst) in out.iter_mut().enumerate() {
            acc.fill(Complex::new(0.0, 0.0));
            let mut any = false;
            for path in paths.iter().filter(|path| path.to == o) {
                for (k, h) in parts[path.ir].iter().enumerate().take(b + 1) {
                    let x = &fdl[path.from][(b + k_max - k) % k_max];
                    for ((a, xv), hv) in acc.iter_mut().zip(x).zip(h) {
                        *a += xv * hv;
                    }
                    any = true;
                }
            }
            if !any {
                continue;
            }
            // A real signal's spectrum: DC and Nyquist are real.
            acc[0].im = 0.0;
            acc[bins - 1].im = 0.0;
            let _ = ifft.process_with_scratch(&mut acc, &mut time, &mut iscratch);
            let start = b * p;
            let end = (start + p).min(out_len);
            dst[start..end].copy_from_slice(&time[p..p + (end - start)]);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Render

/// The wet's settings at a buffer sample, re-evaluated a block at a time
/// when automated.
struct WetAt<'a> {
    eval: Option<Evaluator<'a, ReverbSettings>>,
    base: ReverbSettings,
    block: usize,
    now: ReverbSettings,
}

impl<'a> WetAt<'a> {
    fn new(s: &ReverbSettings, tracks: &'a Tracks<ReverbParam>) -> Self {
        Self {
            eval: (!tracks.is_empty()).then(|| Evaluator::new(*s, tracks)),
            base: *s,
            block: usize::MAX,
            now: *s,
        }
    }

    /// The settings at buffer sample `t`, and whether they moved since the
    /// last call.
    #[inline]
    fn at(&mut self, t: usize) -> (&ReverbSettings, bool) {
        let block = t / AUTOMATION_BLOCK;
        if block == self.block {
            return (&self.now, false);
        }
        let first = self.block == usize::MAX;
        self.block = block;
        if let Some(eval) = self.eval.as_mut() {
            self.now = eval.at(block * AUTOMATION_BLOCK);
            return (&self.now, true);
        }
        self.now = self.base;
        (&self.now, first)
    }
}

/// A low or high cut on one channel of the wet, off at 0 Hz.
struct Cut {
    kind: SectionKind,
    hz: f32,
    filter: Option<SectionFilter>,
}

impl Cut {
    fn new(kind: SectionKind) -> Self {
        Self {
            kind,
            hz: 0.0,
            filter: None,
        }
    }

    fn set(&mut self, hz: f32, sr: u32) {
        if hz == self.hz {
            return;
        }
        self.hz = hz;
        if hz <= 0.0 {
            self.filter = None;
            return;
        }
        let f = f64::from(hz).min(f64::from(sr) * 0.45);
        let section = Section::design(self.kind, f, 0.0, CUT_Q, f64::from(sr));
        match self.filter.as_mut() {
            Some(filter) => filter.set_section(section),
            None => self.filter = Some(SectionFilter::new(section)),
        }
    }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        match self.filter.as_mut() {
            Some(filter) => filter.tick(f64::from(x)) as f32,
            None => x,
        }
    }
}

/// A raised-cosine ramp from 0 to 1 over `len` steps; 1 past it.
#[inline]
fn ramp(i: usize, len: usize) -> f32 {
    if i >= len {
        1.0
    } else {
        let t = (i + 1) as f64 / (len + 1) as f64;
        (0.5 - 0.5 * (std::f64::consts::PI * t).cos()) as f32
    }
}

/// Run the reverb over `channels` (whole buffers, the channels in scope),
/// sending `[s, e)` into it. Returns the channels as they come out, all the
/// same length: the buffer's, or longer when an `Extend` tail runs past its
/// end. `ir` must be at `buffer_sr`. `None` when cancelled.
pub fn render(
    channels: &[&[f32]],
    (s, e): (usize, usize),
    buffer_sr: u32,
    settings: &ReverbSettings,
    tracks: &Tracks<ReverbParam>,
    ir: &ImpulseResponse,
    cancel: &AtomicBool,
) -> Option<Vec<Vec<f32>>> {
    let len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    let n = channels.len();
    let (s, e) = (s.min(len), e.min(len).max(s.min(len)));
    if n == 0 || e <= s {
        return Some(channels.iter().map(|c| c[..len].to_vec()).collect());
    }
    let sr = buffer_sr.max(1);
    let frames = |secs: f64| crate::sample_rate::frames_for_secs(secs, sr);

    // What is sent: the target, faded in and out where it meets audio that
    // is not sent, so its hard edges do not ring through the IR.
    let edge = crate::wave::splice_xfade_samples(sr, e - s, e - s);
    let fade_in = if s > 0 { edge } else { 0 };
    let fade_out = if e < len { edge } else { 0 };
    let span = e - s;
    let send: Vec<Vec<f32>> = channels
        .iter()
        .map(|c| {
            c[s..e]
                .iter()
                .enumerate()
                .map(|(i, v)| v * ramp(i, fade_in) * ramp(span - 1 - i, fade_out))
                .collect()
        })
        .collect();

    // The IR as set, routed onto the target's channels, levelled.
    let (shaped, lead) = shape(ir, settings);
    let routing = route(n, shaped);
    let gain = if settings.normalize {
        normalize_gain(&routing)
    } else {
        1.0
    };
    let (irs, paths): (Vec<Vec<f32>>, Vec<Path>) = match routing {
        Routing::Mono(h) => (
            vec![h],
            (0..n)
                .map(|c| Path {
                    from: c,
                    ir: 0,
                    to: c,
                })
                .collect(),
        ),
        Routing::Stereo([l, r]) => (
            vec![l, r],
            vec![
                Path {
                    from: 0,
                    ir: 0,
                    to: 0,
                },
                Path {
                    from: 1,
                    ir: 1,
                    to: 1,
                },
            ],
        ),
        Routing::TrueStereo([ll, lr, rl, rr]) => (
            vec![ll, lr, rl, rr],
            vec![
                Path {
                    from: 0,
                    ir: 0,
                    to: 0,
                },
                Path {
                    from: 0,
                    ir: 1,
                    to: 1,
                },
                Path {
                    from: 1,
                    ir: 2,
                    to: 0,
                },
                Path {
                    from: 1,
                    ir: 3,
                    to: 1,
                },
            ],
        ),
    };
    let irs: Vec<Vec<f32>> = irs
        .into_iter()
        .map(|h| h.into_iter().map(|v| v * gain).collect())
        .collect();
    let ir_len = irs.iter().map(Vec::len).max().unwrap_or(0);
    let predelay = frames(f64::from(settings.predelay_ms.max(0.0)) / 1000.0);
    let conv_len = (span + ir_len).saturating_sub(1).max(span);
    // Wet frame `j` sounds at `s + j - lead + predelay`: the IR's frames
    // before its time zero sound before the target does, and the direct
    // sound lands where the target is.
    let wet_at = |j: usize| (s + j + predelay).checked_sub(lead);
    let send_refs: Vec<&[f32]> = send.iter().map(Vec::as_slice).collect();
    let ir_refs: Vec<&[f32]> = irs.iter().map(Vec::as_slice).collect();
    let wet = convolve(&send_refs, &ir_refs, &paths, n, conv_len, cancel)?;
    drop(send);

    // Where the wet ends up: from the target's start, delayed, as far as
    // the tail mode lets it ring.
    let wet_end = wet_at(conv_len).unwrap_or(s);
    let (end, new_len) = match settings.tail {
        ReverbTail::Cut => (e, len),
        ReverbTail::Overlap => (wet_end.min(len), len),
        ReverbTail::Extend => {
            // Up to where the tail has died away, no further.
            let peak = wet.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()));
            let floor = peak * db_to_amp(EXTEND_FLOOR_DB);
            let last = wet
                .iter()
                .filter_map(|w| w.iter().rposition(|v| v.abs() > floor))
                .max()
                .map_or(0, |i| i + 1);
            let end = wet_at(last).unwrap_or(s).min(wet_end).max(e);
            (end, end.max(len))
        }
    };
    // The wet's own fade-out where the mode stops it short of its end.
    let stop_fade = match settings.tail {
        ReverbTail::Cut => frames(CUT_FADE_SECS).min(span / 2),
        ReverbTail::Overlap if wet_end > len => frames(OVERLAP_FADE_SECS).min(span / 2),
        _ => 0,
    };

    let mut out: Vec<Vec<f32>> = channels
        .iter()
        .map(|c| {
            let mut v = c[..len].to_vec();
            v.resize(new_len, 0.0);
            v
        })
        .collect();
    let mut at = WetAt::new(settings, tracks);
    let mut lows: Vec<Cut> = (0..n).map(|_| Cut::new(SectionKind::HighPass)).collect();
    let mut highs: Vec<Cut> = (0..n).map(|_| Cut::new(SectionKind::LowPass)).collect();
    let (mut mix, mut gain_out, mut width) = (0.0f32, 1.0f32, 1.0f32);
    let mut frame = vec![0.0f32; n];
    for t in s..end {
        if (t - s) % CANCEL_STRIDE == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let (now, moved) = at.at(t);
        if moved {
            mix = now.mix_pct.clamp(0.0, 100.0) / 100.0;
            gain_out = db_to_amp(now.output_db);
            width = now.width_pct.max(0.0) / 100.0;
            let (lo, hi) = (now.low_cut_hz, now.high_cut_hz);
            for c in &mut lows {
                c.set(lo, sr);
            }
            for c in &mut highs {
                c.set(hi, sr);
            }
        }
        let i = t - s;
        // The wet frame that sounds at `t` (see `wet_at`).
        let j = (i + lead).checked_sub(predelay);
        for (c, w) in frame.iter_mut().enumerate() {
            let raw = j.and_then(|j| wet[c].get(j)).copied().unwrap_or(0.0);
            *w = highs[c].tick(lows[c].tick(raw));
        }
        if n == 2 && width != 1.0 {
            let mid = (frame[0] + frame[1]) * 0.5;
            let side = (frame[0] - frame[1]) * 0.5 * width;
            frame[0] = mid + side;
            frame[1] = mid - side;
        }
        let stop = if stop_fade > 0 {
            ramp(end - 1 - t, stop_fade)
        } else {
            1.0
        };
        let wet_gain = mix * gain_out * stop;
        if t < e {
            // Inside the target the dry is mixed down too, eased in and out
            // where the target meets untouched audio.
            let d = (1.0 - mix) * gain_out;
            let into = ramp(i, fade_in) * ramp(span - 1 - i, fade_out);
            let dry = 1.0 + (d - 1.0) * into;
            for (dst, w) in out.iter_mut().zip(&frame) {
                dst[t] = dst[t] * dry + w * wet_gain;
            }
        } else {
            for (dst, w) in out.iter_mut().zip(&frame) {
                dst[t] += w * wet_gain;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SR: u32 = 48_000;

    fn go() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn ir(channels: Vec<Vec<f32>>) -> ImpulseResponse {
        let len = channels[0].len();
        ImpulseResponse::from_channels(
            IrId::default_ir(),
            "test".into(),
            PathBuf::new(),
            channels,
            0,
            SR,
            SR,
            len,
        )
    }

    /// Deterministic noise in [-1, 1].
    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut x = seed.wrapping_mul(2_654_435_761).max(1);
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    fn direct(x: &[f32], h: &[f32]) -> Vec<f32> {
        let mut y = vec![0.0f32; x.len() + h.len() - 1];
        for (i, xv) in x.iter().enumerate() {
            for (j, hv) in h.iter().enumerate() {
                y[i + j] += xv * hv;
            }
        }
        y
    }

    #[test]
    fn partitioned_convolution_is_direct_convolution() {
        // IRs shorter than, near and longer than a partition (1024 at least).
        for (x_len, h_len) in [(5_000, 300), (3_000, 2_048), (2_500, 9_000), (700, 40)] {
            let x = noise(x_len, 1);
            let h: Vec<f32> = noise(h_len, 2)
                .iter()
                .enumerate()
                .map(|(i, v)| v * (-(i as f32) / 900.0).exp())
                .collect();
            let want = direct(&x, &h);
            let got = convolve(
                &[&x],
                &[&h],
                &[Path {
                    from: 0,
                    ir: 0,
                    to: 0,
                }],
                1,
                want.len(),
                &go(),
            )
            .unwrap();
            let worst = want
                .iter()
                .zip(&got[0])
                .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(worst < 1e-3, "{x_len} x {h_len}: {worst}");
        }
    }

    #[test]
    fn a_unit_impulse_all_wet_gives_the_input_back_in_place() {
        let x = noise(10_000, 3);
        let mut h = vec![0.0f32; 64];
        h[0] = 1.0;
        let s = ReverbSettings {
            mix_pct: 100.0,
            ..ReverbSettings::default()
        };
        let out = render(
            &[&x],
            (0, x.len()),
            SR,
            &s,
            &Tracks::none(),
            &ir(vec![h]),
            &go(),
        )
        .unwrap();
        assert_eq!(out[0].len(), x.len(), "nothing past the end but silence");
        let worst = x
            .iter()
            .zip(&out[0])
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-4, "{worst}");
    }

    #[test]
    fn nothing_wet_and_no_gain_leaves_the_audio_bit_exact() {
        let x = noise(4_000, 4);
        let s = ReverbSettings {
            mix_pct: 0.0,
            tail: ReverbTail::Overlap,
            ..ReverbSettings::default()
        };
        let out = render(
            &[&x],
            (1_000, 3_000),
            SR,
            &s,
            &Tracks::none(),
            &ir(vec![noise(500, 5)]),
            &go(),
        )
        .unwrap();
        assert_eq!(out[0], x);
    }

    #[test]
    fn a_true_stereo_ir_sends_left_into_right_by_its_second_path() {
        let left = noise(3_000, 6);
        let right = vec![0.0f32; 3_000];
        let zero = vec![0.0f32; 32];
        let mut one = zero.clone();
        one[0] = 1.0;
        // Only L -> R.
        let h = ir(vec![zero.clone(), one, zero.clone(), zero]);
        let s = ReverbSettings {
            mix_pct: 100.0,
            normalize: false,
            ..ReverbSettings::default()
        };
        let out = render(
            &[&left, &right],
            (0, 3_000),
            SR,
            &s,
            &Tracks::none(),
            &h,
            &go(),
        )
        .unwrap();
        assert!(out[0].iter().all(|v| v.abs() < 1e-5), "nothing reaches L");
        let worst = left
            .iter()
            .zip(&out[1])
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-4, "L arrives in R: {worst}");
    }

    #[test]
    fn routing_folds_what_the_target_cannot_take() {
        let h = |v: f32| vec![v; 4];
        assert_eq!(route(1, vec![h(1.0), h(3.0)]), Routing::Mono(h(2.0)));
        assert_eq!(
            route(2, vec![h(1.0), h(3.0)]),
            Routing::Stereo([h(1.0), h(3.0)])
        );
        assert_eq!(
            route(1, vec![h(1.0), h(2.0), h(3.0), h(4.0)]),
            Routing::Mono(h(5.0))
        );
        assert!(matches!(route(2, vec![h(1.0); 4]), Routing::TrueStereo(_)));
        assert_eq!(route(6, vec![h(2.0)]), Routing::Mono(h(2.0)));
    }

    #[test]
    fn predelay_length_and_normalize_shape_the_ir() {
        let x = {
            let mut v = vec![0.0f32; 20_000];
            v[100] = 1.0;
            v
        };
        let mut h = vec![0.0f32; 8_000];
        h[0] = 0.5;
        h[6_000] = 0.5;
        let s = ReverbSettings {
            mix_pct: 100.0,
            predelay_ms: 10.0,
            normalize: false,
            ..ReverbSettings::default()
        };
        let delay = SR as usize / 100;
        let out = render(
            &[&x],
            (0, x.len()),
            SR,
            &s,
            &Tracks::none(),
            &ir(vec![h.clone()]),
            &go(),
        )
        .unwrap();
        assert!(
            (out[0][100 + delay] - 0.5).abs() < 1e-4,
            "delayed by the pre-delay"
        );
        assert!((out[0][6_100 + delay] - 0.5).abs() < 1e-4);
        // Half the IR: the second reflection is gone.
        let half = ReverbSettings {
            length_pct: 50.0,
            ..s
        };
        let out = render(
            &[&x],
            (0, x.len()),
            SR,
            &half,
            &Tracks::none(),
            &ir(vec![h.clone()]),
            &go(),
        )
        .unwrap();
        assert!(out[0][6_100 + delay].abs() < 1e-4);
        // Normalized: unit energy, so 0.5, 0.5 becomes 1/sqrt(2) each.
        let norm = ReverbSettings {
            normalize: true,
            ..s
        };
        let out = render(
            &[&x],
            (0, x.len()),
            SR,
            &norm,
            &Tracks::none(),
            &ir(vec![h]),
            &go(),
        )
        .unwrap();
        assert!((out[0][100 + delay] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
    }

    fn decaying(len: usize) -> Vec<f32> {
        noise(len, 9)
            .iter()
            .enumerate()
            .map(|(i, v)| v * (-(i as f32) / 2_400.0).exp())
            .collect()
    }

    #[test]
    fn the_tail_modes_end_where_they_say() {
        let len = 24_000;
        let x = noise(len, 7);
        let (s, e) = (4_000, 12_000);
        let h = ir(vec![decaying(9_600)]);
        let base = ReverbSettings::default();

        let cut = render(
            &[&x],
            (s, e),
            SR,
            &ReverbSettings {
                tail: ReverbTail::Cut,
                ..base
            },
            &Tracks::none(),
            &h,
            &go(),
        )
        .unwrap();
        assert_eq!(cut[0].len(), len);
        assert_eq!(&cut[0][..s], &x[..s], "before the target");
        assert_eq!(&cut[0][e..], &x[e..], "after the target");

        let overlap = render(
            &[&x],
            (s, e),
            SR,
            &ReverbSettings {
                tail: ReverbTail::Overlap,
                ..base
            },
            &Tracks::none(),
            &h,
            &go(),
        )
        .unwrap();
        assert_eq!(overlap[0].len(), len);
        assert_eq!(&overlap[0][..s], &x[..s]);
        assert_ne!(
            &overlap[0][e..e + 1_000],
            &x[e..e + 1_000],
            "the tail rings on after the target"
        );

        // At the end of the file: Extend grows it, Overlap does not.
        let (s, e) = (len - 4_000, len);
        let extend = render(&[&x], (s, e), SR, &base, &Tracks::none(), &h, &go()).unwrap();
        assert!(extend[0].len() > len + 2_000, "{}", extend[0].len());
        assert_eq!(&extend[0][..s], &x[..s]);
        assert!(
            extend[0][len..].iter().any(|v| v.abs() > 1e-3),
            "the tail past the old end"
        );
        let overlap = render(
            &[&x],
            (s, e),
            SR,
            &ReverbSettings {
                tail: ReverbTail::Overlap,
                ..base
            },
            &Tracks::none(),
            &h,
            &go(),
        )
        .unwrap();
        assert_eq!(overlap[0].len(), len);
    }

    #[test]
    fn a_constant_curve_is_the_setting_and_a_moving_one_moves() {
        let len = 24_000;
        let x = noise(len, 11);
        let h = ir(vec![decaying(4_800)]);
        let s = ReverbSettings {
            mix_pct: 40.0,
            low_cut_hz: 200.0,
            ..ReverbSettings::default()
        };
        let plain = render(&[&x], (0, len), SR, &s, &Tracks::none(), &h, &go()).unwrap();
        let mut tracks = Tracks::none();
        tracks.push(
            ReverbParam::Mix,
            crate::automation::Curve::new(vec![(0, 40.0)], Scale::Linear),
        );
        tracks.push(
            ReverbParam::LowCut,
            crate::automation::Curve::new(vec![(0, 200.0)], Scale::Log),
        );
        let same = render(&[&x], (0, len), SR, &s, &tracks, &h, &go()).unwrap();
        let worst = plain[0]
            .iter()
            .zip(&same[0])
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-5, "{worst}");
        // Mix 0 for the first half, 100 for the second: dry, then wet only.
        let mut tracks = Tracks::none();
        tracks.push(
            ReverbParam::Mix,
            crate::automation::Curve::new(
                vec![(len / 2 - 1, 0.0), (len / 2, 100.0)],
                Scale::Linear,
            ),
        );
        let moved = render(
            &[&x],
            (0, len),
            SR,
            &ReverbSettings {
                low_cut_hz: 0.0,
                ..s
            },
            &tracks,
            &h,
            &go(),
        )
        .unwrap();
        assert_eq!(
            &moved[0][..len / 2 - AUTOMATION_BLOCK],
            &x[..len / 2 - AUTOMATION_BLOCK]
        );
        let tail = &moved[0][len / 2 + AUTOMATION_BLOCK..];
        let dry = &x[len / 2 + AUTOMATION_BLOCK..];
        assert!(
            tail.iter().zip(dry).any(|(a, b)| (a - b).abs() > 0.05),
            "wet only"
        );
    }

    #[test]
    fn width_zero_makes_the_wet_mono() {
        let left = noise(6_000, 12);
        let right = noise(6_000, 13);
        let h = ir(vec![
            decaying(2_000),
            decaying(2_000).iter().rev().copied().collect(),
        ]);
        let s = ReverbSettings {
            mix_pct: 100.0,
            width_pct: 0.0,
            ..ReverbSettings::default()
        };
        let out = render(
            &[&left, &right],
            (0, 6_000),
            SR,
            &s,
            &Tracks::none(),
            &h,
            &go(),
        )
        .unwrap();
        let worst = out[0]
            .iter()
            .zip(&out[1])
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(worst < 1e-5, "{worst}");
    }

    #[test]
    fn the_frames_before_an_ir_s_time_zero_sound_before_the_target() {
        // An IR whose time zero is 4 frames in: its direct sound (frame 4)
        // lands on the input, the frame before it a frame early.
        let mut x = vec![0.0f32; 1_000];
        x[500] = 1.0;
        let mut h = vec![0.0f32; 64];
        h[3] = 0.25;
        h[4] = 1.0;
        let ir = ImpulseResponse::from_channels(
            IrId::default_ir(),
            "lead".into(),
            PathBuf::new(),
            vec![h],
            4,
            SR,
            SR,
            60,
        );
        let s = ReverbSettings {
            mix_pct: 100.0,
            normalize: false,
            tail: ReverbTail::Overlap,
            ..ReverbSettings::default()
        };
        let out = render(&[&x], (0, x.len()), SR, &s, &Tracks::none(), &ir, &go()).unwrap();
        assert!((out[0][500] - 1.0).abs() < 1e-4, "{}", out[0][500]);
        assert!((out[0][499] - 0.25).abs() < 1e-4, "{}", out[0][499]);
    }

    /// A real IR from the library, 4.5 s and stereo: the long partitions
    /// (16,384 frames) against convolution done the slow way, deep into the
    /// tail. Skipped without the submodule.
    #[test]
    fn a_library_ir_renders_what_direct_convolution_does() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/IR-Library");
        let id = IrId::of(&crate::impulse::IrRef::Library(
            "Rooms/Commercial/Hotels/Ballrooms/Royal Ballroom.wav".into(),
        ));
        let Ok(ir) = crate::impulse::load(id, &[root], SR) else {
            eprintln!("vendor/IR-Library is not checked out: skipped");
            return;
        };
        let frames = SR as usize * 4;
        let x: Vec<f32> = noise(frames, 21)
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if i < SR as usize / 3 {
                    v * 0.8
                } else {
                    v * 0.1
                }
            })
            .collect();
        let s = ReverbSettings {
            mix_pct: 100.0,
            normalize: false,
            tail: ReverbTail::Overlap,
            ..ReverbSettings::default()
        };
        let out = render(&[&x, &x], (0, frames), SR, &s, &Tracks::none(), &ir, &go()).unwrap();
        for at in [SR as usize / 2, SR as usize * 3 / 2, SR as usize * 5 / 2] {
            for n in at..at + 20 {
                let h = &ir.channels[0];
                let y: f64 = (0..=n.min(h.len() - 1))
                    .map(|j| f64::from(x[n - j]) * f64::from(h[j]))
                    .sum();
                assert!(
                    (f64::from(out[0][n]) - y).abs() < 1e-4,
                    "at {n}: {} vs {y}",
                    out[0][n]
                );
            }
        }
    }

    #[test]
    fn a_cancelled_render_stops() {
        let x = noise(48_000, 14);
        let stop = AtomicBool::new(true);
        let h = ir(vec![decaying(4_800)]);
        assert!(render(
            &[&x],
            (0, x.len()),
            SR,
            &ReverbSettings::default(),
            &Tracks::none(),
            &h,
            &stop
        )
        .is_none());
    }
}
