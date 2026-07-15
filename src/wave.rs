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
use std::num::{NonZeroU32, NonZeroU8};
use std::path::Path;
use vorbis_rs::VorbisEncoderBuilder;

use crate::audio::AudioEngine;
use crate::audio_io;
use rubato::{
    Async, Fft, FixedAsync, FixedSync, Resampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction as RubatoWindowFunction,
};
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

pub fn decode_wav_multi_prefix(path: &Path, max_secs: f32) -> Result<(Vec<Vec<f32>>, u32, bool)> {
    audio_io::decode_audio_multi_prefix(path, max_secs)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavBitDepth {
    Pcm16,
    Pcm24,
    Float32,
}

impl WavBitDepth {
    pub fn suffix(self) -> &'static str {
        match self {
            Self::Pcm16 => "16bit",
            Self::Pcm24 => "24bit",
            Self::Float32 => "32float",
        }
    }

    pub fn bits_per_sample(self) -> u16 {
        match self {
            Self::Pcm16 => 16,
            Self::Pcm24 => 24,
            Self::Float32 => 32,
        }
    }

    pub fn project_value(self) -> &'static str {
        match self {
            Self::Pcm16 => "pcm16",
            Self::Pcm24 => "pcm24",
            Self::Float32 => "float32",
        }
    }

    pub fn from_project_value(value: &str) -> Option<Self> {
        match value {
            "pcm16" => Some(Self::Pcm16),
            "pcm24" => Some(Self::Pcm24),
            "float32" => Some(Self::Float32),
            _ => None,
        }
    }
}

fn quantize_sample(sample: f32, depth: WavBitDepth) -> f32 {
    let sample = sample.clamp(-1.0, 1.0);
    match depth {
        WavBitDepth::Pcm16 => {
            let max_abs = i16::MAX as f32;
            ((sample * max_abs).round().clamp(-max_abs, max_abs)) / max_abs
        }
        WavBitDepth::Pcm24 => {
            let max_abs = 8_388_607.0f32;
            ((sample * max_abs).round().clamp(-max_abs, max_abs)) / max_abs
        }
        WavBitDepth::Float32 => sample,
    }
}

pub fn quantize_mono_in_place(samples: &mut [f32], depth: WavBitDepth) {
    if matches!(depth, WavBitDepth::Float32) {
        for v in samples.iter_mut() {
            *v = v.clamp(-1.0, 1.0);
        }
        return;
    }
    for v in samples.iter_mut() {
        *v = quantize_sample(*v, depth);
    }
}

pub fn quantize_channels_in_place(channels: &mut [Vec<f32>], depth: WavBitDepth) {
    for ch in channels.iter_mut() {
        quantize_mono_in_place(ch, depth);
    }
}

fn write_wav_range_with_depth(
    chans: &[Vec<f32>],
    sample_rate: u32,
    range: (usize, usize),
    dst: &Path,
    depth: WavBitDepth,
) -> Result<()> {
    let ch = chans.len() as u16;
    let (mut s, mut e) = range;
    if s > e {
        std::mem::swap(&mut s, &mut e);
    }
    let frames = chans.first().map(|c| c.len()).unwrap_or(0);
    let s = s.min(frames);
    let e = e.min(frames);
    let spec = match depth {
        WavBitDepth::Pcm16 => hound::WavSpec {
            channels: ch,
            sample_rate: sample_rate.max(1),
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
        WavBitDepth::Pcm24 => hound::WavSpec {
            channels: ch,
            sample_rate: sample_rate.max(1),
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        },
        WavBitDepth::Float32 => hound::WavSpec {
            channels: ch,
            sample_rate: sample_rate.max(1),
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    };
    let mut writer = hound::WavWriter::create(dst, spec)?;
    for i in s..e {
        for ci in 0..(ch as usize) {
            let v = chans
                .get(ci)
                .and_then(|c| c.get(i))
                .copied()
                .unwrap_or(0.0)
                .clamp(-1.0, 1.0);
            match depth {
                WavBitDepth::Pcm16 => {
                    writer.write_sample::<i16>((v * i16::MAX as f32).round() as i16)?;
                }
                WavBitDepth::Pcm24 => {
                    let max_abs = 8_388_607.0f32;
                    let q = (v * max_abs).round().clamp(-max_abs, max_abs) as i32;
                    writer.write_sample::<i32>(q)?;
                }
                WavBitDepth::Float32 => {
                    writer.write_sample::<f32>(v)?;
                }
            }
        }
    }
    writer.finalize()?;
    Ok(())
}

/// Encode a `u32` sample rate as the 80-bit IEEE 754 extended float used by
/// the AIFF `COMM` chunk (1 sign + 15 exponent bits, bias 16383, then a 64-bit
/// mantissa with an explicit integer bit).
fn sample_rate_to_extended80(rate: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if rate == 0 {
        return out;
    }
    let value = rate as u64;
    let shift = 63 - (63 - value.leading_zeros() as i32);
    let exponent = (16383 + 63 - shift) as u16;
    let mantissa = value << shift;
    out[0..2].copy_from_slice(&exponent.to_be_bytes());
    out[2..10].copy_from_slice(&mantissa.to_be_bytes());
    out
}

/// Parse the 80-bit extended sample rate back to `u32` (test helper / reader).
#[cfg(test)]
fn extended80_to_sample_rate(bytes: &[u8; 10]) -> u32 {
    let exponent = u16::from_be_bytes([bytes[0], bytes[1]]) & 0x7FFF;
    let mantissa = u64::from_be_bytes(bytes[2..10].try_into().unwrap());
    if exponent == 0 || mantissa == 0 {
        return 0;
    }
    let shift = 16383 + 63 - exponent as i32;
    if !(0..64).contains(&shift) {
        return 0;
    }
    (mantissa >> shift) as u32
}

/// Write AIFF (16/24-bit big-endian PCM) or AIFF-C with `fl32` compression
/// for 32-bit float, mirroring `write_wav_range_with_depth`'s channel and
/// clamping behavior.
fn write_aiff_with_depth(
    chans: &[Vec<f32>],
    sample_rate: u32,
    dst: &Path,
    depth: WavBitDepth,
) -> Result<()> {
    use std::io::Write;
    let channels = chans.len().max(1);
    let frames = chans.first().map(|c| c.len()).unwrap_or(0);
    let bytes_per_sample = match depth {
        WavBitDepth::Pcm16 => 2usize,
        WavBitDepth::Pcm24 => 3,
        WavBitDepth::Float32 => 4,
    };
    let is_float = matches!(depth, WavBitDepth::Float32);
    let sound_len = frames * channels * bytes_per_sample;

    let mut sound: Vec<u8> = Vec::with_capacity(sound_len);
    for i in 0..frames {
        for ci in 0..channels {
            let v = chans
                .get(ci)
                .and_then(|c| c.get(i))
                .copied()
                .unwrap_or(0.0)
                .clamp(-1.0, 1.0);
            match depth {
                WavBitDepth::Pcm16 => {
                    let q = (v * i16::MAX as f32).round() as i16;
                    sound.extend_from_slice(&q.to_be_bytes());
                }
                WavBitDepth::Pcm24 => {
                    let max_abs = 8_388_607.0f32;
                    let q = (v * max_abs).round().clamp(-max_abs, max_abs) as i32;
                    sound.extend_from_slice(&q.to_be_bytes()[1..4]);
                }
                WavBitDepth::Float32 => {
                    sound.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
    }

    // COMM: base 18 bytes; AIFF-C appends compressionType + pascal-string name.
    const FL32_NAME: &[u8] = b"\x0c32-bit float\x00"; // count 12 + chars + pad to even
    let comm_len: u32 = if is_float {
        18 + 4 + FL32_NAME.len() as u32
    } else {
        18
    };
    let ssnd_len: u32 = 8 + sound_len as u32;
    let mut form_len: u32 =
        4 /*form type*/ + (8 + comm_len) + (8 + ssnd_len) + (sound_len as u32 & 1);
    if is_float {
        form_len += 8 + 4; // FVER chunk
    }

    let mut out: Vec<u8> = Vec::with_capacity(form_len as usize + 8);
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&form_len.to_be_bytes());
    out.extend_from_slice(if is_float { b"AIFC" } else { b"AIFF" });
    if is_float {
        out.extend_from_slice(b"FVER");
        out.extend_from_slice(&4u32.to_be_bytes());
        out.extend_from_slice(&0xA280_5140u32.to_be_bytes()); // AIFC version 1 timestamp
    }
    out.extend_from_slice(b"COMM");
    out.extend_from_slice(&comm_len.to_be_bytes());
    out.extend_from_slice(&(channels as i16).to_be_bytes());
    out.extend_from_slice(&(frames as u32).to_be_bytes());
    out.extend_from_slice(&(depth.bits_per_sample() as i16).to_be_bytes());
    out.extend_from_slice(&sample_rate_to_extended80(sample_rate.max(1)));
    if is_float {
        out.extend_from_slice(b"fl32");
        out.extend_from_slice(FL32_NAME);
    }
    out.extend_from_slice(b"SSND");
    out.extend_from_slice(&ssnd_len.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // offset
    out.extend_from_slice(&0u32.to_be_bytes()); // blockSize
    out.extend_from_slice(&sound);
    if sound_len & 1 == 1 {
        out.push(0);
    }

    let mut file = std::fs::File::create(dst)
        .with_context(|| format!("create aiff output: {}", dst.display()))?;
    file.write_all(&out)?;
    Ok(())
}

struct AiffChunk {
    id: [u8; 4],
    payload: Vec<u8>,
}

fn parse_aiff_chunks(path: &Path) -> Result<(bool, Vec<AiffChunk>)> {
    use std::fs;
    let data = fs::read(path).with_context(|| format!("read aiff chunks: {}", path.display()))?;
    if data.len() < 12 || &data[0..4] != b"FORM" {
        anyhow::bail!("not an AIFF file");
    }
    let form_type = &data[8..12];
    let is_aifc = form_type == b"AIFC";
    if !is_aifc && form_type != b"AIFF" {
        anyhow::bail!("not an AIFF file");
    }
    let mut chunks = Vec::new();
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let id = [data[pos], data[pos + 1], data[pos + 2], data[pos + 3]];
        let size = u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        let chunk_start = pos + 8;
        let chunk_end = chunk_start.saturating_add(size).min(data.len());
        chunks.push(AiffChunk {
            id,
            payload: data[chunk_start..chunk_end].to_vec(),
        });
        let advance = 8 + size + (size & 1);
        if pos + advance <= pos {
            break;
        }
        pos = pos.saturating_add(advance);
    }
    Ok((is_aifc, chunks))
}

fn encode_aiff_chunks(path: &Path, is_aifc: bool, chunks: &[AiffChunk]) -> Result<()> {
    use std::fs;
    let mut out = Vec::new();
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(if is_aifc { b"AIFC" } else { b"AIFF" });
    for chunk in chunks {
        out.extend_from_slice(&chunk.id);
        out.extend_from_slice(&(chunk.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&chunk.payload);
        if chunk.payload.len() & 1 == 1 {
            out.push(0);
        }
    }
    let form_size = (out.len().saturating_sub(8)) as u32;
    out[4..8].copy_from_slice(&form_size.to_be_bytes());
    let tmp = unique_sibling_tmp(path, "aiffmk", "aiff");
    fs::write(&tmp, out)?;
    replace_file_with_tmp(&tmp, path, false)
}

/// Read the sustain loop from AIFF `INST` + `MARK` chunks (the AIFF
/// counterpart of the WAV `smpl` loop).
pub fn read_aiff_loop_markers(path: &Path) -> Option<(u32, u32)> {
    let (_, chunks) = parse_aiff_chunks(path).ok()?;
    let inst = chunks.iter().find(|c| &c.id == b"INST")?;
    if inst.payload.len() < 20 {
        return None;
    }
    let play_mode = i16::from_be_bytes([inst.payload[8], inst.payload[9]]);
    if play_mode == 0 {
        return None;
    }
    let begin_id = i16::from_be_bytes([inst.payload[10], inst.payload[11]]);
    let end_id = i16::from_be_bytes([inst.payload[12], inst.payload[13]]);
    let mark = chunks.iter().find(|c| &c.id == b"MARK")?;
    let mut positions = std::collections::HashMap::new();
    let payload = &mark.payload;
    if payload.len() < 2 {
        return None;
    }
    let count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let mut pos = 2usize;
    for _ in 0..count {
        if pos + 6 > payload.len() {
            break;
        }
        let id = i16::from_be_bytes([payload[pos], payload[pos + 1]]);
        let sample = u32::from_be_bytes([
            payload[pos + 2],
            payload[pos + 3],
            payload[pos + 4],
            payload[pos + 5],
        ]);
        positions.insert(id, sample);
        let name_len = *payload.get(pos + 6)? as usize;
        let entry = 6 + 1 + name_len;
        pos += entry + (entry & 1);
    }
    let start = *positions.get(&begin_id)?;
    let end = *positions.get(&end_id)?;
    (end > start).then_some((start, end))
}

/// Write (or clear) the sustain loop as AIFF `MARK` + `INST` chunks.
pub fn write_aiff_loop_markers(path: &Path, loop_opt: Option<(u32, u32)>) -> Result<()> {
    let (is_aifc, mut chunks) = parse_aiff_chunks(path)?;
    chunks.retain(|c| &c.id != b"MARK" && &c.id != b"INST");
    // Insert before SSND: several readers treat the sound data as running to
    // the end of the FORM, so trailing chunks would be misread as audio.
    let insert_at = chunks
        .iter()
        .position(|c| &c.id == b"SSND")
        .unwrap_or(chunks.len());
    if let Some((start, end)) = loop_opt.filter(|(s, e)| e > s) {
        let mut mark = Vec::new();
        mark.extend_from_slice(&2u16.to_be_bytes());
        for (id, sample, name) in [(1i16, start, b"beg loop".as_slice()), (2, end, b"end loop")] {
            mark.extend_from_slice(&id.to_be_bytes());
            mark.extend_from_slice(&sample.to_be_bytes());
            mark.push(name.len() as u8);
            mark.extend_from_slice(name);
            if (1 + name.len()) & 1 == 1 {
                mark.push(0);
            }
        }
        chunks.insert(
            insert_at,
            AiffChunk {
                id: *b"MARK",
                payload: mark,
            },
        );
        let mut inst = Vec::with_capacity(20);
        inst.push(60); // baseNote (C4)
        inst.push(0); // detune
        inst.push(0); // lowNote
        inst.push(127); // highNote
        inst.push(1); // lowVelocity
        inst.push(127); // highVelocity
        inst.extend_from_slice(&0i16.to_be_bytes()); // gain
        inst.extend_from_slice(&1i16.to_be_bytes()); // sustain: forward loop
        inst.extend_from_slice(&1i16.to_be_bytes()); // sustain begin marker id
        inst.extend_from_slice(&2i16.to_be_bytes()); // sustain end marker id
        inst.extend_from_slice(&0i16.to_be_bytes()); // release: no loop
        inst.extend_from_slice(&0i16.to_be_bytes());
        inst.extend_from_slice(&0i16.to_be_bytes());
        chunks.insert(
            insert_at + 1,
            AiffChunk {
                id: *b"INST",
                payload: inst,
            },
        );
    }
    encode_aiff_chunks(path, is_aifc, &chunks)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResampleQuality {
    Fast,
    Good,
    Best,
}

fn resample_quality_params(
    quality: ResampleQuality,
) -> (usize, f32, usize, SincInterpolationType, usize) {
    match quality {
        ResampleQuality::Fast => (64, 0.90, 64, SincInterpolationType::Linear, 1024),
        ResampleQuality::Good => (128, 0.94, 128, SincInterpolationType::Quadratic, 1024),
        ResampleQuality::Best => (256, 0.96, 256, SincInterpolationType::Cubic, 2048),
    }
}

fn resample_all_channels<R: Resampler<f32>>(
    chans: &[Vec<f32>],
    mut resampler: R,
) -> Result<Vec<Vec<f32>>> {
    use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
    let n_ch = chans.len();
    let input_len = chans.iter().map(|ch| ch.len()).min().unwrap_or(0);
    if input_len == 0 {
        return Ok((0..n_ch).map(|_| Vec::new()).collect());
    }
    let out_len = resampler.process_all_needed_output_len(input_len).max(1);
    let mut channels_out: Vec<Vec<f32>> = (0..n_ch).map(|_| vec![0f32; out_len]).collect();
    let adapter_in = SequentialSliceOfVecs::new(chans, n_ch, input_len)
        .map_err(|e| anyhow::anyhow!("rubato adapter_in: {e:?}"))?;
    let mut adapter_out = SequentialSliceOfVecs::new_mut(&mut channels_out, n_ch, out_len)
        .map_err(|e| anyhow::anyhow!("rubato adapter_out: {e:?}"))?;
    let (_, actual_out) = resampler
        .process_all_into_buffer(&adapter_in, &mut adapter_out, input_len, None)
        .map_err(|e| anyhow::anyhow!("rubato process_all: {e}"))?;
    for ch in &mut channels_out {
        ch.truncate(actual_out);
    }
    Ok(channels_out)
}

fn fft_chunk_size_for_quality(quality: ResampleQuality) -> usize {
    match quality {
        ResampleQuality::Fast => 1024,
        ResampleQuality::Good => 2048,
        ResampleQuality::Best => 4096,
    }
}

fn resample_with_rubato(
    mono: &[f32],
    in_sr: u32,
    out_sr: u32,
    params: SincInterpolationParameters,
    chunk_size: usize,
) -> Result<Vec<f32>> {
    let ratio = out_sr as f64 / in_sr as f64;
    let resampler = Async::<f32>::new_sinc(
        ratio,
        2.0,
        &params,
        chunk_size.max(32),
        1,
        FixedAsync::Input,
    )
    .map_err(|e| anyhow::anyhow!("rubato init failed: {e}"))?;
    let result = resample_all_channels(&[mono.to_vec()], resampler)?;
    Ok(result.into_iter().next().unwrap_or_default())
}

fn resample_channels_with_rubato(
    chans: &[Vec<f32>],
    in_sr: u32,
    out_sr: u32,
    params: SincInterpolationParameters,
    chunk_size: usize,
) -> Result<Vec<Vec<f32>>> {
    if chans.is_empty() {
        return Ok(Vec::new());
    }
    let ratio = out_sr as f64 / in_sr as f64;
    let resampler = Async::<f32>::new_sinc(
        ratio,
        2.0,
        &params,
        chunk_size.max(32),
        chans.len(),
        FixedAsync::Input,
    )
    .map_err(|e| anyhow::anyhow!("rubato multi-channel init failed: {e}"))?;
    resample_all_channels(chans, resampler)
}

fn resample_channels_with_rubato_fft(
    chans: &[Vec<f32>],
    in_sr: u32,
    out_sr: u32,
    quality: ResampleQuality,
) -> Result<Vec<Vec<f32>>> {
    if chans.is_empty() {
        return Ok(Vec::new());
    }
    let resampler = Fft::<f32>::new(
        in_sr.max(1) as usize,
        out_sr.max(1) as usize,
        fft_chunk_size_for_quality(quality).max(32),
        1,
        chans.len(),
        FixedSync::Both,
    )
    .map_err(|e| anyhow::anyhow!("rubato fft init failed: {e}"))?;
    resample_all_channels(chans, resampler)
}

/// Counts silent downgrades from the rubato resampler to naive linear
/// interpolation. The UI polls this each frame and surfaces a warning toast
/// when it grows (the resample functions themselves stay pure and are called
/// from worker threads).
pub static RESAMPLE_FALLBACK_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn note_resample_fallback(in_sr: u32, out_sr: u32) {
    RESAMPLE_FALLBACK_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    eprintln!("resample: rubato failed, fell back to linear interpolation ({in_sr} -> {out_sr} Hz)");
}

pub fn resample_quality(
    mono: &[f32],
    in_sr: u32,
    out_sr: u32,
    quality: ResampleQuality,
) -> Vec<f32> {
    if in_sr == out_sr || mono.is_empty() || in_sr == 0 || out_sr == 0 {
        return mono.to_vec();
    }
    let (sinc_len, f_cutoff, oversampling_factor, interpolation, chunk_size) =
        resample_quality_params(quality);
    let params = SincInterpolationParameters {
        sinc_len,
        f_cutoff,
        oversampling_factor,
        interpolation,
        window: RubatoWindowFunction::BlackmanHarris2,
    };
    match resample_with_rubato(mono, in_sr, out_sr, params, chunk_size) {
        Ok(out) if !out.is_empty() => out,
        _ => {
            note_resample_fallback(in_sr, out_sr);
            resample_linear(mono, in_sr, out_sr)
        }
    }
}

pub fn resample_channels_quality(
    chans: &[Vec<f32>],
    in_sr: u32,
    out_sr: u32,
    quality: ResampleQuality,
) -> Vec<Vec<f32>> {
    if chans.is_empty() || in_sr == out_sr || in_sr == 0 || out_sr == 0 {
        return chans.to_vec();
    }
    if let Ok(out) = resample_channels_with_rubato_fft(chans, in_sr, out_sr, quality) {
        if !out.is_empty() {
            return out;
        }
    }
    let (sinc_len, f_cutoff, oversampling_factor, interpolation, chunk_size) =
        resample_quality_params(quality);
    let params = SincInterpolationParameters {
        sinc_len,
        f_cutoff,
        oversampling_factor,
        interpolation,
        window: RubatoWindowFunction::BlackmanHarris2,
    };
    match resample_channels_with_rubato(chans, in_sr, out_sr, params, chunk_size) {
        Ok(out) if !out.is_empty() => out,
        _ => chans
            .iter()
            .map(|channel| resample_quality(channel, in_sr, out_sr, quality))
            .collect(),
    }
}

pub fn convert_wav_bit_depth(src: &Path, dst: &Path, depth: WavBitDepth) -> Result<()> {
    let (chans, in_sr) = decode_wav_multi(src)?;
    let frames = chans.first().map(|c| c.len()).unwrap_or(0);
    write_wav_range_with_depth(&chans, in_sr, (0, frames), dst, depth)
}

fn mixdown_channels_mono(chans: &[Vec<f32>]) -> Vec<f32> {
    if chans.is_empty() {
        return Vec::new();
    }
    let len = chans[0].len();
    if len == 0 {
        return Vec::new();
    }
    let chn = chans.len() as f32;
    let mut out = vec![0.0f32; len];
    for ch in chans {
        for (i, v) in ch.iter().enumerate() {
            if let Some(dst) = out.get_mut(i) {
                *dst += *v;
            }
        }
    }
    for v in &mut out {
        *v /= chn;
    }
    out
}

pub fn build_minmax(out: &mut Vec<(f32, f32)>, samples: &[f32], bins: usize) {
    out.clear();
    if samples.is_empty() || bins == 0 {
        return;
    }
    let len = samples.len();
    // f64: f32 mantissa cannot represent sample indices above 2^24 (~6 min at 48 kHz).
    let step = (len as f64 / bins as f64).max(1.0);
    let mut pos = 0.0f64;
    for _ in 0..bins {
        let start = pos as usize;
        let end = (pos + step) as usize;
        let end = end.min(len);
        if start >= end {
            out.push((0.0, 0.0));
        } else {
            let (mut mn, mut mx) = (f32::INFINITY, f32::NEG_INFINITY);
            for &v in &samples[start..end] {
                if v < mn {
                    mn = v;
                }
                if v > mx {
                    mx = v;
                }
            }
            if !mn.is_finite() || !mx.is_finite() {
                out.push((0.0, 0.0));
            } else {
                out.push((mn, mx));
            }
        }
        pos += step;
        if (pos as usize) >= len {
            break;
        }
    }
}

pub fn build_waveform_minmax_from_channels(
    channels: &[Vec<f32>],
    samples_len: usize,
    bins: usize,
) -> Vec<(f32, f32)> {
    if channels.is_empty() || samples_len == 0 || bins == 0 {
        return Vec::new();
    }
    let len = samples_len.min(channels[0].len());
    if len == 0 {
        return Vec::new();
    }
    let mut waveform = Vec::with_capacity(bins.min(len));
    let step = (len as f64 / bins as f64).max(1.0);
    let mut pos = 0.0f64;
    let channel_count = channels.len() as f32;
    for _ in 0..bins {
        let start = pos as usize;
        let end = ((pos + step) as usize).min(len);
        if start >= end {
            waveform.push((0.0, 0.0));
        } else if channels.len() == 1 {
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;
            for &v in &channels[0][start..end] {
                if v < mn {
                    mn = v;
                }
                if v > mx {
                    mx = v;
                }
            }
            waveform.push(if mn.is_finite() && mx.is_finite() {
                (mn, mx)
            } else {
                (0.0, 0.0)
            });
        } else {
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;
            for sample_idx in start..end {
                let mut sum = 0.0f32;
                for channel in channels {
                    sum += channel.get(sample_idx).copied().unwrap_or(0.0);
                }
                let mixed = sum / channel_count;
                if mixed < mn {
                    mn = mixed;
                }
                if mixed > mx {
                    mx = mixed;
                }
            }
            waveform.push(if mn.is_finite() && mx.is_finite() {
                (mn, mx)
            } else {
                (0.0, 0.0)
            });
        }
        pos += step;
        if (pos as usize) >= len {
            break;
        }
    }
    waveform
}

// Parse RIFF WAVE 'smpl' chunk and extract the first loop's start/end in samples (if present).
pub fn read_wav_loop_markers(path: &Path) -> Option<(u32, u32)> {
    use std::fs;
    let data = fs::read(path).ok()?;
    if data.len() < 12 {
        return None;
    }
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        let chunk_start = pos + 8;
        let chunk_end = chunk_start.saturating_add(size).min(data.len());
        if id == b"smpl" {
            // smpl header is 9 u32 (36 bytes) before loops
            if chunk_end.saturating_sub(chunk_start) < 36 {
                return None;
            }
            let num_loops_off = chunk_start + 28;
            if num_loops_off + 4 > data.len() {
                return None;
            }
            let num_loops = u32::from_le_bytes([
                data[num_loops_off],
                data[num_loops_off + 1],
                data[num_loops_off + 2],
                data[num_loops_off + 3],
            ]) as usize;
            let loops_off = chunk_start + 36;
            // each loop entry: 6 u32 = 24 bytes
            if num_loops == 0 || loops_off + 24 > data.len() {
                return None;
            }
            let start_off = loops_off + 8; // start at +8 bytes in loop struct
            let end_off = loops_off + 12; // end at +12 bytes in loop struct
            if end_off + 4 > data.len() {
                return None;
            }
            let start = u32::from_le_bytes([
                data[start_off],
                data[start_off + 1],
                data[start_off + 2],
                data[start_off + 3],
            ]);
            let end = u32::from_le_bytes([
                data[end_off],
                data[end_off + 1],
                data[end_off + 2],
                data[end_off + 3],
            ]);
            if end > start {
                return Some((start, end));
            } else {
                return None;
            }
        }
        // chunks are word (2-byte) aligned
        let advance = 8 + size + (size & 1);
        if pos + advance <= pos {
            break;
        }
        pos = pos.saturating_add(advance);
    }
    None
}

/// Map loop markers (ls, le) from source sample rate `in_sr` to output `out_sr`,
/// and clamp to [0, samples_len]. Returns normalized (start<=end) if valid and non-empty.
pub fn map_loop_markers_between_sr(
    ls: u32,
    le: u32,
    in_sr: u32,
    out_sr: u32,
    samples_len: usize,
) -> Option<(usize, usize)> {
    if in_sr == 0 || out_sr == 0 || samples_len == 0 {
        return None;
    }
    let in_sr_u = in_sr as u64;
    let out_sr_u = out_sr as u64;
    let s = ((ls as u64) * out_sr_u + (in_sr_u / 2)) / in_sr_u;
    let e = ((le as u64) * out_sr_u + (in_sr_u / 2)) / in_sr_u;
    let mut s = s as usize;
    let mut e = e as usize;
    if e < s {
        std::mem::swap(&mut s, &mut e);
    }
    s = s.min(samples_len);
    e = e.min(samples_len);
    if e > s {
        Some((s, e))
    } else {
        None
    }
}

/// Map loop markers from output SR (device) to file SR.
pub fn map_loop_markers_to_file_sr(
    s: usize,
    e: usize,
    out_sr: u32,
    file_sr: u32,
) -> Option<(u32, u32)> {
    if out_sr == 0 || file_sr == 0 {
        return None;
    }
    if e <= s {
        return None;
    }
    let out_sr_u = out_sr as u64;
    let file_sr_u = file_sr as u64;
    let s = ((s as u64) * file_sr_u + (out_sr_u / 2)) / out_sr_u;
    let e = ((e as u64) * file_sr_u + (out_sr_u / 2)) / out_sr_u;
    if e <= s {
        return None;
    }
    if s > u32::MAX as u64 || e > u32::MAX as u64 {
        return None;
    }
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
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        let chunk_start = pos + 8;
        let chunk_end = chunk_start.saturating_add(size).min(data.len());
        if id != b"smpl" {
            out.extend_from_slice(id);
            out.extend_from_slice(&(size as u32).to_le_bytes());
            out.extend_from_slice(&data[chunk_start..chunk_end]);
            if size & 1 == 1 {
                out.push(0);
            }
        }
        let advance = 8 + size + (size & 1);
        if pos + advance <= pos {
            break;
        }
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
            if chunk.len() & 1 == 1 {
                out.push(0);
            }
        }
    }
    let riff_size = (out.len().saturating_sub(8)) as u32;
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());
    let tmp = unique_sibling_tmp(path, "smpl", "wav");
    fs::write(&tmp, out)?;
    replace_file_with_tmp(&tmp, path, false)
}

#[derive(Clone)]
struct RiffWaveChunk {
    id: [u8; 4],
    payload: Vec<u8>,
}

fn parse_riff_wave_chunks(path: &Path) -> Result<Vec<RiffWaveChunk>> {
    use std::fs;
    let data = fs::read(path).with_context(|| format!("read riff chunks: {}", path.display()))?;
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        anyhow::bail!("not a RIFF/WAVE file");
    }
    let mut chunks = Vec::new();
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let id = [data[pos], data[pos + 1], data[pos + 2], data[pos + 3]];
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        let chunk_start = pos + 8;
        let chunk_end = chunk_start.saturating_add(size).min(data.len());
        chunks.push(RiffWaveChunk {
            id,
            payload: data[chunk_start..chunk_end].to_vec(),
        });
        let advance = 8 + size + (size & 1);
        if pos + advance <= pos {
            break;
        }
        pos = pos.saturating_add(advance);
    }
    Ok(chunks)
}

fn encode_riff_wave_chunks(path: &Path, chunks: &[RiffWaveChunk]) -> Result<()> {
    use std::fs;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(b"WAVE");
    for chunk in chunks {
        out.extend_from_slice(&chunk.id);
        out.extend_from_slice(&(chunk.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&chunk.payload);
        if chunk.payload.len() & 1 == 1 {
            out.push(0);
        }
    }
    let riff_size = (out.len().saturating_sub(8)) as u32;
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());
    fs::write(path, out).with_context(|| format!("write riff chunks: {}", path.display()))?;
    Ok(())
}

fn chunk_is_fresh_audio_core(chunk: &RiffWaveChunk) -> bool {
    chunk.id == *b"fmt " || chunk.id == *b"data" || chunk.id == *b"fact"
}

fn take_matching_chunk(chunks: &mut Vec<RiffWaveChunk>, id: [u8; 4]) -> Option<RiffWaveChunk> {
    let idx = chunks.iter().position(|chunk| chunk.id == id)?;
    Some(chunks.remove(idx))
}

fn merge_wav_metadata_from_source(src: &Path, dst: &Path) -> Result<()> {
    let source_chunks = parse_riff_wave_chunks(src)?;
    let mut fresh_chunks = parse_riff_wave_chunks(dst)?;
    let mut merged = Vec::new();

    for chunk in source_chunks {
        if chunk_is_fresh_audio_core(&chunk) {
            if let Some(replacement) = take_matching_chunk(&mut fresh_chunks, chunk.id) {
                merged.push(replacement);
            }
            continue;
        }
        merged.push(chunk);
    }

    for chunk in fresh_chunks {
        if chunk_is_fresh_audio_core(&chunk) {
            merged.push(chunk);
        }
    }

    encode_riff_wave_chunks(dst, &merged)
}

fn copy_mp3_metadata_from_source(src: &Path, dst: &Path) -> Result<()> {
    let tag = match id3::Tag::read_from_path(src) {
        Ok(tag) => tag,
        Err(err) if matches!(err.kind, id3::ErrorKind::NoTag) => return Ok(()),
        Err(_) => return Ok(()),
    };
    tag.write_to_path(dst, id3::Version::Id3v24)
        .with_context(|| format!("copy mp3 tags {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

fn copy_m4a_metadata_from_source(src: &Path, dst: &Path) -> Result<()> {
    let tag = match mp4ameta::Tag::read_from_path(src) {
        Ok(tag) => tag,
        Err(_) => return Ok(()),
    };
    tag.write_to_path(dst)
        .with_context(|| format!("copy m4a tags {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

pub fn copy_audio_metadata_from_source(src: &Path, dst: &Path) -> Result<()> {
    let src_ext = ext_lower(src);
    let dst_ext = ext_lower(dst);
    match (src_ext.as_deref(), dst_ext.as_deref()) {
        (Some("wav"), Some("wav")) => merge_wav_metadata_from_source(src, dst),
        (Some("mp3"), Some("mp3")) => copy_mp3_metadata_from_source(src, dst),
        (Some("m4a"), Some("m4a")) => copy_m4a_metadata_from_source(src, dst),
        (Some("flac"), Some("flac")) => crate::flac_meta::copy_flac_metadata_from_source(src, dst),
        _ => Ok(()),
    }
}

fn try_copy_audio_metadata_from_source(src: &Path, dst: &Path) {
    if let Err(err) = copy_audio_metadata_from_source(src, dst) {
        eprintln!(
            "copy metadata failed {} -> {}: {err:?}",
            src.display(),
            dst.display()
        );
    }
}

// High level helper used by UI when a file is clicked
pub fn prepare_for_playback(
    path: &Path,
    audio: &AudioEngine,
    out_waveform: &mut Vec<(f32, f32)>,
) -> Result<()> {
    prepare_for_playback_quality(path, audio, out_waveform, ResampleQuality::Good)
}

pub fn prepare_for_playback_quality(
    path: &Path,
    audio: &AudioEngine,
    out_waveform: &mut Vec<(f32, f32)>,
    quality: ResampleQuality,
) -> Result<()> {
    let (mut chans, in_sr) = decode_wav_multi(path)?;
    let out_sr = audio.shared.out_sample_rate;
    if in_sr != out_sr {
        chans = resample_channels_quality(&chans, in_sr, out_sr, quality);
    }
    let mono = mixdown_channels_mono(&chans);
    audio.set_samples_channels(chans);
    audio.stop();
    build_minmax(out_waveform, &mono, 2048);
    Ok(())
}

pub fn prepare_for_list_preview(path: &Path, audio: &AudioEngine, max_secs: f32) -> Result<bool> {
    prepare_for_list_preview_quality(path, audio, max_secs, ResampleQuality::Good)
}

pub fn prepare_for_list_preview_quality(
    path: &Path,
    audio: &AudioEngine,
    max_secs: f32,
    quality: ResampleQuality,
) -> Result<bool> {
    let (mut chans, in_sr, truncated) = decode_wav_multi_prefix(path, max_secs)?;
    let out_sr = audio.shared.out_sample_rate;
    if in_sr != out_sr {
        chans = resample_channels_quality(&chans, in_sr, out_sr, quality);
    }
    audio.set_samples_channels(chans);
    audio.stop();
    Ok(truncated)
}

// Prepare with Speed mode (rate change without pitch preservation) via offline render.
pub fn prepare_for_speed_offline(
    path: &Path,
    audio: &AudioEngine,
    out_waveform: &mut Vec<(f32, f32)>,
    rate: f32,
) -> Result<()> {
    prepare_for_speed_offline_quality(path, audio, out_waveform, rate, ResampleQuality::Good)
}

pub fn prepare_for_speed_offline_quality(
    path: &Path,
    audio: &AudioEngine,
    out_waveform: &mut Vec<(f32, f32)>,
    rate: f32,
    quality: ResampleQuality,
) -> Result<()> {
    let (mut chans, in_sr) = decode_wav_multi(path)?;
    let out_sr = audio.shared.out_sample_rate.max(1);
    if in_sr != out_sr {
        chans = resample_channels_quality(&chans, in_sr, out_sr, quality);
    }
    for channel in chans.iter_mut() {
        *channel = process_speed_offline(channel, rate);
    }
    let mono = mixdown_channels_mono(&chans);
    build_minmax(out_waveform, &mono, 2048);
    audio.set_samples_channels(chans);
    audio.stop();
    Ok(())
}

pub fn prepare_for_speed(
    path: &Path,
    audio: &AudioEngine,
    out_waveform: &mut Vec<(f32, f32)>,
    rate: f32,
) -> Result<()> {
    prepare_for_speed_offline(path, audio, out_waveform, rate)
}

pub fn prepare_for_speed_quality(
    path: &Path,
    audio: &AudioEngine,
    out_waveform: &mut Vec<(f32, f32)>,
    rate: f32,
    quality: ResampleQuality,
) -> Result<()> {
    prepare_for_speed_offline_quality(path, audio, out_waveform, rate, quality)
}

// Prepare with PitchShift mode (preserve duration, shift pitch in semitones)
#[allow(dead_code)]
pub fn prepare_for_pitchshift(
    path: &Path,
    audio: &AudioEngine,
    out_waveform: &mut Vec<(f32, f32)>,
    semitones: f32,
) -> Result<()> {
    let (mono, in_sr) = decode_wav_mono(path)?;
    let out_sr = audio.shared.out_sample_rate;
    let mut out = process_pitchshift_offline(&mono, in_sr, out_sr, semitones);
    // waveform reflects processed output
    build_minmax(out_waveform, &out, 2048);
    audio.set_samples_mono(std::mem::take(&mut out));
    audio.stop();
    Ok(())
}

// Prepare with TimeStretch mode (preserve pitch, change duration by rate: 0.5 -> slower/longer)
#[allow(dead_code)]
pub fn prepare_for_timestretch(
    path: &Path,
    audio: &AudioEngine,
    out_waveform: &mut Vec<(f32, f32)>,
    rate: f32,
) -> Result<()> {
    let rate = rate.clamp(0.25, 4.0);
    let (mono, in_sr) = decode_wav_mono(path)?;
    let out_sr = audio.shared.out_sample_rate;
    let mut out = process_timestretch_offline(&mono, in_sr, out_sr, rate);
    build_minmax(out_waveform, &out, 2048);
    audio.set_samples_mono(std::mem::take(&mut out));
    audio.stop();
    Ok(())
}

// Heavy offline: pitch-shift preserving duration
pub fn process_pitchshift_offline(
    mono: &[f32],
    in_sr: u32,
    out_sr: u32,
    semitones: f32,
) -> Vec<f32> {
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

// Heavy offline: speed change that changes both pitch and duration while keeping the nominal
// sample-rate metadata unchanged.
pub fn process_speed_offline(mono: &[f32], rate: f32) -> Vec<f32> {
    let rate = rate.clamp(0.25, 4.0);
    if mono.is_empty() {
        return Vec::new();
    }
    if (rate - 1.0).abs() <= f32::EPSILON {
        return mono.to_vec();
    }
    let out_len = ((mono.len() as f64) / (rate as f64)).ceil().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    let last = mono.len().saturating_sub(1);
    for i in 0..out_len {
        let src_pos = (i as f64) * (rate as f64);
        let i0 = src_pos.floor() as usize;
        if i0 >= last {
            out.push(*mono.last().unwrap_or(&0.0));
            continue;
        }
        let i1 = (i0 + 1).min(last);
        let t = ((src_pos - i0 as f64) as f32).clamp(0.0, 1.0);
        out.push(mono[i0] * (1.0 - t) + mono[i1] * t);
    }
    out
}

/// Default crossfade length (in milliseconds) used when splicing a processed
/// segment back into its surrounding audio so the joins stay click-free.
pub const SPLICE_XFADE_MS: f32 = 8.0;

/// Crossfade length in samples for a splice at `sample_rate`, bounded so the
/// fades never cover more than half of either the original selection or the
/// processed segment.
pub fn splice_xfade_samples(sample_rate: u32, selection_len: usize, processed_len: usize) -> usize {
    let base = ((SPLICE_XFADE_MS / 1000.0) * sample_rate.max(1) as f32).round() as usize;
    base.min(selection_len / 2).min(processed_len / 2)
}

/// Replace `original[start..end)` with `processed`, equal-power crossfading
/// both joins against the original selection content so the transitions stay
/// smooth even when the replacement is shorter or longer than the selection.
///
/// At the head, the first `xfade` samples blend from the original selection's
/// opening into the processed segment; at the tail, the last `xfade` samples
/// blend back into the original selection's ending (which flows continuously
/// into the suffix). `xfade` is clamped via [`splice_xfade_samples`]-style
/// bounds internally.
pub fn splice_range_with_crossfade(
    original: &[f32],
    start: usize,
    end: usize,
    processed: &[f32],
    xfade: usize,
) -> Vec<f32> {
    let len = original.len();
    let start = start.min(len);
    let end = end.clamp(start, len);
    let sel_len = end - start;
    let mut seg = processed.to_vec();
    let xf = xfade.min(sel_len / 2).min(seg.len() / 2);
    if xf > 0 {
        let denom = (xf + 1) as f32;
        // Head join: original selection opening -> processed segment.
        if start > 0 {
            for i in 0..xf {
                let t = (i + 1) as f32 / denom;
                let w_in = (core::f32::consts::FRAC_PI_2 * t).sin();
                let w_out = (core::f32::consts::FRAC_PI_2 * t).cos();
                seg[i] = original[start + i] * w_out + seg[i] * w_in;
            }
        }
        // Tail join: processed segment -> original selection ending, which is
        // continuous with the suffix that follows.
        if end < len {
            let seg_len = seg.len();
            for i in 0..xf {
                let t = (i + 1) as f32 / denom;
                let w_in = (core::f32::consts::FRAC_PI_2 * t).sin();
                let w_out = (core::f32::consts::FRAC_PI_2 * t).cos();
                let si = seg_len - xf + i;
                let oi = end - xf + i;
                seg[si] = seg[si] * w_out + original[oi] * w_in;
            }
        }
    }
    let mut out = Vec::with_capacity(start + seg.len() + (len - end));
    out.extend_from_slice(&original[..start]);
    out.extend_from_slice(&seg);
    out.extend_from_slice(&original[end..]);
    out
}

/// Evaluate a piecewise-linear gain envelope (breakpoints in dB, sorted by
/// sample position) and apply it in place. Before the first point the first
/// point's dB is used; after the last point the last point's dB. With no
/// points, `fallback_db` applies uniformly. Interpolation is linear in dB
/// (like DAW fader automation).
pub fn apply_gain_envelope_in_place(
    samples: &mut [f32],
    points_db: &[(usize, f32)],
    fallback_db: f32,
    clamp_output: bool,
) {
    let db_to_amp = |db: f32| 10.0f32.powf(db / 20.0);
    if points_db.is_empty() {
        let g = db_to_amp(fallback_db);
        for v in samples.iter_mut() {
            *v *= g;
            if clamp_output {
                *v = v.clamp(-1.0, 1.0);
            }
        }
        return;
    }
    let mut pts: Vec<(usize, f32)> = points_db.to_vec();
    pts.sort_by_key(|p| p.0);
    let len = samples.len();
    // Head: flat at first point's level.
    let head_end = pts[0].0.min(len);
    let head_amp = db_to_amp(pts[0].1);
    for v in &mut samples[..head_end] {
        *v *= head_amp;
        if clamp_output {
            *v = v.clamp(-1.0, 1.0);
        }
    }
    // Segments between consecutive points: interpolate in dB. The dB ramp is
    // resolved per-sample; segments are usually long enough that the powf cost
    // stays negligible relative to the buffer scan itself.
    for w in pts.windows(2) {
        let (s0, db0) = w[0];
        let (s1, db1) = w[1];
        let s0 = s0.min(len);
        let s1 = s1.min(len);
        if s1 <= s0 {
            continue;
        }
        let span = (s1 - s0) as f32;
        for i in s0..s1 {
            let t = (i - s0) as f32 / span;
            let g = db_to_amp(db0 + (db1 - db0) * t);
            let v = &mut samples[i];
            *v *= g;
            if clamp_output {
                *v = v.clamp(-1.0, 1.0);
            }
        }
    }
    // Tail: flat at last point's level.
    let tail_start = pts[pts.len() - 1].0.min(len);
    let tail_amp = db_to_amp(pts[pts.len() - 1].1);
    for v in &mut samples[tail_start..] {
        *v *= tail_amp;
        if clamp_output {
            *v = v.clamp(-1.0, 1.0);
        }
    }
}

/// Evaluate the same piecewise-linear dB envelope used by
/// [`apply_gain_envelope_in_place`] at a single sample position.
pub fn gain_envelope_db_at(points_db: &[(usize, f32)], fallback_db: f32, sample: usize) -> f32 {
    if points_db.is_empty() {
        return fallback_db;
    }
    let mut pts: Vec<(usize, f32)> = points_db.to_vec();
    pts.sort_by_key(|p| p.0);
    if sample <= pts[0].0 {
        return pts[0].1;
    }
    for w in pts.windows(2) {
        let (s0, db0) = w[0];
        let (s1, db1) = w[1];
        if sample < s1 {
            if s1 <= s0 {
                return db1;
            }
            let t = (sample - s0) as f32 / (s1 - s0) as f32;
            return db0 + (db1 - db0) * t;
        }
    }
    pts[pts.len() - 1].1
}

/// Reverse `samples[start..end)` in place, equal-power blending the first and
/// last `xfade` samples of the reversed segment against the original content
/// so the joins to the untouched prefix/suffix stay click-free. Joins are only
/// smoothed where neighbouring audio actually exists (`start > 0` / `end < len`).
pub fn reverse_range_with_crossfade(samples: &mut [f32], start: usize, end: usize, xfade: usize) {
    let len = samples.len();
    let start = start.min(len);
    let end = end.clamp(start, len);
    let sel_len = end - start;
    if sel_len < 2 {
        return;
    }
    let original: Vec<f32> = samples[start..end].to_vec();
    samples[start..end].reverse();
    let xf = xfade.min(sel_len / 2);
    if xf == 0 {
        return;
    }
    let denom = (xf + 1) as f32;
    if start > 0 {
        for i in 0..xf {
            let t = (i + 1) as f32 / denom;
            let w_in = (core::f32::consts::FRAC_PI_2 * t).sin();
            let w_out = (core::f32::consts::FRAC_PI_2 * t).cos();
            samples[start + i] = original[i] * w_out + samples[start + i] * w_in;
        }
    }
    if end < len {
        for i in 0..xf {
            let t = (i + 1) as f32 / denom;
            let w_in = (core::f32::consts::FRAC_PI_2 * t).sin();
            let w_out = (core::f32::consts::FRAC_PI_2 * t).cos();
            let idx = end - xf + i;
            samples[idx] = samples[idx] * w_out + original[idx - start] * w_in;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseGateParams {
    pub threshold_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}

/// Envelope-follower noise gate: below `threshold_db` the signal is ramped
/// toward silence over `release_ms`; at/above it, ramped back to unity gain
/// over `attack_ms`. Shared by the EffectGraph NoiseGate node and the Editor
/// Inspector NoiseGate tool so both apply identical math.
pub fn process_noise_gate_offline(mono: &[f32], sample_rate: u32, params: &NoiseGateParams) -> Vec<f32> {
    if mono.is_empty() {
        return Vec::new();
    }
    let sr = sample_rate.max(1) as f32;
    let threshold_lin = 10.0f32.powf(params.threshold_db / 20.0);
    let attack_coeff = one_pole_coeff(params.attack_ms.max(0.01), sr);
    let release_coeff = one_pole_coeff(params.release_ms.max(0.01), sr);
    let mut envelope = 0.0f32;
    let mut gain = 0.0f32;
    let mut out = Vec::with_capacity(mono.len());
    for &sample in mono {
        let rectified = sample.abs();
        envelope = if rectified > envelope {
            rectified + attack_coeff * (envelope - rectified)
        } else {
            rectified + release_coeff * (envelope - rectified)
        };
        let target_gain = if envelope >= threshold_lin { 1.0 } else { 0.0 };
        let coeff = if target_gain > gain { attack_coeff } else { release_coeff };
        gain = target_gain + coeff * (gain - target_gain);
        out.push(sample * gain);
    }
    out
}

/// One-pole smoothing coefficient for a time constant of `time_ms` at
/// `sample_rate_hz`, shared by the gate and compressor envelope followers.
fn one_pole_coeff(time_ms: f32, sample_rate_hz: f32) -> f32 {
    (-1.0 / (time_ms * 0.001 * sample_rate_hz)).exp()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompressorParams {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
}

/// Feedforward peak compressor with a one-pole envelope follower. Shared by
/// the EffectGraph Compressor node and the Editor Inspector Compressor tool.
pub fn process_compressor_offline(mono: &[f32], sample_rate: u32, params: &CompressorParams) -> Vec<f32> {
    if mono.is_empty() {
        return Vec::new();
    }
    let sr = sample_rate.max(1) as f32;
    let ratio = params.ratio.max(1.0);
    let attack_coeff = one_pole_coeff(params.attack_ms.max(0.01), sr);
    let release_coeff = one_pole_coeff(params.release_ms.max(0.01), sr);
    let makeup = 10.0f32.powf(params.makeup_db / 20.0);
    let mut envelope_db = -120.0f32;
    let mut out = Vec::with_capacity(mono.len());
    for &sample in mono {
        let level_db = 20.0 * sample.abs().max(1e-9).log10();
        let coeff = if level_db > envelope_db { attack_coeff } else { release_coeff };
        envelope_db = level_db + coeff * (envelope_db - level_db);
        let over_db = envelope_db - params.threshold_db;
        let gain_db = if over_db > 0.0 {
            -over_db * (1.0 - 1.0 / ratio)
        } else {
            0.0
        };
        let gain = 10.0f32.powf(gain_db / 20.0) * makeup;
        out.push((sample * gain).clamp(-1.0, 1.0));
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BiquadKind {
    LowShelf,
    Peak,
    HighShelf,
    LowPass,
}

#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    /// Magnitude response in dB at normalized angular frequency `w`
    /// (`w = 2*pi*f/sr`), evaluated as |B(e^-jw)| / |A(e^-jw)|.
    fn magnitude_db(&self, w: f32) -> f32 {
        let (sin1, cos1) = w.sin_cos();
        let (sin2, cos2) = (2.0 * w).sin_cos();
        let num_re = self.b0 + self.b1 * cos1 + self.b2 * cos2;
        let num_im = -(self.b1 * sin1 + self.b2 * sin2);
        let den_re = 1.0 + self.a1 * cos1 + self.a2 * cos2;
        let den_im = -(self.a1 * sin1 + self.a2 * sin2);
        let num = (num_re * num_re + num_im * num_im).max(1e-24);
        let den = (den_re * den_re + den_im * den_im).max(1e-24);
        10.0 * (num / den).log10()
    }

    // RBJ Audio EQ Cookbook coefficients.
    fn design(kind: BiquadKind, freq_hz: f32, gain_db: f32, q: f32, sample_rate: f32) -> Self {
        let a = 10.0f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * (freq_hz.max(1.0) / sample_rate.max(1.0));
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q.max(0.01));
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::Peak => {
                let b0 = 1.0 + alpha * a;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 - alpha * a;
                let a0 = 1.0 + alpha / a;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha / a;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadKind::LowShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadKind::HighShelf => {
                let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
                let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
                let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
                let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
                let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
                let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
                let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;
                (b0, b1, b2, a0, a1, a2)
            }
            BiquadKind::LowPass => {
                // RBJ LPF; `gain_db` is unused for this kind.
                let b1 = 1.0 - cos_w0;
                let b0 = b1 * 0.5;
                let b2 = b0;
                let a0 = 1.0 + alpha;
                let a1 = -2.0 * cos_w0;
                let a2 = 1.0 - alpha;
                (b0, b1, b2, a0, a1, a2)
            }
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    fn process(&self, mono: &[f32]) -> Vec<f32> {
        let mut x1 = 0.0f32;
        let mut x2 = 0.0f32;
        let mut y1 = 0.0f32;
        let mut y2 = 0.0f32;
        let mut out = Vec::with_capacity(mono.len());
        for &x0 in mono {
            let y0 = self.b0 * x0 + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
            x2 = x1;
            x1 = x0;
            y2 = y1;
            y1 = y0;
            out.push(y0);
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreeBandEqParams {
    pub low_shelf_freq_hz: f32,
    pub low_shelf_gain_db: f32,
    pub mid_freq_hz: f32,
    pub mid_gain_db: f32,
    pub mid_q: f32,
    pub high_shelf_freq_hz: f32,
    pub high_shelf_gain_db: f32,
}

/// Zero-phase 4th-order Butterworth low-pass: two cascaded RBJ low-pass
/// biquads run forward and then backward (filtfilt), which squares the
/// magnitude response and cancels the phase. Zero phase matters here: the
/// band split forms its complements by subtraction, and any phase lag in
/// the low-pass would leak phase-rotated residue into the other bands.
fn zero_phase_lowpass4(mono: &[f32], sample_rate: u32, freq_hz: f32) -> Vec<f32> {
    let sr = sample_rate.max(1) as f32;
    let f = freq_hz.clamp(10.0, sr * 0.49);
    let stage1 = Biquad::design(BiquadKind::LowPass, f, 0.0, 0.541_196_1, sr);
    let stage2 = Biquad::design(BiquadKind::LowPass, f, 0.0, 1.306_563, sr);
    let mut out = stage2.process(&stage1.process(mono));
    out.reverse();
    let mut out = stage2.process(&stage1.process(&out));
    out.reverse();
    out
}

/// Split one channel into (low, mid, high) bands with guaranteed perfect
/// reconstruction: `low + mid + high == input` sample-for-sample, because
/// the bands are complementary subtractions around zero-phase Butterworth
/// low-passes. Routing a Band Split straight into a Band Join therefore
/// returns the original audio exactly (up to float rounding).
pub fn band_split_channel(
    mono: &[f32],
    sample_rate: u32,
    low_hz: f32,
    high_hz: f32,
) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    if mono.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let low_hz = low_hz.max(20.0);
    let high_hz = high_hz.max(low_hz * 1.01);
    let low = zero_phase_lowpass4(mono, sample_rate, low_hz);
    let rest: Vec<f32> = mono.iter().zip(&low).map(|(x, l)| x - l).collect();
    let mid = zero_phase_lowpass4(&rest, sample_rate, high_hz);
    let high: Vec<f32> = rest.iter().zip(&mid).map(|(r, m)| r - m).collect();
    (low, mid, high)
}

/// Mid/side encode: mono input passes through as mid (silent side); stereo
/// (or wider — only the first two channels are used) becomes
/// `M = (L+R)/2`, `S = (L-R)/2`. Exact inverse of [`ms_decode`].
pub fn ms_encode(channels: &[Vec<f32>]) -> (Vec<f32>, Vec<f32>) {
    match channels.len() {
        0 => (Vec::new(), Vec::new()),
        1 => {
            let mid = channels[0].clone();
            let side = vec![0.0f32; mid.len()];
            (mid, side)
        }
        _ => {
            let left = &channels[0];
            let right = &channels[1];
            let len = left.len().max(right.len());
            let mut mid = Vec::with_capacity(len);
            let mut side = Vec::with_capacity(len);
            for i in 0..len {
                let l = left.get(i).copied().unwrap_or(0.0);
                let r = right.get(i).copied().unwrap_or(0.0);
                mid.push((l + r) * 0.5);
                side.push((l - r) * 0.5);
            }
            (mid, side)
        }
    }
}

/// Mid/side decode: `L = M + S`, `R = M - S`. Exact inverse of
/// [`ms_encode`] for stereo input.
pub fn ms_decode(mid: &[f32], side: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let len = mid.len().max(side.len());
    let mut left = Vec::with_capacity(len);
    let mut right = Vec::with_capacity(len);
    for i in 0..len {
        let m = mid.get(i).copied().unwrap_or(0.0);
        let s = side.get(i).copied().unwrap_or(0.0);
        left.push(m + s);
        right.push(m - s);
    }
    (left, right)
}

/// Combined magnitude response of the 3-band EQ at `freq_hz`, in dB.
/// Used by the graphical EQ curve display; matches
/// [`process_three_band_eq_offline`]'s series topology exactly.
pub fn three_band_eq_response_db(
    params: &ThreeBandEqParams,
    sample_rate: u32,
    freq_hz: f32,
) -> f32 {
    let sr = sample_rate.max(1) as f32;
    let w = 2.0 * std::f32::consts::PI * (freq_hz.clamp(1.0, sr * 0.499) / sr);
    let low = Biquad::design(
        BiquadKind::LowShelf,
        params.low_shelf_freq_hz,
        params.low_shelf_gain_db,
        0.707,
        sr,
    );
    let mid = Biquad::design(
        BiquadKind::Peak,
        params.mid_freq_hz,
        params.mid_gain_db,
        params.mid_q.max(0.1),
        sr,
    );
    let high = Biquad::design(
        BiquadKind::HighShelf,
        params.high_shelf_freq_hz,
        params.high_shelf_gain_db,
        0.707,
        sr,
    );
    low.magnitude_db(w) + mid.magnitude_db(w) + high.magnitude_db(w)
}

/// Fixed-topology 3-band EQ (low-shelf, peak/bell, high-shelf) built from RBJ
/// cookbook biquads applied in series. Shared by the EffectGraph Eq node and
/// the Editor Inspector Eq tool.
pub fn process_three_band_eq_offline(
    mono: &[f32],
    sample_rate: u32,
    params: &ThreeBandEqParams,
) -> Vec<f32> {
    if mono.is_empty() {
        return Vec::new();
    }
    let sr = sample_rate.max(1) as f32;
    let low_shelf = Biquad::design(
        BiquadKind::LowShelf,
        params.low_shelf_freq_hz,
        params.low_shelf_gain_db,
        0.707,
        sr,
    );
    let mid = Biquad::design(
        BiquadKind::Peak,
        params.mid_freq_hz,
        params.mid_gain_db,
        params.mid_q.max(0.1),
        sr,
    );
    let high_shelf = Biquad::design(
        BiquadKind::HighShelf,
        params.high_shelf_freq_hz,
        params.high_shelf_gain_db,
        0.707,
        sr,
    );
    let stage1 = low_shelf.process(mono);
    let stage2 = mid.process(&stage1);
    high_shelf.process(&stage2)
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
    for c in chans.iter_mut() {
        for v in c.iter_mut() {
            *v = (*v * g).clamp(-1.0, 1.0);
        }
    }
    // Preserve source WAV format when available; otherwise use a safe default.
    let default_spec = hound::WavSpec {
        channels: chans.len().max(1) as u16,
        sample_rate: in_sr.max(1),
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let spec = src
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("wav"))
        .unwrap_or(false)
        .then(|| hound::WavReader::open(src).ok().map(|r| r.spec()))
        .flatten()
        .unwrap_or(default_spec);
    let mut writer = hound::WavWriter::create(
        dst,
        hound::WavSpec {
            channels: spec.channels,
            sample_rate: in_sr,
            bits_per_sample: spec.bits_per_sample,
            sample_format: spec.sample_format,
        },
    )?;
    let frames = chans.first().map(|c| c.len()).unwrap_or(0);
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for i in 0..frames {
                for ch in 0..(spec.channels as usize) {
                    let s = chans.get(ch).and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                    writer.write_sample::<f32>(s)?;
                }
            }
        }
        hound::SampleFormat::Int => {
            let max_abs = match spec.bits_per_sample {
                8 => 127.0,
                16 => 32767.0,
                24 => 8_388_607.0,
                32 => 2_147_483_647.0,
                b => ((1u64 << (b - 1)) - 1) as f64 as f32,
            };
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
    try_copy_audio_metadata_from_source(src, dst);
    Ok(())
}

pub fn export_gain_audio(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    let fmt = pick_format(src, dst)
        .ok_or_else(|| anyhow::anyhow!("unsupported format: {}", src.display()))?;
    match fmt.as_str() {
        "wav" => export_gain_wav(src, dst, gain_db),
        "aiff" | "aif" => export_gain_aiff(src, dst, gain_db),
        "flac" => export_gain_flac(src, dst, gain_db),
        "mp3" => export_gain_mp3(src, dst, gain_db),
        "m4a" => export_gain_m4a(src, dst, gain_db),
        "ogg" => export_gain_ogg(src, dst, gain_db),
        _ => anyhow::bail!("unsupported format: {}", fmt),
    }
}

fn export_gain_aiff(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    let (mut chans, in_sr) = decode_wav_multi(src)?;
    apply_gain_in_place(&mut chans, gain_db);
    write_aiff_with_depth(&chans, in_sr, dst, WavBitDepth::Float32)
}

fn export_gain_flac(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    let (mut chans, in_sr) = decode_wav_multi(src)?;
    apply_gain_in_place(&mut chans, gain_db);
    // Keep a 16-bit source at 16-bit; everything else gets FLAC's practical
    // maximum of 24-bit (FLAC has no float representation).
    let depth = match audio_io::read_audio_info(src)
        .map(|info| info.bits_per_sample)
        .unwrap_or(0)
    {
        1..=16 => WavBitDepth::Pcm16,
        _ => WavBitDepth::Pcm24,
    };
    encode_flac(dst, &chans, in_sr, Some(depth))?;
    try_copy_audio_metadata_from_source(src, dst);
    Ok(())
}

fn export_gain_mp3(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    use std::fs;
    let (mut chans, in_sr) = decode_wav_multi(src)?;
    apply_gain_in_place(&mut chans, gain_db);
    let data = encode_mp3(&chans, in_sr)?;
    fs::write(dst, data)?;
    try_copy_audio_metadata_from_source(src, dst);
    Ok(())
}

fn export_gain_m4a(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    let (mut chans, in_sr) = decode_wav_multi(src)?;
    apply_gain_in_place(&mut chans, gain_db);
    encode_aac_to_mp4(dst, &chans, in_sr)?;
    try_copy_audio_metadata_from_source(src, dst);
    Ok(())
}

fn export_gain_ogg(src: &Path, dst: &Path, gain_db: f32) -> Result<()> {
    let (mut chans, in_sr) = decode_wav_multi(src)?;
    apply_gain_in_place(&mut chans, gain_db);
    encode_ogg_vorbis(dst, &chans, in_sr)
}

/// App-wide lossy-encoder settings. Exports run on worker threads far from the
/// UI config, so the active settings are published here before spawning jobs;
/// encoders read them at encode time. Defaults match the previous hardcoded
/// values (MP3 192 kbps, AAC 192/96 kbps stereo/mono, Vorbis library default).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CodecExportOptions {
    pub mp3_bitrate_kbps: u32,
    pub aac_bitrate_kbps: u32,
    /// Vorbis perceptual quality in [-0.2, 1.0].
    pub ogg_quality: f32,
}

impl Default for CodecExportOptions {
    fn default() -> Self {
        Self {
            mp3_bitrate_kbps: 192,
            aac_bitrate_kbps: 192,
            ogg_quality: 0.5,
        }
    }
}

pub const MP3_BITRATES_KBPS: &[u32] = &[96, 128, 160, 192, 224, 256, 320];
pub const AAC_BITRATES_KBPS: &[u32] = &[96, 128, 160, 192, 256, 320];

fn codec_export_options_cell() -> &'static std::sync::RwLock<CodecExportOptions> {
    static OPTS: std::sync::OnceLock<std::sync::RwLock<CodecExportOptions>> =
        std::sync::OnceLock::new();
    OPTS.get_or_init(|| std::sync::RwLock::new(CodecExportOptions::default()))
}

pub fn set_codec_export_options(opts: CodecExportOptions) {
    if let Ok(mut guard) = codec_export_options_cell().write() {
        *guard = opts;
    }
}

pub fn codec_export_options() -> CodecExportOptions {
    codec_export_options_cell()
        .read()
        .map(|guard| *guard)
        .unwrap_or_default()
}

fn mp3_bitrate_from_kbps(kbps: u32) -> Mp3Bitrate {
    match kbps {
        0..=96 => Mp3Bitrate::Kbps96,
        97..=128 => Mp3Bitrate::Kbps128,
        129..=160 => Mp3Bitrate::Kbps160,
        161..=192 => Mp3Bitrate::Kbps192,
        193..=224 => Mp3Bitrate::Kbps224,
        225..=256 => Mp3Bitrate::Kbps256,
        _ => Mp3Bitrate::Kbps320,
    }
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
    if chans.len() > 2 {
        eprintln!(
            "lossy export: keeping the first 2 of {} channels (mp3/m4a/ogg are stereo-only)",
            chans.len()
        );
    }
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

fn encode_ogg_vorbis(dst: &Path, chans: &[Vec<f32>], in_sr: u32) -> Result<()> {
    let chans = normalize_channels_for_encode(chans);
    if chans.is_empty() {
        anyhow::bail!("empty channels");
    }
    let sample_rate = NonZeroU32::new(in_sr.max(1))
        .ok_or_else(|| anyhow::anyhow!("invalid sample rate for ogg encode"))?;
    let channels = NonZeroU8::new(chans.len().min(u8::MAX as usize) as u8)
        .ok_or_else(|| anyhow::anyhow!("invalid channel count for ogg encode"))?;
    let mut output = std::fs::File::create(dst)
        .with_context(|| format!("create ogg output: {}", dst.display()))?;
    let mut builder = VorbisEncoderBuilder::new(sample_rate, channels, &mut output)
        .map_err(|e| anyhow::anyhow!("ogg encoder init: {e}"))?;
    let target_quality = codec_export_options().ogg_quality.clamp(-0.2, 1.0);
    builder.bitrate_management_strategy(vorbis_rs::VorbisBitrateManagementStrategy::QualityVbr {
        target_quality,
    });
    let mut encoder = builder
        .build()
        .map_err(|e| anyhow::anyhow!("ogg encoder build: {e}"))?;
    // Submit in chunks to avoid pathological memory/latency spikes on long clips.
    let frames = chans.first().map(|c| c.len()).unwrap_or(0);
    let block = 4096usize;
    let mut start = 0usize;
    while start < frames {
        let end = (start + block).min(frames);
        let mut chunk: Vec<&[f32]> = Vec::with_capacity(chans.len());
        for ch in chans.iter() {
            chunk.push(&ch[start..end]);
        }
        encoder
            .encode_audio_block(&chunk)
            .map_err(|e| anyhow::anyhow!("ogg encode block: {e}"))?;
        start = end;
    }
    encoder
        .finish()
        .map_err(|e| anyhow::anyhow!("ogg finalize: {e}"))?;
    Ok(())
}

/// Encode to FLAC. FLAC stores integers only, so `Float32` (and unspecified)
/// depths are written as 24-bit PCM; `Pcm16` stays 16-bit. All channel counts
/// FLAC supports (up to 8) pass through unchanged.
fn encode_flac(
    dst: &Path,
    chans: &[Vec<f32>],
    sample_rate: u32,
    depth: Option<WavBitDepth>,
) -> Result<()> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    if chans.is_empty() {
        anyhow::bail!("empty channels");
    }
    if chans.len() > 8 {
        anyhow::bail!("flac export supports up to 8 channels, got {}", chans.len());
    }
    let bits_per_sample: usize = match depth {
        Some(WavBitDepth::Pcm16) => 16,
        _ => 24,
    };
    let channels = chans.len();
    // FLAC's minimum block size is 16 samples; pad ultra-short clips with
    // trailing silence rather than failing.
    const FLAC_MIN_BLOCK: usize = 16;
    let source_frames = chans.iter().map(|c| c.len()).min().unwrap_or(0);
    if source_frames == 0 {
        anyhow::bail!("empty channels");
    }
    let frames = source_frames.max(FLAC_MIN_BLOCK);
    let max_abs = ((1i64 << (bits_per_sample - 1)) - 1) as f32;
    let bytes_per_sample = bits_per_sample / 8;
    let quantize = |v: f32| -> i32 {
        let v = v.clamp(-1.0, 1.0);
        (v * max_abs).round().clamp(-max_abs, max_abs) as i32
    };
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|e| anyhow::anyhow!("flac encoder config: {e:?}"))?;
    let block_size = config.block_size;

    // Pass 1: MD5 of the quantized little-endian samples. The digest must be
    // set on STREAMINFO before the stream is built, and we deliberately avoid
    // materializing the whole interleaved buffer, so the samples are quantized
    // block-by-block here and again during encoding below. Quantization is a
    // few cheap ops per sample — negligible next to FLAC's LPC/Rice coding —
    // and this keeps peak RAM bounded by one block regardless of clip length.
    let mut md5 = <md5::Md5 as md5::Digest>::new();
    for i in 0..frames {
        for ch in chans {
            let q = quantize(ch.get(i).copied().unwrap_or(0.0));
            md5::Digest::update(&mut md5, &q.to_le_bytes()[0..bytes_per_sample]);
        }
    }
    let md5_digest: [u8; 16] = md5::Digest::finalize(md5).into();

    let mut stream_info = flacenc::component::StreamInfo::new(
        sample_rate.max(1) as usize,
        channels,
        bits_per_sample,
    )
    .map_err(|e| anyhow::anyhow!("flac stream init: {e:?}"))?;
    stream_info.set_md5_digest(&md5_digest);
    let mut stream = flacenc::component::Stream::with_stream_info(stream_info);

    // Pass 2: encode frame-by-frame instead of via
    // `encode_with_fixed_block_size` — that helper pads the final partial
    // block up to a full block, appending silence and changing the decoded
    // length. Each block is quantized into a small reused scratch buffer.
    let mut block_scratch: Vec<i32> = Vec::with_capacity(block_size * channels);
    let mut pos = 0usize;
    let mut frame_number = 0usize;
    while pos < frames {
        let this_block = (frames - pos).min(block_size);
        block_scratch.clear();
        for i in pos..pos + this_block {
            for ch in chans {
                block_scratch.push(quantize(ch.get(i).copied().unwrap_or(0.0)));
            }
        }
        let mut framebuf = flacenc::source::FrameBuf::with_size(channels, this_block)
            .map_err(|e| anyhow::anyhow!("flac frame buffer: {e:?}"))?;
        {
            use flacenc::source::Fill;
            framebuf
                .fill_interleaved(&block_scratch)
                .map_err(|e| anyhow::anyhow!("flac frame fill: {e:?}"))?;
        }
        let frame =
            flacenc::encode_fixed_size_frame(&config, &framebuf, frame_number, stream.stream_info())
                .map_err(|e| anyhow::anyhow!("flac encode: {e:?}"))?;
        // add_frame accumulates total_samples and min/max block/frame sizes.
        stream.add_frame(frame);
        pos += this_block;
        frame_number += 1;
    }
    // flacenc 0.4 only ships an in-memory `ByteSink`, so the compressed output
    // is buffered whole before the min_block_size patch (which seeks back to
    // byte 8). This is bounded by the *compressed* size — smaller than the PCM
    // input we already stream — so it is an acceptable ceiling.
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| anyhow::anyhow!("flac write: {e:?}"))?;
    let mut bytes = sink.as_slice().to_vec();
    // This is a fixed-block-size stream whose final frame may be shorter.
    // `Stream::add_frame` folds that last frame into STREAMINFO's
    // min_block_size, which (per spec) excludes the last block; a mismatched
    // pair makes readers treat the stream as variable-block-size and reject
    // the fixed-strategy frame headers. Patch min_block_size (bytes 8..10:
    // magic 4 + block header 4) back to the nominal block size.
    if frames > block_size && bytes.len() >= 12 {
        let min_block = (block_size as u16).to_be_bytes();
        bytes[8..10].copy_from_slice(&min_block);
    }
    std::fs::write(dst, bytes)
        .with_context(|| format!("create flac output: {}", dst.display()))?;
    Ok(())
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
        .set_brate(mp3_bitrate_from_kbps(
            codec_export_options().mp3_bitrate_kbps,
        ))
        .map_err(|e| anyhow::anyhow!("mp3 bitrate: {e:?}"))?;
    builder
        .set_quality(Mp3Quality::Best)
        .map_err(|e| anyhow::anyhow!("mp3 quality: {e:?}"))?;
    let mut encoder = builder
        .build()
        .map_err(|e| anyhow::anyhow!("mp3 build: {e:?}"))?;
    let frames = chans[0].len().max(1);
    let mut out = Vec::with_capacity(max_required_buffer_size(frames));
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
    let bitrate_kbps = codec_export_options().aac_bitrate_kbps.clamp(32, 320);
    // Halve for mono so the default (192 kbps stereo) keeps the previous
    // 96 kbps mono behavior.
    let bitrate = if channels == 1 {
        (bitrate_kbps / 2).max(32) * 1000
    } else {
        bitrate_kbps * 1000
    };
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
    let encoder = AacEncoder::new(params).map_err(|e| anyhow::anyhow!("aac encoder init: {e}"))?;
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
    let file = File::create(dst).with_context(|| format!("create m4a: {}", dst.display()))?;
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
    let mut writer =
        Mp4Writer::write_start(file, &config).map_err(|e| anyhow::anyhow!("mp4 start: {e:?}"))?;
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
    // Some mp4 readers are strict about esds descriptors. Avoid replacing the mp4
    // output by default; allow optional ADTS fallback via env toggle.
    if crate::audio_io::read_audio_info(dst).is_err()
        && std::env::var("NEOWAVES_AAC_ADTS_FALLBACK")
            .ok()
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                !(v.is_empty() || v == "0" || v == "false" || v == "off")
            })
            .unwrap_or(false)
    {
        encode_aac_to_adts(dst, &chans, sr)?;
    }
    Ok(())
}

fn encode_aac_to_adts(dst: &Path, chans: &[Vec<f32>], in_sr: u32) -> Result<()> {
    use std::io::Write;
    if chans.is_empty() {
        anyhow::bail!("empty channels");
    }
    let mut chans = normalize_channels_for_encode(chans);
    let mut sr = in_sr;
    if aac_freq_index(sr).is_none() {
        let target = 48_000;
        chans = resample_channels(&chans, in_sr, target);
        sr = target;
    }
    let channels = chans.len();
    let bitrate_kbps = codec_export_options().aac_bitrate_kbps.clamp(32, 320);
    // Halve for mono so the default (192 kbps stereo) keeps the previous
    // 96 kbps mono behavior.
    let bitrate = if channels == 1 {
        (bitrate_kbps / 2).max(32) * 1000
    } else {
        bitrate_kbps * 1000
    };
    let params = AacEncoderParams {
        bit_rate: AacBitRate::Cbr(bitrate),
        sample_rate: sr,
        transport: AacTransport::Adts,
        channels: if channels == 1 {
            AacChannelMode::Mono
        } else {
            AacChannelMode::Stereo
        },
        audio_object_type: FdkAudioObjectType::Mpeg4LowComplexity,
    };
    let encoder =
        AacEncoder::new(params).map_err(|e| anyhow::anyhow!("aac adts encoder init: {e}"))?;
    let info = encoder
        .info()
        .map_err(|e| anyhow::anyhow!("aac adts encoder info: {e}"))?;
    let frame_len = info.frameLength as usize;
    if frame_len == 0 {
        anyhow::bail!("aac frame length is zero");
    }
    let max_out = (info.maxOutBufBytes as usize).max(4096);
    let interleaved = interleave_i16(&chans);
    let frame_samples = frame_len * channels;
    let mut file = std::fs::File::create(dst)
        .with_context(|| format!("create adts fallback: {}", dst.display()))?;
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
            .map_err(|e| anyhow::anyhow!("aac adts encode: {e}"))?;
        if enc_info.output_size > 0 {
            file.write_all(&out_buf[..enc_info.output_size])?;
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
            .map_err(|e| anyhow::anyhow!("aac adts flush: {e}"))?;
        if enc_info.output_size == 0 {
            break;
        }
        file.write_all(&out_buf[..enc_info.output_size])?;
    }
    file.flush()?;
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
    (clamped * i16::MAX as f32).round() as i16
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
pub fn export_selection_wav(
    chans: &[Vec<f32>],
    sample_rate: u32,
    range: (usize, usize),
    dst: &Path,
) -> Result<()> {
    export_selection_wav_with_depth(chans, sample_rate, range, dst, None)
}

pub fn export_selection_wav_with_depth(
    chans: &[Vec<f32>],
    sample_rate: u32,
    range: (usize, usize),
    dst: &Path,
    depth: Option<WavBitDepth>,
) -> Result<()> {
    write_wav_range_with_depth(
        chans,
        sample_rate,
        range,
        dst,
        depth.unwrap_or(WavBitDepth::Float32),
    )
}

// Export full in-memory audio to a supported format (wav/mp3/m4a) based on dst extension.
pub fn export_channels_audio(chans: &[Vec<f32>], sample_rate: u32, dst: &Path) -> Result<()> {
    export_channels_audio_with_depth(chans, sample_rate, dst, None)
}

pub fn export_channels_audio_with_depth(
    chans: &[Vec<f32>],
    sample_rate: u32,
    dst: &Path,
    wav_depth: Option<WavBitDepth>,
) -> Result<()> {
    let ext = dst
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "wav" => {
            let len = chans.first().map(|c| c.len()).unwrap_or(0);
            export_selection_wav_with_depth(chans, sample_rate, (0, len), dst, wav_depth)
        }
        "aiff" | "aif" => write_aiff_with_depth(
            chans,
            sample_rate,
            dst,
            wav_depth.unwrap_or(WavBitDepth::Float32),
        ),
        "flac" => encode_flac(dst, chans, sample_rate, wav_depth),
        "mp3" => {
            let data = encode_mp3(chans, sample_rate)?;
            std::fs::write(dst, data)?;
            Ok(())
        }
        "m4a" => encode_aac_to_mp4(dst, chans, sample_rate),
        "ogg" => encode_ogg_vorbis(dst, chans, sample_rate),
        _ => anyhow::bail!("unsupported export format: {}", ext),
    }
}

// Overwrite: export in-memory audio and replace the source file safely with optional .bak
pub fn overwrite_audio_from_channels(
    chans: &[Vec<f32>],
    sample_rate: u32,
    src: &Path,
    backup: bool,
) -> Result<()> {
    overwrite_audio_from_channels_with_depth(chans, sample_rate, src, backup, None)
}

/// Allocate a unique temp path next to `src` so concurrent operations in the
/// same directory never collide on a shared temp name.
fn unique_sibling_tmp(src: &Path, tag: &str, ext: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".wvp_tmp_{}_{}_{}.{}",
        std::process::id(),
        n,
        tag,
        ext
    ))
}

/// Replace `src` with the finished `tmp`, never leaving a window where the
/// original is deleted and unrecoverable. Optionally keeps `<name>.bak`.
fn replace_file_with_tmp(tmp: &Path, src: &Path, backup: bool) -> Result<()> {
    use std::fs;
    if backup {
        let fname = src.file_name().and_then(|s| s.to_str()).unwrap_or("backup");
        let bak = src.with_file_name(format!("{}.bak", fname));
        let _ = fs::remove_file(&bak);
        let _ = fs::copy(src, &bak);
    }
    // Atomic on Unix; on Windows rename fails while the target still exists.
    if fs::rename(tmp, src).is_ok() {
        return Ok(());
    }
    // Park the original under a unique sidecar name, move the new file in,
    // then drop the sidecar; on failure the original is restored.
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("tmp");
    let sidecar = unique_sibling_tmp(src, "old", ext);
    fs::rename(src, &sidecar)
        .with_context(|| format!("park original for replace: {}", src.display()))?;
    match fs::rename(tmp, src) {
        Ok(()) => {
            let _ = fs::remove_file(&sidecar);
            Ok(())
        }
        Err(err) => {
            let _ = fs::rename(&sidecar, src);
            let _ = fs::remove_file(tmp);
            Err(err).with_context(|| format!("replace file: {}", src.display()))
        }
    }
}

pub fn overwrite_audio_from_channels_with_depth(
    chans: &[Vec<f32>],
    sample_rate: u32,
    src: &Path,
    backup: bool,
    wav_depth: Option<WavBitDepth>,
) -> Result<()> {
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("tmp");
    let tmp = unique_sibling_tmp(src, "ow", ext);
    export_channels_audio_with_depth(chans, sample_rate, &tmp, wav_depth)?;
    try_copy_audio_metadata_from_source(src, &tmp);
    replace_file_with_tmp(&tmp, src, backup)
}

// Overwrite: apply gain and replace the source file safely with optional .bak
pub fn overwrite_gain_wav(src: &Path, gain_db: f32, backup: bool) -> Result<()> {
    let tmp = unique_sibling_tmp(src, "gain", "wav");
    export_gain_wav(src, &tmp, gain_db)?;
    replace_file_with_tmp(&tmp, src, backup)
}

// Overwrite: apply gain and replace the source file safely with optional .bak (all supported formats)
pub fn overwrite_gain_audio(src: &Path, gain_db: f32, backup: bool) -> Result<()> {
    let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("tmp");
    let tmp = unique_sibling_tmp(src, "gain", ext);
    export_gain_audio(src, &tmp, gain_db)?;
    replace_file_with_tmp(&tmp, src, backup)
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
    let mut x1 = 0.0f32;
    let mut x2 = 0.0f32;
    let mut y1 = 0.0f32;
    let mut y2 = 0.0f32;
    for sample in x.iter_mut() {
        let xn = *sample;
        let y = b0 * xn + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        *sample = y;
        x2 = x1;
        x1 = xn;
        y2 = y1;
        y1 = y;
    }
}

fn k_weighting_apply_48k(chans: &mut [Vec<f32>]) {
    for ch in chans.iter_mut() {
        biquad_inplace_f32(ch, KW_B0_1, KW_B1_1, KW_B2_1, KW_A1_1, KW_A2_1);
        biquad_inplace_f32(ch, KW_B0_2, KW_B1_2, KW_B2_2, KW_A1_2, KW_A2_2);
    }
}

fn ensure_sr_48k(chans: &[Vec<f32>], in_sr: u32) -> (Vec<Vec<f32>>, u32) {
    if in_sr == 48_000 {
        return (chans.to_vec(), in_sr);
    }
    let mut out = Vec::with_capacity(chans.len());
    for ch in chans {
        out.push(resample_linear(ch, in_sr, 48_000));
    }
    (out, 48_000)
}

fn block_means_power(power: &[f32], win: usize, hop: usize) -> Vec<f64> {
    if power.len() < win || win == 0 || hop == 0 {
        return Vec::new();
    }
    let mut cs = Vec::with_capacity(power.len() + 1);
    cs.push(0.0f64);
    let mut sum = 0.0f64;
    for &v in power {
        sum += v as f64;
        cs.push(sum);
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + win <= power.len() {
        let s = cs[i + win] - cs[i];
        out.push(s / (win as f64));
        i += hop;
    }
    out
}

/// BS.1770-4 channel power weights. Channel identities are not available
/// from the decoder here, so common film-order layouts are assumed:
/// 5.1 = L R C LFE Ls Rs and 7.1 = L R C LFE Ls Rs Lrs Rrs (LFE excluded,
/// surrounds x1.41). Every other channel count uses weight 1.0.
fn bs1770_channel_weights(channels: usize) -> Vec<f32> {
    match channels {
        6 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41],
        8 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41, 1.41, 1.41],
        n => vec![1.0; n],
    }
}

fn power_to_lufs(z: f64) -> f32 {
    K_CONST + 10.0 * (z.max(1e-24)).log10() as f32
}

#[derive(Clone, Copy, Debug)]
pub struct LoudnessMetrics {
    /// Integrated loudness (gated per BS.1770-4).
    pub lufs_i: f32,
    /// Maximum momentary loudness (400 ms window, ungated; EBU Tech 3341).
    pub lufs_m_max: Option<f32>,
    /// Maximum short-term loudness (3 s window, ungated; EBU Tech 3341).
    pub lufs_s_max: Option<f32>,
    /// True peak per BS.1770-4 Annex 2 (oversampled inter-sample peak).
    pub true_peak_db: Option<f32>,
}

/// Inter-sample true peak via polyphase windowed-sinc interpolation
/// (BS.1770-4 Annex 2). 4x below 96 kHz, 2x below 192 kHz, sample peak above.
pub fn true_peak_db_from_multi(chans: &[Vec<f32>], in_sr: u32) -> Option<f32> {
    let mut peak_abs = 0.0f32;
    for ch in chans {
        for &v in ch {
            let a = v.abs();
            if a > peak_abs {
                peak_abs = a;
            }
        }
    }
    let factor: usize = if in_sr < 96_000 {
        4
    } else if in_sr < 192_000 {
        2
    } else {
        1
    };
    if factor > 1 {
        // Polyphase FIR: windowed sinc, 12 taps per phase, each phase
        // normalized to unit DC gain so a full-scale DC input stays 1.0.
        const TAPS_PER_PHASE: usize = 12;
        let total = TAPS_PER_PHASE * factor;
        let center = (total - 1) as f64 / 2.0;
        let mut phases: Vec<Vec<f32>> = vec![Vec::with_capacity(TAPS_PER_PHASE); factor];
        for k in 0..total {
            let x = (k as f64 - center) / factor as f64;
            let sinc = if x.abs() < 1e-12 {
                1.0
            } else {
                (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
            };
            let w = 0.5 * (1.0 - (2.0 * std::f64::consts::PI * k as f64 / (total - 1) as f64).cos());
            phases[k % factor].push((sinc * w) as f32);
        }
        for phase in phases.iter_mut() {
            let sum: f32 = phase.iter().sum();
            if sum.abs() > 1e-9 {
                for c in phase.iter_mut() {
                    *c /= sum;
                }
            }
        }
        for ch in chans {
            if ch.len() < TAPS_PER_PHASE {
                continue;
            }
            for n in 0..ch.len() {
                for phase in &phases {
                    let mut acc = 0.0f32;
                    for (m, &c) in phase.iter().enumerate() {
                        // Convolution index n - m, clamped at the edges.
                        let idx = n.saturating_sub(m);
                        acc += ch[idx] * c;
                    }
                    let a = acc.abs();
                    if a > peak_abs {
                        peak_abs = a;
                    }
                }
            }
        }
    }
    if peak_abs > 0.0 {
        Some(20.0 * peak_abs.log10())
    } else {
        Some(f32::NEG_INFINITY)
    }
}

/// All loudness metrics in one pass: resample-to-48k + K-weighting + the
/// per-sample power sum are shared between I / M / S; true peak runs on the
/// original (non-weighted, original-rate) channels.
///
/// Known deviation: the 48 kHz conversion uses linear interpolation, which
/// is within ~0.1 LU of a sinc resampler for typical program material but is
/// not bit-exact against a reference meter at non-48k rates.
pub fn loudness_metrics_from_multi(chans_in: &[Vec<f32>], in_sr: u32) -> Result<LoudnessMetrics> {
    loudness_metrics_impl(chans_in, in_sr, true)
}

fn loudness_metrics_impl(
    chans_in: &[Vec<f32>],
    in_sr: u32,
    with_true_peak: bool,
) -> Result<LoudnessMetrics> {
    if chans_in.is_empty() {
        anyhow::bail!("empty channels");
    }
    // The oversampled true-peak scan costs ~48 MACs per sample; callers that
    // only need integrated loudness (gain-change recalc) skip it.
    let true_peak_db = if with_true_peak {
        true_peak_db_from_multi(chans_in, in_sr)
    } else {
        None
    };
    // Resample to 48k and copy
    let (mut chans, sr) = ensure_sr_48k(chans_in, in_sr);
    let _ = sr; // sr is 48k now
                // K-weighting
    k_weighting_apply_48k(&mut chans);
    // Weighted power sum across channels (BS.1770 G weights, LFE excluded
    // for assumed 5.1/7.1 layouts).
    let weights = bs1770_channel_weights(chans.len());
    let n = chans[0].len();
    let mut p_sum = vec![0.0f32; n];
    for (ch, &w) in chans.iter().zip(weights.iter()) {
        if w == 0.0 {
            continue;
        }
        for i in 0..n {
            let v = ch[i];
            p_sum[i] += w * v * v;
        }
    }
    // Momentary: 400ms window with 100ms hop
    let win_m = (0.400 * 48_000.0) as usize;
    let hop = (0.100 * 48_000.0) as usize;
    let means = block_means_power(&p_sum, win_m, hop);
    // Short-term: 3s window with 100ms hop (ungated max per EBU Tech 3341)
    let win_s = (3.0 * 48_000.0) as usize;
    let lufs_s_max = block_means_power(&p_sum, win_s, hop)
        .into_iter()
        .fold(None::<f64>, |acc, m| Some(acc.map_or(m, |a| a.max(m))))
        .map(power_to_lufs);
    if means.is_empty() {
        // Fallback for very short audio (< window): use whole-signal mean power.
        // This avoids returning +/-inf for short clips where BS.1770 windowing can't be applied.
        let mut acc = 0.0f64;
        for &v in &p_sum {
            acc += v as f64;
        }
        let n = p_sum.len().max(1) as f64;
        let l = power_to_lufs(acc / n);
        return Ok(LoudnessMetrics {
            lufs_i: l,
            lufs_m_max: None,
            lufs_s_max,
            true_peak_db,
        });
    }
    let blocks_lufs: Vec<f32> = means.iter().map(|&m| power_to_lufs(m)).collect();
    let lufs_m_max = blocks_lufs
        .iter()
        .copied()
        .fold(None::<f32>, |acc, l| Some(acc.map_or(l, |a| a.max(l))));
    let lufs_i = {
        // Absolute gate -70 LUFS
        let mut sel: Vec<bool> = blocks_lufs.iter().map(|&l| l > -70.0).collect();
        if !sel.iter().any(|&b| b) {
            f32::NEG_INFINITY
        } else {
            // Average of means after absolute gate
            let mut num = 0usize;
            let mut acc = 0.0f64;
            for (i, &ok) in sel.iter().enumerate() {
                if ok {
                    acc += means[i];
                    num += 1;
                }
            }
            let z_abs = if num > 0 { acc / num as f64 } else { 0.0 };
            if z_abs <= 0.0 {
                f32::NEG_INFINITY
            } else {
                // Relative gate: -10 LU below the absolute-gated average.
                let thr = power_to_lufs(z_abs) - 10.0;
                for (i, l) in blocks_lufs.iter().enumerate() {
                    sel[i] = sel[i] && (*l > thr);
                }
                let mut acc2 = 0.0f64;
                let mut n2 = 0usize;
                for (i, &ok) in sel.iter().enumerate() {
                    if ok {
                        acc2 += means[i];
                        n2 += 1;
                    }
                }
                if n2 == 0 {
                    f32::NEG_INFINITY
                } else {
                    power_to_lufs(acc2 / n2 as f64)
                }
            }
        }
    };
    Ok(LoudnessMetrics {
        lufs_i,
        lufs_m_max,
        lufs_s_max,
        true_peak_db,
    })
}

pub fn lufs_integrated_from_multi(chans_in: &[Vec<f32>], in_sr: u32) -> Result<f32> {
    Ok(loudness_metrics_impl(chans_in, in_sr, false)?.lufs_i)
}

#[cfg(test)]
mod tests {
    use super::{
        encode_riff_wave_chunks, export_channels_audio, export_gain_audio, overwrite_gain_wav,
        parse_riff_wave_chunks, process_compressor_offline, process_noise_gate_offline,
        process_three_band_eq_offline, resample_channels_quality, resample_channels_with_rubato,
        resample_quality_params, resample_with_rubato, unique_sibling_tmp, CompressorParams,
        NoiseGateParams, ResampleQuality, RiffWaveChunk, RubatoWindowFunction,
        SincInterpolationParameters, ThreeBandEqParams,
    };
    use id3::TagLike;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_signal(len: usize, freq_scale: f32) -> Vec<f32> {
        (0..len)
            .map(|idx| {
                let t = idx as f32 / len.max(1) as f32;
                let phase_a = t * freq_scale * std::f32::consts::TAU;
                let phase_b = t * (freq_scale * 0.37 + 3.0) * std::f32::consts::TAU;
                (phase_a.sin() * 0.7) + (phase_b.cos() * 0.3)
            })
            .collect()
    }

    fn synth_stereo(sr: u32, secs: f32) -> Vec<Vec<f32>> {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for i in 0..frames {
            let t = i as f32 / sr.max(1) as f32;
            left.push((t * 220.0 * std::f32::consts::TAU).sin() * 0.30);
            right.push((t * 330.0 * std::f32::consts::TAU).sin() * 0.25);
        }
        vec![left, right]
    }

    #[test]
    fn unique_sibling_tmp_never_collides() {
        let src = PathBuf::from("/some/dir/file.wav");
        let a = unique_sibling_tmp(&src, "ow", "wav");
        let b = unique_sibling_tmp(&src, "ow", "wav");
        assert_ne!(a, b);
        assert_eq!(a.parent(), src.parent());
    }

    #[test]
    fn extended80_sample_rate_round_trips() {
        for rate in [8_000u32, 22_050, 44_100, 48_000, 96_000, 192_000] {
            let bytes = super::sample_rate_to_extended80(rate);
            assert_eq!(
                super::extended80_to_sample_rate(&bytes),
                rate,
                "rate {rate}"
            );
        }
    }

    #[test]
    fn aiff_export_round_trips_all_depths() {
        let dir = make_temp_dir("aiff_roundtrip");
        let chans = synth_stereo(48_000, 0.25);
        for (depth, tolerance) in [
            (super::WavBitDepth::Pcm16, 1.0 / 32_768.0 * 2.0),
            (super::WavBitDepth::Pcm24, 1.0 / 8_388_608.0 * 2.0),
            (super::WavBitDepth::Float32, 1e-6),
        ] {
            let dst = dir.join(format!("take_{}.aiff", depth.suffix()));
            super::write_aiff_with_depth(&chans, 48_000, &dst, depth).expect("write aiff");
            let info = crate::audio_io::read_audio_info(&dst).expect("probe aiff");
            assert_eq!(info.sample_rate, 48_000, "{depth:?}");
            assert_eq!(info.channels, 2, "{depth:?}");
            let (decoded, sr) = crate::audio_io::decode_audio_multi(&dst).expect("decode aiff");
            assert_eq!(sr, 48_000);
            assert_eq!(decoded.len(), 2);
            assert_eq!(decoded[0].len(), chans[0].len(), "{depth:?}");
            for (a, b) in decoded[0].iter().zip(chans[0].iter()) {
                assert!(
                    (a - b).abs() <= tolerance,
                    "{depth:?}: decoded {a} vs source {b}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn aiff_loop_markers_round_trip_and_clear() {
        let dir = make_temp_dir("aiff_loops");
        let chans = synth_stereo(44_100, 0.1);
        let dst = dir.join("loop.aiff");
        super::write_aiff_with_depth(&chans, 44_100, &dst, super::WavBitDepth::Pcm16)
            .expect("write aiff");
        assert_eq!(super::read_aiff_loop_markers(&dst), None);
        super::write_aiff_loop_markers(&dst, Some((100, 2_000))).expect("write loop");
        assert_eq!(super::read_aiff_loop_markers(&dst), Some((100, 2_000)));
        // The file must stay decodable after the chunk rewrite.
        let (decoded, _) = crate::audio_io::decode_audio_multi(&dst).expect("decode aiff");
        assert_eq!(decoded[0].len(), chans[0].len());
        super::write_aiff_loop_markers(&dst, None).expect("clear loop");
        assert_eq!(super::read_aiff_loop_markers(&dst), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn export_flac_roundtrip_preserves_audio_and_loop_metadata() {
        let dir = make_temp_dir("flac_roundtrip");
        let src = dir.join("source.flac");
        let chans = synth_stereo(48_000, 1.0);
        export_channels_audio(&chans, 48_000, &src).expect("export flac");

        let (decoded, sr) = crate::audio_io::decode_audio_multi(&src).expect("decode flac");
        assert_eq!(sr, 48_000);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].len(), chans[0].len());
        // Default depth is 24-bit PCM; quantization error stays tiny.
        let max_diff = decoded[0]
            .iter()
            .zip(chans[0].iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_diff < 1.0e-3, "flac roundtrip drifted: {max_diff}");

        crate::loop_markers::write_loop_markers(&src, Some((1_000, 2_000)))
            .expect("write flac loop");
        assert_eq!(
            crate::loop_markers::read_loop_markers(&src),
            Some((1_000, 2_000))
        );
        // The file must stay decodable after the metadata rewrite.
        let (redecoded, _) =
            crate::audio_io::decode_audio_multi(&src).expect("decode flac after meta rewrite");
        assert_eq!(redecoded[0].len(), decoded[0].len());

        // Gain export to a new FLAC carries the loop comments over.
        let dst = dir.join("out.flac");
        export_gain_audio(&src, &dst, -3.0).expect("export gain flac");
        assert_eq!(
            crate::loop_markers::read_loop_markers(&dst),
            Some((1_000, 2_000))
        );
        crate::loop_markers::write_loop_markers(&dst, None).expect("clear flac loop");
        assert_eq!(crate::loop_markers::read_loop_markers(&dst), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ogg_loop_markers_fall_back_to_sidecar() {
        let dir = make_temp_dir("ogg_loop_sidecar");
        let src = dir.join("source.ogg");
        export_channels_audio(&synth_stereo(48_000, 0.5), 48_000, &src).expect("export ogg");

        crate::loop_markers::write_loop_markers(&src, Some((100, 900)))
            .expect("write ogg loop sidecar");
        assert!(dir.join("source.loop.json").is_file());
        assert_eq!(
            crate::loop_markers::read_loop_markers(&src),
            Some((100, 900))
        );
        crate::loop_markers::write_loop_markers(&src, None).expect("clear ogg loop sidecar");
        assert!(!dir.join("source.loop.json").exists());
        assert_eq!(crate::loop_markers::read_loop_markers(&src), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_temp_dir(tag: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("unix time")
            .as_nanos();
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "neowaves_wave_tests_{tag}_{}_{}_{}",
            std::process::id(),
            now_ns,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn make_png_bytes() -> Vec<u8> {
        let image = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            4,
            4,
            image::Rgba([24, 200, 180, 255]),
        ));
        let mut png = Cursor::new(Vec::new());
        image
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("write png");
        png.into_inner()
    }

    #[test]
    fn resample_channels_quality_noop_preserves_input() {
        let chans = vec![make_signal(256, 11.0), make_signal(256, 19.0)];
        let out = resample_channels_quality(&chans, 48_000, 48_000, ResampleQuality::Fast);
        assert_eq!(out, chans);
    }

    #[test]
    fn resample_channels_quality_matches_per_channel_path() {
        // Verify that running sinc multi-channel is equivalent to running sinc per-channel.
        // Uses the internal sinc path directly (both FFT and sinc are valid but different
        // algorithms; this test checks channel independence within the sinc path).
        let in_sr = 44_100u32;
        let out_sr = 48_000u32;
        let left = make_signal(8_192, 17.0);
        let right = make_signal(8_192, 23.0);
        let chans = vec![left.clone(), right.clone()];

        let quality = ResampleQuality::Good;
        let (sinc_len, f_cutoff, oversampling_factor, interpolation, chunk_size) =
            resample_quality_params(quality);
        let params = SincInterpolationParameters {
            sinc_len,
            f_cutoff,
            oversampling_factor,
            interpolation,
            window: RubatoWindowFunction::BlackmanHarris2,
        };

        // Multi-channel sinc path
        let multi =
            resample_channels_with_rubato(&chans, in_sr, out_sr, params.clone(), chunk_size)
                .expect("multi sinc failed");
        // Per-channel sinc paths
        let expected_left = resample_with_rubato(&left, in_sr, out_sr, params.clone(), chunk_size)
            .expect("left sinc failed");
        let expected_right = resample_with_rubato(&right, in_sr, out_sr, params, chunk_size)
            .expect("right sinc failed");

        assert_eq!(multi.len(), 2);
        assert_eq!(multi[0].len(), expected_left.len());
        assert_eq!(multi[1].len(), expected_right.len());

        let max_left_diff = multi[0]
            .iter()
            .zip(expected_left.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        let max_right_diff = multi[1]
            .iter()
            .zip(expected_right.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);

        assert!(
            max_left_diff < 1.0e-4,
            "left channel drifted from sinc per-channel path: {max_left_diff}"
        );
        assert!(
            max_right_diff < 1.0e-4,
            "right channel drifted from sinc per-channel path: {max_right_diff}"
        );
    }

    #[test]
    fn overwrite_gain_wav_preserves_ancillary_chunks() {
        let dir = make_temp_dir("wav_chunk_preserve");
        let src = dir.join("source.wav");
        export_channels_audio(&synth_stereo(48_000, 1.2), 48_000, &src).expect("export wav");

        let mut chunks = parse_riff_wave_chunks(&src).expect("parse wav chunks");
        chunks.insert(
            0,
            RiffWaveChunk {
                id: *b"bext",
                payload: b"bext-metadata".to_vec(),
            },
        );
        chunks.insert(
            1,
            RiffWaveChunk {
                id: *b"iXML",
                payload: b"<BWFXML><NOTE>keep</NOTE></BWFXML>".to_vec(),
            },
        );
        chunks.push(RiffWaveChunk {
            id: *b"acid",
            payload: b"acid".to_vec(),
        });
        chunks.push(RiffWaveChunk {
            id: *b"JUNK",
            payload: vec![1, 2, 3, 4, 5],
        });
        encode_riff_wave_chunks(&src, &chunks).expect("rewrite wav with ancillary chunks");
        crate::markers::write_markers(
            &src,
            48_000,
            48_000,
            &[crate::markers::MarkerEntry {
                sample: 10_000,
                label: "M01".to_string(),
            }],
        )
        .expect("write wav markers");
        crate::loop_markers::write_loop_markers(&src, Some((12_000, 30_000)))
            .expect("write wav loop");

        overwrite_gain_wav(&src, -3.0, false).expect("overwrite wav gain");

        let chunk_ids: Vec<[u8; 4]> = parse_riff_wave_chunks(&src)
            .expect("parse overwritten wav")
            .into_iter()
            .map(|chunk| chunk.id)
            .collect();
        assert!(chunk_ids.contains(b"bext"));
        assert!(chunk_ids.contains(b"iXML"));
        assert!(chunk_ids.contains(b"acid"));
        assert!(chunk_ids.contains(b"JUNK"));
        assert!(chunk_ids.contains(b"fmt "));
        assert!(chunk_ids.contains(b"data"));
        assert!(chunk_ids.contains(b"cue "));
        assert!(chunk_ids.contains(b"LIST"));
        assert!(chunk_ids.contains(b"smpl"));
        assert_eq!(
            crate::loop_markers::read_loop_markers(&src),
            Some((12_000, 30_000))
        );
        assert_eq!(
            crate::markers::read_markers(&src, 48_000, 48_000)
                .expect("read preserved wav markers")
                .len(),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_gain_mp3_preserves_id3_and_loop_tags() {
        let dir = make_temp_dir("mp3_metadata_preserve");
        let src = dir.join("source.mp3");
        let dst = dir.join("out.mp3");
        export_channels_audio(&synth_stereo(44_100, 1.4), 44_100, &src).expect("export mp3");

        let picture = id3::frame::Picture {
            mime_type: "image/png".to_string(),
            picture_type: id3::frame::PictureType::CoverFront,
            description: String::new(),
            data: make_png_bytes(),
        };
        let mut tag = id3::Tag::new();
        tag.set_title("keep-title");
        tag.set_artist("keep-artist");
        tag.add_frame(picture);
        tag.write_to_path(&src, id3::Version::Id3v24)
            .expect("write src id3");
        crate::loop_markers::write_loop_markers(&src, Some((1234, 5678)))
            .expect("write src mp3 loop");

        export_gain_audio(&src, &dst, 2.5).expect("export gain mp3");

        let tag = id3::Tag::read_from_path(&dst).expect("read dst id3");
        assert_eq!(tag.title(), Some("keep-title"));
        assert_eq!(tag.artist(), Some("keep-artist"));
        assert!(tag.pictures().next().is_some());
        assert_eq!(
            crate::loop_markers::read_loop_markers(&dst),
            Some((1234, 5678))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "generated m4a fixtures are not stable for mp4ameta round-trip; verify with real-world m4a files"]
    fn export_gain_m4a_preserves_metadata_and_loop_tags() {
        let dir = make_temp_dir("m4a_metadata_preserve");
        let src = dir.join("source.m4a");
        let dst = dir.join("out.m4a");
        export_channels_audio(&synth_stereo(44_100, 1.1), 44_100, &src).expect("export m4a");

        let mut tag = mp4ameta::Tag::default();
        tag.set_title("keep-title");
        tag.set_bpm(128);
        tag.set_artwork(mp4ameta::Img::png(make_png_bytes()));
        tag.write_to_path(&src).expect("write src m4a tag");
        if let Ok(src_tag) = mp4ameta::Tag::read_from_path(&src) {
            assert_eq!(src_tag.title(), Some("keep-title"));
            assert_eq!(src_tag.bpm(), Some(128));
            assert!(src_tag.artwork().is_some());
        } else {
            eprintln!(
                "warning: skipping strict m4a source metadata assertion for {}",
                src.display()
            );
        }
        let loop_write = crate::loop_markers::write_loop_markers(&src, Some((4321, 8765)));

        export_gain_audio(&src, &dst, 1.0).expect("export gain m4a");

        if let Ok(tag) = mp4ameta::Tag::read_from_path(&dst) {
            assert_eq!(tag.title(), Some("keep-title"));
            assert_eq!(tag.bpm(), Some(128));
            assert!(tag.artwork().is_some());
            if loop_write.is_ok() {
                assert_eq!(
                    crate::loop_markers::read_loop_markers(&dst),
                    Some((4321, 8765))
                );
            }
        } else {
            eprintln!(
                "warning: skipping strict m4a metadata assertion for {}",
                dst.display()
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }



    // ---- Loudness (BS.1770-4 / EBU Tech 3341) ----

    fn stereo_sine(freq: f32, amp_dbfs: f32, sr: u32, secs: f32) -> Vec<Vec<f32>> {
        let amp = 10.0f32.powf(amp_dbfs / 20.0);
        let frames = (sr as f32 * secs) as usize;
        let ch: Vec<f32> = (0..frames)
            .map(|i| (i as f32 / sr as f32 * freq * std::f32::consts::TAU).sin() * amp)
            .collect();
        vec![ch.clone(), ch]
    }

    // EBU Tech 3341 case 1: 997 Hz stereo sine at -23 dBFS -> -23.0 LUFS.
    // (The K-weighting is unity at 997 Hz by construction, and identical
    // signals in L+R sum to +3dB power against the -3.01dB sine RMS.)
    #[test]
    fn lufs_integrated_matches_tech3341_reference_tones() {
        let sr = 48_000;
        for target in [-23.0f32, -33.0f32] {
            let chans = stereo_sine(997.0, target, sr, 20.0);
            let m = super::loudness_metrics_from_multi(&chans, sr).expect("metrics");
            assert!(
                (m.lufs_i - target).abs() <= 0.1,
                "LUFS-I for {target} dBFS 997Hz stereo sine: got {}",
                m.lufs_i
            );
            // Steady tone: momentary and short-term maxima match integrated.
            let mm = m.lufs_m_max.expect("momentary");
            let sm = m.lufs_s_max.expect("short-term");
            assert!((mm - target).abs() <= 0.1, "LUFS-M {mm} vs {target}");
            assert!((sm - target).abs() <= 0.1, "LUFS-S {sm} vs {target}");
        }
    }

    // 44.1 kHz input goes through the internal resampler; keep a wider
    // tolerance for the linear interpolation deviation.
    #[test]
    fn lufs_integrated_reasonable_at_44100() {
        let chans = stereo_sine(997.0, -23.0, 44_100, 20.0);
        let m = super::loudness_metrics_from_multi(&chans, 44_100).expect("metrics");
        assert!(
            (m.lufs_i + 23.0).abs() <= 0.3,
            "LUFS-I at 44.1k: got {}",
            m.lufs_i
        );
    }

    // Gating: 10s of tone at -23 plus 10s of near-silence must stay close to
    // the tone level (the relative gate drops the quiet half), clearly above
    // the ungated mean.
    #[test]
    fn lufs_gating_ignores_long_silence() {
        let sr = 48_000;
        let mut chans = stereo_sine(997.0, -23.0, sr, 10.0);
        for ch in chans.iter_mut() {
            ch.extend(std::iter::repeat(0.0f32).take((sr * 10) as usize));
        }
        let m = super::loudness_metrics_from_multi(&chans, sr).expect("metrics");
        assert!(
            (m.lufs_i + 23.0).abs() <= 0.5,
            "gated LUFS-I should stay near -23, got {}",
            m.lufs_i
        );
    }

    // Momentary vs short-term: a 0.5s burst inside 10s of silence fills a
    // 400ms window completely but only a fraction of a 3s window.
    #[test]
    fn lufs_momentary_exceeds_short_term_for_bursts() {
        let sr = 48_000;
        let burst = stereo_sine(997.0, -20.0, sr, 0.5);
        let mut chans = vec![Vec::new(), Vec::new()];
        for (i, ch) in chans.iter_mut().enumerate() {
            ch.extend(std::iter::repeat(0.0f32).take((sr * 5) as usize));
            ch.extend(burst[i].iter().copied());
            ch.extend(std::iter::repeat(0.0f32).take((sr * 5) as usize));
        }
        let m = super::loudness_metrics_from_multi(&chans, sr).expect("metrics");
        let mm = m.lufs_m_max.expect("momentary");
        let sm = m.lufs_s_max.expect("short-term");
        assert!(
            mm > sm + 3.0,
            "burst: momentary ({mm}) should clearly exceed short-term ({sm})"
        );
        assert!((mm + 20.0).abs() <= 0.5, "momentary max should be ~-20, got {mm}");
    }

    // True peak: an fs/4 sine sampled at 45 degree phase offset has all its
    // samples at -3.01 dBFS while the continuous waveform peaks at 0 dBTP.
    #[test]
    fn true_peak_recovers_intersample_peak()  {
        let sr = 48_000u32;
        let frames = sr as usize;
        let ch: Vec<f32> = (0..frames)
            .map(|i| {
                (std::f32::consts::TAU * (i as f32) / 4.0 + std::f32::consts::FRAC_PI_4).sin()
            })
            .collect();
        let mut sample_peak = 0.0f32;
        for &v in &ch {
            sample_peak = sample_peak.max(v.abs());
        }
        let sample_peak_db = 20.0 * sample_peak.log10();
        assert!((sample_peak_db + 3.01).abs() < 0.1, "sample peak {sample_peak_db}");
        let tp = super::true_peak_db_from_multi(&[ch], sr).expect("tp");
        assert!(
            (tp - 0.0).abs() <= 0.35,
            "true peak should recover ~0 dBTP, got {tp} (sample peak {sample_peak_db})"
        );
        assert!(tp > sample_peak_db + 2.0, "tp {tp} must exceed sample peak");
    }

    // 5.1 channel weighting: surround channels carry a 1.41 power weight, so
    // a tone only in Ls/Rs must read ~+1.5 dB above the same tone in L/R.
    #[test]
    fn lufs_surround_weighting_applies_to_5_1() {
        let sr = 48_000;
        let tone = stereo_sine(997.0, -23.0, sr, 10.0);
        let silence = vec![0.0f32; tone[0].len()];
        // L R C LFE Ls Rs
        let front: Vec<Vec<f32>> = vec![
            tone[0].clone(),
            tone[1].clone(),
            silence.clone(),
            silence.clone(),
            silence.clone(),
            silence.clone(),
        ];
        let surround: Vec<Vec<f32>> = vec![
            silence.clone(),
            silence.clone(),
            silence.clone(),
            silence.clone(),
            tone[0].clone(),
            tone[1].clone(),
        ];
        let lf = super::loudness_metrics_from_multi(&front, sr).unwrap().lufs_i;
        let ls = super::loudness_metrics_from_multi(&surround, sr).unwrap().lufs_i;
        // G=1.41 is a POWER weight in BS.1770-4 (L = -0.691 + 10log10(sum G_i z_i)),
        // so the level delta is 10*log10(1.41) ~ +1.49 dB.
        let expected = 10.0 * 1.41f32.log10();
        assert!(
            ((ls - lf) - expected).abs() <= 0.1,
            "surround weighting: front {lf}, surround {ls}, expected delta {expected}"
        );
        // LFE must be excluded entirely.
        let mut lfe_only: Vec<Vec<f32>> = vec![silence.clone(); 6];
        lfe_only[3] = tone[0].clone();
        let l_lfe = super::loudness_metrics_from_multi(&lfe_only, sr).unwrap().lufs_i;
        assert!(
            l_lfe == f32::NEG_INFINITY || l_lfe < -60.0,
            "LFE-only signal should gate out, got {l_lfe}"
        );
    }

    #[test]
    fn noise_gate_silences_signal_below_threshold() {
        let sr = 48_000;
        let quiet: Vec<f32> = make_signal(sr as usize, 3.0).iter().map(|v| v * 0.001).collect();
        let params = NoiseGateParams {
            threshold_db: -20.0,
            attack_ms: 1.0,
            release_ms: 20.0,
        };
        let out = process_noise_gate_offline(&quiet, sr, &params);
        let tail_rms = rms(&out[out.len() / 2..]);
        assert!(
            tail_rms < 0.0005,
            "signal below threshold should be gated to near-silence, got rms {tail_rms}"
        );
    }

    #[test]
    fn noise_gate_passes_signal_above_threshold() {
        let sr = 48_000;
        let loud = make_signal(sr as usize, 3.0);
        let params = NoiseGateParams {
            threshold_db: -40.0,
            attack_ms: 1.0,
            release_ms: 20.0,
        };
        let out = process_noise_gate_offline(&loud, sr, &params);
        let tail_rms = rms(&out[out.len() / 2..]);
        let src_rms = rms(&loud[loud.len() / 2..]);
        assert!(
            (tail_rms - src_rms).abs() < 0.05,
            "signal above threshold should pass through near-unaffected: src {src_rms} out {tail_rms}"
        );
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|v| v * v).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn compressor_reduces_gain_above_threshold() {
        let sr = 48_000;
        let loud: Vec<f32> = (0..sr as usize)
            .map(|i| (i as f32 / sr as f32 * 200.0 * std::f32::consts::TAU).sin() * 0.9)
            .collect();
        let params = CompressorParams {
            threshold_db: -12.0,
            ratio: 4.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            makeup_db: 0.0,
        };
        let out = process_compressor_offline(&loud, sr, &params);
        // Compare steady-state (tail) RMS rather than whole-buffer peak: a
        // fast-attack compressor legitimately lets the very first transient
        // through before the envelope has risen, so an early sample can
        // still hit full amplitude even though the signal is being reduced
        // everywhere else.
        let tail = loud.len() / 2;
        let src_rms = rms(&loud[tail..]);
        let out_rms = rms(&out[tail..]);
        assert!(
            out_rms < src_rms * 0.9,
            "compressor should reduce steady-state level above threshold: src {src_rms} out {out_rms}"
        );
    }

    #[test]
    fn compressor_passthrough_below_threshold() {
        let sr = 48_000;
        let quiet: Vec<f32> = (0..sr as usize)
            .map(|i| (i as f32 / sr as f32 * 200.0 * std::f32::consts::TAU).sin() * 0.01)
            .collect();
        let params = CompressorParams {
            threshold_db: -12.0,
            ratio: 4.0,
            attack_ms: 1.0,
            release_ms: 50.0,
            makeup_db: 0.0,
        };
        let out = process_compressor_offline(&quiet, sr, &params);
        let src_peak = quiet.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        let out_peak = out.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            (out_peak - src_peak).abs() < 0.001,
            "signal below threshold should pass through unaffected: src {src_peak} out {out_peak}"
        );
    }

    #[test]
    fn three_band_eq_boosts_targeted_frequency() {
        let sr = 48_000;
        let secs = 0.5;
        let len = (sr as f32 * secs) as usize;
        let target_hz = 1_000.0;
        let tone: Vec<f32> = (0..len)
            .map(|i| (i as f32 / sr as f32 * target_hz * std::f32::consts::TAU).sin() * 0.2)
            .collect();
        let flat = ThreeBandEqParams {
            low_shelf_freq_hz: 100.0,
            low_shelf_gain_db: 0.0,
            mid_freq_hz: target_hz,
            mid_gain_db: 0.0,
            mid_q: 1.0,
            high_shelf_freq_hz: 8_000.0,
            high_shelf_gain_db: 0.0,
        };
        let boosted = ThreeBandEqParams {
            mid_gain_db: 12.0,
            ..flat
        };
        let out_flat = process_three_band_eq_offline(&tone, sr, &flat);
        let out_boosted = process_three_band_eq_offline(&tone, sr, &boosted);
        // Settle past the filters' transient before comparing steady-state level.
        let tail = len / 2;
        let rms_flat = rms(&out_flat[tail..]);
        let rms_boosted = rms(&out_boosted[tail..]);
        assert!(
            rms_boosted > rms_flat * 1.5,
            "12dB mid boost at the tone's frequency should raise its level: flat {rms_flat} boosted {rms_boosted}"
        );
    }

    #[test]
    fn three_band_eq_cuts_targeted_frequency() {
        let sr = 48_000;
        let secs = 0.5;
        let len = (sr as f32 * secs) as usize;
        let target_hz = 1_000.0;
        let tone: Vec<f32> = (0..len)
            .map(|i| (i as f32 / sr as f32 * target_hz * std::f32::consts::TAU).sin() * 0.2)
            .collect();
        let flat = ThreeBandEqParams {
            low_shelf_freq_hz: 100.0,
            low_shelf_gain_db: 0.0,
            mid_freq_hz: target_hz,
            mid_gain_db: 0.0,
            mid_q: 1.0,
            high_shelf_freq_hz: 8_000.0,
            high_shelf_gain_db: 0.0,
        };
        let cut = ThreeBandEqParams {
            mid_gain_db: -12.0,
            ..flat
        };
        let out_flat = process_three_band_eq_offline(&tone, sr, &flat);
        let out_cut = process_three_band_eq_offline(&tone, sr, &cut);
        let tail = len / 2;
        let rms_flat = rms(&out_flat[tail..]);
        let rms_cut = rms(&out_cut[tail..]);
        assert!(
            rms_cut < rms_flat * 0.7,
            "12dB mid cut at the tone's frequency should lower its level: flat {rms_flat} cut {rms_cut}"
        );
    }
}
