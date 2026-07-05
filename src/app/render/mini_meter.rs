//! DSP helpers for the editor mini meter strip (spectrum analyzer and
//! stereo correlation). Kept free of egui types so the math can be unit
//! tested and benchmarked without a UI harness.
//!
//! The spectrum analyzer is dual-resolution: a long FFT feeds the low
//! band (accurate bass) and a short FFT feeds the high band (fast
//! response), blended across a crossover region. Display smoothing uses
//! an asymmetric attack/release envelope so bars rise almost instantly
//! and fall back promptly when the signal goes quiet.

use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::cell::RefCell;
use std::sync::Arc;

/// Display floor for the analyzer, in dBFS.
pub const SPECTRUM_DB_FLOOR: f32 = -84.0;
/// Lowest frequency shown on the log axis.
pub const SPECTRUM_MIN_HZ: f32 = 20.0;
/// Below this frequency only the long FFT is used.
const CROSSOVER_LO_HZ: f32 = 420.0;
/// Above this frequency only the short FFT is used.
const CROSSOVER_HI_HZ: f32 = 1_100.0;
/// Analysis window length of the long (low-band) FFT, in seconds.
const LOW_FFT_WINDOW_SECS: f32 = 0.17;
/// Analysis window length of the short (high-band) FFT, in seconds.
const HIGH_FFT_WINDOW_SECS: f32 = 0.043;
/// Smoothing time constant while the level is rising.
pub const SPECTRUM_ATTACK_SECS: f32 = 0.010;
/// Smoothing time constant while the level is falling.
pub const SPECTRUM_RELEASE_SECS: f32 = 0.10;

fn next_pow2(n: usize) -> usize {
    n.max(2).next_power_of_two()
}

/// (long, short) FFT sizes used for the given sample rate.
pub fn spectrum_fft_sizes(sample_rate: u32) -> (usize, usize) {
    let sr = sample_rate.max(1) as f32;
    let low = next_pow2((sr * LOW_FFT_WINDOW_SECS) as usize).clamp(2_048, 16_384);
    let high = next_pow2((sr * HIGH_FFT_WINDOW_SECS) as usize).clamp(512, 4_096);
    (low, high.min(low))
}

/// Samples of history the analyzer wants for the given sample rate.
pub fn spectrum_history_len(sample_rate: u32) -> usize {
    spectrum_fft_sizes(sample_rate).0
}

struct FftScratch {
    plans: Vec<(usize, Arc<dyn Fft<f32>>)>,
    buffer: Vec<Complex<f32>>,
    mags_low: Vec<f32>,
    mags_high: Vec<f32>,
}

thread_local! {
    static SCRATCH: RefCell<FftScratch> = RefCell::new(FftScratch {
        plans: Vec::new(),
        buffer: Vec::new(),
        mags_low: Vec::new(),
        mags_high: Vec::new(),
    });
}

impl FftScratch {
    fn plan(&mut self, n: usize) -> Arc<dyn Fft<f32>> {
        if let Some((_, fft)) = self.plans.iter().find(|(size, _)| *size == n) {
            return fft.clone();
        }
        let fft = FftPlanner::new().plan_fft_forward(n);
        self.plans.push((n, fft.clone()));
        if self.plans.len() > 4 {
            self.plans.remove(0);
        }
        fft
    }

    /// Hann-windowed magnitude spectrum of the last `n_fft` samples of
    /// `mono`, normalized so a full-scale sine peaks near 1.0.
    fn magnitudes(&mut self, mono: &[f32], n_fft: usize, out: &mut Vec<f32>) {
        let take = n_fft.min(mono.len());
        out.clear();
        if take < 64 {
            out.resize(n_fft / 2, 0.0);
            return;
        }
        self.buffer.clear();
        self.buffer.resize(n_fft, Complex { re: 0.0, im: 0.0 });
        let src = &mono[mono.len() - take..];
        let denom = (take - 1).max(1) as f32;
        let mut window_sum = 0.0f32;
        for (i, slot) in self.buffer.iter_mut().take(take).enumerate() {
            let hann = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / denom).cos();
            window_sum += hann;
            slot.re = src[i] * hann;
        }
        let fft = self.plan(n_fft);
        fft.process(&mut self.buffer);
        let scale = 2.0 / window_sum.max(1.0);
        out.extend(self.buffer[..n_fft / 2].iter().map(|c| c.norm() * scale));
    }
}

/// Peak level over `[f0, f1)` Hz taken from a magnitude spectrum,
/// interpolating between bins when the band is narrower than one bin so
/// low frequencies render as a smooth curve instead of stair steps.
fn band_level(mags: &[f32], nyquist: f32, f0: f32, f1: f32) -> f32 {
    let bins = mags.len();
    if bins == 0 || nyquist <= 0.0 {
        return 0.0;
    }
    let bin_hz = nyquist / bins as f32;
    let p0 = (f0 / bin_hz).max(0.0);
    let p1 = (f1 / bin_hz).max(p0);
    if p1 - p0 <= 1.0 {
        let pc = 0.5 * (p0 + p1);
        let i = (pc.floor() as usize).min(bins.saturating_sub(1));
        let frac = (pc - i as f32).clamp(0.0, 1.0);
        let a = mags[i];
        let b = mags.get(i + 1).copied().unwrap_or(a);
        a + (b - a) * frac
    } else {
        let b0 = (p0 as usize).min(bins.saturating_sub(1));
        let b1 = (p1.ceil() as usize).clamp(b0 + 1, bins);
        mags[b0..b1].iter().fold(0.0f32, |acc, v| acc.max(*v))
    }
}

fn amp_to_db(v: f32) -> f32 {
    20.0 * v.max(1.0e-9).log10()
}

/// Fill `out_db` with one dB value per display column over a log
/// frequency axis from [`SPECTRUM_MIN_HZ`] to Nyquist. `mono` should
/// hold at least [`spectrum_history_len`] samples ending at the
/// playhead; shorter input is handled gracefully.
pub fn spectrum_columns(mono: &[f32], sample_rate: u32, cols: usize, out_db: &mut Vec<f32>) {
    out_db.clear();
    out_db.resize(cols, SPECTRUM_DB_FLOOR);
    if cols == 0 || mono.len() < 128 {
        return;
    }
    let sr = sample_rate.max(1) as f32;
    let nyquist = sr * 0.5;
    let f_lo = SPECTRUM_MIN_HZ.min(nyquist * 0.25);
    let f_hi = nyquist.max(f_lo * 2.0);
    let (n_low, n_high) = spectrum_fft_sizes(sample_rate);
    SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        let mut mags_low = std::mem::take(&mut scratch.mags_low);
        let mut mags_high = std::mem::take(&mut scratch.mags_high);
        scratch.magnitudes(mono, n_low, &mut mags_low);
        scratch.magnitudes(mono, n_high, &mut mags_high);
        let ratio = (f_hi / f_lo).max(1.0001);
        for (x, slot) in out_db.iter_mut().enumerate() {
            let t0 = x as f32 / cols as f32;
            let t1 = (x + 1) as f32 / cols as f32;
            let f0 = f_lo * ratio.powf(t0);
            let f1 = f_lo * ratio.powf(t1);
            let f_center = (f0 * f1).sqrt();
            let db = if f_center <= CROSSOVER_LO_HZ {
                amp_to_db(band_level(&mags_low, nyquist, f0, f1))
            } else if f_center >= CROSSOVER_HI_HZ {
                amp_to_db(band_level(&mags_high, nyquist, f0, f1))
            } else {
                let lo_db = amp_to_db(band_level(&mags_low, nyquist, f0, f1));
                let hi_db = amp_to_db(band_level(&mags_high, nyquist, f0, f1));
                let t = (f_center / CROSSOVER_LO_HZ).ln()
                    / (CROSSOVER_HI_HZ / CROSSOVER_LO_HZ).ln();
                lo_db + (hi_db - lo_db) * t.clamp(0.0, 1.0)
            };
            *slot = db.max(SPECTRUM_DB_FLOOR);
        }
        scratch.mags_low = mags_low;
        scratch.mags_high = mags_high;
    });
}

/// Advance the smoothed spectrum toward `target` with a fast attack and
/// a prompt release, so bars track transients but fall back quickly when
/// the signal goes quiet. `dt` is the elapsed time since the last call.
pub fn smooth_spectrum_db(smoothed: &mut Vec<f32>, target: &[f32], dt: f32) {
    if smoothed.len() != target.len() {
        smoothed.clear();
        smoothed.resize(target.len(), SPECTRUM_DB_FLOOR);
    }
    let dt = dt.clamp(0.0, 0.25);
    let attack = 1.0 - (-dt / SPECTRUM_ATTACK_SECS).exp();
    let release = 1.0 - (-dt / SPECTRUM_RELEASE_SECS).exp();
    for (cur, tgt) in smoothed.iter_mut().zip(target.iter()) {
        let k = if *tgt > *cur { attack } else { release };
        *cur += (*tgt - *cur) * k;
        if *cur < SPECTRUM_DB_FLOOR + 0.25 {
            *cur = SPECTRUM_DB_FLOOR;
        }
    }
}

/// Zero-lag correlation of two channels in [-1, 1]. Near-silence maps to
/// 0 (neutral) rather than an arbitrary sign.
pub fn stereo_correlation(l: &[f32], r: &[f32]) -> f32 {
    let n = l.len().min(r.len());
    if n == 0 {
        return 0.0;
    }
    let mut ll = 0.0f64;
    let mut rr = 0.0f64;
    let mut lr = 0.0f64;
    for i in 0..n {
        let a = l[i] as f64;
        let b = r[i] as f64;
        ll += a * a;
        rr += b * b;
        lr += a * b;
    }
    let denom = (ll * rr).sqrt();
    if denom < 1.0e-10 {
        return 0.0;
    }
    (lr / denom).clamp(-1.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sr: u32, secs: f32, amp: f32) -> Vec<f32> {
        let n = (sr as f32 * secs) as usize;
        (0..n)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin() * amp)
            .collect()
    }

    fn column_center_freq(col: usize, cols: usize, sr: u32) -> f32 {
        let nyquist = sr as f32 * 0.5;
        let f_lo = SPECTRUM_MIN_HZ.min(nyquist * 0.25);
        let f_hi = nyquist.max(f_lo * 2.0);
        let t = (col as f32 + 0.5) / cols as f32;
        f_lo * (f_hi / f_lo).powf(t)
    }

    fn peak_column_freq(mono: &[f32], sr: u32, cols: usize) -> f32 {
        let mut out = Vec::new();
        spectrum_columns(mono, sr, cols, &mut out);
        let (idx, _) = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        column_center_freq(idx, cols, sr)
    }

    #[test]
    fn low_frequency_peak_is_accurate() {
        // 55 Hz would smear across a whole octave with a short FFT; the
        // long low-band FFT must localize it.
        let mono = sine(55.0, 48_000, 1.0, 0.5);
        let peak = peak_column_freq(&mono, 48_000, 480);
        assert!(
            (peak - 55.0).abs() / 55.0 < 0.08,
            "low peak off: got {peak} Hz"
        );
    }

    #[test]
    fn mid_and_high_peaks_are_accurate() {
        for freq in [440.0f32, 1_000.0, 8_000.0] {
            let mono = sine(freq, 48_000, 0.5, 0.5);
            let peak = peak_column_freq(&mono, 48_000, 480);
            assert!(
                (peak - freq).abs() / freq < 0.08,
                "peak for {freq} Hz off: got {peak} Hz"
            );
        }
    }

    #[test]
    fn full_scale_sine_lands_near_zero_db() {
        let mono = sine(1_000.0, 48_000, 0.5, 1.0);
        let mut out = Vec::new();
        spectrum_columns(&mono, 48_000, 480, &mut out);
        let max = out.iter().fold(f32::MIN, |a, v| a.max(*v));
        assert!(max.abs() < 2.0, "expected ~0 dBFS, got {max}");
    }

    #[test]
    fn silence_stays_at_floor() {
        let mono = vec![0.0f32; 16_384];
        let mut out = Vec::new();
        spectrum_columns(&mono, 48_000, 300, &mut out);
        assert!(out.iter().all(|db| *db <= SPECTRUM_DB_FLOOR + 0.001));
    }

    #[test]
    fn smoothing_attacks_fast_and_releases_promptly() {
        let cols = 8usize;
        let mut smoothed = vec![SPECTRUM_DB_FLOOR; cols];
        let loud = vec![-6.0f32; cols];
        // One 33 ms frame of attack should get within a few dB.
        smooth_spectrum_db(&mut smoothed, &loud, 0.033);
        assert!(smoothed[0] > -12.0, "attack too slow: {}", smoothed[0]);
        // Half a second of silence must fall back to the floor.
        let quiet = vec![SPECTRUM_DB_FLOOR; cols];
        for _ in 0..15 {
            smooth_spectrum_db(&mut smoothed, &quiet, 0.033);
        }
        assert!(
            smoothed[0] <= SPECTRUM_DB_FLOOR + 2.0,
            "release too slow: {}",
            smoothed[0]
        );
    }

    #[test]
    fn correlation_tracks_phase_relation() {
        let l = sine(440.0, 48_000, 0.1, 0.7);
        let inverted: Vec<f32> = l.iter().map(|v| -v).collect();
        let quadrature: Vec<f32> = (0..l.len())
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).cos() * 0.7)
            .collect();
        assert!(stereo_correlation(&l, &l) > 0.99);
        assert!(stereo_correlation(&l, &inverted) < -0.99);
        assert!(stereo_correlation(&l, &quadrature).abs() < 0.05);
        assert_eq!(stereo_correlation(&[0.0; 512], &[0.0; 512]), 0.0);
    }

    #[test]
    fn spectrum_frame_cost_fits_a_30fps_budget() {
        // Debug builds are much slower than release; this guards against
        // gross regressions (release cost is measured by the ignored
        // test below).
        let mono = sine(220.0, 48_000, 0.4, 0.5);
        let mut out = Vec::new();
        spectrum_columns(&mono, 48_000, 600, &mut out); // warm plans
        let start = std::time::Instant::now();
        let iters = 20u32;
        for _ in 0..iters {
            spectrum_columns(&mono, 48_000, 600, &mut out);
        }
        let per_frame = start.elapsed().as_secs_f64() * 1000.0 / iters as f64;
        eprintln!("spectrum_columns: {per_frame:.3} ms/frame (debug)");
        assert!(
            per_frame < 33.0,
            "spectrum frame cost {per_frame:.2} ms exceeds the 33 ms frame budget"
        );
    }

    #[test]
    #[ignore = "manual perf measurement (run with --release)"]
    fn spectrum_frame_cost_release_measurement() {
        let mono = sine(220.0, 48_000, 0.4, 0.5);
        let mut out = Vec::new();
        spectrum_columns(&mono, 48_000, 600, &mut out);
        let start = std::time::Instant::now();
        let iters = 500u32;
        for _ in 0..iters {
            spectrum_columns(&mono, 48_000, 600, &mut out);
        }
        let per_frame = start.elapsed().as_secs_f64() * 1000.0 / iters as f64;
        eprintln!("spectrum_columns: {per_frame:.3} ms/frame (this profile)");
    }
}
