//! The pan knob and its words, shared by every panner in the app: the
//! Multi Edits track header and the editor's Panner tool. The rules the
//! knobs drive are `crate::panning`.

use egui::{Align2, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::app::multi_edit::LaneParam;
use crate::panning::PAN_TURN_DEG;

/// How far a knob turns per point dragged (right or up turns right): a full
/// side in a hundred points; Shift is ten times finer.
pub(crate) const PAN_DRAG_PER_PX: f32 = 0.01;

/// How far a balance knob's pointer swings for a full side. A turning
/// knob's pointer shows the turn itself (`PAN_TURN_DEG`), so it points where
/// the front of the sound now faces.
pub(crate) const PAN_KNOB_BALANCE_SWEEP_DEG: f32 = 135.0;

/// Why a Multi Edits track's pan does nothing.
pub(crate) const NO_PAN_HOVER: &str =
    "Nothing to pan: a mono track pans only on an output of three or \
                                       more speakers, where the pan turns it round the listener -- \
                                       and a track on the LFE is never turned.";

/// How far the pointer of a knob swings for +-1: the turn itself when the
/// pan turns, a balance knob's travel otherwise.
pub(crate) fn pan_sweep(rotates: bool) -> f32 {
    if rotates {
        PAN_TURN_DEG
    } else {
        PAN_KNOB_BALANCE_SWEEP_DEG
    }
}

/// A pan (-1..1) as the knob and the Pan lane's header say it: L/R and a
/// percentage when it balances, L/R and the angle of the turn when it turns.
pub(crate) fn pan_text(value: f32, rotates: bool) -> String {
    if !rotates {
        return LaneParam::Pan.format_value(value);
    }
    let deg = (value * PAN_TURN_DEG).round() as i32;
    match deg {
        0 => "0\u{b0}".to_string(),
        d if d < 0 => format!("L{}\u{b0}", -d),
        d => format!("R{d}\u{b0}"),
    }
}

/// A knob in `rect` for a value in -1..1 whose pointer swings `sweep_deg`
/// for a full side. Returns its response and the new value while it is
/// dragged; the caller handles the double-click (back to the centre).
pub(crate) fn pan_knob(
    ui: &mut egui::Ui,
    rect: Rect,
    id: egui::Id,
    value: f32,
    enabled: bool,
    sweep_deg: f32,
) -> (egui::Response, Option<f32>) {
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(rect, id, sense);
    let mut changed = None;
    if resp.dragged() {
        let delta = resp.drag_delta();
        let fine = if ui.input(|i| i.modifiers.shift) {
            0.1
        } else {
            1.0
        };
        let step = (delta.x - delta.y) * PAN_DRAG_PER_PX * fine;
        if step != 0.0 {
            changed = Some((value + step).clamp(-1.0, 1.0));
        }
    }
    if resp.hovered() && enabled {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    let shown = changed.unwrap_or(value);
    let visuals = ui.visuals();
    let fade = if enabled { 1.0 } else { 0.35 };
    let hot = enabled && (resp.hovered() || resp.dragged());
    let widget = if hot {
        &visuals.widgets.hovered
    } else {
        &visuals.widgets.inactive
    };
    let centre = rect.center();
    let radius = rect.width().min(rect.height()) * 0.5 - 1.0;
    let painter = ui.painter_at(rect.expand(1.0));
    painter.circle_filled(centre, radius, widget.weak_bg_fill.gamma_multiply(fade));
    // Clockwise from the top, as a turn is seen from above.
    let at = |deg: f32, r: f32| {
        let a = deg.to_radians();
        centre + Vec2::new(a.sin(), -a.cos()) * r
    };
    let arc = |from: f32, to: f32, stroke: Stroke| {
        let steps = ((to - from).abs() / 10.0).ceil().max(2.0) as usize;
        let points: Vec<Pos2> = (0..=steps)
            .map(|i| at(from + (to - from) * i as f32 / steps as f32, radius - 1.5))
            .collect();
        painter.add(egui::Shape::line(points, stroke));
    };
    // The knob's travel, then how far it is turned from the centre.
    arc(
        -sweep_deg,
        sweep_deg,
        Stroke::new(
            2.0_f32,
            visuals
                .widgets
                .noninteractive
                .bg_stroke
                .color
                .gamma_multiply(fade),
        ),
    );
    let angle = shown * sweep_deg;
    if angle.abs() > 0.5 {
        arc(
            0.0,
            angle,
            Stroke::new(2.0_f32, visuals.selection.bg_fill.gamma_multiply(fade)),
        );
    }
    painter.line_segment(
        [at(angle, radius * 0.2), at(angle, radius - 1.0)],
        Stroke::new(2.0_f32, widget.fg_stroke.color.gamma_multiply(fade)),
    );
    (resp, changed)
}

/// What one labelled knob did this frame.
#[derive(Default)]
pub(crate) struct KnobEdit {
    /// The new value, when it moved.
    pub value: Option<f32>,
    /// A gesture ended (released, typed in, reset): the moment to redo
    /// anything too slow to follow the drag.
    pub finished: bool,
}

/// A row of a big knob for `value` within `range` (whose pointer shows
/// `value * scale` degrees of a `sweep_deg` swing for the knob's +-1), its
/// value to drag or type with `suffix`, and `label`. Double-click resets to
/// zero.
#[allow(clippy::too_many_arguments)]
pub(crate) fn labelled_knob(
    ui: &mut egui::Ui,
    id: egui::Id,
    label: &str,
    hover: &str,
    value: f32,
    range: std::ops::RangeInclusive<f32>,
    full_scale: f32,
    sweep_deg: f32,
    suffix: &str,
    decimals: usize,
    enabled: bool,
) -> KnobEdit {
    const KNOB: f32 = 34.0;
    let mut edit = KnobEdit::default();
    let clamp = |v: f32| v.clamp(*range.start(), *range.end());
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(KNOB), Sense::hover());
        let (resp, turned) = pan_knob(ui, rect, id, value / full_scale, enabled, sweep_deg);
        let resp = resp.on_hover_text(hover);
        if resp.double_clicked() {
            edit.value = Some(0.0);
            edit.finished = true;
        } else if let Some(turned) = turned {
            edit.value = Some(clamp(turned * full_scale));
        }
        if resp.drag_stopped() {
            edit.finished = true;
        }
        ui.vertical(|ui| {
            ui.label(RichText::new(label).strong());
            let mut typed = value;
            let field = ui.add_enabled(
                enabled,
                egui::DragValue::new(&mut typed)
                    .range(range.clone())
                    .speed(f64::from(full_scale) * 0.005)
                    .fixed_decimals(decimals)
                    .suffix(suffix),
            );
            if field.changed() {
                edit.value = Some(clamp(typed));
            }
            if field.drag_stopped() || field.lost_focus() {
                edit.finished = true;
            }
        });
    });
    edit
}

/// The room as the Panner sees it: from above (ahead is up) and from the
/// right side (ahead is to the right). Faint dots are the speakers the pan
/// lays sound onto; labelled dots are where the settings send each channel.
/// Vectors are x right, y ahead, z up, unit length.
pub(crate) fn pan_field(ui: &mut egui::Ui, speakers: &[[f32; 3]], sources: &[(String, [f32; 3])]) {
    const CAPTION_H: f32 = 12.0;
    const GAP: f32 = 8.0;
    let width = ui.available_width().clamp(160.0, 320.0);
    let height = (width * 0.5).min(150.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    let half = (rect.width() - GAP) * 0.5;
    let views = [
        (
            "Top",
            Rect::from_min_size(rect.min, Vec2::new(half, rect.height())),
            true,
        ),
        (
            "Side",
            Rect::from_min_size(
                rect.min + Vec2::new(half + GAP, 0.0),
                Vec2::new(half, rect.height()),
            ),
            false,
        ),
    ];
    for (name, area, from_above) in views {
        let room = Rect::from_min_max(Pos2::new(area.left(), area.top() + CAPTION_H), area.max);
        let radius = (room.width().min(room.height()) * 0.5 - 6.0).max(8.0);
        let centre = room.center();
        painter.text(
            area.left_top(),
            Align2::LEFT_TOP,
            name,
            FontId::proportional(10.0),
            visuals.weak_text_color(),
        );
        painter.circle_stroke(
            centre,
            radius,
            Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color),
        );
        painter.circle_filled(centre, 2.0, visuals.weak_text_color());
        let to_screen = |v: [f32; 3]| {
            if from_above {
                centre + Vec2::new(v[0], -v[1]) * radius
            } else {
                centre + Vec2::new(v[1], -v[2]) * radius
            }
        };
        for v in speakers {
            painter.circle_filled(
                to_screen(*v),
                3.0,
                visuals.weak_text_color().gamma_multiply(0.6),
            );
        }
        for (label, v) in sources {
            let at = to_screen(*v);
            painter.circle_filled(at, 4.0, visuals.selection.bg_fill);
            painter.text(
                at + Vec2::new(5.0, -4.0),
                Align2::LEFT_BOTTOM,
                label,
                FontId::proportional(10.0),
                visuals.text_color(),
            );
        }
    }
}
