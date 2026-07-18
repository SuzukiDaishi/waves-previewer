//! Clipped-sample detection and repair (de-clip).
//!
//! Detection looks for *flat runs at the rails*: at least `min_run`
//! consecutive samples of one sign whose magnitude sits above a threshold
//! (a fraction of the scanned region's peak, so attenuated-after-clipping
//! files still register) and whose values barely move (clipping pins the
//! waveform flat). Runs longer than `max_clip_ms` are rejected as content —
//! a square wave holds its rail for whole half-periods, real clipping only
//! for the wave crest it chopped off.
//!
//! Repair reuses the de-click cubic Hermite bridge: anchors just outside
//! the span with one-sided slopes rebuild the chopped crest, arcing above
//! the rail when the surrounding slopes point up (editing buffers keep
//! float headroom, so exceeding ±1.0 is fine and preserved).

use crate::app::declick::repair_spans_hermite;

/// Spans closer than this merge into one repair.
const MERGE_GAP_SAMPLES: usize = 16;
/// Flatness tolerance as a fraction of the region peak. Very tight: rails
/// are pinned, while even a low-frequency sine crest wanders more than this
/// across a few samples.
const FLAT_TOL_FRAC: f32 = 5e-4;
/// The approach slope just outside a run must exceed this multiple of the
/// in-run variation: clipping enters its rail with a corner, a smooth crest
/// approaches as gently as it sits.
const CORNER_RATIO: f32 = 3.0;
/// Regions quieter than this peak are never scanned (silence can't clip).
const MIN_PEAK: f32 = 1e-3;

#[derive(Clone, Copy, Debug)]
pub struct DeclipConfig {
    /// 0..=1; higher catches lower rails (threshold lerps 0.995 -> 0.85 of
    /// the region peak).
    pub sensitivity: f32,
    /// Minimum consecutive rail samples for a clip (>= 3 avoids single-sample
    /// peaks that are ordinary program material).
    pub min_run: usize,
    /// Runs longer than this are content (square waves), not clipping.
    pub max_clip_ms: f32,
    /// Samples added on both sides of each detected span before repair.
    pub margin_samples: usize,
}

impl Default for DeclipConfig {
    fn default() -> Self {
        Self {
            sensitivity: 0.5,
            min_run: 3,
            max_clip_ms: 8.0,
            margin_samples: 2,
        }
    }
}

/// Detect clipped (flat-at-the-rail) spans in `ch`, optionally restricted to
/// `range`. Returns disjoint `[start, end)` spans sorted by position.
pub fn detect_clipped(
    ch: &[f32],
    sr: u32,
    cfg: &DeclipConfig,
    range: Option<(usize, usize)>,
) -> Vec<(usize, usize)> {
    let n = ch.len();
    if n < 8 {
        return Vec::new();
    }
    let (rs, re) = match range {
        Some((s, e)) => (s.min(n), e.min(n)),
        None => (0, n),
    };
    if re <= rs {
        return Vec::new();
    }
    let peak = ch[rs..re].iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak < MIN_PEAK {
        return Vec::new();
    }
    let sens = cfg.sensitivity.clamp(0.0, 1.0);
    let thr = peak * (0.995 + (0.85 - 0.995) * sens);
    let flat_tol = (peak * FLAT_TOL_FRAC).max(1e-6);
    let min_run = cfg.min_run.max(2);
    let max_len = ((cfg.max_clip_ms.max(0.1) / 1000.0) * sr.max(1) as f32).ceil() as usize;

    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = rs;
    while i < re {
        let v = ch[i];
        if v.abs() < thr {
            i += 1;
            continue;
        }
        // Extend a same-sign rail run while the value stays pinned.
        let sign = v.is_sign_positive();
        let (mut lo, mut hi) = (v, v);
        let mut j = i + 1;
        while j < re {
            let w = ch[j];
            if w.abs() < thr || w.is_sign_positive() != sign {
                break;
            }
            let nlo = lo.min(w);
            let nhi = hi.max(w);
            if nhi - nlo > flat_tol {
                break;
            }
            lo = nlo;
            hi = nhi;
            j += 1;
        }
        let run = j - i;
        if run >= min_run && run <= max_len.max(1) {
            // Corner test: the waveform must approach/leave the rail much
            // faster than it moves while on it, or it's just a smooth crest.
            let run_range = hi - lo;
            let d_in = if i >= 3 { (ch[i] - ch[i - 3]).abs() } else { f32::MAX };
            let d_out = if j + 3 <= n {
                (ch[j + 2] - ch[j - 1]).abs()
            } else {
                f32::MAX
            };
            if d_in.max(d_out) > CORNER_RATIO * (run_range + peak * 1e-4) {
                spans.push((i, j));
            }
        }
        i = j.max(i + 1);
    }

    // Merge close runs (both rails of one chopped crest), margin, clamp.
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in spans {
        match merged.last_mut() {
            Some((_, pe)) if s.saturating_sub(*pe) < MERGE_GAP_SAMPLES => *pe = e,
            _ => merged.push((s, e)),
        }
    }
    let m = cfg.margin_samples;
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (s, e) in merged {
        let s = s.saturating_sub(m).max(rs);
        let e = (e + m).min(re);
        match out.last_mut() {
            Some((_, pe)) if s <= *pe => *pe = (*pe).max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Detect and repair clipped spans in one channel. Returns the repaired
/// channel and the number of repaired spans; untouched samples stay
/// bit-identical.
pub fn declip_channel(
    ch: &[f32],
    sr: u32,
    cfg: &DeclipConfig,
    range: Option<(usize, usize)>,
) -> (Vec<f32>, usize) {
    let spans = detect_clipped(ch, sr, cfg, range);
    let mut out = ch.to_vec();
    repair_spans_hermite(&mut out, &spans);
    (out, spans.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(freq: f32, sr: u32, len: usize, amp: f32) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * core::f32::consts::PI * freq * i as f32 / sr as f32).sin() * amp)
            .collect()
    }

    fn hard_clip(sig: &[f32], rail: f32) -> Vec<f32> {
        sig.iter().map(|v| v.clamp(-rail, rail)).collect()
    }

    #[test]
    fn clean_sine_is_untouched() {
        let sig = sine(440.0, SR, 48_000, 0.8);
        let cfg = DeclipConfig::default();
        let (out, count) = declip_channel(&sig, SR, &cfg, None);
        assert_eq!(count, 0, "clean sine flagged {count} clip runs");
        assert_eq!(out, sig);
    }

    #[test]
    fn hard_clipped_sine_is_detected_and_repaired_toward_the_crest() {
        // 0.9-amplitude sine chopped at 0.7: every crest is a flat run.
        let clean = sine(220.0, SR, 24_000, 0.9);
        let clipped = hard_clip(&clean, 0.7);
        let cfg = DeclipConfig::default();
        let spans = detect_clipped(&clipped, SR, &cfg, None);
        // 220 Hz over 0.5 s = 110 periods -> ~220 chopped crests.
        assert!(
            spans.len() > 150,
            "expected most crests detected, got {}",
            spans.len()
        );
        let (out, count) = declip_channel(&clipped, SR, &cfg, None);
        assert_eq!(count, spans.len());
        // Repair must reduce the error vs the true (unclipped) crest.
        let err_before: f32 = clipped
            .iter()
            .zip(&clean)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        let err_after: f32 = out
            .iter()
            .zip(&clean)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(
            err_after < err_before * 0.6,
            "repair should close in on the crest: before={err_before} after={err_after}"
        );
        // The bridge must actually rise above the rail somewhere.
        let peak_after = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak_after > 0.72, "repair never left the rail: {peak_after}");
    }

    #[test]
    fn square_wave_is_not_flagged() {
        // 100 Hz square: 5 ms at each rail — content, not clipping (runs
        // exceed max_clip_ms with the default 8 ms only at lower
        // frequencies, so use 50 Hz for 10 ms rails).
        let sig: Vec<f32> = (0..48_000)
            .map(|i| {
                if ((i as f32 / SR as f32) * 50.0).fract() < 0.5 {
                    0.8
                } else {
                    -0.8
                }
            })
            .collect();
        let cfg = DeclipConfig::default();
        let spans = detect_clipped(&sig, SR, &cfg, None);
        assert!(
            spans.is_empty(),
            "square wave misdetected as clipping: {spans:?}"
        );
    }

    #[test]
    fn range_limits_detection_and_repair() {
        let clean = sine(220.0, SR, 48_000, 0.9);
        let clipped = hard_clip(&clean, 0.7);
        let cfg = DeclipConfig::default();
        let (out, count) = declip_channel(&clipped, SR, &cfg, Some((0, 12_000)));
        assert!(count > 0);
        assert_eq!(
            &out[12_000..],
            &clipped[12_000..],
            "outside the range must stay untouched"
        );
        assert_ne!(&out[..12_000], &clipped[..12_000]);
    }

    #[test]
    fn attenuated_clipped_material_still_registers() {
        // Clip at 0.7 then attenuate to 30%: rails now sit at 0.21 but are
        // still flat relative to the region peak.
        let clean = sine(220.0, SR, 24_000, 0.9);
        let quiet: Vec<f32> = hard_clip(&clean, 0.7).iter().map(|v| v * 0.3).collect();
        let cfg = DeclipConfig::default();
        let spans = detect_clipped(&quiet, SR, &cfg, None);
        assert!(
            spans.len() > 150,
            "peak-relative threshold should still find rails, got {}",
            spans.len()
        );
    }
}
