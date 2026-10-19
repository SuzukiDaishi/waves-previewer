//! Oversampling for the editor's tools that bend the waveform -- the
//! Brickwall Limiter and the Clipper. DSP and rules only.
//!
//! Clipping a wave, or turning its gain down within a few samples, makes
//! harmonics above the buffer's Nyquist, and at the buffer's own rate they
//! fold back into the audible band as aliasing: tones that are not
//! harmonics of anything in the music. Run at `factor` times the rate, the
//! harmonics have room to exist, and the filter on the way back down
//! removes them before they can fold.
//!
//! The filters are linear phase and centred on the sample they make, which
//! offline costs nothing -- the whole buffer is in memory -- so there is no
//! latency and nothing moves in time. The interpolator passes the original
//! samples through unchanged (its phase 0 is the centre tap of a Nyquist
//! filter), and the pair together changes only what lies in the transition
//! band, from 0.45 to 0.55 of the buffer's rate (above 20 kHz at 44.1 kHz).
//!
//! Both directions stream: memory is a few thousand frames however long the
//! audio, since an hour of stereo at 16x would be 22 GB.

use serde::{Deserialize, Serialize};

/// How many times the buffer's rate a tool runs at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Oversampling {
    /// At the buffer's own rate.
    #[default]
    Off,
    X2,
    X4,
    X8,
    X16,
}

impl Oversampling {
    pub const ALL: [Oversampling; 5] = [
        Oversampling::Off,
        Oversampling::X2,
        Oversampling::X4,
        Oversampling::X8,
        Oversampling::X16,
    ];

    pub fn factor(self) -> usize {
        match self {
            Oversampling::Off => 1,
            Oversampling::X2 => 2,
            Oversampling::X4 => 4,
            Oversampling::X8 => 8,
            Oversampling::X16 => 16,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Oversampling::Off => "Off",
            Oversampling::X2 => "2x",
            Oversampling::X4 => "4x",
            Oversampling::X8 => "8x",
            Oversampling::X16 => "16x",
        }
    }
}

/// The filters' reach, in samples of the buffer's rate: 32 either side of a
/// point (0.7 ms at 48 kHz).
pub const TAPS_PER_PHASE: usize = 64;
const HALF: usize = TAPS_PER_PHASE / 2;
/// The Kaiser window's shape: about 100 dB of stopband, which 64 taps per
/// phase hold over a transition band 0.1 of the buffer's rate wide.
const KAISER_BETA: f64 = 10.0;

/// The interpolation and decimation filters for one factor.
pub struct Oversampler {
    factor: usize,
    /// Phases 1..factor: taps over `x[n - HALF + 1 ..= n + HALF]` for the
    /// point `p / factor` after sample `n`. Phase 0 is `x[n]` itself.
    up: Vec<[f32; TAPS_PER_PHASE]>,
    /// The anti-alias filter on the way down: `factor * TAPS_PER_PHASE + 1`
    /// taps, symmetric about the sample it makes.
    down: Vec<f32>,
}

impl Oversampler {
    /// The filters for `os`; `None` for `Off`, which needs none.
    pub fn new(os: Oversampling) -> Option<Self> {
        let factor = os.factor();
        if factor < 2 {
            return None;
        }
        let taps = factor * TAPS_PER_PHASE + 1;
        let centre = factor * HALF;
        let norm = bessel_i0(KAISER_BETA);
        // A windowed sinc with its cutoff at the buffer's Nyquist: zero at
        // every multiple of `factor` but the centre.
        let h: Vec<f64> = (0..taps)
            .map(|k| {
                let d = k as f64 - centre as f64;
                let t = d / factor as f64;
                let sinc = if t == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                };
                let r = d / centre as f64;
                sinc * bessel_i0(KAISER_BETA * (1.0 - r * r).max(0.0).sqrt()) / norm
            })
            .collect();
        let sum: f64 = h.iter().sum();
        let down = h.iter().map(|v| (v / sum) as f32).collect();
        // Each phase on its own passes DC at unity.
        let up = (1..factor)
            .map(|p| {
                let raw: Vec<f64> = (0..TAPS_PER_PHASE)
                    .map(|i| h[p + (TAPS_PER_PHASE - 1 - i) * factor])
                    .collect();
                let sum: f64 = raw.iter().sum();
                let mut taps = [0.0f32; TAPS_PER_PHASE];
                for (t, v) in taps.iter_mut().zip(&raw) {
                    *t = (v / sum) as f32;
                }
                taps
            })
            .collect();
        Some(Self { factor, up, down })
    }

    pub fn factor(&self) -> usize {
        self.factor
    }

    /// High-rate frames run before the buffer's first sample, and after its
    /// last: the filters' half-length, so both ends are rebuilt from the
    /// whole of what the interpolator made rather than cut off.
    pub fn pad(&self) -> usize {
        self.factor * HALF
    }

    /// The high-rate frames a buffer of `len` samples is processed as.
    pub fn processed_len(&self, len: usize) -> usize {
        (len + TAPS_PER_PHASE) * self.factor
    }

    /// The `factor` high-rate frames from sample `n` of `x` onwards (zero
    /// outside `x`) into `out`.
    pub fn upsample(&self, x: &[f32], n: isize, out: &mut [f32]) {
        let mut win = [0.0f32; TAPS_PER_PHASE];
        let first = n - HALF as isize + 1;
        if first >= 0 && first as usize + TAPS_PER_PHASE <= x.len() {
            let first = first as usize;
            win.copy_from_slice(&x[first..first + TAPS_PER_PHASE]);
        } else {
            for (i, w) in win.iter_mut().enumerate() {
                let idx = first + i as isize;
                if idx >= 0 && (idx as usize) < x.len() {
                    *w = x[idx as usize];
                }
            }
        }
        out[0] = win[HALF - 1];
        for (o, taps) in out[1..self.factor].iter_mut().zip(&self.up) {
            *o = dot(&win, taps);
        }
    }

    /// A decimator for one channel.
    pub fn down_stream(&self) -> DownStream<'_> {
        let n = self.down.len();
        DownStream {
            taps: &self.down,
            factor: self.factor,
            ring: vec![0.0; 2 * n],
            pos: 0,
            pushed: 0,
        }
    }
}

/// The buffer's channels at the high rate, made a group of `factor` frames
/// at a time just ahead of where they are read, and kept for as far back as
/// the reader said it reaches. Frame `k` is the point `(k - pad) / factor`
/// samples into the buffer.
pub struct UpStream<'a> {
    os: &'a Oversampler,
    channels: &'a [&'a [f32]],
    base_len: usize,
    ring: Vec<Vec<f32>>,
    mask: usize,
    made: usize,
}

impl<'a> UpStream<'a> {
    /// `reach`: the most frames a read goes back behind the newest frame
    /// read so far.
    pub fn new(
        os: &'a Oversampler,
        channels: &'a [&'a [f32]],
        base_len: usize,
        reach: usize,
    ) -> Self {
        let cap = (reach + 2 * os.factor + 1)
            .next_power_of_two()
            .max(2 * os.factor);
        Self {
            os,
            channels,
            base_len,
            ring: vec![vec![0.0; cap]; channels.len()],
            mask: cap - 1,
            made: 0,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    /// Frames to process: the buffer and the filters' reach either side.
    pub fn len(&self) -> usize {
        self.os.processed_len(self.base_len)
    }

    pub fn is_empty(&self) -> bool {
        self.base_len == 0
    }

    /// Frame `k` of channel `c`.
    #[inline]
    pub fn at(&mut self, c: usize, k: usize) -> f32 {
        while k >= self.made {
            self.make();
        }
        debug_assert!(self.made - k <= self.mask + 1, "read past the kept frames");
        self.ring[c][k & self.mask]
    }

    fn make(&mut self) {
        let f = self.os.factor;
        let n = (self.made / f) as isize - HALF as isize;
        // The capacity is a power of two at least `f`, and so is `f`: a
        // group never wraps.
        let start = self.made & self.mask;
        for (ring, x) in self.ring.iter_mut().zip(self.channels) {
            let x = &x[..self.base_len.min(x.len())];
            self.os.upsample(x, n, &mut ring[start..start + f]);
        }
        self.made += f;
    }
}

/// One channel on its way back down: push every high-rate frame in order,
/// and a sample of the buffer's rate comes out once the filter has all it
/// needs -- `pad` frames later. `processed_len` pushes give exactly the
/// buffer's length back.
pub struct DownStream<'a> {
    taps: &'a [f32],
    factor: usize,
    /// The last `taps.len()` frames, written twice so they are always one
    /// contiguous slice.
    ring: Vec<f32>,
    pos: usize,
    pushed: usize,
}

impl DownStream<'_> {
    #[inline]
    pub fn push(&mut self, y: f32) -> Option<f32> {
        let n = self.taps.len();
        self.ring[self.pos] = y;
        self.ring[self.pos + n] = y;
        self.pos = if self.pos + 1 == n { 0 } else { self.pos + 1 };
        self.pushed += 1;
        (self.pushed >= n && (self.pushed - n) % self.factor == 0)
            .then(|| dot(&self.ring[self.pos..self.pos + n], self.taps))
    }
}

/// `a . b` in eight lanes, which the compiler vectorises (a single running
/// sum of floats cannot be reordered, so it would not).
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    let mut ca = a.chunks_exact(8);
    let mut cb = b.chunks_exact(8);
    for (x, y) in (&mut ca).zip(&mut cb) {
        for l in 0..8 {
            lanes[l] += x[l] * y[l];
        }
    }
    let mut sum: f32 = lanes.iter().sum();
    for (x, y) in ca.remainder().iter().zip(cb.remainder()) {
        sum += x * y;
    }
    sum
}

/// The modified Bessel function of the first kind, order zero.
fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..200 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn tone(hz: f64, len: usize, amp: f32) -> Vec<f32> {
        (0..len)
            .map(|i| ((i as f64 / SR * hz * std::f64::consts::TAU).sin() as f32) * amp)
            .collect()
    }

    /// `x` up and straight back down.
    fn round_trip(os: &Oversampler, x: &[f32]) -> Vec<f32> {
        let chans = [x];
        let mut up = UpStream::new(os, &chans, x.len(), 4);
        let mut down = os.down_stream();
        let mut out = Vec::with_capacity(x.len());
        for k in 0..up.len() {
            if let Some(v) = down.push(up.at(0, k)) {
                out.push(v);
            }
        }
        out
    }

    fn rms(v: &[f32]) -> f64 {
        (v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
    }

    #[test]
    fn off_needs_no_filters() {
        assert!(Oversampler::new(Oversampling::Off).is_none());
        for os in &Oversampling::ALL[1..] {
            assert_eq!(Oversampler::new(*os).unwrap().factor(), os.factor());
        }
    }

    #[test]
    fn the_original_samples_pass_through_the_interpolator() {
        let x = tone(1_234.0, 400, 0.7);
        let os = Oversampler::new(Oversampling::X4).unwrap();
        let mut out = [0.0f32; 4];
        for n in [0isize, 1, 31, 200, 399] {
            os.upsample(&x, n, &mut out);
            assert_eq!(out[0], x[n as usize]);
        }
        os.upsample(&x, -1, &mut out);
        assert_eq!(out[0], 0.0);
    }

    #[test]
    fn the_points_between_samples_follow_the_wave() {
        // A 1 kHz tone at 48 kHz: the interpolated points lie on the sine.
        let x = tone(1_000.0, 2_000, 0.5);
        let os = Oversampler::new(Oversampling::X8).unwrap();
        let mut out = [0.0f32; 8];
        for n in (200..1_800).step_by(37) {
            os.upsample(&x, n as isize, &mut out);
            for (p, v) in out.iter().enumerate() {
                let t = (n as f64 + p as f64 / 8.0) / SR;
                let want = (t * 1_000.0 * std::f64::consts::TAU).sin() * 0.5;
                assert!(
                    (f64::from(*v) - want).abs() < 1e-4,
                    "{n}+{p}/8: {v} vs {want}"
                );
            }
        }
    }

    #[test]
    fn up_and_down_gives_the_audio_back_in_place() {
        let len = 4_800;
        for os in &Oversampling::ALL[1..] {
            let os = Oversampler::new(*os).unwrap();
            for hz in [100.0, 1_000.0, 10_000.0, 20_000.0] {
                let x = tone(hz, len, 0.8);
                let y = round_trip(&os, &x);
                assert_eq!(y.len(), len);
                // Away from the ends, where a cut-off tone is not band-limited.
                let err: Vec<f32> = x[200..len - 200]
                    .iter()
                    .zip(&y[200..len - 200])
                    .map(|(a, b)| a - b)
                    .collect();
                let db = 20.0 * (rms(&err) / rms(&x[200..len - 200])).log10();
                assert!(db < -80.0, "{}x at {hz} Hz: error {db:.1} dB", os.factor());
            }
        }
    }

    #[test]
    fn the_way_down_removes_what_would_fold_back() {
        // A tone above the buffer's Nyquist at the high rate would land at
        // 48 - 30 = 18 kHz.
        let os = Oversampler::new(Oversampling::X4).unwrap();
        let hi_rate = SR * 4.0;
        let mut down = os.down_stream();
        let mut out = Vec::new();
        for k in 0..os.processed_len(9_600) {
            let v = (k as f64 / hi_rate * 30_000.0 * std::f64::consts::TAU).sin() as f32;
            if let Some(y) = down.push(v) {
                out.push(y);
            }
        }
        assert_eq!(out.len(), 9_600);
        let db = 20.0 * rms(&out[100..9_500]).log10();
        assert!(db < -90.0, "{db:.1} dB");
    }

    #[test]
    fn a_short_stream_reads_back_what_it_made() {
        let x = tone(500.0, 64, 0.5);
        let os = Oversampler::new(Oversampling::X2).unwrap();
        let chans = [x.as_slice(), x.as_slice()];
        let mut up = UpStream::new(&os, &chans, x.len(), 8);
        assert_eq!(up.len(), (64 + TAPS_PER_PHASE) * 2);
        assert_eq!(
            up.at(0, 0),
            0.0,
            "before the buffer: the filters' reach of silence"
        );
        // Frame `pad` is sample 0, `pad + 2` sample 1.
        assert_eq!(up.at(1, os.pad()), x[0]);
        assert_eq!(up.at(0, os.pad() + 2), x[1]);
    }
}
