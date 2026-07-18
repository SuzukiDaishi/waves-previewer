//! Mains-hum removal (de-hum): a cascade of narrow biquad cuts at the hum
//! fundamental and its harmonics.
//!
//! STFT-based removal was rejected here on frequency-resolution grounds: at
//! 2048 bins / 48 kHz one bin is ~23 Hz, far too coarse to separate 50 Hz
//! from 60 Hz or to notch a fundamental without chewing the low end. IIR
//! notches have no such limit — an RBJ peaking cut with Q 30 at 60 Hz is
//! ~2 Hz wide.
//!
//! Detection sweeps the fundamental band (45..=65 Hz, covering both mains
//! standards and off-nominal drift) with Goertzel probes and returns the
//! peak when it stands out from the band's background.

/// One RBJ peaking-EQ biquad in Direct Form 1.
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    /// Peaking cut of `depth_db` (> 0) centered on `freq` with quality `q`.
    fn peaking_cut(freq: f32, sr: u32, q: f32, depth_db: f32) -> Self {
        let a = 10f64.powf(f64::from(-depth_db.abs()) / 40.0);
        let w = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(sr.max(1));
        let alpha = w.sin() / (2.0 * f64::from(q.max(0.1)));
        let cos_w = w.cos();
        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w;
        let a2 = 1.0 - alpha / a;
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DehumConfig {
    /// Hum fundamental in Hz (typically 50 or 60, may drift slightly).
    pub base_hz: f32,
    /// Number of harmonics to notch (1 = fundamental only).
    pub harmonics: u32,
    /// Notch quality; higher = narrower (Q 30 at 60 Hz is ~2 Hz wide).
    pub q: f32,
    /// Cut depth at each notch center, in dB.
    pub depth_db: f32,
}

impl Default for DehumConfig {
    fn default() -> Self {
        Self {
            base_hz: 50.0,
            harmonics: 8,
            q: 30.0,
            depth_db: 40.0,
        }
    }
}

/// Filter one whole channel through the notch cascade.
pub fn dehum_channel(ch: &[f32], sr: u32, cfg: &DehumConfig) -> Vec<f32> {
    let nyquist = sr.max(1) as f32 * 0.5;
    let mut filters: Vec<Biquad> = (1..=cfg.harmonics.max(1))
        .map(|k| cfg.base_hz.max(1.0) * k as f32)
        .take_while(|f| *f < nyquist * 0.95)
        .map(|f| Biquad::peaking_cut(f, sr, cfg.q, cfg.depth_db))
        .collect();
    let mut out = Vec::with_capacity(ch.len());
    for &v in ch {
        let mut acc = f64::from(v);
        for f in filters.iter_mut() {
            acc = f.process(acc);
        }
        out.push(acc as f32);
    }
    out
}

/// Splice `processed` into `original` over `[s, e)` with linear crossfades
/// of `fade` samples just inside both edges, so a range-limited de-hum joins
/// its surroundings without steps. Outside the range the original is
/// bit-identical.
pub fn splice_processed_range(
    original: &[f32],
    processed: &[f32],
    s: usize,
    e: usize,
    fade: usize,
) -> Vec<f32> {
    let n = original.len();
    let s = s.min(n);
    let e = e.min(n).max(s);
    let mut out = original.to_vec();
    if e <= s {
        return out;
    }
    let fade = fade.min((e - s) / 2);
    for i in s..e {
        let p = processed.get(i).copied().unwrap_or(original[i]);
        let w_in = if fade > 0 && i < s + fade {
            (i - s + 1) as f32 / (fade + 1) as f32
        } else {
            1.0
        };
        let w_out = if fade > 0 && i >= e - fade {
            (e - i) as f32 / (fade + 1) as f32
        } else {
            1.0
        };
        let w = w_in.min(w_out);
        out[i] = original[i] * (1.0 - w) + p * w;
    }
    out
}

/// Goertzel signal power at `freq` (normalized magnitude-squared).
fn goertzel_power(x: &[f32], sr: u32, freq: f32) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(sr.max(1));
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = f64::from(v) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (x.len().max(1) as f64 * x.len().max(1) as f64)
}

/// Sweep the mains band for a hum fundamental. Returns the peak frequency
/// when it clearly stands out from the band's background (otherwise `None`
/// so the UI can say "no hum found" instead of latching onto noise).
/// Analysis is capped to the first ~10 s for speed.
pub fn detect_hum_hz(ch: &[f32], sr: u32) -> Option<f32> {
    if ch.len() < sr.max(1) as usize / 2 {
        return None;
    }
    let n = ch.len().min(sr.max(1) as usize * 10);
    let x = &ch[..n];
    let mut best = (0.0f32, 0.0f64);
    let mut sum = 0.0f64;
    let mut count = 0usize;
    let mut f = 45.0f32;
    while f <= 65.0 {
        let p = goertzel_power(x, sr, f);
        sum += p;
        count += 1;
        if p > best.1 {
            best = (f, p);
        }
        f += 0.25;
    }
    let mean = sum / count.max(1) as f64;
    // A real hum line concentrates the band's energy at one probe.
    if best.1 > mean * 8.0 && best.1 > 1e-12 {
        Some(best.0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn tone(freq: f32, amp: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin() * amp)
            .collect()
    }

    fn add(a: &mut [f32], b: &[f32]) {
        for (x, y) in a.iter_mut().zip(b) {
            *x += *y;
        }
    }

    fn db(p: f64) -> f64 {
        10.0 * p.max(1e-30).log10()
    }

    #[test]
    fn hum_and_harmonics_are_cut_at_least_30db_while_1khz_survives() {
        let n = SR as usize * 2;
        let mut sig = tone(1000.0, 0.3, n);
        add(&mut sig, &tone(60.0, 0.1, n));
        add(&mut sig, &tone(180.0, 0.05, n));
        add(&mut sig, &tone(300.0, 0.05, n));
        let cfg = DehumConfig {
            base_hz: 60.0,
            harmonics: 8,
            ..Default::default()
        };
        let out = dehum_channel(&sig, SR, &cfg);
        // Skip the first half second of filter settling.
        let settle = SR as usize / 2;
        let (a, b) = (&sig[settle..], &out[settle..]);
        for hum in [60.0, 180.0, 300.0] {
            let drop = db(goertzel_power(a, SR, hum)) - db(goertzel_power(b, SR, hum));
            assert!(drop >= 30.0, "{hum} Hz only dropped {drop:.1} dB");
        }
        let content_change =
            db(goertzel_power(a, SR, 1000.0)) - db(goertzel_power(b, SR, 1000.0));
        assert!(
            content_change.abs() < 0.5,
            "1 kHz content moved {content_change:.2} dB"
        );
    }

    #[test]
    fn detect_finds_off_nominal_hum() {
        let n = SR as usize * 2;
        let mut sig = tone(1000.0, 0.2, n);
        add(&mut sig, &tone(50.5, 0.05, n));
        let found = detect_hum_hz(&sig, SR).expect("hum should be detected");
        assert!(
            (found - 50.5).abs() <= 0.3,
            "detected {found} Hz, expected ~50.5"
        );
    }

    #[test]
    fn detect_returns_none_without_hum() {
        let sig = tone(1000.0, 0.2, SR as usize * 2);
        assert_eq!(detect_hum_hz(&sig, SR), None);
    }

    #[test]
    fn splice_keeps_outside_bit_identical_and_blends_edges() {
        let orig = tone(220.0, 0.5, 10_000);
        let proc = vec![0.0f32; 10_000];
        let out = splice_processed_range(&orig, &proc, 2_000, 8_000, 100);
        assert_eq!(&out[..2_000], &orig[..2_000]);
        assert_eq!(&out[8_000..], &orig[8_000..]);
        // Deep inside the range the processed signal fully replaces.
        assert_eq!(&out[3_000..7_000], &proc[3_000..7_000]);
        // Within the fade the value sits between the two sources.
        let i = 2_010;
        let lo = orig[i].min(proc[i]);
        let hi = orig[i].max(proc[i]);
        assert!(out[i] >= lo && out[i] <= hi);
    }
}
