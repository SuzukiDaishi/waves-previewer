use std::sync::Arc;

use anyhow::{Context, Result};
use arc_swap::ArcSwapOption;
use atomic_float::AtomicF32;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct SharedAudio {
    pub samples: ArcSwapOption<Vec<f32>>,        // mono samples in [-1, 1]
    pub vol: AtomicF32,                          // 0.0..1.0 linear gain
    pub file_gain: AtomicF32,                    // per-file gain factor (can be > 1.0)
    pub playing: std::sync::atomic::AtomicBool,
    pub play_pos: std::sync::atomic::AtomicUsize,
    pub play_pos_f: AtomicF32,                   // fractional position for rate control
    pub meter_rms: AtomicF32,
    pub out_channels: usize,
    pub out_sample_rate: u32,
    pub loop_enabled: std::sync::atomic::AtomicBool,
    pub loop_start: std::sync::atomic::AtomicUsize,
    pub loop_end: std::sync::atomic::AtomicUsize,
    pub rate: AtomicF32,                         // playback rate (0.25..4.0)
}

pub struct AudioEngine {
    _stream: cpal::Stream,
    pub shared: Arc<SharedAudio>,
}

impl AudioEngine {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("No default output device")?;
        let cfg = device
            .default_output_config()
            .context("No default output config")?;

        let shared = Arc::new(SharedAudio {
            samples: ArcSwapOption::from(None),
            vol: AtomicF32::new(1.0),
            file_gain: AtomicF32::new(1.0),
            playing: std::sync::atomic::AtomicBool::new(false),
            play_pos: std::sync::atomic::AtomicUsize::new(0),
            play_pos_f: AtomicF32::new(0.0),
            meter_rms: AtomicF32::new(0.0),
            out_channels: cfg.channels() as usize,
            out_sample_rate: cfg.sample_rate().0,
            loop_enabled: std::sync::atomic::AtomicBool::new(false),
            loop_start: std::sync::atomic::AtomicUsize::new(0),
            loop_end: std::sync::atomic::AtomicUsize::new(0),
            rate: AtomicF32::new(1.0),
        });

        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => Self::build_stream::<f32>(&device, &cfg.into(), shared.clone())?,
            cpal::SampleFormat::I16 => Self::build_stream::<i16>(&device, &cfg.into(), shared.clone())?,
            cpal::SampleFormat::U16 => Self::build_stream::<u16>(&device, &cfg.into(), shared.clone())?,
            _ => anyhow::bail!("Unsupported sample format"),
        };

        Ok(Self { _stream: stream, shared })
    }

    fn build_stream<T>(device: &cpal::Device, cfg: &cpal::StreamConfig, shared: Arc<SharedAudio>) -> Result<cpal::Stream>
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
                let rate = shared.rate.load(std::sync::atomic::Ordering::Relaxed).clamp(0.25, 4.0);
                let looping = shared.loop_enabled.load(std::sync::atomic::Ordering::Relaxed);
                let loop_start = shared.loop_start.load(std::sync::atomic::Ordering::Relaxed);
                let loop_end = shared.loop_end.load(std::sync::atomic::Ordering::Relaxed);
                if playing {
                    if let Some(samples_arc) = maybe_samples.as_ref() {
                        let samples = samples_arc.as_ref();
                        let len = samples.len();
                        let mut pos_f = shared.play_pos_f.load(std::sync::atomic::Ordering::Relaxed);
                        if !pos_f.is_finite() || pos_f < 0.0 { pos_f = 0.0; }
                        let mut pos = pos_f.floor() as usize;
                        let valid_loop = looping && loop_end > loop_start && loop_end <= len;
                        for frame in data.chunks_mut(channels) {
                            if pos >= len {
                                if valid_loop {
                                    pos = loop_start;
                                    pos_f = loop_start as f32;
                                } else {
                                    shared.playing.store(false, std::sync::atomic::Ordering::Relaxed);
                                    for ch in frame.iter_mut() { *ch = T::from_sample(0.0); }
                                    continue;
                                }
                            }
                            if valid_loop && pos >= loop_end {
                                pos = loop_start;
                                pos_f = loop_start as f32;
                            }
                            // fractional linear interpolation
                            let i0 = pos;
                            let i1 = (i0 + 1).min(len.saturating_sub(1));
                            let t = (pos_f - i0 as f32).clamp(0.0, 1.0);
                            let s_lin = samples[i0] * (1.0 - t) + samples[i1] * t;
                            let s = s_lin * vol;
                            pos_f += rate;
                            pos = pos_f.floor() as usize;
                            let s_clamped = s.clamp(-1.0, 1.0);
                            sum_sq += s_clamped * s_clamped;
                            n += 1;
                            for ch in frame.iter_mut() {
                                *ch = T::from_sample(s_clamped);
                            }
                        }
                        shared.play_pos.store(pos, std::sync::atomic::Ordering::Relaxed);
                        shared.play_pos_f.store(pos_f, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        // No buffer, output silence
                        for frame in data.chunks_mut(channels) {
                            for ch in frame.iter_mut() { *ch = T::from_sample(0.0); }
                        }
                    }
                } else {
                    // not playing: silence
                    for frame in data.chunks_mut(channels) {
                        for ch in frame.iter_mut() { *ch = T::from_sample(0.0); }
                    }
                }

                if n > 0 {
                    let rms = (sum_sq / n as f32).sqrt();
                    shared.meter_rms.store(rms, std::sync::atomic::Ordering::Relaxed);
                } else {
                    shared.meter_rms.store(0.0, std::sync::atomic::Ordering::Relaxed);
                }
            },
            err_fn,
            None,
        )?;
        stream.play()?;
        Ok(stream)
    }

    pub fn set_samples(&self, samples: Arc<Vec<f32>>) {
        let len = samples.len();
        self.shared.samples.store(Some(samples));
        self.shared.play_pos.store(0, std::sync::atomic::Ordering::Relaxed);
        self.shared.play_pos_f.store(0.0, std::sync::atomic::Ordering::Relaxed);
        // update loop region to whole buffer by default
        self.shared.loop_start.store(0, std::sync::atomic::Ordering::Relaxed);
        self.shared.loop_end.store(len, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_volume(&self, v: f32) {
        self.shared.vol.store(v.clamp(0.0, 1.0), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_file_gain(&self, g: f32) {
        // allow >1.0 (up to, say, 16x = +24dB). Clamp to a reasonable upper bound.
        let g = g.clamp(0.0, 16.0);
        self.shared.file_gain.store(g, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn toggle_play(&self) {
        let now = !self.shared.playing.load(std::sync::atomic::Ordering::Relaxed);
        self.shared.playing.store(now, std::sync::atomic::Ordering::Relaxed);
        if now && self.shared.samples.load().is_none() {
            self.shared.playing.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        if now {
            // on play, if at end, rewind
            let pos = self.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed);
            if let Some(s) = self.shared.samples.load().as_ref() {
                if pos >= s.len() {
                    self.shared.play_pos.store(0, std::sync::atomic::Ordering::Relaxed);
                    self.shared.play_pos_f.store(0.0, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    pub fn stop(&self) {
        self.shared.playing.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_loop_enabled(&self, en: bool) {
        self.shared.loop_enabled.store(en, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_loop_region(&self, start: usize, end: usize) {
        self.shared.loop_start.store(start, std::sync::atomic::Ordering::Relaxed);
        self.shared.loop_end.store(end, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_rate(&self, rate: f32) {
        self.shared.rate.store(rate.clamp(0.25, 4.0), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn seek_to_sample(&self, pos: usize) {
        // Clamp to buffer length if present
        if let Some(buf) = self.shared.samples.load().as_ref() {
            let len = buf.len();
            let p = pos.min(len);
            self.shared.play_pos.store(p, std::sync::atomic::Ordering::Relaxed);
            self.shared.play_pos_f.store(p as f32, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
