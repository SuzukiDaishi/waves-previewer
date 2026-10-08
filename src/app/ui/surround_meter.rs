//! The mini meter's SURROUND view, in place of the stereo vectorscope for a
//! file with three or more speakers besides LFE: the speakers seen from
//! above, each lit by its level; where the sound comes from (the energy
//! vector) with a short trail; how much of it is overhead; and LFE apart.
//! See `docs/MULTICHANNEL_SPEC.md`.

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, Vec2};

use crate::app::render::mini_meter::{energy_vector, height_share};
use crate::app::types::MiniMeterState;
use crate::audio_channels::SpeakerPos;

/// Points of the direction trail kept (about a second at the meter's rate).
const TRAIL_POINTS: usize = 32;
/// The quietest level a speaker dot shows, in dBFS.
const DOT_FLOOR_DB: f32 = -60.0;
/// Height speakers sit on this fraction of the ear-level ring's radius.
const HEIGHT_RING: f32 = 0.55;

/// Whether `layout` gets the SURROUND view: three or more speakers that
/// have a direction.
pub(super) fn wants_surround(layout: &[Option<SpeakerPos>]) -> bool {
    layout.iter().flatten().filter(|pos| !pos.is_lfe()).count() >= 3
}

fn db(v: f32) -> f32 {
    20.0 * v.max(1.0e-6).log10()
}

/// The level colours of the bar meters: green, yellow from -18, red from -6.
fn level_color(db: f32) -> Color32 {
    if db >= -6.0 {
        Color32::from_rgb(240, 100, 100)
    } else if db >= -18.0 {
        Color32::from_rgb(235, 200, 90)
    } else {
        Color32::from_rgb(88, 200, 120)
    }
}

/// Draw the view into `rect` from `channels[..][start..end]`.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_surround(
    painter: &Painter,
    rect: Rect,
    channels: &[Vec<f32>],
    start: usize,
    end: usize,
    layout: &[Option<SpeakerPos>],
    state: &mut MiniMeterState,
    font: &FontId,
    label_col: Color32,
) {
    let guide = Stroke::new(1.0_f32, Color32::from_rgb(34, 39, 48));
    let side_w = 8.0;
    // Room either side for the gauges, and round the ring for the names.
    let area = Rect::from_min_max(
        rect.left_top() + Vec2::new(side_w + 10.0, 12.0),
        rect.right_bottom() - Vec2::new(side_w + 10.0, 4.0),
    );
    let radius = (area.width().min(area.height()) * 0.5 - 12.0).max(10.0);
    let center = area.center();
    painter.circle_stroke(center, radius, guide);
    painter.circle_stroke(
        center,
        radius * HEIGHT_RING,
        Stroke::new(1.0_f32, Color32::from_rgb(28, 32, 40)),
    );
    // The listener, facing up the panel.
    painter.add(egui::Shape::convex_polygon(
        vec![
            center + Vec2::new(0.0, -4.0),
            center + Vec2::new(3.0, 3.0),
            center + Vec2::new(-3.0, 3.0),
        ],
        Color32::from_rgb(70, 78, 92),
        Stroke::NONE,
    ));

    // Each channel's level, and what the direction needs from it.
    let mut field: Vec<(f32, f32, f32)> = Vec::with_capacity(layout.len());
    let mut layers: Vec<(f32, bool)> = Vec::with_capacity(layout.len());
    let mut lfe_db: Option<f32> = None;
    for (c, pos) in layout.iter().enumerate() {
        let (Some(pos), Some(samples)) = (pos, channels.get(c)) else {
            continue;
        };
        let window = &samples[start.min(samples.len())..end.min(samples.len())];
        let energy = window.iter().map(|v| v * v).sum::<f32>() / window.len().max(1) as f32;
        let peak = window.iter().fold(0.0f32, |a, v| a.max(v.abs()));
        let level_db = db(energy.sqrt());
        if pos.is_lfe() {
            lfe_db = Some(lfe_db.map_or(level_db, |db: f32| db.max(level_db)));
            continue;
        }
        let (azimuth, elevation) = pos.direction(layout);
        field.push((energy, azimuth, elevation));
        layers.push((energy, pos.is_height()));

        // Overhead at the centre, the height layer on the inner ring.
        let ring = if elevation >= 60.0 {
            0.0
        } else if pos.is_height() {
            radius * HEIGHT_RING
        } else {
            radius
        };
        let (sin, cos) = azimuth.to_radians().sin_cos();
        let at = center + Vec2::new(sin * ring, -cos * ring);
        let norm = ((level_db - DOT_FLOOR_DB) / -DOT_FLOOR_DB).clamp(0.0, 1.0);
        let base = Color32::from_rgb(54, 60, 72);
        let lit = level_color(level_db);
        let fill = crate::app::helpers::lerp_color(base, lit, norm);
        painter.circle_filled(at, 2.5 + 4.5 * norm, fill);
        if peak >= 0.999 {
            painter.circle_stroke(at, 8.0, Stroke::new(1.5_f32, Color32::from_rgb(240, 100, 100)));
        } else if let Some(hold) = state.peak_hold_db.get(c) {
            let hold_norm = ((hold - DOT_FLOOR_DB) / -DOT_FLOOR_DB).clamp(0.0, 1.0);
            if hold_norm > norm + 0.02 {
                painter.circle_stroke(
                    at,
                    2.5 + 4.5 * hold_norm,
                    Stroke::new(1.0_f32, Color32::from_rgb(255, 196, 72)),
                );
            }
        }
        // Names outside the outer ring, inside the inner one.
        let label_at = if ring >= radius {
            center + Vec2::new(sin, -cos) * (radius + 9.0)
        } else if ring > 0.0 {
            center + Vec2::new(sin, -cos) * (ring - 9.0)
        } else {
            at + Vec2::new(0.0, -9.0)
        };
        painter.text(
            label_at,
            Align2::CENTER_CENTER,
            pos.label(layout),
            font.clone(),
            label_col,
        );
    }

    // Where the sound comes from, with a short trail.
    let vector = energy_vector(&field);
    state.surround_vector = vector;
    match vector {
        Some([x, y, _]) => {
            state.surround_trail.push_back((x, y));
            while state.surround_trail.len() > TRAIL_POINTS {
                state.surround_trail.pop_front();
            }
        }
        None => {
            state.surround_trail.pop_front();
        }
    }
    let trail: Vec<Pos2> = state
        .surround_trail
        .iter()
        .map(|(x, y)| center + Vec2::new(x * radius, -y * radius))
        .collect();
    for (i, pair) in trail.windows(2).enumerate() {
        let alpha = (i + 1) as f32 / trail.len().max(1) as f32;
        painter.line_segment(
            [pair[0], pair[1]],
            Stroke::new(
                1.5_f32,
                Color32::from_rgb(96, 220, 200).gamma_multiply(alpha * 0.8),
            ),
        );
    }
    if let Some([x, y, _]) = vector {
        let at = center + Vec2::new(x * radius, -y * radius);
        painter.circle_filled(at, 4.0, Color32::from_rgb(96, 220, 200));
        painter.circle_stroke(at, 4.0, Stroke::new(1.0_f32, Color32::from_rgb(10, 40, 36)));
    }

    // Overhead share (right) and LFE (left), as thin gauges.
    let gauge = |x: f32, frac: f32, color: Color32, name: &str| {
        let bar = Rect::from_min_max(
            Pos2::new(x, rect.top() + 14.0),
            Pos2::new(x + side_w - 2.0, rect.bottom() - 14.0),
        );
        painter.rect_filled(bar, 2.0, Color32::from_rgb(24, 27, 33));
        let frac = frac.clamp(0.0, 1.0);
        if frac > 0.0 {
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(bar.left(), bar.bottom() - frac * bar.height()),
                    bar.max,
                ),
                2.0,
                color,
            );
        }
        painter.text(
            Pos2::new(bar.center().x, rect.bottom() - 2.0),
            Align2::CENTER_BOTTOM,
            name,
            font.clone(),
            label_col,
        );
    };
    let has_height = layout.iter().flatten().any(|pos| pos.is_height());
    if has_height {
        gauge(
            rect.right() - side_w - 2.0,
            height_share(&layers),
            Color32::from_rgb(140, 160, 255),
            "H",
        );
    }
    if let Some(lfe_db) = lfe_db {
        gauge(
            rect.left() + 3.0,
            (lfe_db - DOT_FLOOR_DB) / -DOT_FLOOR_DB,
            level_color(lfe_db),
            "LFE",
        );
    }
    painter.text(
        rect.left_top() + Vec2::new(4.0, 2.0),
        Align2::LEFT_TOP,
        "SURROUND",
        font.clone(),
        label_col,
    );
}
