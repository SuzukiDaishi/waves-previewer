use egui::{Color32, RichText, Stroke, Ui};

use crate::app::types::{RecordingSourceKind, RecordingState};

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
                Stroke::new(1.0, Color32::from_gray(75)),
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
                Stroke::new(2.0, color),
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
            painter.circle_stroke(center, r, Stroke::new(2.0, REC_RED));
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
            painter.circle_stroke(center, r, Stroke::new(2.0, Color32::from_gray(90)));
            painter.circle_filled(center, r * 0.42, Color32::from_gray(90));
        }
        RecordingState::Idle | RecordingState::Error(_) => {
            painter.circle_stroke(center, r, Stroke::new(2.0, Color32::from_gray(130)));
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

impl super::super::WavesPreviewer {
    pub(in crate::app) fn ui_recording_view(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let state = self.recording_tab.state.clone();
        let idle = matches!(state, RecordingState::Idle | RecordingState::Error(_));
        let recording = state == RecordingState::Recording;
        let paused = state == RecordingState::Paused;
        let finalizing = state == RecordingState::Finalizing;
        let transport_locked = recording || paused || finalizing;

        ui.heading("Recording");
        ui.add_space(4.0);

        // ---- Source / device card ----
        recording_card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Source").strong());
                ui.add_space(6.0);
                let selected_mic = self.recording_tab.source == RecordingSourceKind::Microphone;
                if ui
                    .add_enabled(
                        !transport_locked,
                        egui::Button::selectable(selected_mic, "🎙 Microphone"),
                    )
                    .clicked()
                {
                    self.recording_tab.source = RecordingSourceKind::Microphone;
                }
                if cfg!(target_os = "windows") {
                    let selected_sys = self.recording_tab.source == RecordingSourceKind::System;
                    if ui
                        .add_enabled(
                            !transport_locked,
                            egui::Button::selectable(selected_sys, "🔊 System Audio"),
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
                                    if ui.selectable_label(sel, &dev.display_name).clicked() {
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

        // ---- Monitor card: meters, waveform, elapsed time ----
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
            if let RecordingState::Error(msg) = &state {
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(format!("Error: {msg}"))
                            .color(ui.style().visuals.error_fg_color),
                    );
                });
            }
        });

        // ---- Transport card ----
        recording_card(ui, |ui| {
            ui.horizontal(|ui| {
                let toggle = record_toggle_button(ui, &state);
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
                        recording || paused,
                        egui::Button::new("■ Stop").min_size(egui::vec2(90.0, 32.0)),
                    )
                    .on_hover_text("Stop and keep the take")
                    .clicked()
                {
                    self.stop_recording();
                }
                if ui
                    .add_enabled(
                        recording || paused,
                        egui::Button::new("✕ Discard").min_size(egui::vec2(90.0, 32.0)),
                    )
                    .on_hover_text("Throw away the current take")
                    .clicked()
                {
                    self.recording_tab.confirm_discard = true;
                }
            });
        });

        // ---- Result card ----
        let has_recording = self.recording_tab.last_recording_path.is_some();
        if has_recording && !recording && !finalizing {
            recording_card(ui, |ui| {
                let name = self
                    .recording_tab
                    .last_recording_path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    .unwrap_or("recording.wav")
                    .to_string();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Last take").strong());
                    ui.label(RichText::new(name).monospace().weak());
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new("Open in Editor").min_size(egui::vec2(120.0, 28.0)))
                        .clicked()
                    {
                        self.open_recording_in_editor(ctx);
                    }
                    if ui
                        .add(egui::Button::new("Save As…").min_size(egui::vec2(100.0, 28.0)))
                        .on_hover_text("Copy the recorded WAV to a file")
                        .clicked()
                    {
                        self.save_recording_as();
                    }
                    if ui
                        .add_enabled(idle, egui::Button::new("Discard take"))
                        .on_hover_text("Forget this recording")
                        .clicked()
                    {
                        self.recording_tab.confirm_discard = true;
                    }
                });
            });
        }

        // ---- Discard confirmation modal ----
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

        // Request repaint while active to animate meters/waveform/clock.
        if recording || paused || finalizing {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn ui_recording_waveform(&mut self, ui: &mut Ui) {
        let overview = self.recording_tab.waveform_overview.clone();
        let desired = egui::vec2(ui.available_width(), 96.0);
        let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_gray(20));
        let mid = rect.center().y;

        // Zero line
        painter.line_segment(
            [egui::pos2(rect.left(), mid), egui::pos2(rect.right(), mid)],
            Stroke::new(1.0, Color32::from_gray(70)),
        );

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

        let n = overview.len();
        let w = rect.width();
        let h = rect.height();

        // Time grid (labelled vertical gridlines covering the visible window)
        let block_secs = self.recording_tab.overview_block_secs.max(0.0001);
        let span_secs = n as f32 * block_secs;
        let now_secs = self.recording_tab.elapsed_secs;
        let start_secs = (now_secs - span_secs).max(0.0);
        let step = recording_grid_time_step(span_secs);
        if step > 0.0 && span_secs > 0.0 {
            let first_tick = (start_secs / step).ceil() * step;
            let mut t = first_tick;
            while t <= now_secs + 0.0001 {
                let frac = ((t - start_secs) / span_secs).clamp(0.0, 1.0);
                let x = rect.left() + frac * w;
                painter.line_segment(
                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                    Stroke::new(1.0, Color32::from_gray(50)),
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

        // Waveform (clipping blocks highlighted in red)
        for (i, &(mn, mx)) in overview.iter().enumerate() {
            let x = rect.left() + (i as f32 / n as f32) * w;
            let y_top = mid - mx.clamp(-1.0, 1.0) * h * 0.5;
            let y_bot = mid - mn.clamp(-1.0, 1.0) * h * 0.5;
            let clipping = mn.abs() >= 0.98 || mx.abs() >= 0.98;
            let color = if clipping { REC_RED } else { METER_GREEN };
            painter.line_segment(
                [egui::pos2(x, y_top), egui::pos2(x, y_bot)],
                Stroke::new(1.0, color),
            );
        }

        // Current-position indicator (right edge = "now")
        painter.line_segment(
            [
                egui::pos2(rect.right() - 1.0, rect.top()),
                egui::pos2(rect.right() - 1.0, rect.bottom()),
            ],
            Stroke::new(1.5, Color32::from_rgb(230, 230, 120)),
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
    use super::{format_elapsed, meter_frac, recording_grid_time_step};

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
