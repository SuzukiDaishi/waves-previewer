//! Patchbay widget for [`ToolKind::ChannelRouting`].
//!
//! A degenerate two-node graph: the file's channels on the left, the routed
//! result on the right, cables in between. Visual language (pin circles, bezier
//! cables, blue/amber strokes) deliberately matches the Effect Graph canvas in
//! `super::effect_graph` so the two read as the same kind of thing.

use egui::{Color32, Sense, Stroke};

use crate::app::types::{ChannelRoutingDraft, CHANNEL_ROUTING_MAX_OUT};
use crate::app::WavesPreviewer;

const PIN_R: f32 = 6.0;
const PIN_HALO_R: f32 = 9.0;
/// Grab radius around a pin. Far larger than the 9px halo it draws — the pin is
/// the affordance, this is the target. Pins are picked by nearest-centre, so
/// neighbouring radii may overlap without becoming ambiguous.
const PIN_HIT_R: f32 = 18.0;
/// Grab radius around a cable, for cutting. Kept under `PIN_HIT_R` and only
/// consulted after pins miss, so a cable ending at a pin never shadows it.
const CABLE_HIT_R: f32 = 11.0;
/// Row pitch. Wide enough that two adjacent `PIN_HIT_R` discs barely meet.
const ROW_H: f32 = 38.0;
const COL_LABEL_W: f32 = 62.0;
/// Horizontal offset of the bezier control points, as in the effect graph.
const CABLE_BOW: f32 = 64.0;

const COLOR_IN_PIN: Color32 = Color32::from_rgb(196, 212, 228);
const COLOR_OUT_PIN: Color32 = Color32::from_rgb(110, 170, 255);
const COLOR_PIN_RING: Color32 = Color32::from_rgb(70, 80, 92);
const COLOR_CABLE: Color32 = Color32::from_rgb(110, 170, 255);
const COLOR_CABLE_LIVE: Color32 = Color32::from_rgb(255, 196, 96);
/// Cable about to be cut.
const COLOR_CABLE_CUT: Color32 = Color32::from_rgb(255, 110, 110);
const COLOR_BG: Color32 = Color32::from_rgb(18, 20, 24);
/// Faint disc showing the active grab radius under the cursor.
const COLOR_HIT_HINT: Color32 = Color32::from_rgb(70, 82, 96);

// Pins are hit-tested before cables, so a cable arriving at a pin must not be
// reachable from further away than the pin itself — otherwise pins would become
// hard to grab wherever a cable already lands on one.
const _: () = assert!(CABLE_HIT_R < PIN_HIT_R);
// Nearest-centre picking resolves overlapping grab discs, but a pin's own
// centre must still fall outside its neighbour's disc.
const _: () = assert!(ROW_H > PIN_HIT_R);

/// What the pointer is over. Pins win over cables so the ends of a cable stay
/// usable for making new connections.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RoutingHit {
    InPin(usize),
    OutPin(usize),
    /// `(output, input)` — one specific cable.
    Cable(usize, usize),
}

fn dist_point_segment(p: egui::Pos2, a: egui::Pos2, b: egui::Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= f32::EPSILON {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

fn dist_to_polyline(p: egui::Pos2, pts: &[egui::Pos2]) -> f32 {
    pts.windows(2)
        .map(|w| dist_point_segment(p, w[0], w[1]))
        .fold(f32::INFINITY, f32::min)
}

fn cubic_point(
    p0: egui::Pos2,
    p1: egui::Pos2,
    p2: egui::Pos2,
    p3: egui::Pos2,
    t: f32,
) -> egui::Pos2 {
    let u = 1.0 - t;
    let (uu, tt) = (u * u, t * t);
    let (uuu, ttt) = (uu * u, tt * t);
    egui::pos2(
        uuu * p0.x + 3.0 * uu * t * p1.x + 3.0 * u * tt * p2.x + ttt * p3.x,
        uuu * p0.y + 3.0 * uu * t * p1.y + 3.0 * u * tt * p2.y + ttt * p3.y,
    )
}

fn cable_points(from: egui::Pos2, to: egui::Pos2) -> Vec<egui::Pos2> {
    let c1 = egui::pos2(from.x + CABLE_BOW, from.y);
    let c2 = egui::pos2(to.x - CABLE_BOW, to.y);
    (0..24)
        .map(|i| cubic_point(from, c1, c2, to, i as f32 / 23.0))
        .collect()
}

/// Key under which the widget publishes its pin positions each frame, so tests
/// can drive real pointer events at them.
pub(crate) fn pin_geometry_id() -> egui::Id {
    egui::Id::new("channel_routing_pin_geometry")
}

/// `L`/`R` only for exactly-stereo, otherwise `Ch N` — the same rule the editor
/// toolbar and meter strip already use.
pub(super) fn channel_label(count: usize, idx: usize) -> String {
    match (count, idx) {
        (2, 0) => "L".to_string(),
        (2, 1) => "R".to_string(),
        _ => format!("Ch {}", idx + 1),
    }
}

impl WavesPreviewer {
    /// Draws the matrix and mutates `draft` in place. Returns true when the
    /// user asked to apply.
    pub(super) fn ui_channel_routing_patchbay(
        ui: &mut egui::Ui,
        draft: &mut ChannelRoutingDraft,
        in_count: usize,
    ) -> bool {
        let in_count = in_count.max(1);
        draft.reseed_if_stale(in_count);

        ui.horizontal(|ui| {
            ui.label("Output channels");
            let mut out = draft.out_count;
            if ui
                .add(
                    egui::DragValue::new(&mut out)
                        .range(1..=CHANNEL_ROUTING_MAX_OUT)
                        .speed(0.1),
                )
                .changed()
            {
                draft.set_out_count(out);
            }
            if ui
                .button("Identity")
                .on_hover_text("Straight-through wiring, one input per output")
                .clicked()
            {
                *draft = ChannelRoutingDraft::identity(in_count);
            }
            if in_count == 2
                && ui
                    .button("Swap L/R")
                    .on_hover_text("Exchange the two channels")
                    .clicked()
            {
                draft.set_out_count(2);
                draft.sources = vec![vec![1], vec![0]];
            }
            if in_count == 1
                && ui
                    .button("Mono → Stereo")
                    .on_hover_text("Duplicate the single channel into two")
                    .clicked()
            {
                draft.set_out_count(2);
                draft.sources = vec![vec![0], vec![0]];
            }
            if in_count >= 2
                && ui
                    .button("→ Mono")
                    .on_hover_text("Mix every channel down into one")
                    .clicked()
            {
                draft.set_out_count(1);
                draft.sources = vec![(0..in_count).collect()];
            }
        });
        ui.add_space(4.0);

        let rows = in_count.max(draft.out_count);
        // Inset by the full grab radius, not just the drawn halo: hits are
        // ignored outside the canvas, so a pin flush against the edge would
        // lose most of its target area.
        let height = (rows as f32 * ROW_H + PIN_HIT_R * 2.0).max(72.0);
        let width = ui.available_width().max(220.0);
        // One response for the whole canvas rather than per-pin `interact`
        // rects: hit priority (pin over cable, nearest wins) has to be decided
        // by distance, which egui's rect stacking cannot express.
        let (canvas, canvas_resp) =
            ui.allocate_exact_size(egui::vec2(width, height), Sense::click_and_drag());
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 6.0, COLOR_BG);

        let pin_x_in = canvas.left() + COL_LABEL_W;
        let pin_x_out = canvas.right() - COL_LABEL_W;
        let pin_y = |idx: usize, count: usize| -> f32 {
            let span = canvas.height() - PIN_HIT_R * 2.0;
            if count <= 1 {
                canvas.center().y
            } else {
                canvas.top() + PIN_HIT_R + span * (idx as f32 / (count - 1) as f32)
            }
        };
        let in_pos: Vec<egui::Pos2> = (0..in_count)
            .map(|i| egui::pos2(pin_x_in, pin_y(i, in_count)))
            .collect();
        let out_pos: Vec<egui::Pos2> = (0..draft.out_count)
            .map(|i| egui::pos2(pin_x_out, pin_y(i, draft.out_count)))
            .collect();

        ui.ctx().data_mut(|d| {
            d.insert_temp(pin_geometry_id(), (in_pos.clone(), out_pos.clone()));
        });

        // Cable geometry is needed for both hit-testing and drawing.
        let mut cables: Vec<(usize, usize, Vec<egui::Pos2>)> = Vec::new();
        for (out_idx, srcs) in draft.sources.iter().enumerate() {
            let Some(to) = out_pos.get(out_idx) else {
                continue;
            };
            for src in srcs {
                let Some(from) = in_pos.get(*src) else {
                    continue;
                };
                cables.push((out_idx, *src, cable_points(*from, *to)));
            }
        }

        // `pointer_latest_pos` rather than `hover_pos`: a drag that leaves the
        // canvas must still track, and the rubber band must follow.
        let pointer = ui.ctx().pointer_latest_pos();
        let pointer_inside = pointer.filter(|p| canvas.contains(*p));
        let hit = pointer_inside.and_then(|p| {
            let nearest_pin = |positions: &[egui::Pos2]| -> Option<(usize, f32)> {
                positions
                    .iter()
                    .enumerate()
                    .map(|(i, pos)| (i, (p - *pos).length()))
                    .filter(|(_, d)| *d <= PIN_HIT_R)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
            };
            let in_hit = nearest_pin(&in_pos).map(|(i, d)| (RoutingHit::InPin(i), d));
            let out_hit = nearest_pin(&out_pos).map(|(i, d)| (RoutingHit::OutPin(i), d));
            if let Some((kind, _)) = match (in_hit, out_hit) {
                (Some(a), Some(b)) => Some(if a.1 <= b.1 { a } else { b }),
                (a, b) => a.or(b),
            } {
                return Some(kind);
            }
            cables
                .iter()
                .map(|(o, s, pts)| (*o, *s, dist_to_polyline(p, pts)))
                .filter(|(_, _, d)| *d <= CABLE_HIT_R)
                .min_by(|a, b| a.2.total_cmp(&b.2))
                .map(|(o, s, _)| RoutingHit::Cable(o, s))
        });

        // Drag-to-connect: press on an input pin, release over an output pin.
        if canvas_resp.drag_started() {
            if let Some(RoutingHit::InPin(i)) = hit {
                draft.connecting_from = Some(i);
            }
        }
        if canvas_resp.drag_stopped() {
            if let (Some(src), Some(RoutingHit::OutPin(o))) = (draft.connecting_from, hit) {
                draft.toggle_link(src, o);
            }
            draft.connecting_from = None;
        }
        // Click-to-connect is kept alongside the drag: click a pin, then the
        // other pin. A plain click never raises drag_started, so the two
        // gestures can't fire together.
        if canvas_resp.clicked() {
            match hit {
                Some(RoutingHit::InPin(i)) => {
                    // Re-clicking the pin you started from cancels.
                    draft.connecting_from = if draft.connecting_from == Some(i) {
                        None
                    } else {
                        Some(i)
                    };
                }
                Some(RoutingHit::OutPin(o)) => {
                    if let Some(src) = draft.connecting_from {
                        draft.toggle_link(src, o);
                        draft.connecting_from = None;
                    }
                }
                Some(RoutingHit::Cable(o, s)) => {
                    draft.toggle_link(s, o);
                    draft.connecting_from = None;
                }
                None => draft.connecting_from = None,
            }
        }
        // Right-click a pin to drop every cable on it at once.
        if canvas_resp.clicked_by(egui::PointerButton::Secondary) {
            match hit {
                Some(RoutingHit::InPin(i)) => {
                    for srcs in draft.sources.iter_mut() {
                        srcs.retain(|s| *s != i);
                    }
                }
                Some(RoutingHit::OutPin(o)) => {
                    if let Some(srcs) = draft.sources.get_mut(o) {
                        srcs.clear();
                    }
                }
                _ => {}
            }
            draft.connecting_from = None;
        }

        let hovered_cable = match hit {
            Some(RoutingHit::Cable(o, s)) => Some((o, s)),
            _ => None,
        };

        // Faint disc under the cursor so the grab radius is discoverable.
        if let Some(p) = pointer_inside {
            let (radius, color) = match hit {
                Some(RoutingHit::Cable(..)) => (CABLE_HIT_R, COLOR_CABLE_CUT),
                Some(_) => (PIN_HIT_R, COLOR_HIT_HINT),
                None => (0.0, COLOR_HIT_HINT),
            };
            if radius > 0.0 {
                painter.circle_filled(p, radius, color.gamma_multiply(0.18));
            }
        }

        // Cables under the pins so pin ends stay clickable.
        for (o, s, points) in &cables {
            let cut = hovered_cable == Some((*o, *s));
            painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(if cut { 6.0 } else { 4.0 }, Color32::from_black_alpha(120)),
            ));
            painter.add(egui::Shape::line(
                points.clone(),
                Stroke::new(
                    if cut { 3.0 } else { 2.0 },
                    if cut { COLOR_CABLE_CUT } else { COLOR_CABLE },
                ),
            ));
        }
        // Scissors marker on the cable about to be cut.
        if let Some(p) = pointer_inside {
            if hovered_cable.is_some() {
                painter.text(
                    egui::pos2(p.x + 12.0, p.y - 12.0),
                    egui::Align2::LEFT_CENTER,
                    "\u{2702}",
                    egui::TextStyle::Body.resolve(ui.style()),
                    COLOR_CABLE_CUT,
                );
            }
        }
        // Cable being dragged.
        if let Some(from) = draft.connecting_from.and_then(|src| in_pos.get(src)) {
            // Off-window pointer: pin the loose end to the source so the cable
            // doesn't vanish mid-drag.
            let cursor = pointer.unwrap_or(*from);
            painter.add(egui::Shape::line(
                cable_points(*from, cursor),
                Stroke::new(2.5, COLOR_CABLE_LIVE),
            ));
        }

        let font = egui::TextStyle::Body.resolve(ui.style());
        let text_color = ui.visuals().text_color();

        for (i, pos) in in_pos.iter().enumerate() {
            let active = draft.connecting_from == Some(i);
            let hovered = hit == Some(RoutingHit::InPin(i));
            painter.circle_filled(*pos, PIN_HALO_R, COLOR_BG);
            painter.circle_filled(
                *pos,
                if hovered { PIN_R + 1.5 } else { PIN_R },
                if active {
                    COLOR_CABLE_LIVE
                } else {
                    COLOR_IN_PIN
                },
            );
            painter.circle_stroke(
                *pos,
                PIN_HALO_R,
                Stroke::new(
                    if hovered { 2.0 } else { 1.0 },
                    if hovered {
                        COLOR_CABLE_LIVE
                    } else {
                        COLOR_PIN_RING
                    },
                ),
            );
            painter.text(
                egui::pos2(pos.x - PIN_HALO_R - 5.0, pos.y),
                egui::Align2::RIGHT_CENTER,
                channel_label(in_count, i),
                font.clone(),
                text_color,
            );
        }

        for (o, pos) in out_pos.iter().enumerate() {
            let hovered = hit == Some(RoutingHit::OutPin(o));
            painter.circle_filled(*pos, PIN_HALO_R, COLOR_BG);
            painter.circle_filled(
                *pos,
                if hovered { PIN_R + 1.5 } else { PIN_R },
                COLOR_OUT_PIN,
            );
            painter.circle_stroke(
                *pos,
                PIN_HALO_R,
                Stroke::new(
                    if hovered { 2.0 } else { 1.0 },
                    if hovered {
                        COLOR_CABLE_LIVE
                    } else {
                        COLOR_PIN_RING
                    },
                ),
            );
            let src_count = draft.sources.get(o).map(|s| s.len()).unwrap_or(0);
            let label = if src_count > 1 {
                format!("{} (mix {})", channel_label(draft.out_count, o), src_count)
            } else if src_count == 0 {
                format!("{} (silent)", channel_label(draft.out_count, o))
            } else {
                channel_label(draft.out_count, o)
            };
            painter.text(
                egui::pos2(pos.x + PIN_HALO_R + 5.0, pos.y),
                egui::Align2::LEFT_CENTER,
                label,
                font.clone(),
                text_color,
            );
        }

        if hovered_cable.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        } else if matches!(hit, Some(RoutingHit::InPin(_) | RoutingHit::OutPin(_))) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        // The rubber band and the hover highlights both track the pointer.
        if draft.connecting_from.is_some() || hit.is_some() {
            ui.ctx().request_repaint();
        }

        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(
                "Drag from an input pin to an output pin to connect (or click one, then the other). \
                 Click a cable to cut it. Right-click a pin to clear its cables. \
                 Outputs fed by several inputs are averaged.",
            )
            .weak()
            .size(11.0),
        );

        let identity = draft.is_identity();
        let mut apply = false;
        ui.horizontal(|ui| {
            let btn = ui.add_enabled(!identity, egui::Button::new("Apply"));
            if btn.clicked() {
                apply = true;
            }
            if identity {
                ui.label(
                    egui::RichText::new("Routing matches the source; nothing to apply.")
                        .weak()
                        .size(11.0),
                );
            } else {
                ui.label(
                    egui::RichText::new(format!(
                        "{} ch → {} ch, {} link(s)",
                        draft.in_count,
                        draft.out_count,
                        draft.total_links()
                    ))
                    .weak()
                    .size(11.0),
                );
            }
        });
        apply
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn point_segment_distance_handles_the_perpendicular_and_the_ends() {
        let (a, b) = (pos2(0.0, 0.0), pos2(10.0, 0.0));
        assert!((dist_point_segment(pos2(5.0, 3.0), a, b) - 3.0).abs() < 1e-4);
        // Past either end it clamps to the endpoint rather than the infinite line.
        assert!((dist_point_segment(pos2(-4.0, 0.0), a, b) - 4.0).abs() < 1e-4);
        assert!((dist_point_segment(pos2(14.0, 0.0), a, b) - 4.0).abs() < 1e-4);
        // Degenerate segment must not divide by zero.
        assert!((dist_point_segment(pos2(0.0, 2.0), a, a) - 2.0).abs() < 1e-4);
    }

    #[test]
    fn cable_grab_radius_reaches_the_middle_of_a_cable() {
        let cable = cable_points(pos2(0.0, 0.0), pos2(200.0, 80.0));
        let mid = cable[cable.len() / 2];
        assert!(dist_to_polyline(mid, &cable) < 0.5, "on the cable");
        // Just inside the grab radius, and just outside it.
        let near = pos2(mid.x, mid.y + CABLE_HIT_R - 2.0);
        let far = pos2(mid.x, mid.y + CABLE_HIT_R + 6.0);
        assert!(dist_to_polyline(near, &cable) <= CABLE_HIT_R);
        assert!(dist_to_polyline(far, &cable) > CABLE_HIT_R);
    }
}
