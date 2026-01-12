use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use fdk_aac::enc::{
    AudioObjectType as FdkAudioObjectType, BitRate as AacBitRate, ChannelMode as AacChannelMode,
    Encoder as AacEncoder, EncoderParams as AacEncoderParams, Transport as AacTransport,
};
use mp3lame_encoder::{
    max_required_buffer_size, Bitrate as Mp3Bitrate, Builder as Mp3Builder, DualPcm, FlushNoGap,
    MonoPcm, Quality as Mp3Quality,
};
use mp4::{
    AacConfig, AudioObjectType as Mp4AudioObjectType, ChannelConfig, MediaConfig, Mp4Config,
    Mp4Sample, Mp4Writer, SampleFreqIndex, TrackConfig, TrackType,
};

use crate::audio::AudioEngine;
use crate::audio_io;
use signalsmith_stretch::Stretch;

pub fn decode_wav_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    audio_io::decode_audio_mono(path)
}

pub fn decode_wav_mono_prefix(path: &Path, max_secs: f32) -> Result<(Vec<f32>, u32, bool)> {
    audio_io::decode_audio_mono_prefix(path, max_secs)
}

pub fn decode_wav_multi(path: &Path) -> Result<(Vec<Vec<f32>>, u32)> {
    audio_io::decode_audio_multi(path)
}

pub fn resample_linear(mono: &[f32], in_sr: u32, out_sr: u32) -> Vec<f32> {
    if in_sr == out_sr || mono.is_empty() {
        return mono.to_vec();
    }
    if in_sr == 0 || out_sr == 0 {
        return mono.to_vec();
    }
    let ratio = out_sr as f64 / in_sr as f64;
    let out_len = ((mono.len() as f64) * ratio).ceil() as usize;
    if out_len == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(out_len);
    let len = mono.len();
    for i in 0..out_len {
        let src_pos = (i as f64) / ratio;
        let i0 = src_pos.floor() as usize;
        if i0 >= len {
            out.push(mono[len - 1]);
            continue;
        }
        let i1 = (i0 + 1).min(len.saturating_sub(1));
        let t = (src_pos - i0 as f64).clamp(0.0, 1.0) as f32;
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

/// Map loop markers (ls, le) from source sample rate `in_sr` to output `out_sr`,
/// and clamp to [0, samples_len]. Returns normalized (start<=end) if valid and non-empty.
pub fn map_loop_markers_between_sr(ls: u32, le: u32, in_sr: u32, out_sr: u32, samples_len: usize) -> Option<(usize, usize)> {
    if in_sr == 0 || out_sr == 0 || samples_len == 0 { return None; }
    let in_sr_u = in_sr as u64;
    let out_sr_u = out_sr as u64;
    let s = ((ls as u64) * out_sr_u + (in_sr_u / 2)) / in_sr_u;
    let e = ((le as u64) * out_sr_u + (in_sr_u / 2)) / in_sr_u;
    let mut s = s as usize;
    let mut e = e as usize;
    if e < s { std::mem::swap(&mut s, &mut e); }
    s = s.min(samples_len);
    e = e.min(samples_len);
    if e > s { Some((s, e)) } else { None }
}

/// Map loop markers from output SR (device) to file SR.
pub fn map_loop_markers_to_file_sr(s: usize, e: usize, out_sr: u32, file_sr: u32) -> Option<(u32, u32)> {
    if out_sr == 0 || file_sr == 0 { return None; }
    if e <= s { return None; }
    let out_sr_u = out_sr as u64;
    let file_sr_u = file_sr as u64;
    let s = ((s as u64) * file_sr_u + (out_sr_u / 2)) / out_sr_u;
    let e = ((e as u64) * file_sr_u + (out_sr_u / 2)) / out_sr_u;
    if e <= s { return None; }
    if s > u32::MAX as u64 || e > u32::MAX as u64 { return None; }
    Some((s as u32, e as u32))
}

/// Write or remove WAV 'smpl' loop markers (overwrites file safely).
pub fn write_wav_loop_markers(path: &Path, loop_opt: Option<(u32, u32)>) -> Result<()> {
    use std::fs;
    let data = fs::read(path).with_context(|| format!("open wav: {}", path.display()))?;
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        anyhow::bail!("not a RIFF/WAVE file");
    }
    let mut out: Vec<u8> = Vec::with_capacity(data.len() + 128);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&[0, 0, 0, 0]); // placeholder size
    out.extend_from_slice(b"WAVE");
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
        let chunk_start = pos + 8;
        let chunk_end = chunk_start.saturating_add(size).min(data.len());
        if id != b"smpl" {
            out.extend_from_slice(id);
            out.extend_from_slice(&(size as u32).to_le_bytes());
            out.extend_from_slice(&data[chunk_start..chunk_end]);
            if size & 1 == 1 { out.push(0); }
        }
        let advance = 8 + size + (size & 1);
        if pos + advance <= pos { break; }
        pos = pos.saturating_add(advance);
    }
    if let Some((ls, le)) = loop_opt {
        if le > ls {
            let mut chunk: Vec<u8> = Vec::with_capacity(60);
            // 9 u32 header fields
            chunk.extend_from_slice(&0u32.to_le_bytes()); // manufacturer
            chunk.extend_from_slice(&0u32.to_le_bytes()); // product
            chunk.extend_from_slice(&0u32.to_le_bytes()); // sample_period
            chunk.extend_from_slice(&60u32.to_le_bytes()); // midi_unity_note (C4)
            chunk.extend_from_slice(&0u32.to_le_bytes()); // midi_pitch_fraction
            chunk.extend_from_slice(&0u32.to_le_bytes()); // smpte_format
            chunk.extend_from_slice(&0u32.to_le_bytes()); // smpte_offset
            chunk.extend_from_slice(&1u32.to_le_bytes()); // num_sample_loops
            chunk.extend_from_slice(&0u32.to_le_bytes()); // sampler_data
            // loop struct (6 u32)
            chunk.extend_from_slice(&0u32.to_le_bytes()); // cue_point_id
            chunk.extend_from_slice(&0u32.to_le_bytes()); // type (0=forward)
            chunk.extend_from_slice(&ls.to_le_bytes()); // start
            chunk.extend_from_slice(&le.to_le_bytes()); // end
            chunk.extend_from_slice(&0u32.to_le_bytes()); // fraction
            chunk.extend_from_slice(&0u32.to_le_bytes()); // play_count
            out.extend_from_slice(b"smpl");
            out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            out.extend_from_slice(&chunk);
            if chunk.len() & 1 == 1 { out.push(0); }
        }
    }
    let riff_size = (out.len().saturating_sub(8)) as u32;
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join("._wvp_tmp_smpl.wav");
    if tmp.exists() { let _ = fs::remove_file(&tmp); }
    fs::write(&tmp, out)?;
    fs::rename(&tmp, path)?;
    Ok(())
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

pub fn prepare_for_list_preview(path: &Path, audio: &AudioEngine, max_secs: f32) -> Result<bool> {
    let (mono, in_sr, truncated) = decode_wav_mono_prefix(path, max_secs)?;
    let resampled = resample_linear(&mono, in_sr, audio.shared.out_sample_rate);
    audio.set_samples(Arc::new(resampled));
    audio.stop();
    Ok(truncated)
}

// Prepare with Speed mode (rate change without pitch preservation)
pub fn prepare_for_speed(path: &Path, audio: &AudioEngine, out_waveform: &mut Vec<(f32, f32)>, _rate: f32) -> Result<()> {
    // playback rate is applied in the audio engine; here we just set the base buffer
    prepare_for_playback(path, audio, out_waveform)
}

// Prepare with PitchShift mode (preserve duration, shift pitch in semitones)
#[allow(dead_code)]
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
#[allow(dead_code)]
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
    if resampled.is_empty() {
        return Vec::new();
    }
    let out_len = resampled.len().max(1);
    let mut stretch = Stretch::preset_default(1, out_sr);
    stretch.set_transpose_factor_semitones(semitones, None);
    let mut out = vec![0.0_f32; out_len];
    // Prefer `exact()` to handle latency alignment; fallback only for very short buffers.
    if stretch.exact(&resampled, &mut out) {
        return out;
    }
    let mut stretch = Stretch::preset_default(1, out_sr);
    stretch.set_transpose_factor_semitones(semitones, None);
    stretch_seek_preroll(&mut stretch, &resampled, 1.0);
    stretch.process(&resampled, &mut out);
    let olat = stretch.output_latency();
    if olat > 0 {
        let mut tail = vec![0.0_f32; olat];
        stretch.flush(&mut tail);
        out.extend_from_slice(&tail);
        if out.len() > olat {
            out.drain(0..olat);
        } else {
            out.clear();
        }
    }
    out
}

// Heavy offline: time-stretch preserving pitch
pub fn process_timestretch_offline(mono: &[f32], in_sr: u32, out_sr: u32, rate: f32) -> Vec<f32> {
    let rate = rate.clamp(0.25, 4.0);
    let resampled = resample_linear(mono, in_sr, out_sr);
    if resampled.is_empty() {
        return Vec::new();
    }
    let out_len = ((resampled.len() as f64) / (rate as f64)).ceil().max(1.0) as usize;
    let mut stretch = Stretch::preset_default(1, out_sr);
    stretch.set_transpose_factor(1.0, None);
    let mut out = vec![0.0_f32; out_len];
    // Prefer `exact()` to handle latency alignment; fallback only for very short buffers.
    if stretch.exact(&resampled, &mut out) {
        return out;
    }
    let mut stretch = Stretch::preset_default(1, out_sr);
    stretch.set_transpose_factor(1.0, None);
    stretch_seek_preroll(&mut stretch, &resampled, rate);
    stretch.process(&resampled, &mut out);
    let olat = stretch.output_latency();
    if olat > 0 {
        let mut tail = vec![0.0_f32; olat];
        stretch.flush(&mut tail);
        out.extend_from_slice(&tail);
        if out.len() > olat {
            out.drain(0..olat);
        } else {
            out.clear();
        }
    }
    out
}

fn stretch_seek_preroll(stretch: &mut Stretch, input: &[f32], playback_rate: f32) {
    let in_lat = stretch.input_latency();
    if in_lat == 0 {
        return;
    }
    // Feed pre-roll so the processed output starts aligned (prevents leading silence drift).
    let take = in_lat.min(input.len());
    let mut pre = Vec::with_capacity(in_lat);
    pre.extend_from_slice(&input[..take]);
    if take < in_lat {
        pre.resize(in_lat, 0.0);
    }
    stretch.seek(&pre, playback_rate as f64);
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

pub fn export_gain_audio(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    let fmt = pick_format(src, dst)
        .ok_or_else(|| anyhow::anyhow!("unsupported format: {}", src.display()))?;
    match fmt.as_str() {
        "wav" => export_gain_wav(src, dst, gain_db),
        "mp3" => export_gain_mp3(src, dst, gain_db),
        "m4a" => export_gain_m4a(src, dst, gain_db),
        _ => anyhow::bail!("unsupported format: {}", fmt),
    }
}

fn export_gain_mp3(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    use std::fs;
    let (mut chans, in_sr) = decode_wav_multi(src)?;
    apply_gain_in_place(&mut chans, gain_db);
    let data = encode_mp3(&chans, in_sr)?;
    fs::write(dst, data)?;
    Ok(())
}

fn export_gain_m4a(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    let (mut chans, in_sr) = decode_wav_multi(src)?;
    apply_gain_in_place(&mut chans, gain_db);
    encode_aac_to_mp4(dst, &chans, in_sr)
}

fn pick_format(src: &Path, dst: &Path) -> Option<String> {
    if let Some(ext) = ext_lower(dst) {
        if audio_io::is_supported_extension(&ext) {
            return Some(ext);
        }
    }
    ext_lower(src).filter(|ext| audio_io::is_supported_extension(ext))
}

fn ext_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
}

fn normalize_channels_for_encode(chans: &[Vec<f32>]) -> Vec<Vec<f32>> {
    match chans.len() {
        0 => Vec::new(),
        1 => vec![chans[0].clone()],
        _ => vec![chans[0].clone(), chans[1].clone()],
    }
}

fn apply_gain_in_place(chans: &mut [Vec<f32>], gain_db: f32) {
    let g = 10.0f32.powf(gain_db / 20.0);
    for ch in chans.iter_mut() {
        for v in ch.iter_mut() {
            *v = (*v * g).clamp(-1.0, 1.0);
        }
    }
}

fn resample_channels(chans: &[Vec<f32>], in_sr: u32, out_sr: u32) -> Vec<Vec<f32>> {
    if in_sr == out_sr {
        return chans.to_vec();
    }
    chans
        .iter()
        .map(|c| resample_linear(c, in_sr, out_sr))
        .collect()
}

fn encode_mp3(chans: &[Vec<f32>], in_sr: u32) -> Result<Vec<u8>> {
    if chans.is_empty() {
        anyhow::bail!("empty channels");
    }
    let mut chans = normalize_channels_for_encode(chans);
    let mut sr = in_sr;
    let mut builder = Mp3Builder::new().context("init mp3 encoder")?;
    builder
        .set_num_channels(chans.len() as u8)
        .map_err(|e| anyhow::anyhow!("mp3 channels: {e:?}"))?;
    if let Err(err) = builder.set_sample_rate(sr) {
        if matches!(err, mp3lame_encoder::BuildError::BadSampleFreq) {
            let target = 44_100;
            chans = resample_channels(&chans, in_sr, target);
            sr = target;
            builder
                .set_sample_rate(sr)
                .map_err(|e| anyhow::anyhow!("mp3 sample rate: {e:?}"))?;
        } else {
            return Err(anyhow::anyhow!("mp3 sample rate: {err:?}"));
        }
    }
    builder
        .set_brate(Mp3Bitrate::Kbps192)
        .map_err(|e| anyhow::anyhow!("mp3 bitrate: {e:?}"))?;
    builder
        .set_quality(Mp3Quality::Best)
        .map_err(|e| anyhow::anyhow!("mp3 quality: {e:?}"))?;
    let mut encoder = builder
        .build()
        .map_err(|e| anyhow::anyhow!("mp3 build: {e:?}"))?;
    let frames = chans[0].len().max(1);
    let mut out = Vec::new();
    out.reserve(max_required_buffer_size(frames));
    if chans.len() == 1 {
        let input = MonoPcm(&chans[0]);
        encoder
            .encode_to_vec(input, &mut out)
            .map_err(|e| anyhow::anyhow!("mp3 encode: {e:?}"))?;
    } else {
        let frames = chans[0].len().min(chans[1].len());
        let input = DualPcm {
            left: &chans[0][..frames],
            right: &chans[1][..frames],
        };
        encoder
            .encode_to_vec(input, &mut out)
            .map_err(|e| anyhow::anyhow!("mp3 encode: {e:?}"))?;
    }
    encoder
        .flush_to_vec::<FlushNoGap>(&mut out)
        .map_err(|e| anyhow::anyhow!("mp3 flush: {e:?}"))?;
    Ok(out)
}

fn encode_aac_to_mp4(dst: &Path, chans: &[Vec<f32>], in_sr: u32) -> Result<()> {
    use std::fs::File;
    if chans.is_empty() {
        anyhow::bail!("empty channels");
    }
    let mut chans = normalize_channels_for_encode(chans);
    let mut sr = in_sr;
    let mut freq_index = aac_freq_index(sr);
    if freq_index.is_none() {
        let target = 48_000;
        chans = resample_channels(&chans, in_sr, target);
        sr = target;
        freq_index = aac_freq_index(sr);
    }
    let freq_index = freq_index.context("unsupported AAC sample rate")?;
    let channels = chans.len();
    let bitrate = if channels == 1 { 96_000 } else { 192_000 };
    let params = AacEncoderParams {
        bit_rate: AacBitRate::Cbr(bitrate),
        sample_rate: sr,
        transport: AacTransport::Raw,
        channels: if channels == 1 {
            AacChannelMode::Mono
        } else {
            AacChannelMode::Stereo
        },
        audio_object_type: FdkAudioObjectType::Mpeg4LowComplexity,
    };
    let encoder = AacEncoder::new(params)
        .map_err(|e| anyhow::anyhow!("aac encoder init: {e}"))?;
    let info = encoder
        .info()
        .map_err(|e| anyhow::anyhow!("aac encoder info: {e}"))?;
    let frame_len = info.frameLength as usize;
    if frame_len == 0 {
        anyhow::bail!("aac frame length is zero");
    }
    let max_out = (info.maxOutBufBytes as usize).max(4096);
    let interleaved = interleave_i16(&chans);
    let frame_samples = frame_len * channels;
    let file = File::create(dst)
        .with_context(|| format!("create m4a: {}", dst.display()))?;
    let config = Mp4Config {
        major_brand: "M4A ".parse().expect("FourCC"),
        minor_version: 512,
        compatible_brands: vec![
            "M4A ".parse().expect("FourCC"),
            "isom".parse().expect("FourCC"),
            "iso2".parse().expect("FourCC"),
            "mp41".parse().expect("FourCC"),
        ],
        timescale: sr.max(1),
    };
    let mut writer = Mp4Writer::write_start(file, &config)
        .map_err(|e| anyhow::anyhow!("mp4 start: {e:?}"))?;
    let track_conf = TrackConfig {
        track_type: TrackType::Audio,
        timescale: sr.max(1),
        language: "und".to_string(),
        media_conf: MediaConfig::AacConfig(AacConfig {
            bitrate,
            profile: Mp4AudioObjectType::AacLowComplexity,
            freq_index,
            chan_conf: if channels == 1 {
                ChannelConfig::Mono
            } else {
                ChannelConfig::Stereo
            },
        }),
    };
    writer
        .add_track(&track_conf)
        .map_err(|e| anyhow::anyhow!("mp4 add track: {e:?}"))?;
    let track_id = 1u32;
    let mut frame_index = 0u64;
    let mut pos = 0usize;
    while pos < interleaved.len() {
        let end = (pos + frame_samples).min(interleaved.len());
        let mut input_slice = &interleaved[pos..end];
        let mut padded;
        if input_slice.len() < frame_samples {
            padded = vec![0i16; frame_samples];
            padded[..input_slice.len()].copy_from_slice(input_slice);
            input_slice = &padded;
        }
        let mut out_buf = vec![0u8; max_out];
        let enc_info = encoder
            .encode(input_slice, &mut out_buf)
            .map_err(|e| anyhow::anyhow!("aac encode: {e}"))?;
        if enc_info.output_size > 0 {
            let bytes = Bytes::copy_from_slice(&out_buf[..enc_info.output_size]);
            let sample = Mp4Sample {
                start_time: frame_index * frame_len as u64,
                duration: frame_len as u32,
                rendering_offset: 0,
                is_sync: true,
                bytes,
            };
            writer
                .write_sample(track_id, &sample)
                .map_err(|e| anyhow::anyhow!("mp4 write sample: {e:?}"))?;
            frame_index += 1;
        }
        if enc_info.input_consumed == 0 {
            break;
        }
        pos += enc_info.input_consumed;
    }
    loop {
        let mut out_buf = vec![0u8; max_out];
        let enc_info = encoder
            .encode(&[], &mut out_buf)
            .map_err(|e| anyhow::anyhow!("aac flush: {e}"))?;
        if enc_info.output_size == 0 {
            break;
        }
        let bytes = Bytes::copy_from_slice(&out_buf[..enc_info.output_size]);
        let sample = Mp4Sample {
            start_time: frame_index * frame_len as u64,
            duration: frame_len as u32,
            rendering_offset: 0,
            is_sync: true,
            bytes,
        };
        writer
            .write_sample(track_id, &sample)
            .map_err(|e| anyhow::anyhow!("mp4 write sample: {e:?}"))?;
        frame_index += 1;
    }
    writer
        .write_end()
        .map_err(|e| anyhow::anyhow!("mp4 finalize: {e:?}"))?;
    Ok(())
}

fn interleave_i16(chans: &[Vec<f32>]) -> Vec<i16> {
    let channels = chans.len().max(1);
    let frames = chans.iter().map(|c| c.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in chans {
            let v = ch.get(i).copied().unwrap_or(0.0);
            out.push(f32_to_i16(v));
        }
    }
    out
}

fn f32_to_i16(v: f32) -> i16 {
    let clamped = v.clamp(-1.0, 1.0);
    (clamped * i16::MAX as f32) as i16
}

fn aac_freq_index(sr: u32) -> Option<SampleFreqIndex> {
    match sr {
        96_000 => Some(SampleFreqIndex::Freq96000),
        88_200 => Some(SampleFreqIndex::Freq88200),
        64_000 => Some(SampleFreqIndex::Freq64000),
        48_000 => Some(SampleFreqIndex::Freq48000),
        44_100 => Some(SampleFreqIndex::Freq44100),
        32_000 => Some(SampleFreqIndex::Freq32000),
        24_000 => Some(SampleFreqIndex::Freq24000),
        22_050 => Some(SampleFreqIndex::Freq22050),
        16_000 => Some(SampleFreqIndex::Freq16000),
        12_000 => Some(SampleFreqIndex::Freq12000),
        11_025 => Some(SampleFreqIndex::Freq11025),
        8_000 => Some(SampleFreqIndex::Freq8000),
        7_350 => Some(SampleFreqIndex::Freq7350),
        _ => None,
    }
}

// Export a selection from in-memory multi-channel samples (float32) to a WAV file.
#[allow(dead_code)]
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

// Overwrite: apply gain and replace the source file safely with optional .bak (all supported formats)
pub fn overwrite_gain_audio(src: &Path, gain_db: f32, backup: bool) -> Result<()> {
    use std::fs;
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("tmp");
    let tmp = parent.join(format!("._wvp_tmp.{}", ext));
    if tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    export_gain_audio(src, &tmp, gain_db)?;
    if backup {
        let fname = src.file_name().and_then(|s| s.to_str()).unwrap_or("backup");
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
    if means.is_empty() {
        // Fallback for very short audio (< window): use whole-signal mean power.
        // This avoids returning +/-inf for short clips where BS.1770 windowing can't be applied.
        let mut acc = 0.0f64;
        for &v in &p_sum { acc += v as f64; }
        let n = p_sum.len().max(1) as f64;
        let z = (acc / n).max(1e-24);
        let l = K_CONST + 10.0 * (z.log10() as f32);
        return Ok(l);
    }
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
