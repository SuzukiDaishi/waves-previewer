//! Breakpoint curves on the waveform canvas: the Gain and Pitch Shift
//! curves and the EQ / Compressor / Limiter / Clipper automation lanes. One editor and
//! one painter, so every curve answers the pointer the same way --
//! double-click empty space to add a point, double-click a point to remove
//! it, drag a point (kept between its neighbours); a plain click still
//! seeks.

use egui::{Color32, FontId, Pos2, Stroke};

/// How close the pointer must be to grab a point.
pub(in crate::app) const POINT_HIT_RADIUS: f32 = 8.0;

/// How a curve's points map onto the canvas.
pub(in crate::app) struct CurveAxis<'a> {
    /// A value to its height on screen.
    pub value_to_y: &'a dyn Fn(f32) -> f32,
    /// A height on screen to a value, kept in the curve's range.
    pub y_to_value: &'a dyn Fn(f32) -> f32,
    /// A sample to its x on screen.
    pub sample_to_x: &'a dyn Fn(usize) -> f32,
    /// An x on screen to a sample (kept on the waveform).
    pub x_to_sample: &'a dyn Fn(f32) -> usize,
}

impl CurveAxis<'_> {
    pub fn point_pos(&self, point: (usize, f32)) -> Pos2 {
        Pos2::new((self.sample_to_x)(point.0), (self.value_to_y)(point.1))
    }

    /// The point under `pos`, the nearest when several are.
    pub fn nearest(&self, points: &[(usize, f32)], pos: Pos2) -> Option<usize> {
        points
            .iter()
            .enumerate()
            .map(|(idx, p)| (idx, self.point_pos(*p).distance(pos)))
            .filter(|(_, d)| *d <= POINT_HIT_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(idx, _)| idx)
    }
}

/// What a frame of editing did.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::app) struct CurveEdit {
    /// A point was added or removed, or a drag ended: the curve is new.
    pub changed: bool,
    /// The curve has the pointer (a double click, a grabbed point): seeking
    /// and selecting stand aside this frame.
    pub owns_pointer: bool,
}

/// Edit `points` with this frame's pointer on the canvas response `resp`.
/// `drag` is the index of the grabbed point, kept across frames.
pub(in crate::app) fn edit_breakpoint_curve(
    ui: &egui::Ui,
    resp: &egui::Response,
    points: &mut Vec<(usize, f32)>,
    drag: &mut Option<usize>,
    axis: &CurveAxis<'_>,
) -> CurveEdit {
    let mut edit = CurveEdit::default();
    let primary = egui::PointerButton::Primary;
    // A canvas that also senses drags cannot rely on egui's own double
    // click; two clicks in place count too (`helpers::note_repeated_click`).
    let repeated = resp.clicked_by(primary)
        && resp.interact_pointer_pos().is_some_and(|pos| {
            let id = resp.id.with("curve_last_click");
            let mut last: Option<(std::time::Instant, Pos2)> =
                ui.data_mut(|d| d.get_temp(id)).flatten();
            let repeat = crate::app::helpers::note_repeated_click(&mut last, pos);
            ui.data_mut(|d| d.insert_temp(id, last));
            repeat
        });
    let double_clicked = resp.double_clicked_by(primary) || repeated;
    if double_clicked {
        edit.owns_pointer = true;
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(idx) = axis.nearest(points, pos) {
                points.remove(idx);
                *drag = None;
            } else {
                let sample = (axis.x_to_sample)(pos.x);
                let value = (axis.y_to_value)(pos.y);
                let at = points
                    .iter()
                    .position(|p| p.0 > sample)
                    .unwrap_or(points.len());
                points.insert(at, (sample, value));
            }
            edit.changed = true;
        }
    }
    // A drag may grab a point; dragging empty space stays a selection.
    if !double_clicked && resp.drag_started_by(primary) && drag.is_none() {
        if let Some(pos) = ui
            .input(|i| i.pointer.press_origin())
            .or_else(|| resp.interact_pointer_pos())
        {
            *drag = axis.nearest(points, pos);
        }
    }
    if drag.is_some() {
        edit.owns_pointer = true;
    }
    if resp.dragged_by(primary) {
        if let (Some(idx), Some(pos)) = (*drag, resp.interact_pointer_pos()) {
            if idx < points.len() {
                let mut sample = (axis.x_to_sample)(pos.x);
                if idx > 0 {
                    sample = sample.max(points[idx - 1].0.saturating_add(1));
                }
                if idx + 1 < points.len() {
                    sample = sample.min(points[idx + 1].0.saturating_sub(1));
                }
                points[idx] = (sample, (axis.y_to_value)(pos.y));
            }
        }
    }
    // Any release (click or drag end) drops the grab: the curve is settled.
    if ui.input(|i| i.pointer.primary_released()) && drag.take().is_some() {
        edit.changed = true;
    }
    edit
}

/// How a curve is drawn.
pub(in crate::app) struct CurveLook<'a> {
    pub color: Color32,
    pub width: f32,
    /// Draw the points (and their labels when hovered or grabbed): the
    /// curve being edited. Other lanes are drawn as a line only.
    pub editable: bool,
    /// A point's value as text.
    pub label: &'a dyn Fn(f32) -> String,
    pub font: FontId,
}

/// Draw `points` across the view: `vis` is the sample span on screen and
/// `x_span` its left and right edges; `value_at` gives the curve's value at
/// a sample (for the line's ends at the edges of the view).
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn draw_breakpoint_curve(
    painter: &egui::Painter,
    axis: &CurveAxis<'_>,
    points: &[(usize, f32)],
    value_at: &dyn Fn(usize) -> f32,
    vis: (usize, usize),
    x_span: (f32, f32),
    look: &CurveLook<'_>,
    drag: Option<usize>,
    hover: Option<Pos2>,
) {
    let (vis_start, vis_end) = vis;
    let mut line = vec![Pos2::new(x_span.0, (axis.value_to_y)(value_at(vis_start)))];
    for p in points {
        if p.0 > vis_start && p.0 < vis_end {
            line.push(axis.point_pos(*p));
        }
    }
    line.push(Pos2::new(x_span.1, (axis.value_to_y)(value_at(vis_end))));
    painter.add(egui::Shape::line(line, Stroke::new(look.width, look.color)));
    if !look.editable {
        return;
    }
    let outline = Color32::from_rgb(30, 24, 18);
    for (idx, p) in points.iter().enumerate() {
        if p.0 < vis_start || p.0 > vis_end {
            continue;
        }
        let at = axis.point_pos(*p);
        let grabbed = drag == Some(idx);
        let hovered = hover.is_some_and(|h| h.distance(at) <= POINT_HIT_RADIUS);
        let r = if grabbed || hovered { 5.0 } else { 3.5 };
        painter.circle_filled(at, r, look.color);
        painter.circle_stroke(at, r, Stroke::new(1.0, outline));
        if grabbed || hovered {
            painter.text(
                at + egui::vec2(8.0, -8.0),
                egui::Align2::LEFT_BOTTOM,
                (look.label)(p.1),
                look.font.clone(),
                look.color,
            );
        }
    }
}
