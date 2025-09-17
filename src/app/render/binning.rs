/// Build pos+step ranges over [0..len) split into `bins` slices.
pub fn pos_step_ranges(len: usize, bins: usize) -> Vec<(usize, usize)> {
    if len == 0 || bins == 0 { return Vec::new(); }
    let step = (len as f32) / (bins as f32);
    let mut pos = 0.0f32;
    let mut out = Vec::with_capacity(bins);
    for _ in 0..bins {
        let i0 = pos.floor() as usize;
        pos += step;
        let mut i1 = pos.floor() as usize;
        if i1 <= i0 { i1 = i0 + 1; }
        if i0 >= len { break; }
        let i1 = i1.min(len);
        out.push((i0, i1));
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
            if v < mn { mn = v; }
            if v > mx { mx = v; }
        }
        if !mn.is_finite() || !mx.is_finite() { out.push((0.0, 0.0)); }
        else { out.push((mn, mx)); }
    }
    out
}

