use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwapOption;
use atomic_float::{AtomicF32, AtomicF64};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use memmap2::Mmap;

#[derive(Debug)]
pub struct AudioBuffer {
    pub channels: Vec<Vec<f32>>, // per-channel samples in [-1, 1]
}

impl AudioBuffer {
    pub fn from_mono(mono: Vec<f32>) -> Self {
        Self {
            channels: vec![mono],
        }
    }

    pub fn from_channels(channels: Vec<Vec<f32>>) -> Self {
        if channels.is_empty() {
            Self {
                channels: vec![Vec::new()],
            }
        } else {
            Self { channels }
        }
    }

    pub fn len(&self) -> usize {
        self.channels.first().map(|c| c.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len().max(1)
    }
}

pub struct SharedAudio {
    pub samples: ArcSwapOption<AudioBuffer>, // multi-channel samples in [-1, 1]
    streamed_wav: ArcSwapOption<MappedWavSource>,
    swap_prev_samples: ArcSwapOption<AudioBuffer>,
    swap_prev_pos_f: AtomicF64,
    swap_xfade_frames_left: std::sync::atomic::AtomicUsize,
    swap_xfade_total_frames: std::sync::atomic::AtomicUsize,
    pub vol: AtomicF32,       // 0.0..1.0 linear gain
    pub file_gain: AtomicF32, // per-file gain factor (can be > 1.0)
    pub playing: std::sync::atomic::AtomicBool,
    pub play_pos: std::sync::atomic::AtomicUsize,
    pub play_pos_f: AtomicF64, // high-precision fractional playhead for rate-converted playback
    pub meter_rms: AtomicF32,
    #[allow(dead_code)]
    pub _out_channels: usize,
    pub out_sample_rate: u32,
    pub loop_enabled: std::sync::atomic::AtomicBool,
    pub loop_start: std::sync::atomic::AtomicUsize,
    pub loop_end: std::sync::atomic::AtomicUsize,
    pub loop_xfade_samples: std::sync::atomic::AtomicUsize,
    pub loop_xfade_shape: std::sync::atomic::AtomicU8, // 0=Linear,1=EqualPower
    pub rate: AtomicF32,                               // playback rate (0.25..4.0)
    pub ramp_gain: AtomicF32,                          // short de-click output ramp 0.0..1.0
    pub ramp_target: AtomicF32,
    pub ramp_step: AtomicF32,
    pub ramp_events: std::sync::atomic::AtomicUsize,
}

pub struct AudioEngine {
    _stream: Option<cpal::Stream>,
    pub shared: Arc<SharedAudio>,
    output_device_name: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug)]
struct MappedWavHeader {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    block_align: u16,
    data_offset: usize,
    data_len: usize,
}

#[allow(dead_code)]
#[derive(Debug)]
struct MappedWavSource {
    path: PathBuf,
    mmap: Mmap,
    header: MappedWavHeader,
    frame_count: usize,
}

#[allow(dead_code)]
impl MappedWavSource {
    fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)
            .with_context(|| format!("open mapped wav source: {}", path.display()))?;
        let header = read_mapped_wav_header(&mut file, path)?
            .with_context(|| format!("unsupported wav stream source: {}", path.display()))?;
        let mmap = unsafe { Mmap::map(&file) }
            .with_context(|| format!("mmap wav source: {}", path.display()))?;
        let frame_count = header.data_len / header.block_align.max(1) as usize;
        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            header,
            frame_count,
        })
    }

    fn len(&self) -> usize {
        self.frame_count
    }

    fn channel_count(&self) -> usize {
        self.header.channels.max(1) as usize
    }

    fn sample_rate(&self) -> u32 {
        self.header.sample_rate.max(1)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn sample_at_interp(&self, ch_idx: usize, pos_f: f64) -> f32 {
        if self.frame_count == 0 {
            return 0.0;
        }
        let max_index = self.frame_count.saturating_sub(1);
        let pf = if pos_f.is_finite() {
            pos_f.clamp(0.0, max_index as f64)
        } else {
            0.0
        };
        let i0 = pf.floor() as usize;
        let i1 = (i0 + 1).min(max_index);
        let t = (pf - i0 as f64).clamp(0.0, 1.0) as f32;
        let s0 = self.sample_at_frame(i0, ch_idx);
        let s1 = self.sample_at_frame(i1, ch_idx);
        s0 * (1.0 - t) + s1 * t
    }

    fn sample_at_frame(&self, frame_idx: usize, ch_idx: usize) -> f32 {
        if self.frame_count == 0 {
            return 0.0;
        }
        let frame_idx = frame_idx.min(self.frame_count.saturating_sub(1));
        let channels = self.channel_count();
        let src_ch = ch_idx.min(channels.saturating_sub(1));
        let bytes_per_sample = (self.header.block_align as usize) / channels.max(1);
        let start = self
            .header
            .data_offset
            .saturating_add(frame_idx.saturating_mul(self.header.block_align as usize))
            .saturating_add(src_ch.saturating_mul(bytes_per_sample));
        let end = start.saturating_add(bytes_per_sample);
        let Some(sample) = self.mmap.get(start..end) else {
            return 0.0;
        };
        match (self.header.audio_format, self.header.bits_per_sample) {
            (3, 32) if sample.len() >= 4 => {
                f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]).clamp(-1.0, 1.0)
            }
            (1, 8) if !sample.is_empty() => ((sample[0] as f32 - 128.0) / 128.0).clamp(-1.0, 1.0),
            (1, 16) if sample.len() >= 2 => (i16::from_le_bytes([sample[0], sample[1]]) as f32
                / i16::MAX as f32)
                .clamp(-1.0, 1.0),
            (1, 24) if sample.len() >= 3 => {
                let sign = if (sample[2] & 0x80) != 0 { 0xFF } else { 0x00 };
                let value = i32::from_le_bytes([sample[0], sample[1], sample[2], sign]);
                (value as f32 / 8_388_607.0).clamp(-1.0, 1.0)
            }
            (1, 32) if sample.len() >= 4 => {
                (i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) as f32
                    / i32::MAX as f32)
                    .clamp(-1.0, 1.0)
            }
            _ => 0.0,
        }
    }
}

#[allow(dead_code)]
fn read_mapped_wav_header(file: &mut File, path: &Path) -> Result<Option<MappedWavHeader>> {
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)
        .with_context(|| format!("read wav header: {}", path.display()))?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Ok(None);
    }
    let mut fmt_audio_format = 0u16;
    let mut fmt_channels = 0u16;
    let mut fmt_sample_rate = 0u32;
    let mut fmt_bits = 0u16;
    let mut fmt_block_align = 0u16;
    let mut data_offset = None;
    let mut data_len = 0usize;
    loop {
        let mut chunk_header = [0u8; 8];
        match file.read_exact(&mut chunk_header) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("read wav chunk header: {}", path.display()))
            }
        }
        let id = &chunk_header[0..4];
        let size = u32::from_le_bytes([
            chunk_header[4],
            chunk_header[5],
            chunk_header[6],
            chunk_header[7],
        ]) as usize;
        let chunk_data_pos = file.stream_position()? as usize;
        if id == b"fmt " {
            let mut fmt = vec![0u8; size];
            file.read_exact(&mut fmt)
                .with_context(|| format!("read wav fmt chunk: {}", path.display()))?;
            if fmt.len() >= 16 {
                fmt_audio_format = u16::from_le_bytes([fmt[0], fmt[1]]);
                fmt_channels = u16::from_le_bytes([fmt[2], fmt[3]]);
                fmt_sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                fmt_block_align = u16::from_le_bytes([fmt[12], fmt[13]]);
                fmt_bits = u16::from_le_bytes([fmt[14], fmt[15]]);
                if fmt_audio_format == 0xFFFE && fmt.len() >= 40 {
                    fmt_audio_format = u16::from_le_bytes([fmt[24], fmt[25]]);
                }
            }
        } else if id == b"data" {
            data_offset = Some(chunk_data_pos);
            data_len = size;
            break;
        }
        let next = chunk_data_pos.saturating_add(size).saturating_add(size & 1);
        file.seek(SeekFrom::Start(next as u64))?;
    }
    let Some(data_offset) = data_offset else {
        return Ok(None);
    };
    let supported = matches!(
        (fmt_audio_format, fmt_bits),
        (1, 8) | (1, 16) | (1, 24) | (1, 32) | (3, 32)
    );
    if !supported || fmt_channels == 0 || fmt_sample_rate == 0 || fmt_block_align == 0 {
        return Ok(None);
    }
    Ok(Some(MappedWavHeader {
        audio_format: fmt_audio_format,
        channels: fmt_channels,
        sample_rate: fmt_sample_rate,
        bits_per_sample: fmt_bits,
        block_align: fmt_block_align,
        data_offset,
        data_len,
    }))
}

impl AudioEngine {
    #[allow(dead_code)]
    const SWAP_XFADE_FRAMES: usize = 96;

    fn device_display_name(device: &cpal::Device) -> Option<String> {
        let description = device.description().ok()?;
        let trimmed = description.name().trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn new_shared(out_channels: usize, out_sample_rate: u32) -> Arc<SharedAudio> {
        Arc::new(SharedAudio {
            samples: ArcSwapOption::from(None),
            streamed_wav: ArcSwapOption::from(None),
            swap_prev_samples: ArcSwapOption::from(None),
            swap_prev_pos_f: AtomicF64::new(0.0),
            swap_xfade_frames_left: std::sync::atomic::AtomicUsize::new(0),
            swap_xfade_total_frames: std::sync::atomic::AtomicUsize::new(0),
            vol: AtomicF32::new(1.0),
            file_gain: AtomicF32::new(1.0),
            playing: std::sync::atomic::AtomicBool::new(false),
            play_pos: std::sync::atomic::AtomicUsize::new(0),
            play_pos_f: AtomicF64::new(0.0),
            meter_rms: AtomicF32::new(0.0),
            _out_channels: out_channels,
            out_sample_rate,
            loop_enabled: std::sync::atomic::AtomicBool::new(false),
            loop_start: std::sync::atomic::AtomicUsize::new(0),
            loop_end: std::sync::atomic::AtomicUsize::new(0),
            loop_xfade_samples: std::sync::atomic::AtomicUsize::new(0),
            loop_xfade_shape: std::sync::atomic::AtomicU8::new(0),
            rate: AtomicF32::new(1.0),
            ramp_gain: AtomicF32::new(1.0),
            ramp_target: AtomicF32::new(1.0),
            ramp_step: AtomicF32::new(1.0),
            ramp_events: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub fn new() -> Result<Self> {
        Self::new_with_output_device_name(None)
    }

    fn choose_output_config(
        device: &cpal::Device,
        preferred_sample_rate: Option<u32>,
    ) -> Result<cpal::SupportedStreamConfig> {
        let default_cfg = device
            .default_output_config()
            .context("No default output config")?;
        let Some(preferred_sr) = preferred_sample_rate.filter(|v| *v > 0) else {
            return Ok(default_cfg);
        };
        if default_cfg.sample_rate() == preferred_sr {
            return Ok(default_cfg);
        }

        let default_format = default_cfg.sample_format();
        let default_channels = default_cfg.channels();
        let mut same_format = None;
        let mut fallback = None;
        if let Ok(configs) = device.supported_output_configs() {
            for range in configs {
                let Some(cfg) = range.try_with_sample_rate(preferred_sr) else {
                    continue;
                };
                if cfg.sample_format() == default_format && cfg.channels() == default_channels {
                    return Ok(cfg);
                }
                if cfg.sample_format() == default_format && same_format.is_none() {
                    same_format = Some(cfg);
                    continue;
                }
                if fallback.is_none() {
                    fallback = Some(cfg);
                }
            }
        }
        Ok(same_format.or(fallback).unwrap_or(default_cfg))
    }

    pub fn list_output_devices() -> Result<Vec<String>> {
        let host = cpal::default_host();
        let devices = host
            .output_devices()
            .context("failed to enumerate output devices")?;
        let mut names = Vec::new();
        for device in devices {
            if let Some(name) = Self::device_display_name(&device) {
                names.push(name);
            }
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub fn new_with_output_device_name(name: Option<&str>) -> Result<Self> {
        Self::new_with_output_device_name_and_sample_rate(name, None)
    }

    pub fn new_with_output_device_name_and_sample_rate(
        name: Option<&str>,
        preferred_sample_rate: Option<u32>,
    ) -> Result<Self> {
        let host = cpal::default_host();
        let requested = name.map(str::trim).filter(|v| !v.is_empty());
        let device = if let Some(requested_name) = requested {
            let mut found = None;
            let devices = host
                .output_devices()
                .context("failed to enumerate output devices")?;
            for candidate in devices {
                let Some(candidate_name) = Self::device_display_name(&candidate) else {
                    continue;
                };
                if candidate_name == requested_name {
                    found = Some(candidate);
                    break;
                }
            }
            found.with_context(|| format!("output device not found: {requested_name}"))?
        } else {
            host.default_output_device()
                .context("No default output device")?
        };
        let device_name = Self::device_display_name(&device);
        let cfg = Self::choose_output_config(&device, preferred_sample_rate)?;

        let shared = Self::new_shared(cfg.channels() as usize, cfg.sample_rate());

        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => {
                Self::build_stream::<f32>(&device, &cfg.into(), shared.clone())?
            }
            cpal::SampleFormat::I16 => {
                Self::build_stream::<i16>(&device, &cfg.into(), shared.clone())?
            }
            cpal::SampleFormat::U16 => {
                Self::build_stream::<u16>(&device, &cfg.into(), shared.clone())?
            }
            _ => anyhow::bail!("Unsupported sample format"),
        };

        Ok(Self {
            _stream: Some(stream),
            shared,
            output_device_name: device_name,
        })
    }

    pub fn new_for_test() -> Self {
        let shared = Self::new_shared(2, 48_000);
        Self {
            _stream: None,
            shared,
            output_device_name: Some("Test Output Device".to_string()),
        }
    }

    pub fn output_device_name(&self) -> Option<&str> {
        self.output_device_name.as_deref()
    }

    pub fn has_output_stream(&self) -> bool {
        self._stream.is_some()
    }

    fn build_stream<T>(
        device: &cpal::Device,
        cfg: &cpal::StreamConfig,
        shared: Arc<SharedAudio>,
    ) -> Result<cpal::Stream>
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let channels = cfg.channels as usize;
        let err_fn = |e| eprintln!("cpal stream error: {e}");
        let stream = device.build_output_stream(
            cfg,
            move |data: &mut [T], _| {
                let maybe_samples = shared.samples.load();
                let maybe_stream = shared.streamed_wav.load();
                let playing = shared.playing.load(std::sync::atomic::Ordering::Relaxed);
                if !playing {
                    for frame in data.chunks_mut(channels) {
                        for ch in frame.iter_mut() {
                            *ch = T::from_sample(0.0);
                        }
                    }
                    shared
                        .meter_rms
                        .store(0.0, std::sync::atomic::Ordering::Relaxed);
                    return;
                }

                let vol = shared.vol.load(std::sync::atomic::Ordering::Relaxed);
                let rate = shared
                    .rate
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .clamp(0.25, 4.0) as f64;
                let looping = shared
                    .loop_enabled
                    .load(std::sync::atomic::Ordering::Relaxed);
                let loop_start = shared.loop_start.load(std::sync::atomic::Ordering::Relaxed);
                let loop_end = shared.loop_end.load(std::sync::atomic::Ordering::Relaxed);
                let mut pos_f = shared.play_pos_f.load(std::sync::atomic::Ordering::Relaxed);
                if !pos_f.is_finite() || pos_f < 0.0 {
                    pos_f = 0.0;
                }

                if let Some(samples_arc) = maybe_samples.as_ref() {
                    let samples = samples_arc.as_ref();
                    let len = samples.len();
                    if len == 0 {
                        shared
                            .playing
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        for frame in data.chunks_mut(channels) {
                            for ch in frame.iter_mut() {
                                *ch = T::from_sample(0.0);
                            }
                        }
                        shared
                            .meter_rms
                            .store(0.0, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                    let src_channels = samples.channel_count();
                    let valid_loop = looping && loop_end > loop_start && loop_end <= len;
                    let mut pos = pos_f.floor() as usize;
                    for frame in data.chunks_mut(channels) {
                        if pos >= len {
                            if valid_loop {
                                pos_f = loop_start as f64;
                                pos = loop_start;
                            } else {
                                shared
                                    .playing
                                    .store(false, std::sync::atomic::Ordering::Relaxed);
                                for ch in frame.iter_mut() {
                                    *ch = T::from_sample(0.0);
                                }
                                continue;
                            }
                        }
                        if valid_loop && pos >= loop_end {
                            pos_f = loop_start as f64;
                        }
                        for (out_ch, out_sample) in frame.iter_mut().enumerate() {
                            let src_ch = if src_channels == 1 {
                                0
                            } else if out_ch < src_channels {
                                out_ch
                            } else {
                                src_channels - 1
                            };
                            let sample = Self::sample_at_interp(samples, src_ch, pos_f);
                            *out_sample = T::from_sample((sample * vol).clamp(-1.0, 1.0));
                        }
                        pos_f += rate;
                        pos = pos_f.floor() as usize;
                    }
                    shared
                        .play_pos
                        .store(pos, std::sync::atomic::Ordering::Relaxed);
                    shared
                        .play_pos_f
                        .store(pos_f, std::sync::atomic::Ordering::Relaxed);
                    shared
                        .meter_rms
                        .store(0.0, std::sync::atomic::Ordering::Relaxed);
                    return;
                }

                if let Some(stream) = maybe_stream.as_ref() {
                    let len = stream.len();
                    if len == 0 {
                        shared
                            .playing
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        for frame in data.chunks_mut(channels) {
                            for ch in frame.iter_mut() {
                                *ch = T::from_sample(0.0);
                            }
                        }
                        shared
                            .meter_rms
                            .store(0.0, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                    let src_channels = stream.channel_count();
                    let valid_loop = looping && loop_end > loop_start && loop_end <= len;
                    let mut pos = pos_f.floor() as usize;
                    for frame in data.chunks_mut(channels) {
                        if pos >= len {
                            if valid_loop {
                                pos_f = loop_start as f64;
                                pos = loop_start;
                            } else {
                                shared
                                    .playing
                                    .store(false, std::sync::atomic::Ordering::Relaxed);
                                for ch in frame.iter_mut() {
                                    *ch = T::from_sample(0.0);
                                }
                                continue;
                            }
                        }
                        if valid_loop && pos >= loop_end {
                            pos_f = loop_start as f64;
                        }
                        for (out_ch, out_sample) in frame.iter_mut().enumerate() {
                            let src_ch = if src_channels == 1 {
                                0
                            } else if out_ch < src_channels {
                                out_ch
                            } else {
                                src_channels - 1
                            };
                            let sample = stream.sample_at_interp(src_ch, pos_f);
                            *out_sample = T::from_sample((sample * vol).clamp(-1.0, 1.0));
                        }
                        pos_f += rate;
                        pos = pos_f.floor() as usize;
                    }
                    shared
                        .play_pos
                        .store(pos, std::sync::atomic::Ordering::Relaxed);
                    shared
                        .play_pos_f
                        .store(pos_f, std::sync::atomic::Ordering::Relaxed);
                    shared
                        .meter_rms
                        .store(0.0, std::sync::atomic::Ordering::Relaxed);
                    return;
                }

                for frame in data.chunks_mut(channels) {
                    for ch in frame.iter_mut() {
                        *ch = T::from_sample(0.0);
                    }
                }
                shared
                    .meter_rms
                    .store(0.0, std::sync::atomic::Ordering::Relaxed);
            },
            err_fn,
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }

    pub fn set_samples(&self, samples: Arc<AudioBuffer>) {
        let len = samples.len();
        self.shared.streamed_wav.store(None);
        self.shared.samples.store(Some(samples));
        self.clear_swap_crossfade();
        self.shared
            .play_pos
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .play_pos_f
            .store(0.0, std::sync::atomic::Ordering::Relaxed);
        // update loop region to whole buffer by default
        self.shared
            .loop_start
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .loop_end
            .store(len, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_samples_mono(&self, mono: Vec<f32>) {
        self.set_samples(Arc::new(AudioBuffer::from_mono(mono)));
    }

    pub fn set_samples_channels(&self, channels: Vec<Vec<f32>>) {
        self.set_samples(Arc::new(AudioBuffer::from_channels(channels)));
    }

    pub fn set_samples_buffer(&self, samples: Arc<AudioBuffer>) {
        self.set_samples(samples);
    }

    fn remap_pos_for_new_source(
        pos_f: f64,
        from_sr: u32,
        to_sr: u32,
        new_len: usize,
    ) -> (usize, f64) {
        if new_len == 0 {
            return (0, 0.0);
        }
        let from_sr = from_sr.max(1) as f64;
        let to_sr = to_sr.max(1) as f64;
        let pos_f = if pos_f.is_finite() && pos_f >= 0.0 {
            pos_f
        } else {
            0.0
        };
        let time_sec = pos_f / from_sr;
        let max_pos_f = new_len.saturating_sub(1) as f64;
        let new_pos_f = (time_sec * to_sr).clamp(0.0, max_pos_f);
        (new_pos_f.floor() as usize, new_pos_f)
    }

    pub fn set_samples_buffer_keep_time_pos(
        &self,
        samples: Arc<AudioBuffer>,
        from_sr: u32,
        to_sr: u32,
    ) {
        let old_pos_f = self
            .shared
            .play_pos_f
            .load(std::sync::atomic::Ordering::Relaxed);
        let new_len = samples.len();
        let (new_pos, new_pos_f) =
            Self::remap_pos_for_new_source(old_pos_f, from_sr, to_sr, new_len);
        self.clear_swap_crossfade();
        self.shared.streamed_wav.store(None);
        self.shared.samples.store(Some(samples));
        self.shared
            .play_pos
            .store(new_pos, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .play_pos_f
            .store(new_pos_f, std::sync::atomic::Ordering::Relaxed);
        let loop_start = self
            .shared
            .loop_start
            .load(std::sync::atomic::Ordering::Relaxed);
        let loop_end = self
            .shared
            .loop_end
            .load(std::sync::atomic::Ordering::Relaxed);
        if loop_start >= new_len {
            self.shared
                .loop_start
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
        if loop_end > new_len {
            self.shared
                .loop_end
                .store(new_len, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn set_samples_channels_keep_time_pos(
        &self,
        channels: Vec<Vec<f32>>,
        from_sr: u32,
        to_sr: u32,
    ) {
        self.set_samples_buffer_keep_time_pos(
            Arc::new(AudioBuffer::from_channels(channels)),
            from_sr,
            to_sr,
        );
    }

    pub fn set_streaming_wav_path(&self, path: &Path) -> Result<()> {
        let source = Arc::new(MappedWavSource::open(path)?);
        let len = source.len();
        self.shared.samples.store(None);
        self.shared.streamed_wav.store(Some(source));
        self.clear_swap_crossfade();
        self.shared
            .play_pos
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .play_pos_f
            .store(0.0, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .loop_start
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .loop_end
            .store(len, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn clear_streaming_source(&self) {
        self.shared.streamed_wav.store(None);
    }

    pub fn has_audio_source(&self) -> bool {
        self.shared
            .samples
            .load()
            .as_ref()
            .map(|buf| !buf.is_empty())
            .unwrap_or(false)
            || self
                .shared
                .streamed_wav
                .load()
                .as_ref()
                .map(|src| src.len() > 0)
                .unwrap_or(false)
    }

    pub fn current_source_len(&self) -> usize {
        self.shared
            .samples
            .load()
            .as_ref()
            .map(|buf| buf.len())
            .or_else(|| {
                self.shared
                    .streamed_wav
                    .load()
                    .as_ref()
                    .map(|src| src.len())
            })
            .unwrap_or(0)
    }

    pub fn streaming_wav_sample_rate(&self) -> Option<u32> {
        self.shared
            .streamed_wav
            .load()
            .as_ref()
            .map(|src| src.sample_rate())
    }

    pub fn is_streaming_wav_path(&self, path: &Path) -> bool {
        self.shared
            .streamed_wav
            .load()
            .as_ref()
            .map(|src| src.path() == path)
            .unwrap_or(false)
    }

    pub fn replace_samples_keep_pos(&self, samples: Arc<AudioBuffer>) {
        let new_len = samples.len();
        let pos = self
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        self.clear_swap_crossfade();
        self.shared.streamed_wav.store(None);
        self.shared.samples.store(Some(samples));
        if pos >= new_len {
            self.shared
                .play_pos
                .store(0, std::sync::atomic::Ordering::Relaxed);
            self.shared
                .play_pos_f
                .store(0.0, std::sync::atomic::Ordering::Relaxed);
        } else {
            self.shared
                .play_pos
                .store(pos, std::sync::atomic::Ordering::Relaxed);
            self.shared
                .play_pos_f
                .store(pos as f64, std::sync::atomic::Ordering::Relaxed);
        }
        let loop_start = self
            .shared
            .loop_start
            .load(std::sync::atomic::Ordering::Relaxed);
        let loop_end = self
            .shared
            .loop_end
            .load(std::sync::atomic::Ordering::Relaxed);
        if loop_start >= new_len {
            self.shared
                .loop_start
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
        if loop_end > new_len {
            self.shared
                .loop_end
                .store(new_len, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn set_volume(&self, v: f32) {
        self.shared
            .vol
            .store(v.clamp(0.0, 1.0), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_file_gain(&self, g: f32) {
        // allow >1.0 (up to, say, 16x = +24dB). Clamp to a reasonable upper bound.
        let g = g.clamp(0.0, 16.0);
        self.shared
            .file_gain
            .store(g, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn toggle_play(&self) {
        let now = !self
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        self.shared
            .playing
            .store(now, std::sync::atomic::Ordering::Relaxed);
        if now && !self.has_audio_source() {
            self.shared
                .playing
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
        if now {
            let pos = self
                .shared
                .play_pos
                .load(std::sync::atomic::Ordering::Relaxed);
            let len = self.current_source_len();
            if len > 0 && pos >= len {
                self.shared
                    .play_pos
                    .store(0, std::sync::atomic::Ordering::Relaxed);
                self.shared
                    .play_pos_f
                    .store(0.0, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    pub fn play(&self) {
        if !self.has_audio_source() {
            return;
        }
        self.shared
            .playing
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let pos = self
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        let len = self.current_source_len();
        if len > 0 && pos >= len {
            self.shared
                .play_pos
                .store(0, std::sync::atomic::Ordering::Relaxed);
            self.shared
                .play_pos_f
                .store(0.0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn stop(&self) {
        self.shared
            .playing
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.clear_swap_crossfade();
    }

    pub fn set_loop_enabled(&self, en: bool) {
        self.shared
            .loop_enabled
            .store(en, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_loop_region(&self, start: usize, end: usize) {
        self.shared
            .loop_start
            .store(start, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .loop_end
            .store(end, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_loop_crossfade(&self, samples: usize, shape_linear_or_equal_power: u8) {
        self.shared
            .loop_xfade_samples
            .store(samples, std::sync::atomic::Ordering::Relaxed);
        self.shared.loop_xfade_shape.store(
            if shape_linear_or_equal_power > 0 {
                1
            } else {
                0
            },
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub fn set_rate(&self, rate: f32) {
        self.shared
            .rate
            .store(rate.clamp(0.25, 4.0), std::sync::atomic::Ordering::Relaxed);
    }

    #[allow(dead_code)]
    fn sample_at_interp(buffer: &AudioBuffer, ch_idx: usize, pos_f: f64) -> f32 {
        let channel = buffer
            .channels
            .get(ch_idx)
            .unwrap_or_else(|| &buffer.channels[0]);
        if channel.is_empty() {
            return 0.0;
        }
        let max_index = channel.len().saturating_sub(1);
        let pf = if pos_f.is_finite() {
            pos_f.clamp(0.0, max_index as f64)
        } else {
            0.0
        };
        let i0 = pf.floor() as usize;
        let i1 = (i0 + 1).min(max_index);
        let t = (pf - i0 as f64).clamp(0.0, 1.0) as f32;
        channel[i0] * (1.0 - t) + channel[i1] * t
    }

    #[allow(dead_code)]
    fn swap_crossfade_weights(frames_left: usize, total_frames: usize) -> (f32, f32) {
        if total_frames == 0 {
            return (0.0, 1.0);
        }
        let progressed = total_frames.saturating_sub(frames_left) as f32 / total_frames as f32;
        let angle = progressed.clamp(0.0, 1.0) * core::f32::consts::FRAC_PI_2;
        (angle.cos(), angle.sin())
    }

    #[allow(dead_code)]
    fn advance_ramp_gain(ramp_gain: &mut f32, ramp_target: f32, ramp_step: f32) {
        if (*ramp_gain - ramp_target).abs() <= 1e-6 {
            *ramp_gain = ramp_target;
            return;
        }
        if *ramp_gain < ramp_target {
            *ramp_gain = (*ramp_gain + ramp_step).min(ramp_target);
        } else {
            *ramp_gain = (*ramp_gain - ramp_step).max(ramp_target);
        }
    }

    #[allow(dead_code)]
    fn start_output_ramp(&self, start: f32, target: f32, duration_ms: f32) {
        let start = if start.is_finite() { start } else { 0.0 }.clamp(0.0, 1.0);
        let target = if target.is_finite() { target } else { 1.0 }.clamp(0.0, 1.0);
        let duration_ms = duration_ms.max(0.1);
        let sr = self.shared.out_sample_rate.max(1) as f32;
        let frames = ((duration_ms / 1000.0) * sr).round().max(1.0);
        let step = ((target - start).abs() / frames).max(1e-5);
        self.shared
            .ramp_gain
            .store(start, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .ramp_target
            .store(target, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .ramp_step
            .store(step, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .ramp_events
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn clear_swap_crossfade(&self) {
        self.shared.swap_prev_samples.store(None);
        self.shared
            .swap_prev_pos_f
            .store(0.0, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .swap_xfade_frames_left
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .swap_xfade_total_frames
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn seek_to_sample(&self, pos: usize) {
        let len = self.current_source_len();
        if len > 0 {
            let p = pos.min(len);
            self.shared
                .play_pos
                .store(p, std::sync::atomic::Ordering::Relaxed);
            self.shared
                .play_pos_f
                .store(p as f64, std::sync::atomic::Ordering::Relaxed);
            self.clear_swap_crossfade();
        }
    }

    pub fn remap_play_pos_to_sample_rate(&self, from_sr: u32, to_sr: u32) {
        let len = self.current_source_len();
        if len == 0 {
            return;
        }
        let mut pos_f = self
            .shared
            .play_pos_f
            .load(std::sync::atomic::Ordering::Relaxed);
        if !pos_f.is_finite() || pos_f < 0.0 {
            pos_f = 0.0;
        }
        let (new_pos, new_pos_f) = Self::remap_pos_for_new_source(pos_f, from_sr, to_sr, len);
        self.shared
            .play_pos
            .store(new_pos, std::sync::atomic::Ordering::Relaxed);
        self.shared
            .play_pos_f
            .store(new_pos_f, std::sync::atomic::Ordering::Relaxed);
        self.clear_swap_crossfade();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hound::{SampleFormat, WavSpec, WavWriter};

    fn write_test_wav(path: &Path, sr: u32, secs: f32) {
        let spec = WavSpec {
            channels: 2,
            sample_rate: sr,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut writer = WavWriter::create(path, spec).expect("create wav");
        let frames = ((sr as f32) * secs).round().max(1.0) as usize;
        for i in 0..frames {
            let t = i as f32 / sr as f32;
            writer
                .write_sample((t * 220.0 * std::f32::consts::TAU).sin() * 0.25)
                .expect("write l");
            writer
                .write_sample((t * 440.0 * std::f32::consts::TAU).sin() * 0.20)
                .expect("write r");
        }
        writer.finalize().expect("finalize wav");
    }

    #[test]
    fn play_keeps_unity_transport_state() {
        let audio = AudioEngine::new_for_test();
        audio.set_samples_mono(vec![0.0, 0.1, -0.1, 0.0]);
        audio.play();
        assert!(audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(
            audio
                .shared
                .ramp_events
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "offline-only transport should not schedule output ramps"
        );
    }

    #[test]
    fn replace_samples_keep_pos_preserves_integer_playhead_without_swap_crossfade() {
        let audio = AudioEngine::new_for_test();
        audio.set_samples_mono(vec![0.0, 0.1, 0.2, 0.3]);
        audio.play();
        audio.seek_to_sample(2);
        audio.replace_samples_keep_pos(Arc::new(AudioBuffer::from_mono(vec![0.1, 0.0, -0.1, 0.0])));
        assert_eq!(
            audio
                .shared
                .play_pos
                .load(std::sync::atomic::Ordering::Relaxed),
            2,
            "buffer replace should preserve the integer playhead"
        );
        assert_eq!(
            audio
                .shared
                .swap_xfade_frames_left
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "offline-only buffer replace should not arm swap crossfade"
        );
    }

    #[test]
    fn streaming_wav_source_reports_length_and_rate_without_heap_buffer() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "neowaves_audio_stream_test_{}_{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        write_test_wav(&path, 44_100, 1.5);

        let audio = AudioEngine::new_for_test();
        audio
            .set_streaming_wav_path(&path)
            .expect("open streaming wav");
        assert!(
            audio.has_audio_source(),
            "stream transport should count as audio source"
        );
        assert!(audio.is_streaming_wav_path(&path));
        assert_eq!(audio.streaming_wav_sample_rate(), Some(44_100));
        assert_eq!(audio.current_source_len(), 66_150);
        assert!(
            audio.shared.samples.load().is_none(),
            "stream transport should avoid loading a heap audio buffer"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remap_play_pos_to_sample_rate_preserves_time_when_switching_sources() {
        let audio = AudioEngine::new_for_test();
        audio.set_samples(Arc::new(AudioBuffer::from_mono(vec![0.0; 96_000])));
        audio
            .shared
            .play_pos
            .store(44_100, std::sync::atomic::Ordering::Relaxed);
        audio
            .shared
            .play_pos_f
            .store(44_100.0, std::sync::atomic::Ordering::Relaxed);

        audio.remap_play_pos_to_sample_rate(44_100, 48_000);

        let pos = audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        let pos_f = audio
            .shared
            .play_pos_f
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(pos, 48_000);
        assert!((pos_f - 48_000.0).abs() < 1.0e-3);
    }

    #[test]
    fn fractional_playhead_precision_stays_stable_for_long_exact_stream_runs() {
        let audio = AudioEngine::new_for_test();
        audio.set_rate(44_100.0f32 / 48_000.0f32);
        audio
            .shared
            .play_pos_f
            .store(0.0, std::sync::atomic::Ordering::Relaxed);

        let rate = audio.shared.rate.load(std::sync::atomic::Ordering::Relaxed);
        let block_frames = 480usize;
        let total_blocks = 18_000usize; // 180 seconds at 48kHz
        let mut min_effective_rate = f64::INFINITY;
        let mut max_effective_rate = 0.0f64;

        for _ in 0..total_blocks {
            let start = audio
                .shared
                .play_pos_f
                .load(std::sync::atomic::Ordering::Relaxed);
            let mut pos_f = start;
            for _ in 0..block_frames {
                pos_f += f64::from(rate);
            }
            audio
                .shared
                .play_pos_f
                .store(pos_f, std::sync::atomic::Ordering::Relaxed);
            let effective_rate = ((pos_f - start) as f64) / block_frames as f64;
            min_effective_rate = min_effective_rate.min(effective_rate);
            max_effective_rate = max_effective_rate.max(effective_rate);
        }

        let expected_rate = rate as f64;
        let block_drift_cents = if min_effective_rate > 0.0 {
            1200.0 * (max_effective_rate / min_effective_rate).log2()
        } else {
            f64::INFINITY
        };
        let expected_pos = total_blocks as f64 * block_frames as f64 * expected_rate;
        let actual_pos = audio
            .shared
            .play_pos_f
            .load(std::sync::atomic::Ordering::Relaxed) as f64;

        assert!(
            block_drift_cents < 0.01,
            "fractional playhead drift should stay sub-cent over long runs: {block_drift_cents:.6} cents"
        );
        assert!(
            (actual_pos - expected_pos).abs() < 1.0e-6 * expected_pos.max(1.0),
            "long-run playhead drifted from expected position: expected={expected_pos} actual={actual_pos}"
        );
    }
}
