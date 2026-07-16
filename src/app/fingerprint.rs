//! Lightweight perceptual audio fingerprints for duplicate / similar-sound
//! detection, plus exact content hashing.
//!
//! Pipeline: mono mixdown → linear decimation to 16 kHz → 1024-sample
//! frames (50% overlap) → 32 log-spaced band energies per frame →
//! Chromaprint-style hash bits (sign of the band-difference delta between
//! consecutive frames), 31 bits per frame. Constant gain cancels out in
//! the double difference, so a copy with different volume still matches.
//! Exact duplicates are found with a plain content hash over the sample
//! bits (all channels).

use std::hash::Hasher;
use std::path::Path;

use realfft::RealFftPlanner;

const FP_SAMPLE_RATE: u32 = 16_000;
const FP_FRAME: usize = 1024;
const FP_HOP: usize = FP_FRAME / 2;
const FP_BANDS: usize = 32;
/// Frame-hash similarity two files must reach to count as "similar".
pub const SIMILARITY_THRESHOLD: f32 = 0.90;
/// Maximum relative duration difference for a similar pair.
pub const MAX_DURATION_DELTA: f32 = 0.10;

#[derive(Clone, Debug, PartialEq)]
pub struct FileFingerprint {
    /// Exact-content hash over every channel's raw sample bits.
    pub content_hash: u64,
    /// One 31-bit spectral-shape hash per analysis frame.
    pub frames: Vec<u64>,
    pub duration_ms: f32,
}

/// Fingerprint already-decoded audio.
pub fn fingerprint_channels(channels: &[Vec<f32>], sample_rate: u32) -> FileFingerprint {
    let sr = sample_rate.max(1);
    let frames_len = channels.iter().map(Vec::len).max().unwrap_or(0);
    let duration_ms = frames_len as f32 * 1000.0 / sr as f32;

    // Exact-content hash: channel count + every sample's bit pattern.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write_usize(channels.len());
    for ch in channels {
        hasher.write_usize(ch.len());
        for v in ch {
            hasher.write_u32(v.to_bits());
        }
    }
    let content_hash = hasher.finish();

    // Mono mixdown.
    let mut mono = vec![0.0f32; frames_len];
    if !channels.is_empty() {
        let inv = 1.0 / channels.len() as f32;
        for ch in channels {
            for (i, v) in ch.iter().enumerate() {
                mono[i] += v * inv;
            }
        }
    }
    // Linear decimation to the fingerprint rate.
    let ratio = sr as f64 / FP_SAMPLE_RATE as f64;
    let out_len = (frames_len as f64 / ratio).floor() as usize;
    let mut ds = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let i0 = pos.floor() as usize;
        let t = (pos - i0 as f64) as f32;
        let a = mono.get(i0).copied().unwrap_or(0.0);
        let b = mono.get(i0 + 1).copied().unwrap_or(a);
        ds.push(a + (b - a) * t);
    }

    FileFingerprint {
        content_hash,
        frames: spectral_frame_hashes(&ds),
        duration_ms,
    }
}

/// Decode `path` and fingerprint it.
pub fn fingerprint_file(path: &Path) -> anyhow::Result<FileFingerprint> {
    let (channels, sr) = crate::audio_io::decode_audio_multi(path)?;
    Ok(fingerprint_channels(&channels, sr))
}

fn spectral_frame_hashes(signal: &[f32]) -> Vec<u64> {
    if signal.len() < FP_FRAME {
        return Vec::new();
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let rfft = planner.plan_fft_forward(FP_FRAME);
    let mut spec = rfft.make_output_vec();
    let mut frame = vec![0.0f32; FP_FRAME];
    let window: Vec<f32> = (0..FP_FRAME)
        .map(|i| {
            let x = i as f32 / FP_FRAME as f32;
            0.5 - 0.5 * (2.0 * core::f32::consts::PI * x).cos()
        })
        .collect();
    // Log-spaced band edges over ~50 Hz .. Nyquist in FFT bins.
    let bins = FP_FRAME / 2 + 1;
    let min_bin = 3.0f32;
    let max_bin = (bins - 1) as f32;
    let edges: Vec<usize> = (0..=FP_BANDS)
        .map(|b| {
            let t = b as f32 / FP_BANDS as f32;
            (min_bin * (max_bin / min_bin).powf(t)).round() as usize
        })
        .collect();

    let frame_count = (signal.len() - FP_FRAME) / FP_HOP + 1;
    let mut prev_bands: Option<Vec<f32>> = None;
    let mut hashes = Vec::with_capacity(frame_count);
    for k in 0..frame_count {
        let start = k * FP_HOP;
        for i in 0..FP_FRAME {
            frame[i] = signal[start + i] * window[i];
        }
        if rfft.process(&mut frame, &mut spec).is_err() {
            break;
        }
        let mut bands = vec![0.0f32; FP_BANDS];
        for b in 0..FP_BANDS {
            let lo = edges[b].min(bins - 1);
            let hi = edges[b + 1].clamp(lo + 1, bins);
            let mut e = 0.0f32;
            for bin in lo..hi {
                e += spec[bin].norm_sqr();
            }
            bands[b] = (e / (hi - lo) as f32 + 1e-12).ln();
        }
        if let Some(prev) = &prev_bands {
            let mut h = 0u64;
            for b in 0..FP_BANDS - 1 {
                let cur_diff = bands[b] - bands[b + 1];
                let prev_diff = prev[b] - prev[b + 1];
                if cur_diff - prev_diff > 0.0 {
                    h |= 1 << b;
                }
            }
            hashes.push(h);
        }
        prev_bands = Some(bands);
    }
    hashes
}

/// Frame-hash similarity in [0, 1]: mean per-bit agreement over the
/// overlapping frames. 0 when either fingerprint is empty or durations
/// differ by more than [`MAX_DURATION_DELTA`].
pub fn similarity(a: &FileFingerprint, b: &FileFingerprint) -> f32 {
    if a.frames.is_empty() || b.frames.is_empty() {
        return 0.0;
    }
    let dmax = a.duration_ms.max(b.duration_ms);
    if dmax > 0.0 && (a.duration_ms - b.duration_ms).abs() / dmax > MAX_DURATION_DELTA {
        return 0.0;
    }
    let n = a.frames.len().min(b.frames.len());
    let bits_per_frame = (FP_BANDS - 1) as u32;
    let mut agree = 0u64;
    for i in 0..n {
        let diff = (a.frames[i] ^ b.frames[i]).count_ones().min(bits_per_frame);
        agree += (bits_per_frame - diff) as u64;
    }
    agree as f32 / (n as u32 * bits_per_frame) as f32
}

/// One reported duplicate group. `exact` when every member has the same
/// content hash; otherwise members are perceptually similar.
#[derive(Clone, Debug)]
pub struct DuplicateGroup {
    pub exact: bool,
    /// Indices into the caller's fingerprint list.
    pub members: Vec<usize>,
    /// Minimum pairwise similarity inside the group (1.0 for exact).
    pub min_similarity: f32,
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }
    fn find(&mut self, i: usize) -> usize {
        if self.parent[i] != i {
            let root = self.find(self.parent[i]);
            self.parent[i] = root;
        }
        self.parent[i]
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

/// Cluster fingerprints into duplicate groups: exact content-hash matches
/// first, then pairwise similarity >= `threshold` (only comparable-length
/// files are paired). O(n^2) over the candidate pairs — fine for a few
/// thousand files.
pub fn cluster_duplicates(fps: &[FileFingerprint], threshold: f32) -> Vec<DuplicateGroup> {
    let n = fps.len();
    let mut uf = UnionFind::new(n);
    // Pairwise similarity (also unions exact matches: identical content
    // yields identical frames).
    let mut pair_sim: std::collections::HashMap<(usize, usize), f32> =
        std::collections::HashMap::new();
    for i in 0..n {
        for j in (i + 1)..n {
            if fps[i].content_hash == fps[j].content_hash {
                uf.union(i, j);
                pair_sim.insert((i, j), 1.0);
                continue;
            }
            let sim = similarity(&fps[i], &fps[j]);
            if sim >= threshold {
                uf.union(i, j);
                pair_sim.insert((i, j), sim);
            }
        }
    }
    let mut groups: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    for i in 0..n {
        let root = uf.find(i);
        groups.entry(root).or_default().push(i);
    }
    let mut out = Vec::new();
    for (_, members) in groups {
        if members.len() < 2 {
            continue;
        }
        let exact = members
            .iter()
            .all(|&m| fps[m].content_hash == fps[members[0]].content_hash);
        let mut min_sim = 1.0f32;
        for (ai, &a) in members.iter().enumerate() {
            for &b in members.iter().skip(ai + 1) {
                let key = if a < b { (a, b) } else { (b, a) };
                let sim = pair_sim
                    .get(&key)
                    .copied()
                    .unwrap_or_else(|| similarity(&fps[a], &fps[b]));
                min_sim = min_sim.min(sim);
            }
        }
        out.push(DuplicateGroup {
            exact,
            members,
            min_similarity: if exact { 1.0 } else { min_sim },
        });
    }
    // Deterministic order: exact groups first, then by first member index.
    for g in &mut out {
        g.members.sort_unstable();
    }
    out.sort_by(|a, b| {
        b.exact
            .cmp(&a.exact)
            .then(a.members[0].cmp(&b.members[0]))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(freq: f32, len: usize, amp: f32) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * core::f32::consts::PI * freq * i as f32 / SR as f32).sin() * amp)
            .collect()
    }

    /// Deterministic LCG noise.
    fn lcg_noise(len: usize, seed: u64, amp: f32) -> Vec<f32> {
        let mut state = seed.max(1);
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (((state >> 33) as f32 / (u32::MAX >> 1) as f32) * 2.0 - 1.0) * amp
            })
            .collect()
    }

    /// A somewhat structured test signal (chirp + noise bed).
    fn content(seed: u64, len: usize) -> Vec<f32> {
        let noise = lcg_noise(len, seed, 0.05);
        (0..len)
            .map(|i| {
                let t = i as f32 / SR as f32;
                let f = 300.0 + 40.0 * (seed % 7) as f32 + 900.0 * t;
                (2.0 * core::f32::consts::PI * f * t).sin() * 0.4 + noise[i]
            })
            .collect()
    }

    #[test]
    fn identical_copies_match_exactly() {
        let a = content(1, SR as usize);
        let fa = fingerprint_channels(&[a.clone()], SR);
        let fb = fingerprint_channels(&[a], SR);
        assert_eq!(fa.content_hash, fb.content_hash);
        assert!(similarity(&fa, &fb) > 0.999);
    }

    #[test]
    fn gain_change_is_similar_but_not_exact() {
        let a = content(2, SR as usize);
        let quieter: Vec<f32> = a.iter().map(|v| v * 0.5).collect();
        let fa = fingerprint_channels(&[a], SR);
        let fq = fingerprint_channels(&[quieter], SR);
        assert_ne!(fa.content_hash, fq.content_hash);
        let sim = similarity(&fa, &fq);
        assert!(sim > SIMILARITY_THRESHOLD, "gain variant similarity {sim}");
    }

    #[test]
    fn unrelated_content_is_dissimilar() {
        let a = fingerprint_channels(&[content(3, SR as usize)], SR);
        let b = fingerprint_channels(&[lcg_noise(SR as usize, 99, 0.4)], SR);
        let sim = similarity(&a, &b);
        assert!(sim < SIMILARITY_THRESHOLD, "unrelated similarity {sim}");
        // Very different durations never pair.
        let c = fingerprint_channels(&[content(3, SR as usize / 2)], SR);
        assert_eq!(similarity(&a, &c), 0.0);
    }

    #[test]
    fn clustering_groups_exact_and_similar() {
        let base = content(4, SR as usize);
        let copy = base.clone();
        let quieter: Vec<f32> = base.iter().map(|v| v * 0.6).collect();
        let unrelated = lcg_noise(SR as usize, 123, 0.4);
        let fps = vec![
            fingerprint_channels(&[base], SR),
            fingerprint_channels(&[copy], SR),
            fingerprint_channels(&[quieter], SR),
            fingerprint_channels(&[unrelated], SR),
        ];
        let groups = cluster_duplicates(&fps, SIMILARITY_THRESHOLD);
        assert_eq!(groups.len(), 1, "one merged group expected: {groups:?}");
        let g = &groups[0];
        // The gain variant joins the exact pair through similarity, so the
        // merged group is not exact and holds indices 0..=2.
        assert_eq!(g.members, vec![0, 1, 2]);
        assert!(!g.exact);
        assert!(g.min_similarity >= SIMILARITY_THRESHOLD);
        assert!(
            !groups.iter().any(|g| g.members.contains(&3)),
            "unrelated file must not join any group"
        );
    }
}
