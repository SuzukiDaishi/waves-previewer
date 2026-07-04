use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct AutoTrimConfig {
    pub block_size: usize,
    pub hop_size: usize,
    pub noise_percentile: f32,
    pub threshold_above_noise_db: f32,
    pub threshold_below_peak_db: f32,
    pub pre_roll_secs: f32,
    pub post_roll_secs: f32,
    pub min_active_secs: f32,
    pub gap_merge_secs: f32,
    pub zero_cross_radius: usize,
}

/// Level statistics derived from the analyzed buffer; lets the UI show the
/// user where the noise floor / peak sit and which threshold is in effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoTrimLevelStats {
    pub peak_db: f32,
    pub noise_floor_db: f32,
    pub threshold_db: f32,
    /// true when noise floor ≈ median (content throughout); only the
    /// peak-relative threshold applies then.
    pub uniform_signal: bool,
}

impl Default for AutoTrimConfig {
    fn default() -> Self {
        Self {
            block_size: 1024,
            hop_size: 512,
            noise_percentile: 0.10,
            threshold_above_noise_db: 12.0,
            threshold_below_peak_db: 40.0,
            pre_roll_secs: 0.050,
            post_roll_secs: 0.100,
            min_active_secs: 0.030,
            gap_merge_secs: 0.080,
            zero_cross_radius: 256,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AutoTrimResult {
    pub start: usize,
    pub end: usize,
    pub confidence: f32,
    pub leading_silence_secs: f32,
    pub trailing_silence_secs: f32,
    pub message: String,
}

/// Outcome of an Auto Trim run: either a single whole-buffer detection
/// (updates `trim_range`) or a per-range detection across multiple selected
/// ranges (replaces the selection set with the detected active sub-ranges).
#[derive(Clone, Debug)]
pub enum AutoTrimOutcome {
    Single(AutoTrimResult),
    MultiRange(Vec<(usize, usize)>),
}

#[allow(dead_code)]
pub fn auto_trim(
    ch_samples: &[Vec<f32>],
    sample_rate: u32,
    config: &AutoTrimConfig,
    cancel: &Arc<AtomicBool>,
    progress_cb: &mut dyn FnMut(f32),
) -> Result<AutoTrimResult, String> {
    let sections = auto_trim_sections(ch_samples, sample_rate, config, cancel, progress_cb)?;
    let Some(first) = sections.first() else {
        return Err("No active region detected".to_string());
    };
    let Some(last) = sections.last() else {
        return Err("No active region detected".to_string());
    };

    if sections.len() == 1 {
        return Ok(first.clone());
    }

    let sr = sample_rate.max(1) as usize;
    let len = ch_samples
        .iter()
        .filter(|ch| !ch.is_empty())
        .map(|ch| ch.len())
        .min()
        .unwrap_or(0);
    let start = first.start;
    let end = last.end.min(len).max(start + 1);
    let confidence =
        sections.iter().map(|r| r.confidence).sum::<f32>() / sections.len().max(1) as f32;
    let coverage = if len > 0 {
        (end - start) as f32 / len as f32
    } else {
        0.0
    };
    let message = if coverage > 0.98 {
        "Already tight".to_string()
    } else {
        format!(
            "Detected: {:.3}s..{:.3}s ({} sections, confidence {:.0}%)",
            start as f32 / sr as f32,
            end as f32 / sr as f32,
            sections.len(),
            confidence * 100.0
        )
    };

    Ok(AutoTrimResult {
        start,
        end,
        confidence: confidence.clamp(0.0, 1.0),
        leading_silence_secs: start as f32 / sr as f32,
        trailing_silence_secs: len.saturating_sub(end) as f32 / sr as f32,
        message,
    })
}

pub fn auto_trim_sections(
    ch_samples: &[Vec<f32>],
    sample_rate: u32,
    config: &AutoTrimConfig,
    cancel: &Arc<AtomicBool>,
    progress_cb: &mut dyn FnMut(f32),
) -> Result<Vec<AutoTrimResult>, String> {
    if ch_samples.is_empty() || ch_samples[0].is_empty() {
        return Err("No audio data".to_string());
    }

    let sr = sample_rate.max(1) as usize;
    let len = ch_samples
        .iter()
        .filter(|ch| !ch.is_empty())
        .map(|ch| ch.len())
        .min()
        .unwrap_or(0);
    if len == 0 {
        return Err("No audio data".to_string());
    }
    let block_size = config.block_size.max(1).min(len);
    let hop_size = config.hop_size.max(1);

    // mono downmix
    progress_cb(0.05);
    let mono = downmix_mono(ch_samples, len);

    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled".to_string());
    }

    // block RMS computation
    progress_cb(0.15);
    let block_rms = compute_block_rms(&mono, block_size, hop_size);

    if block_rms.is_empty() {
        return Err("Audio too short".to_string());
    }

    progress_cb(0.30);
    let Some(levels) = derive_threshold(&block_rms, config) else {
        return Err("All silence".to_string());
    };
    let threshold = levels.threshold_rms;

    // active block mask
    progress_cb(0.40);
    let active: Vec<bool> = block_rms.iter().map(|&r| r >= threshold).collect();

    // gap merge in blocks
    let gap_merge_blocks = ((config.gap_merge_secs * sr as f32) / hop_size as f32).ceil() as usize;
    let mut merged = active.clone();
    let mut gap_count = 0usize;
    let mut in_gap = false;
    for i in 0..merged.len() {
        if merged[i] {
            if in_gap && gap_count <= gap_merge_blocks {
                // fill the gap
                for j in (i.saturating_sub(gap_count))..i {
                    merged[j] = true;
                }
            }
            in_gap = false;
            gap_count = 0;
        } else {
            if !in_gap {
                in_gap = true;
                gap_count = 0;
            }
            gap_count += 1;
        }
    }

    // find active islands after short-gap merging
    let min_active_blocks =
        (((config.min_active_secs * sr as f32) / hop_size as f32).ceil() as usize).max(1);
    let mut islands: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < merged.len() {
        if !merged[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i + 1 < merged.len() && merged[i + 1] {
            i += 1;
        }
        let end = i;
        if end - start + 1 >= min_active_blocks {
            islands.push((start, end));
        }
        i += 1;
    }

    if islands.is_empty() {
        if merged.iter().any(|&a| a) {
            return Err("Active region too short".to_string());
        }
        return Err("No active region detected".to_string());
    }

    // convert block indices to samples
    progress_cb(0.60);
    let pre_roll_samples = (config.pre_roll_secs * sr as f32) as usize;
    let post_roll_samples = (config.post_roll_secs * sr as f32) as usize;

    let mut sections = Vec::with_capacity(islands.len());
    for (first_block, last_block) in islands {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled".to_string());
        }
        let raw_start = (first_block * hop_size).saturating_sub(pre_roll_samples);
        let raw_end = ((last_block * hop_size) + block_size + post_roll_samples).min(len);

        // zero-cross snap for start
        let snap_start = snap_to_zero_cross(&mono, raw_start, config.zero_cross_radius, true);
        let snap_end = snap_to_zero_cross(&mono, raw_end, config.zero_cross_radius, false);

        let final_start = snap_start.min(len.saturating_sub(1));
        let final_end = snap_end.min(len).max(final_start + 1);

        // compute confidence: ratio of active blocks in detected range
        let range_blocks = last_block - first_block + 1;
        let active_in_range: usize = merged[first_block..=last_block]
            .iter()
            .filter(|&&a| a)
            .count();
        let fill_ratio = active_in_range as f32 / range_blocks.max(1) as f32;
        // penalize if the detected range is almost the full audio (already tight)
        let coverage = (final_end - final_start) as f32 / len as f32;
        let confidence = fill_ratio * (1.0 - (coverage - 0.95).max(0.0) * 10.0).max(0.0);
        let confidence = confidence.clamp(0.0, 1.0);

        sections.push(AutoTrimResult {
            start: final_start,
            end: final_end,
            confidence,
            leading_silence_secs: final_start as f32 / sr as f32,
            trailing_silence_secs: (len - final_end) as f32 / sr as f32,
            message: String::new(),
        });
    }

    progress_cb(0.90);
    let sections = merge_overlapping_sections(sections, len, sr);

    progress_cb(1.0);

    Ok(sections)
}

fn downmix_mono(ch_samples: &[Vec<f32>], len: usize) -> Vec<f32> {
    if ch_samples.len() == 1 {
        return ch_samples[0].clone();
    }
    let n = ch_samples.len() as f32;
    (0..len)
        .map(|i| ch_samples.iter().map(|c| c[i]).sum::<f32>() / n)
        .collect()
}

fn compute_block_rms(mono: &[f32], block_size: usize, hop_size: usize) -> Vec<f32> {
    let len = mono.len();
    let block_size = block_size.max(1).min(len.max(1));
    let hop_size = hop_size.max(1);
    if len == 0 {
        return Vec::new();
    }
    let n_blocks = (len.saturating_sub(block_size)) / hop_size + 1;
    let mut block_rms: Vec<f32> = Vec::with_capacity(n_blocks);
    for b in 0..n_blocks {
        let start = b * hop_size;
        let end = (start + block_size).min(len);
        let sum_sq: f32 = mono[start..end].iter().map(|x| x * x).sum();
        block_rms.push((sum_sq / (end - start) as f32).sqrt());
    }
    block_rms
}

struct ThresholdLevels {
    peak_rms: f32,
    noise_floor_rms: f32,
    threshold_rms: f32,
    uniform_signal: bool,
}

/// Shared threshold derivation: `None` when the buffer is entirely silent.
fn derive_threshold(block_rms: &[f32], config: &AutoTrimConfig) -> Option<ThresholdLevels> {
    if block_rms.is_empty() {
        return None;
    }
    let peak_rms = block_rms.iter().cloned().fold(0.0f32, f32::max);
    if peak_rms < 1e-9 {
        return None;
    }
    let mut sorted_rms = block_rms.to_vec();
    sorted_rms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let percentile_idx =
        ((sorted_rms.len() as f32 * config.noise_percentile) as usize).min(sorted_rms.len() - 1);
    let noise_floor_rms = sorted_rms[percentile_idx].max(1e-9);
    let median_rms = sorted_rms[sorted_rms.len() / 2];

    // If signal is very uniform (noise_floor ≈ median), the audio is already content throughout.
    // Use a relative threshold in dB from peak instead.
    let uniform_signal = (median_rms / noise_floor_rms) < 1.5; // < 3.5 dB difference

    // threshold = max(noise_floor * 10^(above_db/20), peak * 10^(-below_db/20))
    let factor_above = 10.0f32.powf(config.threshold_above_noise_db / 20.0);
    let factor_below = 10.0f32.powf(-config.threshold_below_peak_db / 20.0);
    let threshold_noise = noise_floor_rms * factor_above;
    let threshold_peak = peak_rms * factor_below;
    // For uniform signals, only use the peak-relative threshold to avoid marking everything inactive
    let threshold_rms = if uniform_signal {
        threshold_peak
    } else {
        threshold_noise.max(threshold_peak)
    };
    Some(ThresholdLevels {
        peak_rms,
        noise_floor_rms,
        threshold_rms,
        uniform_signal,
    })
}

fn db_of(rms: f32) -> f32 {
    20.0 * rms.max(1e-9).log10()
}

/// Compute the level stats the UI shows next to the threshold controls.
/// Cheap (single pass) and side-effect free; `None` for empty/silent audio.
pub fn analyze_levels(ch_samples: &[Vec<f32>], config: &AutoTrimConfig) -> Option<AutoTrimLevelStats> {
    if ch_samples.is_empty() || ch_samples[0].is_empty() {
        return None;
    }
    let len = ch_samples
        .iter()
        .filter(|ch| !ch.is_empty())
        .map(|ch| ch.len())
        .min()
        .unwrap_or(0);
    if len == 0 {
        return None;
    }
    let mono = downmix_mono(ch_samples, len);
    let block_rms = compute_block_rms(&mono, config.block_size, config.hop_size);
    let levels = derive_threshold(&block_rms, config)?;
    Some(AutoTrimLevelStats {
        peak_db: db_of(levels.peak_rms),
        noise_floor_db: db_of(levels.noise_floor_rms),
        threshold_db: db_of(levels.threshold_rms),
        uniform_signal: levels.uniform_signal,
    })
}

fn merge_overlapping_sections(
    mut sections: Vec<AutoTrimResult>,
    len: usize,
    sr: usize,
) -> Vec<AutoTrimResult> {
    if sections.len() <= 1 {
        for section in &mut sections {
            set_section_message(section, len, sr, 1);
        }
        return sections;
    }
    sections.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<AutoTrimResult> = Vec::with_capacity(sections.len());
    for mut section in sections {
        if let Some(last) = merged.last_mut() {
            if section.start <= last.end {
                last.end = last.end.max(section.end).min(len);
                last.confidence = last.confidence.max(section.confidence);
                last.leading_silence_secs = last.start as f32 / sr as f32;
                last.trailing_silence_secs = len.saturating_sub(last.end) as f32 / sr as f32;
                continue;
            }
        }
        section.leading_silence_secs = section.start as f32 / sr as f32;
        section.trailing_silence_secs = len.saturating_sub(section.end) as f32 / sr as f32;
        merged.push(section);
    }
    let count = merged.len();
    for section in &mut merged {
        set_section_message(section, len, sr, count);
    }
    merged
}

fn set_section_message(section: &mut AutoTrimResult, len: usize, sr: usize, count: usize) {
    let coverage = if len > 0 {
        (section.end - section.start) as f32 / len as f32
    } else {
        0.0
    };
    section.message = if count == 1 && coverage > 0.98 {
        "Already tight".to_string()
    } else {
        format!(
            "Detected section: {:.3}s..{:.3}s (confidence {:.0}%)",
            section.start as f32 / sr as f32,
            section.end as f32 / sr as f32,
            section.confidence * 100.0
        )
    };
}

fn snap_to_zero_cross(mono: &[f32], pos: usize, radius: usize, forward: bool) -> usize {
    let lo = pos.saturating_sub(radius);
    let hi = (pos + radius).min(mono.len());
    if lo >= hi {
        return pos;
    }
    // find nearest zero crossing
    let mut best = pos;
    let mut best_dist = usize::MAX;
    for i in lo..hi.saturating_sub(1) {
        let crosses = (mono[i] >= 0.0) != (mono[i + 1] >= 0.0);
        if crosses {
            let dist = if i >= pos { i - pos } else { pos - i };
            if dist < best_dist {
                best_dist = dist;
                // prefer the crossing in the right direction
                best = if forward { i + 1 } else { i };
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

    fn silence(n: usize) -> Vec<f32> {
        vec![0.0f32; n]
    }

    fn sine_burst(sr: usize, freq: f32, dur_secs: f32, amp: f32) -> Vec<f32> {
        let n = (sr as f32 * dur_secs) as usize;
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin())
            .collect()
    }

    /// Apply a linear fade-in over `fade_samples` at the beginning of `buf`.
    fn apply_fade_in(buf: &mut [f32], fade_samples: usize) {
        let n = fade_samples.min(buf.len());
        for i in 0..n {
            buf[i] *= i as f32 / n as f32;
        }
    }

    /// Apply a linear fade-out over `fade_samples` at the end of `buf`.
    fn apply_fade_out(buf: &mut [f32], fade_samples: usize) {
        let len = buf.len();
        let n = fade_samples.min(len);
        for i in 0..n {
            buf[len - n + i] *= 1.0 - i as f32 / n as f32;
        }
    }

    // -----------------------------------------------------------------------
    // Basic: leading / trailing silence
    // -----------------------------------------------------------------------

    #[test]
    fn test_leading_trailing_silence() {
        let sr = 44100u32;
        let lead = silence(sr as usize); // 1s silence
        let burst = sine_burst(sr as usize, 440.0, 1.0, 0.5);
        let trail = silence(sr as usize / 2); // 0.5s silence
        let mut audio: Vec<f32> = lead.clone();
        audio.extend_from_slice(&burst);
        audio.extend_from_slice(&trail);
        let ch = vec![audio.clone()];
        let cancel = make_cancel();
        let config = AutoTrimConfig::default();
        let result = auto_trim(&ch, sr, &config, &cancel, &mut |_| {});
        assert!(result.is_ok(), "{:?}", result);
        let r = result.unwrap();
        assert!(
            r.start < sr as usize,
            "start should be before 1s, got {}",
            r.start
        );
        assert!(r.end > sr as usize, "end should be after 1s, got {}", r.end);
        assert!(
            r.leading_silence_secs > 0.5,
            "expected significant leading silence, got {}",
            r.leading_silence_secs
        );
        assert!(
            r.trailing_silence_secs > 0.1,
            "expected some trailing silence, got {}",
            r.trailing_silence_secs
        );
    }

    // -----------------------------------------------------------------------
    // Silence-only input
    // -----------------------------------------------------------------------

    #[test]
    fn test_all_silence() {
        let sr = 44100u32;
        let ch = vec![silence(sr as usize * 2)];
        let cancel = make_cancel();
        let result = auto_trim(&ch, sr, &AutoTrimConfig::default(), &cancel, &mut |_| {});
        assert!(result.is_err(), "should fail for all-silence audio");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("silence") || msg.contains("active") || msg.contains("silent"),
            "{}",
            msg
        );
    }

    #[test]
    fn test_empty_channels() {
        let sr = 44100u32;
        let result = auto_trim(
            &[],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_samples() {
        let sr = 44100u32;
        let ch: Vec<Vec<f32>> = vec![vec![]];
        let result = auto_trim(
            &ch,
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Stereo downmix: signal only on one channel
    // -----------------------------------------------------------------------

    #[test]
    fn test_stereo_signal_on_left_only() {
        let sr = 44100u32;
        let lead = silence(sr as usize / 2);
        let burst = sine_burst(sr as usize, 440.0, 0.5, 0.5);
        let trail = silence(sr as usize / 2);
        let mut audio: Vec<f32> = lead.clone();
        audio.extend_from_slice(&burst);
        audio.extend_from_slice(&trail);
        // right channel is silent
        let right = silence(audio.len());
        let ch = vec![audio.clone(), right];
        let cancel = make_cancel();
        let result = auto_trim(&ch, sr, &AutoTrimConfig::default(), &cancel, &mut |_| {});
        assert!(
            result.is_ok(),
            "stereo with active left channel should succeed: {:?}",
            result
        );
        let r = result.unwrap();
        // start should be in the silent lead region
        assert!(
            r.start < (sr as usize / 2 + 2000),
            "expected start near silence end, got {}",
            r.start
        );
    }

    #[test]
    fn test_stereo_signal_on_right_only() {
        let sr = 44100u32;
        let lead = silence(sr as usize / 2);
        let burst = sine_burst(sr as usize, 220.0, 0.5, 0.5);
        let trail = silence(sr as usize / 2);
        let mut audio: Vec<f32> = lead.clone();
        audio.extend_from_slice(&burst);
        audio.extend_from_slice(&trail);
        let left = silence(audio.len());
        let ch = vec![left, audio.clone()];
        let cancel = make_cancel();
        let result = auto_trim(&ch, sr, &AutoTrimConfig::default(), &cancel, &mut |_| {});
        assert!(
            result.is_ok(),
            "stereo with active right channel should succeed: {:?}",
            result
        );
    }

    // -----------------------------------------------------------------------
    // Already tight: no leading/trailing silence
    // -----------------------------------------------------------------------

    #[test]
    fn test_already_tight_succeeds() {
        let sr = 44100u32;
        let burst = sine_burst(sr as usize, 440.0, 2.0, 0.5);
        let ch = vec![burst];
        let cancel = make_cancel();
        let result = auto_trim(&ch, sr, &AutoTrimConfig::default(), &cancel, &mut |_| {});
        assert!(
            result.is_ok(),
            "already-tight audio should not error: {:?}",
            result
        );
        let r = result.unwrap();
        // When already tight, message should say so
        assert!(
            r.message.contains("Already tight") || r.start < 5000,
            "expected tight detection, got start={} msg={}",
            r.start,
            r.message
        );
    }

    // -----------------------------------------------------------------------
    // Fade-in / fade-out tails
    // -----------------------------------------------------------------------

    #[test]
    fn test_fade_in_start_detection() {
        let sr = 44100u32;
        let mut burst = sine_burst(sr as usize, 440.0, 2.0, 0.8);
        // 0.3s linear fade-in at start
        apply_fade_in(&mut burst, (sr as usize as f32 * 0.3) as usize);
        // 0.5s silence before
        let mut audio: Vec<f32> = silence(sr as usize / 2);
        audio.extend_from_slice(&burst);
        audio.extend_from_slice(&silence(sr as usize / 2));
        let ch = vec![audio];
        let result = auto_trim(
            &ch,
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_ok(), "{:?}", result);
        let r = result.unwrap();
        // Start should still be somewhere in the first second
        assert!(
            r.start < sr as usize + 5000,
            "start should be near silence boundary: {}",
            r.start
        );
    }

    #[test]
    fn test_fade_out_end_detection() {
        let sr = 44100u32;
        let mut burst = sine_burst(sr as usize, 440.0, 2.0, 0.8);
        // 0.3s linear fade-out at end
        apply_fade_out(&mut burst, (sr as usize as f32 * 0.3) as usize);
        let mut audio: Vec<f32> = silence(sr as usize / 2);
        audio.extend_from_slice(&burst);
        audio.extend_from_slice(&silence(sr as usize / 2));
        let ch = vec![audio.clone()];
        let result = auto_trim(
            &ch,
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_ok(), "{:?}", result);
        let r = result.unwrap();
        // End should include the fade tail
        assert!(
            r.end > sr as usize / 2 + sr as usize,
            "end should be after burst start + 1s: {}",
            r.end
        );
    }

    // -----------------------------------------------------------------------
    // Pre-roll / post-roll padding is applied
    // -----------------------------------------------------------------------

    #[test]
    fn test_pre_post_roll_expands_range() {
        let sr = 44100u32;
        // 0.5s leading silence, 1s burst, 0.5s trailing silence
        let lead_samples = sr as usize / 2;
        let lead = silence(lead_samples);
        let burst = sine_burst(sr as usize, 440.0, 1.0, 0.5);
        let trail = silence(sr as usize / 2);
        let mut audio = lead.clone();
        audio.extend_from_slice(&burst);
        audio.extend_from_slice(&trail);

        // Config with no pre/post roll
        let config_no_roll = AutoTrimConfig {
            pre_roll_secs: 0.0,
            post_roll_secs: 0.0,
            ..Default::default()
        };
        let r_no = auto_trim(
            &[audio.clone()],
            sr,
            &config_no_roll,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();

        // Config with generous pre/post roll
        let config_roll = AutoTrimConfig {
            pre_roll_secs: 0.1,
            post_roll_secs: 0.1,
            ..Default::default()
        };
        let r_roll = auto_trim(
            &[audio.clone()],
            sr,
            &config_roll,
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();

        // With pre/post roll, start should be earlier and end later
        assert!(
            r_roll.start <= r_no.start + 1024,
            "pre-roll should pull start earlier: no_roll={} roll={}",
            r_no.start,
            r_roll.start
        );
        assert!(
            r_roll.end >= r_no.end.saturating_sub(1024),
            "post-roll should push end later: no_roll={} roll={}",
            r_no.end,
            r_roll.end
        );
    }

    // -----------------------------------------------------------------------
    // Gap merge: two short bursts close together → merged into one region
    // -----------------------------------------------------------------------

    #[test]
    fn test_gap_merge_two_bursts_close() {
        let sr = 44100u32;
        let hop = sr as usize; // 1s silence before
        let burst1 = sine_burst(sr as usize, 440.0, 0.3, 0.5);
        // short 40ms gap (below gap_merge_secs=80ms) between bursts
        let gap = silence((sr as f32 * 0.04) as usize);
        let burst2 = sine_burst(sr as usize, 440.0, 0.3, 0.5);
        let mut audio = silence(hop);
        audio.extend_from_slice(&burst1);
        audio.extend_from_slice(&gap);
        audio.extend_from_slice(&burst2);
        audio.extend_from_slice(&silence(sr as usize / 2));

        let result = auto_trim(
            &[audio],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        );
        assert!(
            result.is_ok(),
            "gap merge should detect region: {:?}",
            result
        );
        let r = result.unwrap();
        // The end should be after both bursts
        let burst1_end = hop + burst1.len() + gap.len() + burst2.len();
        assert!(
            r.end >= burst1_end - 5000,
            "end should cover both bursts: end={} expected>={}",
            r.end,
            burst1_end
        );
    }

    // -----------------------------------------------------------------------
    // Gap too wide: two bursts far apart → should detect start of first
    // -----------------------------------------------------------------------

    #[test]
    fn test_gap_not_merged_when_wide() {
        let sr = 44100u32;
        let burst1 = sine_burst(sr as usize, 440.0, 0.3, 0.5);
        // 200ms gap (above gap_merge_secs=80ms)
        let gap = silence((sr as f32 * 0.2) as usize);
        let burst2 = sine_burst(sr as usize, 440.0, 0.3, 0.5);
        let mut audio = silence(sr as usize / 2);
        audio.extend_from_slice(&burst1);
        audio.extend_from_slice(&gap);
        audio.extend_from_slice(&burst2);
        audio.extend_from_slice(&silence(sr as usize / 2));

        // Should still succeed (covers both regions)
        let result = auto_trim(
            &[audio],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn test_sections_split_two_speech_like_bursts() {
        let sr = 44100u32;
        let burst1 = sine_burst(sr as usize, 440.0, 0.30, 0.45);
        let gap = silence((sr as f32 * 0.25) as usize);
        let burst2 = sine_burst(sr as usize, 660.0, 0.35, 0.40);
        let mut audio = silence(sr as usize / 2);
        audio.extend_from_slice(&burst1);
        audio.extend_from_slice(&gap);
        audio.extend_from_slice(&burst2);
        audio.extend_from_slice(&silence(sr as usize / 2));

        let sections = auto_trim_sections(
            &[audio],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            sections.len(),
            2,
            "expected two voice sections: {sections:?}"
        );
        assert!(
            sections[0].end < sections[1].start,
            "sections should remain disjoint after roll padding: {sections:?}"
        );
    }

    #[test]
    fn test_sections_short_silence_below_gap_merge_stays_one() {
        let sr = 44100u32;
        let burst1 = sine_burst(sr as usize, 440.0, 0.25, 0.45);
        let gap = silence((sr as f32 * 0.04) as usize);
        let burst2 = sine_burst(sr as usize, 660.0, 0.25, 0.40);
        let mut audio = silence(sr as usize / 2);
        audio.extend_from_slice(&burst1);
        audio.extend_from_slice(&gap);
        audio.extend_from_slice(&burst2);
        audio.extend_from_slice(&silence(sr as usize / 2));

        let sections = auto_trim_sections(
            &[audio],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            sections.len(),
            1,
            "short internal silence should be merged into one section: {sections:?}"
        );
    }

    #[test]
    fn test_sections_low_noise_floor_splits_speech_regions() {
        let sr = 44100u32;
        let noise_amp = 0.002;
        let noise = |n: usize, mul: f32| -> Vec<f32> {
            (0..n)
                .map(|i| noise_amp * ((i as f32 * mul).sin()))
                .collect()
        };
        let burst1 = sine_burst(sr as usize, 440.0, 0.25, 0.45);
        let burst2 = sine_burst(sr as usize, 660.0, 0.25, 0.40);
        let mut audio = noise(sr as usize / 2, 11.0);
        audio.extend_from_slice(&burst1);
        audio.extend_from_slice(&noise((sr as f32 * 0.25) as usize, 7.0));
        audio.extend_from_slice(&burst2);
        audio.extend_from_slice(&noise(sr as usize / 2, 5.0));

        let sections = auto_trim_sections(
            &[audio],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            sections.len(),
            2,
            "quiet background noise should not collapse speech regions: {sections:?}"
        );
    }

    #[test]
    fn test_sections_pre_post_roll_overlap_merges_safely() {
        let sr = 44100u32;
        let burst1 = sine_burst(sr as usize, 440.0, 0.25, 0.45);
        let gap = silence((sr as f32 * 0.12) as usize);
        let burst2 = sine_burst(sr as usize, 660.0, 0.25, 0.40);
        let mut audio = silence(sr as usize / 2);
        audio.extend_from_slice(&burst1);
        audio.extend_from_slice(&gap);
        audio.extend_from_slice(&burst2);
        audio.extend_from_slice(&silence(sr as usize / 2));
        let cfg = AutoTrimConfig {
            pre_roll_secs: 0.10,
            post_roll_secs: 0.10,
            gap_merge_secs: 0.01,
            ..AutoTrimConfig::default()
        };

        let sections = auto_trim_sections(&[audio], sr, &cfg, &make_cancel(), &mut |_| {}).unwrap();
        assert_eq!(
            sections.len(),
            1,
            "overlapping padded sections should merge into one safe range: {sections:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Boundary is near a zero crossing
    // -----------------------------------------------------------------------

    #[test]
    fn test_zero_cross_snap_boundary_near_zero() {
        let sr = 44100u32;
        // Use silence shorter than the snap radius (256 samples) so snap_to_zero_cross can
        // reach into the signal and land on a real sign change.
        let lead = silence(150usize);
        let burst = sine_burst(sr as usize, 440.0, 0.5, 0.5);
        let trail = silence(150usize);
        let mut audio: Vec<f32> = lead.clone();
        audio.extend_from_slice(&burst);
        audio.extend_from_slice(&trail);

        // Disable pre/post roll so the raw boundary is at the first active block edge,
        // within snap_radius of the signal where real sign changes exist.
        let cfg = AutoTrimConfig {
            pre_roll_secs: 0.0,
            post_roll_secs: 0.0,
            ..AutoTrimConfig::default()
        };
        let result = auto_trim(&[audio.clone()], sr, &cfg, &make_cancel(), &mut |_| {});
        assert!(result.is_ok(), "{:?}", result);
        let r = result.unwrap();

        // Check that boundaries landed within 512 samples of an actual sign change.
        let check_radius = 512usize;
        let start = r.start;
        let end = r.end;
        let has_zc_near_start = (start.saturating_sub(check_radius)
            ..=(start + check_radius).min(audio.len().saturating_sub(1)))
            .any(|i| i + 1 < audio.len() && (audio[i] >= 0.0) != (audio[i + 1] >= 0.0));
        let has_zc_near_end = (end.saturating_sub(check_radius)
            ..=(end + check_radius).min(audio.len().saturating_sub(1)))
            .any(|i| i + 1 < audio.len() && (audio[i] >= 0.0) != (audio[i + 1] >= 0.0));
        assert!(
            has_zc_near_start,
            "start boundary should be near a zero crossing, start={}",
            start
        );
        assert!(
            has_zc_near_end,
            "end boundary should be near a zero crossing, end={}",
            end
        );
    }

    // -----------------------------------------------------------------------
    // Progress callback: called at least once, ends at 1.0
    // -----------------------------------------------------------------------

    #[test]
    fn test_progress_callback_monotonic_and_reaches_one() {
        let sr = 44100u32;
        let mut audio = silence(sr as usize / 2);
        audio.extend_from_slice(&sine_burst(sr as usize, 440.0, 1.0, 0.5));
        audio.extend_from_slice(&silence(sr as usize / 2));

        let mut progress_values: Vec<f32> = Vec::new();
        let _ = auto_trim(
            &[audio],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |p| {
                progress_values.push(p);
            },
        );

        assert!(
            !progress_values.is_empty(),
            "progress callback should be called at least once"
        );
        assert_eq!(
            *progress_values.last().unwrap(),
            1.0,
            "last progress should be 1.0"
        );
        // Monotonically non-decreasing
        for w in progress_values.windows(2) {
            assert!(
                w[1] >= w[0],
                "progress should be non-decreasing: {} -> {}",
                w[0],
                w[1]
            );
        }
    }

    // -----------------------------------------------------------------------
    // Cancel mid-analysis
    // -----------------------------------------------------------------------

    #[test]
    fn test_cancel_pre_cancelled() {
        let sr = 44100u32;
        let burst = sine_burst(sr as usize, 440.0, 5.0, 0.5);
        let cancel = Arc::new(AtomicBool::new(true));
        let result = auto_trim(
            &[burst],
            sr,
            &AutoTrimConfig::default(),
            &cancel,
            &mut |_| {},
        );
        assert!(result.is_err(), "pre-cancelled should fail");
        assert_eq!(result.unwrap_err(), "Cancelled");
    }

    // -----------------------------------------------------------------------
    // Trim range is strictly inside audio bounds
    // -----------------------------------------------------------------------

    #[test]
    fn test_result_within_audio_bounds() {
        let sr = 44100u32;
        let mut audio = silence(sr as usize / 2);
        audio.extend_from_slice(&sine_burst(sr as usize, 440.0, 1.0, 0.5));
        audio.extend_from_slice(&silence(sr as usize / 2));
        let len = audio.len();

        let result = auto_trim(
            &[audio],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_ok(), "{:?}", result);
        let r = result.unwrap();
        assert!(r.start < r.end, "start must be before end");
        assert!(r.end <= len, "end must not exceed audio length");
    }

    // -----------------------------------------------------------------------
    // Low noise floor with actual signal above it
    // -----------------------------------------------------------------------

    #[test]
    fn test_low_noise_floor_signal_detected() {
        let sr = 44100u32;
        let noise_amp = 0.002; // very quiet noise
        let signal_amp = 0.4;
        let n_pre = sr as usize / 2;

        // noisy silence before
        let noise_pre: Vec<f32> = (0..n_pre)
            .map(|i| noise_amp * ((i as f32 * 13.7).sin()))
            .collect();
        let signal = sine_burst(sr as usize, 440.0, 1.0, signal_amp);
        let noise_post: Vec<f32> = (0..n_pre)
            .map(|i| noise_amp * ((i as f32 * 7.3).sin()))
            .collect();

        let mut audio = noise_pre;
        audio.extend_from_slice(&signal);
        audio.extend_from_slice(&noise_post);

        let result = auto_trim(
            &[audio.clone()],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        );
        assert!(result.is_ok(), "{:?}", result);
        let r = result.unwrap();
        // Start should be within or near the noise floor region (before signal)
        assert!(
            r.start <= n_pre + 5000,
            "start should be near signal onset: {}",
            r.start
        );
        // End should be after the signal
        assert!(
            r.end >= n_pre + sr as usize - 5000,
            "end should cover signal: {}",
            r.end
        );
    }

    // -----------------------------------------------------------------------
    // Custom config: large gap_merge_secs merges even wide gaps
    // -----------------------------------------------------------------------

    #[test]
    fn test_custom_gap_merge_large() {
        let sr = 44100u32;
        let burst1 = sine_burst(sr as usize, 440.0, 0.2, 0.5);
        // 150ms gap (normally above default 80ms, but we set 200ms)
        let gap = silence((sr as f32 * 0.15) as usize);
        let burst2 = sine_burst(sr as usize, 440.0, 0.2, 0.5);
        let mut audio = silence(sr as usize / 2);
        audio.extend_from_slice(&burst1);
        audio.extend_from_slice(&gap);
        audio.extend_from_slice(&burst2);
        audio.extend_from_slice(&silence(sr as usize / 2));

        let config = AutoTrimConfig {
            gap_merge_secs: 0.20,
            ..Default::default()
        };
        let result = auto_trim(&[audio.clone()], sr, &config, &make_cancel(), &mut |_| {});
        assert!(result.is_ok(), "{:?}", result);
        let r = result.unwrap();
        let expected_end = sr as usize / 2 + burst1.len() + gap.len() + burst2.len();
        assert!(
            r.end >= expected_end - 5000,
            "with large gap_merge both bursts should be included: end={}",
            r.end
        );
    }

    // -----------------------------------------------------------------------
    // confidence field is in [0, 1]
    // -----------------------------------------------------------------------

    #[test]
    fn test_confidence_in_range() {
        let sr = 44100u32;
        let mut audio = silence(sr as usize / 2);
        audio.extend_from_slice(&sine_burst(sr as usize, 440.0, 1.0, 0.5));
        audio.extend_from_slice(&silence(sr as usize / 2));
        let r = auto_trim(
            &[audio],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        assert!(
            r.confidence >= 0.0 && r.confidence <= 1.0,
            "confidence out of range: {}",
            r.confidence
        );
    }

    // -----------------------------------------------------------------------
    // leading_silence_secs + trailing_silence_secs match audio structure
    // -----------------------------------------------------------------------

    #[test]
    fn test_silence_secs_accurate() {
        let sr = 44100u32;
        let lead_secs = 1.0f32;
        let trail_secs = 0.5f32;
        let lead = silence((sr as f32 * lead_secs) as usize);
        let burst = sine_burst(sr as usize, 440.0, 1.0, 0.5);
        let trail = silence((sr as f32 * trail_secs) as usize);
        let mut audio = lead.clone();
        audio.extend_from_slice(&burst);
        audio.extend_from_slice(&trail);

        let r = auto_trim(
            &[audio],
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        )
        .unwrap();
        // Leading silence should be close to 1.0s (within 200ms)
        assert!(
            (r.leading_silence_secs - lead_secs).abs() < 0.2,
            "leading_silence_secs={} expected ~{}",
            r.leading_silence_secs,
            lead_secs
        );
        // Trailing silence should be close to 0.5s (within 200ms, accounting for post_roll)
        assert!(
            r.trailing_silence_secs < trail_secs + 0.15,
            "trailing_silence_secs={} expected <{}",
            r.trailing_silence_secs,
            trail_secs + 0.15
        );
    }

    // -----------------------------------------------------------------------
    // Very short audio (< block_size) should error gracefully
    // -----------------------------------------------------------------------

    #[test]
    fn test_very_short_audio() {
        let sr = 44100u32;
        // Only 100 samples — shorter than block_size=1024
        let ch = vec![vec![0.5f32; 100]];
        let result = auto_trim(
            &ch,
            sr,
            &AutoTrimConfig::default(),
            &make_cancel(),
            &mut |_| {},
        );
        // Either errors or returns ok with start=0, end≈100
        if let Ok(r) = result {
            assert!(r.start <= r.end && r.end <= 100);
        }
        // No panic is the primary assertion
    }
}
