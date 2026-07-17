//! Realtime loudness metering DSP (BS.1770): K-weighting at arbitrary sample
//! rates, 100 ms block-power windows for momentary (400 ms) / short-term (3 s)
//! LUFS, and a 4x-oversampled true-peak detector. Pure state machines — the
//! audio callback only feeds a lock-free tap ring (see `audio.rs`); a
//! low-priority thread drives these and publishes atomics for the UI.

/// One biquad stage as (b0, b1, b2, a1, a2), a0 normalized to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct KWeightCoeffs {
    pub shelf: BiquadCoeffs,
    pub highpass: BiquadCoeffs,
}

/// BS.1770 K-weighting for an arbitrary sample rate, via the analog
/// prototypes behind the ITU 48 kHz table (the pyloudnorm/Brecht De Man
/// recomputation): a +4 dB high-shelf around 1.68 kHz and a 38 Hz
/// second-order high-pass, both bilinear-transformed at `sr`.
pub fn k_weight_coeffs(sr: u32) -> KWeightCoeffs {
    let fs = sr.max(1) as f64;

    // Stage 1: spherical-head high shelf.
    let db = 3.999_843_853_973_347f64;
    let f0 = 1_681.974_450_955_533f64;
    let q = 0.707_175_2f64;
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let vh = 10f64.powf(db / 20.0);
    let vb = vh.powf(0.499_666_774_155);
    let a0 = 1.0 + k / q + k * k;
    let shelf = BiquadCoeffs {
        b0: (vh + vb * k / q + k * k) / a0,
        b1: 2.0 * (k * k - vh) / a0,
        b2: (vh - vb * k / q + k * k) / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    };

    // Stage 2: 38 Hz high-pass.
    let f0 = 38.135_470_876_002_09f64;
    let q = 0.500_327_1f64;
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let denom = 1.0 + k / q + k * k;
    let highpass = BiquadCoeffs {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: 2.0 * (k * k - 1.0) / denom,
        a2: (1.0 - k / q + k * k) / denom,
    };

    KWeightCoeffs { shelf, highpass }
}

/// Direct-form-II-transposed biquad state.
#[derive(Clone, Copy, Default)]
struct BiquadState {
    z1: f64,
    z2: f64,
}

impl BiquadState {
    #[inline]
    fn process(&mut self, c: &BiquadCoeffs, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }
}

/// Per-channel K-weighting filter (shelf + high-pass).
#[derive(Clone)]
pub struct KWeightFilter {
    coeffs: KWeightCoeffs,
    shelf: BiquadState,
    highpass: BiquadState,
}

impl KWeightFilter {
    pub fn new(sr: u32) -> Self {
        Self {
            coeffs: k_weight_coeffs(sr),
            shelf: BiquadState::default(),
            highpass: BiquadState::default(),
        }
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f64 {
        let y = self.shelf.process(&self.coeffs.shelf, f64::from(x));
        self.highpass.process(&self.coeffs.highpass, y)
    }

    pub fn reset(&mut self) {
        self.shelf = BiquadState::default();
        self.highpass = BiquadState::default();
    }
}

const BLOCK_MS: usize = 100;
const MOMENTARY_BLOCKS: usize = 4; // 400 ms
const SHORT_TERM_BLOCKS: usize = 30; // 3 s

/// Streaming momentary / short-term loudness over a stereo feed.
pub struct LoudnessMeter {
    filters: [KWeightFilter; 2],
    samples_per_block: usize,
    cur_sum: f64,
    cur_count: usize,
    /// Mean K-weighted power (z_L + z_R) per completed 100 ms block.
    blocks: std::collections::VecDeque<f64>,
}

impl LoudnessMeter {
    pub fn new(sr: u32) -> Self {
        Self {
            filters: [KWeightFilter::new(sr), KWeightFilter::new(sr)],
            samples_per_block: (sr.max(1) as usize * BLOCK_MS / 1000).max(1),
            cur_sum: 0.0,
            cur_count: 0,
            blocks: std::collections::VecDeque::with_capacity(SHORT_TERM_BLOCKS + 1),
        }
    }

    pub fn reset(&mut self) {
        for f in &mut self.filters {
            f.reset();
        }
        self.cur_sum = 0.0;
        self.cur_count = 0;
        self.blocks.clear();
    }

    /// Feed one chunk of stereo frames (equal lengths; mono callers pass the
    /// same slice twice).
    pub fn push(&mut self, l: &[f32], r: &[f32]) {
        let n = l.len().min(r.len());
        for i in 0..n {
            let yl = self.filters[0].process(l[i]);
            let yr = self.filters[1].process(r[i]);
            self.cur_sum += yl * yl + yr * yr;
            self.cur_count += 1;
            if self.cur_count >= self.samples_per_block {
                self.blocks
                    .push_back(self.cur_sum / self.cur_count as f64);
                if self.blocks.len() > SHORT_TERM_BLOCKS {
                    self.blocks.pop_front();
                }
                self.cur_sum = 0.0;
                self.cur_count = 0;
            }
        }
    }

    fn lufs_over_last(&self, blocks: usize) -> Option<f32> {
        if self.blocks.len() < blocks {
            return None;
        }
        let mean: f64 =
            self.blocks.iter().rev().take(blocks).sum::<f64>() / blocks as f64;
        if mean <= 0.0 {
            return Some(f32::NEG_INFINITY);
        }
        Some((-0.691 + 10.0 * mean.log10()) as f32)
    }

    /// Momentary loudness (last 400 ms), once enough audio has been fed.
    pub fn momentary_lufs(&self) -> Option<f32> {
        self.lufs_over_last(MOMENTARY_BLOCKS)
    }

    /// Short-term loudness (last 3 s).
    pub fn short_term_lufs(&self) -> Option<f32> {
        self.lufs_over_last(SHORT_TERM_BLOCKS)
    }
}

const TP_PHASES: usize = 4;
const TP_TAPS_PER_PHASE: usize = 12; // 48-tap prototype

/// 4x polyphase FIR interpolator bank (windowed sinc, Blackman).
fn tp_filter_bank() -> [[f32; TP_TAPS_PER_PHASE]; TP_PHASES] {
    let total = TP_PHASES * TP_TAPS_PER_PHASE;
    let center = (total - 1) as f64 / 2.0;
    let mut bank = [[0f32; TP_TAPS_PER_PHASE]; TP_PHASES];
    for n in 0..total {
        let t = (n as f64 - center) / TP_PHASES as f64;
        let sinc = if t.abs() < 1e-12 {
            1.0
        } else {
            (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
        };
        let w = 0.42
            - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (total - 1) as f64).cos()
            + 0.08 * (4.0 * std::f64::consts::PI * n as f64 / (total - 1) as f64).cos();
        // Interleaved prototype -> polyphase decomposition. Each phase keeps
        // unity DC gain because the prototype is a 4x interpolator.
        bank[n % TP_PHASES][n / TP_PHASES] = (sinc * w) as f32;
    }
    // Normalize each phase to unity gain so a DC input reads 0 dBTP.
    for phase in bank.iter_mut() {
        let sum: f32 = phase.iter().sum();
        if sum.abs() > 1e-9 {
            for v in phase.iter_mut() {
                *v /= sum;
            }
        }
    }
    bank
}

/// Streaming inter-sample (true) peak detector for one channel.
pub struct TruePeakChannel {
    bank: [[f32; TP_TAPS_PER_PHASE]; TP_PHASES],
    hist: [f32; TP_TAPS_PER_PHASE],
}

impl TruePeakChannel {
    pub fn new() -> Self {
        Self {
            bank: tp_filter_bank(),
            hist: [0.0; TP_TAPS_PER_PHASE],
        }
    }

    pub fn reset(&mut self) {
        self.hist = [0.0; TP_TAPS_PER_PHASE];
    }

    /// Feed a chunk; returns the maximum oversampled magnitude seen in it.
    pub fn scan(&mut self, chunk: &[f32]) -> f32 {
        let mut max = 0.0f32;
        for &x in chunk {
            self.hist.rotate_right(1);
            self.hist[0] = x;
            for phase in &self.bank {
                let mut acc = 0.0f32;
                for (h, c) in self.hist.iter().zip(phase.iter()) {
                    acc += h * c;
                }
                max = max.max(acc.abs());
            }
        }
        max
    }
}

/// Linear magnitude -> dBTP (clamped at -99 dB for silence).
pub fn true_peak_db(mag: f32) -> f32 {
    if mag <= 1e-5 {
        -99.0
    } else {
        20.0 * mag.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32, sr: u32, secs: f32) -> Vec<f32> {
        (0..(sr as f32 * secs) as usize)
            .map(|i| (i as f32 / sr as f32 * freq * std::f32::consts::TAU).sin() * amp)
            .collect()
    }

    #[test]
    fn k_weight_matches_offline_constants_at_48k() {
        // wave.rs hardcodes the ITU 48 kHz table; the recomputation must land
        // on the same filter (offline and realtime paths agree).
        let c = k_weight_coeffs(48_000);
        let expect_shelf = [1.535_124_9, -2.691_696_2, 1.198_392_9, -1.690_659_3, 0.732_480_76];
        let got_shelf = [c.shelf.b0, c.shelf.b1, c.shelf.b2, c.shelf.a1, c.shelf.a2];
        for (g, e) in got_shelf.iter().zip(expect_shelf.iter()) {
            assert!((g - e).abs() < 2e-4, "shelf {got_shelf:?} vs {expect_shelf:?}");
        }
        let expect_hp = [1.0, -2.0, 1.0, -1.990_047_5, 0.990_072_25];
        let got_hp = [
            c.highpass.b0,
            c.highpass.b1,
            c.highpass.b2,
            c.highpass.a1,
            c.highpass.a2,
        ];
        for (g, e) in got_hp.iter().zip(expect_hp.iter()) {
            assert!((g - e).abs() < 2e-4, "highpass {got_hp:?} vs {expect_hp:?}");
        }
    }

    #[test]
    fn momentary_and_short_term_match_reference_tone() {
        // EBU Tech 3341 case 1: 997 Hz stereo sine at -23 dBFS -> -23 LUFS.
        for sr in [48_000u32, 44_100] {
            let amp = 10.0f32.powf(-23.0 / 20.0);
            let ch = sine(997.0, amp, sr, 4.0);
            let mut meter = LoudnessMeter::new(sr);
            meter.push(&ch, &ch);
            let m = meter.momentary_lufs().expect("momentary after 4 s");
            let s = meter.short_term_lufs().expect("short-term after 4 s");
            assert!((m + 23.0).abs() <= 0.3, "LUFS-M at {sr}: {m}");
            assert!((s + 23.0).abs() <= 0.3, "LUFS-S at {sr}: {s}");
        }
    }

    #[test]
    fn loudness_meter_needs_enough_audio_and_resets() {
        let sr = 48_000;
        let mut meter = LoudnessMeter::new(sr);
        meter.push(&vec![0.1; sr as usize / 5], &vec![0.1; sr as usize / 5]); // 200 ms
        assert!(meter.momentary_lufs().is_none(), "400 ms not reached yet");
        meter.push(&vec![0.1; sr as usize], &vec![0.1; sr as usize]);
        assert!(meter.momentary_lufs().is_some());
        meter.reset();
        assert!(meter.momentary_lufs().is_none());
    }

    #[test]
    fn true_peak_sees_intersample_overshoot() {
        // Quarter-band sine sampled at its zero-adjacent points: sample peak
        // well below the analog peak. fs/4 with a phase offset puts samples
        // at +-cos(pi/4)=0.707 while the continuous peak is 1.0.
        let sr = 48_000u32;
        let n = 4_800;
        let ch: Vec<f32> = (0..n)
            .map(|i| {
                ((i as f32 + 0.5) / sr as f32 * (sr as f32 / 4.0) * std::f32::consts::TAU).sin()
            })
            .collect();
        let sample_peak = ch.iter().fold(0.0f32, |a, v| a.max(v.abs()));
        assert!(sample_peak < 0.72, "fixture must hide its true peak");
        let mut tp = TruePeakChannel::new();
        let tp_mag = tp.scan(&ch);
        assert!(
            tp_mag > sample_peak + 0.1,
            "oversampling must reveal the inter-sample peak: sample={sample_peak} tp={tp_mag}"
        );
        assert!(tp_mag < 1.2, "and stay near the analog value: {tp_mag}");
    }

    #[test]
    fn true_peak_db_conversion() {
        assert_eq!(true_peak_db(0.0), -99.0);
        assert!((true_peak_db(1.0) - 0.0).abs() < 1e-6);
        assert!((true_peak_db(0.5) + 6.0206).abs() < 1e-3);
    }
}
