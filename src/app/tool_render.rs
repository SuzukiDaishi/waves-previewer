//! One function from samples to samples per editor tool, shared by the
//! preview and by Apply.
//!
//! Before this file each tool had a preview path and an apply path that had
//! drifted apart: Time Stretch previewed a mono mixdown and applied every
//! channel, the gain curve clamped on apply and not in the preview, Gate /
//! EQ / Compressor previewed at the device rate and applied at the file's.
//! Now a preview is a `render` of a [`ToolOp`], and Apply either commits that
//! very output or renders the same op again -- what was heard is what is
//! written.
//!
//! Pure: no UI, no engine, no `EditorTab`. Everything a render needs is in
//! the request, with times already converted to samples at the buffer's own
//! rate (`buffer_sr`), never the device's.

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::types::{
    FadeShape, LoopXfadeShape, NoiseProfile, SpectralBrushStamp, SpectralWarpPoint, ToolKind,
};
use super::WavesPreviewer;
use crate::automation::Tracks;
use crate::markers::MarkerEntry;

/// The least audio a stateful filter (EQ, compressor, limiter, gate) runs
/// over before a range, so its settling happens outside the part that is
/// kept; each grows it from its own settings (`eq_pre_roll`,
/// `dynamics_pre_roll`).
pub const RANGE_CONTEXT_SECS: f64 = 0.05;

/// Less gain change than this (dB) is no change at all.
const NEUTRAL_REDUCTION_DB: f32 = 1e-3;

/// What a tool does, with every parameter it needs, in samples.
#[derive(Clone, Debug)]
pub enum ToolOp {
    Gain {
        db: f32,
    },
    /// A breakpoint curve in (sample, dB) over the whole file.
    GainCurve {
        points: Vec<(usize, f32)>,
    },
    Normalize {
        target_db: f32,
    },
    Loudness {
        target_lufs: f32,
    },
    Reverse,
    InvertPolarity {
        /// Interior-boundary ramp length in samples; 0 is a hard flip.
        fade: usize,
    },
    DcOffset,
    EdgeFade {
        in_len: usize,
        in_shape: FadeShape,
        out_len: usize,
        out_shape: FadeShape,
    },
    InsertSilence {
        len: usize,
    },
    PitchShift {
        semitones: f32,
        /// Empty: the constant `semitones` everywhere.
        curve: Vec<(usize, f32)>,
    },
    TimeStretch {
        rate: f32,
    },
    Speed {
        rate: f32,
    },
    NoiseGate(crate::dynamics::GateSettings),
    /// The settings, and the curves some of them follow (automation).
    Eq(
        crate::eq::ParametricEqParams,
        Tracks<crate::eq::EqParam>,
    ),
    Compressor(
        crate::dynamics::CompressorSettings,
        Tracks<crate::dynamics::CompParam>,
    ),
    Limiter(
        crate::dynamics::LimiterSettings,
        Tracks<crate::dynamics::LimiterParam>,
    ),
    DeClick {
        sensitivity: f32,
    },
    DeClip {
        sensitivity: f32,
    },
    DeHum(crate::app::dehum::DehumConfig),
    DeNoise {
        profile: Arc<NoiseProfile>,
        reduction_db: f32,
        strength: f32,
    },
    SpectralWarp {
        points: Vec<SpectralWarpPoint>,
        time_radius_ms: f32,
        freq_radius_hz: f32,
    },
    SpectralBrush {
        stamps: Vec<SpectralBrushStamp>,
    },
    /// Mute the target, band-limited when a frequency band is given.
    SpectralMute {
        band: Option<(f32, f32)>,
        time_fade: usize,
        freq_fade_hz: f32,
    },
    /// Rebuild the target from the audio around it.
    Heal {
        band: Option<(f32, f32)>,
        time_fade_ms: f32,
        freq_fade_hz: f32,
    },
    /// Bake the loop seam: crossfade the loop's tail into its head.
    LoopCrossfade {
        start: usize,
        end: usize,
        half: usize,
        shape: LoopXfadeShape,
    },
    /// Write the loop out `repeats` times.
    LoopUnroll {
        start: usize,
        end: usize,
        repeats: u32,
        markers: Vec<MarkerEntry>,
    },
}

/// The span a render acts on, in buffer samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RenderTarget {
    Whole,
    Range(usize, usize),
    InsertAt(usize),
}

/// How a position before the render maps to one after it. Used to keep the
/// playhead on the same audio when a preview is swapped in, and to move
/// markers, regions and notes when an edit is committed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TimeMap {
    Identity,
    /// `[start, old_end)` became `[start, new_end)`; what follows shifted.
    Range {
        start: usize,
        old_end: usize,
        new_end: usize,
    },
    /// The whole file was scaled from `old_len` to `new_len`.
    Whole {
        old_len: usize,
        new_len: usize,
    },
    /// `[start, end)` is written out `repeats` times.
    Unwrap {
        start: usize,
        end: usize,
        repeats: u32,
    },
}

impl TimeMap {
    /// A position in the source to the position of the same audio in the
    /// output. Inside an unwrapped loop, the first pass.
    pub fn to_output(&self, src: f64) -> f64 {
        match *self {
            TimeMap::Identity => src,
            TimeMap::Range {
                start,
                old_end,
                new_end,
            } => {
                let (s, oe, ne) = (start as f64, old_end as f64, new_end as f64);
                if src < s {
                    src
                } else if src < oe {
                    s + (src - s) * (ne - s) / (oe - s)
                } else {
                    src + (ne - oe)
                }
            }
            TimeMap::Whole { old_len, new_len } => {
                if old_len == 0 {
                    src
                } else {
                    src * new_len as f64 / old_len as f64
                }
            }
            TimeMap::Unwrap {
                start: _,
                end,
                repeats,
            } => {
                if src < end as f64 {
                    src
                } else {
                    src + Self::unwrap_shift(self, repeats) as f64
                }
            }
        }
        .max(0.0)
    }

    /// A position in the output back to the source. Inside an unwrapped
    /// loop every pass maps onto the one loop it repeats.
    pub fn to_source(&self, out: f64) -> f64 {
        match *self {
            TimeMap::Identity => out,
            TimeMap::Range {
                start,
                old_end,
                new_end,
            } => {
                let (s, oe, ne) = (start as f64, old_end as f64, new_end as f64);
                if out < s {
                    out
                } else if out < ne {
                    s + (out - s) * (oe - s) / (ne - s)
                } else {
                    out - (ne - oe)
                }
            }
            TimeMap::Whole { old_len, new_len } => {
                if new_len == 0 {
                    out
                } else {
                    out * old_len as f64 / new_len as f64
                }
            }
            TimeMap::Unwrap {
                start,
                end,
                repeats,
            } => {
                let (s, e) = (start as f64, end as f64);
                let loop_len = (e - s).max(1.0);
                let unrolled_end = s + loop_len * repeats.max(1) as f64;
                if out < s {
                    out
                } else if out < unrolled_end {
                    s + (out - s) % loop_len
                } else {
                    out - Self::unwrap_shift(self, repeats) as f64
                }
            }
        }
        .max(0.0)
    }

    /// A sample position (a marker, a region edge) through the map.
    pub fn map_sample(&self, sample: usize) -> usize {
        self.to_output(sample as f64).round() as usize
    }

    fn unwrap_shift(&self, repeats: u32) -> usize {
        match *self {
            TimeMap::Unwrap { start, end, .. } => end
                .saturating_sub(start)
                .saturating_mul(repeats.saturating_sub(1) as usize),
            _ => 0,
        }
    }
}

pub struct ToolRenderRequest {
    pub op: ToolOp,
    pub channels: Arc<Vec<Vec<f32>>>,
    /// The rate `channels` are at.
    pub buffer_sr: u32,
    pub target: RenderTarget,
    /// Channels the edit applies to (`None`: all). Ignored by tools that
    /// change the length or move audio between channels.
    pub ch_mask: Option<Vec<bool>>,
}

#[derive(Clone, Debug)]
pub struct ToolRenderOutput {
    pub channels: Vec<Vec<f32>>,
    /// The span the edited audio occupies now, when the tool works on a range.
    pub selection_after: Option<(usize, usize)>,
    pub time_map: TimeMap,
    /// Markers to replace the tab's with (the loop unroll writes its own).
    pub markers_after: Option<Vec<MarkerEntry>>,
    /// The integrated loudness the result was scaled to (LoudNorm).
    pub lufs_override: Option<f32>,
    /// What the render found out, for the Inspector to draw.
    pub analysis: Option<Arc<RenderAnalysis>>,
}

/// What a render found out about the audio, computed alongside the samples
/// so the picture is of exactly what Apply writes.
#[derive(Clone, Debug)]
pub enum RenderAnalysis {
    /// The processed target's average spectrum: `ANALYZER_COLUMNS` columns,
    /// log-spaced over the audible range (dBFS).
    Spectrum(Vec<f32>),
    /// Level and gain change over the target, in buffer samples.
    Dynamics(crate::dynamics::DynamicsTrace),
}

#[derive(Clone, Debug, PartialEq)]
pub enum RenderError {
    Cancelled,
    /// The op would leave the audio as it is (a silent file to normalize, a
    /// range of nothing). Not a failure; there is just nothing to apply.
    NothingToDo,
    Invalid(String),
}

impl ToolOp {
    /// The tool this op belongs to, for labels and the preview key.
    pub fn tool(&self) -> ToolKind {
        match self {
            ToolOp::Gain { .. } | ToolOp::GainCurve { .. } => ToolKind::Gain,
            ToolOp::Normalize { .. } => ToolKind::Normalize,
            ToolOp::Loudness { .. } => ToolKind::Loudness,
            ToolOp::Reverse => ToolKind::Reverse,
            ToolOp::InvertPolarity { .. } => ToolKind::InvertPolarity,
            ToolOp::DcOffset => ToolKind::DcOffset,
            ToolOp::EdgeFade { .. } => ToolKind::Fade,
            ToolOp::InsertSilence { .. } => ToolKind::InsertSilence,
            ToolOp::PitchShift { .. } => ToolKind::PitchShift,
            ToolOp::TimeStretch { .. } => ToolKind::TimeStretch,
            ToolOp::Speed { .. } => ToolKind::Speed,
            ToolOp::NoiseGate(_) => ToolKind::NoiseGate,
            ToolOp::Eq(..) => ToolKind::Eq,
            ToolOp::Compressor(..) => ToolKind::Compressor,
            ToolOp::Limiter(..) => ToolKind::Limiter,
            ToolOp::DeClick { .. } => ToolKind::DeClick,
            ToolOp::DeClip { .. } => ToolKind::DeClip,
            ToolOp::DeHum(_) => ToolKind::DeHum,
            ToolOp::DeNoise { .. } => ToolKind::DeNoise,
            ToolOp::SpectralWarp { .. } => ToolKind::SpectralWarp,
            ToolOp::SpectralBrush { .. } => ToolKind::SpectralBrush,
            ToolOp::SpectralMute { .. } => ToolKind::SpectralMute,
            ToolOp::Heal { .. } => ToolKind::Heal,
            ToolOp::LoopCrossfade { .. } | ToolOp::LoopUnroll { .. } => ToolKind::LoopEdit,
        }
    }

    /// The undo-history label of an apply of this op.
    pub fn label(&self) -> &'static str {
        match self {
            ToolOp::GainCurve { .. } => "Gain Curve",
            ToolOp::SpectralMute { .. } => "Spectral Mute",
            ToolOp::Heal { .. } => "Heal Selection",
            ToolOp::LoopCrossfade { .. } => "Loop Crossfade",
            ToolOp::LoopUnroll { .. } => "Loop Unwrap",
            other => other.tool().label(),
        }
    }

    /// Settings that would change nothing. Apply is not offered for them
    /// (an apply of nothing is an undo step that undoes nothing).
    pub fn is_neutral(&self) -> bool {
        const EPS: f32 = 1e-4;
        match self {
            ToolOp::Gain { db } => db.abs() <= EPS,
            ToolOp::GainCurve { points } => points.iter().all(|(_, db)| db.abs() <= EPS),
            ToolOp::PitchShift { semitones, curve } => {
                if curve.is_empty() {
                    semitones.abs() <= EPS
                } else {
                    curve.iter().all(|(_, st)| st.abs() <= EPS)
                }
            }
            ToolOp::TimeStretch { rate } | ToolOp::Speed { rate } => (rate - 1.0).abs() <= EPS,
            ToolOp::EdgeFade {
                in_len, out_len, ..
            } => *in_len == 0 && *out_len == 0,
            ToolOp::InsertSilence { len } => *len == 0,
            // A curve makes even neutral settings act somewhere.
            ToolOp::Eq(p, tracks) => p.is_neutral() && tracks.is_empty(),
            ToolOp::Compressor(p, tracks) => p.is_neutral() && tracks.is_empty(),
            ToolOp::SpectralWarp { points, .. } => points.iter().all(|p| p.delta_hz.abs() <= 0.5),
            ToolOp::SpectralBrush { stamps } => stamps.iter().all(|s| s.cut_db <= 0.1),
            ToolOp::LoopCrossfade { half, .. } => *half == 0,
            ToolOp::LoopUnroll {
                repeats,
                start,
                end,
                ..
            } => *repeats < 2 || end <= start,
            ToolOp::DeNoise { reduction_db, .. } => *reduction_db <= EPS,
            _ => false,
        }
    }

    /// A stable hash of everything that affects the rendered audio, so a
    /// preview can be matched to the op Apply would run.
    pub fn fingerprint(&self) -> u64 {
        let mut h = rustc_hash::FxHasher::default();
        std::mem::discriminant(self).hash(&mut h);
        let f = |h: &mut rustc_hash::FxHasher, v: f32| v.to_bits().hash(h);
        match self {
            ToolOp::Gain { db } => f(&mut h, *db),
            ToolOp::GainCurve { points } | ToolOp::PitchShift { curve: points, .. } => {
                if let ToolOp::PitchShift { semitones, .. } = self {
                    f(&mut h, *semitones);
                }
                for (s, v) in points {
                    s.hash(&mut h);
                    f(&mut h, *v);
                }
            }
            ToolOp::Normalize { target_db } => f(&mut h, *target_db),
            ToolOp::Loudness { target_lufs } => f(&mut h, *target_lufs),
            ToolOp::Reverse | ToolOp::DcOffset => {}
            ToolOp::InvertPolarity { fade } => fade.hash(&mut h),
            ToolOp::EdgeFade {
                in_len,
                in_shape,
                out_len,
                out_shape,
            } => {
                in_len.hash(&mut h);
                out_len.hash(&mut h);
                (*in_shape as u8).hash(&mut h);
                (*out_shape as u8).hash(&mut h);
            }
            ToolOp::InsertSilence { len } => len.hash(&mut h),
            ToolOp::TimeStretch { rate } | ToolOp::Speed { rate } => f(&mut h, *rate),
            // Plain settings structs: their Debug form names every field and
            // prints each float exactly, so it hashes what renders.
            ToolOp::NoiseGate(p) => hash_debug(&mut h, p),
            ToolOp::Eq(p, tracks) => {
                hash_debug(&mut h, p);
                hash_debug(&mut h, tracks);
            }
            ToolOp::Compressor(p, tracks) => {
                hash_debug(&mut h, p);
                hash_debug(&mut h, tracks);
            }
            ToolOp::Limiter(p, tracks) => {
                hash_debug(&mut h, p);
                hash_debug(&mut h, tracks);
            }
            ToolOp::DeClick { sensitivity } | ToolOp::DeClip { sensitivity } => {
                f(&mut h, *sensitivity)
            }
            ToolOp::DeHum(c) => {
                f(&mut h, c.base_hz);
                c.harmonics.hash(&mut h);
                f(&mut h, c.q);
                f(&mut h, c.depth_db);
            }
            ToolOp::DeNoise {
                profile,
                reduction_db,
                strength,
            } => {
                profile.fft_size.hash(&mut h);
                profile.sample_rate.hash(&mut h);
                f(&mut h, profile.learned_from_ms.0);
                f(&mut h, profile.learned_from_ms.1);
                profile.mag_per_channel.len().hash(&mut h);
                f(&mut h, *reduction_db);
                f(&mut h, *strength);
            }
            ToolOp::SpectralWarp {
                points,
                time_radius_ms,
                freq_radius_hz,
            } => {
                f(&mut h, *time_radius_ms);
                f(&mut h, *freq_radius_hz);
                for p in points {
                    p.sample.hash(&mut h);
                    f(&mut h, p.freq_hz);
                    f(&mut h, p.delta_hz);
                }
            }
            ToolOp::SpectralBrush { stamps } => {
                for s in stamps {
                    s.sample.hash(&mut h);
                    for v in [s.freq_hz, s.cut_db, s.time_sigma_ms, s.freq_sigma_hz] {
                        f(&mut h, v);
                    }
                }
            }
            ToolOp::SpectralMute {
                band,
                time_fade,
                freq_fade_hz,
            } => {
                if let Some((lo, hi)) = band {
                    f(&mut h, *lo);
                    f(&mut h, *hi);
                }
                time_fade.hash(&mut h);
                f(&mut h, *freq_fade_hz);
            }
            ToolOp::Heal {
                band,
                time_fade_ms,
                freq_fade_hz,
            } => {
                if let Some((lo, hi)) = band {
                    f(&mut h, *lo);
                    f(&mut h, *hi);
                }
                f(&mut h, *time_fade_ms);
                f(&mut h, *freq_fade_hz);
            }
            ToolOp::LoopCrossfade {
                start,
                end,
                half,
                shape,
            } => {
                (start, end, half).hash(&mut h);
                (*shape as u8).hash(&mut h);
            }
            ToolOp::LoopUnroll {
                start,
                end,
                repeats,
                markers,
            } => {
                (start, end, repeats).hash(&mut h);
                for m in markers {
                    m.sample.hash(&mut h);
                    m.label.hash(&mut h);
                }
            }
        }
        h.finish()
    }
}

fn check(cancel: &AtomicBool) -> Result<(), RenderError> {
    if cancel.load(Ordering::Relaxed) {
        Err(RenderError::Cancelled)
    } else {
        Ok(())
    }
}

/// Feed `value`'s Debug text to `h` without allocating it.
fn hash_debug(h: &mut rustc_hash::FxHasher, value: &dyn std::fmt::Debug) {
    struct Feed<'a>(&'a mut rustc_hash::FxHasher);
    impl std::fmt::Write for Feed<'_> {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            s.as_bytes().hash(self.0);
            Ok(())
        }
    }
    use std::fmt::Write as _;
    let _ = write!(Feed(h), "{value:?}");
}

/// The audio a dynamics processor runs over before a range, so its
/// envelope has settled by the range's first sample: a few release times.
fn dynamics_pre_roll(release_ms: f32, lookahead_ms: f32, sr: u32) -> usize {
    let secs = (f64::from(release_ms.max(0.0)) * 5.0 + f64::from(lookahead_ms.max(0.0))) / 1000.0;
    crate::sample_rate::frames_for_secs(secs.clamp(RANGE_CONTEXT_SECS, DYNAMICS_MAX_PRE_ROLL_SECS), sr)
}

/// The longest pre-roll a dynamics range gets: past this, a release has
/// long finished whatever its setting.
const DYNAMICS_MAX_PRE_ROLL_SECS: f64 = 3.0;
/// The longest a low corner may ring before a range.
const EQ_MAX_PRE_ROLL_SECS: f64 = 1.0;
/// Periods of the lowest active corner an EQ runs over before a range.
const EQ_PRE_ROLL_PERIODS: f64 = 8.0;

fn eq_pre_roll(
    p: &crate::eq::ParametricEqParams,
    tracks: &Tracks<crate::eq::EqParam>,
    sr: u32,
) -> usize {
    let secs = p
        .lowest_active_hz_automated(tracks)
        .map_or(RANGE_CONTEXT_SECS, |hz| EQ_PRE_ROLL_PERIODS / f64::from(hz.max(1.0)));
    crate::sample_rate::frames_for_secs(secs.clamp(RANGE_CONTEXT_SECS, EQ_MAX_PRE_ROLL_SECS), sr)
}

/// Run `process` over the in-scope channels of `[s, e)` together, with
/// `pre` / `post` samples of the audio around it, and splice each channel
/// back with crossfades at interior edges. `process` is told the buffer
/// sample its slices start at (where automation curves are). Returns the
/// channels and what `process` reported, plus where the processed segment
/// began.
#[allow(clippy::too_many_arguments)]
fn joint_range<X>(
    src: &[Vec<f32>],
    mask: &Option<Vec<bool>>,
    s: usize,
    e: usize,
    sr: u32,
    pre: usize,
    post: usize,
    cancel: &AtomicBool,
    process: impl FnOnce(&[&[f32]], usize) -> Option<(Vec<Vec<f32>>, X)>,
) -> Result<(Vec<Vec<f32>>, X, usize), RenderError> {
    check(cancel)?;
    let len = src.first().map_or(0, Vec::len);
    let whole = s == 0 && e >= len;
    let (seg_s, seg_e) = if whole {
        (0, len)
    } else {
        (s.saturating_sub(pre), (e + post).min(len))
    };
    let scope: Vec<usize> = (0..src.len()).filter(|&ci| channel_in_scope(mask, ci)).collect();
    let slices: Vec<&[f32]> = scope.iter().map(|&ci| &src[ci][seg_s..seg_e]).collect();
    let (processed, extra) = process(&slices, seg_s).ok_or(RenderError::Cancelled)?;
    let mut processed = processed.into_iter();
    let mut out = Vec::with_capacity(src.len());
    for (ci, ch) in src.iter().enumerate() {
        if !scope.contains(&ci) {
            out.push(ch.clone());
            continue;
        }
        let done = processed.next().unwrap_or_else(|| ch[seg_s..seg_e].to_vec());
        if whole {
            out.push(done);
            continue;
        }
        let a = (s - seg_s).min(done.len());
        let b = (e - seg_s).min(done.len()).max(a);
        let kept = &done[a..b];
        let xf = crate::wave::splice_xfade_samples(sr, e - s, kept.len());
        out.push(crate::wave::splice_range_with_crossfade(ch, s, e, kept, xf));
    }
    Ok((out, extra, seg_s))
}

/// Place a dynamics trace of a segment that began at `seg_s` onto the
/// buffer, keeping only `[s, e)`.
fn place_trace(
    trace: crate::dynamics::DynamicsTrace,
    seg_s: usize,
    s: usize,
    e: usize,
) -> crate::dynamics::DynamicsTrace {
    let mut kept = trace.crop(s - seg_s, e - seg_s);
    kept.start += seg_s;
    kept
}

/// The average spectrum of the in-scope channels over `[s, e)`.
fn target_spectrum(channels: &[Vec<f32>], mask: &Option<Vec<bool>>, s: usize, e: usize, sr: u32) -> Vec<f32> {
    let scope: Vec<&[f32]> = channels
        .iter()
        .enumerate()
        .filter(|(ci, _)| channel_in_scope(mask, *ci))
        .map(|(_, c)| c.as_slice())
        .collect();
    crate::app::render::mini_meter::average_spectrum(&scope, s, e, sr)
}

fn channel_in_scope(mask: &Option<Vec<bool>>, ci: usize) -> bool {
    mask.as_ref()
        .map_or(true, |m| m.get(ci).copied().unwrap_or(true))
}

/// Multiply `[s, e)` by `g`, ramping in and out over `ramp` samples at any
/// edge that meets untouched audio so a partial-range gain does not click.
fn scale_range(ch: &mut [f32], s: usize, e: usize, g: f32, ramp: usize) {
    let e = e.min(ch.len());
    let s = s.min(e);
    let len = e - s;
    let ramp = ramp.min(len / 2);
    let ramp_in = ramp > 0 && s > 0;
    let ramp_out = ramp > 0 && e < ch.len();
    for (i, v) in ch[s..e].iter_mut().enumerate() {
        let w = if ramp_in && i < ramp {
            (i + 1) as f32 / (ramp + 1) as f32
        } else if ramp_out && len - 1 - i < ramp {
            (len - i) as f32 / (ramp + 1) as f32
        } else {
            1.0
        };
        *v *= 1.0 + (g - 1.0) * w;
    }
}

/// Render `req.op` over `req.target`. Checks `cancel` between channels.
pub fn render(
    req: &ToolRenderRequest,
    cancel: &AtomicBool,
) -> Result<ToolRenderOutput, RenderError> {
    let sr = req.buffer_sr.max(1);
    let src = req.channels.as_ref();
    let len = src.first().map(Vec::len).unwrap_or(0);
    if len == 0 && !matches!(req.op, ToolOp::InsertSilence { .. }) {
        return Err(RenderError::NothingToDo);
    }
    let (s, e) = match req.target {
        RenderTarget::Whole => (0, len),
        RenderTarget::Range(a, b) => {
            let (a, b) = (a.min(b), a.max(b).min(len));
            if b <= a {
                return Err(RenderError::NothingToDo);
            }
            (a, b)
        }
        RenderTarget::InsertAt(p) => (p.min(len), p.min(len)),
    };
    let range = (s > 0 || e < len).then_some((s, e));
    let mask = &req.ch_mask;
    let ramp = crate::wave::splice_xfade_samples(sr, e.saturating_sub(s), e.saturating_sub(s));
    let mut out = ToolRenderOutput {
        channels: Vec::new(),
        selection_after: None,
        time_map: TimeMap::Identity,
        markers_after: None,
        lufs_override: None,
        analysis: None,
    };
    // Per-channel in-place edits share one shape: copy, edit the channels in
    // scope, leave the rest as they were.
    let per_channel = |edit: &dyn Fn(usize, &mut Vec<f32>)| -> Result<Vec<Vec<f32>>, RenderError> {
        let mut chans = Vec::with_capacity(src.len());
        for (ci, ch) in src.iter().enumerate() {
            check(cancel)?;
            let mut ch = ch.clone();
            if channel_in_scope(mask, ci) {
                edit(ci, &mut ch);
            }
            chans.push(ch);
        }
        Ok(chans)
    };
    match &req.op {
        ToolOp::Gain { db } => {
            let g = crate::app::helpers::db_to_amp(*db);
            out.channels = per_channel(&|_, ch| scale_range(ch, s, e, g, ramp))?;
        }
        ToolOp::GainCurve { points } => {
            if points.is_empty() {
                return Err(RenderError::NothingToDo);
            }
            // Editing buffers keep float headroom; the curve does not clamp.
            out.channels = per_channel(&|_, ch| {
                crate::wave::apply_gain_envelope_in_place(ch, points, 0.0, false)
            })?;
        }
        ToolOp::Normalize { target_db } => {
            let mut peak = 0.0f32;
            for (ci, ch) in src.iter().enumerate() {
                if channel_in_scope(mask, ci) {
                    peak = ch[s..e].iter().fold(peak, |p, v| p.max(v.abs()));
                }
            }
            if peak <= 0.0 {
                return Err(RenderError::NothingToDo);
            }
            let g = crate::app::helpers::db_to_amp(*target_db) / peak.max(1e-12);
            out.channels = per_channel(&|_, ch| scale_range(ch, s, e, g, ramp))?;
        }
        ToolOp::Loudness { target_lufs } => {
            let lufs =
                crate::wave::lufs_integrated_from_multi(src, sr).unwrap_or(f32::NEG_INFINITY);
            if !lufs.is_finite() {
                return Err(RenderError::NothingToDo);
            }
            let g = crate::app::helpers::db_to_amp(*target_lufs - lufs);
            let mut chans = Vec::with_capacity(src.len());
            for ch in src {
                check(cancel)?;
                chans.push(ch.iter().map(|v| v * g).collect());
            }
            out.channels = chans;
            out.lufs_override = Some(*target_lufs);
        }
        ToolOp::Reverse => {
            let xf = if range.is_some() {
                crate::wave::splice_xfade_samples(sr, e - s, e - s).min(256)
            } else {
                0
            };
            out.channels =
                per_channel(&|_, ch| crate::wave::reverse_range_with_crossfade(ch, s, e, xf))?;
        }
        ToolOp::InvertPolarity { fade } => {
            out.channels = per_channel(&|_, ch| {
                WavesPreviewer::invert_polarity_channel_range(ch, s, e, *fade)
            })?;
        }
        ToolOp::DcOffset => {
            out.channels = per_channel(&|_, ch| WavesPreviewer::dc_remove_range(ch, s, e))?;
        }
        ToolOp::EdgeFade {
            in_len,
            in_shape,
            out_len,
            out_shape,
        } => {
            let (in_len, out_len) = ((*in_len).min(len), (*out_len).min(len));
            if in_len == 0 && out_len == 0 {
                return Err(RenderError::NothingToDo);
            }
            out.channels = per_channel(&|_, ch| {
                if in_len > 0 {
                    WavesPreviewer::apply_fade_in_to_slice(&mut ch[..in_len], *in_shape);
                }
                if out_len > 0 {
                    let start = ch.len().saturating_sub(out_len);
                    WavesPreviewer::apply_fade_out_to_slice(&mut ch[start..], *out_shape);
                }
            })?;
        }
        ToolOp::InsertSilence { len: n } => {
            if *n == 0 {
                return Err(RenderError::NothingToDo);
            }
            let mut chans = Vec::with_capacity(src.len());
            for ch in src {
                check(cancel)?;
                let mut c = Vec::with_capacity(ch.len() + n);
                c.extend_from_slice(&ch[..s]);
                c.resize(s + n, 0.0);
                c.extend_from_slice(&ch[s..]);
                chans.push(c);
            }
            out.channels = chans;
            out.time_map = TimeMap::Range {
                start: s,
                old_end: s,
                new_end: s + n,
            };
        }
        ToolOp::PitchShift { semitones, curve } => {
            check(cancel)?;
            let mut chans = crate::wave::process_pitchshift_curve_multi_spliced(
                src, sr, curve, *semitones, range,
            );
            // The length is kept, so channels outside the scope go back as
            // they were.
            for (ci, ch) in chans.iter_mut().enumerate() {
                if !channel_in_scope(mask, ci) {
                    *ch = src[ci].clone();
                }
            }
            out.channels = chans;
            out.selection_after = range;
        }
        ToolOp::TimeStretch { rate } | ToolOp::Speed { rate } => {
            let tool = req.op.tool();
            let mut chans = Vec::with_capacity(src.len());
            for ch in src {
                check(cancel)?;
                chans.push(WavesPreviewer::process_tool_segment_spliced(
                    ch, tool, *rate, sr, range,
                ));
            }
            let new_len = chans.first().map(Vec::len).unwrap_or(0);
            match range {
                Some((s, e)) => {
                    let new_end = new_len.saturating_sub(len.saturating_sub(e));
                    out.selection_after = (new_end > s).then_some((s, new_end));
                    out.time_map = TimeMap::Range {
                        start: s,
                        old_end: e,
                        new_end,
                    };
                }
                None => {
                    out.time_map = TimeMap::Whole {
                        old_len: len,
                        new_len,
                    };
                }
            }
            out.channels = chans;
        }
        ToolOp::Eq(p, tracks) => {
            let pre = eq_pre_roll(p, tracks, sr);
            let (chans, (), _) = joint_range(src, mask, s, e, sr, pre, 0, cancel, |slices, at| {
                let mut chans: Vec<Vec<f32>> = slices.iter().map(|c| c.to_vec()).collect();
                crate::eq::process_automated(&mut chans, sr, p, tracks, at);
                Some((chans, ()))
            })?;
            out.analysis = Some(Arc::new(RenderAnalysis::Spectrum(target_spectrum(
                &chans, mask, s, e, sr,
            ))));
            out.channels = chans;
        }
        ToolOp::Compressor(p, tracks) => {
            let pre = dynamics_pre_roll(p.release_ms, p.lookahead_ms, sr);
            let post = crate::sample_rate::frames_for_secs(f64::from(p.lookahead_ms) / 1000.0, sr) + 1;
            let (chans, trace, seg_s) = joint_range(src, mask, s, e, sr, pre, post, cancel, |slices, at| {
                crate::dynamics::compress_automated(slices, sr, p, tracks, at, cancel)
                    .map(|o| (o.channels, o.trace))
            })?;
            out.analysis = Some(Arc::new(RenderAnalysis::Dynamics(place_trace(trace, seg_s, s, e))));
            out.channels = chans;
        }
        ToolOp::Limiter(p, tracks) => {
            let pre = dynamics_pre_roll(p.release_ms, p.lookahead_ms, sr);
            let post = crate::sample_rate::frames_for_secs(f64::from(p.lookahead_ms) / 1000.0, sr) + 1;
            let (chans, trace, seg_s) = joint_range(src, mask, s, e, sr, pre, post, cancel, |slices, at| {
                crate::dynamics::limit_automated(slices, sr, p, tracks, at, cancel)
                    .map(|o| (o.channels, o.trace))
            })?;
            let trace = place_trace(trace, seg_s, s, e);
            // Nothing over the ceiling and no gain: the audio is as it was.
            let gain_moves = tracks.contains(crate::dynamics::LimiterParam::InputGain);
            if trace.max_reduction_db() < NEUTRAL_REDUCTION_DB
                && p.input_gain_db.abs() < NEUTRAL_REDUCTION_DB
                && !gain_moves
            {
                return Err(RenderError::NothingToDo);
            }
            out.analysis = Some(Arc::new(RenderAnalysis::Dynamics(trace)));
            out.channels = chans;
        }
        ToolOp::NoiseGate(p) => {
            let pre = dynamics_pre_roll(p.release_ms + p.hold_ms, p.lookahead_ms, sr);
            let post = crate::sample_rate::frames_for_secs(f64::from(p.lookahead_ms) / 1000.0, sr) + 1;
            let (chans, trace, seg_s) = joint_range(src, mask, s, e, sr, pre, post, cancel, |slices, _| {
                crate::dynamics::gate(slices, sr, p, cancel).map(|o| (o.channels, o.trace))
            })?;
            out.analysis = Some(Arc::new(RenderAnalysis::Dynamics(place_trace(trace, seg_s, s, e))));
            out.channels = chans;
        }
        ToolOp::DeClick { sensitivity } => {
            let cfg = crate::app::declick::DeclickConfig {
                sensitivity: sensitivity.clamp(0.0, 1.0),
                ..Default::default()
            };
            out.channels = per_channel(&|_, ch| {
                *ch = crate::app::declick::declick_channel(ch, sr, &cfg, range).0
            })?;
        }
        ToolOp::DeClip { sensitivity } => {
            let cfg = crate::app::declip::DeclipConfig {
                sensitivity: sensitivity.clamp(0.0, 1.0),
                ..Default::default()
            };
            out.channels = per_channel(&|_, ch| {
                *ch = crate::app::declip::declip_channel(ch, sr, &cfg, range).0
            })?;
        }
        ToolOp::DeHum(cfg) => {
            // The filter runs over the whole channel so its settling never
            // lands inside the spliced edges; ~10 ms splice.
            let fade = (sr / 100).max(16) as usize;
            out.channels = per_channel(&|_, ch| {
                let filtered = crate::app::dehum::dehum_channel(ch, sr, cfg);
                *ch = match range {
                    Some((s, e)) => {
                        crate::app::dehum::splice_processed_range(ch, &filtered, s, e, fade)
                    }
                    None => filtered,
                }
            })?;
        }
        ToolOp::DeNoise {
            profile,
            reduction_db,
            strength,
        } => {
            if profile.sample_rate != sr {
                return Err(RenderError::Invalid(
                    "the noise profile was learned at another sample rate".into(),
                ));
            }
            out.channels = per_channel(&|ci, ch| {
                let mags = profile
                    .mag_per_channel
                    .get(ci)
                    .or_else(|| profile.mag_per_channel.first());
                if let Some(mags) = mags {
                    *ch = crate::app::spectral_ops::denoise_channel(
                        ch,
                        mags,
                        *reduction_db,
                        *strength,
                        range,
                    );
                }
            })?;
        }
        ToolOp::SpectralWarp {
            points,
            time_radius_ms,
            freq_radius_hz,
        } => {
            out.channels = per_channel(&|_, ch| {
                *ch = crate::app::spectral_ops::warp_channel_with_points(
                    ch,
                    sr,
                    points,
                    time_radius_ms.max(1.0),
                    freq_radius_hz.max(1.0),
                )
            })?;
        }
        ToolOp::SpectralBrush { stamps } => {
            out.channels = per_channel(&|_, ch| {
                *ch = crate::app::spectral_ops::brush_channel_with_stamps(ch, sr, stamps)
            })?;
        }
        ToolOp::SpectralMute {
            band,
            time_fade,
            freq_fade_hz,
        } => {
            out.channels = per_channel(&|_, ch| {
                crate::app::spectral_ops::spectral_mute_channel_range(
                    ch,
                    sr,
                    s,
                    e,
                    *band,
                    *time_fade,
                    *freq_fade_hz,
                )
            })?;
        }
        ToolOp::Heal {
            band,
            time_fade_ms,
            freq_fade_hz,
        } => {
            if (e - s) as f32 / sr as f32 > crate::app::spectral_ops::HEAL_MAX_SELECTION_SECS {
                return Err(RenderError::Invalid(format!(
                    "Heal works on at most {:.0} s at a time",
                    crate::app::spectral_ops::HEAL_MAX_SELECTION_SECS
                )));
            }
            out.channels = per_channel(&|_, ch| {
                *ch = crate::app::spectral_ops::heal_channel_range(
                    ch,
                    sr,
                    s,
                    e,
                    *band,
                    *freq_fade_hz,
                    *time_fade_ms,
                )
            })?;
        }
        ToolOp::LoopCrossfade {
            start,
            end,
            half,
            shape,
        } => {
            if *half == 0 || end <= start || *end > len {
                return Err(RenderError::NothingToDo);
            }
            let mut chans = src.clone();
            check(cancel)?;
            WavesPreviewer::apply_loop_xfade_to_channels(&mut chans, *start, *end, *half, *shape);
            out.channels = chans;
        }
        ToolOp::LoopUnroll {
            start,
            end,
            repeats,
            markers,
        } => {
            let (start, end) = (*start, (*end).min(len));
            if *repeats < 2 || end <= start {
                return Err(RenderError::NothingToDo);
            }
            let reps = *repeats as usize;
            let mut chans = Vec::with_capacity(src.len());
            for ch in src {
                check(cancel)?;
                let mut c = Vec::with_capacity(ch.len() + (end - start) * (reps - 1));
                c.extend_from_slice(&ch[..start]);
                for _ in 0..reps {
                    c.extend_from_slice(&ch[start..end]);
                }
                c.extend_from_slice(&ch[end..]);
                chans.push(c);
            }
            out.channels = chans;
            out.time_map = TimeMap::Unwrap {
                start,
                end,
                repeats: *repeats,
            };
            out.markers_after = Some(WavesPreviewer::build_loop_unwrap_markers(
                markers, start, end, len, reps,
            ));
        }
    }
    check(cancel)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn tone(len: usize, freq: f32) -> Vec<f32> {
        (0..len)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / SR as f32).sin() * 0.5)
            .collect()
    }

    fn stereo(len: usize) -> Arc<Vec<Vec<f32>>> {
        Arc::new(vec![tone(len, 440.0), tone(len, 660.0)])
    }

    fn req(op: ToolOp, channels: Arc<Vec<Vec<f32>>>, target: RenderTarget) -> ToolRenderRequest {
        ToolRenderRequest {
            op,
            channels,
            buffer_sr: SR,
            target,
            ch_mask: None,
        }
    }

    fn run(r: &ToolRenderRequest) -> ToolRenderOutput {
        render(r, &AtomicBool::new(false)).expect("render")
    }

    #[test]
    fn a_range_edit_leaves_everything_outside_bit_exact() {
        let src = stereo(SR as usize);
        let (s, e) = (12_000, 30_000);
        let ops = [
            ToolOp::Gain { db: 6.0 },
            ToolOp::Normalize { target_db: -1.0 },
            ToolOp::Reverse,
            ToolOp::InvertPolarity { fade: 96 },
            ToolOp::DcOffset,
            ToolOp::Eq(
                {
                    let mut p = crate::eq::ParametricEqParams::default();
                    p.bands[1].gain_db = 6.0;
                    p
                },
                Tracks::none(),
            ),
            ToolOp::Compressor(
                crate::dynamics::CompressorSettings {
                    threshold_db: -30.0,
                    ..Default::default()
                },
                Tracks::none(),
            ),
            ToolOp::Limiter(
                crate::dynamics::LimiterSettings {
                    input_gain_db: 12.0,
                    ..Default::default()
                },
                Tracks::none(),
            ),
            ToolOp::NoiseGate(crate::dynamics::GateSettings::default()),
        ];
        for op in ops {
            let name = format!("{op:?}");
            let out = run(&req(op, src.clone(), RenderTarget::Range(s, e)));
            for (ci, ch) in out.channels.iter().enumerate() {
                assert_eq!(ch.len(), src[ci].len(), "{name}");
                assert_eq!(&ch[..s], &src[ci][..s], "{name}: before the range");
                assert_eq!(&ch[e..], &src[ci][e..], "{name}: after the range");
            }
        }
    }

    #[test]
    fn the_channel_mask_keeps_other_channels_untouched() {
        let src = stereo(8_000);
        let mut r = req(ToolOp::Gain { db: -6.0 }, src.clone(), RenderTarget::Whole);
        r.ch_mask = Some(vec![false, true]);
        let out = run(&r);
        assert_eq!(out.channels[0], src[0]);
        assert_ne!(out.channels[1], src[1]);
        let g = crate::app::helpers::db_to_amp(-6.0);
        assert!((out.channels[1][100] - src[1][100] * g).abs() < 1e-6);
    }

    #[test]
    fn rendering_twice_gives_the_same_samples() {
        // A preview committed as-is and an apply rendered fresh must agree.
        let src = stereo(20_000);
        for op in [
            ToolOp::TimeStretch { rate: 1.25 },
            ToolOp::PitchShift {
                semitones: 3.0,
                curve: Vec::new(),
            },
            ToolOp::DeClick { sensitivity: 0.7 },
        ] {
            let a = run(&req(
                op.clone(),
                src.clone(),
                RenderTarget::Range(4_000, 12_000),
            ));
            let b = run(&req(op, src.clone(), RenderTarget::Range(4_000, 12_000)));
            assert_eq!(a.channels, b.channels);
        }
    }

    #[test]
    fn a_range_stretch_reports_where_the_range_went() {
        let src = stereo(SR as usize);
        let (s, e) = (10_000, 20_000);
        let out = run(&req(
            ToolOp::TimeStretch { rate: 0.5 },
            src.clone(),
            RenderTarget::Range(s, e),
        ));
        let new_len = out.channels[0].len();
        let (sel_s, sel_e) = out.selection_after.expect("selection after");
        assert_eq!(sel_s, s);
        assert_eq!(
            new_len - sel_e,
            src[0].len() - e,
            "the tail keeps its length"
        );
        assert!(
            sel_e - sel_s > (e - s) * 3 / 2,
            "half speed roughly doubles the range"
        );
        assert_eq!(
            out.channels.len(),
            2,
            "every channel is stretched, not a mixdown"
        );
        match out.time_map {
            TimeMap::Range {
                start,
                old_end,
                new_end,
            } => assert_eq!((start, old_end, new_end), (s, e, sel_e)),
            other => panic!("unexpected map {other:?}"),
        }
    }

    #[test]
    fn insert_silence_maps_later_positions_right() {
        let src = stereo(1_000);
        let out = run(&req(
            ToolOp::InsertSilence { len: 300 },
            src.clone(),
            RenderTarget::InsertAt(400),
        ));
        assert_eq!(out.channels[0].len(), 1_300);
        assert!(out.channels[0][400..700].iter().all(|v| *v == 0.0));
        assert_eq!(&out.channels[0][700..], &src[0][400..]);
        assert_eq!(out.time_map.map_sample(100), 100);
        assert_eq!(out.time_map.map_sample(400), 700);
        assert_eq!(out.time_map.map_sample(999), 1_299);
    }

    #[test]
    fn time_maps_round_trip() {
        let maps = [
            TimeMap::Range {
                start: 100,
                old_end: 200,
                new_end: 400,
            },
            TimeMap::Whole {
                old_len: 1_000,
                new_len: 750,
            },
            TimeMap::Unwrap {
                start: 100,
                end: 300,
                repeats: 3,
            },
        ];
        for map in maps {
            for src in [0.0, 50.0, 150.0, 250.0, 600.0] {
                let back = map.to_source(map.to_output(src));
                assert!((back - src).abs() < 1e-6, "{map:?}: {src} -> {back}");
            }
        }
        // Every pass of an unwrapped loop maps back onto the one loop.
        let unwrap = maps[2];
        assert!((unwrap.to_source(350.0) - 150.0).abs() < 1e-6);
        assert!((unwrap.to_source(550.0) - 150.0).abs() < 1e-6);
        assert!((unwrap.to_output(400.0) - 800.0).abs() < 1e-6);
    }

    #[test]
    fn loop_unroll_writes_the_loop_out_and_its_markers() {
        let src = stereo(1_000);
        let out = run(&req(
            ToolOp::LoopUnroll {
                start: 200,
                end: 400,
                repeats: 3,
                markers: vec![MarkerEntry {
                    sample: 500,
                    label: "after".into(),
                }],
            },
            src.clone(),
            RenderTarget::Whole,
        ));
        assert_eq!(out.channels[0].len(), 1_400);
        assert_eq!(&out.channels[0][400..600], &src[0][200..400]);
        let markers = out.markers_after.expect("markers");
        assert!(markers
            .iter()
            .any(|m| m.label == "after" && m.sample == 900));
        assert!(markers
            .iter()
            .any(|m| m.label == "loop_end" && m.sample == 800));
    }

    #[test]
    fn neutral_settings_are_recognised() {
        assert!(ToolOp::Gain { db: 0.0 }.is_neutral());
        assert!(!ToolOp::Gain { db: 0.5 }.is_neutral());
        assert!(ToolOp::TimeStretch { rate: 1.0 }.is_neutral());
        assert!(ToolOp::Eq(crate::eq::ParametricEqParams::default(), Tracks::none()).is_neutral());
        let mut tracks = Tracks::none();
        tracks.push(
            crate::eq::EqParam::Gain(1),
            crate::automation::Curve::new(vec![(0, 0.0), (100, 6.0)], crate::automation::Scale::Linear),
        );
        assert!(
            !ToolOp::Eq(crate::eq::ParametricEqParams::default(), tracks).is_neutral(),
            "a curve makes a flat EQ act"
        );
        assert!(!ToolOp::Reverse.is_neutral());
        assert!(ToolOp::PitchShift {
            semitones: 5.0,
            curve: vec![(0, 0.0), (100, 0.0)]
        }
        .is_neutral());
    }

    #[test]
    fn a_cancelled_render_stops() {
        let r = req(ToolOp::Gain { db: 3.0 }, stereo(1_000), RenderTarget::Whole);
        assert_eq!(
            render(&r, &AtomicBool::new(true)).unwrap_err(),
            RenderError::Cancelled
        );
    }

    #[test]
    fn an_automated_range_render_reads_the_curve_at_buffer_positions() {
        // A threshold step at 60 000: a range render that starts later
        // must see the step where the buffer has it, not at its own start.
        let src = stereo(SR as usize * 2);
        let settings = crate::dynamics::CompressorSettings {
            threshold_db: 0.0,
            ratio: 10.0,
            knee_db: 0.0,
            ..Default::default()
        };
        let mut tracks = Tracks::none();
        tracks.push(
            crate::dynamics::CompParam::Threshold,
            crate::automation::Curve::new(
                vec![(60_000, 0.0), (60_001, -40.0)],
                crate::automation::Scale::Linear,
            ),
        );
        let op = ToolOp::Compressor(settings, tracks);
        let range = run(&req(op.clone(), src.clone(), RenderTarget::Range(40_000, 90_000)));
        let whole = run(&req(op, src.clone(), RenderTarget::Whole));
        // Before the step nothing is compressed; after it, the range and the
        // whole render agree once both have settled.
        for i in (45_000..59_000).step_by(997) {
            assert!((range.channels[0][i] - src[0][i]).abs() < 1e-4, "at {i}");
        }
        for i in (70_000..88_000).step_by(997) {
            assert!(
                (range.channels[0][i] - whole.channels[0][i]).abs() < 1e-3,
                "at {i}: {} vs {}",
                range.channels[0][i],
                whole.channels[0][i]
            );
        }
    }

    #[test]
    fn fingerprints_follow_the_parameters() {
        let a = ToolOp::Gain { db: 3.0 }.fingerprint();
        assert_eq!(a, ToolOp::Gain { db: 3.0 }.fingerprint());
        assert_ne!(a, ToolOp::Gain { db: 3.1 }.fingerprint());
        assert_ne!(a, ToolOp::Normalize { target_db: 3.0 }.fingerprint());
        let limiter = crate::dynamics::LimiterSettings::default();
        let plain = ToolOp::Limiter(limiter, Tracks::none()).fingerprint();
        let mut tracks = Tracks::none();
        tracks.push(
            crate::dynamics::LimiterParam::Ceiling,
            crate::automation::Curve::new(vec![(0, -3.0)], crate::automation::Scale::Linear),
        );
        assert_ne!(plain, ToolOp::Limiter(limiter, tracks).fingerprint());
    }
}
