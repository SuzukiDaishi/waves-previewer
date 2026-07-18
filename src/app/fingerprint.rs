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

/// Milliseconds represented by one fingerprint frame hop.
pub const FP_FRAME_MS: f32 = FP_HOP as f32 * 1000.0 / FP_SAMPLE_RATE as f32;

#[derive(Clone, Debug, PartialEq)]
pub struct FileFingerprint {
    /// Exact-content hash over every channel's raw sample bits.
    pub content_hash: u64,
    /// One 31-bit spectral-shape hash per analysis frame.
    pub frames: Vec<u64>,
    pub duration_ms: f32,
    /// Leading silence (below -60 dBFS on the mixdown) trimmed off before
    /// hashing, in ms. Frames are content-aligned: a silence-padded copy
    /// hashes identically and the pad length shows up here instead.
    pub lead_trim_ms: f32,
    /// Mean spectral centroid in band units (0..FP_BANDS): a cheap O(1)
    /// prefilter — files whose centroids sit far apart can't be similar,
    /// so the O(frames) comparison is skipped for them.
    pub mean_band_centroid: f32,
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

    // Content-align the hash stream: drop leading DIGITAL ZEROS so padded
    // copies hash identically to their originals (the pad length becomes
    // `lead_trim_ms`). Exact zeros only, deliberately: silence padding in
    // game pipelines is zero samples, and an amplitude threshold would let
    // a noisy/dithered intro straddle it between two otherwise identical
    // files — desynchronizing their trims and breaking the aligned match
    // they previously had. Gain scaling preserves zeroness, so gain
    // variants keep matching too.
    let lead = ds.iter().position(|v| *v != 0.0).unwrap_or(ds.len());
    let lead_trim_ms = lead as f32 * 1000.0 / FP_SAMPLE_RATE as f32;
    let (frames, mean_band_centroid) = spectral_frame_hashes(&ds[lead..]);
    FileFingerprint {
        content_hash,
        frames,
        duration_ms,
        lead_trim_ms,
        mean_band_centroid,
    }
}

/// Decode `path` and fingerprint it.
pub fn fingerprint_file(path: &Path) -> anyhow::Result<FileFingerprint> {
    let (channels, sr) = crate::audio_io::decode_audio_multi(path)?;
    Ok(fingerprint_channels(&channels, sr))
}

fn spectral_frame_hashes(signal: &[f32]) -> (Vec<u64>, f32) {
    if signal.len() < FP_FRAME {
        return (Vec::new(), 0.0);
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
    let mut centroid_sum = 0.0f64;
    let mut centroid_frames = 0usize;
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
        {
            // Linear-energy centroid in band units for the prefilter.
            let mut wsum = 0.0f64;
            let mut esum = 0.0f64;
            for (b, lb) in bands.iter().enumerate() {
                let e = f64::from(lb.exp());
                wsum += b as f64 * e;
                esum += e;
            }
            if esum > 1e-12 {
                centroid_sum += wsum / esum;
                centroid_frames += 1;
            }
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
    let centroid = if centroid_frames > 0 {
        (centroid_sum / centroid_frames as f64) as f32
    } else {
        0.0
    };
    (hashes, centroid)
}

/// Mean per-bit agreement over the overlapping prefix of two hash streams.
fn frames_agreement(a: &[u64], b: &[u64]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let bits_per_frame = (FP_BANDS - 1) as u32;
    let mut agree = 0u64;
    for i in 0..n {
        let diff = (a[i] ^ b[i]).count_ones().min(bits_per_frame);
        agree += (bits_per_frame - diff) as u64;
    }
    agree as f32 / (n as u32 * bits_per_frame) as f32
}

/// Similarity tolerating a leading-time offset up to `max_offset_ms`.
/// The hash streams are already content-aligned (leading silence trimmed
/// at fingerprint time), so a silence-padded copy compares frame-for-frame;
/// the detected offset is the difference of the trimmed lead times.
/// Returns (similarity, offset_ms); positive when `b` starts later.
pub fn similarity_with_offset(
    a: &FileFingerprint,
    b: &FileFingerprint,
    max_offset_ms: f32,
) -> (f32, f32) {
    if a.frames.is_empty() || b.frames.is_empty() {
        return (0.0, 0.0);
    }
    let offset_ms = b.lead_trim_ms - a.lead_trim_ms;
    if offset_ms.abs() > max_offset_ms {
        return (0.0, 0.0);
    }
    let dmax = a.duration_ms.max(b.duration_ms);
    if dmax > 0.0
        && (a.duration_ms - b.duration_ms).abs() > dmax * MAX_DURATION_DELTA + max_offset_ms
    {
        return (0.0, 0.0);
    }
    let min_overlap = ((a.frames.len().min(b.frames.len()) as f32) * 0.6).max(4.0) as usize;
    if a.frames.len().min(b.frames.len()) < min_overlap {
        return (0.0, 0.0);
    }
    (frames_agreement(&a.frames, &b.frames), offset_ms)
}


/// Frame-hash similarity in [0, 1]: mean per-bit agreement over the
/// overlapping frames. 0 when either fingerprint is empty, when the
/// content-aligned hashes start at different times (differing lead trims),
/// or when durations differ by more than [`MAX_DURATION_DELTA`].
pub fn similarity(a: &FileFingerprint, b: &FileFingerprint) -> f32 {
    if a.frames.is_empty() || b.frames.is_empty() {
        return 0.0;
    }
    if (a.lead_trim_ms - b.lead_trim_ms).abs() > FP_FRAME_MS * 0.5 {
        return 0.0;
    }
    let dmax = a.duration_ms.max(b.duration_ms);
    if dmax > 0.0 && (a.duration_ms - b.duration_ms).abs() / dmax > MAX_DURATION_DELTA {
        return 0.0;
    }
    frames_agreement(&a.frames, &b.frames)
}

/// One reported duplicate group. `exact` when every member has the same
/// content hash; otherwise members are perceptually similar.
#[derive(Clone, Debug)]
pub struct DuplicateGroup {
    pub exact: bool,
    /// Largest detected time offset among the group's similar pairs, in ms
    /// (0 for aligned matches and exact groups).
    pub max_offset_ms: f32,
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
    cluster_duplicates_with_options(fps, threshold, false, 0.0)
}

/// Extra threshold demanded of pairs that only match at a non-zero offset
/// (compensates the higher false-positive rate of the offset search).
pub const OFFSET_THRESHOLD_BUMP: f32 = 0.025;
/// Default offset search range for the "allow offset" scan mode.
pub const MAX_SIMILAR_OFFSET_MS: f32 = 700.0;
/// Centroid prefilter: pairs whose mean band centroids differ by more than
/// this many band units are skipped without a frame comparison.
const CENTROID_GATE_BANDS: f32 = 1.5;

/// Cluster fingerprints into duplicate groups: exact content-hash matches
/// first, then pairwise similarity >= `threshold`. With `allow_offset`,
/// pairs are also tried at time shifts up to `max_offset_ms` (with a
/// slightly raised threshold). Cheap O(1) duration + centroid gates skip
/// the O(frames) comparison for hopeless pairs, keeping the O(n^2) sweep
/// fast on a few thousand files.
pub fn cluster_duplicates_with_options(
    fps: &[FileFingerprint],
    threshold: f32,
    allow_offset: bool,
    max_offset_ms: f32,
) -> Vec<DuplicateGroup> {
    let n = fps.len();
    let mut uf = UnionFind::new(n);
    let mut pair_sim: std::collections::HashMap<(usize, usize), (f32, f32)> =
        std::collections::HashMap::new();
    for i in 0..n {
        for j in (i + 1)..n {
            if fps[i].content_hash == fps[j].content_hash {
                uf.union(i, j);
                pair_sim.insert((i, j), (1.0, 0.0));
                continue;
            }
            // O(1) gates before the O(frames) comparison.
            let dmax = fps[i].duration_ms.max(fps[j].duration_ms);
            let allowance = if allow_offset { max_offset_ms } else { 0.0 };
            if dmax > 0.0
                && (fps[i].duration_ms - fps[j].duration_ms).abs()
                    > dmax * MAX_DURATION_DELTA + allowance
            {
                continue;
            }
            if (fps[i].mean_band_centroid - fps[j].mean_band_centroid).abs()
                > CENTROID_GATE_BANDS
            {
                continue;
            }
            let (sim, offset_ms) = if allow_offset {
                similarity_with_offset(&fps[i], &fps[j], max_offset_ms)
            } else {
                (similarity(&fps[i], &fps[j]), 0.0)
            };
            let effective_threshold = if offset_ms.abs() > FP_FRAME_MS * 0.5 {
                threshold + OFFSET_THRESHOLD_BUMP
            } else {
                threshold
            };
            if sim >= effective_threshold {
                uf.union(i, j);
                pair_sim.insert((i, j), (sim, offset_ms));
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
        let mut max_offset = 0.0f32;
        for (ai, &a) in members.iter().enumerate() {
            for &b in members.iter().skip(ai + 1) {
                let key = if a < b { (a, b) } else { (b, a) };
                // Transitive pairs (unioned through a chain) were below the
                // threshold or gated in the sweep; re-measure them with the
                // same mode the sweep used — plain similarity would hit the
                // lead-trim gate for offset-matched groups and report 0.
                let (sim, offset_ms) = pair_sim.get(&key).copied().unwrap_or_else(|| {
                    if allow_offset {
                        similarity_with_offset(&fps[a], &fps[b], max_offset_ms)
                    } else {
                        (similarity(&fps[a], &fps[b]), 0.0)
                    }
                });
                min_sim = min_sim.min(sim);
                max_offset = max_offset.max(offset_ms.abs());
            }
        }
        out.push(DuplicateGroup {
            exact,
            max_offset_ms: if exact { 0.0 } else { max_offset },
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
    #[test]
    fn silence_prefixed_copy_matches_only_with_offset() {
        // 1.2 s of content vs the same content behind 300 ms of silence.
        let base = content(7, (SR as f32 * 1.2) as usize);
        let mut padded = vec![0.0f32; (SR as f32 * 0.3) as usize];
        padded.extend_from_slice(&base);
        let fp_a = fingerprint_channels(&[base.clone()], SR);
        let fp_b = fingerprint_channels(&[padded], SR);

        // Aligned comparison misses it (this is the motivating case)...
        assert!(similarity(&fp_a, &fp_b) < SIMILARITY_THRESHOLD);
        // ...the offset search finds it near +300 ms.
        let (sim, offset_ms) = similarity_with_offset(&fp_a, &fp_b, MAX_SIMILAR_OFFSET_MS);
        assert!(
            sim >= SIMILARITY_THRESHOLD + OFFSET_THRESHOLD_BUMP,
            "offset similarity too low: {sim}"
        );
        assert!(
            (offset_ms - 300.0).abs() <= 35.0,
            "detected offset should be near 300 ms, got {offset_ms}"
        );

        // Clustering with offsets groups them and reports the offset.
        let unrelated = fingerprint_channels(&[content(99, (SR as f32 * 1.2) as usize)], SR);
        let fps = vec![fp_a.clone(), fp_b.clone(), unrelated];
        let groups =
            cluster_duplicates_with_options(&fps, SIMILARITY_THRESHOLD, true, MAX_SIMILAR_OFFSET_MS);
        assert_eq!(groups.len(), 1, "one similar group expected");
        assert_eq!(groups[0].members, vec![0, 1]);
        assert!(groups[0].max_offset_ms >= 200.0);
        // Without offsets the pair is not grouped.
        let plain = cluster_duplicates(&fps, SIMILARITY_THRESHOLD);
        assert!(plain.is_empty(), "aligned clustering must miss the padded copy");
    }

    #[test]
    fn centroid_gate_keeps_true_pairs_and_separates_extremes() {
        let low = fingerprint_channels(&[sine(120.0, SR as usize, 0.5)], SR);
        let high = fingerprint_channels(&[sine(6_000.0, SR as usize, 0.5)], SR);
        assert!(
            (high.mean_band_centroid - low.mean_band_centroid).abs() > 1.5,
            "extreme spectra must differ in centroid: low={} high={}",
            low.mean_band_centroid,
            high.mean_band_centroid
        );
        // A copy keeps its centroid, so the gate never rejects real pairs.
        let copy = fingerprint_channels(&[sine(120.0, SR as usize, 0.5)], SR);
        assert!((copy.mean_band_centroid - low.mean_band_centroid).abs() < 1e-3);
    }

}
