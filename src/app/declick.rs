//! Click/pop detection and repair (de-click).
//!
//! Detection runs on the second-difference residual
//! `r[i] = x[i] - 0.5*(x[i-1] + x[i+1])` — smooth program material has a
//! small residual while single-sample spikes and short pops stand out.
//! The threshold adapts per 2048-sample window (50% overlap) via the MAD
//! robust scale estimate (`1.4826 * median(|r|)`), scaled by a
//! sensitivity-controlled factor. Flagged samples become spans, close
//! spans merge, spans longer than `max_click_ms` are rejected as content
//! (a transient that long is a feature, not a defect), and the surviving
//! spans grow by a safety margin.
//!
//! Repair replaces each span with a cubic Hermite bridge anchored on
//! 3-sample averages just outside the span with one-sided slopes, which
//! is transparent for the sub-millisecond gaps clicks leave behind.
//! (AR/LPC extrapolation is a possible future upgrade for long spans.)

const DETECT_WINDOW: usize = 2048;
const DETECT_HOP: usize = DETECT_WINDOW / 2;
/// Spans closer than this merge into one repair.
const MERGE_GAP_SAMPLES: usize = 32;
/// Absolute residual floor so digital silence never flags.
const RESIDUAL_FLOOR: f32 = 1e-4;

#[derive(Clone, Copy, Debug)]
pub struct DeclickConfig {
    /// 0..=1; higher finds smaller clicks (threshold factor lerps 40 -> 8).
    pub sensitivity: f32,
    /// Runs longer than this are content, not clicks.
    pub max_click_ms: f32,
    /// Samples added on both sides of each detected span before repair.
    pub margin_samples: usize,
}

impl Default for DeclickConfig {
    fn default() -> Self {
        Self {
            sensitivity: 0.5,
            max_click_ms: 2.0,
            margin_samples: 8,
        }
    }
}

/// Detect click spans in `ch` (optionally restricted to `range`).
/// Returns disjoint `[start, end)` spans sorted by position.
pub fn detect_clicks(
    ch: &[f32],
    sr: u32,
    cfg: &DeclickConfig,
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
    // Second-difference residual over the scanned region (plus one sample
    // of context on each side for the stencil).
    let mut resid = vec![0.0f32; re - rs];
    for i in rs.max(1)..re.min(n - 1) {
        resid[i - rs] = ch[i] - 0.5 * (ch[i - 1] + ch[i + 1]);
    }

    let sens = cfg.sensitivity.clamp(0.0, 1.0);
    let k = 40.0 + (8.0 - 40.0) * sens;
    let mut flags = vec![false; resid.len()];
    let mut scratch: Vec<f32> = Vec::with_capacity(DETECT_WINDOW);
    let mut start = 0usize;
    while start < resid.len() {
        let end = (start + DETECT_WINDOW).min(resid.len());
        scratch.clear();
        scratch.extend(resid[start..end].iter().map(|v| v.abs()));
        let mid = scratch.len() / 2;
        let (_, med, _) =
            scratch.select_nth_unstable_by(mid, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let thr = (1.4826 * *med * k).max(RESIDUAL_FLOOR);
        for i in start..end {
            if resid[i].abs() > thr {
                flags[i] = true;
            }
        }
        if end == resid.len() {
            break;
        }
        start += DETECT_HOP;
    }

    // Flags -> runs.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut run_start: Option<usize> = None;
    for (i, &f) in flags.iter().enumerate() {
        match (f, run_start) {
            (true, None) => run_start = Some(i),
            (false, Some(s)) => {
                spans.push((rs + s, rs + i));
                run_start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = run_start {
        spans.push((rs + s, rs + flags.len()));
    }

    // Merge close runs, then reject anything too long to be a click.
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in spans {
        match merged.last_mut() {
            Some((_, pe)) if s.saturating_sub(*pe) < MERGE_GAP_SAMPLES => *pe = e,
            _ => merged.push((s, e)),
        }
    }
    let max_len = ((cfg.max_click_ms.max(0.1) / 1000.0) * sr.max(1) as f32).ceil() as usize;
    merged.retain(|(s, e)| e - s <= max_len.max(1));

    // Safety margin, clamped to the scan region, then re-merge overlaps.
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

/// Replace each span with a cubic Hermite bridge anchored just outside it.
pub fn repair_spans_hermite(ch: &mut [f32], spans: &[(usize, usize)]) {
    let n = ch.len();
    for &(s, e) in spans {
        let e = e.min(n);
        if s >= e {
            continue;
        }
        // Anchors: 3-sample averages outside the span; one-sided slopes.
        let left_lo = s.saturating_sub(3);
        let p0 = if left_lo < s {
            ch[left_lo..s].iter().sum::<f32>() / (s - left_lo) as f32
        } else {
            0.0
        };
        let right_hi = (e + 3).min(n);
        let p1 = if e < right_hi {
            ch[e..right_hi].iter().sum::<f32>() / (right_hi - e) as f32
        } else {
            0.0
        };
        let m0 = if s >= 4 { (ch[s - 1] - ch[s - 4]) / 3.0 } else { 0.0 };
        let m1 = if e + 4 <= n { (ch[e + 3] - ch[e]) / 3.0 } else { 0.0 };
        // Bridge from the sample before the span to the one after it.
        let span_len = (e - s + 1) as f32;
        for i in s..e {
            let t = (i - s + 1) as f32 / span_len;
            let t2 = t * t;
            let t3 = t2 * t;
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            ch[i] = h00 * p0 + h10 * m0 * span_len + h01 * p1 + h11 * m1 * span_len;
        }
    }
}

/// Detect and repair clicks in one channel. Returns the repaired channel
/// and the number of repaired spans; untouched samples are bit-identical.
pub fn declick_channel(
    ch: &[f32],
    sr: u32,
    cfg: &DeclickConfig,
    range: Option<(usize, usize)>,
) -> (Vec<f32>, usize) {
    let spans = detect_clicks(ch, sr, cfg, range);
    let mut out = ch.to_vec();
    repair_spans_hermite(&mut out, &spans);
    (out, spans.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sr: u32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * core::f32::consts::PI * freq * i as f32 / sr as f32).sin() * 0.5)
            .collect()
    }

    fn rms(sig: &[f32]) -> f32 {
        if sig.is_empty() {
            return 0.0;
        }
        (sig.iter().map(|v| v * v).sum::<f32>() / sig.len() as f32).sqrt()
    }

    /// Deterministic LCG noise in [-1, 1].
    fn lcg_noise(len: usize, seed: u64) -> Vec<f32> {
        let mut state = seed.max(1);
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((state >> 33) as f32 / (u32::MAX >> 1) as f32) * 2.0 - 1.0
            })
            .collect()
    }

    const SR: u32 = 48_000;

    #[test]
    fn clean_sine_is_untouched() {
        let sig = sine(440.0, SR, 48_000);
        let cfg = DeclickConfig::default();
        let (out, count) = declick_channel(&sig, SR, &cfg, None);
        assert_eq!(count, 0, "clean sine flagged {count} clicks");
        assert_eq!(out, sig);
    }

    #[test]
    fn injected_clicks_are_detected_and_repaired() {
        let clean = sine(440.0, SR, 48_000);
        let mut damaged = clean.clone();
        let clicks = [5_000usize, 11_000, 17_500, 23_000, 31_000, 40_000];
        for &pos in &clicks {
            damaged[pos] = 1.0;
            damaged[pos + 1] = -0.8;
        }
        let cfg = DeclickConfig::default();
        let spans = detect_clicks(&damaged, SR, &cfg, None);
        for &pos in &clicks {
            assert!(
                spans.iter().any(|&(s, e)| pos >= s && pos < e),
                "click at {pos} not detected (spans: {spans:?})"
            );
        }
        let (out, count) = declick_channel(&damaged, SR, &cfg, None);
        assert!(count >= clicks.len(), "only {count} spans repaired");
        let err: Vec<f32> = out.iter().zip(&clean).map(|(o, c)| o - c).collect();
        let max_err = err.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(max_err < 0.05, "max repair error {max_err}");
        assert!(rms(&err) < 0.005, "rms repair error {}", rms(&err));
    }

    #[test]
    fn low_noise_floor_keeps_false_positives_bounded() {
        let clean = sine(440.0, SR, 48_000);
        let noise = lcg_noise(clean.len(), 42);
        let mut damaged: Vec<f32> = clean
            .iter()
            .zip(&noise)
            .map(|(s, n)| s + n * 0.01) // -40 dB noise bed
            .collect();
        let clicks = [8_000usize, 20_000, 36_000];
        for &pos in &clicks {
            damaged[pos] = 1.0;
        }
        let cfg = DeclickConfig::default();
        let spans = detect_clicks(&damaged, SR, &cfg, None);
        for &pos in &clicks {
            assert!(
                spans.iter().any(|&(s, e)| pos >= s && pos < e),
                "click at {pos} lost in the noise"
            );
        }
        assert!(
            spans.len() <= clicks.len() * 2,
            "too many false positives: {} spans for {} clicks",
            spans.len(),
            clicks.len()
        );
    }

    #[test]
    fn sustained_burst_is_not_a_click() {
        // 10 ms of loud content is program material, not a click.
        let mut sig = sine(440.0, SR, 48_000);
        let s = 24_000usize;
        let e = s + 480; // 10 ms
        for (i, v) in sig[s..e].iter_mut().enumerate() {
            *v = (2.0 * core::f32::consts::PI * 3_000.0 * i as f32 / SR as f32).sin() * 0.9;
        }
        let cfg = DeclickConfig::default();
        let spans = detect_clicks(&sig, SR, &cfg, None);
        // The burst interior must survive; its hard edges may flag a couple
        // of tiny spans, but never the 480-sample run itself.
        let covered: usize = spans
            .iter()
            .map(|&(a, b)| b.min(e).saturating_sub(a.max(s)))
            .sum();
        assert!(
            covered < 120,
            "sustained burst treated as click: {covered} samples flagged"
        );
    }

    #[test]
    fn range_limits_the_repair() {
        let clean = sine(440.0, SR, 48_000);
        let mut damaged = clean.clone();
        damaged[10_000] = 1.0; // inside the range
        damaged[40_000] = 1.0; // outside the range
        let cfg = DeclickConfig::default();
        let (out, count) = declick_channel(&damaged, SR, &cfg, Some((5_000, 20_000)));
        assert_eq!(count, 1);
        assert!((out[10_000] - clean[10_000]).abs() < 0.05, "in-range click kept");
        assert_eq!(out[40_000], 1.0, "out-of-range click must stay untouched");
        assert_eq!(&out[20_000..], &damaged[20_000..]);
    }
}
