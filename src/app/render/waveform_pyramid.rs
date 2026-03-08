use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Peak {
    pub min: f32,
    pub max: f32,
}

impl Peak {
    #[inline]
    pub fn zero() -> Self {
        Self { min: 0.0, max: 0.0 }
    }
}

#[derive(Clone, Debug)]
pub struct PeakLevel {
    pub bin_samples: usize,
    pub peaks: Vec<Peak>,
}

#[derive(Clone, Debug, Default)]
pub struct PeakPyramid {
    pub levels: Vec<PeakLevel>,
    pub total_samples: usize,
}

#[derive(Clone, Debug, Default)]
pub struct WaveformPyramidSet {
    pub mixdown: Arc<PeakPyramid>,
    pub channels: Vec<Arc<PeakPyramid>>,
    pub base_bin_samples: usize,
}

#[derive(Default)]
pub struct WaveformScratch {
    pub peaks: Vec<Peak>,
    pub mono: Vec<f32>,
    pub shapes: Vec<egui::Shape>,
    pub line_points: Vec<egui::Pos2>,
}

pub const DEFAULT_BASE_BIN_SAMPLES: usize = 64;
pub const DEFAULT_LOADING_OVERVIEW_BINS: usize = 2048;

#[derive(Clone, Debug)]
pub struct StreamingWaveformOverview {
    pub bins: Vec<Peak>,
    pub total_source_frames: usize,
    pub decoded_source_frames: usize,
    filled: Vec<bool>,
}

impl StreamingWaveformOverview {
    pub fn new(total_source_frames: usize, bins: usize) -> Self {
        let bins = bins.max(1);
        Self {
            bins: vec![Peak::zero(); bins],
            total_source_frames: total_source_frames.max(1),
            decoded_source_frames: 0,
            filled: vec![false; bins],
        }
    }

    pub fn append_mixdown_chunk(&mut self, start_frame_source: usize, chunk_channels: &[Vec<f32>]) {
        let chunk_len = min_channel_len(chunk_channels);
        if chunk_len == 0 || chunk_channels.is_empty() || self.bins.is_empty() {
            self.decoded_source_frames = self
                .decoded_source_frames
                .max(start_frame_source.saturating_add(chunk_len));
            return;
        }
        let end_frame_source = start_frame_source.saturating_add(chunk_len);
        let total = self.total_source_frames.max(end_frame_source).max(1);
        let bins_len = self.bins.len();
        let inv_channels = 1.0 / chunk_channels.len().max(1) as f32;
        let first_bin = (start_frame_source.saturating_mul(bins_len) / total).min(bins_len - 1);
        let last_bin = ((end_frame_source.saturating_sub(1)).saturating_mul(bins_len) / total)
            .min(bins_len - 1);
        for bin_idx in first_bin..=last_bin {
            let bin_start = bin_idx.saturating_mul(total) / bins_len;
            let mut bin_end = (bin_idx + 1).saturating_mul(total).div_ceil(bins_len);
            if bin_end <= bin_start {
                bin_end = bin_start + 1;
            }
            let overlap_start = start_frame_source.max(bin_start);
            let overlap_end = end_frame_source.min(bin_end);
            if overlap_start >= overlap_end {
                continue;
            }
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;
            for source_idx in overlap_start..overlap_end {
                let local_idx = source_idx.saturating_sub(start_frame_source);
                let mut sum = 0.0f32;
                for channel in chunk_channels {
                    sum += channel.get(local_idx).copied().unwrap_or(0.0);
                }
                let sample = sum * inv_channels;
                if sample < mn {
                    mn = sample;
                }
                if sample > mx {
                    mx = sample;
                }
            }
            if !mn.is_finite() || !mx.is_finite() {
                continue;
            }
            if self.filled[bin_idx] {
                self.bins[bin_idx].min = self.bins[bin_idx].min.min(mn);
                self.bins[bin_idx].max = self.bins[bin_idx].max.max(mx);
            } else {
                self.bins[bin_idx] = Peak { min: mn, max: mx };
                self.filled[bin_idx] = true;
            }
        }
        self.decoded_source_frames = self.decoded_source_frames.max(end_frame_source);
    }

    pub fn snapshot_minmax(&self) -> Vec<(f32, f32)> {
        self.bins.iter().map(|peak| (peak.min, peak.max)).collect()
    }

    pub fn seed_from_minmax(&mut self, overview: &[(f32, f32)]) {
        if overview.is_empty() || self.bins.is_empty() {
            return;
        }
        let src_len = overview.len().max(1);
        let dst_len = self.bins.len();
        for dst_idx in 0..dst_len {
            let src_start = dst_idx.saturating_mul(src_len) / dst_len;
            let mut src_end = (dst_idx + 1).saturating_mul(src_len).div_ceil(dst_len);
            if src_end <= src_start {
                src_end = src_start + 1;
            }
            let src_start = src_start.min(src_len - 1);
            let src_end = src_end.clamp(src_start + 1, src_len);
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;
            for &(lo, hi) in &overview[src_start..src_end] {
                mn = mn.min(lo);
                mx = mx.max(hi);
            }
            if mn.is_finite() && mx.is_finite() {
                self.bins[dst_idx] = Peak { min: mn, max: mx };
                self.filled[dst_idx] = true;
            }
        }
    }
}

impl PeakPyramid {
    pub fn from_samples(samples: &[f32], base_bin_samples: usize) -> Self {
        let base_bin_samples = base_bin_samples.max(1);
        let total_samples = samples.len();
        let mut levels = Vec::new();
        if total_samples == 0 {
            return Self {
                levels,
                total_samples,
            };
        }
        let mut peaks = build_fixed_bin_minmax_samples(samples, base_bin_samples);
        let mut bin_samples = base_bin_samples;
        levels.push(PeakLevel {
            bin_samples,
            peaks: peaks.clone(),
        });
        while peaks.len() > 1 {
            peaks = merge_peak_level(&peaks);
            bin_samples = bin_samples.saturating_mul(2);
            levels.push(PeakLevel {
                bin_samples,
                peaks: peaks.clone(),
            });
        }
        Self {
            levels,
            total_samples,
        }
    }

    pub fn from_mixdown_channels(
        channels: &[Vec<f32>],
        samples_len: usize,
        base_bin_samples: usize,
    ) -> Self {
        let base_bin_samples = base_bin_samples.max(1);
        let total_samples = samples_len.min(min_channel_len(channels));
        let mut levels = Vec::new();
        if total_samples == 0 {
            return Self {
                levels,
                total_samples,
            };
        }
        let mut peaks = build_fixed_bin_minmax_mixdown(channels, total_samples, base_bin_samples);
        let mut bin_samples = base_bin_samples;
        levels.push(PeakLevel {
            bin_samples,
            peaks: peaks.clone(),
        });
        while peaks.len() > 1 {
            peaks = merge_peak_level(&peaks);
            bin_samples = bin_samples.saturating_mul(2);
            levels.push(PeakLevel {
                bin_samples,
                peaks: peaks.clone(),
            });
        }
        Self {
            levels,
            total_samples,
        }
    }

    pub fn query_columns(
        &self,
        start: usize,
        end: usize,
        width_px: usize,
        spp: f32,
        out: &mut Vec<Peak>,
    ) {
        out.clear();
        if self.levels.is_empty() || width_px == 0 || start >= end || self.total_samples == 0 {
            return;
        }
        let start = start.min(self.total_samples);
        let end = end.min(self.total_samples);
        if start >= end {
            return;
        }
        let visible_len = end.saturating_sub(start);
        let level = self.pick_level(spp);
        let bin_samples = level.bin_samples.max(1);
        out.reserve(width_px);
        for x in 0..width_px {
            let s0 = start.saturating_add(visible_len.saturating_mul(x) / width_px);
            let mut s1 = start.saturating_add(visible_len.saturating_mul(x + 1) / width_px);
            if s1 <= s0 {
                s1 = (s0 + 1).min(end);
            }
            let s0 = s0.min(end.saturating_sub(1));
            let s1 = s1.max(s0 + 1).min(end);
            let i0 = (s0 / bin_samples).min(level.peaks.len().saturating_sub(1));
            let mut i1 = ((s1 + bin_samples - 1) / bin_samples).min(level.peaks.len());
            if i1 <= i0 {
                i1 = (i0 + 1).min(level.peaks.len());
            }
            out.push(aggregate_peaks(&level.peaks[i0..i1]));
        }
    }

    fn pick_level(&self, spp: f32) -> &PeakLevel {
        let mut selected = &self.levels[0];
        for level in &self.levels {
            if (level.bin_samples as f32) <= spp.max(1.0) {
                selected = level;
            } else {
                break;
            }
        }
        selected
    }
}

pub fn build_editor_waveform_cache(
    channels: &[Vec<f32>],
    samples_len: usize,
    overview_bins: usize,
    base_bin_samples: usize,
) -> (Vec<(f32, f32)>, Arc<WaveformPyramidSet>) {
    let samples_len = samples_len.min(min_channel_len(channels));
    let overview = crate::wave::build_waveform_minmax_from_channels(channels, samples_len, overview_bins);
    let mixdown = Arc::new(PeakPyramid::from_mixdown_channels(
        channels,
        samples_len,
        base_bin_samples,
    ));
    let channels = channels
        .iter()
        .map(|channel| Arc::new(PeakPyramid::from_samples(&channel[..samples_len.min(channel.len())], base_bin_samples)))
        .collect();
    let set = WaveformPyramidSet {
        mixdown,
        channels,
        base_bin_samples: base_bin_samples.max(1),
    };
    (overview, Arc::new(set))
}

pub fn build_visible_minmax(samples: &[f32], bins: usize, out: &mut Vec<Peak>) {
    out.clear();
    if samples.is_empty() || bins == 0 {
        return;
    }
    out.reserve(bins.min(samples.len()));
    let len = samples.len();
    let mut pos = 0.0f32;
    let step = (len as f32 / bins as f32).max(1.0);
    while (pos as usize) < len {
        let start = pos.floor() as usize;
        let mut end = (pos + step).floor() as usize;
        if end <= start {
            end = start + 1;
        }
        let end = end.min(len);
        out.push(aggregate_samples(&samples[start..end]));
        pos += step;
    }
}

pub fn build_mixdown_visible(samples: &[Vec<f32>], start: usize, end: usize, out: &mut Vec<f32>) {
    out.clear();
    let len = end.saturating_sub(start).min(min_channel_len(samples).saturating_sub(start));
    if len == 0 || samples.is_empty() {
        return;
    }
    out.resize(len, 0.0);
    let inv_channels = 1.0 / samples.len().max(1) as f32;
    for channel in samples {
        for (dst, &sample) in out.iter_mut().zip(channel[start..start + len].iter()) {
            *dst += sample * inv_channels;
        }
    }
}

pub fn build_mixdown_minmax_visible(
    channels: &[Vec<f32>],
    start: usize,
    end: usize,
    bins: usize,
    out: &mut Vec<Peak>,
) {
    out.clear();
    let samples_len = min_channel_len(channels);
    let start = start.min(samples_len);
    let end = end.min(samples_len);
    if channels.is_empty() || bins == 0 || start >= end {
        return;
    }
    let visible_len = end - start;
    out.reserve(bins.min(visible_len));
    let inv_channels = 1.0 / channels.len().max(1) as f32;
    let mut pos = 0.0f32;
    let step = (visible_len as f32 / bins as f32).max(1.0);
    while (pos as usize) < visible_len {
        let i0 = start + pos.floor() as usize;
        let mut i1 = start + (pos + step).floor() as usize;
        if i1 <= i0 {
            i1 = i0 + 1;
        }
        let i1 = i1.min(end);
        let mut mn = f32::INFINITY;
        let mut mx = f32::NEG_INFINITY;
        for sample_idx in i0..i1 {
            let mut sum = 0.0f32;
            for channel in channels {
                sum += channel[sample_idx];
            }
            let sample = sum * inv_channels;
            if sample < mn {
                mn = sample;
            }
            if sample > mx {
                mx = sample;
            }
        }
        out.push(if mn.is_finite() && mx.is_finite() {
            Peak { min: mn, max: mx }
        } else {
            Peak::zero()
        });
        pos += step;
    }
}

fn min_channel_len(channels: &[Vec<f32>]) -> usize {
    channels.iter().map(|channel| channel.len()).min().unwrap_or(0)
}

fn build_fixed_bin_minmax_samples(samples: &[f32], bin_samples: usize) -> Vec<Peak> {
    let mut out = Vec::with_capacity((samples.len() + bin_samples - 1) / bin_samples);
    let mut start = 0usize;
    while start < samples.len() {
        let end = (start + bin_samples).min(samples.len());
        out.push(aggregate_samples(&samples[start..end]));
        start = end;
    }
    out
}

fn build_fixed_bin_minmax_mixdown(
    channels: &[Vec<f32>],
    samples_len: usize,
    bin_samples: usize,
) -> Vec<Peak> {
    let mut out = Vec::with_capacity((samples_len + bin_samples - 1) / bin_samples);
    let inv_channels = 1.0 / channels.len().max(1) as f32;
    let mut start = 0usize;
    while start < samples_len {
        let end = (start + bin_samples).min(samples_len);
        let mut mn = f32::INFINITY;
        let mut mx = f32::NEG_INFINITY;
        for sample_idx in start..end {
            let mut sum = 0.0f32;
            for channel in channels {
                sum += channel[sample_idx];
            }
            let sample = sum * inv_channels;
            if sample < mn {
                mn = sample;
            }
            if sample > mx {
                mx = sample;
            }
        }
        out.push(if mn.is_finite() && mx.is_finite() {
            Peak { min: mn, max: mx }
        } else {
            Peak::zero()
        });
        start = end;
    }
    out
}

fn merge_peak_level(prev: &[Peak]) -> Vec<Peak> {
    let mut next = Vec::with_capacity((prev.len() + 1) / 2);
    let mut i = 0usize;
    while i < prev.len() {
        let a = prev[i];
        let b = prev.get(i + 1).copied().unwrap_or(a);
        next.push(Peak {
            min: a.min.min(b.min),
            max: a.max.max(b.max),
        });
        i += 2;
    }
    next
}

fn aggregate_samples(samples: &[f32]) -> Peak {
    let mut mn = f32::INFINITY;
    let mut mx = f32::NEG_INFINITY;
    for &sample in samples {
        if sample < mn {
            mn = sample;
        }
        if sample > mx {
            mx = sample;
        }
    }
    if mn.is_finite() && mx.is_finite() {
        Peak { min: mn, max: mx }
    } else {
        Peak::zero()
    }
}

fn aggregate_peaks(peaks: &[Peak]) -> Peak {
    let mut mn = f32::INFINITY;
    let mut mx = f32::NEG_INFINITY;
    for peak in peaks {
        if peak.min < mn {
            mn = peak.min;
        }
        if peak.max > mx {
            mx = peak.max;
        }
    }
    if mn.is_finite() && mx.is_finite() {
        Peak { min: mn, max: mx }
    } else {
        Peak::zero()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_mixdown_minmax_visible, build_visible_minmax, Peak, PeakPyramid,
        StreamingWaveformOverview,
        DEFAULT_BASE_BIN_SAMPLES,
    };

    #[test]
    fn peak_pyramid_level_zero_uses_fixed_bin_size() {
        let samples: Vec<f32> = (0..130).map(|i| i as f32).collect();
        let pyramid = PeakPyramid::from_samples(&samples, DEFAULT_BASE_BIN_SAMPLES);
        assert_eq!(pyramid.levels[0].bin_samples, DEFAULT_BASE_BIN_SAMPLES);
        assert_eq!(pyramid.levels[0].peaks.len(), 3);
        assert_eq!(pyramid.levels[0].peaks[0].min, 0.0);
        assert_eq!(pyramid.levels[0].peaks[0].max, 63.0);
        assert_eq!(pyramid.levels[0].peaks[1].min, 64.0);
        assert_eq!(pyramid.levels[0].peaks[1].max, 127.0);
        assert_eq!(pyramid.levels[0].peaks[2].min, 128.0);
        assert_eq!(pyramid.levels[0].peaks[2].max, 129.0);
    }

    #[test]
    fn peak_pyramid_pairwise_merge_is_correct() {
        let samples: Vec<f32> = (0..256).map(|i| (i as f32) - 128.0).collect();
        let pyramid = PeakPyramid::from_samples(&samples, 64);
        assert_eq!(pyramid.levels[1].bin_samples, 128);
        assert_eq!(pyramid.levels[1].peaks.len(), 2);
        assert_eq!(pyramid.levels[1].peaks[0].min, -128.0);
        assert_eq!(pyramid.levels[1].peaks[0].max, -1.0);
        assert_eq!(pyramid.levels[1].peaks[1].min, 0.0);
        assert_eq!(pyramid.levels[1].peaks[1].max, 127.0);
    }

    #[test]
    fn query_columns_matches_direct_visible_minmax_for_aligned_window() {
        let samples: Vec<f32> = (0..512)
            .map(|i| ((i as f32) * 0.05).sin())
            .collect();
        let pyramid = PeakPyramid::from_samples(&samples, 64);
        let mut from_query = Vec::new();
        let mut direct = Vec::new();
        pyramid.query_columns(64, 320, 4, 64.0, &mut from_query);
        build_visible_minmax(&samples[64..320], 4, &mut direct);
        assert_eq!(from_query.len(), direct.len());
        for (a, b) in from_query.iter().zip(direct.iter()) {
            assert!((a.min - b.min).abs() < 1.0e-6, "min mismatch: {:?} vs {:?}", a, b);
            assert!((a.max - b.max).abs() < 1.0e-6, "max mismatch: {:?} vs {:?}", a, b);
        }
    }

    #[test]
    fn build_mixdown_minmax_visible_uses_actual_mixdown() {
        let channels = vec![
            vec![1.0, 1.0, -1.0, -1.0],
            vec![-1.0, 1.0, 1.0, -1.0],
        ];
        let mut peaks = Vec::new();
        build_mixdown_minmax_visible(&channels, 0, 4, 2, &mut peaks);
        assert_eq!(
            peaks,
            vec![
                Peak { min: 0.0, max: 1.0 },
                Peak { min: -1.0, max: 0.0 },
            ]
        );
    }

    #[test]
    fn streaming_waveform_overview_fills_bins_monotonically() {
        let mut overview = StreamingWaveformOverview::new(16, 4);
        overview.append_mixdown_chunk(0, &[vec![0.2; 4], vec![0.2; 4]]);
        let first = overview.snapshot_minmax();
        assert_eq!(first.len(), 4);
        assert_eq!(first[0], (0.2, 0.2));
        assert_eq!(first[1], (0.0, 0.0));
        assert_eq!(first[2], (0.0, 0.0));
        assert_eq!(first[3], (0.0, 0.0));

        overview.append_mixdown_chunk(8, &[vec![-0.4; 8], vec![0.0; 8]]);
        let second = overview.snapshot_minmax();
        assert_eq!(second[0], (0.2, 0.2));
        assert_eq!(second[1], (0.0, 0.0));
        assert!(second[2].0 <= -0.2 && second[2].1 >= -0.2);
        assert!(second[3].0 <= -0.2 && second[3].1 >= -0.2);
    }
}
