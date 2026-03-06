use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwapOption;
use atomic_float::AtomicF32;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

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
        self.channels.get(0).map(|c| c.len()).unwrap_or(0)
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len().max(1)
    }
}

pub struct SharedAudio {
    pub samples: ArcSwapOption<AudioBuffer>, // multi-channel samples in [-1, 1]
    pub vol: AtomicF32,                      // 0.0..1.0 linear gain
    pub file_gain: AtomicF32,                // per-file gain factor (can be > 1.0)
    pub playing: std::sync::atomic::AtomicBool,
    pub play_pos: std::sync::atomic::AtomicUsize,
    pub play_pos_f: AtomicF32, // fractional position for rate control
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

impl AudioEngine {
    fn new_shared(out_channels: usize, out_sample_rate: u32) -> Arc<SharedAudio> {
        Arc::new(SharedAudio {
            samples: ArcSwapOption::from(None),
            vol: AtomicF32::new(1.0),
            file_gain: AtomicF32::new(1.0),
            playing: std::sync::atomic::AtomicBool::new(false),
            play_pos: std::sync::atomic::AtomicUsize::new(0),
            play_pos_f: AtomicF32::new(0.0),
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

    pub fn list_output_devices() -> Result<Vec<String>> {
        let host = cpal::default_host();
        let devices = host
            .output_devices()
            .context("failed to enumerate output devices")?;
        let mut names = Vec::new();
        for device in devices {
            if let Ok(name) = device.name() {
                let trimmed = name.trim();
                if !trimmed.is_empty() {
                    names.push(trimmed.to_string());
                }
            }
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub fn new_with_output_device_name(name: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let requested = name.map(str::trim).filter(|v| !v.is_empty());
        let device = if let Some(requested_name) = requested {
            let mut found = None;
            let devices = host
                .output_devices()
                .context("failed to enumerate output devices")?;
            for candidate in devices {
                let candidate_name = match candidate.name() {
                    Ok(v) => v,
                    Err(_) => continue,
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
        let device_name = device.name().ok().and_then(|v| {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        let cfg = device
            .default_output_config()
            .context("No default output config")?;

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
                // audio callback
                let mut sum_sq = 0.0f32;
                let mut n = 0usize;
                let maybe_samples = shared.samples.load();
                let playing = shared.playing.load(std::sync::atomic::Ordering::Relaxed);
                let vol = shared.vol.load(std::sync::atomic::Ordering::Relaxed)
                    * shared.file_gain.load(std::sync::atomic::Ordering::Relaxed);
                let rate = shared
                    .rate
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .clamp(0.25, 4.0);
                let looping = shared
                    .loop_enabled
                    .load(std::sync::atomic::Ordering::Relaxed);
                let loop_start = shared.loop_start.load(std::sync::atomic::Ordering::Relaxed);
                let loop_end = shared.loop_end.load(std::sync::atomic::Ordering::Relaxed);
                let mut ramp_gain = shared.ramp_gain.load(std::sync::atomic::Ordering::Relaxed);
                if !ramp_gain.is_finite() {
                    ramp_gain = 1.0;
                }
                let ramp_target = shared
                    .ramp_target
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .clamp(0.0, 1.0);
                let mut ramp_step = shared.ramp_step.load(std::sync::atomic::Ordering::Relaxed);
                if !ramp_step.is_finite() || ramp_step <= 0.0 {
                    ramp_step = 1.0;
                }
                if playing {
                    if let Some(samples_arc) = maybe_samples.as_ref() {
                        let samples = samples_arc.as_ref();
                        let len = samples.len();
                        if len == 0 {
                            for frame in data.chunks_mut(channels) {
                                for ch in frame.iter_mut() {
                                    *ch = T::from_sample(0.0);
                                }
                            }
                            return;
                        }
                        let src_channels = samples.channel_count();
                        let mut pos_f =
                            shared.play_pos_f.load(std::sync::atomic::Ordering::Relaxed);
                        if !pos_f.is_finite() || pos_f < 0.0 {
                            pos_f = 0.0;
                        }
                        let mut pos = pos_f.floor() as usize;
                        let valid_loop = looping && loop_end > loop_start && loop_end <= len;
                        for frame in data.chunks_mut(channels) {
                                if pos >= len {
                                    if valid_loop {
                                        pos_f = loop_start as f32;
                                    } else {
                                        shared
                                        .playing
                                        .store(false, std::sync::atomic::Ordering::Relaxed);
                                    for ch in frame.iter_mut() {
                                        *ch = T::from_sample(0.0);
                                    }
                                    Self::advance_ramp_gain(
                                        &mut ramp_gain,
                                        ramp_target,
                                        ramp_step,
                                    );
                                    continue;
                                }
                            }
                            if valid_loop && pos >= loop_end {
                                pos_f = loop_start as f32;
                            }
                            // fractional sample accessor (per channel)
                            let sample_at = |ch_idx: usize, pf: f32| -> f32 {
                                let channel = samples
                                    .channels
                                    .get(ch_idx)
                                    .unwrap_or_else(|| &samples.channels[0]);
                                let i0 = pf.floor() as usize;
                                let i1 = (i0 + 1).min(channel.len().saturating_sub(1));
                                let t = (pf - i0 as f32).clamp(0.0, 1.0);
                                channel[i0] * (1.0 - t) + channel[i1] * t
                            };

                            let mut frame_sum = 0.0f32;
                            for (out_ch, out_sample) in frame.iter_mut().enumerate() {
                                let src_ch = if src_channels == 1 {
                                    0
                                } else if out_ch < src_channels {
                                    out_ch
                                } else {
                                    src_channels - 1
                                };
                                let mut s_lin = sample_at(src_ch, pos_f);
                                // Crossfade near loop start/end if enabled (using centered windows)
                                if valid_loop {
                                    let xfade = shared
                                        .loop_xfade_samples
                                        .load(std::sync::atomic::Ordering::Relaxed);
                                    if xfade > 0 {
                                        let loop_len = loop_end.saturating_sub(loop_start);
                                        let pre = loop_start;
                                        let post = len.saturating_sub(loop_end);
                                        let xfade = xfade.min(loop_len / 2).min(pre).min(post);
                                        if xfade > 0 {
                                            let xfade_f = xfade as f32;
                                            let win_len = xfade_f * 2.0;
                                            let s_start = loop_start as f32 - xfade_f;
                                            let s_end = loop_start as f32 + xfade_f;
                                            let e_start = loop_end as f32 - xfade_f;
                                            let e_end = loop_end as f32 + xfade_f;
                                            let shape = shared
                                                .loop_xfade_shape
                                                .load(std::sync::atomic::Ordering::Relaxed);
                                            let weights = |tcf: f32| -> (f32, f32) {
                                                match shape {
                                                    1 => {
                                                        let a = core::f32::consts::FRAC_PI_2 * tcf;
                                                        (a.cos(), a.sin())
                                                    }
                                                    _ => (1.0 - tcf, tcf),
                                                }
                                            };
                                            if (pos_f >= s_start && pos_f < s_end)
                                                || (pos_f >= e_start && pos_f < e_end)
                                            {
                                                let win_start = if pos_f < s_end && pos_f >= s_start
                                                {
                                                    s_start
                                                } else {
                                                    e_start
                                                };
                                                let offset = pos_f - win_start;
                                                let tcf = (offset / win_len).clamp(0.0, 1.0);
                                                let s_pf = s_start + offset;
                                                let e_pf = e_start + offset;
                                                let s_s = sample_at(src_ch, s_pf);
                                                let s_e = sample_at(src_ch, e_pf);
                                                let (w_e, w_s) = weights(tcf);
                                                s_lin = s_e * w_e + s_s * w_s;
                                            }
                                        }
                                    }
                                }
                                let s = (s_lin * vol * ramp_gain).clamp(-1.0, 1.0);
                                frame_sum += s;
                                *out_sample = T::from_sample(s);
                            }
                            let frame_avg = frame_sum / channels as f32;
                            sum_sq += frame_avg * frame_avg;
                            n += 1;
                            pos_f += rate;
                            pos = pos_f.floor() as usize;
                            Self::advance_ramp_gain(&mut ramp_gain, ramp_target, ramp_step);
                        }
                        shared
                            .play_pos
                            .store(pos, std::sync::atomic::Ordering::Relaxed);
                        shared
                            .play_pos_f
                            .store(pos_f, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        // No buffer, output silence
                        for frame in data.chunks_mut(channels) {
                            for ch in frame.iter_mut() {
                                *ch = T::from_sample(0.0);
                            }
                            Self::advance_ramp_gain(&mut ramp_gain, ramp_target, ramp_step);
                        }
                    }
                } else {
                    // not playing: silence
                    for frame in data.chunks_mut(channels) {
                        for ch in frame.iter_mut() {
                            *ch = T::from_sample(0.0);
                        }
                        Self::advance_ramp_gain(&mut ramp_gain, ramp_target, ramp_step);
                    }
                }
                shared
                    .ramp_gain
                    .store(ramp_gain, std::sync::atomic::Ordering::Relaxed);

                if n > 0 {
                    let rms = (sum_sq / n as f32).sqrt();
                    shared
                        .meter_rms
                        .store(rms, std::sync::atomic::Ordering::Relaxed);
                } else {
                    shared
                        .meter_rms
                        .store(0.0, std::sync::atomic::Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }

    pub fn set_samples(&self, samples: Arc<AudioBuffer>) {
        let len = samples.len();
        self.shared.samples.store(Some(samples));
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

    pub fn replace_samples_keep_pos(&self, samples: Arc<AudioBuffer>) {
        let new_len = samples.len();
        let old_len = self
            .shared
            .samples
            .load()
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);
        let pos = self
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        let mut pos_f = self
            .shared
            .play_pos_f
            .load(std::sync::atomic::Ordering::Relaxed);
        if !pos_f.is_finite() || pos_f < 0.0 {
            pos_f = 0.0;
        }
        self.shared.samples.store(Some(samples));
        if pos >= new_len {
            self.shared
                .play_pos
                .store(0, std::sync::atomic::Ordering::Relaxed);
            self.shared
                .play_pos_f
                .store(0.0, std::sync::atomic::Ordering::Relaxed);
        } else {
            let max_pos_f = new_len.saturating_sub(1) as f32;
            if pos_f > max_pos_f {
                pos_f = pos as f32;
            }
            self.shared
                .play_pos
                .store(pos, std::sync::atomic::Ordering::Relaxed);
            self.shared
                .play_pos_f
                .store(pos_f, std::sync::atomic::Ordering::Relaxed);
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
        if loop_end == old_len || loop_end > new_len {
            self.shared
                .loop_end
                .store(new_len, std::sync::atomic::Ordering::Relaxed);
        }
        if self
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.start_output_ramp(0.0, 1.0, 8.0);
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
        if now && self.shared.samples.load().is_none() {
            self.shared
                .playing
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
        if now {
            self.start_output_ramp(0.0, 1.0, 6.0);
            // on play, if at end, rewind
            let pos = self
                .shared
                .play_pos
                .load(std::sync::atomic::Ordering::Relaxed);
            if let Some(s) = self.shared.samples.load().as_ref() {
                if pos >= s.len() {
                    self.shared
                        .play_pos
                        .store(0, std::sync::atomic::Ordering::Relaxed);
                    self.shared
                        .play_pos_f
                        .store(0.0, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    pub fn play(&self) {
        if self.shared.samples.load().is_none() {
            return;
        }
        self.start_output_ramp(0.0, 1.0, 6.0);
        self.shared
            .playing
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let pos = self
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        if let Some(s) = self.shared.samples.load().as_ref() {
            if pos >= s.len() {
                self.shared
                    .play_pos
                    .store(0, std::sync::atomic::Ordering::Relaxed);
                self.shared
                    .play_pos_f
                    .store(0.0, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    pub fn stop(&self) {
        self.shared
            .playing
            .store(false, std::sync::atomic::Ordering::Relaxed);
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

    pub fn seek_to_sample(&self, pos: usize) {
        // Clamp to buffer length if present
        if let Some(buf) = self.shared.samples.load().as_ref() {
            let len = buf.len();
            let p = pos.min(len);
            self.shared
                .play_pos
                .store(p, std::sync::atomic::Ordering::Relaxed);
            self.shared
                .play_pos_f
                .store(p as f32, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_triggers_output_ramp_event() {
        let audio = AudioEngine::new_for_test();
        audio.set_samples_mono(vec![0.0, 0.1, -0.1, 0.0]);
        let before = audio
            .shared
            .ramp_events
            .load(std::sync::atomic::Ordering::Relaxed);
        audio.play();
        let after = audio
            .shared
            .ramp_events
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(after > before, "expected ramp event on play");
    }

    #[test]
    fn replace_samples_keep_pos_triggers_ramp_when_playing() {
        let audio = AudioEngine::new_for_test();
        audio.set_samples_mono(vec![0.0, 0.1, 0.2, 0.3]);
        audio.play();
        let before = audio
            .shared
            .ramp_events
            .load(std::sync::atomic::Ordering::Relaxed);
        audio.replace_samples_keep_pos(Arc::new(AudioBuffer::from_mono(vec![
            0.1, 0.0, -0.1, 0.0,
        ])));
        let after = audio
            .shared
            .ramp_events
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(after > before, "expected ramp event on buffer replace");
    }
}
