use rustfft::{num_complex::Complex, FftPlanner};

use crate::app::types::{ChromagramData, SpectrogramConfig, TempogramData, WindowFunction};

const MIN_CHROMA_FREQ_HZ: f32 = 55.0;
const MAX_CHROMA_FREQ_HZ: f32 = 5_000.0;
const MIN_TEMPO_BPM: f32 = 30.0;
const MAX_TEMPO_BPM: f32 = 300.0;
const PREFERRED_MIN_TEMPO_BPM: f32 = 60.0;
const PREFERRED_MAX_TEMPO_BPM: f32 = 180.0;
const TEMPO_WINDOW_SECS: f32 = 8.0;
const CHROMA_BINS_PER_OCTAVE: usize = 36;
const CHROMA_CENS_SMOOTH_WIN: usize = 41;
const CHROMA_FMIN_HZ: f32 = 32.703_197;
const CHROMA_SUBBIN_CENTER_OFFSET: f32 = 1.0;
const CHROMA_QUANT_STEPS: [f32; 4] = [0.4, 0.2, 0.1, 0.05];
const CHROMA_QUANT_WEIGHTS: [f32; 4] = [0.25, 0.25, 0.25, 0.25];

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
    let window_frames = ((TEMPO_WINDOW_SECS / hop_sec).round() as usize)
        .clamp(32, 384)
        .min(stft.frames.max(32));
    let values = compute_local_tempogram_acf(&onset, &lags, window_frames);

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
    let chroma_bins_per_octave = CHROMA_BINS_PER_OCTAVE;
    let chroma_input_bins = (((max_freq / CHROMA_FMIN_HZ).log2().ceil().max(1.0) as usize)
        * chroma_bins_per_octave)
        .max(chroma_bins_per_octave);
    let chroma_basis = build_cq_to_chroma_basis(chroma_input_bins, chroma_bins_per_octave, 12);
    let mut raw_values = vec![0.0f32; stft.frames * 12];
    for frame in 0..stft.frames {
        let src = &stft.values[frame * stft.bins..(frame + 1) * stft.bins];
        let row = &mut raw_values[frame * 12..(frame + 1) * 12];
        let subbin_energy = compute_chroma_subbin_energy(
            src,
            sr,
            stft.fft_size.max(1),
            max_freq,
            chroma_input_bins,
        );
        for chroma in 0..12 {
            let base = chroma * chroma_input_bins;
            let mut acc = 0.0f32;
            for idx in 0..chroma_input_bins {
                acc += chroma_basis[base + idx] * subbin_energy[idx];
            }
            row[chroma] = acc;
        }
        let sum: f32 = row.iter().sum();
        if sum > 0.0 {
            for value in row.iter_mut() {
                *value /= sum;
            }
        }
    }
    let values = apply_cens_smoothing(&raw_values, stft.frames, 12, CHROMA_CENS_SMOOTH_WIN);

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
            + profile[(tonic + 4) % 12] * 0.22
            + profile[(tonic + 7) % 12] * 0.2
            - profile[(tonic + 3) % 12] * 0.12;
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
            + profile[(tonic + 3) % 12] * 0.22
            + profile[(tonic + 7) % 12] * 0.2
            - profile[(tonic + 4) % 12] * 0.12;
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

fn compute_chroma_subbin_energy(
    power_row: &[f32],
    sample_rate: f32,
    fft_size: usize,
    max_freq: f32,
    n_input: usize,
) -> Vec<f32> {
    let mut energy = vec![0.0f32; n_input.max(1)];
    if power_row.is_empty() || sample_rate <= 0.0 || fft_size == 0 || max_freq <= MIN_CHROMA_FREQ_HZ
    {
        return energy;
    }
    let bins_per_octave = CHROMA_BINS_PER_OCTAVE as f32;
    let half_step_ratio = 2.0_f32.powf(0.5 / bins_per_octave);
    for (input_idx, slot) in energy.iter_mut().enumerate() {
        let subbin = input_idx as f32 - CHROMA_SUBBIN_CENTER_OFFSET;
        let center_freq = CHROMA_FMIN_HZ * 2.0_f32.powf(subbin / bins_per_octave);
        if !center_freq.is_finite() || center_freq < MIN_CHROMA_FREQ_HZ || center_freq > max_freq {
            continue;
        }
        let lo = sample_stft_magnitude_at_freq(
            power_row,
            center_freq / half_step_ratio,
            sample_rate,
            fft_size,
        );
        let mid = sample_stft_magnitude_at_freq(power_row, center_freq, sample_rate, fft_size);
        let hi = sample_stft_magnitude_at_freq(
            power_row,
            center_freq * half_step_ratio,
            sample_rate,
            fft_size,
        );
        *slot = (0.25 * lo + 0.5 * mid + 0.25 * hi).ln_1p();
    }
    let peak_floor = energy
        .iter()
        .copied()
        .fold(0.0f32, f32::max)
        .mul_add(0.15, 0.0)
        .max(1.0e-6);
    let mut filtered = vec![0.0f32; energy.len()];
    for idx in 1..energy.len().saturating_sub(1) {
        let value = energy[idx];
        if value < peak_floor || value < energy[idx - 1] || value < energy[idx + 1] {
            continue;
        }
        filtered[idx] = (0.25 * energy[idx - 1] + value + 0.25 * energy[idx + 1]).max(0.0);
    }
    if filtered.iter().any(|&value| value > 0.0) {
        filtered
    } else {
        energy
    }
}

fn build_cq_to_chroma_basis(n_input: usize, bins_per_octave: usize, n_chroma: usize) -> Vec<f32> {
    let mut basis = vec![0.0f32; n_input.saturating_mul(n_chroma)];
    if n_input == 0 || n_chroma == 0 || bins_per_octave == 0 {
        return basis;
    }
    let merge = (bins_per_octave / n_chroma).max(1);
    for input_idx in 0..n_input {
        let chroma = ((input_idx % bins_per_octave) / merge).min(n_chroma - 1);
        basis[chroma * n_input + input_idx] = 1.0;
    }
    basis
}

fn quantize_chroma_levels(raw_values: &[f32], frames: usize, bins: usize) -> Vec<f32> {
    let mut quantized = vec![0.0f32; raw_values.len()];
    for frame in 0..frames {
        let src = &raw_values[frame * bins..(frame + 1) * bins];
        let dst = &mut quantized[frame * bins..(frame + 1) * bins];
        for (bin, value) in src.iter().copied().enumerate() {
            let mut level = 0.0f32;
            for (threshold, weight) in CHROMA_QUANT_STEPS
                .iter()
                .copied()
                .zip(CHROMA_QUANT_WEIGHTS.iter().copied())
            {
                if value > threshold {
                    level += weight;
                }
            }
            dst[bin] = level;
        }
    }
    quantized
}

fn apply_cens_smoothing(
    raw_values: &[f32],
    frames: usize,
    bins: usize,
    win_len: usize,
) -> Vec<f32> {
    if frames == 0 || bins == 0 || raw_values.is_empty() {
        return Vec::new();
    }
    let quantized = quantize_chroma_levels(raw_values, frames, bins);
    let win_len = win_len.max(1).min(frames.max(1));
    let window = if win_len <= 1 {
        vec![1.0]
    } else {
        let mut w = hann_window(win_len + 2);
        if w.len() > 2 {
            w = w[1..w.len() - 1].to_vec();
        }
        let sum = w.iter().sum::<f32>().max(1.0e-9);
        for value in &mut w {
            *value /= sum;
        }
        w
    };
    let half = window.len() / 2;
    let mut cens = vec![0.0f32; quantized.len()];
    for frame in 0..frames {
        for tap_idx in 0..window.len() {
            let source_frame = frame as isize + tap_idx as isize - half as isize;
            if source_frame < 0 || source_frame as usize >= frames {
                continue;
            }
            let src = &quantized[source_frame as usize * bins..(source_frame as usize + 1) * bins];
            let dst = &mut cens[frame * bins..(frame + 1) * bins];
            let weight = window[tap_idx];
            for bin in 0..bins {
                dst[bin] += src[bin] * weight;
            }
        }
        let row = &mut cens[frame * bins..(frame + 1) * bins];
        let norm = row.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in row.iter_mut() {
                *value /= norm;
            }
        }
    }
    cens
}

fn compute_local_tempogram_acf(onset: &[f32], lags: &[usize], win_length: usize) -> Vec<f32> {
    let frames = onset.len();
    let mut values = vec![0.0f32; frames.saturating_mul(lags.len())];
    if frames == 0 || lags.is_empty() || win_length == 0 {
        return values;
    }
    let padded = pad_linear_ramp_to_zero(onset, win_length / 2);
    let window = hann_window(win_length);
    for frame in 0..frames {
        let slice = &padded[frame..frame + win_length];
        let row = &mut values[frame * lags.len()..(frame + 1) * lags.len()];
        for (lag_idx, &lag) in lags.iter().enumerate() {
            if lag >= win_length {
                continue;
            }
            let limit = win_length - lag;
            let mut acc = 0.0f32;
            for i in 0..limit {
                acc += slice[i] * window[i] * slice[i + lag] * window[i + lag];
            }
            row[lag_idx] = acc.max(0.0);
        }
        let row_max = row.iter().copied().fold(0.0f32, f32::max).max(1.0e-9);
        for value in row.iter_mut() {
            *value = (*value / row_max).clamp(0.0, 1.0);
        }
    }
    values
}

fn sample_stft_magnitude_at_freq(
    power_row: &[f32],
    freq: f32,
    sample_rate: f32,
    fft_size: usize,
) -> f32 {
    if power_row.is_empty()
        || fft_size == 0
        || sample_rate <= 0.0
        || !freq.is_finite()
        || freq <= 0.0
    {
        return 0.0;
    }
    let bin_pos = freq * fft_size as f32 / sample_rate;
    if !bin_pos.is_finite() {
        return 0.0;
    }
    let bin0 = bin_pos.floor().max(0.0) as usize;
    let bin1 = (bin0 + 1).min(power_row.len().saturating_sub(1));
    let frac = (bin_pos - bin0 as f32).clamp(0.0, 1.0);
    let mag0 = power_row.get(bin0).copied().unwrap_or(0.0).max(0.0).sqrt();
    let mag1 = power_row.get(bin1).copied().unwrap_or(0.0).max(0.0).sqrt();
    mag0 + (mag1 - mag0) * frac
}

fn pad_linear_ramp_to_zero(values: &[f32], pad: usize) -> Vec<f32> {
    if pad == 0 {
        return values.to_vec();
    }
    let mut out = Vec::with_capacity(values.len() + pad * 2);
    let first = values.first().copied().unwrap_or(0.0);
    for idx in 0..pad {
        let t = (idx + 1) as f32 / (pad + 1) as f32;
        out.push(first * t);
    }
    out.extend_from_slice(values);
    let last = values.last().copied().unwrap_or(0.0);
    for idx in 0..pad {
        let t = 1.0 - (idx + 1) as f32 / (pad + 1) as f32;
        out.push(last * t);
    }
    out
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

    fn average_chroma_profile(data: &ChromagramData) -> [f32; 12] {
        let mut profile = [0.0f32; 12];
        if data.frames == 0 || data.bins != 12 {
            return profile;
        }
        for frame in 0..data.frames {
            let row = &data.values[frame * 12..(frame + 1) * 12];
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
        profile
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

        let a_minor = sine_mix(sample_rate, 4.0, &[110.0, 220.0, 261.63, 329.63, 392.0]);
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

    #[test]
    fn chromagram_octaves_share_same_pitch_class() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        for freq in [110.0, 220.0, 440.0] {
            let tone = sine_mix(sample_rate, 3.0, &[freq]);
            let data = compute_chromagram(&tone, sample_rate, &cfg);
            let profile = average_chroma_profile(&data);
            let (best_idx, _) = profile
                .iter()
                .copied()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .unwrap();
            assert_eq!(best_idx, 9, "expected A pitch class for freq {freq}");
        }
    }

    #[test]
    fn chromagram_single_note_maps_to_expected_pitch_class() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        let cases = [(261.63, 0usize), (311.13, 3usize), (440.0, 9usize)];
        for (freq, expected_idx) in cases {
            let tone = sine_mix(sample_rate, 3.0, &[freq]);
            let data = compute_chromagram(&tone, sample_rate, &cfg);
            let profile = average_chroma_profile(&data);
            let (best_idx, _) = profile
                .iter()
                .copied()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .unwrap();
            assert_eq!(
                best_idx, expected_idx,
                "freq {freq} should map to {expected_idx}"
            );
        }
    }

    #[test]
    fn chromagram_transposition_rotates_profile() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        let c_major = sine_mix(sample_rate, 4.0, &[261.63, 329.63, 392.0]);
        let d_major = sine_mix(sample_rate, 4.0, &[293.66, 369.99, 440.0]);
        let c_profile = average_chroma_profile(&compute_chromagram(&c_major, sample_rate, &cfg));
        let d_profile = average_chroma_profile(&compute_chromagram(&d_major, sample_rate, &cfg));
        for idx in 0..12 {
            assert!(
                (c_profile[idx] - d_profile[(idx + 2) % 12]).abs() < 0.12,
                "profile should rotate by +2 semitones at idx={idx}: c={} d={}",
                c_profile[idx],
                d_profile[(idx + 2) % 12]
            );
        }
    }

    #[test]
    fn chromagram_c_major_does_not_bias_fsharp() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        let c_major = sine_mix(sample_rate, 4.0, &[261.63, 329.63, 392.0]);
        let profile = average_chroma_profile(&compute_chromagram(&c_major, sample_rate, &cfg));
        let triad_peak = profile[0].max(profile[4]).max(profile[7]);
        assert!(
            profile[6] < triad_peak * 0.45,
            "F# should stay well below C/E/G triad energy: {:?}",
            profile
        );
    }

    #[test]
    fn chromagram_dsharp_bias_regression() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        let cases = [
            sine_mix(sample_rate, 4.0, &[261.63, 329.63, 392.0]),
            sine_mix(sample_rate, 4.0, &[196.0, 246.94, 293.66]),
            sine_mix(sample_rate, 4.0, &[220.0, 261.63, 329.63]),
        ];
        for (case_idx, mono) in cases.iter().enumerate() {
            let profile = average_chroma_profile(&compute_chromagram(mono, sample_rate, &cfg));
            let (best_idx, _) = profile
                .iter()
                .copied()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .unwrap();
            assert_ne!(
                best_idx, 3,
                "case {case_idx} should not bias D# profile: {:?}",
                profile
            );
        }
    }

    #[test]
    fn chromagram_key_detection_uses_cens_profile() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        let c_major = sine_mix(sample_rate, 2.0, &[261.63, 329.63, 392.0]);
        let g_major = sine_mix(sample_rate, 2.0, &[196.0, 246.94, 293.66]);
        let mut progression = c_major.clone();
        progression.extend_from_slice(&g_major);
        let data = compute_chromagram(&progression, sample_rate, &cfg);
        assert_eq!(data.estimated_key.as_deref(), Some("C"));
        assert_eq!(data.estimated_mode.as_deref(), Some("Major"));
    }

    #[test]
    fn tempogram_axis_values_remain_monotonic_after_refactor() {
        let sample_rate = 44_100;
        let cfg = SpectrogramConfig::default();
        let mono = click_train(sample_rate, 12.0, 120.0, 1);
        let data = compute_tempogram(&mono, sample_rate, &cfg);
        assert!(data.bpm_values.len() > 8);
        for pair in data.bpm_values.windows(2) {
            assert!(
                pair[0] <= pair[1],
                "bpm axis should stay monotonic ascending: {:?}",
                pair
            );
        }
    }
}
