use egui::{Color32, Painter, Rect};

use super::binning::{pos_step_ranges, minmax_over_ranges};

/// Map the visible original window [start, start+visible_len) into overlay domain of length `overlay_total`.
/// Returns (startb, endb, over_vis).
pub fn map_visible_overlay(start: usize, visible_len: usize, orig_total: usize, overlay_total: usize) -> (usize, usize, usize) {
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
    for r in &mut ranges { r.0 += start; r.1 += start; }
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
    if visible_len == 0 || over_vis == 0 { return Vec::new(); }
    let mut out = Vec::with_capacity(ranges_base.len());
    for &(i0, i1) in ranges_base {
        let o0 = startb + (((i0.saturating_sub(start)) as f32 * over_vis as f32 / visible_len as f32).round() as usize);
        let mut o1 = startb + (((i1.saturating_sub(start)) as f32 * over_vis as f32 / visible_len as f32).round() as usize);
        if o1 <= o0 { o1 = o0 + 1; }
        out.push((o0, o1));
    }
    out
}

/// Draw bins (min/max) locked to px columns over lane_rect width.
pub fn draw_bins_locked(
    painter: &Painter,
    lane_rect: Rect,
    wave_w: f32,
    bins_values: &[(f32, f32)],
    scale: f32,
    color: Color32,
    stroke: f32,
) {
    let n = bins_values.len().max(1) as f32;
    for (idx, &(mn0, mx0)) in bins_values.iter().enumerate() {
        let mn = (mn0 * scale).clamp(-1.0, 1.0);
        let mx = (mx0 * scale).clamp(-1.0, 1.0);
        let x = lane_rect.left() + (idx as f32 / n) * wave_w;
        let y0 = lane_rect.center().y - mx * (lane_rect.height() * 0.48);
        let y1 = lane_rect.center().y - mn * (lane_rect.height() * 0.48);
        painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(stroke, color));
    }
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
    if over_vis == 0 || bins == 0 { return None; }
    let s = seg_start.max(startb);
    let e = seg_end.min(startb + over_vis);
    if e <= s { return None; }
    let rel0 = (s - startb) as f32 / over_vis as f32;
    let rel1 = (e - startb) as f32 / over_vis as f32;
    let mut p0 = (rel0 * bins as f32).floor() as usize;
    let mut p1 = (rel1 * bins as f32).ceil() as usize;
    if p1 <= p0 { p1 = (p0 + 1).min(bins); }
    p0 = p0.min(bins);
    p1 = p1.min(bins);
    if p1 <= p0 { return None; }
    Some((p0, p1))
}

/// Draw bins into an arbitrary horizontal span within the lane rect.
/// The span is given as a Rect with the same vertical extent as the lane.
pub fn draw_bins_in_rect(
    painter: &Painter,
    span_rect: Rect,
    bins_values: &[(f32, f32)],
    scale: f32,
    color: Color32,
    stroke: f32,
) {
    let n = bins_values.len().max(1) as f32;
    let wave_w = span_rect.width().max(1.0);
    for (idx, &(mn0, mx0)) in bins_values.iter().enumerate() {
        let mn = (mn0 * scale).clamp(-1.0, 1.0);
        let mx = (mx0 * scale).clamp(-1.0, 1.0);
        let x = span_rect.left() + (idx as f32 / n) * wave_w;
        let y0 = span_rect.center().y - mx * (span_rect.height() * 0.48);
        let y1 = span_rect.center().y - mn * (span_rect.height() * 0.48);
        painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(stroke, color));
    }
}
