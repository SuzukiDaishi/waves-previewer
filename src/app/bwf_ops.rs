//! BWF `bext` metadata batch editing (WAV only).

use crate::app::types::ToastSeverity;
use crate::wave::BextFields;

impl crate::app::WavesPreviewer {
    pub(super) fn open_bwf_dialog(&mut self) {
        // Prefill from the first selected WAV that already carries bext.
        let mut fields = BextFields::default();
        for path in self.selected_paths() {
            if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("wav"))
                .unwrap_or(false)
            {
                if let Ok(Some(existing)) = crate::wave::read_wav_bext(&path) {
                    fields = existing;
                    break;
                }
            }
        }
        self.bwf_fields = fields;
        self.show_bwf_dialog = true;
    }

    pub(crate) fn ui_bwf_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_bwf_dialog {
            return;
        }
        let mut open = true;
        let mut apply_clicked = false;
        let wav_count = self
            .selected_paths()
            .iter()
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("wav"))
                    .unwrap_or(false)
            })
            .count();
        let total = self.selected_paths().len();
        egui::Window::new("Edit BWF Metadata")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "Writes the bext chunk into {wav_count} selected WAV file(s){}.",
                    if total > wav_count {
                        format!(" ({} non-WAV skipped)", total - wav_count)
                    } else {
                        String::new()
                    }
                ));
                ui.separator();
                ui.label("Description (max 256)");
                ui.text_edit_singleline(&mut self.bwf_fields.description);
                ui.label("Originator (max 32)");
                ui.text_edit_singleline(&mut self.bwf_fields.originator);
                ui.label("Originator reference (max 32)");
                ui.text_edit_singleline(&mut self.bwf_fields.originator_reference);
                ui.label(
                    egui::RichText::new(
                        "Origination date/time are stamped automatically; other chunks (loops, markers, audio) are preserved.",
                    )
                    .weak(),
                );
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(wav_count > 0, egui::Button::new("Write"))
                        .clicked()
                    {
                        apply_clicked = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_bwf_dialog = false;
                    }
                });
            });
        if apply_clicked {
            self.show_bwf_dialog = false;
            let fields = self.bwf_fields.clone();
            let mut written = 0usize;
            let mut skipped = 0usize;
            let mut failed = 0usize;
            for path in self.selected_paths() {
                let is_wav = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("wav"))
                    .unwrap_or(false);
                if !is_wav {
                    skipped += 1;
                    continue;
                }
                match crate::wave::write_wav_bext(&path, &fields) {
                    Ok(()) => written += 1,
                    Err(_) => failed += 1,
                }
            }
            let severity = if failed > 0 {
                ToastSeverity::Warning
            } else {
                ToastSeverity::Info
            };
            self.push_toast(
                severity,
                format!(
                    "BWF metadata: wrote {written} file(s){}{}",
                    if skipped > 0 {
                        format!(", skipped {skipped} non-WAV")
                    } else {
                        String::new()
                    },
                    if failed > 0 {
                        format!(", {failed} failed")
                    } else {
                        String::new()
                    },
                ),
            );
        } else if !open {
            self.show_bwf_dialog = false;
        }
    }
}
