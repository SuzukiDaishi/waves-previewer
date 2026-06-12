use egui::{RichText, Sense};

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_transcript_window(&mut self, ctx: &egui::Context) {
        if !self.show_transcript_window {
            return;
        }
        let mut open = self.show_transcript_window;
        egui::Window::new("Transcript")
            .resizable(true)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 12.0))
            .open(&mut open)
            .show(ctx, |ui| {
                let Some(path) = self.current_active_path().cloned() else {
                    ui.label("No active file.");
                    return;
                };
                self.queue_transcript_for_path(&path, true);
                ui.label(path.display().to_string());
                let transcript = self.transcript_for_path(&path).cloned();
                let Some(transcript) = transcript else {
                    if self.transcript_inflight.contains(&path) {
                        ui.label("Loading transcript...");
                    } else {
                        ui.label("No transcript found.");
                    }
                    return;
                };
                let mut seek_ms: Option<u64> = None;
                // Compile the highlight regex once per frame, not per segment.
                let highlight_re = self.cached_highlight_regex();
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for seg in &transcript.segments {
                        ui.horizontal(|ui| {
                            let time = format_timestamp_ms(seg.start_ms);
                            if ui
                                .add(egui::Button::new(time).small().sense(Sense::click()))
                                .clicked()
                            {
                                seek_ms = Some(seg.start_ms);
                            }
                            let text = seg.text.as_str();
                            let label = if let Some(job) = highlight_re.as_ref().and_then(|re| {
                                crate::app::helpers::highlight_text_job_with_regex(
                                    text,
                                    re,
                                    ui.style(),
                                )
                            }) {
                                egui::Label::new(job).wrap()
                            } else {
                                egui::Label::new(RichText::new(text)).wrap()
                            };
                            ui.add(label);
                        });
                    }
                });
                if let Some(ms) = seek_ms {
                    self.request_transcript_seek(&path, ms);
                }
            });
        self.show_transcript_window = open;
    }
}

fn format_timestamp_ms(ms: u64) -> String {
    let total_ms = ms;
    let total_secs = total_ms / 1000;
    let m = total_secs / 60;
    let s = total_secs % 60;
    let ms = total_ms % 1000;
    format!("{m}:{s:02}.{ms:03}")
}

