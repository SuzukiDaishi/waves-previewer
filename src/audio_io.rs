use std::fs::File;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};
use id3::TagLike;

pub const SUPPORTED_EXTS: &[&str] = &["wav", "mp3", "m4a", "ogg"];

#[derive(Clone, Copy, Debug)]
pub struct AudioInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub bit_rate_bps: Option<u32>,
    pub duration_secs: Option<f32>,
    pub created_at: Option<SystemTime>,
    pub modified_at: Option<SystemTime>,
}

pub fn is_supported_extension(ext: &str) -> bool {
    SUPPORTED_EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e))
}

pub fn is_supported_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|ext| is_supported_extension(ext))
        .unwrap_or(false)
}

pub fn read_audio_info(path: &Path) -> Result<AudioInfo> {
    let metadata = std::fs::metadata(path).ok();
    let created_at = metadata.as_ref().and_then(|m| m.created().ok());
    let modified_at = metadata.as_ref().and_then(|m| m.modified().ok());
    let file_size = metadata.as_ref().map(|m| m.len());
    let file = File::open(path).with_context(|| format!("open audio: {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    let probed = get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let format = probed.format;
    let track = format
        .default_track()
        .context("no default track")?;
    let cp = &track.codec_params;
    let channels = cp.channels.map(|c| c.count() as u16).unwrap_or(0);
    let sample_rate = cp.sample_rate.unwrap_or(0);
    let bits_per_sample = cp.bits_per_sample.unwrap_or(0) as u16;
    let duration_secs = match (cp.time_base, cp.n_frames) {
        (Some(tb), Some(n)) => {
            let secs = (n as f64) * (tb.numer as f64) / (tb.denom as f64);
            Some(secs as f32)
        }
        _ => None,
    };
    let mut bit_rate_bps = None;
    if let (Some(secs), Some(bytes)) = (duration_secs, file_size) {
        if secs.is_finite() && secs > 0.0 {
            let bps = ((bytes as f64) * 8.0 / secs as f64).round();
            if bps.is_finite() && bps > 0.0 {
                bit_rate_bps = Some(bps as u32);
            }
        }
    }
    Ok(AudioInfo {
        channels,
        sample_rate,
        bits_per_sample,
        bit_rate_bps,
        duration_secs,
        created_at,
        modified_at,
    })
}

pub fn read_audio_bpm(path: &Path) -> Option<f32> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "m4a" => read_bpm_m4a(path),
        "mp3" => read_bpm_id3(path),
        "wav" => read_bpm_wav(path),
        _ => None,
    }
}

fn parse_bpm_text(text: &str) -> Option<f32> {
    let mut buf = String::new();
    let mut started = false;
    for ch in text.trim().chars() {
        if ch.is_ascii_digit() || ch == '.' {
            buf.push(ch);
            started = true;
        } else if started {
            break;
        }
    }
    if buf.is_empty() {
        return None;
    }
    let v: f32 = buf.parse().ok()?;
    if v.is_finite() && v > 0.0 {
        Some(v)
    } else {
        None
    }
}

fn read_bpm_id3(path: &Path) -> Option<f32> {
    let tag = id3::Tag::read_from_path(path).ok()?;
    let text = tag
        .get("TBPM")
        .and_then(|f| f.content().text())
        .or_else(|| tag.get("TBP").and_then(|f| f.content().text()));
    text.and_then(parse_bpm_text)
}

fn read_bpm_m4a(path: &Path) -> Option<f32> {
    let tag = mp4ameta::Tag::read_from_path(path).ok()?;
    tag.bpm().map(|v| v as f32)
}

fn read_bpm_wav(path: &Path) -> Option<f32> {
    if let Some(bpm) = read_bpm_wav_acid(path) {
        return Some(bpm);
    }
    read_bpm_id3(path)
}

fn read_bpm_wav_acid(path: &Path) -> Option<f32> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).ok()?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return None;
    }
    loop {
        let mut chunk_header = [0u8; 8];
        if file.read_exact(&mut chunk_header).is_err() {
            break;
        }
        let id = &chunk_header[0..4];
        let size = u32::from_le_bytes([
            chunk_header[4],
            chunk_header[5],
            chunk_header[6],
            chunk_header[7],
        ]) as u64;
        if id == b"acid" || id == b"ACID" {
            let read_len = size.min(64) as usize;
            let mut buf = vec![0u8; read_len];
            if file.read_exact(&mut buf).is_err() {
                return None;
            }
            if size > read_len as u64 {
                let _ = file.seek(SeekFrom::Current((size - read_len as u64) as i64));
            }
            if buf.len() >= 24 {
                let tempo_raw = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
                let mut candidates = Vec::new();
                candidates.push(tempo_raw as f32);
                candidates.push((tempo_raw as f32) / 100.0);
                let tempo_f = f32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
                candidates.push(tempo_f);
                for bpm in candidates {
                    if bpm.is_finite() && bpm >= 20.0 && bpm <= 400.0 {
                        return Some(bpm);
                    }
                }
            }
            return None;
        }
        let skip = size + (size & 1);
        if file.seek(SeekFrom::Current(skip as i64)).is_err() {
            break;
        }
    }
    None
}

fn open_decoder(
    path: &Path,
) -> Result<(
    Box<dyn symphonia::core::formats::FormatReader>,
    Box<dyn symphonia::core::codecs::Decoder>,
    u32,
    u32,
)> {
    let file = File::open(path).with_context(|| format!("open audio: {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }
    let probed = get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let format = probed.format;
    let track = format
        .default_track()
        .context("no default track")?
        .clone();
    let decoder = get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let sample_rate_hint = track.codec_params.sample_rate.unwrap_or(0);
    Ok((format, decoder, track.id, sample_rate_hint))
}

pub fn decode_audio_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut mono: Vec<f32> = Vec::new();
    let mut decode_errors = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                if decode_errors > 8 {
                    break;
                }
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(err.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(err.into()),
        };
        if sample_rate == 0 {
            sample_rate = decoded.spec().rate;
        }
        let channels = decoded.spec().channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            let mut acc = 0.0f32;
            for &v in frame {
                acc += v;
            }
            mono.push(acc / channels as f32);
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    Ok((mono, sample_rate))
}

pub fn decode_audio_mono_prefix(path: &Path, max_secs: f32) -> Result<(Vec<f32>, u32, bool)> {
    if max_secs <= 0.0 {
        let (mono, sr) = decode_audio_mono(path)?;
        return Ok((mono, sr, false));
    }
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut mono: Vec<f32> = Vec::new();
    let mut max_frames: Option<usize> = None;
    let mut frames_read: usize = 0;
    let mut reached_eof = false;
    let mut decode_errors = 0u32;
    if sample_rate > 0 {
        let target = ((sample_rate as f32) * max_secs).ceil() as usize;
        max_frames = Some(target.max(1));
        mono.reserve(target.max(1));
    }
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                if decode_errors > 8 {
                    break;
                }
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                reached_eof = true;
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(err.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                reached_eof = true;
                break;
            }
            Err(err) => return Err(err.into()),
        };
        if sample_rate == 0 {
            sample_rate = decoded.spec().rate;
            if sample_rate == 0 {
                anyhow::bail!("unknown sample rate: {}", path.display());
            }
            let target = ((sample_rate as f32) * max_secs).ceil() as usize;
            max_frames = Some(target.max(1));
            mono.reserve(target.max(1));
        }
        let channels = decoded.spec().channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            let mut acc = 0.0f32;
            for &v in frame {
                acc += v;
            }
            mono.push(acc / channels as f32);
            frames_read += 1;
            if let Some(limit) = max_frames {
                if frames_read >= limit {
                    return Ok((mono, sample_rate, !reached_eof));
                }
            }
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    Ok((mono, sample_rate, !reached_eof))
}

pub fn decode_audio_multi(path: &Path) -> Result<(Vec<Vec<f32>>, u32)> {
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut chans: Vec<Vec<f32>> = Vec::new();
    let mut decode_errors = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                if decode_errors > 8 {
                    break;
                }
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(err.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(err.into()),
        };
        if sample_rate == 0 {
            sample_rate = decoded.spec().rate;
        }
        let channels = decoded.spec().channels.count().max(1);
        if chans.is_empty() {
            chans = vec![Vec::new(); channels];
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            for (ci, &v) in frame.iter().enumerate() {
                chans[ci].push(v);
            }
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    Ok((chans, sample_rate))
}

pub fn decode_audio_multi_prefix(path: &Path, max_secs: f32) -> Result<(Vec<Vec<f32>>, u32, bool)> {
    if max_secs <= 0.0 {
        let (chans, sr) = decode_audio_multi(path)?;
        return Ok((chans, sr, false));
    }
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut chans: Vec<Vec<f32>> = Vec::new();
    let mut max_frames: Option<usize> = None;
    let mut frames_read: usize = 0;
    let mut reached_eof = false;
    let mut decode_errors = 0u32;
    if sample_rate > 0 {
        let target = ((sample_rate as f32) * max_secs).ceil() as usize;
        max_frames = Some(target.max(1));
    }
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                if decode_errors > 8 {
                    break;
                }
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                reached_eof = true;
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(err.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                reached_eof = true;
                break;
            }
            Err(err) => return Err(err.into()),
        };
        if sample_rate == 0 {
            sample_rate = decoded.spec().rate;
            if sample_rate == 0 {
                anyhow::bail!("unknown sample rate: {}", path.display());
            }
            let target = ((sample_rate as f32) * max_secs).ceil() as usize;
            max_frames = Some(target.max(1));
        }
        let channels = decoded.spec().channels.count().max(1);
        if chans.is_empty() {
            chans = vec![Vec::new(); channels];
            if let Some(limit) = max_frames {
                for ch in chans.iter_mut() {
                    ch.reserve(limit.max(1));
                }
            }
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            for (ci, &v) in frame.iter().enumerate() {
                chans[ci].push(v);
            }
            frames_read += 1;
            if let Some(limit) = max_frames {
                if frames_read >= limit {
                    return Ok((chans, sample_rate, !reached_eof));
                }
            }
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    Ok((chans, sample_rate, !reached_eof))
}

pub fn decode_audio_mono_prefix_with_errors(
    path: &Path,
    max_secs: f32,
) -> Result<(Vec<f32>, u32, bool, u32)> {
    if max_secs <= 0.0 {
        let (mono, sr, err) = decode_audio_mono_with_errors(path)?;
        return Ok((mono, sr, false, err));
    }
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut mono: Vec<f32> = Vec::new();
    let mut max_frames: Option<usize> = None;
    let mut frames_read: usize = 0;
    let mut reached_eof = false;
    let mut decode_errors = 0u32;
    if sample_rate > 0 {
        let target = ((sample_rate as f32) * max_secs).ceil() as usize;
        max_frames = Some(target.max(1));
        mono.reserve(target.max(1));
    }
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                if decode_errors > 64 {
                    break;
                }
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                reached_eof = true;
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(err.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                reached_eof = true;
                break;
            }
            Err(err) => return Err(err.into()),
        };
        if sample_rate == 0 {
            sample_rate = decoded.spec().rate;
            if sample_rate == 0 {
                anyhow::bail!("unknown sample rate: {}", path.display());
            }
            let target = ((sample_rate as f32) * max_secs).ceil() as usize;
            max_frames = Some(target.max(1));
            mono.reserve(target.max(1));
        }
        let channels = decoded.spec().channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            let mut acc = 0.0f32;
            for &v in frame {
                acc += v;
            }
            mono.push(acc / channels as f32);
            frames_read += 1;
            if let Some(limit) = max_frames {
                if frames_read >= limit {
                    return Ok((mono, sample_rate, !reached_eof, decode_errors));
                }
            }
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    Ok((mono, sample_rate, !reached_eof, decode_errors))
}

pub fn decode_audio_mono_with_errors(path: &Path) -> Result<(Vec<f32>, u32, u32)> {
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut mono: Vec<f32> = Vec::new();
    let mut decode_errors = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                if decode_errors > 64 {
                    break;
                }
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(err.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(err.into()),
        };
        if sample_rate == 0 {
            sample_rate = decoded.spec().rate;
        }
        let channels = decoded.spec().channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            let mut acc = 0.0f32;
            for &v in frame {
                acc += v;
            }
            mono.push(acc / channels as f32);
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    Ok((mono, sample_rate, decode_errors))
}

pub fn decode_audio_multi_with_errors(path: &Path) -> Result<(Vec<Vec<f32>>, u32, u32)> {
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut chans: Vec<Vec<f32>> = Vec::new();
    let mut decode_errors = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                if decode_errors > 64 {
                    break;
                }
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(err) => return Err(err.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors += 1;
                continue;
            }
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(err.into()),
        };
        if sample_rate == 0 {
            sample_rate = decoded.spec().rate;
        }
        let channels = decoded.spec().channels.count().max(1);
        if chans.is_empty() {
            chans = vec![Vec::new(); channels];
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks(channels) {
            for (ci, &v) in frame.iter().enumerate() {
                chans[ci].push(v);
            }
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    Ok((chans, sample_rate, decode_errors))
}
