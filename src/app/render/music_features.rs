use rustfft::{num_complex::Complex, FftPlanner};

use crate::app::types::{ChromagramData, SpectrogramConfig, TempogramData, WindowFunction};

const MIN_CHROMA_FREQ_HZ: f32 = 27.5;
const MAX_CHROMA_FREQ_HZ: f32 = 5_000.0;
const MIN_TEMPO_BPM: f32 = 30.0;
const MAX_TEMPO_BPM: f32 = 300.0;
const PREFERRED_MIN_TEMPO_BPM: f32 = 60.0;
const PREFERRED_MAX_TEMPO_BPM: f32 = 180.0;
const TEMPO_WINDOW_SECS: f32 = 8.0;

struct StftPowerData {
    frames: usize,
    bins: usize,
    frame_step: usize,
    fft_size: usize,
    sample_rate: u32,
    values: Vec<f32>,
}

pub fn compute_tempogram(mono: &[f32], sample_rate: u32, cfg: &SpectrogramConfig) -> TempogramData {
    let stft = compute_stft_power(mono, sample_rate, cfg);
    if stft.frames == 0 || stft.bins == 0 {
        return TempogramData {
            frames: 0,
            tempo_bins: 0,
            frame_step: stft.frame_step.max(1),
            sample_rate,
            bpm_values: Vec::new(),
            values: Vec::new(),
            estimated_bpm: None,
            confidence: 0.0,
        };
    }

    let onset = spectral_flux_onset_envelope(&stft);
    let hop_sec = stft.frame_step.max(1) as f32 / stft.sample_rate.max(1) as f32;
    let min_lag = ((60.0 / MAX_TEMPO_BPM) / hop_sec).floor().max(1.0) as usize;
    let mut max_lag = ((60.0 / MIN_TEMPO_BPM) / hop_sec)
        .ceil()
        .max(min_lag as f32) as usize;
    max_lag = max_lag.min(stft.frames.saturating_sub(1).max(min_lag));
    if max_lag < min_lag {
        return TempogramData {
            frames: stft.frames,
            tempo_bins: 0,
            frame_step: stft.frame_step,
            sample_rate,
            bpm_values: Vec::new(),
            values: Vec::new(),
            estimated_bpm: None,
            confidence: 0.0,
        };
    }

    let lags: Vec<usize> = (min_lag..=max_lag).rev().collect();
    let bpm_values: Vec<f32> = lags
        .iter()
        .map(|&lag| 60.0 / (lag as f32 * hop_sec))
        .collect();
    let window_frames =
        ((TEMPO_WINDOW_SECS / hop_sec).round() as usize).clamp(16, stft.frames.max(16));
    let half_window = window_frames / 2;
    let mut values = vec![0.0f32; stft.frames.saturating_mul(lags.len())];
    for frame in 0..stft.frames {
        let start = frame.saturating_sub(half_window);
        let end = (frame + half_window + 1).min(stft.frames);
        let segment = &onset[start..end];
        let row = &mut values[frame * lags.len()..(frame + 1) * lags.len()];
        for (idx, &lag) in lags.iter().enumerate() {
            if lag >= segment.len() {
                continue;
            }
            let limit = segment.len() - lag;
            let mut acc = 0.0f32;
            let mut norm = 0.0f32;
            for i in 0..limit {
                let taper = triangular_weight(i, limit);
                acc += segment[i] * segment[i + lag] * taper;
                norm += taper;
            }
            if norm > 0.0 {
                row[idx] = acc / norm;
            }
        }
    }

    let mut tempo_profile = vec![0.0f32; lags.len()];
    for frame in 0..stft.frames {
        let row = &values[frame * lags.len()..(frame + 1) * lags.len()];
        for (idx, value) in row.iter().enumerate() {
            tempo_profile[idx] += *value;
        }
    }
    if stft.frames > 0 {
        let inv = 1.0 / stft.frames as f32;
        for value in &mut tempo_profile {
            *value *= inv;
        }
    }

    let (estimated_bpm, confidence) = estimate_tempo_from_profile(&tempo_profile, &bpm_values);

    TempogramData {
        frames: stft.frames,
        tempo_bins: bpm_values.len(),
        frame_step: stft.frame_step,
        sample_rate,
        bpm_values,
        values,
        estimated_bpm,
        confidence,
    }
}

pub fn compute_chromagram(
    mono: &[f32],
    sample_rate: u32,
    cfg: &SpectrogramConfig,
) -> ChromagramData {
    let stft = compute_stft_power(mono, sample_rate, cfg);
    if stft.frames == 0 || stft.bins == 0 {
        return ChromagramData {
            frames: 0,
            bins: 12,
            frame_step: stft.frame_step.max(1),
            sample_rate,
            values: Vec::new(),
            estimated_key: None,
            estimated_mode: None,
            confidence: 0.0,
        };
    }

    let sr = stft.sample_rate.max(1) as f32;
    let max_freq = if cfg.max_freq_hz > 0.0 {
        cfg.max_freq_hz.min(sr * 0.5).max(MIN_CHROMA_FREQ_HZ)
    } else {
        (sr * 0.5).min(MAX_CHROMA_FREQ_HZ)
    };
    let mut bin_to_pitch = vec![None; stft.bins];
    for bin in 1..stft.bins {
        let freq = bin as f32 * sr / stft.fft_size.max(1) as f32;
        if !(MIN_CHROMA_FREQ_HZ..=max_freq).contains(&freq) {
            continue;
        }
        let midi = 69.0 + 12.0 * (freq / 440.0).log2();
        let rounded = midi.round();
        let dist = (midi - rounded).abs();
        let weight = (1.0 - dist).max(0.0).powf(2.0);
        let pc = (rounded as i32).rem_euclid(12) as usize;
        bin_to_pitch[bin] = Some((pc, weight));
    }

    let mut values = vec![0.0f32; stft.frames * 12];
    for frame in 0..stft.frames {
        let src = &stft.values[frame * stft.bins..(frame + 1) * stft.bins];
        let row = &mut values[frame * 12..(frame + 1) * 12];
        for bin in 1..stft.bins {
            if let Some((pc, weight)) = bin_to_pitch[bin] {
                row[pc] += src[bin] * weight;
            }
        }
        let sum: f32 = row.iter().sum();
        if sum > 0.0 {
            for value in row.iter_mut() {
                *value /= sum;
            }
        }
    }

    let mut profile = [0.0f32; 12];
    for frame in 0..stft.frames {
        let row = &values[frame * 12..(frame + 1) * 12];
        for (idx, value) in row.iter().enumerate() {
            profile[idx] += *value;
        }
    }
    let sum: f32 = profile.iter().sum();
    if sum > 0.0 {
        for value in &mut profile {
            *value /= sum;
        }
    }
    let (estimated_key, estimated_mode, confidence) = estimate_key_mode_from_profile(&profile);

    ChromagramData {
        frames: stft.frames,
        bins: 12,
        frame_step: stft.frame_step,
        sample_rate,
        values,
        estimated_key,
        estimated_mode,
        confidence,
    }
}

fn compute_stft_power(mono: &[f32], sample_rate: u32, cfg: &SpectrogramConfig) -> StftPowerData {
    let params = super::spectrogram::spectrogram_params(mono.len(), cfg);
    if mono.is_empty() || params.frames == 0 || params.bins == 0 {
        return StftPowerData {
            frames: 0,
            bins: params.bins,
            frame_step: params.frame_step,
            fft_size: params.win,
            sample_rate,
            values: Vec::new(),
        };
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(params.win);
    let window = match params.window {
        WindowFunction::Hann => hann_window(params.win),
        WindowFunction::BlackmanHarris => blackman_harris_window(params.win),
    };
    let mut buffer = vec![Complex { re: 0.0, im: 0.0 }; params.win];
    let mut values = vec![0.0f32; params.frames * params.bins];
    for frame in 0..params.frames {
        let center = frame.saturating_mul(params.frame_step);
        let start = center.saturating_sub(params.win / 2);
        for i in 0..params.win {
            let idx = start + i;
            let sample = mono.get(idx).copied().unwrap_or(0.0);
            buffer[i].re = sample * window[i];
            buffer[i].im = 0.0;
        }
        fft.process(&mut buffer);
        let row = &mut values[frame * params.bins..(frame + 1) * params.bins];
        for bin in 0..params.bins {
            let c = buffer[bin];
            row[bin] = c.re * c.re + c.im * c.im;
        }
    }

    StftPowerData {
        frames: params.frames,
        bins: params.bins,
        frame_step: params.frame_step,
        fft_size: params.win,
        sample_rate,
        values,
    }
}

fn spectral_flux_onset_envelope(stft: &StftPowerData) -> Vec<f32> {
    let mut onset = vec![0.0f32; stft.frames];
    if stft.frames <= 1 || stft.bins == 0 {
        return onset;
    }
    for frame in 1..stft.frames {
        let prev = &stft.values[(frame - 1) * stft.bins..frame * stft.bins];
        let curr = &stft.values[frame * stft.bins..(frame + 1) * stft.bins];
        let mut acc = 0.0f32;
        for bin in 1..stft.bins {
            let prev_log = (prev[bin] + 1.0e-9).ln();
            let curr_log = (curr[bin] + 1.0e-9).ln();
            let diff = curr_log - prev_log;
            if diff > 0.0 {
                acc += diff;
            }
        }
        onset[frame] = acc;
    }

    let smoothing = 16usize.min(stft.frames.max(1));
    let mut filtered = vec![0.0f32; onset.len()];
    for idx in 0..onset.len() {
        let start = idx.saturating_sub(smoothing / 2);
        let end = (idx + smoothing / 2 + 1).min(onset.len());
        let mean = onset[start..end].iter().sum::<f32>() / (end - start).max(1) as f32;
        filtered[idx] = (onset[idx] - mean).max(0.0);
    }
    let max_value = filtered
        .iter()
        .copied()
        .fold(0.0f32, |acc, value| acc.max(value));
    if max_value > 0.0 {
        for value in &mut filtered {
            *value /= max_value;
        }
    }
    filtered
}

fn estimate_tempo_from_profile(profile: &[f32], bpm_values: &[f32]) -> (Option<f32>, f32) {
    if profile.is_empty() || profile.len() != bpm_values.len() {
        return (None, 0.0);
    }
    let mut folded_scores = vec![0.0f32; profile.len()];
    for idx in 0..profile.len() {
        let bpm = bpm_values[idx];
        let mut score = profile[idx];
        if let Some(half_idx) = nearest_bpm_index(bpm_values, bpm * 0.5) {
            score += profile[half_idx] * 0.35;
        }
        if let Some(double_idx) = nearest_bpm_index(bpm_values, bpm * 2.0) {
            score += profile[double_idx] * 0.5;
        }
        score *= (bpm.max(PREFERRED_MIN_TEMPO_BPM) / PREFERRED_MIN_TEMPO_BPM).powf(0.25);
        folded_scores[idx] = score;
    }
    let Some((best_idx, &best_score)) = folded_scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
    else {
        return (None, 0.0);
    };
    if best_score <= 0.0 {
        return (None, 0.0);
    }
    let mut chosen_idx = best_idx;
    let best_bpm = bpm_values[best_idx];
    if best_bpm < 90.0 {
        if let Some(double_idx) = nearest_bpm_index(bpm_values, best_bpm * 2.0) {
            if profile[double_idx] >= profile[best_idx] * 0.55 {
                chosen_idx = double_idx;
            }
        }
    } else if best_bpm > PREFERRED_MAX_TEMPO_BPM {
        if let Some(half_idx) = nearest_bpm_index(bpm_values, best_bpm * 0.5) {
            if profile[half_idx] >= profile[best_idx] * 0.55 {
                chosen_idx = half_idx;
            }
        }
    }
    let chosen_bpm = bpm_values[chosen_idx];
    let chosen_score = profile[chosen_idx].max(1.0e-9);
    let second_score = profile
        .iter()
        .enumerate()
        .filter(|(idx, _)| {
            let bpm = bpm_values[*idx];
            let delta = (bpm - chosen_bpm).abs();
            delta > 4.0 && delta > chosen_bpm * 0.05 && !tempo_is_harmonic_relation(chosen_bpm, bpm)
        })
        .map(|(_, score)| *score)
        .fold(0.0f32, f32::max);
    let harmonic_score = profile
        .iter()
        .enumerate()
        .filter(|(idx, _)| *idx != chosen_idx)
        .filter(|(idx, _)| tempo_is_harmonic_relation(chosen_bpm, bpm_values[*idx]))
        .map(|(_, score)| *score)
        .fold(0.0f32, f32::max);
    let total_score = folded_scores.iter().sum::<f32>().max(best_score);
    let dominance = ((chosen_score - second_score) / chosen_score).clamp(0.0, 1.0);
    let harmonic_gap = ((chosen_score - harmonic_score) / chosen_score).clamp(0.0, 1.0);
    let occupancy = (best_score / total_score).clamp(0.0, 1.0);
    let confidence = (dominance * 0.55 + harmonic_gap * 0.35 + occupancy * 0.10).clamp(0.0, 1.0);
    (Some(normalize_bpm_estimate(chosen_bpm)), confidence)
}

fn estimate_key_mode_from_profile(profile: &[f32; 12]) -> (Option<String>, Option<String>, f32) {
    const NOTE_NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    const MAJOR_TEMPLATE: [f32; 12] = [
        6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
    ];
    const MINOR_TEMPLATE: [f32; 12] = [
        6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
    ];

    let energy: f32 = profile.iter().sum();
    if energy <= 0.0 {
        return (None, None, 0.0);
    }

    let major_norm = template_norm(&MAJOR_TEMPLATE);
    let minor_norm = template_norm(&MINOR_TEMPLATE);
    let profile_norm = profile
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    let mut best_score = f32::MIN;
    let mut second_score = f32::MIN;
    let mut best_tonic = 0usize;
    let mut best_mode = "Major";
    for tonic in 0..12 {
        let major_triad = profile[tonic] * 0.35
            + profile[(tonic + 4) % 12] * 0.2
            + profile[(tonic + 7) % 12] * 0.2;
        let major_score =
            cosine_similarity(profile, &MAJOR_TEMPLATE, tonic, profile_norm, major_norm)
                + major_triad;
        if major_score > best_score {
            second_score = best_score;
            best_score = major_score;
            best_tonic = tonic;
            best_mode = "Major";
        } else if major_score > second_score {
            second_score = major_score;
        }

        let minor_triad = profile[tonic] * 0.35
            + profile[(tonic + 3) % 12] * 0.2
            + profile[(tonic + 7) % 12] * 0.2;
        let minor_score =
            cosine_similarity(profile, &MINOR_TEMPLATE, tonic, profile_norm, minor_norm)
                + minor_triad;
        if minor_score > best_score {
            second_score = best_score;
            best_score = minor_score;
            best_tonic = tonic;
            best_mode = "Minor";
        } else if minor_score > second_score {
            second_score = minor_score;
        }
    }

    let confidence = if best_score.is_finite() && best_score > 0.0 {
        ((best_score - second_score.max(0.0)) / best_score).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (
        Some(NOTE_NAMES[best_tonic].to_string()),
        Some(best_mode.to_string()),
        confidence,
    )
}

fn cosine_similarity(
    profile: &[f32; 12],
    template: &[f32; 12],
    tonic: usize,
    profile_norm: f32,
    template_norm: f32,
) -> f32 {
    if profile_norm <= 0.0 || template_norm <= 0.0 {
        return 0.0;
    }
    let mut dot = 0.0f32;
    for pc in 0..12 {
        dot += profile[pc] * template[(pc + 12 - tonic) % 12];
    }
    dot / (profile_norm * template_norm)
}

fn template_norm(template: &[f32; 12]) -> f32 {
    template
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt()
}

fn nearest_bpm_index(bpm_values: &[f32], target: f32) -> Option<usize> {
    let mut best_idx = None;
    let mut best_delta = f32::INFINITY;
    for (idx, bpm) in bpm_values.iter().copied().enumerate() {
        let delta = (bpm - target).abs();
        if delta < best_delta {
            best_delta = delta;
            best_idx = Some(idx);
        }
    }
    let tolerance = target.abs().max(1.0) * 0.08;
    if best_delta <= tolerance {
        best_idx
    } else {
        None
    }
}

fn tempo_is_harmonic_relation(a: f32, b: f32) -> bool {
    let hi = a.max(b);
    let lo = a.min(b).max(1.0e-6);
    let ratio = hi / lo;
    (ratio - 2.0).abs() <= 0.12 || (ratio - 4.0).abs() <= 0.2
}

fn normalize_bpm_estimate(mut bpm: f32) -> f32 {
    while bpm < PREFERRED_MIN_TEMPO_BPM {
        bpm *= 2.0;
    }
    while bpm > PREFERRED_MAX_TEMPO_BPM {
        bpm *= 0.5;
    }
    bpm
}

fn triangular_weight(idx: usize, len: usize) -> f32 {
    if len <= 1 {
        return 1.0;
    }
    let center = (len - 1) as f32 * 0.5;
    let dist = ((idx as f32) - center).abs();
    (1.0 - dist / center.max(1.0)).max(0.1)
}

fn hann_window(n: usize) -> Vec<f32> {
    if n <= 1 {
        return vec![1.0; n];
    }
    let n_f = (n - 1) as f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / n_f;
            0.5 - 0.5 * (2.0 * std::f32::consts::PI * t).cos()
        })
        .collect()
}

fn blackman_harris_window(n: usize) -> Vec<f32> {
    if n <= 1 {
        return vec![1.0; n];
    }
    let a0 = 0.35875;
    let a1 = 0.48829;
    let a2 = 0.14128;
    let a3 = 0.01168;
    let n_f = (n - 1) as f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / n_f;
            let two_pi = 2.0 * std::f32::consts::PI * t;
            a0 - a1 * two_pi.cos() + a2 * (2.0 * two_pi).cos() - a3 * (3.0 * two_pi).cos()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click_train(sample_rate: u32, seconds: f32, bpm: f32, subdivisions: usize) -> Vec<f32> {
        let len = (sample_rate as f32 * seconds).round() as usize;
        let mut out = vec![0.0f32; len];
        let interval =
            (sample_rate as f32 * 60.0 / bpm / subdivisions.max(1) as f32).round() as usize;
        let pulse_len = (sample_rate / 200).max(8) as usize;
        let accent_period = subdivisions.max(1);
        let mut idx = 0usize;
        let mut pulse = 0usize;
        while idx < len {
            let gain = if pulse % accent_period == 0 {
                1.0
            } else {
                0.45
            };
            for s in idx..(idx + pulse_len).min(len) {
                let phase = (s - idx) as f32 / pulse_len as f32;
                out[s] += gain * (1.0 - phase);
            }
            idx = idx.saturating_add(interval.max(1));
            pulse += 1;
        }
        out
    }

    fn sine_mix(sample_rate: u32, seconds: f32, freqs: &[f32]) -> Vec<f32> {
        let len = (sample_rate as f32 * seconds).round() as usize;
        let mut out = vec![0.0f32; len];
        for &freq in freqs {
            for (idx, sample) in out.iter_mut().enumerate() {
                let t = idx as f32 / sample_rate as f32;
                *sample += (2.0 * std::f32::consts::PI * freq * t).sin();
            }
        }
        let scale = 1.0 / freqs.len().max(1) as f32;
        for sample in &mut out {
            *sample *= scale;
        }
        out
    }

    #[test]
    fn tempogram_estimates_click_train_bpm() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        for bpm in [60.0, 90.0, 120.0, 150.0] {
            let mono = click_train(sample_rate, 12.0, bpm, 1);
            let data = compute_tempogram(&mono, sample_rate, &cfg);
            let estimated = data.estimated_bpm.expect("estimated bpm");
            assert!(
                (estimated - bpm).abs() <= 5.0,
                "expected {bpm}, got {estimated}"
            );
            assert!(
                data.confidence > 0.05,
                "confidence too low: {}",
                data.confidence
            );
        }
    }

    #[test]
    fn tempogram_confidence_drops_for_half_double_ambiguous_pattern() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        let strong = click_train(sample_rate, 12.0, 120.0, 1);
        let ambiguous = click_train(sample_rate, 12.0, 120.0, 2);
        let strong_data = compute_tempogram(&strong, sample_rate, &cfg);
        let ambiguous_data = compute_tempogram(&ambiguous, sample_rate, &cfg);
        assert!(ambiguous_data.confidence < strong_data.confidence);
    }

    #[test]
    fn chromagram_detects_c_major_and_a_minor() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        let c_major = sine_mix(sample_rate, 4.0, &[261.63, 329.63, 392.0]);
        let c_major_data = compute_chromagram(&c_major, sample_rate, &cfg);
        assert_eq!(c_major_data.estimated_key.as_deref(), Some("C"));
        assert_eq!(c_major_data.estimated_mode.as_deref(), Some("Major"));

        let a_minor = sine_mix(sample_rate, 4.0, &[110.0, 220.0, 261.63, 329.63, 415.30]);
        let a_minor_data = compute_chromagram(&a_minor, sample_rate, &cfg);
        assert_eq!(a_minor_data.estimated_key.as_deref(), Some("A"));
        assert_eq!(a_minor_data.estimated_mode.as_deref(), Some("Minor"));
    }

    #[test]
    fn chromagram_ambiguous_input_has_lower_confidence() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        let chord = sine_mix(sample_rate, 4.0, &[261.63, 329.63, 392.0]);
        let single = sine_mix(sample_rate, 4.0, &[261.63]);
        let chord_data = compute_chromagram(&chord, sample_rate, &cfg);
        let single_data = compute_chromagram(&single, sample_rate, &cfg);
        assert!(single_data.confidence < chord_data.confidence);
    }
}
