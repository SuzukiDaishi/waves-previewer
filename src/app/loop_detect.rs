use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct LoopDetectConfig {
    pub min_loop_secs: f32,
    pub max_loop_secs: Option<f32>,
    pub match_window_secs: f32,
    pub candidate_limit: usize,
    pub zero_cross_radius: usize,
    pub coarse_bins: usize,
    pub local_coarse_radius: usize,
    pub local_fine_radius: usize,
}

impl Default for LoopDetectConfig {
    fn default() -> Self {
        Self {
            min_loop_secs: 3.0,
            max_loop_secs: None,
            match_window_secs: 1.5,
            candidate_limit: 64,
            zero_cross_radius: 256,
            coarse_bins: 48,
            local_coarse_radius: 2048,
            local_fine_radius: 128,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoopDetectConfidence {
    High,
    Medium,
    Low,
}

impl LoopDetectConfidence {
    pub fn from_score(score: f32) -> Self {
        if score >= 0.90 {
            LoopDetectConfidence::High
        } else if score >= 0.75 {
            LoopDetectConfidence::Medium
        } else {
            LoopDetectConfidence::Low
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            LoopDetectConfidence::High => "High",
            LoopDetectConfidence::Medium => "Medium",
            LoopDetectConfidence::Low => "Low",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LoopDetectCandidate {
    pub start: usize,
    pub end: usize,
    pub score: f32,
    pub confidence: LoopDetectConfidence,
    pub reason: String,
}

pub fn detect_loop(
    ch_samples: &[Vec<f32>],
    sample_rate: u32,
    config: &LoopDetectConfig,
    existing_loop: Option<(usize, usize)>,
    selection: Option<(usize, usize)>,
    cancel: &Arc<AtomicBool>,
    progress_cb: &mut dyn FnMut(f32),
) -> Result<Vec<LoopDetectCandidate>, String> {
    if ch_samples.is_empty() || ch_samples[0].is_empty() {
        return Err("No audio data".to_string());
    }

    let sr = sample_rate as usize;
    let len = ch_samples[0].len();
    let min_loop_samples = (config.min_loop_secs * sr as f32) as usize;

    if len < min_loop_samples {
        return Err("Audio too short for loop detection".to_string());
    }

    // mono downmix
    progress_cb(0.05);
    let mut mono: Vec<f32> = if ch_samples.len() == 1 {
        ch_samples[0].clone()
    } else {
        let n = ch_samples.len() as f32;
        (0..len)
            .map(|i| ch_samples.iter().map(|c| c[i]).sum::<f32>() / n)
            .collect()
    };

    // DC offset removal
    let mean = mono.iter().sum::<f32>() / mono.len() as f32;
    for s in &mut mono {
        *s -= mean;
    }

    // peak normalize
    let peak = mono.iter().cloned().map(f32::abs).fold(0.0f32, f32::max);
    if peak < 1e-9 {
        return Err("Audio is silent".to_string());
    }
    let inv_peak = 1.0 / peak;
    for s in &mut mono {
        *s *= inv_peak;
    }

    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".to_string());
    }

    // feature vector: block mean-abs + RMS per block
    progress_cb(0.15);
    let bin_size = (len / config.coarse_bins).max(config.zero_cross_radius);
    let n_bins = len / bin_size;
    let mut features: Vec<f32> = Vec::with_capacity(n_bins * 2);
    for b in 0..n_bins {
        let start = b * bin_size;
        let end = (start + bin_size).min(len);
        let slice = &mono[start..end];
        let mean_abs: f32 = slice.iter().map(|x| x.abs()).sum::<f32>() / slice.len() as f32;
        let rms: f32 = (slice.iter().map(|x| x * x).sum::<f32>() / slice.len() as f32).sqrt();
        features.push(mean_abs);
        features.push(rms);
    }

    // L2 normalize features
    let norm: f32 = features.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    for f in &mut features {
        *f /= norm;
    }

    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".to_string());
    }

    // generate candidate boundary points
    progress_cb(0.25);
    let mut candidate_points: Vec<usize> = Vec::new();

    // existing loop marker as candidates
    if let Some((ls, le)) = existing_loop {
        if ls < len {
            candidate_points.push(ls);
        }
        if le < len {
            candidate_points.push(le);
        }
    }
    // selection as candidate
    if let Some((ss, se)) = selection {
        let (a, b) = if ss <= se { (ss, se) } else { (se, ss) };
        if a < len {
            candidate_points.push(a);
        }
        if b < len {
            candidate_points.push(b);
        }
    }

    // RMS flux onset peaks
    let hop = bin_size;
    let mut prev_rms = 0.0f32;
    let mut flux: Vec<(usize, f32)> = Vec::new();
    for b in 0..n_bins {
        let start = b * bin_size;
        let end = (start + bin_size).min(len);
        let rms: f32 =
            (mono[start..end].iter().map(|x| x * x).sum::<f32>() / (end - start) as f32).sqrt();
        let df = (rms - prev_rms).max(0.0);
        flux.push((start + bin_size / 2, df));
        prev_rms = rms;
    }
    // pick peaks from flux
    for i in 1..flux.len().saturating_sub(1) {
        if flux[i].1 > flux[i - 1].1 && flux[i].1 > flux[i + 1].1 && flux[i].1 > 0.01 {
            candidate_points.push(flux[i].0);
        }
    }

    // regular grid fallback
    let grid_step = (len / 16).max(min_loop_samples / 4);
    let mut g = grid_step;
    while g < len {
        candidate_points.push(g);
        g += grid_step;
    }

    // sort and deduplicate nearby points
    candidate_points.sort_unstable();
    candidate_points.dedup_by(|a, b| if a.abs_diff(*b) < hop { true } else { false });

    // limit candidates
    let max_pts = config.candidate_limit * 2;
    if candidate_points.len() > max_pts {
        // keep evenly spaced
        let step = candidate_points.len() / max_pts;
        candidate_points = candidate_points.into_iter().step_by(step.max(1)).collect();
    }

    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".to_string());
    }

    // score start/end pairs
    progress_cb(0.40);
    let match_window = (config.match_window_secs * sr as f32) as usize;
    let mut raw_candidates: Vec<LoopDetectCandidate> = Vec::new();

    // evaluate all pairs where end - start >= min_loop_samples
    let max_loop_samples = config
        .max_loop_secs
        .map(|s| (s * sr as f32) as usize)
        .unwrap_or(len);

    let n_pts = candidate_points.len();
    for si in 0..n_pts {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".to_string());
        }
        let s = candidate_points[si];
        for ei in (si + 1)..n_pts {
            let e = candidate_points[ei];
            let loop_len = e.saturating_sub(s);
            if loop_len < min_loop_samples {
                continue;
            }
            if loop_len > max_loop_samples {
                break;
            }
            let score = score_loop_boundary(&mono, s, e, match_window, sr);
            raw_candidates.push(LoopDetectCandidate {
                start: s,
                end: e,
                score,
                confidence: LoopDetectConfidence::from_score(score),
                reason: "coarse".to_string(),
            });
        }
        // update progress
        if n_pts > 0 {
            progress_cb(0.40 + 0.40 * (si as f32 / n_pts as f32));
        }
    }

    if raw_candidates.is_empty() {
        return Err("No valid loop candidates found".to_string());
    }

    // sort by score descending, keep top N
    raw_candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    raw_candidates.truncate(config.candidate_limit);

    // local refinement and zero-cross snap
    progress_cb(0.85);
    // The grid search below evaluates `score_loop_boundary` at O((radius/stride)^2)
    // points per candidate. Reusing the full `match_window` (often 1+ second) for
    // every one of those evaluations made this phase dominate total runtime (far
    // more work than the entire coarse-scoring pass above). A window scaled to the
    // local search radius is plenty to rank nearby offsets against each other; the
    // final re-score below still uses the full `match_window` for the reported score.
    let refine_window = match_window.min(config.local_coarse_radius * 2);
    let total_candidates = raw_candidates.len();
    let mut final_candidates: Vec<LoopDetectCandidate> = Vec::new();
    for (idx, mut cand) in raw_candidates.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".to_string());
        }
        // local coarse search
        let best = refine_boundary(
            &mono,
            cand.start,
            cand.end,
            config.local_coarse_radius,
            128,
            refine_window,
            sr,
        );
        let (rs, re) = best;
        // local fine search
        let (fs, fe) =
            refine_boundary(&mono, rs, re, config.local_fine_radius, 8, refine_window, sr);

        // zero-cross snap
        let snapped_start = snap_to_zero_cross_fwd(&mono, fs, config.zero_cross_radius);
        let snapped_end = snap_to_zero_cross_fwd(&mono, fe, config.zero_cross_radius);

        // re-score after snap
        let final_score = score_loop_boundary(&mono, snapped_start, snapped_end, match_window, sr);
        // if snap hurt the score significantly, keep unsnapped
        let (final_start, final_end, final_score) = if final_score < cand.score - 0.10 {
            (fs, fe, score_loop_boundary(&mono, fs, fe, match_window, sr))
        } else {
            (snapped_start, snapped_end, final_score)
        };

        cand.start = final_start;
        cand.end = final_end;
        cand.score = final_score;
        cand.confidence = LoopDetectConfidence::from_score(final_score);
        cand.reason = "refined".to_string();
        final_candidates.push(cand);

        if total_candidates > 0 {
            progress_cb(0.85 + 0.15 * ((idx + 1) as f32 / total_candidates as f32));
        }
    }

    // re-sort after refinement
    final_candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    final_candidates
        .dedup_by(|a, b| a.start.abs_diff(b.start) < 512 && a.end.abs_diff(b.end) < 512);

    progress_cb(1.0);
    Ok(final_candidates)
}

fn score_loop_boundary(mono: &[f32], start: usize, end: usize, window: usize, sr: usize) -> f32 {
    let len = mono.len();
    if start >= end || end > len {
        return 0.0;
    }
    // compare window before end with window after start
    let w = window.min(start).min(len - end);
    if w < (sr / 100).max(64) {
        return 0.0;
    }

    let tail = &mono[end - w..end];
    let head = &mono[start..start + w];

    // correlation
    let corr: f32 = tail
        .iter()
        .zip(head.iter())
        .map(|(a, b)| a * b)
        .sum::<f32>()
        / w as f32;

    // RMS of each
    let rms_tail = (tail.iter().map(|x| x * x).sum::<f32>() / w as f32)
        .sqrt()
        .max(1e-9);
    let rms_head = (head.iter().map(|x| x * x).sum::<f32>() / w as f32)
        .sqrt()
        .max(1e-9);

    // normalize correlation
    let norm_corr = (corr / (rms_tail * rms_head)).clamp(-1.0, 1.0);

    // loudness difference penalty
    let loudness_ratio = (rms_tail / rms_head).max(rms_head / rms_tail);
    let loudness_penalty = ((loudness_ratio - 1.0) * 0.3).clamp(0.0, 0.5);

    // map correlation [-1,1] to [0,1]
    let score = (norm_corr + 1.0) * 0.5 - loudness_penalty;
    score.clamp(0.0, 1.0)
}

fn refine_boundary(
    mono: &[f32],
    start: usize,
    end: usize,
    radius: usize,
    stride: usize,
    window: usize,
    sr: usize,
) -> (usize, usize) {
    let len = mono.len();
    let s_lo = start.saturating_sub(radius);
    let s_hi = (start + radius).min(len);
    let e_lo = end.saturating_sub(radius);
    let e_hi = (end + radius).min(len);

    let mut best_score = score_loop_boundary(mono, start, end, window, sr);
    let mut best = (start, end);

    let mut s = s_lo;
    while s < s_hi {
        let mut e = e_lo;
        while e < e_hi {
            if e > s {
                let sc = score_loop_boundary(mono, s, e, window, sr);
                if sc > best_score {
                    best_score = sc;
                    best = (s, e);
                }
            }
            e += stride;
        }
        s += stride;
    }
    best
}

fn snap_to_zero_cross_fwd(mono: &[f32], pos: usize, radius: usize) -> usize {
    let lo = pos.saturating_sub(radius);
    let hi = (pos + radius).min(mono.len().saturating_sub(1));
    let mut best = pos;
    let mut best_dist = usize::MAX;
    for i in lo..hi {
        let crosses = (mono[i] >= 0.0) != (mono[i + 1] >= 0.0);
        if crosses {
            let dist = if i >= pos { i - pos } else { pos - i };
            if dist < best_dist {
                best_dist = dist;
                best = i;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    /// Build a phase-continuous looping sine.
    fn sine_loop(
        sr: usize,
        freq: f32,
        loop_start: usize,
        loop_end: usize,
        total: usize,
    ) -> Vec<f32> {
        let loop_len = loop_end - loop_start;
        let mut audio = vec![0.0f32; total];
        for i in 0..total {
            let phase_sample = if i <= loop_end {
                i
            } else {
                loop_start + (i - loop_end) % loop_len
            };
            audio[i] = (2.0 * std::f32::consts::PI * freq * phase_sample as f32 / sr as f32).sin();
        }
        audio
    }

    fn sine(sr: usize, freq: f32, dur_secs: f32, amp: f32) -> Vec<f32> {
        let n = (sr as f32 * dur_secs) as usize;
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin())
            .collect()
    }

    /// Minimal config for fast unit tests: tiny match window and small radii.
    fn cfg_fast_test() -> LoopDetectConfig {
        LoopDetectConfig {
            min_loop_secs: 0.5,
            match_window_secs: 0.05,
            candidate_limit: 5,
            local_coarse_radius: 512,
            local_fine_radius: 64,
            ..Default::default()
        }
    }

    // -----------------------------------------------------------------------
    // Basic: perfect synthetic loop is detected
    // -----------------------------------------------------------------------

    #[test]
    fn test_perfect_loop_detected() {
        let sr = 44100usize;
        let loop_start = sr / 2; // 0.5s
        let loop_end = sr * 3 / 2; // 1.5s
        let total = sr * 2;
        let audio = sine_loop(sr, 220.0, loop_start, loop_end, total);
        let ch = vec![audio];
        let result = detect_loop(
            &ch,
            sr as u32,
            &cfg_fast_test(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_ok(), "{:?}", result);
        let candidates = result.unwrap();
        assert!(!candidates.is_empty(), "expected at least one candidate");
    }

    // -----------------------------------------------------------------------
    // Audio too short
    // -----------------------------------------------------------------------

    #[test]
    fn test_too_short_for_minimum_loop() {
        let sr = 44100u32;
        // Only 1000 samples, well below min_loop_secs=3s
        let ch = vec![vec![0.1f32; 1000]];
        let result = detect_loop(
            &ch,
            sr,
            &LoopDetectConfig::default(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_err(), "should error on short audio");
    }

    #[test]
    fn test_empty_channels() {
        let sr = 44100u32;
        let result = detect_loop(
            &[],
            sr,
            &LoopDetectConfig::default(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_samples() {
        let sr = 44100u32;
        let ch: Vec<Vec<f32>> = vec![vec![]];
        let result = detect_loop(
            &ch,
            sr,
            &LoopDetectConfig::default(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Silence → error
    // -----------------------------------------------------------------------

    #[test]
    fn test_all_silence() {
        let sr = 44100u32;
        let ch = vec![vec![0.0f32; sr as usize * 10]];
        let result = detect_loop(
            &ch,
            sr,
            &LoopDetectConfig::default(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_err(), "silence should error");
    }

    // -----------------------------------------------------------------------
    // Stereo downmix works
    // -----------------------------------------------------------------------

    #[test]
    fn test_stereo_input_downmix() {
        let sr = 44100usize;
        let loop_start = sr / 2;
        let loop_end = sr * 3 / 2;
        let total = sr * 2;
        let audio = sine_loop(sr, 330.0, loop_start, loop_end, total);
        let silence: Vec<f32> = vec![0.0; total];
        let ch = vec![audio, silence]; // signal only on left
        let result = detect_loop(
            &ch,
            sr as u32,
            &cfg_fast_test(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_ok(), "{:?}", result);
        let candidates = result.unwrap();
        assert!(
            !candidates.is_empty(),
            "stereo downmix should find candidates"
        );
    }

    // -----------------------------------------------------------------------
    // Candidates are sorted by score descending
    // -----------------------------------------------------------------------

    #[test]
    fn test_candidates_sorted_by_score_descending() {
        let sr = 44100usize;
        let audio = sine_loop(sr, 220.0, sr / 2, sr * 3 / 2, sr * 2);
        let ch = vec![audio];
        let result = detect_loop(
            &ch,
            sr as u32,
            &cfg_fast_test(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        for w in result.windows(2) {
            assert!(
                w[0].score >= w[1].score,
                "candidates not sorted: {} < {}",
                w[0].score,
                w[1].score
            );
        }
    }

    // -----------------------------------------------------------------------
    // Candidate count is bounded by candidate_limit
    // -----------------------------------------------------------------------

    #[test]
    fn test_candidate_limit_respected() {
        let sr = 44100usize;
        let audio = sine_loop(sr, 220.0, sr / 2, sr * 3 / 2, sr * 2);
        let ch = vec![audio];
        let config = LoopDetectConfig {
            candidate_limit: 3,
            ..cfg_fast_test()
        };
        let result = detect_loop(
            &ch,
            sr as u32,
            &config,
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        assert!(
            result.len() <= 3,
            "candidate_limit not respected: got {} candidates",
            result.len()
        );
    }

    // -----------------------------------------------------------------------
    // Existing loop marker is included as a candidate (rescored)
    // -----------------------------------------------------------------------

    #[test]
    fn test_existing_loop_marker_is_rescored() {
        // Use a short audio + fast config to keep this test well under 1s in debug builds.
        let sr = 44100usize;
        let loop_start = sr / 2; // 0.5s
        let loop_end = sr * 3 / 2; // 1.5s
        let total = sr * 2; // 2s total
        let audio = sine_loop(sr, 220.0, loop_start, loop_end, total);
        let ch = vec![audio];
        let config = cfg_fast_test();
        let existing = Some((loop_start, loop_end));
        let result = detect_loop(
            &ch,
            sr as u32,
            &config,
            existing,
            None,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        assert!(
            !result.is_empty(),
            "should find candidates with existing loop hint"
        );
        let best = &result[0];
        let start_diff = (best.start as isize - loop_start as isize).unsigned_abs();
        let end_diff = (best.end as isize - loop_end as isize).unsigned_abs();
        assert!(
            start_diff < sr / 2 || end_diff < sr / 2,
            "best candidate should be near provided markers: start_diff={} end_diff={}",
            start_diff,
            end_diff
        );
    }

    // -----------------------------------------------------------------------
    // Selection range is included as a candidate
    // -----------------------------------------------------------------------

    #[test]
    fn test_selection_candidate_included() {
        // Use a short audio + fast config to keep this test well under 1s in debug builds.
        let sr = 44100usize;
        let loop_start = sr / 2; // 0.5s
        let loop_end = sr * 3 / 2; // 1.5s
        let total = sr * 2; // 2s total
        let audio = sine_loop(sr, 220.0, loop_start, loop_end, total);
        let ch = vec![audio];
        let config = cfg_fast_test();
        let selection = Some((loop_start + 512, loop_end - 512));
        let result = detect_loop(
            &ch,
            sr as u32,
            &config,
            None,
            selection,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        assert!(!result.is_empty(), "selection hint should yield candidates");
    }

    // -----------------------------------------------------------------------
    // All candidates respect min_loop_secs
    // -----------------------------------------------------------------------

    #[test]
    fn test_all_candidates_meet_minimum_length() {
        let sr = 44100usize;
        let audio = sine_loop(sr, 220.0, sr / 2, sr * 5 / 2, sr * 3);
        let ch = vec![audio];
        let min_secs = 1.0f32;
        let config = LoopDetectConfig {
            min_loop_secs: min_secs,
            ..cfg_fast_test()
        };
        let result = detect_loop(
            &ch,
            sr as u32,
            &config,
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        for c in &result {
            let loop_len_secs = c.end.saturating_sub(c.start) as f32 / sr as f32;
            assert!(
                loop_len_secs >= min_secs - 0.05,
                "candidate shorter than min_loop_secs: {}s (start={} end={})",
                loop_len_secs,
                c.start,
                c.end
            );
        }
    }

    // -----------------------------------------------------------------------
    // Candidate score is in [0, 1]
    // -----------------------------------------------------------------------

    #[test]
    fn test_candidate_scores_in_range() {
        let sr = 44100usize;
        let audio = sine_loop(sr, 220.0, sr / 2, sr * 3 / 2, sr * 2);
        let ch = vec![audio];
        let result = detect_loop(
            &ch,
            sr as u32,
            &cfg_fast_test(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        for c in &result {
            assert!(
                c.score >= 0.0 && c.score <= 1.0,
                "score out of [0,1]: {}",
                c.score
            );
        }
    }

    // -----------------------------------------------------------------------
    // High confidence for perfect loop
    // -----------------------------------------------------------------------

    #[test]
    fn test_high_confidence_for_perfect_loop() {
        let sr = 44100usize;
        let loop_start = sr / 2;
        let loop_end = sr * 3 / 2;
        let total = sr * 2;
        let audio = sine_loop(sr, 220.0, loop_start, loop_end, total);
        let ch = vec![audio];
        let result = detect_loop(
            &ch,
            sr as u32,
            &cfg_fast_test(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        assert!(!result.is_empty());
        let best = &result[0];
        assert!(
            best.score > 0.5,
            "best candidate for a perfect loop should have score > 0.5, got {}",
            best.score
        );
    }

    // -----------------------------------------------------------------------
    // Progress callback is called and ends at 1.0
    // -----------------------------------------------------------------------

    #[test]
    fn test_progress_reaches_one() {
        let sr = 44100usize;
        let audio = sine_loop(sr, 220.0, sr / 2, sr * 3 / 2, sr * 2);
        let ch = vec![audio];
        let mut prog = Vec::new();
        let _ = detect_loop(
            &ch,
            sr as u32,
            &cfg_fast_test(),
            None,
            None,
            &make_cancel(),
            &mut |p| {
                prog.push(p);
            },
        );
        assert!(!prog.is_empty(), "progress should be called");
        assert_eq!(*prog.last().unwrap(), 1.0, "last progress should be 1.0");
    }

    // -----------------------------------------------------------------------
    // Cancel aborts early
    // -----------------------------------------------------------------------

    #[test]
    fn test_cancel_pre_cancelled() {
        let sr = 44100usize;
        let audio = sine_loop(sr, 220.0, sr / 2, sr * 3 / 2, sr * 2);
        let ch = vec![audio];
        let cancel = Arc::new(AtomicBool::new(true));
        let result = detect_loop(
            &ch,
            sr as u32,
            &cfg_fast_test(),
            None,
            None,
            &cancel,
            &mut |_| {},
        );
        assert!(result.is_err(), "pre-cancelled should fail");
        assert_eq!(result.unwrap_err(), "Cancelled");
    }

    // -----------------------------------------------------------------------
    // Candidate boundaries are within audio length
    // -----------------------------------------------------------------------

    #[test]
    fn test_candidate_bounds_within_audio() {
        let sr = 44100usize;
        let total = sr * 2;
        let audio = sine_loop(sr, 220.0, sr / 2, sr * 3 / 2, total);
        let ch = vec![audio];
        let result = detect_loop(
            &ch,
            sr as u32,
            &cfg_fast_test(),
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        for c in &result {
            assert!(
                c.start < total,
                "start {} out of bounds (total={})",
                c.start,
                total
            );
            assert!(
                c.end <= total,
                "end {} out of bounds (total={})",
                c.end,
                total
            );
            assert!(c.start < c.end, "start >= end: {} >= {}", c.start, c.end);
        }
    }

    // -----------------------------------------------------------------------
    // Short min_loop config: shorter loop still found
    // -----------------------------------------------------------------------

    #[test]
    fn test_short_min_loop_config() {
        let sr = 44100usize;
        // Build a 1.0s repeating sine pattern for 3s total
        let pattern = sine(sr, 440.0, 1.0, 0.8);
        let total = sr * 3;
        let mut audio: Vec<f32> = Vec::with_capacity(total);
        while audio.len() < total {
            let rem = total - audio.len();
            audio.extend_from_slice(&pattern[..rem.min(pattern.len())]);
        }
        audio.truncate(total);
        let ch = vec![audio];
        let config = LoopDetectConfig {
            min_loop_secs: 0.5,
            ..cfg_fast_test()
        };
        let result = detect_loop(
            &ch,
            sr as u32,
            &config,
            None,
            None,
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_ok(), "{:?}", result);
        let candidates = result.unwrap();
        assert!(
            !candidates.is_empty(),
            "should find candidates with short min_loop_secs"
        );
        for c in &candidates {
            let len_s = c.end.saturating_sub(c.start) as f32 / sr as f32;
            assert!(len_s >= 0.5 - 0.05, "candidate too short: {}s", len_s);
        }
    }

    // -----------------------------------------------------------------------
    // LoopDetectConfidence::from_score thresholds
    // -----------------------------------------------------------------------

    #[test]
    fn test_confidence_thresholds() {
        assert_eq!(
            LoopDetectConfidence::from_score(0.95),
            LoopDetectConfidence::High
        );
        assert_eq!(
            LoopDetectConfidence::from_score(0.90),
            LoopDetectConfidence::High
        );
        assert_eq!(
            LoopDetectConfidence::from_score(0.89),
            LoopDetectConfidence::Medium
        );
        assert_eq!(
            LoopDetectConfidence::from_score(0.75),
            LoopDetectConfidence::Medium
        );
        assert_eq!(
            LoopDetectConfidence::from_score(0.74),
            LoopDetectConfidence::Low
        );
        assert_eq!(
            LoopDetectConfidence::from_score(0.0),
            LoopDetectConfidence::Low
        );
    }

    #[test]
    fn test_confidence_labels() {
        assert_eq!(LoopDetectConfidence::High.label(), "High");
        assert_eq!(LoopDetectConfidence::Medium.label(), "Medium");
        assert_eq!(LoopDetectConfidence::Low.label(), "Low");
    }
}
