use egui::{RichText, Ui};

use crate::app::types::{RecordingSourceKind, RecordingState};

impl super::super::WavesPreviewer {
    pub(in crate::app) fn ui_recording_view(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        ui.heading("Recording");
        ui.separator();

        // Source selection
        ui.horizontal(|ui| {
            ui.label("Source:");
            let sources = [
                (RecordingSourceKind::Microphone, "Microphone"),
                (RecordingSourceKind::System, "System Audio"),
                (RecordingSourceKind::SystemAndMicrophone, "System + Mic"),
            ];
            for (kind, label) in &sources {
                let selected = &self.recording_tab.source == kind;
                if ui.selectable_label(selected, *label).clicked() {
                    self.recording_tab.source = kind.clone();
                }
            }
        });

        #[cfg(not(target_os = "windows"))]
        if self.recording_tab.source != RecordingSourceKind::Microphone {
            ui.label(
                RichText::new("System audio capture is only supported on Windows.")
                    .color(ui.style().visuals.warn_fg_color),
            );
        }

        // Device selection
        if self.recording_tab.source != RecordingSourceKind::System {
            ui.horizontal(|ui| {
                ui.label("Input device:");
                if self.recording_tab.input_devices.is_empty() {
                    ui.label(RichText::new("(no devices)").weak());
                } else {
                    let current = self
                        .recording_tab
                        .selected_mic_id
                        .clone()
                        .unwrap_or_default();
                    egui::ComboBox::from_id_salt("rec_input_device")
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
                                let sel =
                                    self.recording_tab.selected_mic_id.as_deref() == Some(&dev.id);
                                if ui.selectable_label(sel, &dev.display_name).clicked() {
                                    self.recording_tab.selected_mic_id = Some(dev.id.clone());
                                }
                            }
                        });
                }
                if ui.button("Refresh").clicked() {
                    self.recording_refresh_devices();
                }
            });
        }

        ui.separator();

        // Level meter
        ui.horizontal(|ui| {
            ui.label("L:");
            let level_l = self.recording_tab.level_l.min(1.0);
            ui.add(
                egui::ProgressBar::new(level_l)
                    .desired_width(120.0)
                    .fill(if level_l > 0.9 {
                        egui::Color32::RED
                    } else {
                        egui::Color32::GREEN
                    }),
            );
            ui.label("R:");
            let level_r = self.recording_tab.level_r.min(1.0);
            ui.add(
                egui::ProgressBar::new(level_r)
                    .desired_width(120.0)
                    .fill(if level_r > 0.9 {
                        egui::Color32::RED
                    } else {
                        egui::Color32::GREEN
                    }),
            );
        });

        // Waveform overview
        let overview = self.recording_tab.waveform_overview.clone();
        if !overview.is_empty() {
            let desired = egui::vec2(ui.available_width(), 80.0);
            let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, egui::Color32::from_gray(24));
            let n = overview.len();
            let w = rect.width();
            let h = rect.height();
            let mid = rect.center().y;

            // Zero line
            painter.line_segment(
                [egui::pos2(rect.left(), mid), egui::pos2(rect.right(), mid)],
                egui::Stroke::new(1.0, egui::Color32::from_gray(70)),
            );

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
                        egui::Stroke::new(1.0, egui::Color32::from_gray(50)),
                    );
                    painter.text(
                        egui::pos2(x + 2.0, rect.top() + 2.0),
                        egui::Align2::LEFT_TOP,
                        crate::app::helpers::format_time_s(t),
                        egui::FontId::monospace(10.0),
                        egui::Color32::from_gray(150),
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
                let color = if clipping {
                    egui::Color32::from_rgb(220, 60, 60)
                } else {
                    egui::Color32::from_rgb(80, 180, 80)
                };
                painter.line_segment(
                    [egui::pos2(x, y_top), egui::pos2(x, y_bot)],
                    egui::Stroke::new(1.0, color),
                );
            }

            // Current-position indicator (right edge = "now")
            painter.line_segment(
                [
                    egui::pos2(rect.right() - 1.0, rect.top()),
                    egui::pos2(rect.right() - 1.0, rect.bottom()),
                ],
                egui::Stroke::new(1.5, egui::Color32::from_rgb(230, 230, 120)),
            );
        }

        // Elapsed time
        let elapsed = self.recording_tab.elapsed_secs;
        let h = (elapsed / 3600.0) as u32;
        let m = ((elapsed % 3600.0) / 60.0) as u32;
        let s = (elapsed % 60.0) as u32;
        ui.label(RichText::new(format!("{h:02}:{m:02}:{s:02}")).monospace());

        // Status message
        if !self.recording_tab.progress_message.is_empty() {
            ui.label(RichText::new(&self.recording_tab.progress_message.clone()).weak());
        }
        if let RecordingState::Error(ref msg) = self.recording_tab.state.clone() {
            ui.label(
                RichText::new(format!("Error: {msg}")).color(ui.style().visuals.error_fg_color),
            );
        }

        ui.separator();

        // Transport buttons
        let state = self.recording_tab.state.clone();
        ui.horizontal(|ui| {
            let idle = matches!(state, RecordingState::Idle | RecordingState::Error(_));
            let recording = state == RecordingState::Recording;
            let paused = state == RecordingState::Paused;
            let finalizing = state == RecordingState::Finalizing;

            if paused {
                if ui.button("▶ Resume").clicked() {
                    self.resume_recording();
                }
            } else if ui
                .add_enabled(idle, egui::Button::new("● Record"))
                .clicked()
            {
                self.start_recording();
            }
            if ui
                .add_enabled(recording, egui::Button::new("⏸ Pause"))
                .clicked()
            {
                self.pause_recording();
            }
            if ui
                .add_enabled(recording || paused, egui::Button::new("■ Stop"))
                .clicked()
            {
                self.stop_recording();
            }
            if ui
                .add_enabled(!idle && !finalizing, egui::Button::new("✕ Discard"))
                .clicked()
            {
                self.discard_recording();
            }
        });

        // Open in Editor
        let has_recording = self.recording_tab.last_recording_path.is_some();
        if ui
            .add_enabled(
                has_recording
                    && !matches!(
                        state,
                        RecordingState::Finalizing | RecordingState::Recording
                    ),
                egui::Button::new("Open in Editor"),
            )
            .clicked()
        {
            self.open_recording_in_editor(ctx);
        }

        // Request repaint while recording to update meter/waveform
        if matches!(
            state,
            RecordingState::Recording | RecordingState::Finalizing
        ) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
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
    use super::recording_grid_time_step;

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
}
