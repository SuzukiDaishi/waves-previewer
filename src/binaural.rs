//! Headphone (binaural) monitoring of multichannel material.
//!
//! Every source channel is convolved with the head-related impulse responses
//! (HRIRs) of the direction its virtual speaker stands in, and the results are
//! summed per ear. DSP only: no UI, no disk. The HRIRs come from a SOFA file
//! that `app::hrtf_ops` opens on a worker (through `sofar`); this module turns
//! them into partitioned spectra ([`BinauralFilters`], built off the audio
//! thread and never changed after) and runs the convolution
//! ([`BinauralState`], owned by the output callback).
//!
//! The convolution is uniformly partitioned overlap-save. The input is cut
//! into blocks of [`BINAURAL_PARTITION_FRAMES`]; the spectrum of each block
//! (with the block before it) joins a frequency-domain delay line per
//! channel; each ear's output block is one inverse FFT of the sum, over every
//! channel and partition, of delay line times filter. Summing in the frequency
//! domain is what keeps twelve channels at two inverse FFTs a block.
//!
//! Directions are the app's: azimuth in degrees, negative to the left and 0
//! straight ahead, elevation in degrees, positive up -- the convention of
//! `SpeakerPos::direction`. [`Direction::sofa_xyz`] converts to SOFA's.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

/// The convolution block, and so the latency of the binaural path.
///
/// 128 frames is 2.7 ms at 48 kHz -- far under a video frame, so a picture
/// shown against the audio clock does not visibly lead -- and puts a 256-tap
/// HRIR in two partitions. Halving it would double the FFTs per second for no
/// audible gain; doubling it would only add latency.
pub const BINAURAL_PARTITION_FRAMES: usize = 128;

/// Corner of the optional LFE low-pass: the top of the LFE channel's band in
/// every cinema and broadcast specification.
pub const LFE_LOWPASS_HZ: f32 = 120.0;

/// Where a virtual speaker stands, as the app measures it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Direction {
    /// Degrees, negative to the left, 0 straight ahead, +-180 behind.
    pub azimuth_deg: f32,
    /// Degrees, positive up.
    pub elevation_deg: f32,
}

impl Direction {
    pub fn new(azimuth_deg: f32, elevation_deg: f32) -> Self {
        Self {
            azimuth_deg,
            elevation_deg,
        }
    }

    /// A unit vector in SOFA's cartesian convention: x forward, y left, z up.
    pub fn sofa_xyz(self) -> [f32; 3] {
        let az = self.azimuth_deg.to_radians();
        let el = self.elevation_deg.to_radians();
        [el.cos() * az.cos(), -el.cos() * az.sin(), el.sin()]
    }
}

/// What one source channel does in the binaural mix.
#[derive(Clone, Debug, PartialEq)]
pub enum ChannelRoute {
    /// Convolved with the HRIR pair of `direction`, at a linear gain.
    Speaker { direction: Direction, gain: f32 },
    /// Added to both ears equally, without HRIRs: the LFE, which has no
    /// direction. Optionally low-passed at [`LFE_LOWPASS_HZ`].
    BothEars { gain: f32, lowpass: bool },
    /// Not heard.
    Silent,
}

/// HRIR pairs by direction: in the app, an opened SOFA file.
pub trait HrirSource {
    /// Left and right impulse responses for `direction`, at the rate the
    /// filters will run at, any interaural delay already applied.
    fn hrir(&self, direction: Direction) -> [Vec<f32>; 2];
}

impl HrirSource for sofar::reader::Sofar {
    fn hrir(&self, direction: Direction) -> [Vec<f32>; 2] {
        let mut filter = sofar::reader::Filter::new(self.filter_len());
        let [x, y, z] = direction.sofa_xyz();
        self.filter(x, y, z, &mut filter);
        // Files that store minimum-phase HRIRs carry the interaural time
        // difference as a delay instead; put it back as leading zeros.
        let file_sr = self.sample_rate();
        let delayed = |ir: &[f32], delay_secs: f32| {
            let delay = (delay_secs * file_sr).round().max(0.0) as usize;
            let mut out = vec![0.0; delay];
            out.extend_from_slice(ir);
            out
        };
        [
            delayed(&filter.left, filter.ldelay),
            delayed(&filter.right, filter.rdelay),
        ]
    }
}

/// Coefficients of a biquad, transposed direct form II.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    /// Second-order Butterworth low-pass (RBJ cookbook, Q = 1/sqrt 2).
    fn lowpass(corner_hz: f32, sample_rate: u32) -> Self {
        let w0 = std::f32::consts::TAU * corner_hz / sample_rate.max(1) as f32;
        let alpha = w0.sin() / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let cos = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 - cos) / 2.0 / a0,
            b1: (1.0 - cos) / a0,
            b2: (1.0 - cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn run(&self, state: &mut [f32; 2], x: f32) -> f32 {
        let y = self.b0 * x + state[0];
        state[0] = self.b1 * x - self.a1 * y + state[1];
        state[1] = self.b2 * x - self.a2 * y;
        y
    }
}

/// How the filters treat one channel, as the callback needs it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Path {
    Convolved,
    Direct { gain: f32, lowpass: bool },
    Silent,
}

/// The binaural filters for one source layout: every channel's HRIR pair cut
/// into partitions and transformed, ready for the callback. Built off the
/// audio thread; immutable after.
pub struct BinauralFilters {
    channels: usize,
    partitions: usize,
    paths: Vec<Path>,
    /// `[channel][partition][ear][bin]`, flattened. Gains and the inverse
    /// FFT's 1/N are already folded in.
    spectra: Vec<Complex<f32>>,
    lowpass: Biquad,
    trim: f32,
}

/// Spectrum bins of one partition's 2P-point real FFT.
const BINS: usize = BINAURAL_PARTITION_FRAMES + 1;
/// The FFT length: one partition and the one before it.
const FFT_LEN: usize = 2 * BINAURAL_PARTITION_FRAMES;

impl BinauralFilters {
    /// Filters for a source whose channel `i` does `routes[i]`, at
    /// `sample_rate` (the output's, which `source` must already be at), with
    /// a linear output `trim`.
    pub fn build(
        source: &dyn HrirSource,
        routes: &[ChannelRoute],
        sample_rate: u32,
        trim: f32,
    ) -> Self {
        let mut irs: Vec<Option<[Vec<f32>; 2]>> = Vec::with_capacity(routes.len());
        let mut paths = Vec::with_capacity(routes.len());
        for route in routes {
            match route {
                ChannelRoute::Speaker { direction, gain } => {
                    let [mut left, mut right] = source.hrir(*direction);
                    for v in left.iter_mut().chain(right.iter_mut()) {
                        *v *= gain;
                    }
                    irs.push(Some([left, right]));
                    paths.push(Path::Convolved);
                }
                ChannelRoute::BothEars { gain, lowpass } => {
                    irs.push(None);
                    paths.push(Path::Direct {
                        gain: *gain,
                        lowpass: *lowpass,
                    });
                }
                ChannelRoute::Silent => {
                    irs.push(None);
                    paths.push(Path::Silent);
                }
            }
        }
        let longest = irs
            .iter()
            .flatten()
            .map(|pair| pair[0].len().max(pair[1].len()))
            .max()
            .unwrap_or(0);
        let partitions = longest.div_ceil(BINAURAL_PARTITION_FRAMES).max(1);
        let mut spectra = vec![Complex::default(); routes.len() * partitions * 2 * BINS];
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_LEN);
        let mut time = fft.make_input_vec();
        let mut scratch = fft.make_scratch_vec();
        // The inverse FFT is unnormalised; dividing once here saves it a
        // multiply per sample per block.
        let scale = 1.0 / FFT_LEN as f32;
        for (ch, pair) in irs.iter().enumerate() {
            let Some(pair) = pair else {
                continue;
            };
            for (ear, ir) in pair.iter().enumerate() {
                for part in 0..partitions {
                    time.iter_mut().for_each(|v| *v = 0.0);
                    let start = part * BINAURAL_PARTITION_FRAMES;
                    for (i, v) in ir
                        .iter()
                        .skip(start)
                        .take(BINAURAL_PARTITION_FRAMES)
                        .enumerate()
                    {
                        time[i] = v * scale;
                    }
                    let at = ((ch * partitions + part) * 2 + ear) * BINS;
                    let out = &mut spectra[at..at + BINS];
                    fft.process_with_scratch(&mut time, out, &mut scratch)
                        .expect("buffer lengths come from the plan");
                }
            }
        }
        Self {
            channels: routes.len(),
            partitions,
            paths,
            spectra,
            lowpass: Biquad::lowpass(LFE_LOWPASS_HZ, sample_rate),
            trim,
        }
    }

    /// The source channel count these filters are for.
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn partitions(&self) -> usize {
        self.partitions
    }

    fn spectrum(&self, ch: usize, part: usize, ear: usize) -> &[Complex<f32>] {
        let at = ((ch * self.partitions + part) * 2 + ear) * BINS;
        &self.spectra[at..at + BINS]
    }
}

/// The running convolution, owned by the output callback.
///
/// Allocates only when the shape changes (another channel count or HRIR
/// length): a new clip, not a frame. A change of filters with the same shape
/// keeps the input history and crossfades from the old filters to the new
/// over one block, so dragging a speaker does not click.
pub struct BinauralState {
    current: Option<Arc<BinauralFilters>>,
    fading_from: Option<Arc<BinauralFilters>>,
    channels: usize,
    partitions: usize,
    fft: Option<Arc<dyn RealToComplex<f32>>>,
    ifft: Option<Arc<dyn ComplexToReal<f32>>>,
    /// Per channel, the previous block and the one being filled.
    input: Vec<f32>,
    /// Per channel and partition slot, the spectra of past input windows.
    fdl: Vec<Complex<f32>>,
    fdl_head: usize,
    /// Frames taken into the block being filled; also the read position in
    /// `output`, which holds the block computed last time.
    fill: usize,
    output: [Vec<f32>; 2],
    /// The direct (LFE) path for the block being filled, both ears alike.
    direct: Vec<f32>,
    lowpass_state: Vec<[f32; 2]>,
    time: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    accum: [Vec<Complex<f32>>; 2],
    fft_scratch: Vec<Complex<f32>>,
    ifft_scratch: Vec<Complex<f32>>,
    previous_block: Vec<f32>,
    dirty: bool,
}

impl Default for BinauralState {
    fn default() -> Self {
        Self {
            current: None,
            fading_from: None,
            channels: 0,
            partitions: 0,
            fft: None,
            ifft: None,
            input: Vec::new(),
            fdl: Vec::new(),
            fdl_head: 0,
            fill: 0,
            output: [Vec::new(), Vec::new()],
            direct: Vec::new(),
            lowpass_state: Vec::new(),
            time: Vec::new(),
            spectrum: Vec::new(),
            accum: [Vec::new(), Vec::new()],
            fft_scratch: Vec::new(),
            ifft_scratch: Vec::new(),
            previous_block: Vec::new(),
            dirty: false,
        }
    }
}

impl BinauralState {
    /// Forget all history, so nothing of what played before comes out after
    /// a stop or once the binaural path is switched off. Free when already
    /// clear.
    pub fn reset(&mut self) {
        if !self.dirty {
            return;
        }
        self.input.iter_mut().for_each(|v| *v = 0.0);
        self.fdl.iter_mut().for_each(|v| *v = Complex::default());
        self.output
            .iter_mut()
            .for_each(|ear| ear.iter_mut().for_each(|v| *v = 0.0));
        self.direct.iter_mut().for_each(|v| *v = 0.0);
        self.lowpass_state.iter_mut().for_each(|s| *s = [0.0; 2]);
        self.fdl_head = 0;
        self.fill = 0;
        self.fading_from = None;
        self.dirty = false;
    }

    fn reshape(&mut self, channels: usize, partitions: usize) {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_LEN);
        let ifft = planner.plan_fft_inverse(FFT_LEN);
        self.fft_scratch = fft.make_scratch_vec();
        self.ifft_scratch = ifft.make_scratch_vec();
        self.time = fft.make_input_vec();
        self.spectrum = fft.make_output_vec();
        self.accum = [fft.make_output_vec(), fft.make_output_vec()];
        self.fft = Some(fft);
        self.ifft = Some(ifft);
        self.channels = channels;
        self.partitions = partitions;
        self.input = vec![0.0; channels * FFT_LEN];
        self.fdl = vec![Complex::default(); channels * partitions * BINS];
        self.output = [
            vec![0.0; BINAURAL_PARTITION_FRAMES],
            vec![0.0; BINAURAL_PARTITION_FRAMES],
        ];
        self.direct = vec![0.0; BINAURAL_PARTITION_FRAMES];
        self.lowpass_state = vec![[0.0; 2]; channels];
        self.previous_block = vec![0.0; 2 * BINAURAL_PARTITION_FRAMES];
        self.fdl_head = 0;
        self.fill = 0;
        self.fading_from = None;
        self.current = None;
        self.dirty = false;
    }

    /// Take one frame of the source (`frame[ch]` for every channel) and give
    /// back one frame for the two ears, [`BINAURAL_PARTITION_FRAMES`] later.
    pub fn process_frame(&mut self, filters: &Arc<BinauralFilters>, frame: &[f32]) -> [f32; 2] {
        let switched = !self
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, filters));
        if switched {
            if filters.channels != self.channels || filters.partitions != self.partitions {
                self.reshape(filters.channels, filters.partitions);
            } else if self.dirty {
                // Same shape: keep the history, fade over to the new filters
                // in the next block.
                self.fading_from = self.current.take();
            }
            self.current = Some(Arc::clone(filters));
        }
        self.dirty = true;

        let at = self.fill;
        for ch in 0..self.channels {
            let x = frame.get(ch).copied().unwrap_or(0.0);
            // Kept for every channel, so a channel that changes path while
            // the filters crossfade still has its history.
            self.input[ch * FFT_LEN + BINAURAL_PARTITION_FRAMES + at] = x;
            match filters.paths[ch] {
                Path::Convolved => {}
                Path::Direct { gain, lowpass } => {
                    let x = if lowpass {
                        filters.lowpass.run(&mut self.lowpass_state[ch], x)
                    } else {
                        x
                    };
                    self.direct[at] += x * gain;
                }
                Path::Silent => {}
            }
        }
        let out = [self.output[0][at], self.output[1][at]];
        self.fill += 1;
        if self.fill == BINAURAL_PARTITION_FRAMES {
            self.compute_block(filters);
            self.fill = 0;
        }
        out
    }

    /// Transform the block just filled, convolve, and leave the result in
    /// `output` for the next block's frames to read.
    fn compute_block(&mut self, filters: &Arc<BinauralFilters>) {
        let (Some(fft), Some(ifft)) = (self.fft.clone(), self.ifft.clone()) else {
            return;
        };
        let parts = self.partitions;
        let head = self.fdl_head;
        for ch in 0..self.channels {
            let window = &mut self.input[ch * FFT_LEN..(ch + 1) * FFT_LEN];
            let convolved = filters.paths[ch] == Path::Convolved
                || self
                    .fading_from
                    .as_ref()
                    .is_some_and(|old| old.paths.get(ch) == Some(&Path::Convolved));
            if convolved {
                self.time.copy_from_slice(window);
                let slot = (ch * parts + head) * BINS;
                // Lengths come from the plan, so this cannot fail; the audio
                // thread must not panic over it all the same.
                let _ = fft.process_with_scratch(
                    &mut self.time,
                    &mut self.fdl[slot..slot + BINS],
                    &mut self.fft_scratch,
                );
            }
            // Slide: this block becomes the previous one.
            window.copy_within(BINAURAL_PARTITION_FRAMES.., 0);
        }

        let fading_from = self.fading_from.take();
        if let Some(old) = fading_from.as_ref() {
            self.convolve(old, head, ifft.as_ref());
            for ear in 0..2 {
                self.previous_block
                    [ear * BINAURAL_PARTITION_FRAMES..(ear + 1) * BINAURAL_PARTITION_FRAMES]
                    .copy_from_slice(&self.output[ear]);
            }
        }
        self.convolve(filters, head, ifft.as_ref());
        let len = BINAURAL_PARTITION_FRAMES as f32;
        for ear in 0..2 {
            for i in 0..BINAURAL_PARTITION_FRAMES {
                let mut v = self.output[ear][i];
                if fading_from.is_some() {
                    let w = (i as f32 + 0.5) / len;
                    let old = self.previous_block[ear * BINAURAL_PARTITION_FRAMES + i];
                    v = old * (1.0 - w) + v * w;
                }
                self.output[ear][i] = (v + self.direct[i]) * filters.trim;
            }
        }
        self.direct.iter_mut().for_each(|v| *v = 0.0);
        self.fdl_head = (head + 1) % parts;
    }

    /// Sum delay line times `filters` over channels and partitions for both
    /// ears and inverse-transform into `output` (overlap-save: the second
    /// half of each window is the linear convolution).
    fn convolve(&mut self, filters: &BinauralFilters, head: usize, ifft: &dyn ComplexToReal<f32>) {
        let parts = self.partitions;
        for ear in 0..2 {
            let acc = &mut self.accum[ear];
            acc.iter_mut().for_each(|v| *v = Complex::default());
            for ch in 0..self.channels {
                if filters.paths.get(ch) != Some(&Path::Convolved) {
                    continue;
                }
                for part in 0..parts {
                    let slot = (ch * parts + (head + parts - part) % parts) * BINS;
                    let x = &self.fdl[slot..slot + BINS];
                    let h = filters.spectrum(ch, part, ear);
                    for ((a, x), h) in acc.iter_mut().zip(x).zip(h) {
                        *a += x * h;
                    }
                }
            }
            // A real signal's DC and Nyquist bins are real; float noise in
            // the products must not make the inverse FFT refuse them.
            acc[0].im = 0.0;
            acc[BINS - 1].im = 0.0;
            self.spectrum.copy_from_slice(acc);
            let _ = ifft.process_with_scratch(
                &mut self.spectrum,
                &mut self.time,
                &mut self.ifft_scratch,
            );
            self.output[ear].copy_from_slice(&self.time[BINAURAL_PARTITION_FRAMES..]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An HRIR source that answers every direction with the same pair.
    struct Fixed([Vec<f32>; 2]);

    impl HrirSource for Fixed {
        fn hrir(&self, _direction: Direction) -> [Vec<f32>; 2] {
            self.0.clone()
        }
    }

    /// A source whose left ear hears azimuth/90 and right ear 1 - that,
    /// through a one-tap response: enough to tell directions apart.
    struct Panning;

    impl HrirSource for Panning {
        fn hrir(&self, direction: Direction) -> [Vec<f32>; 2] {
            let left = (-direction.azimuth_deg / 180.0 + 0.5).clamp(0.0, 1.0);
            [vec![left], vec![1.0 - left]]
        }
    }

    /// Deterministic noise, so a failure reproduces.
    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(2_654_435_761).max(1);
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    fn direct_convolution(x: &[f32], h: &[f32]) -> Vec<f32> {
        (0..x.len())
            .map(|n| {
                h.iter()
                    .enumerate()
                    .filter(|(k, _)| *k <= n)
                    .map(|(k, hk)| hk * x[n - k])
                    .sum()
            })
            .collect()
    }

    fn run(
        state: &mut BinauralState,
        filters: &Arc<BinauralFilters>,
        input: &[Vec<f32>],
    ) -> [Vec<f32>; 2] {
        let frames = input[0].len();
        let mut out = [Vec::with_capacity(frames), Vec::with_capacity(frames)];
        let mut frame = vec![0.0; input.len()];
        for n in 0..frames {
            for (ch, x) in input.iter().enumerate() {
                frame[ch] = x[n];
            }
            let [l, r] = state.process_frame(filters, &frame);
            out[0].push(l);
            out[1].push(r);
        }
        out
    }

    #[test]
    fn partitioned_convolution_matches_direct_convolution() {
        for taps in [1usize, 100, 256, 512] {
            let left = noise(taps, 1);
            let right = noise(taps, 2);
            let source = Fixed([left.clone(), right.clone()]);
            let routes = vec![
                ChannelRoute::Speaker {
                    direction: Direction::new(0.0, 0.0),
                    gain: 1.0,
                },
                ChannelRoute::Speaker {
                    direction: Direction::new(0.0, 0.0),
                    gain: 0.5,
                },
            ];
            let filters = Arc::new(BinauralFilters::build(&source, &routes, 48_000, 1.0));
            assert_eq!(
                filters.partitions(),
                taps.div_ceil(BINAURAL_PARTITION_FRAMES)
            );
            // Not a multiple of the block: the callback is called with 441
            // or 480 frames, and the state must not care.
            let frames = 1_337;
            let input = vec![noise(frames, 3), noise(frames, 4)];
            let mut state = BinauralState::default();
            let out = run(&mut state, &filters, &input);
            let mixed: Vec<f32> = (0..frames)
                .map(|n| input[0][n] + 0.5 * input[1][n])
                .collect();
            for (ear, ir) in [&left, &right].into_iter().enumerate() {
                let expected = direct_convolution(&mixed, ir);
                for n in BINAURAL_PARTITION_FRAMES..frames {
                    let got = out[ear][n];
                    let want = expected[n - BINAURAL_PARTITION_FRAMES];
                    assert!(
                        (got - want).abs() < 1e-3,
                        "taps {taps} ear {ear} frame {n}: {got} vs {want}"
                    );
                }
            }
        }
    }

    #[test]
    fn switching_filters_crossfades_instead_of_stepping() {
        let routes = |az: f32| {
            vec![ChannelRoute::Speaker {
                direction: Direction::new(az, 0.0),
                gain: 1.0,
            }]
        };
        let left = Arc::new(BinauralFilters::build(
            &Panning,
            &routes(-90.0),
            48_000,
            1.0,
        ));
        let right = Arc::new(BinauralFilters::build(&Panning, &routes(90.0), 48_000, 1.0));
        let mut state = BinauralState::default();
        let dc = vec![vec![1.0; 4 * BINAURAL_PARTITION_FRAMES]];
        let before = run(&mut state, &left, &dc);
        assert!(
            (before[0].last().unwrap() - 1.0).abs() < 1e-4,
            "settled on the left"
        );
        let after = run(&mut state, &right, &dc);
        // The left ear goes from 1 to 0 over one block, never in one step.
        let steps: Vec<f32> = after[0].windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        let largest = steps.iter().fold(0.0f32, |m, s| m.max(*s));
        assert!(largest < 0.02, "largest step {largest}");
        assert!(after[0].last().unwrap().abs() < 1e-4, "ends on the right");
        assert!((after[1].last().unwrap() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn the_lfe_reaches_both_ears_alike_and_can_be_low_passed() {
        let routes = vec![ChannelRoute::BothEars {
            gain: 0.5,
            lowpass: false,
        }];
        let filters = Arc::new(BinauralFilters::build(&Panning, &routes, 48_000, 1.0));
        let mut state = BinauralState::default();
        let input = vec![noise(1_000, 9)];
        let out = run(&mut state, &filters, &input);
        for n in BINAURAL_PARTITION_FRAMES..1_000 {
            assert_eq!(out[0][n], out[1][n]);
            assert!((out[0][n] - 0.5 * input[0][n - BINAURAL_PARTITION_FRAMES]).abs() < 1e-6);
        }
        // 2 kHz through the 120 Hz low-pass comes out far quieter.
        let lowpassed = Arc::new(BinauralFilters::build(
            &Panning,
            &[ChannelRoute::BothEars {
                gain: 1.0,
                lowpass: true,
            }],
            48_000,
            1.0,
        ));
        let mut state = BinauralState::default();
        let tone: Vec<f32> = (0..4_800)
            .map(|n| (std::f32::consts::TAU * 2_000.0 * n as f32 / 48_000.0).sin())
            .collect();
        let out = run(&mut state, &lowpassed, &[tone]);
        let peak = out[0][2_000..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(
            peak < 0.01,
            "2 kHz through the LFE low-pass peaks at {peak}"
        );
    }

    #[test]
    fn a_reset_forgets_what_played() {
        let routes = vec![ChannelRoute::Speaker {
            direction: Direction::new(0.0, 0.0),
            gain: 1.0,
        }];
        let filters = Arc::new(BinauralFilters::build(
            &Fixed([noise(256, 5), noise(256, 6)]),
            &routes,
            48_000,
            1.0,
        ));
        let mut state = BinauralState::default();
        run(&mut state, &filters, &[noise(500, 7)]);
        state.reset();
        let out = run(&mut state, &filters, &[vec![0.0; 600]]);
        assert!(out[0].iter().chain(&out[1]).all(|v| *v == 0.0));
    }

    #[test]
    fn directions_follow_sofa_axes() {
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
        assert!(close(Direction::new(0.0, 0.0).sofa_xyz(), [1.0, 0.0, 0.0]));
        // Left of the listener is negative here and +y in SOFA.
        assert!(close(
            Direction::new(-90.0, 0.0).sofa_xyz(),
            [0.0, 1.0, 0.0]
        ));
        assert!(close(
            Direction::new(90.0, 0.0).sofa_xyz(),
            [0.0, -1.0, 0.0]
        ));
        assert!(close(Direction::new(0.0, 90.0).sofa_xyz(), [0.0, 0.0, 1.0]));
    }

    /// The bundled HRTF, through the same calls the app makes.
    #[test]
    fn the_bundled_hrtf_puts_a_left_speaker_on_the_left() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/sofas/D1_HRIR_SOFA/D1_48K_24bit_256tap_FIR_SOFA.sofa");
        let sofa = sofar::reader::OpenOptions::new()
            .sample_rate(48_000.0)
            .open(&path)
            .expect("open the bundled SOFA");
        let energy = |dir: Direction| {
            let [l, r] = sofa.hrir(dir);
            (
                l.iter().map(|v| v * v).sum::<f32>(),
                r.iter().map(|v| v * v).sum::<f32>(),
            )
        };
        let (l, r) = energy(Direction::new(-90.0, 0.0));
        assert!(l > 10.0 * r, "left speaker: L {l} R {r}");
        let (l, r) = energy(Direction::new(110.0, 0.0));
        assert!(r > 5.0 * l, "right surround: L {l} R {r}");
        let (l, r) = energy(Direction::new(0.0, 0.0));
        assert!((l / r).ln().abs() < 0.5, "centre: L {l} R {r}");
        // A height speaker is not the ear-level one: its spectrum differs.
        let [front, _] = sofa.hrir(Direction::new(-30.0, 0.0));
        let [high, _] = sofa.hrir(Direction::new(-30.0, 45.0));
        let diff: f32 = front.iter().zip(&high).map(|(a, b)| (a - b).powi(2)).sum();
        assert!(diff > 0.05, "a 45 degree rise changed the HRIR by {diff}");
    }
}
