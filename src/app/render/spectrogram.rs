use rustfft::{num_complex::Complex, FftPlanner};

use crate::app::types::SpectrogramData;

pub fn compute_spectrogram(mono: &[f32], sample_rate: u32) -> SpectrogramData {
    let win = 2048usize;
    let bins = win / 2;
    let len = mono.len();
    if len == 0 {
        return SpectrogramData {
            frames: 0,
            bins,
            frame_step: 1,
            sample_rate,
            values_db: Vec::new(),
        };
    }

    let max_frames = 2048usize;
    let min_step = win / 8;
    let mut frame_step = len / max_frames;
    if frame_step < min_step {
        frame_step = min_step;
    }
    if frame_step == 0 {
        frame_step = 1;
    }
    let frames = ((len + frame_step - 1) / frame_step).max(1);

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(win);

    let window = hann_window(win);
    let mut buffer = vec![Complex { re: 0.0, im: 0.0 }; win];
    let mut values_db = Vec::with_capacity(frames * bins);

    for frame in 0..frames {
        let center = frame.saturating_mul(frame_step);
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

    SpectrogramData {
        frames,
        bins,
        frame_step,
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
