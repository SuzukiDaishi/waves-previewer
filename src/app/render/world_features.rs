//! F0 trajectory and spectral envelope analysis based on the WORLD vocoder.
//!
//! This is an independent pure-Rust port of the core analysis algorithms from
//! the WORLD speech analysis/synthesis system by Masanori Morise
//! (<https://github.com/mmorise/World>, BSD-3-Clause):
//!
//! * DIO (`dio.cpp`): F0 estimation from zero-crossing interval analysis on
//!   multiple octave-band low-passed signals.
//! * StoneMask (`stonemask.cpp`): F0 refinement using instantaneous frequency.
//! * CheapTrick (`cheaptrick.cpp`): pitch-adaptive spectral envelope
//!   estimation.
//!
//! The port follows the reference C++ implementation closely; deviations are
//! noted inline (the anti-aliasing decimation filter is realized in the
//! frequency domain instead of WORLD's hard-coded IIR, and a tiny positive
//! clamp guards the log of the smoothed power spectrum).

// Not wired into the renderer yet; remove once a view calls `analyze_world`.
#![allow(dead_code)]

use realfft::RealFftPlanner;
use rustfft::num_complex::Complex;

/// DIO default F0 search floor in Hz (WORLD `DioOption::f0_floor`).
const F0_FLOOR_HZ: f64 = 71.0;
/// DIO default F0 search ceiling in Hz (WORLD `DioOption::f0_ceil`).
const F0_CEIL_HZ: f64 = 800.0;
/// Number of DIO filter channels per octave (WORLD `channels_in_octave`).
const CHANNELS_IN_OCTAVE: f64 = 2.0;
/// Maximum relative F0 jump between adjacent frames (WORLD `allowed_range`).
const ALLOWED_RANGE: f64 = 0.1;
/// DIO low-cut filter cutoff in Hz (WORLD `kCutOff`).
const LOW_CUT_HZ: f64 = 50.0;
/// Guard against division by zero (WORLD `kMySafeGuardMinimum`).
const SAFE_GUARD_MINIMUM: f64 = 0.000_000_000_001;
/// Score assigned to unusable F0 candidates (WORLD `kMaximumValue`).
const MAXIMUM_SCORE: f64 = 100_000.0;
/// F0 used by CheapTrick for unvoiced frames (WORLD `kDefaultF0`).
const DEFAULT_F0_HZ: f64 = 500.0;
/// CheapTrick spectral recovery lifter coefficient (WORLD `q1`).
const CHEAPTRICK_Q1: f64 = -0.15;
/// StoneMask refuses to refine F0 values at or below this (WORLD `kFloorF0StoneMask`).
const STONEMASK_F0_FLOOR_HZ: f64 = 40.0;
/// Magnitude of the noise added to the smoothed power spectrum (WORLD `kEps`).
const INFINITESIMAL_NOISE: f64 = 2.220_446_049_250_313e-16;
/// Sample rate DIO decimates the input towards before band analysis.
///
/// WORLD exposes this as the `speed` option (decimation ratio 1..=12); a
/// decimated rate of ~4 kHz keeps `f0_ceil` comfortably below Nyquist.
const DIO_TARGET_FS: f64 = 4000.0;

/// F0 contour and CheapTrick spectral envelope for a mono clip.
pub struct WorldFeatures {
    pub frame_period_ms: f64,
    pub sample_rate: u32,
    /// CheapTrick FFT size.
    pub fft_size: usize,
    pub f0_floor: f64,
    pub f0_ceil: f64,
    /// Refined F0 per frame in Hz; `0.0` marks an unvoiced frame.
    pub f0: Vec<f32>,
    /// Spectral envelope, `frames * bins` values in dB (`10*log10(power)`).
    pub envelope_db: Vec<f32>,
    pub frames: usize,
    /// `fft_size / 2 + 1`.
    pub bins: usize,
}

/// Runs DIO + StoneMask + CheapTrick on a mono signal.
pub fn analyze_world(mono: &[f32], sample_rate: u32, frame_period_ms: f64) -> WorldFeatures {
    let fft_size = cheaptrick_fft_size(sample_rate.max(1));
    let bins = fft_size / 2 + 1;
    if mono.is_empty() || sample_rate == 0 || !(frame_period_ms > 0.0) {
        return WorldFeatures {
            frame_period_ms,
            sample_rate,
            fft_size,
            f0_floor: F0_FLOOR_HZ,
            f0_ceil: F0_CEIL_HZ,
            f0: Vec::new(),
            envelope_db: Vec::new(),
            frames: 0,
            bins,
        };
    }

    let fs = sample_rate as f64;
    let x: Vec<f64> = mono.iter().map(|&v| v as f64).collect();
    // WORLD GetSamplesForDIO: shared by DIO and CheapTrick so the F0 contour
    // and the envelope have identical frame counts.
    let frames = (1000.0 * x.len() as f64 / fs / frame_period_ms) as usize + 1;
    let temporal_positions: Vec<f64> = (0..frames)
        .map(|i| i as f64 * frame_period_ms / 1000.0)
        .collect();

    let mut planner = RealFftPlanner::<f64>::new();
    let f0_dio = dio_f0(&x, fs, frame_period_ms, &temporal_positions, &mut planner);
    let f0 = stonemask(&x, fs, &temporal_positions, &f0_dio, &mut planner);
    let envelope_db = cheaptrick(&x, fs, &temporal_positions, &f0, fft_size, &mut planner);

    WorldFeatures {
        frame_period_ms,
        sample_rate,
        fft_size,
        f0_floor: F0_FLOOR_HZ,
        f0_ceil: F0_CEIL_HZ,
        f0: f0.iter().map(|&v| v as f32).collect(),
        envelope_db,
        frames,
        bins,
    }
}

// ---------------------------------------------------------------------------
// Shared helpers (WORLD matlabfunctions.cpp / common.cpp equivalents)
// ---------------------------------------------------------------------------

/// Rounds half away from zero (WORLD `matlab_round`).
fn matlab_round(x: f64) -> i64 {
    if x > 0.0 {
        (x + 0.5) as i64
    } else {
        (x - 0.5) as i64
    }
}

/// Smallest power of two strictly greater than `sample` for `sample >= 1`
/// (WORLD `GetSuitableFFTSize`).
fn get_suitable_fft_size(sample: usize) -> usize {
    let mut size = 1usize;
    while size <= sample {
        size <<= 1;
    }
    size
}

/// Nuttall window used by DIO's band low-pass filters (WORLD `NuttallWindow`).
fn nuttall_window(len: usize, out: &mut [f64]) {
    for (i, v) in out.iter_mut().take(len).enumerate() {
        let t = i as f64 / (len as f64 - 1.0);
        *v = 0.355768 - 0.487396 * (2.0 * std::f64::consts::PI * t).cos()
            + 0.144232 * (4.0 * std::f64::consts::PI * t).cos()
            - 0.012604 * (6.0 * std::f64::consts::PI * t).cos();
    }
}

/// Linear interpolation with linear extrapolation beyond the edges
/// (equivalent to WORLD `interp1`). `x` must be increasing.
fn interp1(x: &[f64], y: &[f64], xi: &[f64], yi: &mut [f64]) {
    debug_assert_eq!(x.len(), y.len());
    if x.is_empty() {
        yi.fill(0.0);
        return;
    }
    if x.len() == 1 {
        yi.fill(y[0]);
        return;
    }
    for (out, &q) in yi.iter_mut().zip(xi.iter()) {
        let seg = x
            .partition_point(|&v| v <= q)
            .saturating_sub(1)
            .min(x.len() - 2);
        let dx = x[seg + 1] - x[seg];
        *out = if dx > 0.0 {
            y[seg] + (y[seg + 1] - y[seg]) * (q - x[seg]) / dx
        } else {
            y[seg]
        };
    }
}

/// Interpolation over an equidistant abscissa starting at `x0` with step `dx`
/// (WORLD `interp1Q`).
fn interp1q(x0: f64, dx: f64, y: &[f64], xi: &[f64], yi: &mut [f64]) {
    debug_assert!(!y.is_empty());
    for (out, &q) in yi.iter_mut().zip(xi.iter()) {
        let pos = (q - x0) / dx;
        let base = (pos as i64).clamp(0, y.len() as i64 - 1) as usize;
        let frac = pos - base as f64;
        let delta = if base + 1 < y.len() {
            y[base + 1] - y[base]
        } else {
            0.0
        };
        *out = y[base] + delta * frac;
    }
}

/// Forward real FFT of `time` zero-padded to `fft_size`.
fn real_fft(
    planner: &mut RealFftPlanner<f64>,
    time: &[f64],
    fft_size: usize,
) -> Vec<Complex<f64>> {
    let r2c = planner.plan_fft_forward(fft_size);
    let mut input = vec![0.0f64; fft_size];
    let n = time.len().min(fft_size);
    input[..n].copy_from_slice(&time[..n]);
    let mut spectrum = r2c.make_output_vec();
    r2c.process(&mut input, &mut spectrum)
        .expect("real FFT failed");
    spectrum
}

/// Inverse real FFT normalized by `1 / fft_size`.
fn real_ifft(
    planner: &mut RealFftPlanner<f64>,
    spectrum: &mut [Complex<f64>],
    fft_size: usize,
) -> Vec<f64> {
    let c2r = planner.plan_fft_inverse(fft_size);
    spectrum[0].im = 0.0;
    if fft_size % 2 == 0 {
        let last = spectrum.len() - 1;
        spectrum[last].im = 0.0;
    }
    let mut out = vec![0.0f64; fft_size];
    c2r.process(spectrum, &mut out)
        .expect("inverse real FFT failed");
    let scale = 1.0 / fft_size as f64;
    for v in &mut out {
        *v *= scale;
    }
    out
}

/// WORLD's xorshift-based `randn()` (deterministic, seeded like the
/// reference so results are reproducible run to run).
struct WorldRandn {
    x: u32,
    y: u32,
    z: u32,
    w: u32,
}

impl WorldRandn {
    fn new() -> Self {
        Self {
            x: 123456789,
            y: 362436069,
            z: 521288629,
            w: 88675123,
        }
    }

    fn next_u32(&mut self) -> u32 {
        let t = self.x ^ (self.x << 11);
        self.x = self.y;
        self.y = self.z;
        self.z = self.w;
        self.w = (self.w ^ (self.w >> 19)) ^ (t ^ (t >> 8));
        self.w
    }

    /// Approximately N(0, 1): sum of 12 uniforms minus 6.
    fn randn(&mut self) -> f64 {
        let mut acc = 0u64;
        for _ in 0..12 {
            acc += (self.next_u32() >> 4) as u64;
        }
        acc as f64 / 268_435_456.0 - 6.0
    }
}

// ---------------------------------------------------------------------------
// DIO (dio.cpp)
// ---------------------------------------------------------------------------

/// Zero-crossing intervals of one event type: `(interval_locations, intervals)`
/// in seconds and Hz respectively.
type ZeroCrossings = (Vec<f64>, Vec<f64>);

/// DIO F0 estimation. Returns one F0 value per temporal position
/// (0.0 = unvoiced).
fn dio_f0(
    x: &[f64],
    fs: f64,
    frame_period_ms: f64,
    temporal_positions: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f64> {
    let f0_length = temporal_positions.len();
    let number_of_bands = 1 + ((F0_CEIL_HZ / F0_FLOOR_HZ).log2() * CHANNELS_IN_OCTAVE) as usize;
    let boundary_f0_list: Vec<f64> = (0..number_of_bands)
        .map(|i| F0_FLOOR_HZ * 2f64.powf((i as f64 + 1.0) / CHANNELS_IN_OCTAVE))
        .collect();

    // Decimation ratio (WORLD `speed`, 1..=12), chosen so the decimated rate
    // stays close to DIO_TARGET_FS while keeping f0_ceil below Nyquist.
    let mut decimation_ratio = ((fs / DIO_TARGET_FS) as usize).clamp(1, 12);
    while decimation_ratio > 1 && fs / decimation_ratio as f64 <= 2.0 * F0_CEIL_HZ {
        decimation_ratio -= 1;
    }
    let y = decimate(x, decimation_ratio, planner);
    let actual_fs = fs / decimation_ratio as f64;
    let y_length = y.len();

    let fft_size = get_suitable_fft_size(
        y_length
            + (matlab_round(actual_fs / LOW_CUT_HZ) as usize) * 2
            + 1
            + 4 * (1.0 + actual_fs / boundary_f0_list[0] / 2.0) as usize,
    );
    let y_spectrum = spectrum_for_estimation(&y, fft_size, actual_fs, planner);

    let mut f0_candidates = vec![vec![0.0f64; f0_length]; number_of_bands];
    let mut f0_scores = vec![vec![0.0f64; f0_length]; number_of_bands];
    let mut candidate = vec![0.0f64; f0_length];
    let mut score = vec![0.0f64; f0_length];
    for (band, &boundary_f0) in boundary_f0_list.iter().enumerate() {
        get_f0_candidate_from_raw_event(
            boundary_f0,
            actual_fs,
            &y_spectrum,
            y_length,
            fft_size,
            temporal_positions,
            &mut candidate,
            &mut score,
            planner,
        );
        for j in 0..f0_length {
            // Normalization avoiding zero division (dio.cpp GetF0CandidatesAndScores).
            f0_scores[band][j] = score[j] / (candidate[j] + SAFE_GUARD_MINIMUM);
            f0_candidates[band][j] = candidate[j];
        }
    }

    // Best candidate per frame = lowest score (dio.cpp GetBestF0Contour).
    let mut best_f0_contour = vec![0.0f64; f0_length];
    for i in 0..f0_length {
        let mut best_score = f0_scores[0][i];
        best_f0_contour[i] = f0_candidates[0][i];
        for band in 1..number_of_bands {
            if f0_scores[band][i] < best_score {
                best_score = f0_scores[band][i];
                best_f0_contour[i] = f0_candidates[band][i];
            }
        }
    }

    fix_f0_contour(frame_period_ms, &f0_candidates, &best_f0_contour)
}

/// Anti-aliased decimation by an integer ratio.
///
/// Deviation from the reference: WORLD (`matlabfunctions.cpp decimate`) uses a
/// zero-phase IIR with hard-coded coefficients per ratio; this port low-passes
/// in the frequency domain (raised-cosine roll-off just below the new
/// Nyquist), which is also zero-phase and alias-free.
fn decimate(x: &[f64], ratio: usize, planner: &mut RealFftPlanner<f64>) -> Vec<f64> {
    if ratio <= 1 {
        return x.to_vec();
    }
    let fft_size = get_suitable_fft_size(x.len());
    let mut spectrum = real_fft(planner, x, fft_size);
    let pass_end = fft_size as f64 / (2.0 * ratio as f64);
    let pass_start = 0.85 * pass_end;
    for (k, bin) in spectrum.iter_mut().enumerate() {
        let kf = k as f64;
        if kf <= pass_start {
            continue;
        }
        let gain = if kf >= pass_end {
            0.0
        } else {
            0.5 * (1.0
                + (std::f64::consts::PI * (kf - pass_start) / (pass_end - pass_start)).cos())
        };
        *bin *= gain;
    }
    let filtered = real_ifft(planner, &mut spectrum, fft_size);
    let y_length = 1 + x.len() / ratio;
    (0..y_length)
        .map(|i| {
            let idx = i * ratio;
            if idx < x.len() {
                filtered[idx]
            } else {
                0.0
            }
        })
        .collect()
}

/// Spectrum of the DC-removed, low-cut-filtered decimated signal
/// (dio.cpp `GetSpectrumForEstimation`).
fn spectrum_for_estimation(
    y: &[f64],
    fft_size: usize,
    actual_fs: f64,
    planner: &mut RealFftPlanner<f64>,
) -> Vec<Complex<f64>> {
    let mean_y = y.iter().sum::<f64>() / y.len().max(1) as f64;
    let centered: Vec<f64> = y.iter().map(|&v| v - mean_y).collect();
    let mut y_spectrum = real_fft(planner, &centered, fft_size);

    // Low-cut filtering below ~50 Hz (from WORLD 0.1.4).
    let n = (matlab_round(actual_fs / LOW_CUT_HZ) as usize) * 2 + 1;
    let low_cut_filter = design_low_cut_filter(n, fft_size);
    let filter_spectrum = real_fft(planner, &low_cut_filter, fft_size);
    for (bin, filt) in y_spectrum.iter_mut().zip(filter_spectrum.iter()) {
        *bin *= *filt;
    }
    y_spectrum
}

/// High-pass FIR built from a normalized Hanning-like smoother
/// (dio.cpp `DesignLowCutFilter`). `n` must be odd.
fn design_low_cut_filter(n: usize, fft_size: usize) -> Vec<f64> {
    let mut filter = vec![0.0f64; fft_size];
    for i in 1..=n {
        filter[i - 1] = 0.5 - 0.5 * (i as f64 * 2.0 * std::f64::consts::PI / (n as f64 + 1.0)).cos();
    }
    let sum: f64 = filter[..n].iter().sum();
    for v in filter[..n].iter_mut() {
        *v = -*v / sum;
    }
    let half = (n - 1) / 2;
    for i in 0..half {
        filter[fft_size - half + i] = filter[i];
    }
    for i in 0..n {
        filter[i] = filter[i + half];
    }
    filter[0] += 1.0;
    filter
}

/// One band of DIO: filter, extract zero-crossing events, interpolate a
/// candidate contour (dio.cpp `GetF0CandidateFromRawEvent`).
#[allow(clippy::too_many_arguments)]
fn get_f0_candidate_from_raw_event(
    boundary_f0: f64,
    fs: f64,
    y_spectrum: &[Complex<f64>],
    y_length: usize,
    fft_size: usize,
    temporal_positions: &[f64],
    f0_candidate: &mut [f64],
    f0_score: &mut [f64],
    planner: &mut RealFftPlanner<f64>,
) {
    let half_average_length = matlab_round(fs / boundary_f0 / 2.0).max(1) as usize;
    let filtered_signal =
        get_filtered_signal(half_average_length, fft_size, y_spectrum, y_length, planner);
    let events = get_four_zero_crossing_intervals(filtered_signal, fs);
    get_f0_candidate_contour(
        &events,
        boundary_f0,
        temporal_positions,
        f0_candidate,
        f0_score,
    );
}

/// Band-limits the signal with a Nuttall-window low-pass whose length adapts
/// to the band's boundary F0 (dio.cpp `GetFilteredSignal`).
fn get_filtered_signal(
    half_average_length: usize,
    fft_size: usize,
    y_spectrum: &[Complex<f64>],
    y_length: usize,
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f64> {
    let filter_length = half_average_length * 4;
    let mut low_pass_filter = vec![0.0f64; filter_length];
    nuttall_window(filter_length, &mut low_pass_filter);
    let filter_spectrum = real_fft(planner, &low_pass_filter, fft_size);

    let mut product: Vec<Complex<f64>> = y_spectrum
        .iter()
        .zip(filter_spectrum.iter())
        .map(|(a, b)| a * b)
        .collect();
    let filtered = real_ifft(planner, &mut product, fft_size);

    // Compensation of the filter group delay.
    let index_bias = half_average_length * 2;
    (0..y_length)
        .map(|i| filtered[(i + index_bias).min(fft_size - 1)])
        .collect()
}

/// Negative/positive zero crossings plus peaks and dips
/// (dio.cpp `GetFourZeroCrossingIntervals`).
fn get_four_zero_crossing_intervals(mut signal: Vec<f64>, fs: f64) -> [ZeroCrossings; 4] {
    let y_length = signal.len();
    let negatives = zero_crossing_engine(&signal, fs);
    for v in signal.iter_mut() {
        *v = -*v;
    }
    let positives = zero_crossing_engine(&signal, fs);
    if y_length < 3 {
        return [negatives, positives, (Vec::new(), Vec::new()), (Vec::new(), Vec::new())];
    }
    for i in 0..y_length - 1 {
        signal[i] -= signal[i + 1];
    }
    let peaks = zero_crossing_engine(&signal[..y_length - 1], fs);
    for v in signal[..y_length - 2].iter_mut() {
        *v = -*v;
    }
    let dips = zero_crossing_engine(&signal[..y_length - 2], fs);
    [negatives, positives, peaks, dips]
}

/// Sub-sample negative-going zero crossings: interval frequencies and their
/// temporal locations (dio.cpp `ZeroCrossingEngine`).
fn zero_crossing_engine(signal: &[f64], fs: f64) -> ZeroCrossings {
    let mut edges: Vec<usize> = Vec::new();
    for i in 0..signal.len().saturating_sub(1) {
        if signal[i] > 0.0 && signal[i + 1] <= 0.0 {
            edges.push(i + 1);
        }
    }
    if edges.len() < 2 {
        return (Vec::new(), Vec::new());
    }
    let fine_edges: Vec<f64> = edges
        .iter()
        .map(|&e| e as f64 - signal[e - 1] / (signal[e] - signal[e - 1]))
        .collect();
    let mut locations = Vec::with_capacity(fine_edges.len() - 1);
    let mut intervals = Vec::with_capacity(fine_edges.len() - 1);
    for w in fine_edges.windows(2) {
        intervals.push(fs / (w[1] - w[0]));
        locations.push((w[0] + w[1]) / 2.0 / fs);
    }
    (locations, intervals)
}

/// Combines the four interval types into an F0 candidate and stability score
/// per frame (dio.cpp `GetF0CandidateContour` + `...Sub`).
fn get_f0_candidate_contour(
    events: &[ZeroCrossings; 4],
    boundary_f0: f64,
    temporal_positions: &[f64],
    f0_candidate: &mut [f64],
    f0_score: &mut [f64],
) {
    // A channel is usable only when all four event types have > 2 intervals.
    let usable = events.iter().all(|(_, intervals)| intervals.len() > 2);
    if !usable {
        f0_candidate.fill(0.0);
        f0_score.fill(MAXIMUM_SCORE);
        return;
    }

    let f0_length = temporal_positions.len();
    let mut interpolated = vec![vec![0.0f64; f0_length]; 4];
    for (dst, (locations, intervals)) in interpolated.iter_mut().zip(events.iter()) {
        interp1(locations, intervals, temporal_positions, dst);
    }

    for i in 0..f0_length {
        let mean =
            (interpolated[0][i] + interpolated[1][i] + interpolated[2][i] + interpolated[3][i])
                / 4.0;
        let deviation = (interpolated
            .iter()
            .map(|set| (set[i] - mean) * (set[i] - mean))
            .sum::<f64>()
            / 3.0)
            .sqrt();
        if mean > boundary_f0 || mean < boundary_f0 / 2.0 || mean > F0_CEIL_HZ || mean < F0_FLOOR_HZ
        {
            f0_candidate[i] = 0.0;
            f0_score[i] = MAXIMUM_SCORE;
        } else {
            f0_candidate[i] = mean;
            f0_score[i] = deviation;
        }
    }
}

/// Four-step post-processing of the best contour
/// (dio.cpp `FixF0Contour` and `FixStep1`..`FixStep4`).
fn fix_f0_contour(
    frame_period_ms: f64,
    f0_candidates: &[Vec<f64>],
    best_f0_contour: &[f64],
) -> Vec<f64> {
    let voice_range_minimum =
        ((0.5 + 1000.0 / frame_period_ms / F0_FLOOR_HZ) as usize) * 2 + 1;
    let f0_step1 = fix_step_1(best_f0_contour, voice_range_minimum);
    let f0_step2 = fix_step_2(&f0_step1, voice_range_minimum);
    let (positive_index, negative_index) = get_voiced_section_edges(&f0_step2);
    let f0_step3 = fix_step_3(&f0_step2, f0_candidates, &negative_index);
    fix_step_4(&f0_step3, f0_candidates, &positive_index)
}

/// Step 1: clear contour edges and frames with excessive F0 jumps.
fn fix_step_1(best_f0_contour: &[f64], voice_range_minimum: usize) -> Vec<f64> {
    let n = best_f0_contour.len();
    let mut f0_base = vec![0.0f64; n];
    if n > voice_range_minimum * 2 {
        f0_base[voice_range_minimum..n - voice_range_minimum]
            .copy_from_slice(&best_f0_contour[voice_range_minimum..n - voice_range_minimum]);
    }
    let mut f0_step1 = vec![0.0f64; n];
    for i in voice_range_minimum..n {
        let jump = ((f0_base[i] - f0_base[i - 1]) / (SAFE_GUARD_MINIMUM + f0_base[i])).abs();
        f0_step1[i] = if jump < ALLOWED_RANGE { f0_base[i] } else { 0.0 };
    }
    f0_step1
}

/// Step 2: remove voiced sections shorter than `voice_range_minimum`.
fn fix_step_2(f0_step1: &[f64], voice_range_minimum: usize) -> Vec<f64> {
    let n = f0_step1.len();
    let mut f0_step2 = f0_step1.to_vec();
    let center = (voice_range_minimum - 1) / 2;
    if n <= center * 2 {
        return f0_step2;
    }
    for i in center..n - center {
        for j in i - center..=i + center {
            if f0_step1[j] == 0.0 {
                f0_step2[i] = 0.0;
                break;
            }
        }
    }
    f0_step2
}

/// Voiced-section onsets and offsets (dio.cpp `GetNumberOfVoicedSections`).
fn get_voiced_section_edges(f0: &[f64]) -> (Vec<usize>, Vec<usize>) {
    let mut positive_index = Vec::new();
    let mut negative_index = Vec::new();
    for i in 1..f0.len() {
        if f0[i] == 0.0 && f0[i - 1] != 0.0 {
            negative_index.push(i - 1);
        } else if f0[i - 1] == 0.0 && f0[i] != 0.0 {
            positive_index.push(i);
        }
    }
    (positive_index, negative_index)
}

/// Picks the candidate closest to a linear extrapolation of the recent
/// contour (dio.cpp `SelectBestF0`).
fn select_best_f0(
    current_f0: f64,
    past_f0: f64,
    f0_candidates: &[Vec<f64>],
    target_index: usize,
) -> f64 {
    let reference_f0 = (current_f0 * 3.0 - past_f0) / 2.0;
    if reference_f0 == 0.0 {
        return 0.0;
    }
    let mut best_f0 = f0_candidates[0][target_index];
    let mut minimum_error = (reference_f0 - best_f0).abs();
    for band in &f0_candidates[1..] {
        let error = (reference_f0 - band[target_index]).abs();
        if error < minimum_error {
            minimum_error = error;
            best_f0 = band[target_index];
        }
    }
    if (1.0 - best_f0 / reference_f0).abs() > ALLOWED_RANGE {
        0.0
    } else {
        best_f0
    }
}

/// Step 3: extend voiced sections forward through candidate continuity.
fn fix_step_3(
    f0_step2: &[f64],
    f0_candidates: &[Vec<f64>],
    negative_index: &[usize],
) -> Vec<f64> {
    let n = f0_step2.len();
    let mut f0_step3 = f0_step2.to_vec();
    for (i, &start) in negative_index.iter().enumerate() {
        let limit = if i == negative_index.len() - 1 {
            n - 1
        } else {
            negative_index[i + 1]
        };
        for j in start..limit {
            if j < 1 || j + 1 >= n {
                break;
            }
            f0_step3[j + 1] =
                select_best_f0(f0_step3[j], f0_step3[j - 1], f0_candidates, j + 1);
            if f0_step3[j + 1] == 0.0 {
                break;
            }
        }
    }
    f0_step3
}

/// Step 4: extend voiced sections backward through candidate continuity.
fn fix_step_4(
    f0_step3: &[f64],
    f0_candidates: &[Vec<f64>],
    positive_index: &[usize],
) -> Vec<f64> {
    let n = f0_step3.len();
    let mut f0_step4 = f0_step3.to_vec();
    for i in (0..positive_index.len()).rev() {
        let limit = if i == 0 { 1 } else { positive_index[i - 1] };
        let mut j = positive_index[i];
        while j > limit {
            if j + 1 >= n {
                break;
            }
            f0_step4[j - 1] =
                select_best_f0(f0_step4[j], f0_step4[j + 1], f0_candidates, j - 1);
            if f0_step4[j - 1] == 0.0 {
                break;
            }
            j -= 1;
        }
    }
    f0_step4
}

// ---------------------------------------------------------------------------
// StoneMask (stonemask.cpp)
// ---------------------------------------------------------------------------

/// Refines each voiced frame's F0 using instantaneous frequency
/// (stonemask.cpp `StoneMask`).
fn stonemask(
    x: &[f64],
    fs: f64,
    temporal_positions: &[f64],
    f0: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f64> {
    temporal_positions
        .iter()
        .zip(f0.iter())
        .map(|(&position, &initial_f0)| get_refined_f0(x, fs, position, initial_f0, planner))
        .collect()
}

/// stonemask.cpp `GetRefinedF0`.
fn get_refined_f0(
    x: &[f64],
    fs: f64,
    current_position: f64,
    initial_f0: f64,
    planner: &mut RealFftPlanner<f64>,
) -> f64 {
    if initial_f0 <= STONEMASK_F0_FLOOR_HZ || initial_f0 > fs / 12.0 {
        return 0.0;
    }
    let half_window_length = (1.5 * fs / initial_f0 + 1.0) as usize;
    let base_time_length = half_window_length * 2 + 1;
    let window_length_in_time = (2.0 * half_window_length as f64 + 1.0) / fs;
    let base_time: Vec<f64> = (0..base_time_length)
        .map(|i| (i as f64 - half_window_length as f64) / fs)
        .collect();
    let fft_size = 1usize << (2 + (base_time_length as f64).log2() as usize);

    let mean_f0 = get_mean_f0(
        x,
        fs,
        current_position,
        initial_f0,
        fft_size,
        window_length_in_time,
        &base_time,
        planner,
    );

    // If the amount of correction is overlarge (20 %), the initial F0 is kept.
    if (mean_f0 - initial_f0).abs() > initial_f0 * 0.2 {
        initial_f0
    } else {
        mean_f0
    }
}

/// stonemask.cpp `GetMeanF0` (spectra + instantaneous-frequency estimate).
#[allow(clippy::too_many_arguments)]
fn get_mean_f0(
    x: &[f64],
    fs: f64,
    current_position: f64,
    current_f0: f64,
    fft_size: usize,
    window_length_in_time: f64,
    base_time: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> f64 {
    let base_time_length = base_time.len();
    let mut main_window = vec![0.0f64; base_time_length];
    let mut waveform = vec![0.0f64; base_time_length];

    // Blackman window over ~3 periods, evaluated on the quantized time axis.
    let mut safe_index = vec![0usize; base_time_length];
    for i in 0..base_time_length {
        let index_raw = matlab_round((current_position + base_time[i]) * fs + 0.001);
        safe_index[i] = index_raw.clamp(0, x.len() as i64 - 1) as usize;
        let window_time = index_raw as f64 / fs - current_position;
        main_window[i] = 0.42
            + 0.5 * (2.0 * std::f64::consts::PI * window_time / window_length_in_time).cos()
            + 0.08 * (4.0 * std::f64::consts::PI * window_time / window_length_in_time).cos();
    }
    let mut diff_window = vec![0.0f64; base_time_length];
    diff_window[0] = -main_window[1] / 2.0;
    for i in 1..base_time_length - 1 {
        diff_window[i] = -(main_window[i + 1] - main_window[i - 1]) / 2.0;
    }
    diff_window[base_time_length - 1] = main_window[base_time_length - 2] / 2.0;

    for i in 0..base_time_length {
        waveform[i] = x[safe_index[i]] * main_window[i];
    }
    let main_spectrum = real_fft(planner, &waveform, fft_size);
    for i in 0..base_time_length {
        waveform[i] = x[safe_index[i]] * diff_window[i];
    }
    let diff_spectrum = real_fft(planner, &waveform, fft_size);

    let bins = fft_size / 2 + 1;
    let mut power_spectrum = vec![0.0f64; bins];
    let mut numerator_i = vec![0.0f64; bins];
    for k in 0..bins {
        numerator_i[k] =
            main_spectrum[k].re * diff_spectrum[k].im - main_spectrum[k].im * diff_spectrum[k].re;
        power_spectrum[k] = main_spectrum[k].norm_sqr();
    }

    // Two-pass refinement (stonemask.cpp GetTentativeF0).
    let tentative_f0 = fix_f0(&power_spectrum, &numerator_i, fft_size, fs, current_f0, 2);
    if tentative_f0 <= 0.0 || tentative_f0 > current_f0 * 2.0 {
        return 0.0;
    }
    fix_f0(&power_spectrum, &numerator_i, fft_size, fs, tentative_f0, 6)
}

/// Amplitude-weighted mean of the instantaneous frequencies at the first
/// `number_of_harmonics` harmonics (stonemask.cpp `FixF0`).
fn fix_f0(
    power_spectrum: &[f64],
    numerator_i: &[f64],
    fft_size: usize,
    fs: f64,
    initial_f0: f64,
    number_of_harmonics: usize,
) -> f64 {
    let mut numerator = 0.0f64;
    let mut denominator = 0.0f64;
    for harmonic in 1..=number_of_harmonics {
        let index = matlab_round(initial_f0 * fft_size as f64 / fs * harmonic as f64)
            .clamp(0, (fft_size / 2) as i64) as usize;
        let instantaneous_frequency = if power_spectrum[index] == 0.0 {
            0.0
        } else {
            index as f64 * fs / fft_size as f64
                + numerator_i[index] / power_spectrum[index] * fs
                    / (2.0 * std::f64::consts::PI)
        };
        let amplitude = power_spectrum[index].sqrt();
        numerator += amplitude * instantaneous_frequency;
        denominator += amplitude * harmonic as f64;
    }
    numerator / (denominator + SAFE_GUARD_MINIMUM)
}

// ---------------------------------------------------------------------------
// CheapTrick (cheaptrick.cpp)
// ---------------------------------------------------------------------------

/// CheapTrick FFT size (cheaptrick.cpp `GetFFTSizeForCheapTrick`).
fn cheaptrick_fft_size(sample_rate: u32) -> usize {
    get_suitable_fft_size((3.0 * sample_rate as f64 / F0_FLOOR_HZ + 1.0) as usize)
}

/// Spectral envelope for every frame, returned as dB (`10*log10(power)`).
fn cheaptrick(
    x: &[f64],
    fs: f64,
    temporal_positions: &[f64],
    f0: &[f64],
    fft_size: usize,
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f32> {
    let bins = fft_size / 2 + 1;
    // Lowest F0 the FFT size supports (cheaptrick.cpp GetF0FloorForCheapTrick).
    let f0_floor = 3.0 * fs / (fft_size as f64 - 3.0);
    let mut rng = WorldRandn::new();
    let mut envelope_db = Vec::with_capacity(temporal_positions.len() * bins);
    let mut envelope_log = vec![0.0f64; bins];
    for (&position, &frame_f0) in temporal_positions.iter().zip(f0.iter()) {
        let current_f0 = if frame_f0 <= f0_floor { DEFAULT_F0_HZ } else { frame_f0 };
        cheaptrick_general_body(
            x,
            fs,
            current_f0,
            fft_size,
            position,
            planner,
            &mut rng,
            &mut envelope_log,
        );
        envelope_db.extend(
            envelope_log
                .iter()
                .map(|&v| (10.0 * v * std::f64::consts::LOG10_E) as f32),
        );
    }
    envelope_db
}

/// Envelope of a single frame; writes the natural log of the envelope power
/// into `envelope_log` (cheaptrick.cpp `CheapTrickGeneralBody`).
#[allow(clippy::too_many_arguments)]
fn cheaptrick_general_body(
    x: &[f64],
    fs: f64,
    current_f0: f64,
    fft_size: usize,
    current_position: f64,
    planner: &mut RealFftPlanner<f64>,
    rng: &mut WorldRandn,
    envelope_log: &mut [f64],
) {
    // First step: F0-adaptive windowing and power spectrum with DC correction.
    let waveform = get_windowed_waveform(x, fs, current_f0, current_position, rng);
    let spectrum = real_fft(planner, &waveform, fft_size);
    let bins = fft_size / 2 + 1;
    let mut power_spectrum: Vec<f64> = spectrum[..bins].iter().map(|c| c.norm_sqr()).collect();
    dc_correction(&mut power_spectrum, current_f0, fs, fft_size);

    // Second step: smoothing of the power spectrum on the linear axis.
    linear_smoothing(&mut power_spectrum, current_f0 * 2.0 / 3.0, fs, fft_size);

    // Third step: infinitesimal noise to avoid log(0).
    for v in power_spectrum.iter_mut() {
        *v += rng.randn().abs() * INFINITESIMAL_NOISE;
    }

    // Fourth step: cepstral smoothing and recovery liftering.
    smoothing_with_recovery(&power_spectrum, current_f0, fs, fft_size, planner, envelope_log);
}

/// Hanning window over 3 periods around `current_position`, RMS-normalized
/// and DC-removed (cheaptrick.cpp `GetWindowedWaveform`).
fn get_windowed_waveform(
    x: &[f64],
    fs: f64,
    current_f0: f64,
    current_position: f64,
    rng: &mut WorldRandn,
) -> Vec<f64> {
    let half_window_length = matlab_round(1.5 * fs / current_f0) as usize;
    let length = half_window_length * 2 + 1;
    let origin = matlab_round(current_position * fs + 0.001);

    let mut window = vec![0.0f64; length];
    let mut average = 0.0f64;
    for (i, w) in window.iter_mut().enumerate() {
        let base_index = i as f64 - half_window_length as f64;
        let position = base_index / 1.5 / fs;
        *w = 0.5 * (std::f64::consts::PI * position * current_f0).cos() + 0.5;
        average += *w * *w;
    }
    let average = average.sqrt();
    for w in window.iter_mut() {
        *w /= average;
    }

    let mut waveform = vec![0.0f64; length];
    let mut tmp_weight1 = 0.0f64;
    let mut tmp_weight2 = 0.0f64;
    for i in 0..length {
        let safe_index = (origin + i as i64 - half_window_length as i64)
            .clamp(0, x.len() as i64 - 1) as usize;
        waveform[i] = x[safe_index] * window[i] + rng.randn() * SAFE_GUARD_MINIMUM;
        tmp_weight1 += waveform[i];
        tmp_weight2 += window[i];
    }
    let weighting_coefficient = tmp_weight1 / tmp_weight2;
    for i in 0..length {
        waveform[i] -= window[i] * weighting_coefficient;
    }
    waveform
}

/// Mirrors sub-F0 spectral energy back above DC (common.cpp `DCCorrection`).
fn dc_correction(power_spectrum: &mut [f64], f0: f64, fs: f64, fft_size: usize) {
    let upper_limit = 2 + (f0 * fft_size as f64 / fs) as usize;
    let upper_limit_replica = upper_limit - 1;
    let low_frequency_axis: Vec<f64> = (0..upper_limit_replica)
        .map(|i| i as f64 * fs / fft_size as f64)
        .collect();
    let mut low_frequency_replica = vec![0.0f64; upper_limit_replica];
    interp1q(
        f0,
        -fs / fft_size as f64,
        &power_spectrum[..(upper_limit + 1).min(power_spectrum.len())],
        &low_frequency_axis,
        &mut low_frequency_replica,
    );
    for (v, replica) in power_spectrum
        .iter_mut()
        .zip(low_frequency_replica.iter())
    {
        *v += replica;
    }
}

/// Rectangular smoothing of the power spectrum over `width` Hz via a
/// cumulative integral (common.cpp `LinearSmoothing`).
fn linear_smoothing(power_spectrum: &mut [f64], width: f64, fs: f64, fft_size: usize) {
    let bins = fft_size / 2 + 1;
    let boundary = (width * fft_size as f64 / fs) as usize + 1;
    let mirror_length = fft_size / 2 + boundary * 2 + 1;

    let mut mirroring_spectrum = vec![0.0f64; mirror_length];
    for i in 0..boundary {
        mirroring_spectrum[i] = power_spectrum[boundary - i];
    }
    for i in boundary..fft_size / 2 + boundary {
        mirroring_spectrum[i] = power_spectrum[i - boundary];
    }
    for i in fft_size / 2 + boundary..mirror_length {
        mirroring_spectrum[i] = power_spectrum[fft_size / 2 - (i - (fft_size / 2 + boundary))];
    }

    // Cumulative integral of the mirrored spectrum.
    let mut mirroring_segment = vec![0.0f64; mirror_length];
    let df = fs / fft_size as f64;
    let mut acc = 0.0f64;
    for (seg, &v) in mirroring_segment.iter_mut().zip(mirroring_spectrum.iter()) {
        acc += v * df;
        *seg = acc;
    }

    let origin_of_mirroring_axis = -(boundary as f64 - 0.5) * df;
    let mut frequency_axis: Vec<f64> = (0..bins).map(|i| i as f64 * df - width / 2.0).collect();
    let mut low_levels = vec![0.0f64; bins];
    let mut high_levels = vec![0.0f64; bins];
    interp1q(
        origin_of_mirroring_axis,
        df,
        &mirroring_segment,
        &frequency_axis,
        &mut low_levels,
    );
    for v in frequency_axis.iter_mut() {
        *v += width;
    }
    interp1q(
        origin_of_mirroring_axis,
        df,
        &mirroring_segment,
        &frequency_axis,
        &mut high_levels,
    );
    for i in 0..bins {
        power_spectrum[i] = (high_levels[i] - low_levels[i]) / width;
    }
}

/// Cepstral smoothing of the log spectrum plus recovery liftering; writes the
/// log envelope (cheaptrick.cpp `SmoothingWithRecovery`).
fn smoothing_with_recovery(
    power_spectrum: &[f64],
    f0: f64,
    fs: f64,
    fft_size: usize,
    planner: &mut RealFftPlanner<f64>,
    envelope_log: &mut [f64],
) {
    let bins = fft_size / 2 + 1;
    // Log power spectrum, mirrored to a full symmetric sequence.
    let mut log_spectrum = vec![0.0f64; fft_size];
    for i in 0..bins {
        // Deviation from the reference: clamp to the smallest positive double
        // so rounding in LinearSmoothing can never feed log() a non-positive
        // value.
        log_spectrum[i] = power_spectrum[i].max(f64::MIN_POSITIVE).ln();
    }
    for i in 1..fft_size / 2 {
        log_spectrum[fft_size - i] = log_spectrum[i];
    }
    let cepstrum = real_fft(planner, &log_spectrum, fft_size);

    // WORLD divides by fft_size here and runs an unnormalized inverse FFT;
    // real_ifft is normalized, so the two factors cancel and the lifters are
    // applied without extra scaling.
    let mut liftered: Vec<Complex<f64>> = Vec::with_capacity(bins);
    liftered.push(Complex::new(cepstrum[0].re, 0.0));
    for i in 1..bins {
        let quefrency = i as f64 / fs;
        let smoothing_lifter = (std::f64::consts::PI * f0 * quefrency).sin()
            / (std::f64::consts::PI * f0 * quefrency);
        let compensation_lifter = (1.0 - 2.0 * CHEAPTRICK_Q1)
            + 2.0 * CHEAPTRICK_Q1 * (2.0 * std::f64::consts::PI * quefrency * f0).cos();
        liftered.push(Complex::new(
            cepstrum[i].re * smoothing_lifter * compensation_lifter,
            0.0,
        ));
    }
    let smoothed = real_ifft(planner, &mut liftered, fft_size);
    envelope_log.copy_from_slice(&smoothed[..bins]);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FS: u32 = 48_000;
    const FRAME_PERIOD_MS: f64 = 5.0;

    fn sine(freq: f64, secs: f64, fs: u32) -> Vec<f32> {
        let n = (secs * fs as f64) as usize;
        (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / fs as f64).sin() as f32 * 0.5
            })
            .collect()
    }

    /// Linear chirp from `f_start` to `f_end` Hz over `secs`.
    fn sweep(f_start: f64, f_end: f64, secs: f64, fs: u32) -> Vec<f32> {
        let n = (secs * fs as f64) as usize;
        let rate = (f_end - f_start) / secs;
        (0..n)
            .map(|i| {
                let t = i as f64 / fs as f64;
                let phase = 2.0 * std::f64::consts::PI * (f_start * t + 0.5 * rate * t * t);
                phase.sin() as f32 * 0.5
            })
            .collect()
    }

    fn voiced_values(f0: &[f32]) -> Vec<f32> {
        f0.iter().copied().filter(|&v| v > 0.0).collect()
    }

    fn median(values: &[f32]) -> f32 {
        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        sorted[sorted.len() / 2]
    }

    #[test]
    fn sine_440_f0() {
        let signal = sine(440.0, 1.0, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let voiced = voiced_values(&features.f0);
        let voiced_ratio = voiced.len() as f64 / features.frames as f64;
        assert!(
            voiced_ratio > 0.8,
            "expected >80% voiced frames, got {:.1}%",
            voiced_ratio * 100.0
        );
        let med = median(&voiced);
        assert!(
            (med - 440.0).abs() < 5.0,
            "median voiced F0 {med} not within 5 Hz of 440"
        );
    }

    #[test]
    fn sine_100_f0() {
        let signal = sine(100.0, 1.0, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let voiced = voiced_values(&features.f0);
        assert!(
            voiced.len() as f64 / features.frames as f64 > 0.5,
            "expected mostly voiced frames for a 100 Hz sine"
        );
        let med = median(&voiced);
        assert!(
            (med - 100.0).abs() < 5.0,
            "median voiced F0 {med} not within 5 Hz of 100"
        );
    }

    #[test]
    fn silence_is_unvoiced() {
        let signal = vec![0.0f32; TEST_FS as usize / 2];
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        assert!(features.frames > 0);
        assert!(
            features.f0.iter().all(|&v| v == 0.0),
            "silence produced voiced frames"
        );
        assert!(
            features.envelope_db.iter().all(|v| v.is_finite()),
            "silence envelope contains non-finite values"
        );
    }

    #[test]
    fn noise_is_mostly_unvoiced() {
        // Deterministic LCG white-ish noise.
        let mut state = 0x1234_5678_9abc_def0u64;
        let signal: Vec<f32> = (0..TEST_FS as usize)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((state >> 33) as f64 / (1u64 << 31) as f64 - 1.0) as f32 * 0.5
            })
            .collect();
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let voiced_ratio =
            voiced_values(&features.f0).len() as f64 / features.frames as f64;
        assert!(
            voiced_ratio < 0.35,
            "expected mostly unvoiced frames for noise, got {:.1}% voiced",
            voiced_ratio * 100.0
        );
    }

    #[test]
    fn sweep_f0_increases() {
        let signal = sweep(150.0, 300.0, 1.0, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        // Ignore the contour edges where DIO clears frames by design.
        let inner: Vec<f32> = features.f0[features.frames / 10..features.frames * 9 / 10].to_vec();
        let voiced: Vec<f32> = voiced_values(&inner);
        assert!(
            voiced.len() as f64 / inner.len() as f64 > 0.6,
            "expected mostly voiced frames for the sweep"
        );
        // Monotonically increasing with a small tolerance for refinement noise.
        for pair in voiced.windows(2) {
            assert!(
                pair[1] >= pair[0] - 3.0,
                "sweep F0 decreased: {} -> {}",
                pair[0],
                pair[1]
            );
        }
        assert!(
            voiced[voiced.len() - 1] - voiced[0] > 80.0,
            "sweep F0 did not rise enough: {} -> {}",
            voiced[0],
            voiced[voiced.len() - 1]
        );
    }

    #[test]
    fn envelope_peaks_near_440() {
        let signal = sine(440.0, 1.0, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        assert!(
            features.envelope_db.iter().all(|v| v.is_finite()),
            "envelope contains NaN or infinite values"
        );
        let bins = features.bins;
        let hz_per_bin = TEST_FS as f32 / features.fft_size as f32;
        let frame = features.frames / 2;
        let row = &features.envelope_db[frame * bins..(frame + 1) * bins];
        // WORLD's DC correction mirrors sub-F0 energy downward, so the
        // envelope is intentionally flat below F0; search above F0/2.
        let lo = (220.0 / hz_per_bin).round() as usize;
        let hi = (800.0 / hz_per_bin).round() as usize;
        let peak_bin = (lo..=hi)
            .max_by(|&a, &b| row[a].partial_cmp(&row[b]).unwrap())
            .unwrap();
        let peak_hz = peak_bin as f32 * hz_per_bin;
        assert!(
            (340.0..=540.0).contains(&peak_hz),
            "envelope peak at {peak_hz} Hz, expected near 440"
        );
        let at_440 = row[(440.0 / hz_per_bin).round() as usize];
        let at_3000 = row[(3000.0 / hz_per_bin).round() as usize];
        assert!(
            at_440 - at_3000 > 15.0,
            "envelope at 440 Hz ({at_440} dB) not well above 3 kHz ({at_3000} dB)"
        );
    }

    #[test]
    fn frame_counts_are_consistent() {
        let signal = sine(220.0, 0.73, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let expected_frames =
            (1000.0 * signal.len() as f64 / TEST_FS as f64 / FRAME_PERIOD_MS) as usize + 1;
        assert_eq!(features.frames, expected_frames);
        assert_eq!(features.f0.len(), features.frames);
        assert_eq!(features.bins, features.fft_size / 2 + 1);
        assert_eq!(features.envelope_db.len(), features.frames * features.bins);
    }

    #[test]
    fn empty_input_yields_no_frames() {
        let features = analyze_world(&[], TEST_FS, FRAME_PERIOD_MS);
        assert_eq!(features.frames, 0);
        assert!(features.f0.is_empty());
        assert!(features.envelope_db.is_empty());
    }

    #[test]
    fn timing_one_second() {
        let signal = sine(440.0, 1.0, TEST_FS);
        let start = std::time::Instant::now();
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let elapsed = start.elapsed();
        println!(
            "analyze_world: 1.0 s @ 48 kHz -> {} frames x {} bins in {:.1?}",
            features.frames, features.bins, elapsed
        );
        assert!(features.frames > 0);
    }

    #[test]
    #[ignore = "timing benchmark; run with --ignored"]
    fn timing_ten_seconds() {
        let signal = sine(440.0, 10.0, TEST_FS);
        let start = std::time::Instant::now();
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let elapsed = start.elapsed();
        println!(
            "analyze_world: 10.0 s @ 48 kHz -> {} frames x {} bins in {:.1?}",
            features.frames, features.bins, elapsed
        );
        assert!(features.frames > 0);
    }
}
