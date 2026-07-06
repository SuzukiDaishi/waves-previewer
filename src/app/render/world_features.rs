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
//! * D4C (`d4c.cpp`): band aperiodicity estimation with the LoveTrain
//!   voiced/unvoiced decision.
//! * Synthesis (`synthesis.cpp`): minimum-phase excitation synthesis from
//!   (F0, spectral envelope, aperiodicity).
//!
//! The port follows the reference C++ implementation closely; deviations are
//! noted inline (the anti-aliasing decimation filter is realized in the
//! frequency domain instead of WORLD's hard-coded IIR, and tiny positive
//! clamps guard logarithms and divisions that the reference leaves bare).

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
/// D4C voiced/unvoiced LoveTrain threshold (WORLD `D4COption::threshold`).
const D4C_THRESHOLD: f64 = 0.85;
/// Lowest F0 D4C's own FFT size is designed for (WORLD `kFloorF0D4C`).
const D4C_F0_FLOOR_HZ: f64 = 47.0;
/// Upper limit of the coarse aperiodicity bands in Hz (WORLD `kUpperLimit`).
const D4C_UPPER_LIMIT_HZ: f64 = 15_000.0;
/// Spacing of the coarse aperiodicity bands in Hz (WORLD `kFrequencyInterval`).
const D4C_FREQUENCY_INTERVAL_HZ: f64 = 3_000.0;
/// Lowest F0 assumed by the LoveTrain V/UV analysis (`lowest_f0` in d4c.cpp).
const D4C_LOVE_TRAIN_LOWEST_F0_HZ: f64 = 40.0;
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
    /// D4C band aperiodicity, `frames * bins` linear values in `0..1`
    /// (`1.0` = fully aperiodic; unvoiced frames are all `~1.0`).
    pub aperiodicity: Vec<f32>,
    pub frames: usize,
    /// `fft_size / 2 + 1`.
    pub bins: usize,
}

/// F0 estimator selection for [`analyze_world_with_options`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorldF0Estimator {
    /// DIO + StoneMask refinement: fast, WORLD's default.
    Dio,
    /// Harvest: slower but more accurate, fewer voiced/unvoiced errors.
    Harvest,
}

/// Runs DIO + StoneMask + CheapTrick on a mono signal.
// Production callers report progress; the unit tests use this wrapper.
#[cfg_attr(not(test), allow(dead_code))]
pub fn analyze_world(mono: &[f32], sample_rate: u32, frame_period_ms: f64) -> WorldFeatures {
    analyze_world_with_progress(mono, sample_rate, frame_period_ms, None)
}

/// [`analyze_world`] with an optional progress sink (called with 0.0..=1.0
/// from the analysis thread; keep the callback cheap).
pub fn analyze_world_with_progress(
    mono: &[f32],
    sample_rate: u32,
    frame_period_ms: f64,
    progress: Option<&dyn Fn(f32)>,
) -> WorldFeatures {
    analyze_world_with_options(
        mono,
        sample_rate,
        frame_period_ms,
        WorldF0Estimator::Dio,
        progress,
    )
}

/// [`analyze_world_with_progress`] with a selectable F0 estimator.
pub fn analyze_world_with_options(
    mono: &[f32],
    sample_rate: u32,
    frame_period_ms: f64,
    estimator: WorldF0Estimator,
    progress: Option<&dyn Fn(f32)>,
) -> WorldFeatures {
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
            aperiodicity: Vec::new(),
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
    let report = |v: f32| {
        if let Some(cb) = progress {
            cb(v.clamp(0.0, 1.0));
        }
    };
    report(0.01);
    let f0 = match estimator {
        WorldF0Estimator::Dio => {
            let f0_dio = dio_f0(
                &x,
                fs,
                frame_period_ms,
                &temporal_positions,
                &mut planner,
                &|p| report(0.01 + p * 0.34),
            );
            report(0.35);
            stonemask(&x, fs, &temporal_positions, &f0_dio, &mut planner, &|p| {
                report(0.35 + p * 0.20)
            })
        }
        // Harvest refines candidates by instantaneous frequency itself, so no
        // StoneMask pass follows (matches the reference tool chain).
        WorldF0Estimator::Harvest => {
            harvest_f0(&x, fs, &temporal_positions, &mut planner, &|p| {
                report(0.01 + p * 0.54)
            })
        }
    };
    report(0.55);
    let envelope_db = cheaptrick(
        &x,
        fs,
        &temporal_positions,
        &f0,
        fft_size,
        &mut planner,
        &|p| report(0.55 + p * 0.25),
    );
    report(0.80);
    let aperiodicity = d4c(
        &x,
        fs,
        &temporal_positions,
        &f0,
        fft_size,
        &mut planner,
        &|p| report(0.80 + p * 0.19),
    );
    report(1.0);

    WorldFeatures {
        frame_period_ms,
        sample_rate,
        fft_size,
        f0_floor: F0_FLOOR_HZ,
        f0_ceil: F0_CEIL_HZ,
        f0: f0.iter().map(|&v| v as f32).collect(),
        envelope_db,
        aperiodicity,
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
    progress: &dyn Fn(f32),
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
        progress(0.1 + 0.9 * band as f32 / number_of_bands.max(1) as f32);
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
    progress: &dyn Fn(f32),
) -> Vec<f64> {
    let total = temporal_positions.len().max(1);
    temporal_positions
        .iter()
        .zip(f0.iter())
        .enumerate()
        .map(|(i, (&position, &initial_f0))| {
            if i % 256 == 0 {
                progress(i as f32 / total as f32);
            }
            get_refined_f0(x, fs, position, initial_f0, planner)
        })
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
// Harvest (harvest.cpp)
// ---------------------------------------------------------------------------

/// Sample rate Harvest decimates the input towards (harvest.cpp `target_fs`).
const HARVEST_TARGET_FS: f64 = 8000.0;
/// Filter-bank density (harvest.cpp `channels_in_octave`).
const HARVEST_CHANNELS_IN_OCTAVE: f64 = 40.0;
/// Frames whose refined score falls below this are dropped
/// (harvest.cpp `GetRefinedF0`).
const HARVEST_SCORE_THRESHOLD: f64 = 2.5;

/// Harvest F0 estimation (harvest.cpp `Harvest`). The contour is estimated on
/// the reference's 1 ms basic grid and then sampled at `temporal_positions`.
fn harvest_f0(
    x: &[f64],
    fs: f64,
    temporal_positions: &[f64],
    planner: &mut RealFftPlanner<f64>,
    progress: &dyn Fn(f32),
) -> Vec<f64> {
    let basic_frames = (1000.0 * x.len() as f64 / fs) as usize + 1;
    let basic_f0 = harvest_general_body(x, fs, basic_frames, planner, progress);
    if basic_f0.is_empty() {
        return vec![0.0; temporal_positions.len()];
    }
    temporal_positions
        .iter()
        .map(|&t| basic_f0[(matlab_round(t * 1000.0).max(0) as usize).min(basic_frames - 1)])
        .collect()
}

/// F0 contour on a 1 ms grid (harvest.cpp `HarvestGeneralBody`).
fn harvest_general_body(
    x: &[f64],
    fs: f64,
    f0_length: usize,
    planner: &mut RealFftPlanner<f64>,
    progress: &dyn Fn(f32),
) -> Vec<f64> {
    if x.is_empty() || f0_length == 0 {
        return vec![0.0; f0_length];
    }
    let adjusted_f0_floor = F0_FLOOR_HZ * 0.9;
    let adjusted_f0_ceil = F0_CEIL_HZ * 1.1;
    let number_of_channels = 1
        + ((adjusted_f0_ceil / adjusted_f0_floor).log2() * HARVEST_CHANNELS_IN_OCTAVE) as usize;
    let boundary_f0_list: Vec<f64> = (0..number_of_channels)
        .map(|i| adjusted_f0_floor * 2f64.powf((i as f64 + 1.0) / HARVEST_CHANNELS_IN_OCTAVE))
        .collect();

    let decimation_ratio = matlab_round(fs / HARVEST_TARGET_FS).clamp(1, 12) as usize;
    let y_length = (x.len() as f64 / decimation_ratio as f64).ceil() as usize;
    let actual_fs = fs / decimation_ratio as f64;
    let fft_size = get_suitable_fft_size(
        y_length + 5 + 2 * (2.0 * actual_fs / boundary_f0_list[0]) as usize,
    );

    // Downsampled waveform and its spectrum (GetWaveformAndSpectrum). Unlike
    // DIO there is no low-cut filter, only DC removal; the DC-removed signal
    // is also what the refinement stage windows later.
    let mut y = harvest_downsampled_waveform(x, y_length, decimation_ratio, planner);
    let mean_y = y.iter().sum::<f64>() / y.len().max(1) as f64;
    for v in y.iter_mut() {
        *v -= mean_y;
    }
    let y_spectrum = real_fft(planner, &y, fft_size);

    let temporal_positions: Vec<f64> = (0..f0_length).map(|i| i as f64 / 1000.0).collect();

    let overlap_parameter = 7usize;
    let max_candidates =
        (matlab_round(number_of_channels as f64 / 10.0) as usize).max(1) * overlap_parameter;

    // Stage 1: raw candidates per filter-bank channel (GetRawF0Candidates).
    let mut raw_f0_candidates = vec![vec![0.0f64; f0_length]; number_of_channels];
    for (i, (raw, &boundary_f0)) in raw_f0_candidates
        .iter_mut()
        .zip(boundary_f0_list.iter())
        .enumerate()
    {
        if i % 8 == 0 {
            progress(0.30 * i as f32 / number_of_channels as f32);
        }
        harvest_f0_candidate_from_raw_event(
            boundary_f0,
            actual_fs,
            &y_spectrum,
            y_length,
            fft_size,
            &temporal_positions,
            raw,
            planner,
        );
    }
    progress(0.30);

    // Stage 2: merge channels into per-frame candidate lists and spread them
    // to neighbouring frames (DetectOfficialF0Candidates + Overlap).
    let (mut f0_candidates, base_candidates) = harvest_detect_official_f0_candidates(
        &raw_f0_candidates,
        f0_length,
        max_candidates,
        overlap_parameter,
    );
    drop(raw_f0_candidates);
    harvest_overlap_f0_candidates(&mut f0_candidates, base_candidates);
    let number_of_candidates = base_candidates * overlap_parameter;

    // Stage 3: refine every candidate by instantaneous frequency and score it
    // (RefineF0Candidates). This dominates the runtime, so frames are split
    // across worker threads.
    let mut f0_scores = vec![vec![0.0f64; max_candidates]; f0_length];
    harvest_refine_f0_candidates(
        &y,
        actual_fs,
        &temporal_positions,
        number_of_candidates,
        &mut f0_candidates,
        &mut f0_scores,
        &|p| progress(0.30 + 0.60 * p),
    );
    harvest_remove_unreliable_candidates(number_of_candidates, &mut f0_candidates, &mut f0_scores);
    progress(0.95);

    // Stage 4: contour selection, fixing, and smoothing.
    let best_f0_contour = harvest_fix_f0_contour(&f0_candidates, &f0_scores, number_of_candidates);
    let smoothed = harvest_smooth_f0_contour(&best_f0_contour);
    progress(1.0);
    smoothed
}

/// MATLAB-compatible edge-padded decimation
/// (harvest.cpp `GetWaveformAndSpectrumSub`).
fn harvest_downsampled_waveform(
    x: &[f64],
    y_length: usize,
    decimation_ratio: usize,
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f64> {
    if decimation_ratio <= 1 {
        return x.to_vec();
    }
    let lag = ((140.0 / decimation_ratio as f64).ceil() as usize) * decimation_ratio;
    let mut extended = Vec::with_capacity(x.len() + lag * 2);
    extended.extend(std::iter::repeat(x[0]).take(lag));
    extended.extend_from_slice(x);
    extended.extend(std::iter::repeat(x[x.len() - 1]).take(lag));
    let decimated = decimate(&extended, decimation_ratio, planner);
    let offset = lag / decimation_ratio;
    (0..y_length)
        .map(|i| {
            decimated
                .get(offset + i)
                .copied()
                .unwrap_or_else(|| *decimated.last().unwrap_or(&0.0))
        })
        .collect()
}

/// One filter-bank channel: band-pass around `boundary_f0`, zero-crossing
/// events, interpolated candidate contour
/// (harvest.cpp `GetF0CandidateFromRawEvent`).
#[allow(clippy::too_many_arguments)]
fn harvest_f0_candidate_from_raw_event(
    boundary_f0: f64,
    fs: f64,
    y_spectrum: &[Complex<f64>],
    y_length: usize,
    fft_size: usize,
    temporal_positions: &[f64],
    f0_candidate: &mut [f64],
    planner: &mut RealFftPlanner<f64>,
) {
    let filtered = harvest_filtered_signal(boundary_f0, fft_size, fs, y_spectrum, y_length, planner);
    let events = harvest_four_zero_crossing_intervals(filtered, fs);
    harvest_f0_candidate_contour(&events, boundary_f0, temporal_positions, f0_candidate);
}

/// Band-pass filtering with a cosine-modulated Nuttall window
/// (harvest.cpp `GetFilteredSignal`).
fn harvest_filtered_signal(
    boundary_f0: f64,
    fft_size: usize,
    fs: f64,
    y_spectrum: &[Complex<f64>],
    y_length: usize,
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f64> {
    let filter_length_half = matlab_round(fs / boundary_f0 * 2.0).max(1) as usize;
    let filter_length = filter_length_half * 2 + 1;
    let mut band_pass_filter = vec![0.0f64; filter_length];
    nuttall_window(filter_length, &mut band_pass_filter);
    for (i, v) in band_pass_filter.iter_mut().enumerate() {
        let k = i as f64 - filter_length_half as f64;
        *v *= (2.0 * std::f64::consts::PI * boundary_f0 * k / fs).cos();
    }
    let filter_spectrum = real_fft(planner, &band_pass_filter, fft_size);
    let mut product: Vec<Complex<f64>> = y_spectrum
        .iter()
        .zip(filter_spectrum.iter())
        .map(|(a, b)| a * b)
        .collect();
    let filtered = real_ifft(planner, &mut product, fft_size);

    // Compensation of the filter group delay.
    let index_bias = filter_length_half + 1;
    (0..y_length)
        .map(|i| filtered[(i + index_bias).min(fft_size - 1)])
        .collect()
}

/// Negative/positive zero crossings plus peaks and dips
/// (harvest.cpp `GetFourZeroCrossingIntervals`).
fn harvest_four_zero_crossing_intervals(mut signal: Vec<f64>, fs: f64) -> [ZeroCrossings; 4] {
    let y_length = signal.len();
    let negatives = zero_crossing_engine(&signal, fs);
    for v in signal.iter_mut() {
        *v = -*v;
    }
    let positives = zero_crossing_engine(&signal, fs);
    if y_length < 2 {
        return [
            negatives,
            positives,
            (Vec::new(), Vec::new()),
            (Vec::new(), Vec::new()),
        ];
    }
    for i in 0..y_length - 1 {
        signal[i] -= signal[i + 1];
    }
    let peaks = zero_crossing_engine(&signal[..y_length - 1], fs);
    for v in signal[..y_length - 1].iter_mut() {
        *v = -*v;
    }
    let dips = zero_crossing_engine(&signal[..y_length - 1], fs);
    [negatives, positives, peaks, dips]
}

/// Interpolates the four event trains and keeps only frames whose mean lies
/// close to the channel's boundary F0 (harvest.cpp `GetF0CandidateContour`).
fn harvest_f0_candidate_contour(
    events: &[ZeroCrossings; 4],
    boundary_f0: f64,
    temporal_positions: &[f64],
    f0_candidate: &mut [f64],
) {
    let usable = events.iter().all(|(_, intervals)| intervals.len() > 2);
    if !usable {
        f0_candidate.fill(0.0);
        return;
    }
    let f0_length = temporal_positions.len();
    let mut interpolated = vec![vec![0.0f64; f0_length]; 4];
    for (dst, (locations, intervals)) in interpolated.iter_mut().zip(events.iter()) {
        interp1(locations, intervals, temporal_positions, dst);
    }
    let upper = boundary_f0 * 1.1;
    let lower = boundary_f0 * 0.9;
    for (i, out) in f0_candidate.iter_mut().enumerate() {
        let mean =
            (interpolated[0][i] + interpolated[1][i] + interpolated[2][i] + interpolated[3][i])
                / 4.0;
        *out = if mean > upper || mean < lower || mean > F0_CEIL_HZ || mean < F0_FLOOR_HZ {
            0.0
        } else {
            mean
        };
    }
}

/// Collapses per-channel candidates into per-frame candidate lists: runs of
/// >= 10 adjacent voiced channels average into one candidate
/// (harvest.cpp `DetectOfficialF0Candidates`). Returns the per-frame candidate
/// matrix (`f0_length x max_candidates`) and the base candidate count.
fn harvest_detect_official_f0_candidates(
    raw_f0_candidates: &[Vec<f64>],
    f0_length: usize,
    max_candidates: usize,
    overlap_parameter: usize,
) -> (Vec<Vec<f64>>, usize) {
    let number_of_channels = raw_f0_candidates.len();
    let mut f0_candidates = vec![vec![0.0f64; max_candidates]; f0_length];
    // Overlapping (x7) must fit into max_candidates columns.
    let base_limit = (max_candidates / overlap_parameter).max(1);
    let mut number_of_candidates = 0usize;
    if number_of_channels < 2 {
        return (f0_candidates, 0);
    }
    let mut vuv = vec![0i32; number_of_channels];
    for (i, frame) in f0_candidates.iter_mut().enumerate() {
        for (flag, channel) in vuv.iter_mut().zip(raw_f0_candidates.iter()) {
            *flag = if channel[i] > 0.0 { 1 } else { 0 };
        }
        vuv[0] = 0;
        vuv[number_of_channels - 1] = 0;
        let mut count = 0usize;
        let mut section_start = 0usize;
        for j in 1..number_of_channels {
            let step = vuv[j] - vuv[j - 1];
            if step == 1 {
                section_start = j;
            }
            if step == -1 && j - section_start >= 10 && count < base_limit {
                let mean = raw_f0_candidates[section_start..j]
                    .iter()
                    .map(|channel| channel[i])
                    .sum::<f64>()
                    / (j - section_start) as f64;
                frame[count] = mean;
                count += 1;
            }
        }
        number_of_candidates = number_of_candidates.max(count);
    }
    (f0_candidates, number_of_candidates)
}

/// Spreads each frame's candidates to the three frames on either side
/// (harvest.cpp `OverlapF0Candidates`).
fn harvest_overlap_f0_candidates(f0_candidates: &mut [Vec<f64>], number_of_candidates: usize) {
    let n = 3usize;
    let f0_length = f0_candidates.len();
    if number_of_candidates == 0 {
        return;
    }
    for i in 1..=n {
        for j in 0..number_of_candidates {
            for k in i..f0_length {
                let v = f0_candidates[k - i][j];
                f0_candidates[k][j + number_of_candidates * i] = v;
            }
            for k in 0..f0_length.saturating_sub(i) {
                let v = f0_candidates[k + i][j];
                f0_candidates[k][j + number_of_candidates * (i + n)] = v;
            }
        }
    }
}

/// Refines every candidate by instantaneous frequency, in parallel across
/// frames (harvest.cpp `RefineF0Candidates`).
fn harvest_refine_f0_candidates(
    y: &[f64],
    fs: f64,
    temporal_positions: &[f64],
    number_of_candidates: usize,
    f0_candidates: &mut [Vec<f64>],
    f0_scores: &mut [Vec<f64>],
    progress: &dyn Fn(f32),
) {
    let f0_length = f0_candidates.len();
    if f0_length == 0 || number_of_candidates == 0 {
        progress(1.0);
        return;
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let chunk_len = f0_length.div_ceil(threads);
    let done = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for ((cand_chunk, score_chunk), position_chunk) in f0_candidates
            .chunks_mut(chunk_len)
            .zip(f0_scores.chunks_mut(chunk_len))
            .zip(temporal_positions.chunks(chunk_len))
        {
            let done = &done;
            scope.spawn(move || {
                let mut planner = RealFftPlanner::<f64>::new();
                for ((candidates, scores), &position) in cand_chunk
                    .iter_mut()
                    .zip(score_chunk.iter_mut())
                    .zip(position_chunk.iter())
                {
                    for j in 0..number_of_candidates {
                        let (refined, score) =
                            harvest_refined_f0(y, fs, position, candidates[j], &mut planner);
                        candidates[j] = refined;
                        scores[j] = score;
                    }
                    done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
        loop {
            let finished = done.load(std::sync::atomic::Ordering::Relaxed);
            progress(finished as f32 / f0_length as f32);
            if finished >= f0_length {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    });
}

/// Instantaneous-frequency refinement of one candidate; returns
/// `(refined_f0, score)`, both zero when rejected
/// (harvest.cpp `GetRefinedF0` + `GetMeanF0` + `FixF0`).
fn harvest_refined_f0(
    y: &[f64],
    fs: f64,
    current_position: f64,
    current_f0: f64,
    planner: &mut RealFftPlanner<f64>,
) -> (f64, f64) {
    if current_f0 <= 0.0 {
        return (0.0, 0.0);
    }
    let half_window_length = (1.5 * fs / current_f0 + 1.0) as usize;
    let base_time_length = half_window_length * 2 + 1;
    let window_length_in_time = base_time_length as f64 / fs;
    let fft_size = 2 * get_suitable_fft_size(base_time_length);
    let bins = fft_size / 2 + 1;

    // Blackman window centred on the analysis position and its derivative.
    let basic_index =
        matlab_round((current_position - half_window_length as f64 / fs) * fs + 0.001);
    let mut main_window = vec![0.0f64; base_time_length];
    for (i, w) in main_window.iter_mut().enumerate() {
        let t = (basic_index as f64 + i as f64 - 1.0) / fs - current_position;
        *w = 0.42
            + 0.5 * (2.0 * std::f64::consts::PI * t / window_length_in_time).cos()
            + 0.08 * (4.0 * std::f64::consts::PI * t / window_length_in_time).cos();
    }
    let mut diff_window = vec![0.0f64; base_time_length];
    diff_window[0] = -main_window[1] / 2.0;
    for i in 1..base_time_length - 1 {
        diff_window[i] = -(main_window[i + 1] - main_window[i - 1]) / 2.0;
    }
    diff_window[base_time_length - 1] = main_window[base_time_length - 2] / 2.0;

    // Spectra of the waveform windowed by both windows (GetSpectra).
    let mut main_waveform = vec![0.0f64; base_time_length];
    let mut diff_waveform = vec![0.0f64; base_time_length];
    for i in 0..base_time_length {
        let safe_index = (basic_index + i as i64 - 1).clamp(0, y.len() as i64 - 1) as usize;
        main_waveform[i] = y[safe_index] * main_window[i];
        diff_waveform[i] = y[safe_index] * diff_window[i];
    }
    let main_spectrum = real_fft(planner, &main_waveform, fft_size);
    let diff_spectrum = real_fft(planner, &diff_waveform, fft_size);

    let number_of_harmonics = ((fs / 2.0 / current_f0) as usize).clamp(1, 6);
    let mut numerator = 0.0f64;
    let mut denominator = 0.0f64;
    let mut score = 0.0f64;
    for harmonic in 1..=number_of_harmonics {
        let index = (matlab_round(current_f0 * fft_size as f64 / fs * harmonic as f64).max(0)
            as usize)
            .min(bins - 1);
        let power = main_spectrum[index].norm_sqr();
        let instantaneous_frequency = if power == 0.0 {
            0.0
        } else {
            let numerator_i = main_spectrum[index].re * diff_spectrum[index].im
                - main_spectrum[index].im * diff_spectrum[index].re;
            index as f64 * fs / fft_size as f64
                + numerator_i / power * fs / (2.0 * std::f64::consts::PI)
        };
        let amplitude = power.sqrt();
        numerator += amplitude * instantaneous_frequency;
        denominator += amplitude * harmonic as f64;
        score += ((instantaneous_frequency / harmonic as f64 - current_f0) / current_f0).abs();
    }
    let refined_f0 = numerator / (denominator + SAFE_GUARD_MINIMUM);
    let refined_score =
        1.0 / (score / number_of_harmonics as f64 + SAFE_GUARD_MINIMUM);
    if refined_f0 < F0_FLOOR_HZ || refined_f0 > F0_CEIL_HZ || refined_score < HARVEST_SCORE_THRESHOLD
    {
        (0.0, 0.0)
    } else {
        (refined_f0, refined_score)
    }
}

/// Nearest candidate to `reference_f0` within `allowed_range` relative error;
/// returns `(best_f0, best_error)` (harvest.cpp `SelectBestF0`).
fn harvest_select_best_f0(
    reference_f0: f64,
    f0_candidates: &[f64],
    allowed_range: f64,
) -> (f64, f64) {
    let mut best_f0 = 0.0f64;
    let mut best_error = allowed_range;
    for &candidate in f0_candidates {
        let error = (reference_f0 - candidate).abs() / reference_f0;
        if error > best_error {
            continue;
        }
        best_f0 = candidate;
        best_error = error;
    }
    (best_f0, best_error)
}

/// Zeros candidates with no close match in either neighbouring frame
/// (harvest.cpp `RemoveUnreliableCandidates`).
fn harvest_remove_unreliable_candidates(
    number_of_candidates: usize,
    f0_candidates: &mut [Vec<f64>],
    f0_scores: &mut [Vec<f64>],
) {
    let f0_length = f0_candidates.len();
    if f0_length < 3 || number_of_candidates == 0 {
        return;
    }
    let snapshot: Vec<Vec<f64>> = f0_candidates.to_vec();
    let threshold = 0.05f64;
    for i in 1..f0_length - 1 {
        for j in 0..number_of_candidates {
            let reference_f0 = f0_candidates[i][j];
            if reference_f0 == 0.0 {
                continue;
            }
            let (_, error1) = harvest_select_best_f0(
                reference_f0,
                &snapshot[i + 1][..number_of_candidates],
                1.0,
            );
            let (_, error2) = harvest_select_best_f0(
                reference_f0,
                &snapshot[i - 1][..number_of_candidates],
                1.0,
            );
            if error1.min(error2) > threshold {
                f0_candidates[i][j] = 0.0;
                f0_scores[i][j] = 0.0;
            }
        }
    }
}

/// Start/end frame pairs of voiced sections; ends are inclusive
/// (harvest.cpp `GetBoundaryList`).
fn harvest_boundary_list(f0: &[f64]) -> Vec<usize> {
    let n = f0.len();
    if n < 2 {
        return Vec::new();
    }
    let mut vuv: Vec<i32> = f0.iter().map(|&v| if v > 0.0 { 1 } else { 0 }).collect();
    vuv[0] = 0;
    vuv[n - 1] = 0;
    let mut list = Vec::new();
    for i in 1..n {
        if vuv[i] != vuv[i - 1] {
            list.push(i - list.len() % 2);
        }
    }
    list
}

/// One buffer per voiced section, zero elsewhere
/// (harvest.cpp `GetMultiChannelF0`).
fn harvest_multi_channel_f0(f0: &[f64], boundary_list: &[usize]) -> Vec<Vec<f64>> {
    (0..boundary_list.len() / 2)
        .map(|i| {
            let mut channel = vec![0.0f64; f0.len()];
            let (st, ed) = (boundary_list[i * 2], boundary_list[i * 2 + 1]);
            channel[st..=ed].copy_from_slice(&f0[st..=ed]);
            channel
        })
        .collect()
}

/// Contour selection and the four fixing steps
/// (harvest.cpp `FixF0Contour`, parameters as in the reference).
fn harvest_fix_f0_contour(
    f0_candidates: &[Vec<f64>],
    f0_scores: &[Vec<f64>],
    number_of_candidates: usize,
) -> Vec<f64> {
    let base = harvest_search_f0_base(f0_candidates, f0_scores, number_of_candidates);
    let step1 = harvest_fix_step_1(&base, 0.008);
    let step2 = harvest_fix_step_2(&step1, 6);
    let step3 = harvest_fix_step_3(&step2, f0_candidates, f0_scores, number_of_candidates, 0.18);
    harvest_fix_step_4(&step3, 9)
}

/// Highest-scoring candidate per frame (harvest.cpp `SearchF0Base`).
fn harvest_search_f0_base(
    f0_candidates: &[Vec<f64>],
    f0_scores: &[Vec<f64>],
    number_of_candidates: usize,
) -> Vec<f64> {
    f0_candidates
        .iter()
        .zip(f0_scores.iter())
        .map(|(candidates, scores)| {
            let mut best_f0 = 0.0f64;
            let mut best_score = 0.0f64;
            for j in 0..number_of_candidates {
                if scores[j] > best_score {
                    best_f0 = candidates[j];
                    best_score = scores[j];
                }
            }
            best_f0
        })
        .collect()
}

/// Step 1: rapid F0 jumps are replaced by 0 (harvest.cpp `FixStep1`).
fn harvest_fix_step_1(f0_base: &[f64], allowed_range: f64) -> Vec<f64> {
    let mut out = vec![0.0f64; f0_base.len()];
    for i in 2..f0_base.len() {
        if f0_base[i] == 0.0 {
            continue;
        }
        let reference_f0 = f0_base[i - 1] * 2.0 - f0_base[i - 2];
        out[i] = if ((f0_base[i] - reference_f0) / reference_f0).abs() > allowed_range
            && ((f0_base[i] - f0_base[i - 1]).abs()) / f0_base[i - 1] > allowed_range
        {
            0.0
        } else {
            f0_base[i]
        };
    }
    out
}

/// Step 2: voiced sections shorter than `voice_range_minimum` frames are
/// removed (harvest.cpp `FixStep2`).
fn harvest_fix_step_2(f0_step1: &[f64], voice_range_minimum: usize) -> Vec<f64> {
    let mut out = f0_step1.to_vec();
    let boundary_list = harvest_boundary_list(f0_step1);
    for pair in boundary_list.chunks_exact(2) {
        if pair[1] - pair[0] >= voice_range_minimum {
            continue;
        }
        for v in out[pair[0]..=pair[1]].iter_mut() {
            *v = 0.0;
        }
    }
    out
}

/// Step 3: voiced sections are extended along the candidate trellis and
/// overlaps merged by score (harvest.cpp `FixStep3`).
fn harvest_fix_step_3(
    f0_step2: &[f64],
    f0_candidates: &[Vec<f64>],
    f0_scores: &[Vec<f64>],
    number_of_candidates: usize,
    allowed_range: f64,
) -> Vec<f64> {
    let f0_length = f0_step2.len();
    let mut out = f0_step2.to_vec();
    let mut boundary_list = harvest_boundary_list(f0_step2);
    if boundary_list.len() < 2 || f0_length < 2 {
        return out;
    }
    let mut multi_channel_f0 = harvest_multi_channel_f0(f0_step2, &boundary_list);
    let number_of_channels = harvest_extend(
        &mut multi_channel_f0,
        &mut boundary_list,
        f0_length,
        f0_candidates,
        number_of_candidates,
        allowed_range,
    );
    if number_of_channels != 0 {
        harvest_merge_f0(
            &multi_channel_f0,
            &mut boundary_list,
            number_of_channels,
            f0_candidates,
            f0_scores,
            number_of_candidates,
            &mut out,
        );
    }
    out
}

/// Extends one section's contour frame by frame, following the nearest
/// candidate; gives up after four consecutive misses
/// (harvest.cpp `ExtendF0`). Returns the last extended frame.
fn harvest_extend_f0(
    extended_f0: &mut [f64],
    origin: usize,
    last_point: usize,
    shift: i64,
    f0_candidates: &[Vec<f64>],
    number_of_candidates: usize,
    allowed_range: f64,
) -> usize {
    let threshold = 4usize;
    let mut tmp_f0 = extended_f0[origin];
    let mut shifted_origin = origin;
    let distance = (last_point as i64 - origin as i64).unsigned_abs() as usize;
    let mut count = 0usize;
    for i in 0..=distance {
        let next = origin as i64 + shift * i as i64 + shift;
        if next < 0 || next >= extended_f0.len() as i64 {
            break;
        }
        let next = next as usize;
        let (best, _) = harvest_select_best_f0(
            tmp_f0,
            &f0_candidates[next][..number_of_candidates],
            allowed_range,
        );
        extended_f0[next] = best;
        if best == 0.0 {
            count += 1;
        } else {
            tmp_f0 = best;
            count = 0;
            shifted_origin = next;
        }
        if count == threshold {
            break;
        }
    }
    shifted_origin
}

/// Extends every section in both directions, then keeps only sections long
/// enough relative to their mean F0 (harvest.cpp `Extend` + `ExtendSub`).
/// Kept sections are swapped to the front; returns their count.
fn harvest_extend(
    multi_channel_f0: &mut [Vec<f64>],
    boundary_list: &mut [usize],
    f0_length: usize,
    f0_candidates: &[Vec<f64>],
    number_of_candidates: usize,
    allowed_range: f64,
) -> usize {
    let threshold = 100usize;
    let number_of_sections = multi_channel_f0.len();
    for i in 0..number_of_sections {
        let origin_ed = boundary_list[i * 2 + 1];
        boundary_list[i * 2 + 1] = harvest_extend_f0(
            &mut multi_channel_f0[i],
            origin_ed,
            (origin_ed + threshold).min(f0_length.saturating_sub(2)),
            1,
            f0_candidates,
            number_of_candidates,
            allowed_range,
        );
        let origin_st = boundary_list[i * 2];
        boundary_list[i * 2] = harvest_extend_f0(
            &mut multi_channel_f0[i],
            origin_st,
            origin_st.saturating_sub(threshold).max(1),
            -1,
            f0_candidates,
            number_of_candidates,
            allowed_range,
        );
    }

    // ExtendSub: `mean_f0` deliberately carries its previous value across
    // sections — a quirk of the reference implementation kept for
    // compatibility.
    let section_threshold = 2200.0f64;
    let mut count = 0usize;
    let mut mean_f0 = 0.0f64;
    for i in 0..number_of_sections {
        let st = boundary_list[i * 2];
        let ed = boundary_list[i * 2 + 1];
        for j in st..ed {
            mean_f0 += multi_channel_f0[i][j];
        }
        mean_f0 /= (ed - st).max(1) as f64;
        if section_threshold / mean_f0 < (ed - st) as f64 {
            multi_channel_f0.swap(count, i);
            boundary_list.swap(count * 2, i * 2);
            boundary_list.swap(count * 2 + 1, i * 2 + 1);
            count += 1;
        }
    }
    count
}

/// Highest score attached to `f0` among a frame's candidates
/// (harvest.cpp `SearchScore`).
fn harvest_search_score(
    f0: f64,
    f0_candidates: &[f64],
    f0_scores: &[f64],
    number_of_candidates: usize,
) -> f64 {
    let mut score = 0.0f64;
    for i in 0..number_of_candidates {
        if f0 == f0_candidates[i] && score < f0_scores[i] {
            score = f0_scores[i];
        }
    }
    score
}

/// Merges two overlapping sections, keeping the higher-scoring contour in the
/// overlap (harvest.cpp `MergeF0Sub`). Returns the merged end frame.
#[allow(clippy::too_many_arguments)]
fn harvest_merge_f0_sub(
    merged_f0: &mut [f64],
    st1: usize,
    ed1: usize,
    f0_2: &[f64],
    st2: usize,
    ed2: usize,
    f0_candidates: &[Vec<f64>],
    f0_scores: &[Vec<f64>],
    number_of_candidates: usize,
) -> usize {
    if st1 <= st2 && ed1 >= ed2 {
        return ed1;
    }
    let mut score1 = 0.0f64;
    let mut score2 = 0.0f64;
    for i in st2..=ed1 {
        score1 += harvest_search_score(
            merged_f0[i],
            &f0_candidates[i],
            &f0_scores[i],
            number_of_candidates,
        );
        score2 += harvest_search_score(
            f0_2[i],
            &f0_candidates[i],
            &f0_scores[i],
            number_of_candidates,
        );
    }
    // Loops (not slices) so an empty range degenerates gracefully like the
    // reference's `for` when the sections are sorted unexpectedly.
    let start = if score1 > score2 { ed1 } else { st2 };
    for i in start..=ed2 {
        merged_f0[i] = f0_2[i];
    }
    ed2
}

/// Merges all extended sections in start order (harvest.cpp `MergeF0`).
#[allow(clippy::too_many_arguments)]
fn harvest_merge_f0(
    multi_channel_f0: &[Vec<f64>],
    boundary_list: &mut [usize],
    number_of_channels: usize,
    f0_candidates: &[Vec<f64>],
    f0_scores: &[Vec<f64>],
    number_of_candidates: usize,
    merged_f0: &mut [f64],
) {
    // Insertion sort of section order by start frame (MakeSortedOrder).
    let mut order: Vec<usize> = (0..number_of_channels).collect();
    for i in 1..number_of_channels {
        for j in (0..i).rev() {
            if boundary_list[order[j] * 2] > boundary_list[order[i] * 2] {
                order.swap(i, j);
            } else {
                break;
            }
        }
    }

    merged_f0.copy_from_slice(&multi_channel_f0[0]);
    for i in 1..number_of_channels {
        let st = boundary_list[order[i] * 2];
        let ed = boundary_list[order[i] * 2 + 1];
        if st as i64 - boundary_list[1] as i64 > 0 {
            // No overlap with the merged contour so far.
            merged_f0[st..=ed].copy_from_slice(&multi_channel_f0[order[i]][st..=ed]);
            boundary_list[0] = st;
            boundary_list[1] = ed;
        } else {
            boundary_list[1] = harvest_merge_f0_sub(
                merged_f0,
                boundary_list[0],
                boundary_list[1],
                &multi_channel_f0[order[i]],
                st,
                ed,
                f0_candidates,
                f0_scores,
                number_of_candidates,
            );
        }
    }
}

/// Step 4: short unvoiced gaps are bridged linearly (harvest.cpp `FixStep4`).
fn harvest_fix_step_4(f0_step3: &[f64], threshold: usize) -> Vec<f64> {
    let mut out = f0_step3.to_vec();
    let boundary_list = harvest_boundary_list(f0_step3);
    let sections = boundary_list.len() / 2;
    if sections < 2 {
        return out;
    }
    for i in 0..sections - 1 {
        let gap_start = boundary_list[i * 2 + 1];
        let gap_end = boundary_list[(i + 1) * 2];
        let distance = gap_end - gap_start - 1;
        if distance >= threshold {
            continue;
        }
        let tmp0 = f0_step3[gap_start] + 1.0;
        let tmp1 = f0_step3[gap_end] - 1.0;
        let coefficient = (tmp1 - tmp0) / (distance as f64 + 1.0);
        let mut count = 1.0f64;
        for v in out[gap_start + 1..gap_end].iter_mut() {
            *v = tmp0 + coefficient * count;
            count += 1.0;
        }
    }
    out
}

/// Zero-lag (forward-backward) 2nd-order Butterworth over one section
/// (harvest.cpp `FilteringF0`). `x` is edge-extended in place.
fn harvest_filtering_f0(x: &mut [f64], st: usize, ed: usize) -> Vec<f64> {
    const B: [f64; 2] = [0.0078202080334971724, 0.015640416066994345];
    const A: [f64; 2] = [1.7347257688092754, -0.76600660094326412];
    let n = x.len();
    let head = x[st];
    for v in x[..st].iter_mut() {
        *v = head;
    }
    let tail = x[ed];
    for v in x[ed + 1..].iter_mut() {
        *v = tail;
    }

    let mut w = [0.0f64; 2];
    let mut tmp = vec![0.0f64; n];
    for i in 0..n {
        let wt = x[i] + A[0] * w[0] + A[1] * w[1];
        tmp[n - i - 1] = B[0] * wt + B[1] * w[0] + B[0] * w[1];
        w[1] = w[0];
        w[0] = wt;
    }
    w = [0.0; 2];
    let mut y = vec![0.0f64; n];
    for i in 0..n {
        let wt = tmp[i] + A[0] * w[0] + A[1] * w[1];
        y[n - i - 1] = B[0] * wt + B[1] * w[0] + B[0] * w[1];
        w[1] = w[0];
        w[0] = wt;
    }
    y
}

/// Smooths each voiced section with the zero-lag Butterworth filter
/// (harvest.cpp `SmoothF0Contour`).
fn harvest_smooth_f0_contour(f0: &[f64]) -> Vec<f64> {
    let lag = 300usize;
    let new_length = f0.len() + lag * 2;
    let mut padded = vec![0.0f64; new_length];
    padded[lag..lag + f0.len()].copy_from_slice(f0);

    let boundary_list = harvest_boundary_list(&padded);
    let mut smoothed = vec![0.0f64; f0.len()];
    let mut multi_channel_f0 = harvest_multi_channel_f0(&padded, &boundary_list);
    for (i, channel) in multi_channel_f0.iter_mut().enumerate() {
        let st = boundary_list[i * 2];
        let ed = boundary_list[i * 2 + 1];
        let filtered = harvest_filtering_f0(channel, st, ed);
        for j in st..=ed {
            if j >= lag && j - lag < smoothed.len() {
                smoothed[j - lag] = filtered[j];
            }
        }
    }
    smoothed
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
    progress: &dyn Fn(f32),
) -> Vec<f32> {
    let bins = fft_size / 2 + 1;
    // Lowest F0 the FFT size supports (cheaptrick.cpp GetF0FloorForCheapTrick).
    let f0_floor = 3.0 * fs / (fft_size as f64 - 3.0);
    let mut rng = WorldRandn::new();
    let mut envelope_db = Vec::with_capacity(temporal_positions.len() * bins);
    let mut envelope_log = vec![0.0f64; bins];
    let total = temporal_positions.len().max(1);
    for (i, (&position, &frame_f0)) in temporal_positions.iter().zip(f0.iter()).enumerate() {
        if i % 128 == 0 {
            progress(i as f32 / total as f32);
        }
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
// D4C (d4c.cpp)
// ---------------------------------------------------------------------------

/// Window shapes used by D4C's windowed-waveform extraction.
#[derive(Clone, Copy)]
enum D4cWindowType {
    Hanning,
    Blackman,
}

/// D4C's own FFT size (d4c.cpp `D4C`, sized for `kFloorF0D4C`).
fn d4c_fft_size(fs: f64) -> usize {
    1usize << (1 + (4.0 * fs / D4C_F0_FLOOR_HZ + 1.0).log2() as usize)
}

/// Band aperiodicity for every frame, `frames * bins` linear values in `0..1`
/// (d4c.cpp `D4C`). `fft_size_for_spectrogram` is CheapTrick's FFT size.
fn d4c(
    x: &[f64],
    fs: f64,
    temporal_positions: &[f64],
    f0: &[f64],
    fft_size_for_spectrogram: usize,
    planner: &mut RealFftPlanner<f64>,
    progress: &dyn Fn(f32),
) -> Vec<f32> {
    let bins = fft_size_for_spectrogram / 2 + 1;
    let mut rng = WorldRandn::new();
    let fft_size_d4c = d4c_fft_size(fs);

    let number_of_aperiodicities = (D4C_UPPER_LIMIT_HZ
        .min(fs / 2.0 - D4C_FREQUENCY_INTERVAL_HZ)
        / D4C_FREQUENCY_INTERVAL_HZ) as usize;
    // The Nuttall window is common to every frame, so it is designed once.
    let window_length =
        (D4C_FREQUENCY_INTERVAL_HZ * fft_size_d4c as f64 / fs) as usize * 2 + 1;
    let mut window = vec![0.0f64; window_length];
    nuttall_window(window_length, &mut window);

    // D4C LoveTrain (the aperiodicity of totally unvoiced-looking frames is
    // decided by a different algorithm).
    let aperiodicity0 = d4c_love_train(x, fs, temporal_positions, f0, planner, &mut rng);

    let mut coarse_aperiodicity = vec![0.0f64; number_of_aperiodicities + 2];
    coarse_aperiodicity[0] = -60.0;
    coarse_aperiodicity[number_of_aperiodicities + 1] = -SAFE_GUARD_MINIMUM;
    let mut coarse_frequency_axis: Vec<f64> = (0..=number_of_aperiodicities)
        .map(|i| i as f64 * D4C_FREQUENCY_INTERVAL_HZ)
        .collect();
    coarse_frequency_axis.push(fs / 2.0);

    let frequency_axis: Vec<f64> = (0..bins)
        .map(|i| i as f64 * fs / fft_size_for_spectrogram as f64)
        .collect();

    let mut aperiodicity = Vec::with_capacity(temporal_positions.len() * bins);
    let mut row = vec![0.0f64; bins];
    let total = temporal_positions.len().max(1);
    for (i, (&position, (&frame_f0, &love_train))) in temporal_positions
        .iter()
        .zip(f0.iter().zip(aperiodicity0.iter()))
        .enumerate()
    {
        if i % 128 == 0 {
            progress(i as f32 / total as f32);
        }
        if frame_f0 == 0.0 || love_train <= D4C_THRESHOLD {
            // d4c.cpp InitializeAperiodicity: everything is aperiodic.
            aperiodicity
                .extend(std::iter::repeat((1.0 - SAFE_GUARD_MINIMUM) as f32).take(bins));
            continue;
        }
        d4c_general_body(
            x,
            fs,
            frame_f0.max(D4C_F0_FLOOR_HZ),
            fft_size_d4c,
            position,
            &window,
            planner,
            &mut rng,
            &mut coarse_aperiodicity[1..=number_of_aperiodicities],
        );
        // Linear interpolation (in dB) of the coarse aperiodicity into its
        // spectral representation (d4c.cpp GetAperiodicity).
        interp1(
            &coarse_frequency_axis,
            &coarse_aperiodicity,
            &frequency_axis,
            &mut row,
        );
        aperiodicity.extend(row.iter().map(|&v| 10f64.powf(v / 20.0) as f32));
    }
    aperiodicity
}

/// Voiced/unvoiced measure per frame: ratio of the cumulative power below
/// 4 kHz to the cumulative power below 7.9 kHz (d4c.cpp `D4CLoveTrain`).
fn d4c_love_train(
    x: &[f64],
    fs: f64,
    temporal_positions: &[f64],
    f0: &[f64],
    planner: &mut RealFftPlanner<f64>,
    rng: &mut WorldRandn,
) -> Vec<f64> {
    let fft_size =
        1usize << (1 + (3.0 * fs / D4C_LOVE_TRAIN_LOWEST_F0_HZ + 1.0).log2() as usize);
    // Cumulative powers at 100, 4000 and 7900 Hz are used for VUV
    // identification (clamped to Nyquist for very low sample rates).
    let boundary0 = ((100.0 * fft_size as f64 / fs).ceil() as usize).min(fft_size / 2);
    let boundary1 = ((4000.0 * fft_size as f64 / fs).ceil() as usize).min(fft_size / 2);
    let boundary2 = ((7900.0 * fft_size as f64 / fs).ceil() as usize).min(fft_size / 2);

    f0.iter()
        .zip(temporal_positions.iter())
        .map(|(&frame_f0, &position)| {
            if frame_f0 == 0.0 {
                return 0.0;
            }
            d4c_love_train_sub(
                x,
                fs,
                frame_f0.max(D4C_LOVE_TRAIN_LOWEST_F0_HZ),
                position,
                fft_size,
                boundary0,
                boundary1,
                boundary2,
                planner,
                rng,
            )
        })
        .collect()
}

/// d4c.cpp `D4CLoveTrainSub`.
#[allow(clippy::too_many_arguments)]
fn d4c_love_train_sub(
    x: &[f64],
    fs: f64,
    current_f0: f64,
    current_position: f64,
    fft_size: usize,
    boundary0: usize,
    boundary1: usize,
    boundary2: usize,
    planner: &mut RealFftPlanner<f64>,
    rng: &mut WorldRandn,
) -> f64 {
    let waveform = get_windowed_waveform_d4c(
        x,
        fs,
        current_f0,
        current_position,
        D4cWindowType::Blackman,
        3.0,
        rng,
    );
    let spectrum = real_fft(planner, &waveform, fft_size);
    let mut power_spectrum = vec![0.0f64; fft_size / 2 + 1];
    for i in (boundary0 + 1)..=fft_size / 2 {
        power_spectrum[i] = spectrum[i].norm_sqr();
    }
    for i in boundary0..=boundary2 {
        power_spectrum[i] += power_spectrum[i - 1];
    }
    // Deviation from the reference: guard the denominator so digital silence
    // cannot produce NaN (which would silently count as "unvoiced" anyway).
    power_spectrum[boundary1] / power_spectrum[boundary2].max(f64::MIN_POSITIVE)
}

/// F0-adaptive Hanning/Blackman windowed waveform with DC removal
/// (d4c.cpp `GetWindowedWaveform` + `SetParametersForGetWindowedWaveform`).
///
/// Unlike CheapTrick's variant this one does not RMS-normalize the window.
fn get_windowed_waveform_d4c(
    x: &[f64],
    fs: f64,
    current_f0: f64,
    current_position: f64,
    window_type: D4cWindowType,
    window_length_ratio: f64,
    rng: &mut WorldRandn,
) -> Vec<f64> {
    let half_window_length =
        matlab_round(window_length_ratio * fs / current_f0 / 2.0).max(1) as usize;
    let length = half_window_length * 2 + 1;
    let origin = matlab_round(current_position * fs + 0.001);

    let mut window = vec![0.0f64; length];
    for (i, w) in window.iter_mut().enumerate() {
        let base_index = i as f64 - half_window_length as f64;
        let position = 2.0 * base_index / window_length_ratio / fs;
        let phase = std::f64::consts::PI * position * current_f0;
        *w = match window_type {
            D4cWindowType::Hanning => 0.5 * phase.cos() + 0.5,
            D4cWindowType::Blackman => 0.42 + 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos(),
        };
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

/// Energy-weighted temporal centroid spectrum of one windowed segment
/// (d4c.cpp `GetCentroid`).
fn get_centroid(
    x: &[f64],
    fs: f64,
    current_f0: f64,
    fft_size: usize,
    current_position: f64,
    planner: &mut RealFftPlanner<f64>,
    rng: &mut WorldRandn,
) -> Vec<f64> {
    let mut waveform = get_windowed_waveform_d4c(
        x,
        fs,
        current_f0,
        current_position,
        D4cWindowType::Blackman,
        4.0,
        rng,
    );
    let power: f64 = waveform.iter().map(|&v| v * v).sum();
    let norm = power.sqrt().max(f64::MIN_POSITIVE);
    for v in waveform.iter_mut() {
        *v /= norm;
    }
    let main_spectrum = real_fft(planner, &waveform, fft_size);
    let weighted: Vec<f64> = waveform
        .iter()
        .enumerate()
        .map(|(i, &v)| v * (i as f64 + 1.0))
        .collect();
    let weighted_spectrum = real_fft(planner, &weighted, fft_size);
    main_spectrum
        .iter()
        .zip(weighted_spectrum.iter())
        .map(|(m, w)| m.re * w.re + m.im * w.im)
        .collect()
}

/// Sum of two centroids a quarter period apart, DC-corrected
/// (d4c.cpp `GetStaticCentroid`).
fn get_static_centroid(
    x: &[f64],
    fs: f64,
    current_f0: f64,
    fft_size: usize,
    current_position: f64,
    planner: &mut RealFftPlanner<f64>,
    rng: &mut WorldRandn,
) -> Vec<f64> {
    let centroid1 = get_centroid(
        x,
        fs,
        current_f0,
        fft_size,
        current_position - 0.25 / current_f0,
        planner,
        rng,
    );
    let centroid2 = get_centroid(
        x,
        fs,
        current_f0,
        fft_size,
        current_position + 0.25 / current_f0,
        planner,
        rng,
    );
    let mut static_centroid: Vec<f64> = centroid1
        .iter()
        .zip(centroid2.iter())
        .map(|(a, b)| a + b)
        .collect();
    dc_correction(&mut static_centroid, current_f0, fs, fft_size);
    static_centroid
}

/// Smoothed power spectrum of one Hanning-windowed segment
/// (d4c.cpp `GetSmoothedPowerSpectrum`).
fn get_smoothed_power_spectrum(
    x: &[f64],
    fs: f64,
    current_f0: f64,
    fft_size: usize,
    current_position: f64,
    planner: &mut RealFftPlanner<f64>,
    rng: &mut WorldRandn,
) -> Vec<f64> {
    let waveform = get_windowed_waveform_d4c(
        x,
        fs,
        current_f0,
        current_position,
        D4cWindowType::Hanning,
        4.0,
        rng,
    );
    let spectrum = real_fft(planner, &waveform, fft_size);
    let mut power_spectrum: Vec<f64> =
        spectrum[..fft_size / 2 + 1].iter().map(|c| c.norm_sqr()).collect();
    dc_correction(&mut power_spectrum, current_f0, fs, fft_size);
    linear_smoothing(&mut power_spectrum, current_f0, fs, fft_size);
    power_spectrum
}

/// Static group delay = centroid / power, band-pass smoothed
/// (d4c.cpp `GetStaticGroupDelay`).
fn get_static_group_delay(
    static_centroid: &[f64],
    smoothed_power_spectrum: &[f64],
    fs: f64,
    f0: f64,
    fft_size: usize,
) -> Vec<f64> {
    // Deviation from the reference: guard the division; the smoothed power
    // spectrum is strictly positive for any real input but a clamp keeps the
    // group delay finite even for pathological signals.
    let mut static_group_delay: Vec<f64> = static_centroid
        .iter()
        .zip(smoothed_power_spectrum.iter())
        .map(|(c, p)| c / p.max(f64::MIN_POSITIVE))
        .collect();
    linear_smoothing(&mut static_group_delay, f0 / 2.0, fs, fft_size);
    let mut smoothed_group_delay = static_group_delay.clone();
    linear_smoothing(&mut smoothed_group_delay, f0, fs, fft_size);
    for (v, s) in static_group_delay
        .iter_mut()
        .zip(smoothed_group_delay.iter())
    {
        *v -= s;
    }
    static_group_delay
}

/// Aperiodicity in dB for each 3 kHz band from the flatness of the group
/// delay spectrum (d4c.cpp `GetCoarseAperiodicity`).
fn get_coarse_aperiodicity(
    static_group_delay: &[f64],
    fs: f64,
    fft_size: usize,
    window: &[f64],
    planner: &mut RealFftPlanner<f64>,
    coarse_aperiodicity: &mut [f64],
) {
    let boundary = matlab_round(fft_size as f64 * 8.0 / window.len() as f64) as usize;
    let half_window_length = window.len() / 2;
    let mut segment = vec![0.0f64; window.len()];
    for (band, out) in coarse_aperiodicity.iter_mut().enumerate() {
        let center = (D4C_FREQUENCY_INTERVAL_HZ * (band + 1) as f64 * fft_size as f64 / fs)
            as usize;
        for (j, seg) in segment.iter_mut().enumerate() {
            *seg = static_group_delay[center - half_window_length + j] * window[j];
        }
        let spectrum = real_fft(planner, &segment, fft_size);
        let mut power_spectrum: Vec<f64> = spectrum.iter().map(|c| c.norm_sqr()).collect();
        power_spectrum.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for j in 1..power_spectrum.len() {
            power_spectrum[j] += power_spectrum[j - 1];
        }
        *out = 10.0
            * (power_spectrum[fft_size / 2 - boundary - 1]
                / power_spectrum[fft_size / 2].max(f64::MIN_POSITIVE))
            .max(f64::MIN_POSITIVE)
            .log10();
    }
}

/// Coarse aperiodicity of one voiced frame (d4c.cpp `D4CGeneralBody`).
#[allow(clippy::too_many_arguments)]
fn d4c_general_body(
    x: &[f64],
    fs: f64,
    current_f0: f64,
    fft_size: usize,
    current_position: f64,
    window: &[f64],
    planner: &mut RealFftPlanner<f64>,
    rng: &mut WorldRandn,
    coarse_aperiodicity: &mut [f64],
) {
    let static_centroid =
        get_static_centroid(x, fs, current_f0, fft_size, current_position, planner, rng);
    let smoothed_power_spectrum =
        get_smoothed_power_spectrum(x, fs, current_f0, fft_size, current_position, planner, rng);
    let static_group_delay = get_static_group_delay(
        &static_centroid,
        &smoothed_power_spectrum,
        fs,
        current_f0,
        fft_size,
    );
    get_coarse_aperiodicity(
        &static_group_delay,
        fs,
        fft_size,
        window,
        planner,
        coarse_aperiodicity,
    );

    // Revision of the estimate based on the F0.
    for v in coarse_aperiodicity.iter_mut() {
        *v = (*v + (current_f0 - 100.0) / 50.0).min(0.0);
    }
}

// ---------------------------------------------------------------------------
// Synthesis (synthesis.cpp)
// ---------------------------------------------------------------------------

/// Pulse positions and per-sample voicing for synthesis
/// (synthesis.cpp `GetTimeBase`).
struct SynthesisTimeBase {
    /// Pulse positions in seconds (quantized to the sample grid).
    pulse_locations: Vec<f64>,
    /// Pulse positions in samples.
    pulse_locations_index: Vec<usize>,
    /// Sub-sample offset of the exact pulse position, in seconds (`0..1/fs`).
    pulse_locations_time_shift: Vec<f64>,
    /// Per-sample voiced flag (`0.0` or `1.0`).
    interpolated_vuv: Vec<f64>,
}

/// Interpolates the frame-rate F0 contour to sample rate and accumulates
/// phase to place one pulse per period (synthesis.cpp `GetTimeBase`,
/// `GetTemporalParametersForTimeBase`, `GetPulseLocationsForTimeBase`).
fn get_time_base(
    f0: &[f64],
    fs: f64,
    frame_period: f64,
    y_length: usize,
    lowest_f0: f64,
) -> SynthesisTimeBase {
    let f0_length = f0.len();
    let time_axis: Vec<f64> = (0..y_length).map(|i| i as f64 / fs).collect();

    let mut coarse_time_axis = Vec::with_capacity(f0_length + 1);
    let mut coarse_f0 = Vec::with_capacity(f0_length + 1);
    let mut coarse_vuv = Vec::with_capacity(f0_length + 1);
    for (i, &v) in f0.iter().enumerate() {
        coarse_time_axis.push(i as f64 * frame_period);
        let clamped = if v < lowest_f0 { 0.0 } else { v };
        coarse_f0.push(clamped);
        coarse_vuv.push(if clamped == 0.0 { 0.0 } else { 1.0 });
    }
    // One extrapolated frame past the end so interpolation covers y_length.
    coarse_time_axis.push(f0_length as f64 * frame_period);
    if f0_length >= 2 {
        coarse_f0.push(coarse_f0[f0_length - 1] * 2.0 - coarse_f0[f0_length - 2]);
        coarse_vuv.push(coarse_vuv[f0_length - 1] * 2.0 - coarse_vuv[f0_length - 2]);
    } else {
        coarse_f0.push(coarse_f0[0]);
        coarse_vuv.push(coarse_vuv[0]);
    }

    let mut interpolated_f0 = vec![0.0f64; y_length];
    let mut interpolated_vuv = vec![0.0f64; y_length];
    interp1(&coarse_time_axis, &coarse_f0, &time_axis, &mut interpolated_f0);
    interp1(&coarse_time_axis, &coarse_vuv, &time_axis, &mut interpolated_vuv);
    for i in 0..y_length {
        interpolated_vuv[i] = if interpolated_vuv[i] > 0.5 { 1.0 } else { 0.0 };
        if interpolated_vuv[i] == 0.0 {
            // Unvoiced regions still get pulses at kDefaultF0 spacing; their
            // excitation is pure noise.
            interpolated_f0[i] = DEFAULT_F0_HZ;
        }
    }

    // Pulse locations from the wrap points of the accumulated phase.
    let mut pulse_locations = Vec::new();
    let mut pulse_locations_index = Vec::new();
    let mut pulse_locations_time_shift = Vec::new();
    let two_pi = 2.0 * std::f64::consts::PI;
    let mut total_phase = two_pi * interpolated_f0[0] / fs;
    let mut wrap_phase_prev = total_phase % two_pi;
    for i in 1..y_length {
        total_phase += two_pi * interpolated_f0[i] / fs;
        let wrap_phase = total_phase % two_pi;
        if (wrap_phase - wrap_phase_prev).abs() > std::f64::consts::PI {
            pulse_locations.push(time_axis[i - 1]);
            pulse_locations_index.push(i - 1);
            // Sub-sample time of the exact 2*pi crossing between samples
            // i - 1 and i, solved from the two wrapped phases.
            let y1 = wrap_phase_prev - two_pi;
            let y2 = wrap_phase;
            let x_frac = -y1 / (y2 - y1);
            pulse_locations_time_shift.push(x_frac / fs);
        }
        wrap_phase_prev = wrap_phase;
    }

    SynthesisTimeBase {
        pulse_locations,
        pulse_locations_index,
        pulse_locations_time_shift,
        interpolated_vuv,
    }
}

/// Hanning-shaped kernel that redistributes a removed DC component
/// (synthesis.cpp `GetDCRemover`).
fn get_dc_remover(fft_size: usize) -> Vec<f64> {
    let mut dc_remover = vec![0.0f64; fft_size];
    let mut dc_component = 0.0f64;
    for i in 0..fft_size / 2 {
        let v = 0.5
            - 0.5
                * (2.0 * std::f64::consts::PI * (i as f64 + 1.0) / (1.0 + fft_size as f64))
                    .cos();
        dc_remover[i] = v;
        dc_remover[fft_size - i - 1] = v;
        dc_component += v * 2.0;
    }
    for i in 0..fft_size / 2 {
        dc_remover[i] /= dc_component;
        dc_remover[fft_size - i - 1] = dc_remover[i];
    }
    dc_remover
}

/// Swaps the two halves of `x` (WORLD `fftshift`).
fn fftshift_vec(x: &[f64]) -> Vec<f64> {
    let half = x.len() / 2;
    let mut y = vec![0.0f64; x.len()];
    y[..half].copy_from_slice(&x[half..]);
    y[half..half * 2].copy_from_slice(&x[..half]);
    y
}

/// Removes the DC component of the (causal, second-half) response and spreads
/// the correction with the DC remover kernel (synthesis.cpp
/// `RemoveDCComponent`).
fn remove_dc_component(response: &mut [f64], dc_remover: &[f64]) {
    let half = response.len() / 2;
    let dc_component: f64 = response[half..].iter().sum();
    for i in 0..half {
        response[i] = -dc_component * dc_remover[i];
    }
    for i in half..response.len() {
        response[i] -= dc_component * dc_remover[i];
    }
}

/// Minimum-phase spectrum from half a log-magnitude spectrum via the
/// cepstrum method (common.cpp `GetMinimumPhaseSpectrum`).
///
/// The reference's unnormalized FFT pair plus its final `1 / fft_size` is
/// realized here with the normalized `real_ifft`, so no extra scaling is
/// needed by the callers.
fn get_minimum_phase_spectrum(
    log_spectrum: &[f64],
    fft_size: usize,
    planner: &mut RealFftPlanner<f64>,
) -> Vec<Complex<f64>> {
    // The log spectrum is real and even, so its inverse FFT (the cepstrum)
    // is real; realize the mirrored transform with the real FFT pair.
    let mut spectrum: Vec<Complex<f64>> = log_spectrum
        .iter()
        .map(|&v| Complex::new(v, 0.0))
        .collect();
    let mut cepstrum = real_ifft(planner, &mut spectrum, fft_size);
    // Fold the anticausal part onto the causal part.
    for v in cepstrum[1..fft_size / 2].iter_mut() {
        *v *= 2.0;
    }
    for v in cepstrum[fft_size / 2 + 1..].iter_mut() {
        *v = 0.0;
    }
    let folded_spectrum = real_fft(planner, &cepstrum, fft_size);
    folded_spectrum
        .iter()
        .map(|c| Complex::from_polar(c.re.exp(), c.im))
        .collect()
}

/// Impulse response of the periodic (pulse-excited) part with fractional
/// time shift and DC removal (synthesis.cpp `GetPeriodicResponse`).
#[allow(clippy::too_many_arguments)]
fn get_periodic_response(
    fft_size: usize,
    spectral_envelope: &[f64],
    aperiodic_ratio: &[f64],
    current_vuv: f64,
    fractional_time_shift: f64,
    fs: f64,
    dc_remover: &[f64],
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f64> {
    if current_vuv <= 0.5 || aperiodic_ratio[0] > 0.999 {
        return vec![0.0f64; fft_size];
    }

    let log_spectrum: Vec<f64> = spectral_envelope
        .iter()
        .zip(aperiodic_ratio.iter())
        .map(|(&s, &a)| (s * (1.0 - a) + SAFE_GUARD_MINIMUM).ln() / 2.0)
        .collect();
    let mut spectrum = get_minimum_phase_spectrum(&log_spectrum, fft_size, planner);

    // Fractional time delay as a linear phase shift (synthesis.cpp
    // GetSpectrumWithFractionalTimeShift); the shift is < 1/fs, so the phase
    // stays within [0, pi] and sin can be recovered from cos.
    let coefficient =
        2.0 * std::f64::consts::PI * fractional_time_shift * fs / fft_size as f64;
    for (i, c) in spectrum.iter_mut().enumerate() {
        let re2 = (coefficient * i as f64).cos();
        let im2 = (1.0 - re2 * re2).max(0.0).sqrt();
        *c = Complex::new(c.re * re2 + c.im * im2, c.im * re2 - c.re * im2);
    }

    let waveform = real_ifft(planner, &mut spectrum, fft_size);
    let mut response = fftshift_vec(&waveform);
    remove_dc_component(&mut response, dc_remover);
    response
}

/// Impulse response of the aperiodic (noise-excited) part
/// (synthesis.cpp `GetAperiodicResponse` + `GetNoiseSpectrum`).
#[allow(clippy::too_many_arguments)]
fn get_aperiodic_response(
    noise_size: usize,
    fft_size: usize,
    spectral_envelope: &[f64],
    aperiodic_ratio: &[f64],
    current_vuv: f64,
    planner: &mut RealFftPlanner<f64>,
    rng: &mut WorldRandn,
) -> Vec<f64> {
    // Zero-mean white noise excitation covering one pulse interval.
    let n = noise_size.min(fft_size);
    let mut noise = vec![0.0f64; fft_size];
    if n > 0 {
        let mut average = 0.0f64;
        for v in noise[..n].iter_mut() {
            *v = rng.randn();
            average += *v;
        }
        average /= n as f64;
        for v in noise[..n].iter_mut() {
            *v -= average;
        }
    }
    let noise_spectrum = real_fft(planner, &noise, fft_size);

    let log_spectrum: Vec<f64> = if current_vuv != 0.0 {
        spectral_envelope
            .iter()
            .zip(aperiodic_ratio.iter())
            .map(|(&s, &a)| (s * a).max(f64::MIN_POSITIVE).ln() / 2.0)
            .collect()
    } else {
        spectral_envelope
            .iter()
            .map(|&s| s.max(f64::MIN_POSITIVE).ln() / 2.0)
            .collect()
    };
    let minimum_phase = get_minimum_phase_spectrum(&log_spectrum, fft_size, planner);

    let mut product: Vec<Complex<f64>> = minimum_phase
        .iter()
        .zip(noise_spectrum.iter())
        .map(|(a, b)| a * b)
        .collect();
    let waveform = real_ifft(planner, &mut product, fft_size);
    fftshift_vec(&waveform)
}

/// Spectral envelope (linear power) interpolated at `current_time`
/// (synthesis.cpp `GetSpectralEnvelope`).
fn get_spectral_envelope(
    spectrogram: &[f64],
    bins: usize,
    f0_length: usize,
    frame_period: f64,
    current_time: f64,
    out: &mut [f64],
) {
    let position = current_time / frame_period;
    let frame_floor = (position.floor() as usize).min(f0_length - 1);
    let frame_ceil = (position.ceil() as usize).min(f0_length - 1);
    let interpolation = position - frame_floor as f64;
    let row_floor = &spectrogram[frame_floor * bins..frame_floor * bins + bins];
    if frame_floor == frame_ceil {
        for (o, &v) in out.iter_mut().zip(row_floor.iter()) {
            *o = v.abs();
        }
    } else {
        let row_ceil = &spectrogram[frame_ceil * bins..frame_ceil * bins + bins];
        for i in 0..bins {
            out[i] =
                (1.0 - interpolation) * row_floor[i].abs() + interpolation * row_ceil[i].abs();
        }
    }
}

/// synthesis.cpp `GetSafeAperiodicity`.
fn safe_aperiodicity(x: f64) -> f64 {
    x.clamp(0.001, 0.999_999_999_999)
}

/// Squared aperiodicity interpolated at `current_time`
/// (synthesis.cpp `GetAperiodicRatio`).
fn get_aperiodic_ratio(
    aperiodicity: &[f64],
    bins: usize,
    f0_length: usize,
    frame_period: f64,
    current_time: f64,
    out: &mut [f64],
) {
    let position = current_time / frame_period;
    let frame_floor = (position.floor() as usize).min(f0_length - 1);
    let frame_ceil = (position.ceil() as usize).min(f0_length - 1);
    let interpolation = position - frame_floor as f64;
    let row_floor = &aperiodicity[frame_floor * bins..frame_floor * bins + bins];
    if frame_floor == frame_ceil {
        for (o, &v) in out.iter_mut().zip(row_floor.iter()) {
            *o = safe_aperiodicity(v).powi(2);
        }
    } else {
        let row_ceil = &aperiodicity[frame_ceil * bins..frame_ceil * bins + bins];
        for i in 0..bins {
            out[i] = ((1.0 - interpolation) * safe_aperiodicity(row_floor[i])
                + interpolation * safe_aperiodicity(row_ceil[i]))
            .powi(2);
        }
    }
}

/// Synthesizes a waveform from a WORLD parameter set (synthesis.cpp
/// `Synthesis`).
///
/// `f0` is the per-frame contour in Hz (`0.0` = unvoiced, may be user
/// edited), `envelope_db` the CheapTrick envelope as produced by
/// [`analyze_world`] (power dB), `aperiodicity` the linear D4C band
/// aperiodicity, and `fft_size` the CheapTrick FFT size used at analysis
/// time. The output is exactly `out_len` samples (zero-padded/truncated).
#[allow(clippy::too_many_arguments)]
pub fn synthesize_world(
    f0: &[f32],
    envelope_db: &[f32],
    aperiodicity: &[f32],
    bins: usize,
    fft_size: usize,
    sample_rate: u32,
    frame_period_ms: f64,
    out_len: usize,
) -> Vec<f32> {
    if out_len == 0 {
        return Vec::new();
    }
    let f0_length = f0.len();
    if f0_length == 0
        || sample_rate == 0
        || !(frame_period_ms > 0.0)
        || bins == 0
        || bins != fft_size / 2 + 1
        || envelope_db.len() < f0_length * bins
        || aperiodicity.len() < f0_length * bins
    {
        return vec![0.0f32; out_len];
    }

    let fs = sample_rate as f64;
    let frame_period = frame_period_ms / 1000.0;
    let f0_f64: Vec<f64> = f0.iter().map(|&v| v as f64).collect();
    // Power dB back to linear power.
    let spectrogram: Vec<f64> = envelope_db[..f0_length * bins]
        .iter()
        .map(|&v| 10f64.powf(v as f64 / 10.0))
        .collect();
    let aperiodicity_f64: Vec<f64> = aperiodicity[..f0_length * bins]
        .iter()
        .map(|&v| v as f64)
        .collect();

    // Lowest F0 the impulse-response length supports; WORLD computes this
    // with integer division (`fs / fft_size + 1.0`).
    let lowest_f0 = (sample_rate as usize / fft_size) as f64 + 1.0;
    let time_base = get_time_base(&f0_f64, fs, frame_period, out_len, lowest_f0);

    let dc_remover = get_dc_remover(fft_size);
    // One planner for the whole call: realfft caches plans per size, so no
    // FFT is replanned per pulse.
    let mut planner = RealFftPlanner::<f64>::new();
    let mut rng = WorldRandn::new();

    let mut y = vec![0.0f64; out_len];
    let mut spectral_envelope = vec![0.0f64; bins];
    let mut aperiodic_ratio = vec![0.0f64; bins];
    let number_of_pulses = time_base.pulse_locations.len();
    for i in 0..number_of_pulses {
        let noise_size = time_base.pulse_locations_index
            [(i + 1).min(number_of_pulses - 1)]
            - time_base.pulse_locations_index[i];
        let current_vuv = time_base.interpolated_vuv[time_base.pulse_locations_index[i]];
        let current_time = time_base.pulse_locations[i];

        get_spectral_envelope(
            &spectrogram,
            bins,
            f0_length,
            frame_period,
            current_time,
            &mut spectral_envelope,
        );
        get_aperiodic_ratio(
            &aperiodicity_f64,
            bins,
            f0_length,
            frame_period,
            current_time,
            &mut aperiodic_ratio,
        );

        let periodic_response = get_periodic_response(
            fft_size,
            &spectral_envelope,
            &aperiodic_ratio,
            current_vuv,
            time_base.pulse_locations_time_shift[i],
            fs,
            &dc_remover,
            &mut planner,
        );
        let aperiodic_response = get_aperiodic_response(
            noise_size,
            fft_size,
            &spectral_envelope,
            &aperiodic_ratio,
            current_vuv,
            &mut planner,
            &mut rng,
        );

        // real_ifft is normalized, so WORLD's final 1 / fft_size division is
        // already folded into both responses.
        let sqrt_noise_size = (noise_size as f64).sqrt();
        let offset = time_base.pulse_locations_index[i] as i64 - (fft_size / 2) as i64 + 1;
        let lower = (-offset).max(0) as usize;
        let upper = (out_len as i64 - offset).clamp(0, fft_size as i64) as usize;
        for j in lower..upper {
            y[(j as i64 + offset) as usize] +=
                periodic_response[j] * sqrt_noise_size + aperiodic_response[j];
        }
    }
    y.iter().map(|&v| v as f32).collect()
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
        sine_amp(freq, 0.5, secs, fs)
    }

    fn sine_amp(freq: f64, amp: f32, secs: f64, fs: u32) -> Vec<f32> {
        let n = (secs * fs as f64) as usize;
        (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / fs as f64).sin() as f32 * amp
            })
            .collect()
    }

    /// Deterministic LCG white-ish noise in `-0.5..0.5`.
    fn lcg_noise(n: usize) -> Vec<f32> {
        let mut state = 0x1234_5678_9abc_def0u64;
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((state >> 33) as f64 / (1u64 << 31) as f64 - 1.0) as f32 * 0.5
            })
            .collect()
    }

    /// Convenience wrapper: synthesize from analyzed features.
    fn synthesize(features: &WorldFeatures, f0: &[f32], out_len: usize) -> Vec<f32> {
        synthesize_world(
            f0,
            &features.envelope_db,
            &features.aperiodicity,
            features.bins,
            features.fft_size,
            features.sample_rate,
            features.frame_period_ms,
            out_len,
        )
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

    fn analyze_harvest(mono: &[f32], fs: u32) -> WorldFeatures {
        analyze_world_with_options(mono, fs, FRAME_PERIOD_MS, WorldF0Estimator::Harvest, None)
    }

    /// Band-limited sawtooth (harmonics `1/k` up to Nyquist). Harvest's
    /// refinement scores harmonic consistency, so it needs harmonic-rich
    /// material; the reference rejects pure sines outright.
    fn saw(freq: f64, secs: f64, fs: u32) -> Vec<f32> {
        let n = (secs * fs as f64) as usize;
        let mut x = vec![0.0f32; n];
        let mut k = 1.0f64;
        while k * freq < fs as f64 / 2.0 {
            for (i, v) in x.iter_mut().enumerate() {
                *v += ((2.0 * std::f64::consts::PI * k * freq * i as f64 / fs as f64).sin() / k)
                    as f32;
            }
            k += 1.0;
        }
        let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-9);
        for v in x.iter_mut() {
            *v *= 0.5 / peak;
        }
        x
    }

    #[test]
    fn harvest_saw_440_f0() {
        let signal = saw(440.0, 1.0, TEST_FS);
        let features = analyze_harvest(&signal, TEST_FS);
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
            "harvest median voiced F0 {med} not within 5 Hz of 440"
        );
    }

    #[test]
    fn harvest_saw_220_at_44100() {
        // Non-48k rate exercises the decimation-ratio rounding path.
        let signal = saw(220.0, 1.0, 44_100);
        let features = analyze_harvest(&signal, 44_100);
        let voiced = voiced_values(&features.f0);
        assert!(
            voiced.len() as f64 / features.frames as f64 > 0.8,
            "expected mostly voiced frames"
        );
        let med = median(&voiced);
        assert!(
            (med - 220.0).abs() < 5.0,
            "harvest median voiced F0 {med} not within 5 Hz of 220"
        );
    }

    #[test]
    fn harvest_rejects_pure_sine_like_reference() {
        // pyworld 0.3.5's Harvest returns 0 voiced frames for a bare 440 Hz
        // sine (no harmonics to score against); the port must agree rather
        // than "helpfully" voice it.
        let signal = sine(440.0, 1.0, TEST_FS);
        let features = analyze_harvest(&signal, TEST_FS);
        let voiced_ratio = voiced_values(&features.f0).len() as f64 / features.frames as f64;
        assert!(
            voiced_ratio < 0.1,
            "expected near-zero voiced frames, got {:.1}%",
            voiced_ratio * 100.0
        );
    }

    #[test]
    fn harvest_silence_is_unvoiced() {
        let signal = vec![0.0f32; TEST_FS as usize / 2];
        let features = analyze_harvest(&signal, TEST_FS);
        assert!(features.frames > 0);
        assert!(
            features.f0.iter().all(|&v| v == 0.0),
            "silence produced voiced frames"
        );
    }

    #[test]
    fn harvest_noise_is_mostly_unvoiced() {
        let signal = lcg_noise(TEST_FS as usize);
        let features = analyze_harvest(&signal, TEST_FS);
        let voiced_ratio = voiced_values(&features.f0).len() as f64 / features.frames as f64;
        assert!(
            voiced_ratio < 0.35,
            "expected mostly unvoiced frames for noise, got {:.1}% voiced",
            voiced_ratio * 100.0
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
        let signal = lcg_noise(TEST_FS as usize);
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
        assert_eq!(features.aperiodicity.len(), features.frames * features.bins);
        assert!(
            features
                .aperiodicity
                .iter()
                .all(|&v| (0.0..=1.0).contains(&v)),
            "aperiodicity out of the 0..1 range"
        );
    }

    #[test]
    fn empty_input_yields_no_frames() {
        let features = analyze_world(&[], TEST_FS, FRAME_PERIOD_MS);
        assert_eq!(features.frames, 0);
        assert!(features.f0.is_empty());
        assert!(features.envelope_db.is_empty());
        assert!(features.aperiodicity.is_empty());
    }

    #[test]
    fn aperiodicity_low_for_sine_high_for_noise() {
        // A pure tone is nearly periodic: the lowest band of the voiced
        // frames must be far away from "fully aperiodic".
        let signal = sine_amp(220.0, 0.7, 1.0, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let bins = features.bins;
        let hz_per_bin = TEST_FS as f64 / features.fft_size as f64;
        let band_hi = (3000.0 / hz_per_bin) as usize;
        let mut sum = 0.0f64;
        let mut count = 0usize;
        for (frame, &f0) in features.f0.iter().enumerate() {
            if f0 <= 0.0 {
                continue;
            }
            for &v in &features.aperiodicity[frame * bins..frame * bins + band_hi + 1] {
                sum += v as f64;
                count += 1;
            }
        }
        assert!(count > 0, "sine produced no voiced frames");
        let mean = sum / count as f64;
        assert!(
            mean < 0.6,
            "mean lowest-band aperiodicity {mean:.3} of a sine not < 0.6"
        );

        // Noise: voiced frames are rare, and where DIO hallucinates voicing
        // the D4C LoveTrain must still report ~fully aperiodic frames.
        let noise = lcg_noise(TEST_FS as usize);
        let noise_features = analyze_world(&noise, TEST_FS, FRAME_PERIOD_MS);
        let voiced_frames: Vec<usize> = noise_features
            .f0
            .iter()
            .enumerate()
            .filter(|(_, &v)| v > 0.0)
            .map(|(i, _)| i)
            .collect();
        let voiced_ratio = voiced_frames.len() as f64 / noise_features.frames as f64;
        assert!(
            voiced_ratio < 0.35,
            "noise voiced ratio {:.1}% not rare",
            voiced_ratio * 100.0
        );
        if !voiced_frames.is_empty() {
            let mut sum = 0.0f64;
            let mut count = 0usize;
            for &frame in &voiced_frames {
                for &v in &noise_features.aperiodicity[frame * bins..(frame + 1) * bins] {
                    sum += v as f64;
                    count += 1;
                }
            }
            let mean = sum / count as f64;
            assert!(
                mean > 0.7,
                "mean aperiodicity {mean:.3} of voiced noise frames not near 1"
            );
        }
    }

    #[test]
    fn synthesis_roundtrip_preserves_pitch() {
        let signal = sine_amp(220.0, 0.7, 1.0, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let start = std::time::Instant::now();
        let out = synthesize(&features, &features.f0, signal.len());
        println!(
            "synthesize_world: 1.0 s @ 48 kHz in {:.1?}",
            start.elapsed()
        );
        assert_eq!(out.len(), signal.len());
        let reanalyzed = analyze_world(&out, TEST_FS, FRAME_PERIOD_MS);
        let voiced = voiced_values(&reanalyzed.f0);
        assert!(
            voiced.len() as f64 / reanalyzed.frames as f64 > 0.5,
            "resynthesized sine is mostly unvoiced"
        );
        let med = median(&voiced);
        assert!(
            (med - 220.0).abs() < 8.0,
            "roundtrip median F0 {med} not within 8 Hz of 220"
        );
    }

    #[test]
    fn synthesis_pitch_edit_shifts_f0() {
        let signal = sine_amp(220.0, 0.7, 1.0, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let edited: Vec<f32> = features.f0.iter().map(|&v| v * 1.5).collect();
        let out = synthesize(&features, &edited, signal.len());
        let reanalyzed = analyze_world(&out, TEST_FS, FRAME_PERIOD_MS);
        let voiced = voiced_values(&reanalyzed.f0);
        assert!(!voiced.is_empty(), "pitch-shifted output is unvoiced");
        let med = median(&voiced);
        assert!(
            (med - 330.0).abs() < 12.0,
            "pitch-edited median F0 {med} not within 12 Hz of 330"
        );
    }

    #[test]
    fn synthesis_output_hygiene() {
        let signal = sine_amp(220.0, 0.7, 0.5, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        for &out_len in &[signal.len(), signal.len() - 1234, signal.len() + 4321] {
            let out = synthesize(&features, &features.f0, out_len);
            assert_eq!(out.len(), out_len, "out_len not respected");
            assert!(
                out.iter().all(|v| v.is_finite()),
                "output contains non-finite samples"
            );
            let max_abs = out.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
            assert!(max_abs < 4.0, "output peak {max_abs} not < 4.0");
        }
    }

    #[test]
    fn synthesis_of_noise_is_nonsilent() {
        let signal = lcg_noise(TEST_FS as usize / 2);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let out = synthesize(&features, &features.f0, signal.len());
        assert_eq!(out.len(), signal.len());
        assert!(
            out.iter().all(|v| v.is_finite()),
            "noise resynthesis contains non-finite samples"
        );
        let energy: f64 = out.iter().map(|&v| (v as f64) * (v as f64)).sum();
        assert!(
            energy > 1e-6,
            "noise resynthesis is silent (energy {energy:.3e})"
        );
    }

    #[test]
    fn synthesis_is_deterministic() {
        let signal = sine_amp(220.0, 0.7, 0.5, TEST_FS);
        let features = analyze_world(&signal, TEST_FS, FRAME_PERIOD_MS);
        let a = synthesize(&features, &features.f0, signal.len());
        let b = synthesize(&features, &features.f0, signal.len());
        assert_eq!(a, b, "two identical synthesize_world calls differ");
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
