use std::fs::File;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

use anyhow::{Context, Result};
use fdk_aac::dec::{Decoder as AacDecoder, DecoderError as AacDecoderError, Transport as AacTransport};
use id3::TagLike;
use mp4::{ChannelConfig, Mp4Reader, TrackType};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};

pub const SUPPORTED_EXTS: &[&str] = &["wav", "mp3", "m4a", "ogg"];

fn io_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("NEOWAVES_IO_TRACE")
            .ok()
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                !(v.is_empty() || v == "0" || v == "false" || v == "off")
            })
            .unwrap_or(false)
    })
}

fn io_trace(
    event: &str,
    path: &Path,
    container: &str,
    codec: &str,
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    frames: Option<usize>,
) {
    if !io_trace_enabled() {
        return;
    }
    let frames_text = frames
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    eprintln!(
        "io_trace event={event} path=\"{}\" container={container} codec={codec} sr={sample_rate} ch={channels} bits={bits_per_sample} frames={frames_text}",
        path.display()
    );
}

#[cfg(debug_assertions)]
fn sanitize_non_finite_mono(path: &Path, stage: &str, samples: &mut [f32]) {
    let mut replaced = 0usize;
    for v in samples.iter_mut() {
        if !v.is_finite() {
            *v = 0.0;
            replaced += 1;
        }
    }
    if replaced > 0 {
        eprintln!(
            "io_pcm_sanitize stage={stage} path=\"{}\" replaced_non_finite={replaced}",
            path.display()
        );
    }
}

#[cfg(debug_assertions)]
fn sanitize_non_finite_multi(path: &Path, stage: &str, channels: &mut [Vec<f32>]) {
    let mut replaced = 0usize;
    for ch in channels.iter_mut() {
        for v in ch.iter_mut() {
            if !v.is_finite() {
                *v = 0.0;
                replaced += 1;
            }
        }
    }
    if replaced > 0 {
        eprintln!(
            "io_pcm_sanitize stage={stage} path=\"{}\" replaced_non_finite={replaced}",
            path.display()
        );
    }
}

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

fn is_m4a_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("m4a"))
        .unwrap_or(false)
}

fn channel_config_count(cfg: ChannelConfig) -> u16 {
    match cfg {
        ChannelConfig::Mono => 1,
        ChannelConfig::Stereo => 2,
        ChannelConfig::Three => 3,
        ChannelConfig::Four => 4,
        ChannelConfig::Five => 5,
        ChannelConfig::FiveOne => 6,
        ChannelConfig::SevenOne => 8,
    }
}

fn read_audio_info_m4a_mp4(
    path: &Path,
    created_at: Option<SystemTime>,
    modified_at: Option<SystemTime>,
    file_size: Option<u64>,
) -> Result<AudioInfo> {
    let file = File::open(path).with_context(|| format!("open m4a: {}", path.display()))?;
    let size = file_size.unwrap_or_else(|| file.metadata().map(|m| m.len()).unwrap_or(0));
    let reader = Mp4Reader::read_header(file, size)
        .map_err(|e| anyhow::anyhow!("mp4 header: {e:?}"))?;
    let mut picked = None;
    for (&track_id, track) in reader.tracks() {
        if let Ok(TrackType::Audio) = track.track_type() {
            picked = Some((track_id, track));
            break;
        }
    }
    let (_, track) = picked.context("m4a: no audio track")?;
    let sample_rate = track
        .sample_freq_index()
        .map(|idx| idx.freq())
        .unwrap_or_else(|_| track.timescale())
        .max(1);
    let channels = track
        .channel_config()
        .map(channel_config_count)
        .or_else(|_| {
            track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .mp4a
                .as_ref()
                .map(|m| m.channelcount)
                .ok_or_else(|| anyhow::anyhow!("m4a: channel config missing"))
        })
        .unwrap_or(2);
    let duration_secs = track.duration().as_secs_f32();
    let bit_rate_bps = if duration_secs.is_finite() && duration_secs > 0.0 {
        file_size
            .map(|bytes| ((bytes as f64) * 8.0 / duration_secs as f64).round() as u32)
            .filter(|v| *v > 0)
    } else {
        None
    };
    Ok(AudioInfo {
        channels,
        sample_rate,
        bits_per_sample: 16,
        bit_rate_bps,
        duration_secs: Some(duration_secs),
        created_at,
        modified_at,
    })
}

fn audio_specific_config_bytes(profile: u8, freq_index: u8, chan_conf: u8) -> [u8; 2] {
    let byte_a = (profile << 3) | (freq_index >> 1);
    let byte_b = (freq_index << 7) | (chan_conf << 3);
    [byte_a, byte_b]
}

fn decode_m4a_fdk(path: &Path, max_secs: Option<f32>) -> Result<(Vec<Vec<f32>>, u32, bool)> {
    let file = File::open(path).with_context(|| format!("open m4a: {}", path.display()))?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader =
        Mp4Reader::read_header(file, size).map_err(|e| anyhow::anyhow!("mp4 header: {e:?}"))?;
    let mut picked = None;
    for (&track_id, track) in reader.tracks() {
        if let Ok(TrackType::Audio) = track.track_type() {
            picked = Some((track_id, track));
            break;
        }
    }
    let (track_id, track) = picked.context("m4a: no audio track")?;
    let sample_rate = track
        .sample_freq_index()
        .map(|idx| idx.freq())
        .unwrap_or_else(|_| track.timescale())
        .max(1);
    let channel_cfg = track.channel_config().unwrap_or(ChannelConfig::Stereo);
    let channels = channel_config_count(channel_cfg).max(1) as usize;
    let profile = track
        .trak
        .mdia
        .minf
        .stbl
        .stsd
        .mp4a
        .as_ref()
        .and_then(|m| m.esds.as_ref())
        .map(|esds| esds.es_desc.dec_config.dec_specific.profile)
        .unwrap_or(2);
    let freq_index = track
        .sample_freq_index()
        .map(|idx| idx as u8)
        .unwrap_or(mp4::SampleFreqIndex::Freq44100 as u8);
    let chan_conf = channel_cfg as u8;
    let asc = audio_specific_config_bytes(profile, freq_index, chan_conf);
    let mut decoder = AacDecoder::new(AacTransport::Raw);
    decoder
        .config_raw(&asc)
        .map_err(|e| anyhow::anyhow!("m4a decoder config: {e}"))?;
    let sample_count = reader.sample_count(track_id)?;
    let mut chans: Vec<Vec<f32>> = vec![Vec::new(); channels];
    let max_frames = max_secs
        .and_then(|s| {
            if s <= 0.0 {
                None
            } else {
                Some(((sample_rate as f32) * s).ceil() as usize)
            }
        })
        .filter(|v| *v > 0);
    let mut reached_eof = true;
    for sample_id in 1..=sample_count {
        let Some(sample) = reader.read_sample(track_id, sample_id)? else {
            continue;
        };
        let mut offset = 0usize;
        while offset < sample.bytes.len() {
            let used = decoder
                .fill(&sample.bytes[offset..])
                .map_err(|e| anyhow::anyhow!("m4a decoder fill: {e}"))?;
            if used == 0 {
                break;
            }
            offset += used;
            let mut frame_size = decoder.decoded_frame_size();
            if frame_size == 0 {
                frame_size = 2048 * channels;
            }
            let mut pcm = vec![0i16; frame_size];
            match decoder.decode_frame(&mut pcm) {
                Ok(()) => {
                    let info = decoder.stream_info();
                    let ch = info.numChannels.max(1) as usize;
                    let frames = info.frameSize as usize;
                    let needed = ch.saturating_mul(frames);
                    if needed == 0 || pcm.len() < needed {
                        continue;
                    }
                    if chans.len() != ch {
                        chans = vec![Vec::new(); ch];
                    }
                    for i in 0..frames {
                        for c in 0..ch {
                            let v = pcm[i * ch + c] as f32 / i16::MAX as f32;
                            chans[c].push(v);
                        }
                    }
                    if let Some(limit) = max_frames {
                        if chans[0].len() >= limit {
                            reached_eof = false;
                            for ch in &mut chans {
                                ch.truncate(limit);
                            }
                            return Ok((chans, sample_rate, reached_eof));
                        }
                    }
                }
                Err(err) => {
                    if err == AacDecoderError::NOT_ENOUGH_BITS
                        || err == AacDecoderError::TRANSPORT_SYNC_ERROR
                    {
                        break;
                    }
                    return Err(anyhow::anyhow!("m4a decode: {err}"));
                }
            }
        }
    }
    Ok((chans, sample_rate, reached_eof))
}

fn mixdown_to_mono(chans: &[Vec<f32>]) -> Vec<f32> {
    if chans.is_empty() {
        return Vec::new();
    }
    let frames = chans.iter().map(|c| c.len()).min().unwrap_or(0);
    let mut mono = Vec::with_capacity(frames);
    for i in 0..frames {
        let mut acc = 0.0f32;
        let mut c = 0usize;
        for ch in chans {
            if let Some(&v) = ch.get(i) {
                acc += v;
                c += 1;
            }
        }
        mono.push(if c > 0 { acc / (c as f32) } else { 0.0 });
    }
    mono
}

pub fn read_audio_info(path: &Path) -> Result<AudioInfo> {
    let metadata = std::fs::metadata(path).ok();
    let created_at = metadata.as_ref().and_then(|m| m.created().ok());
    let modified_at = metadata.as_ref().and_then(|m| m.modified().ok());
    let file_size = metadata.as_ref().map(|m| m.len());
    let ext_hint = path.extension().and_then(|s| s.to_str());
    let probe_once = |hint_ext: Option<&str>| -> Result<_> {
        let file = File::open(path).with_context(|| format!("open audio: {}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = hint_ext {
            hint.with_extension(ext);
        }
        get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(Into::into)
    };
    let probed = match probe_once(ext_hint) {
        Ok(v) => v,
        Err(first_err) => {
            if is_m4a_path(path) {
                if let Ok(info) = read_audio_info_m4a_mp4(path, created_at, modified_at, file_size)
                {
                    return Ok(info);
                }
            }
            if ext_hint.is_some() {
                match probe_once(None) {
                    Ok(v) => v,
                    Err(err) => {
                        if is_m4a_path(path) {
                            if let Ok(info) =
                                read_audio_info_m4a_mp4(path, created_at, modified_at, file_size)
                            {
                                return Ok(info);
                            }
                        }
                        return Err(err).with_context(|| {
                            format!(
                                "probe audio failed with and without hint: {}",
                                path.display()
                            )
                        });
                    }
                }
            } else {
                return Err(first_err);
            }
        }
    };
    let format = probed.format;
    let track = format.default_track().context("no default track")?;
    let cp = &track.codec_params;
    let codec_name = format!("{:?}", cp.codec);
    let mut channels = cp.channels.map(|c| c.count() as u16).unwrap_or(0);
    let mut sample_rate = cp.sample_rate.unwrap_or(0);
    let mut bits_per_sample = cp.bits_per_sample.unwrap_or(0) as u16;
    let duration_secs = match (cp.time_base, cp.n_frames) {
        (Some(tb), Some(n)) => {
            let secs = (n as f64) * (tb.numer as f64) / (tb.denom as f64);
            Some(secs as f32)
        }
        _ => None,
    };
    if channels == 0 || sample_rate == 0 || bits_per_sample == 0 {
        if let Some((head_channels, head_sr, head_bits)) = decode_audio_head_info(path) {
            if channels == 0 {
                channels = head_channels;
            }
            if sample_rate == 0 {
                sample_rate = head_sr;
            }
            if bits_per_sample == 0 {
                bits_per_sample = head_bits;
            }
        }
    }
    let mut bit_rate_bps = None;
    if let (Some(secs), Some(bytes)) = (duration_secs, file_size) {
        if secs.is_finite() && secs > 0.0 {
            let bps = ((bytes as f64) * 8.0 / secs as f64).round();
            if bps.is_finite() && bps > 0.0 {
                bit_rate_bps = Some(bps as u32);
            }
        }
    }
    io_trace(
        "probe",
        path,
        ext_hint.unwrap_or("-"),
        &codec_name,
        sample_rate,
        channels,
        bits_per_sample,
        cp.n_frames.map(|v| v as usize),
    );
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

fn decoded_bits_per_sample(decoded: AudioBufferRef<'_>) -> u16 {
    match decoded {
        AudioBufferRef::U8(_) | AudioBufferRef::S8(_) => 8,
        AudioBufferRef::U16(_) | AudioBufferRef::S16(_) => 16,
        AudioBufferRef::U24(_) | AudioBufferRef::S24(_) => 24,
        AudioBufferRef::U32(_) | AudioBufferRef::S32(_) | AudioBufferRef::F32(_) => 32,
        AudioBufferRef::F64(_) => 64,
    }
}

fn decode_audio_head_info(path: &Path) -> Option<(u16, u32, u16)> {
    let (mut format, mut decoder, track_id, mut sample_rate_hint) = open_decoder(path).ok()?;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(_)) => return None,
            Err(SymphoniaError::ResetRequired) => return None,
            Err(_) => continue,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(_) => return None,
        };
        let spec = decoded.spec();
        if sample_rate_hint == 0 {
            sample_rate_hint = spec.rate;
        }
        let channels = spec.channels.count().max(1) as u16;
        let sample_rate = sample_rate_hint.max(1);
        let bits_per_sample = decoded_bits_per_sample(decoded).max(16);
        return Some((channels, sample_rate, bits_per_sample));
    }
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
    let ext_hint = path.extension().and_then(|s| s.to_str());
    let probe_once = |hint_ext: Option<&str>| -> Result<_> {
        let file = File::open(path).with_context(|| format!("open audio: {}", path.display()))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = hint_ext {
            hint.with_extension(ext);
        }
        get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .map_err(Into::into)
    };
    let probed = match probe_once(ext_hint) {
        Ok(v) => v,
        Err(first_err) => {
            if ext_hint.is_some() {
                probe_once(None).with_context(|| {
                    format!(
                        "open decoder probe failed with and without hint: {}",
                        path.display()
                    )
                })?
            } else {
                return Err(first_err);
            }
        }
    };
    let format = probed.format;
    let track = format.default_track().context("no default track")?.clone();
    let decoder = get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let sample_rate_hint = track.codec_params.sample_rate.unwrap_or(0);
    Ok((format, decoder, track.id, sample_rate_hint))
}

pub fn decode_audio_mono(path: &Path) -> Result<(Vec<f32>, u32)> {
    if is_m4a_path(path) {
        let (chans, sr) = decode_audio_multi(path)?;
        let mono = mixdown_to_mono(&chans);
        io_trace(
            "decode_mono_m4a",
            path,
            path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
            "aac",
            sr,
            1,
            16,
            Some(mono.len()),
        );
        return Ok((mono, sr));
    }
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut mono: Vec<f32> = Vec::new();
    let mut decode_errors = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors = decode_errors.saturating_add(1);
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
    #[cfg(debug_assertions)]
    sanitize_non_finite_mono(path, "decode_mono", &mut mono);
    io_trace(
        "decode_mono",
        path,
        path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
        "-",
        sample_rate,
        1,
        32,
        Some(mono.len()),
    );
    Ok((mono, sample_rate))
}

pub fn decode_audio_mono_prefix(path: &Path, max_secs: f32) -> Result<(Vec<f32>, u32, bool)> {
    if is_m4a_path(path) {
        let max = if max_secs <= 0.0 { None } else { Some(max_secs) };
        let (chans, sr, reached_eof) = decode_m4a_fdk(path, max)?;
        let mono = mixdown_to_mono(&chans);
        io_trace(
            "decode_mono_prefix_m4a",
            path,
            path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
            "aac",
            sr,
            1,
            16,
            Some(mono.len()),
        );
        return Ok((mono, sr, reached_eof));
    }
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
                decode_errors = decode_errors.saturating_add(1);
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
                    #[cfg(debug_assertions)]
                    sanitize_non_finite_mono(path, "decode_mono_prefix", &mut mono);
                    io_trace(
                        "decode_mono_prefix",
                        path,
                        path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
                        "-",
                        sample_rate,
                        1,
                        32,
                        Some(mono.len()),
                    );
                    return Ok((mono, sample_rate, !reached_eof));
                }
            }
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    #[cfg(debug_assertions)]
    sanitize_non_finite_mono(path, "decode_mono_prefix", &mut mono);
    io_trace(
        "decode_mono_prefix",
        path,
        path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
        "-",
        sample_rate,
        1,
        32,
        Some(mono.len()),
    );
    Ok((mono, sample_rate, !reached_eof))
}

pub fn decode_audio_multi(path: &Path) -> Result<(Vec<Vec<f32>>, u32)> {
    if is_m4a_path(path) {
        let (mut chans, sr, _) = decode_m4a_fdk(path, None)?;
        #[cfg(debug_assertions)]
        sanitize_non_finite_multi(path, "decode_multi_m4a", &mut chans);
        io_trace(
            "decode_multi_m4a",
            path,
            path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
            "aac",
            sr,
            chans.len() as u16,
            16,
            chans.get(0).map(|c| c.len()),
        );
        return Ok((chans, sr));
    }
    let res = (|| {
        let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
        let mut chans: Vec<Vec<f32>> = Vec::new();
        let mut decode_errors = 0u32;
        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::DecodeError(_)) => {
                    decode_errors = decode_errors.saturating_add(1);
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
        #[cfg(debug_assertions)]
        sanitize_non_finite_multi(path, "decode_multi", &mut chans);
        io_trace(
            "decode_multi",
            path,
            path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
            "-",
            sample_rate,
            chans.len() as u16,
            32,
            chans.get(0).map(|c| c.len()),
        );
        Ok((chans, sample_rate))
    })();
    if res.is_err() && is_m4a_path(path) {
        if let Ok((mut chans, sr, _)) = decode_m4a_fdk(path, None) {
            #[cfg(debug_assertions)]
            sanitize_non_finite_multi(path, "decode_multi_m4a", &mut chans);
            io_trace(
                "decode_multi_m4a",
                path,
                path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
                "aac",
                sr,
                chans.len() as u16,
                16,
                chans.get(0).map(|c| c.len()),
            );
            return Ok((chans, sr));
        }
    }
    res
}

pub fn decode_audio_multi_prefix(path: &Path, max_secs: f32) -> Result<(Vec<Vec<f32>>, u32, bool)> {
    if is_m4a_path(path) {
        let max = if max_secs <= 0.0 { None } else { Some(max_secs) };
        let (chans, sr, reached_eof) = decode_m4a_fdk(path, max)?;
        return Ok((chans, sr, reached_eof));
    }
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
                decode_errors = decode_errors.saturating_add(1);
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
                    #[cfg(debug_assertions)]
                    sanitize_non_finite_multi(path, "decode_multi_prefix", &mut chans);
                    io_trace(
                        "decode_multi_prefix",
                        path,
                        path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
                        "-",
                        sample_rate,
                        chans.len() as u16,
                        32,
                        chans.get(0).map(|c| c.len()),
                    );
                    return Ok((chans, sample_rate, !reached_eof));
                }
            }
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    #[cfg(debug_assertions)]
    sanitize_non_finite_multi(path, "decode_multi_prefix", &mut chans);
    io_trace(
        "decode_multi_prefix",
        path,
        path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
        "-",
        sample_rate,
        chans.len() as u16,
        32,
        chans.get(0).map(|c| c.len()),
    );
    Ok((chans, sample_rate, !reached_eof))
}

pub fn decode_audio_multi_progressive<C, F>(
    path: &Path,
    prefix_secs: f32,
    emit_every_secs: f32,
    mut should_cancel: C,
    mut on_chunk: F,
) -> Result<()>
where
    C: FnMut() -> bool,
    F: FnMut(Vec<Vec<f32>>, u32, bool) -> bool,
{
    if is_m4a_path(path) {
        let (chans, sr, _) = decode_m4a_fdk(path, None)?;
        let frames = chans.get(0).map(|c| c.len()).unwrap_or(0);
        if frames == 0 {
            return Ok(());
        }
        if prefix_secs > 0.0 {
            let target = ((sr as f32) * prefix_secs).ceil() as usize;
            let prefix_frames = target.max(1).min(frames);
            let mut prefix = Vec::with_capacity(chans.len());
            for ch in &chans {
                prefix.push(ch[..prefix_frames].to_vec());
            }
            let is_final = prefix_frames >= frames;
            if !on_chunk(prefix, sr, is_final) {
                return Ok(());
            }
            if is_final {
                return Ok(());
            }
        }
        let _ = emit_every_secs;
        let _ = should_cancel;
        let _ = on_chunk(chans, sr, true);
        return Ok(());
    }
    let wants_prefix = prefix_secs > 0.0;
    let wants_emit = emit_every_secs > 0.0;
    if !wants_prefix && !wants_emit {
        let (chans, sr) = decode_audio_multi(path)?;
        let _ = on_chunk(chans, sr, true);
        return Ok(());
    }
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut chans: Vec<Vec<f32>> = Vec::new();
    let mut prefix_frames: Option<usize> = None;
    let mut emit_frames: Option<usize> = None;
    let mut next_emit_frames: Option<usize> = None;
    let mut frames_read: usize = 0;
    let mut prefix_sent = false;
    if sample_rate > 0 {
        if wants_prefix {
            let target = ((sample_rate as f32) * prefix_secs).ceil() as usize;
            prefix_frames = Some(target.max(1));
            next_emit_frames = prefix_frames;
        }
        if wants_emit {
            let target = ((sample_rate as f32) * emit_every_secs).ceil() as usize;
            emit_frames = Some(target.max(1));
            if next_emit_frames.is_none() {
                next_emit_frames = emit_frames;
            }
        }
    }
    loop {
        if should_cancel() {
            return Ok(());
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => continue,
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
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(err.into()),
        };
        if sample_rate == 0 {
            sample_rate = decoded.spec().rate;
            if sample_rate == 0 {
                anyhow::bail!("unknown sample rate: {}", path.display());
            }
            if wants_prefix {
                let target = ((sample_rate as f32) * prefix_secs).ceil() as usize;
                prefix_frames = Some(target.max(1));
                next_emit_frames = prefix_frames;
            }
            if wants_emit {
                let target = ((sample_rate as f32) * emit_every_secs).ceil() as usize;
                emit_frames = Some(target.max(1));
                if next_emit_frames.is_none() {
                    next_emit_frames = emit_frames;
                }
            }
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
            frames_read += 1;
        }
        if let Some(threshold) = next_emit_frames {
            if frames_read >= threshold {
                let is_prefix = !prefix_sent && prefix_frames.is_some();
                #[cfg(debug_assertions)]
                let mut chunk = chans.clone();
                #[cfg(not(debug_assertions))]
                let chunk = chans.clone();
                #[cfg(debug_assertions)]
                sanitize_non_finite_multi(path, "decode_multi_progressive_chunk", &mut chunk);
                io_trace(
                    "decode_multi_progressive_chunk",
                    path,
                    path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
                    "-",
                    sample_rate,
                    chunk.len() as u16,
                    32,
                    chunk.get(0).map(|c| c.len()),
                );
                if !on_chunk(chunk, sample_rate, false) {
                    return Ok(());
                }
                if is_prefix {
                    prefix_sent = true;
                }
                if let Some(step) = emit_frames {
                    next_emit_frames = Some(frames_read.saturating_add(step));
                } else {
                    next_emit_frames = None;
                }
            }
        }
    }
    if sample_rate == 0 {
        anyhow::bail!("unknown sample rate: {}", path.display());
    }
    if should_cancel() {
        return Ok(());
    }
    #[cfg(debug_assertions)]
    let mut final_chunk = chans;
    #[cfg(not(debug_assertions))]
    let final_chunk = chans;
    #[cfg(debug_assertions)]
    sanitize_non_finite_multi(path, "decode_multi_progressive_final", &mut final_chunk);
    io_trace(
        "decode_multi_progressive_final",
        path,
        path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
        "-",
        sample_rate,
        final_chunk.len() as u16,
        32,
        final_chunk.get(0).map(|c| c.len()),
    );
    let _ = on_chunk(final_chunk, sample_rate, true);
    Ok(())
}

pub fn decode_audio_mono_prefix_with_errors(
    path: &Path,
    max_secs: f32,
) -> Result<(Vec<f32>, u32, bool, u32)> {
    if is_m4a_path(path) {
        let max = if max_secs <= 0.0 { None } else { Some(max_secs) };
        let (chans, sr, reached_eof) = decode_m4a_fdk(path, max)?;
        let mono = mixdown_to_mono(&chans);
        return Ok((mono, sr, reached_eof, 0));
    }
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
                decode_errors = decode_errors.saturating_add(1);
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
                decode_errors = decode_errors.saturating_add(1);
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
    if is_m4a_path(path) {
        let (mut chans, sr, _) = decode_m4a_fdk(path, None)?;
        #[cfg(debug_assertions)]
        sanitize_non_finite_multi(path, "decode_multi_m4a", &mut chans);
        io_trace(
            "decode_multi_m4a",
            path,
            path.extension().and_then(|s| s.to_str()).unwrap_or("-"),
            "aac",
            sr,
            chans.len() as u16,
            16,
            chans.get(0).map(|c| c.len()),
        );
        return Ok((chans, sr, 0));
    }
    let (mut format, mut decoder, track_id, mut sample_rate) = open_decoder(path)?;
    let mut chans: Vec<Vec<f32>> = Vec::new();
    let mut decode_errors = 0u32;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::DecodeError(_)) => {
                decode_errors = decode_errors.saturating_add(1);
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
