/// Build pos+step ranges over [0..len) split into `bins` slices.
pub fn pos_step_ranges(len: usize, bins: usize) -> Vec<(usize, usize)> {
    if len == 0 || bins == 0 {
        return Vec::new();
    }
    // Integer bin edges: float accumulation drifts and f32 cannot even represent
    // sample indices above 2^24 (~6 min at 48 kHz). k*len/bins keeps ranges exact,
    // contiguous, and ending at len.
    let mut out = Vec::with_capacity(bins);
    for k in 0..bins {
        let i0 = ((k as u128 * len as u128) / bins as u128) as usize;
        if i0 >= len {
            break;
        }
        let mut i1 = (((k as u128 + 1) * len as u128) / bins as u128) as usize;
        if i1 <= i0 {
            i1 = i0 + 1;
        }
        out.push((i0, i1.min(len)));
    }
    out
}

/// Compute (min,max) for each range.
pub fn minmax_over_ranges(samples: &[f32], ranges: &[(usize, usize)]) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(ranges.len());
    for &(i0, i1) in ranges {
        let mut mn = f32::INFINITY;
        let mut mx = f32::NEG_INFINITY;
        let end = i1.min(samples.len());
        let start = i0.min(end);
        for &v in &samples[start..end] {
            if v < mn {
                mn = v;
            }
            if v > mx {
                mx = v;
            }
        }
        if !mn.is_finite() || !mx.is_finite() {
            out.push((0.0, 0.0));
        } else {
            out.push((mn, mx));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pos_step_ranges_exact_beyond_f32_mantissa() {
        // Above 2^24 samples an f32 accumulator quantizes indices; ranges must
        // stay contiguous, non-empty, and cover [0, len) exactly.
        let len = (1usize << 24) + 12_345;
        let bins = 1000;
        let ranges = pos_step_ranges(len, bins);
        assert_eq!(ranges.len(), bins);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges.last().unwrap().1, len);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "ranges must be contiguous");
        }
        for &(i0, i1) in &ranges {
            assert!(i0 < i1, "ranges must be non-empty");
        }
    }
}
