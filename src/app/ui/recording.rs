use egui::{Color32, RichText, Stroke, Ui};

use crate::app::types::{RecordingSourceKind, RecordingState, RecordingTakeState};

const CARD_FILL: Color32 = Color32::from_rgb(24, 24, 27);
const REC_RED: Color32 = Color32::from_rgb(220, 60, 60);
const METER_GREEN: Color32 = Color32::from_rgb(80, 180, 80);
const METER_YELLOW: Color32 = Color32::from_rgb(220, 190, 70);
const METER_MIN_DB: f32 = -60.0;
const PEAK_HOLD_SECS: f32 = 1.5;

fn recording_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let inner = egui::Frame::NONE
        .fill(CARD_FILL)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(6.0);
    inner
}

fn db_from_linear(level: f32) -> f32 {
    if level <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * level.log10()
    }
}

fn meter_frac(db: f32) -> f32 {
    ((db - METER_MIN_DB) / -METER_MIN_DB).clamp(0.0, 1.0)
}

/// dB-scaled level meter: green < -12 dB, yellow -12..-3 dB, red > -3 dB,
/// with tick marks and a peak-hold line.
fn draw_db_meter(ui: &mut Ui, label: &str, level: f32, peak_hold: f32) {
    let row_h = 16.0;
    ui.horizontal(|ui| {
        ui.add_sized(
            [14.0, row_h],
            egui::Label::new(RichText::new(label).monospace().small()),
        );
        let db_text_w = 64.0;
        let bar_w = (ui.available_width() - db_text_w - 8.0).max(60.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, row_h), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, Color32::from_gray(32));

        let level_db = db_from_linear(level);
        let level_frac = meter_frac(level_db);
        let zones = [
            (METER_MIN_DB, -12.0, METER_GREEN),
            (-12.0, -3.0, METER_YELLOW),
            (-3.0, 0.0, REC_RED),
        ];
        for (z0, z1, color) in zones {
            let f0 = meter_frac(z0);
            let f1 = meter_frac(z1).min(level_frac);
            if f1 > f0 {
                let x0 = rect.left() + f0 * rect.width();
                let x1 = rect.left() + f1 * rect.width();
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x0, rect.top() + 2.0),
                        egui::pos2(x1, rect.bottom() - 2.0),
                    ),
                    2.0,
                    color,
                );
            }
        }
        for db in [-48.0, -36.0, -24.0, -12.0, -6.0, -3.0] {
            let x = rect.left() + meter_frac(db) * rect.width();
            painter.line_segment(
                [
                    egui::pos2(x, rect.bottom() - 5.0),
                    egui::pos2(x, rect.bottom() - 1.0),
                ],
                Stroke::new(1.0_f32, Color32::from_gray(75)),
            );
        }
        if peak_hold > 0.0 {
            let hold_db = db_from_linear(peak_hold);
            let x = rect.left() + meter_frac(hold_db) * rect.width();
            let color = if hold_db > -3.0 {
                REC_RED
            } else {
                Color32::from_gray(210)
            };
            painter.line_segment(
                [
                    egui::pos2(x, rect.top() + 1.0),
                    egui::pos2(x, rect.bottom() - 1.0),
                ],
                Stroke::new(2.0_f32, color),
            );
        }

        let text = if level_db.is_finite() && level_db > METER_MIN_DB {
            format!("{level_db:5.1} dB")
        } else {
            "  -∞ dB".to_string()
        };
        ui.add_sized(
            [db_text_w, row_h],
            egui::Label::new(RichText::new(text).monospace().small()),
        );
    });
}

/// Classic peak hold: track the maximum, and after `PEAK_HOLD_SECS` let it
/// drop back to the current level.
fn update_peak_hold(hold: &mut f32, hold_at: &mut Option<std::time::Instant>, level: f32) {
    let now = std::time::Instant::now();
    let expired = match *hold_at {
        None => true,
        Some(at) => now.duration_since(at).as_secs_f32() > PEAK_HOLD_SECS,
    };
    if level >= *hold || expired {
        *hold = level;
        *hold_at = Some(now);
    }
}

/// Big painter-drawn record/pause toggle: red disc with pause bars while
/// recording, ring with red dot when idle, ring with play triangle when paused.
fn record_toggle_button(ui: &mut Ui, state: &RecordingState) -> egui::Response {
    let diameter = 52.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let center = rect.center();
    let r = diameter * 0.5 - 2.0;
    match state {
        RecordingState::Recording => {
            painter.circle_filled(center, r, REC_RED);
            let (bw, bh, gap) = (5.0, 16.0, 5.0);
            for dx in [-(gap * 0.5 + bw), gap * 0.5] {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(center.x + dx, center.y - bh * 0.5),
                        egui::vec2(bw, bh),
                    ),
                    1.0,
                    Color32::WHITE,
                );
            }
        }
        RecordingState::Paused => {
            painter.circle_stroke(center, r, Stroke::new(2.0_f32, REC_RED));
            let s = 9.0;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(center.x - s * 0.6, center.y - s),
                    egui::pos2(center.x - s * 0.6, center.y + s),
                    egui::pos2(center.x + s, center.y),
                ],
                REC_RED,
                Stroke::NONE,
            ));
        }
        RecordingState::Finalizing => {
            painter.circle_stroke(center, r, Stroke::new(2.0_f32, Color32::from_gray(90)));
            painter.circle_filled(center, r * 0.42, Color32::from_gray(90));
        }
        RecordingState::Idle | RecordingState::Error(_) => {
            painter.circle_stroke(center, r, Stroke::new(2.0_f32, Color32::from_gray(130)));
            painter.circle_filled(center, r * 0.42, REC_RED);
        }
    }
    let hover = match state {
        RecordingState::Recording => "Pause",
        RecordingState::Paused => "Resume",
        RecordingState::Finalizing => "Finalizing…",
        RecordingState::Idle | RecordingState::Error(_) => "Record",
    };
    resp.on_hover_text(hover)
}

fn format_elapsed(elapsed: f32, with_tenths: bool) -> String {
    let h = (elapsed / 3600.0) as u32;
    let m = ((elapsed % 3600.0) / 60.0) as u32;
    let s = (elapsed % 60.0) as u32;
    if with_tenths {
        let tenths = ((elapsed % 1.0) * 10.0) as u32;
        format!("{h:02}:{m:02}:{s:02}.{tenths}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
}

/// Painter-drawn microphone: capsule, cradle, stem and base. A glyph would
/// depend on the fallback font having U+1F399, which the bundled one does not
/// (it rendered as a box).
fn paint_mic_icon(painter: &egui::Painter, center: egui::Pos2, size: f32, color: Color32) {
    let stroke = Stroke::new((size * 0.1).max(1.2), color);
    let body_w = size * 0.36;
    let body_h = size * 0.52;
    let body = egui::Rect::from_center_size(
        egui::pos2(center.x, center.y - size * 0.18),
        egui::vec2(body_w, body_h),
    );
    painter.rect_filled(body, body_w * 0.5, color);
    // Cradle: the lower half of a circle around the capsule's bottom.
    let r = size * 0.3;
    let cy = body.bottom() - body_w * 0.5;
    let points: Vec<egui::Pos2> = (0..=12)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 12.0);
            egui::pos2(center.x + r * a.cos(), cy + r * a.sin())
        })
        .collect();
    painter.add(egui::Shape::line(points, stroke));
    let base_y = center.y + size * 0.48;
    painter.line_segment(
        [egui::pos2(center.x, cy + r), egui::pos2(center.x, base_y)],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x - size * 0.2, base_y),
            egui::pos2(center.x + size * 0.2, base_y),
        ],
        stroke,
    );
}

/// Painter-drawn speaker with two sound waves, to match the microphone.
fn paint_speaker_icon(painter: &egui::Painter, center: egui::Pos2, size: f32, color: Color32) {
    let left = center.x - size * 0.45;
    let box_w = size * 0.2;
    let box_h = size * 0.34;
    let cone_x = left + box_w + size * 0.24;
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(left, center.y - box_h * 0.5),
            egui::pos2(left + box_w, center.y - box_h * 0.5),
            egui::pos2(cone_x, center.y - size * 0.4),
            egui::pos2(cone_x, center.y + size * 0.4),
            egui::pos2(left + box_w, center.y + box_h * 0.5),
            egui::pos2(left, center.y + box_h * 0.5),
        ],
        color,
        Stroke::NONE,
    ));
    let stroke = Stroke::new((size * 0.09).max(1.1), color);
    for r in [size * 0.2, size * 0.38] {
        let points: Vec<egui::Pos2> = (0..=10)
            .map(|i| {
                let a = -0.8 + 1.6 * (i as f32 / 10.0);
                egui::pos2(cone_x + r * a.cos(), center.y + r * a.sin())
            })
            .collect();
        painter.add(egui::Shape::line(points, stroke));
    }
}

/// A selectable source button with a painted icon in front of its label.
fn source_button(
    ui: &mut Ui,
    enabled: bool,
    selected: bool,
    label: &str,
    icon: fn(&egui::Painter, egui::Pos2, f32, Color32),
) -> egui::Response {
    // Leading spaces reserve the icon's room inside the button's own layout,
    // so its hover/selection frame covers the icon too.
    let resp = ui.add_enabled(
        enabled,
        egui::Button::selectable(selected, format!("      {label}")),
    );
    let color = if !enabled {
        ui.visuals().weak_text_color()
    } else if selected {
        ui.visuals().selection.stroke.color
    } else {
        ui.visuals().widgets.inactive.fg_stroke.color
    };
    let size = (resp.rect.height() * 0.62).min(14.0);
    let center = egui::pos2(resp.rect.left() + 6.0 + size * 0.5, resp.rect.center().y);
    icon(ui.painter(), center, size, color);
    resp
}

/// Small painted status badge for a take (no glyphs, for the same reason).
fn take_badge(ui: &mut Ui, state: &RecordingTakeState) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let c = rect.center();
    match state {
        RecordingTakeState::Recording => {
            painter.circle_filled(c, 5.0, REC_RED);
        }
        RecordingTakeState::Stopped => {
            painter.rect_filled(
                egui::Rect::from_center_size(c, egui::vec2(9.0, 9.0)),
                1.5,
                Color32::from_gray(150),
            );
        }
        RecordingTakeState::Saved(_) => {
            painter.add(egui::Shape::line(
                vec![
                    egui::pos2(c.x - 5.0, c.y),
                    egui::pos2(c.x - 1.5, c.y + 4.0),
                    egui::pos2(c.x + 5.5, c.y - 4.5),
                ],
                Stroke::new(2.0_f32, METER_GREEN),
            ));
        }
    }
}

enum TakeAction {
    Open(u64),
    SaveAs(u64),
    Discard(u64),
    Forget(u64),
}

/// Folds overview blocks into one (min, max) per pixel column of the window
/// `[start_secs, start_secs + window_secs)`, `width` columns wide. Columns no
/// block reaches are `None`.
fn waveform_columns(
    blocks: &std::collections::VecDeque<(f32, f32)>,
    first_frame: u64,
    block_frames: u64,
    sample_rate: u32,
    start_secs: f32,
    window_secs: f32,
    width: usize,
) -> Vec<Option<(f32, f32)>> {
    let mut cols: Vec<Option<(f32, f32)>> = vec![None; width];
    if width == 0 || window_secs <= 0.0 {
        return cols;
    }
    let sr = sample_rate.max(1) as f64;
    let px_per_sec = width as f64 / window_secs as f64;
    for (i, &(mn, mx)) in blocks.iter().enumerate() {
        let t = (first_frame + i as u64 * block_frames) as f64 / sr;
        let x = ((t - start_secs as f64) * px_per_sec).floor();
        if x < 0.0 || x >= width as f64 {
            continue;
        }
        let col = &mut cols[x as usize];
        *col = Some(match *col {
            Some((a, b)) => (a.min(mn), b.max(mx)),
            None => (mn, mx),
        });
    }
    cols
}

impl super::super::WavesPreviewer {
    pub(in crate::app) fn ui_recording_view(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        self.prune_recording_takes();
        let state = self.recording_tab.state.clone();
        let recording = state == RecordingState::Recording;
        let paused = state == RecordingState::Paused;
        let finalizing = state == RecordingState::Finalizing;
        let transport_locked = recording || paused || finalizing;

        // M drops a marker, unless something is taking typed text.
        if (recording || paused)
            && !ctx.egui_wants_keyboard_input()
            && ctx.input(|i| i.key_pressed(egui::Key::M) && i.modifiers.is_none())
        {
            self.add_recording_marker();
        }

        ui.heading("Recording");
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .id_salt("recording_view_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.ui_recording_source_card(ui, transport_locked);
                self.ui_recording_monitor_card(ui, &state);
                self.ui_recording_transport_card(ui, &state);
                self.ui_recording_takes_card(ui);
            });

        // ---- Discard confirmation for the take being captured ----
        if self.recording_tab.confirm_discard {
            let modal =
                egui::Modal::new(egui::Id::new("recording_discard_confirm")).show(ctx, |ui| {
                    ui.set_width(280.0);
                    ui.heading("Discard recording?");
                    ui.label("The current take will be deleted.");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.recording_tab.confirm_discard = false;
                        }
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Discard").color(Color32::WHITE))
                                    .fill(Color32::from_rgb(170, 40, 40)),
                            )
                            .clicked()
                        {
                            self.discard_recording();
                        }
                    });
                });
            if modal.should_close() {
                self.recording_tab.confirm_discard = false;
            }
        }

        // ---- Discard confirmation for a stopped take ----
        if let Some(take_id) = self.recording_tab.confirm_discard_take {
            let name = self
                .recording_tab
                .takes
                .iter()
                .find(|t| t.id == take_id)
                .map(|t| t.display_name.clone())
                .unwrap_or_default();
            let modal = egui::Modal::new(egui::Id::new("recording_take_discard_confirm")).show(
                ctx,
                |ui| {
                    ui.set_width(300.0);
                    ui.heading("Discard take?");
                    ui.label(format!(
                        "{name} and its (virtual) row in the list will be removed."
                    ));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.recording_tab.confirm_discard_take = None;
                        }
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Discard").color(Color32::WHITE))
                                    .fill(Color32::from_rgb(170, 40, 40)),
                            )
                            .clicked()
                        {
                            self.discard_recording_take(take_id);
                        }
                    });
                },
            );
            if modal.should_close() {
                self.recording_tab.confirm_discard_take = None;
            }
        }

        // Request repaint while active to animate meters/waveform/clock.
        if transport_locked {
            ctx.request_repaint_after(crate::app::ui_timing::SMOOTH_REFRESH);
        }
    }

    fn ui_recording_source_card(&mut self, ui: &mut Ui, transport_locked: bool) {
        recording_card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Source").strong());
                ui.add_space(6.0);
                let selected_mic = self.recording_tab.source == RecordingSourceKind::Microphone;
                if source_button(
                    ui,
                    !transport_locked,
                    selected_mic,
                    "Microphone",
                    paint_mic_icon,
                )
                .clicked()
                {
                    self.recording_tab.source = RecordingSourceKind::Microphone;
                }
                if cfg!(target_os = "windows") {
                    let selected_sys = self.recording_tab.source == RecordingSourceKind::System;
                    if source_button(
                        ui,
                        !transport_locked,
                        selected_sys,
                        "System Audio",
                        paint_speaker_icon,
                    )
                    .clicked()
                    {
                        self.recording_tab.source = RecordingSourceKind::System;
                    }
                    ui.add_enabled(false, egui::Button::selectable(false, "System + Mic"))
                        .on_disabled_hover_text(
                            "Not implemented yet — would record the microphone only",
                        );
                }
            });
            // A stale selection (old session state) could still point at the
            // unimplemented mixed source; snap it back to the microphone.
            if self.recording_tab.source == RecordingSourceKind::SystemAndMicrophone
                || (!cfg!(target_os = "windows")
                    && self.recording_tab.source != RecordingSourceKind::Microphone)
            {
                self.recording_tab.source = RecordingSourceKind::Microphone;
            }

            if self.recording_tab.source != RecordingSourceKind::System {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Input device");
                    ui.add_space(6.0);
                    let current = self
                        .recording_tab
                        .selected_mic_id
                        .clone()
                        .unwrap_or_default();
                    ui.add_enabled_ui(!transport_locked, |ui| {
                        egui::ComboBox::from_id_salt("rec_input_device")
                            .width(260.0)
                            .selected_text(if current.is_empty() {
                                "Default".to_string()
                            } else {
                                current.clone()
                            })
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(current.is_empty(), "Default").clicked() {
                                    self.recording_tab.selected_mic_id = None;
                                }
                                for dev in &self.recording_tab.input_devices.clone() {
                                    let sel = self.recording_tab.selected_mic_id.as_deref()
                                        == Some(&dev.id);
                                    if ui
                                        .selectable_label(
                                            sel,
                                            format!(
                                                "{}  ({})",
                                                dev.display_name,
                                                dev.format_label()
                                            ),
                                        )
                                        .clicked()
                                    {
                                        self.recording_tab.selected_mic_id = Some(dev.id.clone());
                                    }
                                }
                            });
                        if ui
                            .button("⟳")
                            .on_hover_text("Refresh device list")
                            .clicked()
                        {
                            self.recording_refresh_devices();
                        }
                    });
                    if self.recording_tab.input_devices.is_empty() {
                        ui.label(RichText::new("(no devices)").weak());
                    }
                });
            }
        });
    }

    fn ui_recording_monitor_card(&mut self, ui: &mut Ui, state: &RecordingState) {
        let recording = *state == RecordingState::Recording;
        let paused = *state == RecordingState::Paused;
        let transport_locked = recording || paused || *state == RecordingState::Finalizing;
        recording_card(ui, |ui| {
            let level_l = self.recording_tab.level_l;
            let level_r = self.recording_tab.level_r;
            {
                let tab = &mut self.recording_tab;
                update_peak_hold(&mut tab.peak_hold_l, &mut tab.peak_hold_l_at, level_l);
                update_peak_hold(&mut tab.peak_hold_r, &mut tab.peak_hold_r_at, level_r);
            }
            draw_db_meter(ui, "L", level_l, self.recording_tab.peak_hold_l);
            draw_db_meter(ui, "R", level_r, self.recording_tab.peak_hold_r);
            ui.add_space(6.0);

            self.ui_recording_waveform(ui);

            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format_elapsed(
                        self.recording_tab.elapsed_secs,
                        recording || paused,
                    ))
                    .monospace()
                    .size(30.0)
                    .color(if recording {
                        Color32::from_gray(235)
                    } else {
                        Color32::from_gray(170)
                    }),
                );
            });

            // Status / warnings
            let overruns = self
                .recording_tab
                .overrun_count
                .load(std::sync::atomic::Ordering::Relaxed);
            if overruns > 0 && transport_locked {
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(format!("Input overrun — {overruns} buffer(s) dropped"))
                            .color(ui.style().visuals.warn_fg_color)
                            .small(),
                    );
                });
            }
            if !self.recording_tab.progress_message.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(self.recording_tab.progress_message.clone())
                            .weak()
                            .small(),
                    );
                });
            }
            if let RecordingState::Error(msg) = state {
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(format!("Error: {msg}"))
                            .color(ui.style().visuals.error_fg_color),
                    );
                });
            }
        });
    }

    fn ui_recording_transport_card(&mut self, ui: &mut Ui, state: &RecordingState) {
        let capturing = matches!(state, RecordingState::Recording | RecordingState::Paused);
        recording_card(ui, |ui| {
            ui.horizontal(|ui| {
                let toggle = record_toggle_button(ui, state);
                if toggle.clicked() {
                    match state {
                        RecordingState::Recording => self.pause_recording(),
                        RecordingState::Paused => self.resume_recording(),
                        RecordingState::Idle | RecordingState::Error(_) => self.start_recording(),
                        RecordingState::Finalizing => {}
                    }
                }
                ui.add_space(10.0);
                if ui
                    .add_enabled(
                        capturing,
                        egui::Button::new("■ Stop").min_size(egui::vec2(90.0, 32.0)),
                    )
                    .on_hover_text("Stop and keep the take")
                    .clicked()
                {
                    self.stop_recording();
                }
                if ui
                    .add_enabled(
                        capturing,
                        egui::Button::new("Add Marker").min_size(egui::vec2(110.0, 32.0)),
                    )
                    .on_hover_text("Mark this moment of the take (M)")
                    .clicked()
                {
                    self.add_recording_marker();
                }
                if ui
                    .add_enabled(
                        capturing,
                        // U+00D7, not U+2715: only the system faces carry the
                        // heavier X, so it renders as a box until the async
                        // font upgrade lands.
                        egui::Button::new("× Discard").min_size(egui::vec2(90.0, 32.0)),
                    )
                    .on_hover_text("Throw away the current take")
                    .clicked()
                {
                    self.recording_tab.confirm_discard = true;
                }
            });
        });
    }

    fn ui_recording_takes_card(&mut self, ui: &mut Ui) {
        if self.recording_tab.takes.is_empty() {
            return;
        }
        struct Row {
            id: u64,
            name: String,
            state: RecordingTakeState,
            secs: f32,
            markers: usize,
        }
        let live_markers = self
            .recording_tab
            .live_markers
            .lock()
            .map(|m| m.len())
            .unwrap_or(0);
        // Newest first: the take just recorded is the one being acted on.
        let rows: Vec<Row> = self
            .recording_tab
            .takes
            .iter()
            .rev()
            .filter_map(|take| {
                let state = self.recording_take_state(take)?;
                let capturing = state == RecordingTakeState::Recording;
                Some(Row {
                    id: take.id,
                    name: take.display_name.clone(),
                    secs: if capturing {
                        self.recording_tab.elapsed_secs
                    } else {
                        take.frames as f32 / take.sample_rate.max(1) as f32
                    },
                    markers: if capturing {
                        live_markers
                    } else {
                        take.markers.len()
                    },
                    state,
                })
            })
            .collect();
        let mut action: Option<TakeAction> = None;
        recording_card(ui, |ui| {
            ui.label(RichText::new("Takes").strong());
            ui.add_space(4.0);
            egui::Grid::new("recording_takes_grid")
                .num_columns(6)
                .spacing(egui::vec2(14.0, 6.0))
                .striped(true)
                .show(ui, |ui| {
                    for row in &rows {
                        ui.horizontal(|ui| {
                            take_badge(ui, &row.state);
                            ui.label(match &row.state {
                                RecordingTakeState::Recording => "Recording",
                                RecordingTakeState::Stopped => "Stopped",
                                RecordingTakeState::Saved(_) => "Saved",
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&row.name).monospace());
                            if row.state == RecordingTakeState::Stopped {
                                ui.label(RichText::new("(virtual)").weak());
                            }
                        });
                        ui.label(RichText::new(format_elapsed(row.secs, true)).monospace());
                        ui.label(if row.markers == 0 {
                            RichText::new("no markers").weak()
                        } else {
                            RichText::new(format!("{} marker(s)", row.markers))
                        });
                        match &row.state {
                            RecordingTakeState::Saved(path) => {
                                let shown = path.display().to_string();
                                ui.label(RichText::new(&shown).weak().small())
                                    .on_hover_text(shown);
                            }
                            _ => {
                                ui.label("");
                            }
                        }
                        ui.horizontal(|ui| match &row.state {
                            RecordingTakeState::Recording => {}
                            RecordingTakeState::Stopped => {
                                if ui.button("Open in Editor").clicked() {
                                    action = Some(TakeAction::Open(row.id));
                                }
                                if ui
                                    .button("Save As…")
                                    .on_hover_text(
                                        "Save to a file; the list row becomes that file",
                                    )
                                    .clicked()
                                {
                                    action = Some(TakeAction::SaveAs(row.id));
                                }
                                if ui
                                    .button("Discard")
                                    .on_hover_text("Delete this take and its list row")
                                    .clicked()
                                {
                                    action = Some(TakeAction::Discard(row.id));
                                }
                            }
                            RecordingTakeState::Saved(_) => {
                                if ui.button("Open in Editor").clicked() {
                                    action = Some(TakeAction::Open(row.id));
                                }
                                if ui
                                    .button("Remove from takes")
                                    .on_hover_text(
                                        "Only hides it here; the file and its list row stay",
                                    )
                                    .clicked()
                                {
                                    action = Some(TakeAction::Forget(row.id));
                                }
                            }
                        });
                        ui.end_row();
                    }
                });
        });
        match action {
            Some(TakeAction::Open(id)) => self.open_recording_take_in_editor(id),
            Some(TakeAction::SaveAs(id)) => self.save_recording_take_as(id),
            Some(TakeAction::Discard(id)) => self.recording_tab.confirm_discard_take = Some(id),
            Some(TakeAction::Forget(id)) => self.recording_tab.takes.retain(|t| t.id != id),
            None => {}
        }
    }

    fn ui_recording_waveform(&mut self, ui: &mut Ui) {
        let desired = egui::vec2(ui.available_width(), 140.0);
        let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_gray(20));
        let mid = rect.center().y;

        // Zero line
        painter.line_segment(
            [egui::pos2(rect.left(), mid), egui::pos2(rect.right(), mid)],
            Stroke::new(1.0_f32, Color32::from_gray(70)),
        );

        let overview = &self.recording_tab.waveform_overview;
        if overview.is_empty() {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "no signal yet",
                egui::FontId::monospace(11.0),
                Color32::from_gray(90),
            );
            return;
        }

        let sr = self.recording_tab.recording_sample_rate.max(1);
        let window = crate::app::recording_ops::LIVE_WAVEFORM_WINDOW_SECS;
        let block_frames = crate::app::recording_ops::live_waveform_block_frames(sr) as u64;
        let now_secs = self.recording_tab.written_frames as f32 / sr as f32;
        // Fixed-length window: the take grows in from the left, and once it
        // is longer than the window it scrolls with "now" at the right edge.
        let start_secs = (now_secs - window).max(0.0);
        let w = rect.width();
        let h = rect.height();
        let x_of = |t: f32| rect.left() + (t - start_secs) / window * w;

        // Time grid
        let step = recording_grid_time_step(window);
        if step > 0.0 {
            let mut t = (start_secs / step).ceil() * step;
            while t <= start_secs + window + 0.0001 {
                let x = x_of(t);
                painter.line_segment(
                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                    Stroke::new(1.0_f32, Color32::from_gray(45)),
                );
                painter.text(
                    egui::pos2(x + 2.0, rect.top() + 2.0),
                    egui::Align2::LEFT_TOP,
                    crate::app::helpers::format_time_s(t),
                    egui::FontId::monospace(10.0),
                    Color32::from_gray(150),
                );
                t += step;
            }
        }

        // Waveform: one (min, max) per physical pixel column, clipping in red.
        let ppp = ui.ctx().pixels_per_point();
        let columns = (w * ppp).round().max(1.0) as usize;
        let cols = waveform_columns(
            overview,
            self.recording_tab.waveform_start_frame,
            block_frames,
            sr,
            start_secs,
            window,
            columns,
        );
        let col_w = w / columns as f32;
        for (i, col) in cols.iter().enumerate() {
            let Some((mn, mx)) = *col else {
                continue;
            };
            let x = rect.left() + (i as f32 + 0.5) * col_w;
            let mut y_top = mid - mx.clamp(-1.0, 1.0) * h * 0.5;
            let mut y_bot = mid - mn.clamp(-1.0, 1.0) * h * 0.5;
            if y_bot - y_top < 1.0 {
                // Keep near-silence visible as a hairline.
                let c = (y_top + y_bot) * 0.5;
                y_top = c - 0.5;
                y_bot = c + 0.5;
            }
            let clipping = mn.abs() >= 0.98 || mx.abs() >= 0.98;
            let color = if clipping { REC_RED } else { METER_GREEN };
            painter.line_segment(
                [egui::pos2(x, y_top), egui::pos2(x, y_bot)],
                Stroke::new(col_w.max(1.0 / ppp), color),
            );
        }

        // Markers dropped on the take being captured.
        let markers = self
            .recording_tab
            .live_markers
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        let marker_col = Color32::from_rgb(255, 196, 72);
        for marker in &markers {
            let t = marker.sample as f32 / sr as f32;
            if t < start_secs || t > start_secs + window {
                continue;
            }
            let x = x_of(t);
            painter.line_segment(
                [egui::pos2(x, rect.top() + 14.0), egui::pos2(x, rect.bottom())],
                Stroke::new(1.5_f32, marker_col),
            );
            painter.text(
                egui::pos2(x + 3.0, rect.bottom() - 2.0),
                egui::Align2::LEFT_BOTTOM,
                &marker.label,
                egui::FontId::monospace(10.0),
                marker_col,
            );
        }

        // "Now"
        let now_x = x_of(now_secs).min(rect.right() - 1.0);
        painter.line_segment(
            [egui::pos2(now_x, rect.top()), egui::pos2(now_x, rect.bottom())],
            Stroke::new(1.5_f32, Color32::from_rgb(230, 230, 120)),
        );
    }
}

/// Picks a "nice" gridline interval (seconds) so the visible window shows roughly
/// 4-6 labelled ticks regardless of recording length.
fn recording_grid_time_step(span_secs: f32) -> f32 {
    const STEPS: [f32; 9] = [0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0];
    if !span_secs.is_finite() || span_secs <= 0.0 {
        return 0.0;
    }
    let target_lines = 6.0;
    for &step in &STEPS {
        if span_secs / step <= target_lines {
            return step;
        }
    }
    300.0
}

#[cfg(test)]
mod tests {
    use super::{format_elapsed, meter_frac, recording_grid_time_step, waveform_columns};

    #[test]
    fn waveform_columns_fold_blocks_per_pixel_and_skip_outside_window() {
        // 400 blocks/s at 48 kHz, 2 s of blocks starting at t=1 s.
        let blocks: std::collections::VecDeque<(f32, f32)> =
            (0..800).map(|i| (-(i as f32) / 800.0, i as f32 / 800.0)).collect();
        // Window 0..10 s over 100 columns: 10 px/s, 40 blocks per column.
        let cols = waveform_columns(&blocks, 48_000, 120, 48_000, 0.0, 10.0, 100);
        assert_eq!(cols.len(), 100);
        assert!(cols[..10].iter().all(Option::is_none), "nothing before 1 s");
        assert!(cols[10..30].iter().all(Option::is_some));
        assert!(cols[30..].iter().all(Option::is_none), "nothing after 3 s");
        // Column 10 holds blocks 0..40: max of those is 39/800.
        let (mn, mx) = cols[10].unwrap();
        assert!((mx - 39.0 / 800.0).abs() < 1e-6);
        assert!((mn + 39.0 / 800.0).abs() < 1e-6);

        // Scrolled so the window starts mid-take: earlier blocks drop out.
        let cols = waveform_columns(&blocks, 48_000, 120, 48_000, 2.0, 10.0, 100);
        assert!(cols[0].is_some() && cols[9].is_some() && cols[10].is_none());
    }

    #[test]
    fn returns_zero_for_non_positive_or_non_finite_span() {
        assert_eq!(recording_grid_time_step(0.0), 0.0);
        assert_eq!(recording_grid_time_step(-1.0), 0.0);
        assert_eq!(recording_grid_time_step(f32::NAN), 0.0);
        assert_eq!(recording_grid_time_step(f32::INFINITY), 0.0);
    }

    #[test]
    fn picks_smallest_step_keeping_at_most_six_grid_lines() {
        // span / step <= 6.0 must hold whenever a wide-enough step exists in the
        // table (largest step * 6 = 720s); beyond that the function falls back
        // to the widest step regardless (covered separately below).
        for span in [0.6, 1.5, 4.0, 9.0, 27.0, 58.0, 119.0, 400.0, 700.0] {
            let step = recording_grid_time_step(span);
            assert!(step > 0.0, "expected a positive step for span={span}");
            assert!(
                span / step <= 6.0,
                "span={span} step={step} should keep <= 6 grid lines (ratio={})",
                span / step
            );
        }
    }

    #[test]
    fn step_grows_monotonically_with_span() {
        let spans = [1.0, 5.0, 15.0, 45.0, 90.0, 200.0, 1000.0];
        let mut last = 0.0_f32;
        for span in spans {
            let step = recording_grid_time_step(span);
            assert!(
                step >= last,
                "step should not shrink as span grows: span={span} step={step} last={last}"
            );
            last = step;
        }
    }

    #[test]
    fn falls_back_to_widest_step_for_very_long_spans() {
        assert_eq!(recording_grid_time_step(100_000.0), 300.0);
    }

    #[test]
    fn meter_frac_clamps_to_unit_range() {
        assert_eq!(meter_frac(-120.0), 0.0);
        assert_eq!(meter_frac(0.0), 1.0);
        assert!(meter_frac(-30.0) > 0.0 && meter_frac(-30.0) < 1.0);
    }

    #[test]
    fn elapsed_formatting_includes_tenths_only_when_asked() {
        assert_eq!(format_elapsed(3661.25, false), "01:01:01");
        assert_eq!(format_elapsed(3661.25, true), "01:01:01.2");
    }
}
