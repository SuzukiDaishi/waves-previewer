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

// Parse RIFF WAVE 'smpl' chunk and extract the first loop's start/end in samples (if present).
pub fn read_wav_loop_markers(path: &Path) -> Option<(u32, u32)> {
    use std::fs;
    let data = fs::read(path).ok()?;
    if data.len() < 12 { return None; }
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" { return None; }
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos+4];
        let size = u32::from_le_bytes([data[pos+4], data[pos+5], data[pos+6], data[pos+7]]) as usize;
        let chunk_start = pos + 8;
        let chunk_end = chunk_start.saturating_add(size).min(data.len());
        if id == b"smpl" {
            // smpl header is 9 u32 (36 bytes) before loops
            if chunk_end.saturating_sub(chunk_start) < 36 { return None; }
            let num_loops_off = chunk_start + 28;
            if num_loops_off + 4 > data.len() { return None; }
            let num_loops = u32::from_le_bytes([
                data[num_loops_off], data[num_loops_off+1], data[num_loops_off+2], data[num_loops_off+3]
            ]) as usize;
            let loops_off = chunk_start + 36;
            // each loop entry: 6 u32 = 24 bytes
            if num_loops == 0 || loops_off + 24 > data.len() { return None; }
            let start_off = loops_off + 8;  // start at +8 bytes in loop struct
            let end_off = loops_off + 12;   // end at +12 bytes in loop struct
            if end_off + 4 > data.len() { return None; }
            let start = u32::from_le_bytes([
                data[start_off], data[start_off+1], data[start_off+2], data[start_off+3]
            ]);
            let end = u32::from_le_bytes([
                data[end_off], data[end_off+1], data[end_off+2], data[end_off+3]
            ]);
            if end > start { return Some((start, end)); }
            else { return None; }
        }
        // chunks are word (2-byte) aligned
        let advance = 8 + size + (size & 1);
        if pos + advance <= pos { break; }
        pos = pos.saturating_add(advance);
    }
    None
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

// Export: apply gain to source WAV and write to a new file (preserve channels & sample_rate).
pub fn export_gain_wav(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    let (mut chans, in_sr) = decode_wav_multi(src)?;
    let g = 10.0f32.powf(gain_db / 20.0);
    for c in chans.iter_mut() { for v in c.iter_mut() { *v = (*v * g).clamp(-1.0, 1.0); } }
    // Try to preserve original bit depth and format if possible
    let spec = hound::WavReader::open(src)?.spec();
    let mut writer = hound::WavWriter::create(dst, hound::WavSpec{
        channels: spec.channels,
        sample_rate: in_sr,
        bits_per_sample: spec.bits_per_sample,
        sample_format: spec.sample_format,
    })?;
    let frames = chans.get(0).map(|c| c.len()).unwrap_or(0);
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for i in 0..frames { for ch in 0..(spec.channels as usize) { let s = chans.get(ch).and_then(|c| c.get(i)).copied().unwrap_or(0.0); writer.write_sample::<f32>(s)?; } }
        }
        hound::SampleFormat::Int => {
            let max_abs = match spec.bits_per_sample { 8 => 127.0, 16 => 32767.0, 24 => 8_388_607.0, 32 => 2_147_483_647.0, b => ((1u64 << (b - 1)) - 1) as f64 as f32 };
            for i in 0..frames {
                for ch in 0..(spec.channels as usize) {
                    let s = chans.get(ch).and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                    let v = (s * max_abs).round().clamp(-(max_abs), max_abs) as i32;
                    writer.write_sample::<i32>(v)?;
                }
            }
        }
    }
    writer.finalize()?;
    Ok(())
}

// Export a selection from in-memory multi-channel samples (float32) to a WAV file.
pub fn export_selection_wav(chans: &[Vec<f32>], sample_rate: u32, range: (usize,usize), dst: &Path) -> Result<()> {
    let ch = chans.len() as u16;
    let (mut s, mut e) = range;
    if s > e { std::mem::swap(&mut s, &mut e); }
    let frames = chans.get(0).map(|c| c.len()).unwrap_or(0);
    let s = s.min(frames);
    let e = e.min(frames);
    let mut writer = hound::WavWriter::create(dst, hound::WavSpec{
        channels: ch,
        sample_rate: sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    })?;
    for i in s..e {
        for ci in 0..(ch as usize) {
            let v = chans.get(ci).and_then(|c| c.get(i)).copied().unwrap_or(0.0).clamp(-1.0, 1.0);
            writer.write_sample::<f32>(v)?;
        }
    }
    writer.finalize()?;
    Ok(())
}


// Overwrite: apply gain and replace the source file safely with optional .bak
pub fn overwrite_gain_wav(src: &Path, gain_db: f32, backup: bool) -> Result<()> {
    use std::fs;
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join("._wvp_tmp.wav");
    if tmp.exists() { let _ = fs::remove_file(&tmp); }
    export_gain_wav(src, &tmp, gain_db)?;
    if backup {
        // backup as "<original>.wav.bak"
        let fname = src.file_name().and_then(|s| s.to_str()).unwrap_or("backup.wav");
        let bak = src.with_file_name(format!("{}.bak", fname));
        let _ = fs::remove_file(&bak);
        let _ = fs::copy(src, &bak);
    }
    let _ = fs::remove_file(src);
    fs::rename(&tmp, src)?;
    Ok(())
}

// ---- Loudness (LUFS) utilities ----

// K-weighting biquad coefficients for fs=48kHz (BS.1770)
const KW_B0_1: f32 = 1.5351249;
const KW_B1_1: f32 = -2.6916962;
const KW_B2_1: f32 = 1.1983929;
const KW_A1_1: f32 = -1.6906593;
const KW_A2_1: f32 = 0.73248076;

const KW_B0_2: f32 = 1.0;
const KW_B1_2: f32 = -2.0;
const KW_B2_2: f32 = 1.0;
const KW_A1_2: f32 = -1.9900475;
const KW_A2_2: f32 = 0.99007225;

const K_CONST: f32 = -0.691; // 997Hz calibration constant

fn biquad_inplace_f32(x: &mut [f32], b0: f32, b1: f32, b2: f32, a1: f32, a2: f32) {
    let mut x1 = 0.0f32; let mut x2 = 0.0f32; let mut y1 = 0.0f32; let mut y2 = 0.0f32;
    for n in 0..x.len() {
        let xn = x[n];
        let y = b0 * xn + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        x[n] = y;
        x2 = x1; x1 = xn; y2 = y1; y1 = y;
    }
}

fn k_weighting_apply_48k(chans: &mut [Vec<f32>]) {
    for ch in chans.iter_mut() {
        biquad_inplace_f32(ch, KW_B0_1, KW_B1_1, KW_B2_1, KW_A1_1, KW_A2_1);
        biquad_inplace_f32(ch, KW_B0_2, KW_B1_2, KW_B2_2, KW_A1_2, KW_A2_2);
    }
}

fn ensure_sr_48k(chans: &[Vec<f32>], in_sr: u32) -> (Vec<Vec<f32>>, u32) {
    if in_sr == 48_000 { return (chans.to_vec(), in_sr); }
    let mut out = Vec::with_capacity(chans.len());
    for ch in chans {
        out.push(resample_linear(ch, in_sr, 48_000));
    }
    (out, 48_000)
}

fn block_means_power(power: &[f32], win: usize, hop: usize) -> Vec<f64> {
    if power.len() < win || win == 0 || hop == 0 { return Vec::new(); }
    let mut cs = Vec::with_capacity(power.len() + 1);
    cs.push(0.0f64);
    let mut sum = 0.0f64;
    for &v in power { sum += v as f64; cs.push(sum); }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + win <= power.len() {
        let s = cs[i + win] - cs[i];
        out.push(s / (win as f64));
        i += hop;
    }
    out
}

pub fn lufs_integrated_from_multi(chans_in: &[Vec<f32>], in_sr: u32) -> Result<f32> {
    if chans_in.is_empty() { anyhow::bail!("empty channels"); }
    // Resample to 48k and copy
    let (mut chans, sr) = ensure_sr_48k(chans_in, in_sr);
    let _ = sr; // sr is 48k now
    // K-weighting
    k_weighting_apply_48k(&mut chans);
    // Sum weighted power across channels (weights=1.0, LFE not identified here)
    let n = chans[0].len();
    let mut p_sum = vec![0.0f32; n];
    for ch in &chans { for i in 0..n { let v = ch[i]; p_sum[i] += v * v; } }
    // 400ms window with 100ms hop
    let win = (0.400 * 48_000.0) as usize;
    let hop = (0.100 * 48_000.0) as usize;
    let means = block_means_power(&p_sum, win, hop);
    if means.is_empty() { return Ok(f32::NEG_INFINITY); }
    let blocks_lufs: Vec<f32> = means.iter().map(|&m| K_CONST + 10.0 * (m.max(1e-24)).log10() as f32).collect();
    // Absolute gate -70 LUFS
    let mut sel: Vec<bool> = blocks_lufs.iter().map(|&l| l > -70.0).collect();
    if !sel.iter().any(|&b| b) { return Ok(f32::NEG_INFINITY); }
    // Average of means after absolute gate
    let mut num = 0usize; let mut acc = 0.0f64;
    for (i, &ok) in sel.iter().enumerate() { if ok { acc += means[i]; num += 1; } }
    let z_abs = if num>0 { acc / num as f64 } else { 0.0 };
    if z_abs <= 0.0 { return Ok(f32::NEG_INFINITY); }
    let l_abs = K_CONST + 10.0 * (z_abs.max(1e-24)).log10() as f32;
    let thr = l_abs - 10.0;
    for (i, l) in blocks_lufs.iter().enumerate() { sel[i] = sel[i] && (*l > thr); }
    if !sel.iter().any(|&b| b) { return Ok(f32::NEG_INFINITY); }
    let mut acc2 = 0.0f64; let mut n2 = 0usize;
    for (i, &ok) in sel.iter().enumerate() { if ok { acc2 += means[i]; n2 += 1; } }
    if n2 == 0 { return Ok(f32::NEG_INFINITY); }
    let z_final = acc2 / n2 as f64;
    let l = K_CONST + 10.0 * (z_final.max(1e-24)).log10() as f32;
    Ok(l)
}
