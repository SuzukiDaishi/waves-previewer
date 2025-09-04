use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::audio::AudioEngine;
use signalsmith_stretch::Stretch;

pub fn decode_wav_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open wav: {}", path.display()))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let in_sr = spec.sample_rate;
    let mut mono: Vec<f32> = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Float => {
            let mut acc: f32 = 0.0; let mut c = 0usize;
            for s in reader.samples::<f32>() { let v = s?; acc += v; c += 1; if c == ch { mono.push(acc / ch as f32); acc = 0.0; c = 0; } }
        }
        hound::SampleFormat::Int => {
            let max_abs = match spec.bits_per_sample { 8 => 127.0, 16 => 32767.0, 24 => 8_388_607.0, 32 => 2_147_483_647.0, b => ((1u64 << (b - 1)) - 1) as f64 as f32 };
            let mut acc: f32 = 0.0; let mut c = 0usize;
            for s in reader.samples::<i32>() { let v_i = s?; let v = (v_i as f32) / max_abs; acc += v; c += 1; if c == ch { mono.push(acc / ch as f32); acc = 0.0; c = 0; } }
        }
    }
    Ok((mono, in_sr))
}

pub fn decode_wav_multi(path: &Path) -> Result<(Vec<Vec<f32>>, u32)> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open wav: {}", path.display()))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let in_sr = spec.sample_rate;
    let mut chans: Vec<Vec<f32>> = vec![Vec::new(); ch];
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for (i, s) in reader.samples::<f32>().enumerate() {
                let v = s?;
                let ci = i % ch;
                chans[ci].push(v);
            }
        }
        hound::SampleFormat::Int => {
            let max_abs = match spec.bits_per_sample { 8 => 127.0, 16 => 32767.0, 24 => 8_388_607.0, 32 => 2_147_483_647.0, b => ((1u64 << (b - 1)) - 1) as f64 as f32 };
            for (i, s) in reader.samples::<i32>().enumerate() {
                let v_i = s?;
                let v = (v_i as f32) / max_abs;
                let ci = i % ch;
                chans[ci].push(v);
            }
        }
    }
    Ok((chans, in_sr))
}

pub fn resample_linear(mono: &[f32], in_sr: u32, out_sr: u32) -> Vec<f32> {
    if in_sr == out_sr || mono.is_empty() { return mono.to_vec(); }
    let ratio = out_sr as f32 / in_sr as f32;
    let out_len = (mono.len() as f32 * ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = (i as f32) / ratio;
        let i0 = src_pos.floor() as usize;
        let i1 = (i0 + 1).min(mono.len().saturating_sub(1));
        let t = (src_pos - i0 as f32).clamp(0.0, 1.0);
        let v = mono[i0] * (1.0 - t) + mono[i1] * t;
        out.push(v);
    }
    out
}

pub fn build_minmax(out: &mut Vec<(f32, f32)>, samples: &[f32], bins: usize) {
    out.clear();
    if samples.is_empty() || bins == 0 { return; }
    let len = samples.len();
    let step = (len as f32 / bins as f32).max(1.0);
    let mut pos = 0.0f32;
    for _ in 0..bins {
        let start = pos as usize;
        let end = (pos + step) as usize;
        let end = end.min(len);
        if start >= end { out.push((0.0, 0.0)); } else {
            let (mut mn, mut mx) = (f32::INFINITY, f32::NEG_INFINITY);
            for &v in &samples[start..end] { if v < mn { mn = v; } if v > mx { mx = v; } }
            if !mn.is_finite() || !mx.is_finite() { out.push((0.0, 0.0)); } else { out.push((mn, mx)); }
        }
        pos += step; if (pos as usize) >= len { break; }
    }
}

// High level helper used by UI when a file is clicked
pub fn prepare_for_playback(path: &Path, audio: &AudioEngine, out_waveform: &mut Vec<(f32, f32)>) -> Result<()> {
    let (mono, in_sr) = decode_wav_mono(path)?;
    let resampled = resample_linear(&mono, in_sr, audio.shared.out_sample_rate);
    audio.set_samples(Arc::new(resampled));
    audio.stop();
    build_minmax(out_waveform, &mono, 2048);
    Ok(())
}

// Prepare with Speed mode (rate change without pitch preservation)
pub fn prepare_for_speed(path: &Path, audio: &AudioEngine, out_waveform: &mut Vec<(f32, f32)>, _rate: f32) -> Result<()> {
    // playback rate is applied in the audio engine; here we just set the base buffer
    prepare_for_playback(path, audio, out_waveform)
}

// Prepare with PitchShift mode (preserve duration, shift pitch in semitones)
pub fn prepare_for_pitchshift(path: &Path, audio: &AudioEngine, out_waveform: &mut Vec<(f32, f32)>, semitones: f32) -> Result<()> {
    let (mono, in_sr) = decode_wav_mono(path)?;
    let out_sr = audio.shared.out_sample_rate;
    let mut out = process_pitchshift_offline(&mono, in_sr, out_sr, semitones);
    // waveform reflects processed output
    build_minmax(out_waveform, &out, 2048);
    audio.set_samples(Arc::new(std::mem::take(&mut out)));
    audio.stop();
    Ok(())
}

// Prepare with TimeStretch mode (preserve pitch, change duration by rate: 0.5 -> slower/longer)
pub fn prepare_for_timestretch(path: &Path, audio: &AudioEngine, out_waveform: &mut Vec<(f32, f32)>, rate: f32) -> Result<()> {
    let rate = rate.clamp(0.25, 4.0);
    let (mono, in_sr) = decode_wav_mono(path)?;
    let out_sr = audio.shared.out_sample_rate;
    let mut out = process_timestretch_offline(&mono, in_sr, out_sr, rate);
    build_minmax(out_waveform, &out, 2048);
    audio.set_samples(Arc::new(std::mem::take(&mut out)));
    audio.stop();
    Ok(())
}

// Heavy offline: pitch-shift preserving duration
pub fn process_pitchshift_offline(mono: &[f32], in_sr: u32, out_sr: u32, semitones: f32) -> Vec<f32> {
    let resampled = resample_linear(mono, in_sr, out_sr);
    let mut stretch = Stretch::preset_default(1, out_sr);
    stretch.set_transpose_factor_semitones(semitones, None);
    let mut out = vec![0.0_f32; resampled.len()];
    stretch.process(&resampled, &mut out);
    // Append remaining tail to avoid end being cut
    let olat = stretch.output_latency();
    if olat > 0 {
        let mut tail = vec![0.0_f32; olat];
        stretch.flush(&mut tail);
        out.extend_from_slice(&tail);
    }
    out
}

// Heavy offline: time-stretch preserving pitch
pub fn process_timestretch_offline(mono: &[f32], in_sr: u32, out_sr: u32, rate: f32) -> Vec<f32> {
    let rate = rate.clamp(0.25, 4.0);
    let resampled = resample_linear(mono, in_sr, out_sr);
    let mut stretch = Stretch::preset_default(1, out_sr);
    stretch.set_transpose_factor(1.0, None);
    let out_len = ((resampled.len() as f64) / (rate as f64)).ceil() as usize;
    let mut out = vec![0.0_f32; out_len];
    stretch.process(&resampled, &mut out);
    // Append remaining tail to avoid end being cut
    let olat = stretch.output_latency();
    if olat > 0 {
        let mut tail = vec![0.0_f32; olat];
        stretch.flush(&mut tail);
        out.extend_from_slice(&tail);
    }
    out
}
