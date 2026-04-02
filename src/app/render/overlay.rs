use egui::{Color32, Painter, Rect};

use super::binning::{minmax_over_ranges, pos_step_ranges};

pub fn visible_half_amplitude(vertical_zoom: f32) -> f32 {
    1.0 / vertical_zoom.clamp(
        crate::app::EDITOR_MIN_VERTICAL_ZOOM,
        crate::app::EDITOR_MAX_VERTICAL_ZOOM,
    )
}

pub fn clamped_vertical_view_center(vertical_zoom: f32, vertical_view_center: f32) -> f32 {
    let zoom = vertical_zoom.clamp(
        crate::app::EDITOR_MIN_VERTICAL_ZOOM,
        crate::app::EDITOR_MAX_VERTICAL_ZOOM,
    );
    if zoom <= 1.0 {
        0.0
    } else {
        let half = visible_half_amplitude(zoom).clamp(0.0, 1.0);
        let limit = (1.0 - half).max(0.0);
        vertical_view_center.clamp(-limit, limit)
    }
}

pub fn waveform_y_from_amp(
    lane_rect: Rect,
    vertical_zoom: f32,
    vertical_view_center: f32,
    amp: f32,
) -> f32 {
    let zoom = vertical_zoom.clamp(
        crate::app::EDITOR_MIN_VERTICAL_ZOOM,
        crate::app::EDITOR_MAX_VERTICAL_ZOOM,
    );
    let visible_half = visible_half_amplitude(zoom).max(f32::EPSILON);
    let center = clamped_vertical_view_center(zoom, vertical_view_center);
    let normalized = ((amp.clamp(-1.0, 1.0) - center) / visible_half).clamp(-1.0, 1.0);
    lane_rect.center().y - normalized * (lane_rect.height() * 0.48)
}

#[derive(Clone, Copy, Debug)]
pub struct WaveformDeviceColumns {
    left_device_px: i32,
    column_count: usize,
    pixels_per_point: f32,
}

impl WaveformDeviceColumns {
    pub fn column_count(&self) -> usize {
        self.column_count
    }

    pub fn device_pixel_for_column(&self, idx: usize) -> i32 {
        self.left_device_px + idx.min(self.column_count.saturating_sub(1)) as i32
    }

    pub fn x_center_pt(&self, idx: usize) -> f32 {
        (self.device_pixel_for_column(idx) as f32 + 0.5) / self.pixels_per_point
    }

    fn min_bar_height_pt(&self) -> f32 {
        (1.0 / self.pixels_per_point.max(f32::EPSILON)).max(0.5)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AggregatedWaveColumn {
    pub min: f32,
    pub max: f32,
    pub color: Color32,
    pub stroke: f32,
}

pub fn compute_waveform_device_columns(
    span_rect: Rect,
    pixels_per_point: f32,
) -> WaveformDeviceColumns {
    let pixels_per_point = pixels_per_point.max(1.0);
    let left_device_px = (span_rect.left() * pixels_per_point).round() as i32;
    let column_count = ((span_rect.width().max(1.0) * pixels_per_point).round()).max(1.0) as usize;
    WaveformDeviceColumns {
        left_device_px,
        column_count,
        pixels_per_point,
    }
}

pub fn draw_aggregated_waveform_columns<F>(
    painter: &Painter,
    lane_rect: Rect,
    columns: &WaveformDeviceColumns,
    start_column: usize,
    column_count: usize,
    vertical_zoom: f32,
    vertical_view_center: f32,
    mut column_at: F,
) where
    F: FnMut(usize) -> Option<AggregatedWaveColumn>,
{
    if columns.column_count() == 0 || column_count == 0 || start_column >= columns.column_count() {
        return;
    }
    let max_count = column_count.min(columns.column_count().saturating_sub(start_column));
    let min_bar_height = columns.min_bar_height_pt();
    for local_idx in 0..max_count {
        let Some(column) = column_at(local_idx) else {
            continue;
        };
        let x = columns.x_center_pt(start_column + local_idx);
        if !x.is_finite() || !column.min.is_finite() || !column.max.is_finite() {
            continue;
        }
        let mut y0 = waveform_y_from_amp(lane_rect, vertical_zoom, vertical_view_center, column.max);
        let mut y1 = waveform_y_from_amp(lane_rect, vertical_zoom, vertical_view_center, column.min);
        if !y0.is_finite() || !y1.is_finite() {
            continue;
        }
        if (y1 - y0).abs() < min_bar_height {
            let mid = (y0 + y1) * 0.5;
            let half = min_bar_height * 0.5;
            y0 = mid - half;
            y1 = mid + half;
        }
        painter.line_segment(
            [egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))],
            egui::Stroke::new(column.stroke, column.color),
        );
    }
}

/// Map the visible original window [start, start+visible_len) into overlay domain of length `overlay_total`.
/// Returns (startb, endb, over_vis).
pub fn map_visible_overlay(
    start: usize,
    visible_len: usize,
    orig_total: usize,
    overlay_total: usize,
) -> (usize, usize, usize) {
    if orig_total == 0 || overlay_total == 0 || visible_len == 0 {
        return (0, 0, 0);
    }
    let ratio = overlay_total as f32 / orig_total as f32;
    let startb = ((start as f32) * ratio).round() as usize;
    let endb = startb + ((visible_len as f32) * ratio).ceil() as usize;
    let endb = endb.min(overlay_total);
    let startb = startb.min(endb);
    let over_vis = endb.saturating_sub(startb);
    (startb, endb, over_vis)
}

/// Build base px-column ranges over the visible original window using pos+step.
pub fn base_ranges_for_bins(start: usize, visible_len: usize, bins: usize) -> Vec<(usize, usize)> {
    let mut ranges = pos_step_ranges(visible_len, bins);
    for r in &mut ranges {
        r.0 += start;
        r.1 += start;
    }
    ranges
}

/// Map base ranges [i0,i1) in original domain to overlay ranges [o0,o1) for the same px columns.
pub fn map_ranges_to_overlay(
    ranges_base: &[(usize, usize)],
    start: usize,
    visible_len: usize,
    startb: usize,
    over_vis: usize,
) -> Vec<(usize, usize)> {
    if visible_len == 0 || over_vis == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(ranges_base.len());
    for &(i0, i1) in ranges_base {
        let o0 = startb
            + (((i0.saturating_sub(start)) as f32 * over_vis as f32 / visible_len as f32).round()
                as usize);
        let mut o1 = startb
            + (((i1.saturating_sub(start)) as f32 * over_vis as f32 / visible_len as f32).round()
                as usize);
        if o1 <= o0 {
            o1 = o0 + 1;
        }
        out.push((o0, o1));
    }
    out
}

/// Draw bins (min/max) locked to px columns over lane_rect width.
pub fn draw_bins_locked(
    painter: &Painter,
    lane_rect: Rect,
    columns: &WaveformDeviceColumns,
    bins_values: &[(f32, f32)],
    scale: f32,
    vertical_zoom: f32,
    vertical_view_center: f32,
    color: Color32,
    stroke: f32,
) {
    draw_aggregated_waveform_columns(
        painter,
        lane_rect,
        columns,
        0,
        bins_values.len(),
        vertical_zoom,
        vertical_view_center,
        |idx| {
            let &(mn0, mx0) = bins_values.get(idx)?;
            let mn = (mn0 * scale).clamp(-1.0, 1.0);
            let mx = (mx0 * scale).clamp(-1.0, 1.0);
            if !mn.is_finite() || !mx.is_finite() {
                return None;
            }
            Some(AggregatedWaveColumn {
                min: mn,
                max: mx,
                color,
                stroke,
            })
        },
    );
}

/// Convenience: compute overlay bins for base px columns using mapping and return min/max per px.
pub fn compute_overlay_bins_for_base_columns(
    start: usize,
    visible_len: usize,
    startb: usize,
    over_vis: usize,
    overlay_samples: &[f32],
    bins: usize,
) -> Vec<(f32, f32)> {
    let ranges_base = base_ranges_for_bins(start, visible_len, bins);
    let ranges_overlay = map_ranges_to_overlay(&ranges_base, start, visible_len, startb, over_vis);
    minmax_over_ranges(overlay_samples, &ranges_overlay)
}

/// Compute overlay bins for loop-unwrap preview with an anchor at loop_start.
/// Pre-loop region is 1:1 aligned; the tail is stretched to the overlay length.
pub fn compute_overlay_bins_for_unwrap(
    start: usize,
    visible_len: usize,
    base_total: usize,
    loop_start: usize,
    overlay_samples: &[f32],
    overlay_total: usize,
    bins: usize,
) -> Vec<(f32, f32)> {
    if visible_len == 0 || bins == 0 || overlay_total == 0 || base_total == 0 {
        return Vec::new();
    }
    if overlay_total <= base_total || loop_start >= base_total {
        return compute_overlay_bins_for_base_columns(
            start,
            visible_len,
            start.min(overlay_total),
            visible_len.min(overlay_total.saturating_sub(start)),
            overlay_samples,
            bins,
        );
    }
    let anchor = loop_start.min(base_total);
    let tail_base = base_total.saturating_sub(anchor).max(1) as f32;
    let tail_overlay = overlay_total.saturating_sub(anchor).max(1) as f32;
    let tail_ratio = tail_overlay / tail_base;
    let map_point = |x: usize| -> usize {
        if x <= anchor {
            x.min(overlay_total)
        } else {
            let mapped = anchor as f32 + (x.saturating_sub(anchor) as f32) * tail_ratio;
            mapped.round().clamp(0.0, overlay_total as f32) as usize
        }
    };
    let ranges_base = base_ranges_for_bins(start, visible_len, bins);
    let mut ranges_overlay = Vec::with_capacity(ranges_base.len());
    for (i0, i1) in ranges_base {
        let mut o0 = map_point(i0);
        let mut o1 = map_point(i1);
        if o1 <= o0 {
            o1 = o0.saturating_add(1);
        }
        o0 = o0.min(overlay_total);
        o1 = o1.min(overlay_total);
        if o1 <= o0 {
            o1 = (o0 + 1).min(overlay_total.max(o0 + 1));
        }
        ranges_overlay.push((o0, o1));
    }
    minmax_over_ranges(overlay_samples, &ranges_overlay)
}

/// Given an overlay-visible window [startb, startb+over_vis) and total `bins` columns,
/// return the pixel-column range [p0,p1) which corresponds to an overlay segment [seg_start, seg_end).
/// Returns None if the intersection is empty.
pub fn overlay_px_range_for_segment(
    startb: usize,
    over_vis: usize,
    bins: usize,
    seg_start: usize,
    seg_end: usize,
) -> Option<(usize, usize)> {
    if over_vis == 0 || bins == 0 {
        return None;
    }
    let s = seg_start.max(startb);
    let e = seg_end.min(startb + over_vis);
    if e <= s {
        return None;
    }
    let rel0 = (s - startb) as f32 / over_vis as f32;
    let rel1 = (e - startb) as f32 / over_vis as f32;
    let mut p0 = (rel0 * bins as f32).floor() as usize;
    let mut p1 = (rel1 * bins as f32).ceil() as usize;
    if p1 <= p0 {
        p1 = (p0 + 1).min(bins);
    }
    p0 = p0.min(bins);
    p1 = p1.min(bins);
    if p1 <= p0 {
        return None;
    }
    Some((p0, p1))
}

/// Draw bins into an arbitrary horizontal span within the lane rect.
/// The span is given as a Rect with the same vertical extent as the lane.
#[allow(dead_code)]
pub fn draw_bins_in_rect(
    painter: &Painter,
    span_rect: Rect,
    bins_values: &[(f32, f32)],
    pixels_per_point: f32,
    scale: f32,
    vertical_zoom: f32,
    vertical_view_center: f32,
    color: Color32,
    stroke: f32,
) {
    let columns = compute_waveform_device_columns(span_rect, pixels_per_point);
    draw_bins_locked(
        painter,
        span_rect,
        &columns,
        bins_values,
        scale,
        vertical_zoom,
        vertical_view_center,
        color,
        stroke,
    );
}

#[cfg(test)]
mod tests {
    use super::{compute_waveform_device_columns, WaveformDeviceColumns};
    use egui::{pos2, vec2, Rect};

    fn assert_monotonic_columns(columns: WaveformDeviceColumns, rect: Rect, ppp: f32) {
        assert!(columns.column_count() >= 1);
        let mut prev_device_px = i32::MIN;
        for idx in 0..columns.column_count() {
            let device_px = columns.device_pixel_for_column(idx);
            let x = columns.x_center_pt(idx);
            assert!(x.is_finite(), "column x must stay finite");
            assert!(
                device_px > prev_device_px,
                "device pixel must strictly increase: idx={idx} prev={prev_device_px} cur={device_px}"
            );
            prev_device_px = device_px;
        }
        let first = columns.x_center_pt(0);
        let last = columns.x_center_pt(columns.column_count() - 1);
        let pad = 1.0 / ppp.max(1.0);
        assert!(first >= rect.left() - pad);
        assert!(last <= rect.right() + pad);
    }

    #[test]
    fn waveform_device_columns_are_strictly_increasing_without_duplicates() {
        for (left, width, ppp) in [
            (0.0, 822.0, 1.0),
            (12.25, 822.0, 1.25),
            (7.75, 913.4, 1.5),
            (31.2, 1101.75, 2.0),
        ] {
            let rect = Rect::from_min_size(pos2(left, 20.0), vec2(width, 80.0));
            let columns = compute_waveform_device_columns(rect, ppp);
            assert_monotonic_columns(columns, rect, ppp);
        }
    }

    #[test]
    fn waveform_device_columns_follow_rounded_device_width() {
        let rect = Rect::from_min_size(pos2(10.3, 0.0), vec2(822.4, 50.0));
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            let columns = compute_waveform_device_columns(rect, ppp);
            let expected = ((rect.width() * ppp).round()).max(1.0) as usize;
            assert_eq!(columns.column_count(), expected);
        }
    }
}
