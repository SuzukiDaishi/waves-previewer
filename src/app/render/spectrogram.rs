use rustfft::{num_complex::Complex, FftPlanner};

use crate::app::types::{SpectrogramConfig, SpectrogramData, WindowFunction};

pub struct SpectrogramParams {
    pub frames: usize,
    pub bins: usize,
    pub frame_step: usize,
    pub win: usize,
    pub window: WindowFunction,
}

pub fn spectrogram_params(len: usize, cfg: &SpectrogramConfig) -> SpectrogramParams {
    let win = cfg.fft_size.max(2);
    let bins = win / 2;
    if len == 0 {
        return SpectrogramParams {
            frames: 0,
            bins,
            frame_step: 1,
            win,
            window: cfg.window,
        };
    }
    let max_frames = cfg.max_frames.max(1);
    let mut frame_step = ((win as f32) * (1.0 - cfg.overlap)).round() as usize;
    if frame_step == 0 {
        frame_step = 1;
    }
    let mut frames = ((len + frame_step - 1) / frame_step).max(1);
    if frames > max_frames {
        frame_step = (len / max_frames).max(1);
        frames = ((len + frame_step - 1) / frame_step).max(1);
    }
    SpectrogramParams {
        frames,
        bins,
        frame_step,
        win,
        window: cfg.window,
    }
}

pub fn compute_spectrogram_tile(
    mono: &[f32],
    _sample_rate: u32,
    params: &SpectrogramParams,
    start_frame: usize,
    end_frame: usize,
) -> Vec<f32> {
    let win = params.win;
    let bins = params.bins;
    let len = mono.len();
    if len == 0 || params.frames == 0 || start_frame >= end_frame {
        return Vec::new();
    }
    let end_frame = end_frame.min(params.frames);
    let frame_count = end_frame.saturating_sub(start_frame);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(win);
    let window = match params.window {
        WindowFunction::Hann => hann_window(win),
        WindowFunction::BlackmanHarris => blackman_harris_window(win),
    };
    let mut buffer = vec![Complex { re: 0.0, im: 0.0 }; win];
    let mut values_db = Vec::with_capacity(frame_count * bins);

    for frame in start_frame..end_frame {
        let center = frame.saturating_mul(params.frame_step);
        let start = center.saturating_sub(win / 2);
        for i in 0..win {
            let idx = start + i;
            let sample = if idx < len { mono[idx] } else { 0.0 };
            buffer[i].re = sample * window[i];
            buffer[i].im = 0.0;
        }
        fft.process(&mut buffer);
        for b in 0..bins {
            let c = buffer[b];
            let mag = (c.re * c.re + c.im * c.im).sqrt();
            let db = 20.0 * (mag.max(1e-9)).log10();
            values_db.push(db);
        }
    }

    values_db
}

#[allow(dead_code)]
pub fn compute_spectrogram(mono: &[f32], sample_rate: u32) -> SpectrogramData {
    let params = spectrogram_params(mono.len(), &SpectrogramConfig::default());
    if params.frames == 0 {
        return SpectrogramData {
            frames: 0,
            bins: params.bins,
            frame_step: params.frame_step,
            sample_rate,
            values_db: Vec::new(),
        };
    }
    let values_db = compute_spectrogram_tile(mono, sample_rate, &params, 0, params.frames);

    SpectrogramData {
        frames: params.frames,
        bins: params.bins,
        frame_step: params.frame_step,
        sample_rate,
        values_db,
    }
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
