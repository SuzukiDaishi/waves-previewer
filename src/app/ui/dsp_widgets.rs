//! Interactive plot widgets for the DSP tools (EQ / Compressor / Noise
//! Gate), shared by the Editor Inspector and the Effect Graph node UIs.
//!
//! Each widget draws a small parameter plot with draggable handles and
//! returns `true` when the user changed a parameter through it. Numeric
//! DragValues stay available next to the plots for exact entry — the plots
//! are the "grab it and shape it" surface.

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, Vec2};

use crate::wave::{CompressorParams, NoiseGateParams, ThreeBandEqParams};

const PLOT_BG: Color32 = Color32::from_rgb(24, 26, 30);
const PLOT_GRID: Color32 = Color32::from_rgb(52, 56, 64);
const PLOT_GRID_ZERO: Color32 = Color32::from_rgb(84, 90, 100);
const PLOT_CURVE: Color32 = Color32::from_rgb(120, 200, 255);
const PLOT_REFERENCE: Color32 = Color32::from_rgb(70, 76, 86);
const HANDLE_LOW: Color32 = Color32::from_rgb(255, 170, 60);
const HANDLE_MID: Color32 = Color32::from_rgb(120, 220, 160);
const HANDLE_HIGH: Color32 = Color32::from_rgb(220, 140, 255);
const HANDLE_HIT_RADIUS: f32 = 10.0;

const EQ_FREQ_MIN: f32 = 20.0;
const EQ_FREQ_MAX: f32 = 20_000.0;
const EQ_DB_RANGE: f32 = 24.0;

fn plot_frame(ui: &mut egui::Ui, height: f32) -> (egui::Response, egui::Painter, Rect) {
    let width = ui.available_width().max(120.0);
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, height), Sense::click_and_drag());
    let rect = resp.rect;
    painter.rect_filled(rect, 4.0, PLOT_BG);
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, PLOT_GRID),
        egui::StrokeKind::Inside,
    );
    (resp, painter, rect)
}

fn freq_to_x(rect: Rect, hz: f32) -> f32 {
    let t = (hz.clamp(EQ_FREQ_MIN, EQ_FREQ_MAX) / EQ_FREQ_MIN).ln()
        / (EQ_FREQ_MAX / EQ_FREQ_MIN).ln();
    rect.left() + t * rect.width()
}

fn x_to_freq(rect: Rect, x: f32) -> f32 {
    let t = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    EQ_FREQ_MIN * (EQ_FREQ_MAX / EQ_FREQ_MIN).powf(t)
}

fn db_to_y(rect: Rect, db: f32, range: f32) -> f32 {
    let t = ((db + range) / (2.0 * range)).clamp(0.0, 1.0);
    rect.bottom() - t * rect.height()
}

fn y_to_db(rect: Rect, y: f32, range: f32) -> f32 {
    let t = ((rect.bottom() - y) / rect.height().max(1.0)).clamp(0.0, 1.0);
    -range + t * 2.0 * range
}

fn handle_label(painter: &egui::Painter, ui: &egui::Ui, rect: Rect, pos: Pos2, text: String) {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let align = if pos.x > rect.center().x {
        egui::Align2::RIGHT_BOTTOM
    } else {
        egui::Align2::LEFT_BOTTOM
    };
    let offset = if pos.x > rect.center().x {
        Vec2::new(-8.0, -6.0)
    } else {
        Vec2::new(8.0, -6.0)
    };
    painter.text(
        pos + offset,
        align,
        text,
        font,
        Color32::from_rgb(220, 224, 230),
    );
}

fn draw_handle(painter: &egui::Painter, pos: Pos2, color: Color32, active: bool) {
    let r = if active { 6.0 } else { 4.5 };
    painter.circle_filled(pos, r, color);
    painter.circle_stroke(pos, r, Stroke::new(1.5, Color32::from_rgb(20, 20, 24)));
}

/// Which EQ handle a drag is grabbing (persisted in egui temp memory keyed
/// by the plot's id so the grab survives across frames).
#[derive(Clone, Copy, PartialEq, Eq)]
enum EqHandle {
    Low,
    Mid,
    High,
}

/// Interactive 3-band EQ response plot. Drag the colored handles: horizontal
/// = frequency, vertical = gain. Scroll over the mid handle to adjust Q.
/// Returns true when a parameter changed.
pub(crate) fn eq_response_plot(
    ui: &mut egui::Ui,
    id: egui::Id,
    params: &mut ThreeBandEqParams,
    sample_rate: u32,
) -> bool {
    let (resp, painter, rect) = plot_frame(ui, 150.0);
    let inner = rect.shrink(6.0);
    // Grid: frequency decades + dB lines.
    for hz in [100.0f32, 1_000.0, 10_000.0] {
        let x = freq_to_x(inner, hz);
        painter.line_segment(
            [Pos2::new(x, inner.top()), Pos2::new(x, inner.bottom())],
            Stroke::new(1.0, PLOT_GRID),
        );
        let font = egui::TextStyle::Small.resolve(ui.style());
        let label = if hz >= 1000.0 {
            format!("{}k", hz as u32 / 1000)
        } else {
            format!("{}", hz as u32)
        };
        painter.text(
            Pos2::new(x + 2.0, inner.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            font,
            PLOT_GRID_ZERO,
        );
    }
    for db in [-12.0f32, 0.0, 12.0] {
        let y = db_to_y(inner, db, EQ_DB_RANGE);
        let color = if db == 0.0 { PLOT_GRID_ZERO } else { PLOT_GRID };
        painter.line_segment(
            [Pos2::new(inner.left(), y), Pos2::new(inner.right(), y)],
            Stroke::new(1.0, color),
        );
    }
    // Response curve.
    let steps = 128;
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let hz = EQ_FREQ_MIN * (EQ_FREQ_MAX / EQ_FREQ_MIN).powf(t);
        let db = crate::wave::three_band_eq_response_db(params, sample_rate, hz);
        pts.push(Pos2::new(
            inner.left() + t * inner.width(),
            db_to_y(inner, db, EQ_DB_RANGE),
        ));
    }
    painter.add(egui::Shape::line(pts, Stroke::new(2.0, PLOT_CURVE)));

    // Handles at each band's (freq, gain).
    let handles = [
        (
            EqHandle::Low,
            HANDLE_LOW,
            params.low_shelf_freq_hz,
            params.low_shelf_gain_db,
        ),
        (
            EqHandle::Mid,
            HANDLE_MID,
            params.mid_freq_hz,
            params.mid_gain_db,
        ),
        (
            EqHandle::High,
            HANDLE_HIGH,
            params.high_shelf_freq_hz,
            params.high_shelf_gain_db,
        ),
    ];
    let drag_id = id.with("eq_drag");
    let mut dragging: Option<EqHandle> = ui.data_mut(|d| d.get_temp(drag_id)).flatten();
    let hover = resp.hover_pos();
    let mut changed = false;

    if resp.drag_started() && dragging.is_none() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let mut best: Option<(EqHandle, f32)> = None;
            for (h, _, hz, db) in handles {
                let hp = Pos2::new(freq_to_x(inner, hz), db_to_y(inner, db, EQ_DB_RANGE));
                let d = hp.distance(pos);
                if d <= HANDLE_HIT_RADIUS && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                    best = Some((h, d));
                }
            }
            dragging = best.map(|(h, _)| h);
        }
    }
    if resp.dragged() {
        if let (Some(handle), Some(pos)) = (dragging, resp.interact_pointer_pos()) {
            let hz = x_to_freq(inner, pos.x);
            let db = y_to_db(inner, pos.y, EQ_DB_RANGE).clamp(-EQ_DB_RANGE, EQ_DB_RANGE);
            match handle {
                EqHandle::Low => {
                    params.low_shelf_freq_hz = hz.clamp(20.0, 2_000.0);
                    params.low_shelf_gain_db = db;
                }
                EqHandle::Mid => {
                    params.mid_freq_hz = hz.clamp(50.0, 12_000.0);
                    params.mid_gain_db = db;
                }
                EqHandle::High => {
                    params.high_shelf_freq_hz = hz.clamp(500.0, 20_000.0);
                    params.high_shelf_gain_db = db;
                }
            }
            changed = true;
        }
    }
    if !ui.input(|i| i.pointer.primary_down()) {
        dragging = None;
    }
    // Scroll over the mid handle adjusts Q.
    if let Some(pos) = hover {
        let mid_pos = Pos2::new(
            freq_to_x(inner, params.mid_freq_hz),
            db_to_y(inner, params.mid_gain_db, EQ_DB_RANGE),
        );
        if mid_pos.distance(pos) <= HANDLE_HIT_RADIUS * 1.5 {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                params.mid_q = (params.mid_q * (1.0 + scroll.signum() * 0.1)).clamp(0.1, 10.0);
                changed = true;
            }
        }
    }
    ui.data_mut(|d| d.insert_temp(drag_id, dragging));

    for (h, color, hz, db) in [
        (
            EqHandle::Low,
            HANDLE_LOW,
            params.low_shelf_freq_hz,
            params.low_shelf_gain_db,
        ),
        (
            EqHandle::Mid,
            HANDLE_MID,
            params.mid_freq_hz,
            params.mid_gain_db,
        ),
        (
            EqHandle::High,
            HANDLE_HIGH,
            params.high_shelf_freq_hz,
            params.high_shelf_gain_db,
        ),
    ] {
        let hp = Pos2::new(freq_to_x(inner, hz), db_to_y(inner, db, EQ_DB_RANGE));
        let active = dragging == Some(h)
            || hover.map(|p| hp.distance(p) <= HANDLE_HIT_RADIUS).unwrap_or(false);
        draw_handle(&painter, hp, color, active);
        if active {
            let text = if h == EqHandle::Mid {
                format!("{:.0} Hz {:+.1} dB Q {:.2}", hz, db, params.mid_q)
            } else {
                format!("{hz:.0} Hz {db:+.1} dB")
            };
            handle_label(&painter, ui, inner, hp, text);
        }
    }
    if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = CursorIcon::Crosshair);
    }
    resp.on_hover_text("Drag a handle: horizontal = frequency, vertical = gain. Scroll on the green mid handle to change Q.");
    changed
}

const DYN_DB_MIN: f32 = -60.0;

fn dyn_to_x(rect: Rect, db: f32) -> f32 {
    let t = ((db - DYN_DB_MIN) / -DYN_DB_MIN).clamp(0.0, 1.0);
    rect.left() + t * rect.width()
}

fn dyn_x_to_db(rect: Rect, x: f32) -> f32 {
    let t = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    DYN_DB_MIN + t * -DYN_DB_MIN
}

fn dyn_to_y(rect: Rect, db: f32) -> f32 {
    let t = ((db - DYN_DB_MIN) / -DYN_DB_MIN).clamp(0.0, 1.0);
    rect.bottom() - t * rect.height()
}

fn dyn_grid(ui: &egui::Ui, painter: &egui::Painter, inner: Rect) {
    let font = egui::TextStyle::Small.resolve(ui.style());
    for db in [-48.0f32, -36.0, -24.0, -12.0] {
        let x = dyn_to_x(inner, db);
        painter.line_segment(
            [Pos2::new(x, inner.top()), Pos2::new(x, inner.bottom())],
            Stroke::new(1.0, PLOT_GRID),
        );
        let y = dyn_to_y(inner, db);
        painter.line_segment(
            [Pos2::new(inner.left(), y), Pos2::new(inner.right(), y)],
            Stroke::new(1.0, PLOT_GRID),
        );
        painter.text(
            Pos2::new(x + 2.0, inner.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{db:.0}"),
            font.clone(),
            PLOT_GRID_ZERO,
        );
    }
    // Unity (1:1) reference diagonal.
    painter.line_segment(
        [
            Pos2::new(dyn_to_x(inner, DYN_DB_MIN), dyn_to_y(inner, DYN_DB_MIN)),
            Pos2::new(dyn_to_x(inner, 0.0), dyn_to_y(inner, 0.0)),
        ],
        Stroke::new(1.0, PLOT_REFERENCE),
    );
}

/// Static compressor transfer curve (input dB -> output dB) with draggable
/// handles: the knee point sets the threshold (drag horizontally), the top
/// endpoint sets the ratio (drag vertically). Returns true on change.
pub(crate) fn compressor_transfer_plot(
    ui: &mut egui::Ui,
    id: egui::Id,
    params: &mut CompressorParams,
) -> bool {
    let (resp, painter, rect) = plot_frame(ui, 150.0);
    let inner = rect.shrink(6.0);
    dyn_grid(ui, &painter, inner);

    let out_db = |in_db: f32, p: &CompressorParams| -> f32 {
        let over = in_db - p.threshold_db;
        let compressed = if over > 0.0 {
            p.threshold_db + over / p.ratio.max(1.0)
        } else {
            in_db
        };
        compressed + p.makeup_db
    };

    // Transfer curve.
    let steps = 96;
    let mut pts = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let in_db = DYN_DB_MIN + (i as f32 / steps as f32) * -DYN_DB_MIN;
        pts.push(Pos2::new(
            dyn_to_x(inner, in_db),
            dyn_to_y(inner, out_db(in_db, params).clamp(DYN_DB_MIN, 0.0)),
        ));
    }
    painter.add(egui::Shape::line(pts, Stroke::new(2.0, PLOT_CURVE)));

    // Handles: knee (threshold) and ceiling endpoint (ratio).
    let knee = Pos2::new(
        dyn_to_x(inner, params.threshold_db),
        dyn_to_y(inner, out_db(params.threshold_db, params).clamp(DYN_DB_MIN, 0.0)),
    );
    let top = Pos2::new(
        dyn_to_x(inner, 0.0),
        dyn_to_y(inner, out_db(0.0, params).clamp(DYN_DB_MIN, 0.0)),
    );
    let drag_id = id.with("comp_drag");
    let mut dragging: Option<u8> = ui.data_mut(|d| d.get_temp(drag_id)).flatten();
    let hover = resp.hover_pos();
    let mut changed = false;
    if resp.drag_started() && dragging.is_none() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let dk = knee.distance(pos);
            let dt = top.distance(pos);
            if dk <= HANDLE_HIT_RADIUS && dk <= dt {
                dragging = Some(0);
            } else if dt <= HANDLE_HIT_RADIUS {
                dragging = Some(1);
            }
        }
    }
    if resp.dragged() {
        if let (Some(handle), Some(pos)) = (dragging, resp.interact_pointer_pos()) {
            match handle {
                0 => {
                    params.threshold_db = dyn_x_to_db(inner, pos.x).clamp(-60.0, 0.0);
                    changed = true;
                }
                _ => {
                    // Top endpoint: out(0) = thr - thr/ratio + makeup.
                    // Solve ratio from the dragged output level.
                    let target_out =
                        (DYN_DB_MIN + ((inner.bottom() - pos.y) / inner.height().max(1.0)) * -DYN_DB_MIN)
                            .clamp(DYN_DB_MIN, 0.0);
                    let reduced = target_out - params.makeup_db - params.threshold_db;
                    let over = -params.threshold_db;
                    if over > 0.5 {
                        let ratio = over / reduced.max(over / 20.0);
                        params.ratio = ratio.clamp(1.0, 20.0);
                        changed = true;
                    }
                }
            }
        }
    }
    if !ui.input(|i| i.pointer.primary_down()) {
        dragging = None;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, dragging));

    let knee_active = dragging == Some(0)
        || hover.map(|p| knee.distance(p) <= HANDLE_HIT_RADIUS).unwrap_or(false);
    let top_active = dragging == Some(1)
        || hover.map(|p| top.distance(p) <= HANDLE_HIT_RADIUS).unwrap_or(false);
    draw_handle(&painter, knee, HANDLE_LOW, knee_active);
    draw_handle(&painter, top, HANDLE_MID, top_active);
    if knee_active {
        handle_label(
            &painter,
            ui,
            inner,
            knee,
            format!("Thr {:.1} dB", params.threshold_db),
        );
    }
    if top_active {
        handle_label(&painter, ui, inner, top, format!("{:.1}:1", params.ratio));
    }
    if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = CursorIcon::Crosshair);
    }
    resp.on_hover_text(
        "Static transfer curve (in dB -> out dB). Drag the orange knee horizontally to set the threshold; drag the green top point vertically to set the ratio.",
    );
    changed
}

/// Static gate transfer curve: unity above the threshold, dropping to
/// silence below it. Drag the handle horizontally to set the threshold.
pub(crate) fn noise_gate_plot(
    ui: &mut egui::Ui,
    id: egui::Id,
    params: &mut NoiseGateParams,
) -> bool {
    let (resp, painter, rect) = plot_frame(ui, 120.0);
    let inner = rect.shrink(6.0);
    dyn_grid(ui, &painter, inner);

    let thr = params.threshold_db.clamp(DYN_DB_MIN, 0.0);
    let thr_x = dyn_to_x(inner, thr);
    // Closed region shading below the threshold.
    painter.rect_filled(
        Rect::from_min_max(
            Pos2::new(inner.left(), inner.top()),
            Pos2::new(thr_x, inner.bottom()),
        ),
        0.0,
        Color32::from_rgba_unmultiplied(200, 80, 80, 26),
    );
    // Transfer: floor below threshold, unity above.
    let floor_y = dyn_to_y(inner, DYN_DB_MIN);
    let mut pts = vec![
        Pos2::new(inner.left(), floor_y),
        Pos2::new(thr_x, floor_y),
        Pos2::new(thr_x, dyn_to_y(inner, thr)),
        Pos2::new(dyn_to_x(inner, 0.0), dyn_to_y(inner, 0.0)),
    ];
    painter.add(egui::Shape::line(std::mem::take(&mut pts), Stroke::new(2.0, PLOT_CURVE)));

    let handle = Pos2::new(thr_x, dyn_to_y(inner, thr));
    let drag_id = id.with("gate_drag");
    let mut dragging: bool = ui
        .data_mut(|d| d.get_temp(drag_id))
        .unwrap_or(false);
    let hover = resp.hover_pos();
    let mut changed = false;
    if resp.drag_started() && !dragging {
        if let Some(pos) = resp.interact_pointer_pos() {
            if handle.distance(pos) <= HANDLE_HIT_RADIUS || (pos.x - thr_x).abs() <= 6.0 {
                dragging = true;
            }
        }
    }
    if resp.dragged() && dragging {
        if let Some(pos) = resp.interact_pointer_pos() {
            params.threshold_db = dyn_x_to_db(inner, pos.x).clamp(-80.0, 0.0);
            changed = true;
        }
    }
    if !ui.input(|i| i.pointer.primary_down()) {
        dragging = false;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, dragging));

    let active = dragging
        || hover.map(|p| handle.distance(p) <= HANDLE_HIT_RADIUS).unwrap_or(false);
    draw_handle(&painter, handle, HANDLE_LOW, active);
    if active {
        handle_label(
            &painter,
            ui,
            inner,
            handle,
            format!("Gate {:.1} dB", params.threshold_db),
        );
    }
    if resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = CursorIcon::Crosshair);
    }
    resp.on_hover_text(
        "Gate transfer curve: audio below the threshold (shaded) is silenced. Drag the handle to set the threshold.",
    );
    changed
}
