//! The dynamics tools on the waveform itself: each tool's level lines
//! (threshold, close, where the limiter or clipper starts) as +/- amplitude lines across
//! every lane, and the gain the rendered preview takes, hanging from the top
//! of the waveform over the target -- so where the compressor works, the
//! limiter catches and the gate shuts is seen against the audio.
//!
//! Built per frame from the tab (`for_tab`, a few floats and an `Arc`); the
//! trace it draws is the one the render produced, never recomputed here.

use std::sync::Arc;

use egui::{Color32, Painter, Pos2, Rect, Stroke};

use crate::app::tool_render::RenderAnalysis;
use crate::app::types::{EditorTab, ToolKind};

const THRESHOLD: Color32 = Color32::from_rgb(255, 170, 70);
const CLOSE: Color32 = Color32::from_rgb(246, 208, 80);
const CEILING: Color32 = Color32::from_rgb(255, 104, 104);
const GR_FILL: Color32 = Color32::from_rgba_premultiplied(110, 30, 30, 120);
const GR_EDGE: Color32 = Color32::from_rgb(255, 110, 110);
const CLOSED: Color32 = Color32::from_rgba_premultiplied(40, 10, 10, 46);
/// The band's depth for a full-scale reduction, as a share of the waveform.
const GR_DEPTH_SHARE: f32 = 0.35;
/// The gain a gate counts as shut at, for the shading.
const CLOSED_BELOW_DB: f32 = -6.0;

/// What one frame draws.
pub struct DynamicsOverlay {
    /// Level lines in dBFS, with their colours.
    lines: Vec<(f32, Color32)>,
    analysis: Option<Arc<RenderAnalysis>>,
    shade_closed: bool,
    /// The reduction drawn at full depth.
    gain_scale_db: f32,
}

impl DynamicsOverlay {
    /// The overlay for the tab's active tool, if it is a dynamics tool.
    pub fn for_tab(tab: &EditorTab) -> Option<Self> {
        let st = &tab.tool_state;
        let (lines, shade_closed, gain_scale_db) = match tab.active_tool {
            ToolKind::Compressor => (vec![(st.compressor.threshold_db, THRESHOLD)], false, 24.0),
            // Where the source starts being limited: the ceiling less the
            // gain that pushes the audio into it.
            ToolKind::Limiter => (
                vec![(st.limiter.ceiling_db - st.limiter.input_gain_db, CEILING)],
                false,
                24.0,
            ),
            // Where the source starts being cut, and where a soft knee
            // starts bending it.
            ToolKind::Clipper => {
                let c = &st.clipper;
                let mut lines = vec![(c.ceiling_db - c.input_gain_db, CEILING)];
                if c.knee_db > 0.0 {
                    lines.push((c.ceiling_db - c.knee_db - c.input_gain_db, THRESHOLD));
                }
                (lines, false, c.gr_scale_db())
            }
            ToolKind::NoiseGate => (
                vec![
                    (st.gate.threshold_db, THRESHOLD),
                    (st.gate.close_db(), CLOSE),
                ],
                true,
                (-st.gate.range_db).max(12.0),
            ),
            _ => return None,
        };
        let session = &tab.tool_session;
        let analysis = session
            .preview
            .as_ref()
            .filter(|_| !session.ab_original)
            .and_then(|p| p.output.analysis.clone());
        Some(Self {
            lines,
            analysis,
            shade_closed,
            gain_scale_db,
        })
    }

    /// The level lines across one lane. `amp_to_y` is the lane's own
    /// amplitude mapping (vertical zoom and centre included).
    pub fn draw_lane_lines(&self, painter: &Painter, lane: Rect, amp_to_y: impl Fn(f32) -> f32) {
        for &(db, color) in &self.lines {
            let amp = 10f32.powf(db / 20.0);
            for y in [amp_to_y(amp), amp_to_y(-amp)] {
                if y < lane.top() || y > lane.bottom() {
                    continue;
                }
                painter.add(egui::Shape::dashed_line(
                    &[Pos2::new(lane.left(), y), Pos2::new(lane.right(), y)],
                    Stroke::new(1.0, color.gamma_multiply(0.8)),
                    6.0,
                    4.0,
                ));
            }
        }
    }

    /// The gain the render takes, over `area` (every lane). `sample_x` maps a
    /// tab sample to its x; `visible` is the samples on screen.
    pub fn draw_gain(
        &self,
        painter: &Painter,
        area: Rect,
        visible: (usize, usize),
        sample_x: impl Fn(usize) -> f32,
    ) {
        let Some(RenderAnalysis::Dynamics(trace)) = self.analysis.as_deref() else {
            return;
        };
        let hop = trace.hop.max(1);
        let first = visible.0.saturating_sub(trace.start) / hop;
        let last =
            (visible.1.saturating_sub(trace.start).div_ceil(hop) + 1).min(trace.gain_db.len());
        if first >= last {
            return;
        }
        // One point per pixel at most: the deepest reduction under it.
        let mut columns: Vec<(f32, f32)> = Vec::new();
        for i in first..last {
            let x = sample_x(trace.start + i * hop).clamp(area.left(), area.right());
            let gain = trace.gain_db[i];
            match columns.last_mut() {
                Some((cx, g)) if (x - *cx).abs() < 1.0 => *g = g.min(gain),
                _ => columns.push((x, gain)),
            }
        }
        let depth = |gain: f32| {
            (-gain / self.gain_scale_db).clamp(0.0, 1.0) * area.height() * GR_DEPTH_SHARE
        };
        if self.shade_closed {
            for w in columns.windows(2) {
                if w[0].1 <= CLOSED_BELOW_DB {
                    painter.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(w[0].0, area.top()),
                            Pos2::new(w[1].0, area.bottom()),
                        ),
                        0.0,
                        CLOSED,
                    );
                }
            }
            return;
        }
        let mut mesh = egui::Mesh::default();
        let mut edge = Vec::with_capacity(columns.len());
        for &(x, gain) in &columns {
            let y = area.top() + depth(gain);
            mesh.colored_vertex(Pos2::new(x, area.top()), GR_FILL);
            mesh.colored_vertex(Pos2::new(x, y), GR_FILL);
            edge.push(Pos2::new(x, y));
        }
        for i in 0..(columns.len() as u32).saturating_sub(1) {
            let (a, b, c, d) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
            mesh.add_triangle(a, b, c);
            mesh.add_triangle(c, b, d);
        }
        painter.add(egui::Shape::mesh(mesh));
        painter.add(egui::Shape::line(edge, Stroke::new(1.0, GR_EDGE)));
    }

    /// Whether the overlay has a render to draw (for tests).
    #[cfg(feature = "kittest")]
    pub fn has_gain(&self) -> bool {
        matches!(self.analysis.as_deref(), Some(RenderAnalysis::Dynamics(_)))
    }
}
