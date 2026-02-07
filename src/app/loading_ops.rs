use egui::{Color32, RichText};

use super::types::ProcessingResult;
use super::BULK_RESAMPLE_BLOCK_SECS;

impl super::WavesPreviewer {
    pub(super) fn tick_processing_state(&mut self, ctx: &egui::Context) {
        let mut processing_done: Option<(ProcessingResult, bool)> = None;
        if let Some(state) = &mut self.processing {
            if let Ok(res) = state.rx.try_recv() {
                processing_done = Some((res, state.autoplay_when_ready));
            }
        }
        if let Some((res, autoplay_when_ready)) = processing_done {
            let ProcessingResult {
                path,
                samples,
                waveform,
                mut channels,
            } = res;
            // Apply processed buffer + waveform and refresh playback state.
            if channels.is_empty() {
                self.audio.set_samples_mono(samples);
            } else {
                self.apply_sample_rate_preview_for_path(
                    &path,
                    &mut channels,
                    self.audio.shared.out_sample_rate,
                );
                self.audio.set_samples_channels(channels);
            }
            self.audio.stop();
            if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
                if let Some(tab) = self.tabs.get_mut(idx) {
                    tab.waveform_minmax = waveform;
                }
            }
            // update current playing path (for effective volume using pending gains)
            self.playing_path = Some(path.clone());
            // full-buffer loop region if needed
            if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                self.audio.set_loop_region(0, buf.len());
            }
            self.processing = None;
            if autoplay_when_ready
                && self.auto_play_list_nav
                && self.selected_path_buf().as_ref() == Some(&path)
            {
                self.audio.play();
                self.debug_mark_list_play_start(&path);
            }
            ctx.request_repaint();
        }
    }

    pub(super) fn ui_busy_overlay(&mut self, ctx: &egui::Context) {
        let bulk_blocking = self
            .bulk_resample_state
            .as_ref()
            .map(|s| s.started_at.elapsed().as_secs() >= BULK_RESAMPLE_BLOCK_SECS)
            .unwrap_or(false);
        let block_busy = self.export_state.is_some()
            || self.editor_apply_state.is_some()
            || self.csv_export_state.is_some()
            || bulk_blocking;
        if !block_busy {
            return;
        }
        // Block input and show a modal spinner for operations that must not be interrupted.
        use egui::{Id, LayerId, Order};
        let screen = ctx.viewport_rect();
        // block input
        egui::Area::new("busy_block_input".into())
            .order(Order::Foreground)
            .show(ctx, |ui| {
                let _ = ui.allocate_rect(screen, egui::Sense::click_and_drag());
            });
        // darken background
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("busy_layer")));
        painter.rect_filled(screen, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 180));
        // centered box with spinner and text
        egui::Area::new("busy_center".into())
            .order(Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                egui::Frame::window(ui.style()).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Spinner::new());
                        let msg = if let Some(p) = &self.processing {
                            p.msg.as_str()
                        } else if let Some(st) = &self.editor_apply_state {
                            st.msg.as_str()
                        } else if let Some(st) = &self.export_state {
                            st.msg.as_str()
                        } else if self.csv_export_state.is_some() {
                            "Preparing CSV..."
                        } else if self.bulk_resample_state.is_some() {
                            "Applying sample rate..."
                        } else {
                            "Working..."
                        };
                        ui.label(RichText::new(msg).strong());
                        if self.editor_apply_state.is_some() {
                            if ui.button("Cancel").clicked() {
                                self.cancel_editor_apply();
                            }
                        }
                        if let Some(state) = &mut self.bulk_resample_state {
                            let total = state.targets.len().max(1);
                            let pct =
                                (state.index as f32 / total as f32).clamp(0.0, 1.0);
                            ui.add(
                                egui::ProgressBar::new(pct)
                                    .desired_width(180.0)
                                    .show_percentage(),
                            );
                            if ui.button("Cancel").clicked() {
                                state.cancel_requested = true;
                            }
                        }
                        if let Some(csv) = &self.csv_export_state {
                            if csv.total > 0 {
                                let pct = (csv.done as f32 / csv.total as f32)
                                    .clamp(0.0, 1.0);
                                ui.add(
                                    egui::ProgressBar::new(pct)
                                        .desired_width(180.0)
                                        .show_percentage(),
                                );
                            }
                        }
                    });
                });
            });
    }
}
