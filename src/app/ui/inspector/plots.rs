//! The graphs of the Inspector's EQ and dynamics tools. Each one draws the
//! settings it is handed over the audio they act on -- the source measured
//! over the target, the render of the settings, what is playing now -- and
//! reports what the pointer did ([`PlotEdit`]). The panels own the settings
//! and the gesture bookkeeping; nothing here computes an analysis.

use egui::{Color32, CursorIcon, Mesh, Pos2, Rect, Sense, Stroke, Vec2};

use crate::app::render::mini_meter::SPECTRUM_DB_FLOOR;
use crate::app::tool_session::LiveMeters;
use crate::dynamics::{ClipperSettings, CompressorSettings, DynamicsTrace, GateSettings};
use crate::eq::{EqBand, EqChannelMode, EqKind, ParametricEqParams, EQ_MAX_BANDS};

const BG: Color32 = Color32::from_rgb(19, 21, 26);
const BORDER: Color32 = Color32::from_rgb(48, 53, 62);
const GRID: Color32 = Color32::from_rgb(38, 42, 50);
const GRID_STRONG: Color32 = Color32::from_rgb(66, 72, 84);
const LABEL: Color32 = Color32::from_rgb(112, 122, 138);
const CURVE: Color32 = Color32::from_rgb(236, 240, 248);
const MID_CURVE: Color32 = Color32::from_rgb(255, 176, 80);
const SIDE_CURVE: Color32 = Color32::from_rgb(196, 140, 255);
/// Premultiplied fills (`from_rgba_premultiplied` is const).
const PRE_FILL: Color32 = Color32::from_rgba_premultiplied(44, 47, 53, 80);
const LIVE_FILL: Color32 = Color32::from_rgba_premultiplied(18, 52, 56, 64);
const POST_LINE: Color32 = Color32::from_rgb(92, 182, 255);
const INPUT_FILL: Color32 = Color32::from_rgba_premultiplied(52, 55, 61, 92);
const GR_FILL: Color32 = Color32::from_rgba_premultiplied(104, 32, 32, 112);
const GR_LINE: Color32 = Color32::from_rgb(255, 104, 104);
const CLOSED_SHADE: Color32 = Color32::from_rgba_premultiplied(26, 8, 8, 32);
const PLAYHEAD: Color32 = Color32::from_rgb(250, 250, 250);
const LIVE_DOT: Color32 = Color32::from_rgb(120, 240, 170);
const HANDLE_HIT: f32 = 11.0;

/// One colour per EQ band slot.
pub(in crate::app) const BAND_COLORS: [Color32; EQ_MAX_BANDS] = [
    Color32::from_rgb(255, 150, 70),
    Color32::from_rgb(246, 208, 80),
    Color32::from_rgb(124, 220, 120),
    Color32::from_rgb(80, 210, 220),
    Color32::from_rgb(104, 150, 255),
    Color32::from_rgb(178, 124, 255),
    Color32::from_rgb(255, 112, 180),
    Color32::from_rgb(232, 128, 104),
];

/// What the pointer did to a graph this frame.
#[derive(Default, Clone, Copy, Debug)]
pub(in crate::app) struct PlotEdit {
    pub changed: bool,
    /// A drag on the graph began / ended: one draft step per drag.
    pub began: bool,
    pub ended: bool,
    /// A click on a time graph: go to this sample.
    pub seek: Option<usize>,
}

fn frame(ui: &mut egui::Ui, size: Vec2) -> (egui::Response, egui::Painter, Rect) {
    let size = Vec2::new(size.x.max(80.0), size.y.max(40.0));
    let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
    painter.rect_filled(resp.rect, 4.0, BG);
    painter.rect_stroke(
        resp.rect,
        4.0,
        Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );
    let inner = resp.rect.shrink(4.0);
    (resp, painter, inner)
}

fn small_font(ui: &egui::Ui) -> egui::FontId {
    egui::TextStyle::Small.resolve(ui.style())
}

fn hline(painter: &egui::Painter, r: Rect, y: f32, color: Color32) {
    painter.line_segment(
        [Pos2::new(r.left(), y), Pos2::new(r.right(), y)],
        Stroke::new(1.0, color),
    );
}

fn vline(painter: &egui::Painter, r: Rect, x: f32, color: Color32) {
    painter.line_segment(
        [Pos2::new(x, r.top()), Pos2::new(x, r.bottom())],
        Stroke::new(1.0, color),
    );
}

/// Fill between a curve and a horizontal baseline, one strip per segment.
fn fill_to(painter: &egui::Painter, pts: &[Pos2], base_y: f32, color: Color32) {
    if pts.len() < 2 {
        return;
    }
    let mut mesh = Mesh::default();
    for p in pts {
        mesh.colored_vertex(*p, color);
        mesh.colored_vertex(Pos2::new(p.x, base_y), color);
    }
    for i in 0..(pts.len() as u32 - 1) {
        let (a, b, c, d) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
        mesh.add_triangle(a, b, c);
        mesh.add_triangle(c, b, d);
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn label_at(
    painter: &egui::Painter,
    ui: &egui::Ui,
    pos: Pos2,
    align: egui::Align2,
    text: String,
    color: Color32,
) {
    painter.text(pos, align, text, small_font(ui), color);
}

/// A tag beside a handle, on a dark backing, kept inside the plot.
fn tag(painter: &egui::Painter, ui: &egui::Ui, r: Rect, at: Pos2, text: String) {
    let galley = painter.layout_no_wrap(text, small_font(ui), CURVE);
    let size = galley.size();
    let x = if at.x > r.center().x {
        at.x - 10.0 - size.x
    } else {
        at.x + 10.0
    };
    let x = x.clamp(
        r.left() + 2.0,
        (r.right() - size.x - 2.0).max(r.left() + 2.0),
    );
    let y = (at.y - 8.0 - size.y).max(r.top() + 2.0);
    let pos = Pos2::new(x, y);
    painter.rect_filled(
        Rect::from_min_size(pos, size).expand(2.0),
        2.0,
        Color32::from_rgba_premultiplied(0, 0, 0, 170),
    );
    painter.galley(pos, galley, CURVE);
}

fn hz_text(hz: f32) -> String {
    if hz >= 1_000.0 {
        format!("{:.2} kHz", hz / 1_000.0)
    } else {
        format!("{hz:.0} Hz")
    }
}

// ---------------------------------------------------------------------------
// EQ

const EQ_F_LO: f32 = crate::sample_rate::AUDIBLE_LOW_HZ;
const EQ_F_HI: f32 = crate::sample_rate::AUDIBLE_HIGH_HZ;
/// The curve's vertical span: the largest band gain.
const EQ_DB: f32 = *ParametricEqParams::GAIN_DB.end();
/// Q change per wheel notch.
const Q_WHEEL_STEP: f32 = 1.12;

fn freq_to_x(r: Rect, hz: f32) -> f32 {
    let t = (hz.clamp(EQ_F_LO, EQ_F_HI) / EQ_F_LO).ln() / (EQ_F_HI / EQ_F_LO).ln();
    r.left() + t * r.width()
}

fn x_to_freq(r: Rect, x: f32) -> f32 {
    let t = ((x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0);
    EQ_F_LO * (EQ_F_HI / EQ_F_LO).powf(t)
}

fn gain_to_y(r: Rect, db: f32) -> f32 {
    let t = ((db + EQ_DB) / (2.0 * EQ_DB)).clamp(0.0, 1.0);
    r.bottom() - t * r.height()
}

fn y_to_gain(r: Rect, y: f32) -> f32 {
    let t = ((r.bottom() - y) / r.height().max(1.0)).clamp(0.0, 1.0);
    -EQ_DB + t * 2.0 * EQ_DB
}

/// How high a cut's handle sits: the resonance its Q adds over the flat
/// corner, in dB. Dragging the handle up and down sets that Q.
fn cut_resonance_db(cut_q: f32) -> f32 {
    20.0 * (cut_q / crate::eq::BUTTERWORTH_Q as f32).max(1e-3).log10()
}

fn cut_q_for_resonance_db(db: f32) -> f32 {
    (crate::eq::BUTTERWORTH_Q as f32 * 10f32.powf(db / 20.0)).clamp(
        *ParametricEqParams::CUT_Q.start(),
        *ParametricEqParams::CUT_Q.end(),
    )
}

/// The height a band's handle sits at: its gain, a cut's resonance, or
/// 0 dB (a notch).
fn handle_db(b: &EqBand) -> f32 {
    if b.kind.uses_gain() {
        b.gain_db
    } else if b.kind.uses_slope() {
        cut_resonance_db(b.cut_q)
    } else {
        0.0
    }
}

/// Where a band's handle sits: its corner, at `handle_db`.
fn band_handle(r: Rect, b: &EqBand) -> Pos2 {
    Pos2::new(freq_to_x(r, b.freq_hz), gain_to_y(r, handle_db(b)))
}

/// The audio behind the EQ's curve.
pub(in crate::app) struct EqView<'a> {
    pub sample_rate: u32,
    /// The source over the target, before the edit.
    pub pre: Option<&'a [f32]>,
    /// The render of these settings.
    pub post: Option<&'a [f32]>,
    /// What is playing now.
    pub live: Option<&'a [f32]>,
    /// The file has a stereo pair: Mid / Side bands mean something.
    pub stereo: bool,
}

#[derive(Clone, Copy)]
struct EqGrab {
    band: usize,
    from: Pos2,
    freq: f32,
    gain: f32,
}

/// The parametric EQ's response over its analyzer, with a handle per band.
/// Double-click empty space to add a band; drag a handle (Shift: fine) --
/// a cut's height is its resonance; scroll on one for Q (a cut's slope);
/// Alt+click to bypass; right-click for its shape, mode and Delete.
pub(in crate::app) fn eq_plot(
    ui: &mut egui::Ui,
    id: egui::Id,
    params: &mut ParametricEqParams,
    selected: &mut usize,
    view: &EqView<'_>,
    size: Vec2,
) -> PlotEdit {
    let (resp, painter, r) = frame(ui, size);
    let mut edit = PlotEdit::default();
    let sr = view.sample_rate.max(1);

    // Grid.
    for hz in [
        30.0f32, 50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0,
    ] {
        let x = freq_to_x(r, hz);
        let strong = matches!(hz as u32, 100 | 1_000 | 10_000);
        vline(&painter, r, x, if strong { GRID_STRONG } else { GRID });
        if strong || hz as u32 == 50 || hz as u32 == 500 || hz as u32 == 5_000 {
            let text = if hz >= 1_000.0 {
                format!("{}k", hz as u32 / 1_000)
            } else {
                format!("{}", hz as u32)
            };
            label_at(
                &painter,
                ui,
                Pos2::new(x + 2.0, r.bottom() - 1.0),
                egui::Align2::LEFT_BOTTOM,
                text,
                LABEL,
            );
        }
    }
    for db in [-18.0f32, -12.0, -6.0, 0.0, 6.0, 12.0, 18.0] {
        let y = gain_to_y(r, db);
        hline(&painter, r, y, if db == 0.0 { GRID_STRONG } else { GRID });
        if db != 0.0 && (db as i32) % 12 == 0 {
            label_at(
                &painter,
                ui,
                Pos2::new(r.left() + 2.0, y - 1.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{db:+.0}"),
                LABEL,
            );
        }
    }

    // The audio: before (filled), live (filled), after (a line).
    if let Some(pre) = view.pre {
        painter.add(crate::app::render::spectrum_fill::spectrum_mesh(
            r,
            pre,
            SPECTRUM_DB_FLOOR,
            0.0,
            |_, _| PRE_FILL,
        ));
    }
    if let Some(live) = view.live.filter(|l| !l.is_empty()) {
        painter.add(crate::app::render::spectrum_fill::spectrum_mesh(
            r,
            live,
            SPECTRUM_DB_FLOOR,
            0.0,
            |_, _| LIVE_FILL,
        ));
    }
    if let Some(post) = view.post {
        let n = post.len().max(1);
        let pts: Vec<Pos2> = post
            .iter()
            .enumerate()
            .map(|(i, db)| {
                let t = (i as f32 + 0.5) / n as f32;
                let norm = ((db - SPECTRUM_DB_FLOOR) / -SPECTRUM_DB_FLOOR).clamp(0.0, 1.0);
                Pos2::new(r.left() + t * r.width(), r.bottom() - norm * r.height())
            })
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.2, POST_LINE)));
    }

    // Each band's own curve, filled to 0 dB.
    let steps = (r.width() / 3.0).clamp(48.0, 200.0) as usize;
    let xs: Vec<f32> = (0..=steps)
        .map(|i| r.left() + i as f32 / steps as f32 * r.width())
        .collect();
    let zero_y = gain_to_y(r, 0.0);
    for (i, band) in params.bands.iter().enumerate() {
        if !band.is_heard() {
            continue;
        }
        let color = BAND_COLORS[i];
        let pts: Vec<Pos2> = xs
            .iter()
            .map(|&x| {
                let db = crate::eq::band_response_db(band, sr, x_to_freq(r, x));
                Pos2::new(x, gain_to_y(r, db))
            })
            .collect();
        let alpha = if i == *selected { 60 } else { 34 };
        fill_to(
            &painter,
            &pts,
            zero_y,
            color.gamma_multiply(alpha as f32 / 255.0),
        );
        painter.add(egui::Shape::line(
            pts,
            Stroke::new(1.0, color.gamma_multiply(0.75)),
        ));
    }
    // The totals: what the stereo bands do, and with M/S, mid and side.
    let total = |mode: EqChannelMode| -> Vec<Pos2> {
        xs.iter()
            .map(|&x| {
                Pos2::new(
                    x,
                    gain_to_y(r, crate::eq::response_db(params, mode, sr, x_to_freq(r, x))),
                )
            })
            .collect()
    };
    if params.has_mid_side() && view.stereo {
        painter.add(egui::Shape::line(
            total(EqChannelMode::Mid),
            Stroke::new(1.8, MID_CURVE),
        ));
        painter.add(egui::Shape::line(
            total(EqChannelMode::Side),
            Stroke::new(1.8, SIDE_CURVE),
        ));
    }
    painter.add(egui::Shape::line(
        total(EqChannelMode::Stereo),
        Stroke::new(2.2, CURVE),
    ));

    // Pointer.
    let grab_id = id.with("eq_grab");
    let menu_id = id.with("eq_menu_band");
    let mut grab: Option<EqGrab> = ui.data_mut(|d| d.get_temp(grab_id)).flatten();
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    let nearest = |pos: Pos2, params: &ParametricEqParams| -> Option<usize> {
        params
            .bands
            .iter()
            .enumerate()
            .filter(|(_, b)| b.enabled)
            .map(|(i, b)| (i, band_handle(r, b).distance(pos)))
            .filter(|(_, d)| *d <= HANDLE_HIT)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let (shift, alt) = ui.input(|i| (i.modifiers.shift, i.modifiers.alt));
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some(band) = nearest(pos, params) {
                *selected = band;
                let b = params.bands[band];
                grab = Some(EqGrab {
                    band,
                    from: pos,
                    freq: b.freq_hz,
                    gain: handle_db(&b),
                });
                edit.began = true;
            }
        }
    }
    if let (true, Some(g), Some(pos)) = (resp.dragged(), grab, resp.interact_pointer_pos()) {
        let factor = if shift { 0.2 } else { 1.0 };
        let x = freq_to_x(r, g.freq) + (pos.x - g.from.x) * factor;
        let y = gain_to_y(r, g.gain) + (pos.y - g.from.y) * factor;
        let b = &mut params.bands[g.band];
        b.freq_hz = x_to_freq(r, x).clamp(
            *ParametricEqParams::FREQ_HZ.start(),
            *ParametricEqParams::FREQ_HZ.end(),
        );
        if b.kind.uses_gain() {
            b.gain_db = y_to_gain(r, y).clamp(-EQ_DB, EQ_DB);
        } else if b.kind.uses_slope() {
            b.cut_q = cut_q_for_resonance_db(y_to_gain(r, y));
        }
        edit.changed = true;
    }
    if resp.drag_stopped() && grab.is_some() {
        grab = None;
        edit.ended = true;
    }
    if !ui.input(|i| i.pointer.any_down()) && grab.is_some() {
        grab = None;
        edit.ended = true;
    }
    ui.data_mut(|d| d.insert_temp(grab_id, grab));

    if resp.clicked() {
        if let Some(band) = pointer.and_then(|p| nearest(p, params)) {
            *selected = band;
            if alt {
                params.bands[band].bypassed = !params.bands[band].bypassed;
                edit.changed = true;
            }
        }
    }
    if resp.double_clicked() {
        if let Some(pos) = pointer {
            if nearest(pos, params).is_none() {
                if let Some(slot) = params.free_slot() {
                    params.bands[slot] = EqBand {
                        enabled: true,
                        kind: EqKind::Bell,
                        freq_hz: x_to_freq(r, pos.x),
                        gain_db: y_to_gain(r, pos.y).clamp(-EQ_DB, EQ_DB),
                        q: 1.0,
                        ..EqBand::default()
                    };
                    *selected = slot;
                    edit.changed = true;
                }
            }
        }
    }
    if resp.secondary_clicked() {
        if let Some(band) = pointer.and_then(|p| nearest(p, params)) {
            *selected = band;
            ui.data_mut(|d| d.insert_temp(menu_id, band));
        }
    }
    // The wheel over a handle shapes it; elsewhere it scrolls the Inspector.
    if let Some(band) = resp.hover_pos().and_then(|p| nearest(p, params)) {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 {
            let b = &mut params.bands[band];
            if b.kind.uses_slope() {
                let all = crate::eq::CutSlope::ALL;
                let at = all.iter().position(|s| *s == b.slope).unwrap_or(1) as i32;
                let next = (at + scroll.signum() as i32).clamp(0, all.len() as i32 - 1);
                b.slope = all[next as usize];
            } else {
                let step = if scroll > 0.0 {
                    Q_WHEEL_STEP
                } else {
                    1.0 / Q_WHEEL_STEP
                };
                b.q = (b.q * step)
                    .clamp(*ParametricEqParams::Q.start(), *ParametricEqParams::Q.end());
            }
            edit.changed = true;
            ui.ctx().input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
        }
    }

    // Handles.
    let hovered = resp.hover_pos().and_then(|p| nearest(p, params));
    for (i, band) in params.bands.iter().enumerate() {
        if !band.enabled {
            continue;
        }
        let p = band_handle(r, band);
        let color = BAND_COLORS[i];
        let active = hovered == Some(i) || grab.is_some_and(|g| g.band == i);
        let radius = if active { 8.0 } else { 7.0 };
        if band.bypassed {
            painter.circle_stroke(p, radius, Stroke::new(1.5, color.gamma_multiply(0.6)));
        } else {
            painter.circle_filled(p, radius, color);
        }
        if i == *selected {
            painter.circle_stroke(p, radius + 2.5, Stroke::new(1.5, CURVE));
        }
        painter.text(
            p,
            egui::Align2::CENTER_CENTER,
            format!("{}", i + 1),
            egui::FontId::proportional(10.0),
            if band.bypassed {
                color
            } else {
                Color32::from_rgb(16, 16, 20)
            },
        );
        if active {
            let mut text = format!("{} {} ", i + 1, band.kind.label());
            text.push_str(&hz_text(band.freq_hz));
            if band.kind.uses_gain() {
                text.push_str(&format!(" {:+.1} dB", band.gain_db));
            }
            if band.kind.uses_slope() {
                text.push_str(&format!(" {}", band.slope.label()));
            }
            text.push_str(&format!(" Q {:.2}", band.q_value()));
            if band.mode != EqChannelMode::Stereo {
                text.push_str(&format!(" · {}", band.mode.label()));
            }
            tag(&painter, ui, r, p, text);
        }
    }

    // Right-click menu of the band last right-clicked.
    let menu_band: Option<usize> = ui.data_mut(|d| d.get_temp(menu_id));
    resp.context_menu(|ui| {
        let Some(band) = menu_band.filter(|b| params.bands[*b].enabled) else {
            ui.label("Right-click a band's handle");
            return;
        };
        let b = &mut params.bands[band];
        ui.label(egui::RichText::new(format!("Band {}", band + 1)).strong());
        for kind in EqKind::ALL {
            if ui.radio(b.kind == kind, kind.label()).clicked() && b.kind != kind {
                b.kind = kind;
                edit.changed = true;
            }
        }
        if b.kind.uses_slope() {
            ui.separator();
            for slope in crate::eq::CutSlope::ALL {
                if ui.radio(b.slope == slope, slope.label()).clicked() && b.slope != slope {
                    b.slope = slope;
                    edit.changed = true;
                }
            }
        }
        ui.separator();
        ui.add_enabled_ui(view.stereo, |ui| {
            for mode in EqChannelMode::ALL {
                if ui.radio(b.mode == mode, mode.label()).clicked() && b.mode != mode {
                    b.mode = mode;
                    edit.changed = true;
                }
            }
        });
        ui.separator();
        let mut on = !b.bypassed;
        if ui.checkbox(&mut on, "On").changed() {
            b.bypassed = !on;
            edit.changed = true;
        }
        if ui.button("Delete").clicked() {
            *b = EqBand::default();
            edit.changed = true;
            ui.close();
        }
    });

    if resp.hovered() {
        ui.ctx()
            .set_cursor_icon(if hovered.is_some() || grab.is_some() {
                CursorIcon::Grab
            } else {
                CursorIcon::Crosshair
            });
    }
    if edit.changed && !edit.began {
        // A click, a wheel step or a menu choice is a gesture of its own.
        edit.began = grab.is_none();
        edit.ended = grab.is_none();
    }
    edit
}

// ---------------------------------------------------------------------------
// Dynamics: transfer curve

const DYN_LO: f32 = -60.0;
const DYN_HI: f32 = 0.0;

fn dyn_x(r: Rect, db: f32) -> f32 {
    r.left() + ((db - DYN_LO) / (DYN_HI - DYN_LO)).clamp(0.0, 1.0) * r.width()
}

fn dyn_y(r: Rect, db: f32) -> f32 {
    r.bottom() - ((db - DYN_LO) / (DYN_HI - DYN_LO)).clamp(0.0, 1.0) * r.height()
}

fn dyn_x_db(r: Rect, x: f32) -> f32 {
    DYN_LO + ((x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0) * (DYN_HI - DYN_LO)
}

fn dyn_y_db(r: Rect, y: f32) -> f32 {
    DYN_LO + ((r.bottom() - y) / r.height().max(1.0)).clamp(0.0, 1.0) * (DYN_HI - DYN_LO)
}

/// The settings a transfer curve draws and edits.
pub(in crate::app) enum Transfer<'a> {
    Compressor(&'a mut CompressorSettings),
    Gate(&'a mut GateSettings),
    Clipper(&'a mut ClipperSettings),
}

/// Input level (x) to output level (y), with how often each input level
/// occurs over the target along the bottom and the level playing now as a
/// dot. Handles: the compressor's threshold, ratio and knee width; the
/// gate's open and close thresholds and its floor; the clipper's corner
/// (ceiling up and down, input gain sideways) and its knee.
pub(in crate::app) fn transfer_plot(
    ui: &mut egui::Ui,
    id: egui::Id,
    transfer: Transfer<'_>,
    levels: Option<&DynamicsTrace>,
    live_in_db: Option<f32>,
    size: Vec2,
) -> PlotEdit {
    let (resp, painter, r) = frame(ui, size);
    let mut edit = PlotEdit::default();
    for db in [-48.0f32, -36.0, -24.0, -12.0] {
        vline(&painter, r, dyn_x(r, db), GRID);
        hline(&painter, r, dyn_y(r, db), GRID);
        label_at(
            &painter,
            ui,
            Pos2::new(dyn_x(r, db) + 2.0, r.bottom() - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{db:.0}"),
            LABEL,
        );
    }
    painter.line_segment(
        [
            Pos2::new(dyn_x(r, DYN_LO), dyn_y(r, DYN_LO)),
            Pos2::new(dyn_x(r, DYN_HI), dyn_y(r, DYN_HI)),
        ],
        Stroke::new(1.0, GRID_STRONG),
    );

    // How the input is distributed: one bar per dB.
    if let Some(levels) = levels {
        let bins = (DYN_HI - DYN_LO) as usize;
        let mut counts = vec![0u32; bins];
        for db in &levels.input_db {
            if *db >= DYN_LO && *db < DYN_HI {
                counts[((db - DYN_LO) as usize).min(bins - 1)] += 1;
            }
        }
        let most = counts.iter().copied().max().unwrap_or(0).max(1) as f32;
        let bar_h = r.height() * 0.28;
        for (i, c) in counts.iter().enumerate() {
            if *c == 0 {
                continue;
            }
            let x0 = dyn_x(r, DYN_LO + i as f32);
            let x1 = dyn_x(r, DYN_LO + i as f32 + 1.0);
            let h = (*c as f32 / most).sqrt() * bar_h;
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x0, r.bottom() - h),
                    Pos2::new(x1 - 0.5, r.bottom()),
                ),
                0.0,
                INPUT_FILL,
            );
        }
    }

    let handle = |painter: &egui::Painter, p: Pos2, color: Color32, active: bool| {
        painter.circle_filled(p, if active { 6.5 } else { 5.0 }, color);
        painter.circle_stroke(
            p,
            if active { 6.5 } else { 5.0 },
            Stroke::new(1.5, Color32::from_rgb(14, 14, 18)),
        );
    };
    let grab_id = id.with("transfer_grab");
    let mut grab: Option<u8> = ui.data_mut(|d| d.get_temp(grab_id)).flatten();
    let hover = resp.hover_pos();
    let steps = 120usize;
    match transfer {
        Transfer::Compressor(s) => {
            let curve: Vec<Pos2> = (0..=steps)
                .map(|i| {
                    let db = DYN_LO + i as f32 / steps as f32 * (DYN_HI - DYN_LO);
                    Pos2::new(dyn_x(r, db), dyn_y(r, s.transfer_db(db)))
                })
                .collect();
            if s.knee_db > 0.0 {
                let a = dyn_x(r, s.threshold_db - s.knee_db * 0.5);
                let b = dyn_x(r, s.threshold_db + s.knee_db * 0.5);
                painter.rect_filled(
                    Rect::from_x_y_ranges(a..=b, r.y_range()),
                    0.0,
                    Color32::from_rgba_premultiplied(20, 20, 8, 24),
                );
            }
            painter.add(egui::Shape::line(curve, Stroke::new(2.0, CURVE)));
            let thr = Pos2::new(
                dyn_x(r, s.threshold_db),
                dyn_y(r, s.transfer_db(s.threshold_db)),
            );
            let top = Pos2::new(dyn_x(r, DYN_HI), dyn_y(r, s.transfer_db(DYN_HI)));
            let knee_db = s.threshold_db + s.knee_db * 0.5;
            let knee = Pos2::new(dyn_x(r, knee_db), dyn_y(r, s.transfer_db(knee_db)) + 10.0);
            let pts = [thr, top, knee];
            if resp.drag_started() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    grab = pts
                        .iter()
                        .enumerate()
                        .map(|(i, p)| (i as u8, p.distance(pos)))
                        .filter(|(_, d)| *d <= HANDLE_HIT)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(i, _)| i);
                    edit.began = grab.is_some();
                }
            }
            if let (true, Some(which), Some(pos)) =
                (resp.dragged(), grab, resp.interact_pointer_pos())
            {
                match which {
                    0 => {
                        s.threshold_db = dyn_x_db(r, pos.x).clamp(
                            *CompressorSettings::THRESHOLD_DB.start(),
                            *CompressorSettings::THRESHOLD_DB.end(),
                        )
                    }
                    1 => {
                        // out(0) = thr + (0 - thr) / ratio (+ makeup): solve for the ratio.
                        let out = dyn_y_db(r, pos.y) - s.effective_makeup_db();
                        let over = -s.threshold_db;
                        let rise =
                            (out - s.threshold_db).max(over / *CompressorSettings::RATIO.end());
                        if over > 0.5 {
                            s.ratio = (over / rise).clamp(
                                *CompressorSettings::RATIO.start(),
                                *CompressorSettings::RATIO.end(),
                            );
                        }
                    }
                    _ => {
                        s.knee_db = ((dyn_x_db(r, pos.x) - s.threshold_db) * 2.0).clamp(
                            *CompressorSettings::KNEE_DB.start(),
                            *CompressorSettings::KNEE_DB.end(),
                        )
                    }
                }
                edit.changed = true;
            }
            for (i, (p, color)) in [
                (thr, BAND_COLORS[0]),
                (top, BAND_COLORS[2]),
                (knee, BAND_COLORS[1]),
            ]
            .into_iter()
            .enumerate()
            {
                let active =
                    grab == Some(i as u8) || hover.is_some_and(|h| h.distance(p) <= HANDLE_HIT);
                handle(&painter, p, color, active);
                if active {
                    let text = match i {
                        0 => format!("Threshold {:.1} dB", s.threshold_db),
                        1 => format!("Ratio {:.1}:1", s.ratio),
                        _ => format!("Knee {:.1} dB", s.knee_db),
                    };
                    tag(&painter, ui, r, p, text);
                }
            }
            if let Some(db) = live_in_db.filter(|db| *db > DYN_LO) {
                painter.circle_filled(
                    Pos2::new(dyn_x(r, db), dyn_y(r, s.transfer_db(db))),
                    4.0,
                    LIVE_DOT,
                );
            }
        }
        Transfer::Clipper(s) => {
            clipper_transfer(ui, &painter, &resp, r, s, &mut grab, live_in_db, &mut edit);
        }
        Transfer::Gate(s) => {
            let floor = s.range_db;
            let close = s.close_db();
            let shut = |db: f32| db + floor;
            // Below the close line the floor applies; above the threshold
            // it is open; in between it depends on which way the level came.
            painter.rect_filled(
                Rect::from_x_y_ranges(dyn_x(r, DYN_LO)..=dyn_x(r, close), r.y_range()),
                0.0,
                CLOSED_SHADE,
            );
            let open_line: Vec<Pos2> = (0..=steps)
                .map(|i| {
                    let db = close + i as f32 / steps as f32 * (DYN_HI - close);
                    Pos2::new(dyn_x(r, db), dyn_y(r, db))
                })
                .collect();
            let shut_line: Vec<Pos2> = (0..=steps)
                .map(|i| {
                    let db = DYN_LO + i as f32 / steps as f32 * (s.threshold_db - DYN_LO);
                    Pos2::new(dyn_x(r, db), dyn_y(r, shut(db)))
                })
                .collect();
            painter.add(egui::Shape::line(shut_line, Stroke::new(2.0, CURVE)));
            painter.add(egui::Shape::line(open_line, Stroke::new(2.0, CURVE)));
            let thr = Pos2::new(dyn_x(r, s.threshold_db), dyn_y(r, s.threshold_db));
            let close_p = Pos2::new(dyn_x(r, close), dyn_y(r, shut(close).max(DYN_LO)));
            // The floor is a depth, often deeper than the plot: its handle
            // rides a scale from the open line (0 dB) down to the bottom
            // edge (silence), drawn as a bracket inside the closed stretch.
            let floor_x = dyn_x(r, (close - 4.0).max(DYN_LO + 2.0));
            let open_y = dyn_y(r, dyn_x_db(r, floor_x));
            let deepest = *GateSettings::RANGE_DB.start();
            let floor_p = Pos2::new(
                floor_x,
                open_y + (r.bottom() - open_y) * (floor / deepest).clamp(0.0, 1.0),
            );
            painter.line_segment(
                [Pos2::new(floor_x, open_y), Pos2::new(floor_x, r.bottom())],
                Stroke::new(1.0, GR_LINE.gamma_multiply(0.4)),
            );
            let pts = [thr, close_p, floor_p];
            if resp.drag_started() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    grab = pts
                        .iter()
                        .enumerate()
                        .map(|(i, p)| (i as u8, p.distance(pos)))
                        .filter(|(_, d)| *d <= HANDLE_HIT)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(i, _)| i);
                    edit.began = grab.is_some();
                }
            }
            if let (true, Some(which), Some(pos)) =
                (resp.dragged(), grab, resp.interact_pointer_pos())
            {
                match which {
                    0 => {
                        s.threshold_db = dyn_x_db(r, pos.x).clamp(
                            *GateSettings::THRESHOLD_DB.start(),
                            *GateSettings::THRESHOLD_DB.end(),
                        )
                    }
                    1 => {
                        s.hysteresis_db = (s.threshold_db - dyn_x_db(r, pos.x)).clamp(
                            *GateSettings::HYSTERESIS_DB.start(),
                            *GateSettings::HYSTERESIS_DB.end(),
                        )
                    }
                    _ => {
                        let depth =
                            ((pos.y - open_y) / (r.bottom() - open_y).max(1.0)).clamp(0.0, 1.0);
                        s.range_db = (deepest * depth).clamp(
                            *GateSettings::RANGE_DB.start(),
                            *GateSettings::RANGE_DB.end(),
                        )
                    }
                }
                edit.changed = true;
            }
            for (i, (p, color)) in [
                (thr, BAND_COLORS[0]),
                (close_p, BAND_COLORS[1]),
                (floor_p, GR_LINE),
            ]
            .into_iter()
            .enumerate()
            {
                let active =
                    grab == Some(i as u8) || hover.is_some_and(|h| h.distance(p) <= HANDLE_HIT);
                handle(&painter, p, color, active);
                if active {
                    let text = match i {
                        0 => format!("Opens at {:.1} dB", s.threshold_db),
                        1 => format!("Closes at {:.1} dB", s.close_db()),
                        _ => format!("Floor {:.0} dB", s.range_db),
                    };
                    tag(&painter, ui, r, p, text);
                }
            }
            if let Some(db) = live_in_db.filter(|db| *db > DYN_LO) {
                let out = if db >= close { db } else { shut(db) };
                painter.circle_filled(Pos2::new(dyn_x(r, db), dyn_y(r, out)), 4.0, LIVE_DOT);
            }
        }
    }
    if resp.drag_stopped() && grab.is_some() {
        grab = None;
        edit.ended = true;
    }
    if !ui.input(|i| i.pointer.any_down()) && grab.is_some() {
        grab = None;
        edit.ended = true;
    }
    ui.data_mut(|d| d.insert_temp(grab_id, grab));
    if resp.hovered() {
        ui.ctx().set_cursor_icon(if grab.is_some() {
            CursorIcon::Grabbing
        } else {
            CursorIcon::Crosshair
        });
    }
    edit
}

/// The clipper's curve on a transfer plot: the ceiling, the knee shaded,
/// and its two handles.
#[allow(clippy::too_many_arguments)]
fn clipper_transfer(
    ui: &egui::Ui,
    painter: &egui::Painter,
    resp: &egui::Response,
    r: Rect,
    s: &mut ClipperSettings,
    grab: &mut Option<u8>,
    live_in_db: Option<f32>,
    edit: &mut PlotEdit,
) {
    let steps = 160usize;
    // Where the source reaches the ceiling, and where the knee starts.
    let corner_in = s.ceiling_db - s.input_gain_db;
    let knee_in = corner_in - s.knee_db;
    if s.knee_db > 0.0 {
        painter.rect_filled(
            Rect::from_x_y_ranges(dyn_x(r, knee_in)..=dyn_x(r, corner_in), r.y_range()),
            0.0,
            Color32::from_rgba_premultiplied(20, 20, 8, 24),
        );
    }
    painter.add(egui::Shape::dashed_line(
        &[
            Pos2::new(r.left(), dyn_y(r, s.ceiling_db)),
            Pos2::new(r.right(), dyn_y(r, s.ceiling_db)),
        ],
        Stroke::new(1.0, GR_LINE.gamma_multiply(0.6)),
        5.0,
        3.0,
    ));
    let curve: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let db = DYN_LO + i as f32 / steps as f32 * (DYN_HI - DYN_LO);
            Pos2::new(dyn_x(r, db), dyn_y(r, s.transfer_db(db)))
        })
        .collect();
    painter.add(egui::Shape::line(curve, Stroke::new(2.0, CURVE)));
    let corner = Pos2::new(dyn_x(r, corner_in), dyn_y(r, s.ceiling_db));
    let knee = Pos2::new(dyn_x(r, knee_in), dyn_y(r, s.transfer_db(knee_in)) + 10.0);
    let pts = [corner, knee];
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            *grab = pts
                .iter()
                .enumerate()
                .map(|(i, p)| (i as u8, p.distance(pos)))
                .filter(|(_, d)| *d <= HANDLE_HIT)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i);
            edit.began = grab.is_some();
        }
    }
    if let (true, Some(which), Some(pos)) = (resp.dragged(), *grab, resp.interact_pointer_pos()) {
        if which == 0 {
            s.ceiling_db = dyn_y_db(r, pos.y).clamp(
                *ClipperSettings::CEILING_DB.start(),
                *ClipperSettings::CEILING_DB.end(),
            );
            s.input_gain_db = (s.ceiling_db - dyn_x_db(r, pos.x)).clamp(
                *ClipperSettings::INPUT_GAIN_DB.start(),
                *ClipperSettings::INPUT_GAIN_DB.end(),
            );
        } else {
            s.knee_db = (s.ceiling_db - s.input_gain_db - dyn_x_db(r, pos.x)).clamp(
                *ClipperSettings::KNEE_DB.start(),
                *ClipperSettings::KNEE_DB.end(),
            );
        }
        edit.changed = true;
    }
    let hover = resp.hover_pos();
    for (i, (p, color)) in [(corner, BAND_COLORS[0]), (knee, BAND_COLORS[1])]
        .into_iter()
        .enumerate()
    {
        let active = *grab == Some(i as u8) || hover.is_some_and(|h| h.distance(p) <= HANDLE_HIT);
        painter.circle_filled(p, if active { 6.5 } else { 5.0 }, color);
        painter.circle_stroke(
            p,
            if active { 6.5 } else { 5.0 },
            Stroke::new(1.5, Color32::from_rgb(14, 14, 18)),
        );
        if active {
            let text = if i == 0 {
                format!("Ceiling {:.1} dB · Input +{:.1} dB", s.ceiling_db, s.input_gain_db)
            } else {
                format!("Knee {:.1} dB", s.knee_db)
            };
            tag(painter, ui, r, p, text);
        }
    }
    if let Some(db) = live_in_db.filter(|db| *db > DYN_LO) {
        painter.circle_filled(Pos2::new(dyn_x(r, db), dyn_y(r, s.transfer_db(db))), 4.0, LIVE_DOT);
    }
}

// ---------------------------------------------------------------------------
// Dynamics: level and gain over time

const HIST_LO: f32 = -60.0;
const HIST_HI: f32 = 6.0;

fn hist_y(r: Rect, db: f32) -> f32 {
    r.bottom() - ((db - HIST_LO) / (HIST_HI - HIST_LO)).clamp(0.0, 1.0) * r.height()
}

fn hist_db(r: Rect, y: f32) -> f32 {
    HIST_LO + ((r.bottom() - y) / r.height().max(1.0)).clamp(0.0, 1.0) * (HIST_HI - HIST_LO)
}

/// A level line on the time graph (a threshold, a ceiling): horizontal at
/// `db`, or -- when the setting follows a curve -- the curve, which is
/// edited on the waveform and so is not draggable here.
pub(in crate::app) struct HistoryLine {
    pub db: f32,
    pub color: Color32,
    pub label: &'static str,
    pub draggable: bool,
    pub curve: Option<crate::automation::Curve>,
}

/// What the time graph shows.
pub(in crate::app) struct HistoryView<'a> {
    /// The target, in tab samples: the graph's time axis.
    pub range: (usize, usize),
    /// The source's level (before the edit).
    pub levels: Option<&'a DynamicsTrace>,
    /// The render: its gain change gives the output and the reduction.
    pub result: Option<&'a DynamicsTrace>,
    pub lines: &'a [HistoryLine],
    pub playhead: Option<usize>,
    /// Shade where the gain is down (the gate's closed stretches).
    pub shade_closed: bool,
    /// The reduction that fills half the graph's height.
    pub gain_scale_db: f32,
}

/// The target over time: the input level (grey), the output the settings
/// make (blue), the gain taken (red, hanging from the top), the lines, and
/// the playhead. Click to go there; drag a line to move it.
pub(in crate::app) fn history_plot(
    ui: &mut egui::Ui,
    id: egui::Id,
    view: &HistoryView<'_>,
    size: Vec2,
) -> (PlotEdit, Option<(usize, f32)>) {
    let (resp, painter, r) = frame(ui, size);
    let mut edit = PlotEdit::default();
    let (s, e) = view.range;
    let span = e.saturating_sub(s).max(1);
    for db in [-48.0f32, -36.0, -24.0, -12.0, 0.0] {
        hline(
            &painter,
            r,
            hist_y(r, db),
            if db == 0.0 { GRID_STRONG } else { GRID },
        );
        label_at(
            &painter,
            ui,
            Pos2::new(r.left() + 2.0, hist_y(r, db) - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{db:.0}"),
            LABEL,
        );
    }
    let cols = r.width().max(1.0) as usize;
    let column_of = |trace: &DynamicsTrace, x: usize| -> Option<(f32, f32)> {
        let a = s + span * x / cols;
        let b = (s + span * (x + 1) / cols).max(a + 1);
        let hop = trace.hop.max(1);
        let i0 = a.checked_sub(trace.start).map(|v| v / hop)?;
        let i1 = (b.saturating_sub(trace.start))
            .div_ceil(hop)
            .min(trace.input_db.len());
        if i0 >= i1 {
            return None;
        }
        let input = trace.input_db[i0..i1]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        let gain = trace.gain_db[i0..i1].iter().copied().fold(0.0f32, f32::min);
        Some((input, gain))
    };
    let source = view.levels.or(view.result);
    if let Some(trace) = source {
        let mut fill = Vec::with_capacity(cols);
        for x in 0..cols {
            if let Some((input, _)) = column_of(trace, x) {
                fill.push(Pos2::new(r.left() + x as f32 + 0.5, hist_y(r, input)));
            }
        }
        fill_to(&painter, &fill, r.bottom(), INPUT_FILL);
    }
    if let Some(result) = view.result {
        let mut out_line = Vec::with_capacity(cols);
        let mut gr = Vec::with_capacity(cols);
        for x in 0..cols {
            let Some((input, gain)) = column_of(result, x) else {
                continue;
            };
            let px = r.left() + x as f32 + 0.5;
            if view.shade_closed && gain < -6.0 {
                vline(&painter, r, px, CLOSED_SHADE);
            }
            out_line.push(Pos2::new(
                px,
                hist_y(r, input + gain + result.output_offset_db),
            ));
            let depth = (-gain / view.gain_scale_db.max(1.0)).clamp(0.0, 1.0) * r.height() * 0.5;
            gr.push(Pos2::new(px, r.top() + depth));
        }
        fill_to(&painter, &gr, r.top(), GR_FILL);
        painter.add(egui::Shape::line(gr, Stroke::new(1.0, GR_LINE)));
        painter.add(egui::Shape::line(out_line, Stroke::new(1.2, POST_LINE)));
    }

    // Lines.
    let grab_id = id.with("history_line");
    let mut grab: Option<usize> = ui.data_mut(|d| d.get_temp(grab_id)).flatten();
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            grab = view
                .lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.draggable)
                .map(|(i, l)| (i, (hist_y(r, l.db) - pos.y).abs()))
                .filter(|(_, d)| *d <= 6.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i);
            edit.began = grab.is_some();
        }
    }
    let mut moved = None;
    if let (true, Some(line), Some(pos)) = (resp.dragged(), grab, resp.interact_pointer_pos()) {
        moved = Some((line, hist_db(r, pos.y)));
        edit.changed = true;
    }
    if (resp.drag_stopped() || !ui.input(|i| i.pointer.any_down())) && grab.is_some() {
        grab = None;
        edit.ended = true;
    }
    ui.data_mut(|d| d.insert_temp(grab_id, grab));
    let hover = resp.hover_pos();
    for (i, line) in view.lines.iter().enumerate() {
        if let Some(curve) = line.curve.as_ref() {
            // The setting over the target, a few pixels a step.
            let steps = (r.width() / 3.0).max(2.0) as usize;
            let at = |k: usize| s + (span as f64 * k as f64 / steps as f64) as usize;
            let pts: Vec<Pos2> = (0..=steps)
                .map(|k| {
                    let db = curve.value_at(at(k)).unwrap_or(line.db);
                    Pos2::new(
                        r.left() + k as f32 / steps as f32 * r.width(),
                        hist_y(r, db),
                    )
                })
                .collect();
            let end_db = curve.value_at(e).unwrap_or(line.db);
            painter.add(egui::Shape::line(pts, Stroke::new(1.6, line.color)));
            label_at(
                &painter,
                ui,
                Pos2::new(r.right() - 2.0, hist_y(r, end_db) - 1.0),
                egui::Align2::RIGHT_BOTTOM,
                format!("{} (curve)", line.label),
                line.color,
            );
            continue;
        }
        let y = hist_y(
            r,
            moved.filter(|(l, _)| *l == i).map_or(line.db, |(_, db)| db),
        );
        let active =
            grab == Some(i) || (line.draggable && hover.is_some_and(|h| (h.y - y).abs() <= 6.0));
        let stroke = Stroke::new(if active { 2.0 } else { 1.2 }, line.color);
        painter.add(egui::Shape::dashed_line(
            &[Pos2::new(r.left(), y), Pos2::new(r.right(), y)],
            stroke,
            5.0,
            3.0,
        ));
        label_at(
            &painter,
            ui,
            Pos2::new(r.right() - 2.0, y - 1.0),
            egui::Align2::RIGHT_BOTTOM,
            format!("{} {:.1}", line.label, hist_db(r, y)),
            line.color,
        );
    }
    if let Some(at) = view.playhead.filter(|p| *p >= s && *p <= e) {
        let x = r.left() + (at - s) as f32 / span as f32 * r.width();
        vline(&painter, r, x, PLAYHEAD);
    }
    if resp.clicked() && grab.is_none() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let t = ((pos.x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0);
            edit.seek = Some(s + (t as f64 * span as f64) as usize);
        }
    }
    if resp.hovered() {
        let near_line = hover.is_some_and(|h| {
            view.lines
                .iter()
                .any(|l| l.draggable && (hist_y(r, l.db) - h.y).abs() <= 6.0)
        });
        ui.ctx().set_cursor_icon(if near_line || grab.is_some() {
            CursorIcon::ResizeVertical
        } else {
            CursorIcon::PointingHand
        });
    }
    (edit, moved)
}

// ---------------------------------------------------------------------------
// Reverb: the impulse response

const IR_LO_DB: f32 = -90.0;

/// An impulse response as the settings use it: its envelope (dB) over time,
/// after the pre-delay (shaded), reversed when it is, with the point where
/// Length cuts it.
pub(in crate::app) fn ir_plot(
    ui: &mut egui::Ui,
    ir: &crate::impulse::ImpulseResponse,
    s: &crate::reverb::ReverbSettings,
    size: Vec2,
) {
    let (_resp, painter, r) = frame(ui, size);
    let sr = ir.buffer_sr.max(1) as f32;
    let predelay = s.predelay_ms.max(0.0) / 1000.0;
    let ir_secs = ir.frames() as f32 / sr;
    let total = (predelay + ir_secs).max(0.01);
    let x_of = |secs: f32| r.left() + (secs / total).clamp(0.0, 1.0) * r.width();
    let y_of =
        |db: f32| r.bottom() - ((db - IR_LO_DB) / -IR_LO_DB).clamp(0.0, 1.0) * r.height();
    for db in [-30.0f32, -60.0] {
        hline(&painter, r, y_of(db), GRID);
        label_at(
            &painter,
            ui,
            Pos2::new(r.left() + 2.0, y_of(db) - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{db:.0}"),
            LABEL,
        );
    }
    if predelay > 0.0 {
        painter.rect_filled(
            Rect::from_x_y_ranges(r.left()..=x_of(predelay), r.y_range()),
            0.0,
            CLOSED_SHADE,
        );
    }
    let hop_secs = ir.envelope_hop as f32 / sr;
    let points = ir.envelope_db.len();
    let env = |i: usize| {
        if s.reverse {
            ir.envelope_db[points - 1 - i]
        } else {
            ir.envelope_db[i]
        }
    };
    let curve: Vec<Pos2> = (0..points)
        .map(|i| Pos2::new(x_of(predelay + i as f32 * hop_secs), y_of(env(i))))
        .collect();
    fill_to(&painter, &curve, r.bottom(), INPUT_FILL);
    painter.add(egui::Shape::line(curve, Stroke::new(1.4, CURVE)));
    let kept = ir_secs * s.length_pct.clamp(0.0, 100.0) / 100.0;
    if kept < ir_secs {
        let x = x_of(predelay + kept);
        painter.add(egui::Shape::dashed_line(
            &[Pos2::new(x, r.top()), Pos2::new(x, r.bottom())],
            Stroke::new(1.2, GR_LINE),
            5.0,
            3.0,
        ));
        painter.rect_filled(
            Rect::from_x_y_ranges(x..=r.right(), r.y_range()),
            0.0,
            CLOSED_SHADE,
        );
    }
    label_at(
        &painter,
        ui,
        Pos2::new(r.right() - 2.0, r.top() + 1.0),
        egui::Align2::RIGHT_TOP,
        format!("{:.2} s", predelay + kept),
        LABEL,
    );
}

// ---------------------------------------------------------------------------
// Meters

const METER_LO: f32 = -60.0;
/// The reduction a full GR bar shows, unless the tool asks for more.
pub(in crate::app) const METER_GR_SCALE_DB: f32 = 24.0;

/// In / GR / Out bars for the playing audio. A full GR bar is
/// `gr_scale_db` of reduction.
pub(in crate::app) fn meters(
    ui: &mut egui::Ui,
    live: &LiveMeters,
    ceiling_db: Option<f32>,
    gr_scale_db: f32,
    size: Vec2,
) {
    let (resp, painter) = ui.allocate_painter(size, Sense::hover());
    let r = resp.rect;
    painter.rect_filled(r, 4.0, BG);
    painter.rect_stroke(r, 4.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Inside);
    let inner = r.shrink(4.0);
    let label_h = 12.0;
    let bars = Rect::from_min_max(
        inner.min,
        Pos2::new(inner.right(), inner.bottom() - label_h),
    );
    let w = bars.width() / 3.0;
    let level_y =
        |db: f32| bars.bottom() - ((db - METER_LO) / -METER_LO).clamp(0.0, 1.0) * bars.height();
    let bar = |i: usize| {
        Rect::from_min_max(
            Pos2::new(bars.left() + w * i as f32 + 2.0, bars.top()),
            Pos2::new(bars.left() + w * (i + 1) as f32 - 2.0, bars.bottom()),
        )
    };
    for (i, (name, db)) in [
        ("IN", live.in_db),
        ("GR", live.gain_db),
        ("OUT", live.out_db),
    ]
    .into_iter()
    .enumerate()
    {
        let b = bar(i);
        painter.rect_filled(b, 1.0, Color32::from_rgb(28, 31, 38));
        if i == 1 {
            let depth = (-db / gr_scale_db.max(1.0)).clamp(0.0, 1.0) * b.height();
            painter.rect_filled(
                Rect::from_min_max(b.min, Pos2::new(b.right(), b.top() + depth)),
                1.0,
                GR_LINE,
            );
        } else if db > METER_LO {
            let top = level_y(db);
            let color = if db > -1.0 {
                Color32::from_rgb(255, 90, 80)
            } else if db > -12.0 {
                Color32::from_rgb(240, 210, 90)
            } else {
                Color32::from_rgb(110, 220, 130)
            };
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(b.left(), top), b.max),
                1.0,
                color,
            );
        }
        label_at(
            &painter,
            ui,
            Pos2::new(b.center().x, inner.bottom()),
            egui::Align2::CENTER_BOTTOM,
            name.to_string(),
            LABEL,
        );
        if live.playing {
            let value = if i == 1 {
                format!("{:.1}", -db)
            } else if db > METER_LO {
                format!("{db:.0}")
            } else {
                "-".to_string()
            };
            painter.text(
                Pos2::new(b.center().x, b.top() + 1.0),
                egui::Align2::CENTER_TOP,
                value,
                egui::FontId::proportional(9.0),
                CURVE,
            );
        }
    }
    if let Some(ceiling) = ceiling_db {
        let y = level_y(ceiling);
        let b = bar(2);
        painter.line_segment(
            [Pos2::new(b.left() - 2.0, y), Pos2::new(b.right() + 2.0, y)],
            Stroke::new(1.5, GR_LINE),
        );
    }
    resp.on_hover_text("IN: the source at the playhead · GR: gain reduction · OUT: what is heard");
}
