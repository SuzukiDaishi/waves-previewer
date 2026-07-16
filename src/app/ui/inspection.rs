//! Batch inspection results window: severity-filtered rows, click-to-select,
//! CSV export.

use egui::{Color32, RichText};

use crate::app::inspection::{InspectionRow, IssueSeverity};
use crate::app::types::ToastSeverity;

#[derive(Clone, Copy, PartialEq, Eq)]
enum InspectionFilter {
    IssuesOnly,
    ErrorsOnly,
    All,
}

impl crate::app::WavesPreviewer {
    pub(crate) fn ui_inspection_window(&mut self, ctx: &egui::Context) {
        if !self.show_inspection_window {
            return;
        }
        let Some(report_len) = self.inspection_report.as_ref().map(|r| r.rows.len()) else {
            self.show_inspection_window = false;
            return;
        };
        let mut open = true;
        let mut clicked_path: Option<std::path::PathBuf> = None;
        let mut save_csv = false;
        egui::Window::new("Inspection Results")
            .open(&mut open)
            .default_width(760.0)
            .default_height(420.0)
            .resizable(true)
            .show(ctx, |ui| {
                let (errors, warnings, cancelled, cfg) = {
                    let report = self.inspection_report.as_ref().expect("report present");
                    let errors = report
                        .rows
                        .iter()
                        .filter(|r| r.severity == Some(IssueSeverity::Error))
                        .count();
                    let warnings = report
                        .rows
                        .iter()
                        .filter(|r| r.severity == Some(IssueSeverity::Warning))
                        .count();
                    (errors, warnings, report.cancelled, report.cfg)
                };
                let passed = report_len - errors - warnings;
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("{errors} errors"))
                            .color(Color32::from_rgb(240, 100, 100))
                            .strong(),
                    );
                    ui.label(
                        RichText::new(format!("{warnings} warnings"))
                            .color(Color32::from_rgb(235, 200, 90)),
                    );
                    ui.label(RichText::new(format!("{passed} passed")).weak());
                    if cancelled {
                        ui.label(RichText::new("(cancelled — partial)").weak());
                    }
                    ui.separator();
                    let mut filter = self.inspection_window_filter();
                    egui::ComboBox::from_id_salt("inspection_filter")
                        .selected_text(match filter {
                            InspectionFilter::IssuesOnly => "Issues only",
                            InspectionFilter::ErrorsOnly => "Errors only",
                            InspectionFilter::All => "All files",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut filter,
                                InspectionFilter::IssuesOnly,
                                "Issues only",
                            );
                            ui.selectable_value(
                                &mut filter,
                                InspectionFilter::ErrorsOnly,
                                "Errors only",
                            );
                            ui.selectable_value(&mut filter, InspectionFilter::All, "All files");
                        });
                    self.set_inspection_window_filter(filter);
                    if ui.button("Save CSV...").clicked() {
                        save_csv = true;
                    }
                });
                ui.label(
                    RichText::new(format!(
                        "target {:+.1} LUFS ±{:.1} LU · ceiling {:+.1} dBTP · silence > {:.0}/{:.0} ms @ {:.0} dBFS",
                        cfg.target_lufs,
                        cfg.lufs_tolerance_lu,
                        cfg.tp_ceiling_db,
                        cfg.max_leading_silence_ms,
                        cfg.max_trailing_silence_ms,
                        cfg.silence_threshold_dbfs,
                    ))
                    .weak()
                    .small(),
                );
                ui.separator();

                let filter = self.inspection_window_filter();
                let report = self.inspection_report.as_ref().expect("report present");
                let visible: Vec<usize> = report
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| match filter {
                        InspectionFilter::All => true,
                        InspectionFilter::IssuesOnly => r.severity.is_some(),
                        InspectionFilter::ErrorsOnly => r.severity == Some(IssueSeverity::Error),
                    })
                    .map(|(i, _)| i)
                    .collect();
                if visible.is_empty() {
                    ui.label(RichText::new("No rows match the current filter.").weak());
                    return;
                }
                let row_height = ui.text_style_height(&egui::TextStyle::Monospace) * 2.2;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, row_height, visible.len(), |ui, range| {
                        let report = self.inspection_report.as_ref().expect("report present");
                        for &row_idx in &visible[range] {
                            let row = &report.rows[row_idx];
                            if Self::ui_inspection_result_row(ui, row) {
                                clicked_path = Some(std::path::PathBuf::from(&row.path));
                            }
                        }
                    });
            });
        if let Some(path) = clicked_path {
            if let Some(row_idx) = self.row_for_path(&path) {
                self.update_selection_on_click(row_idx, egui::Modifiers::NONE);
                self.selected = Some(row_idx);
                self.scroll_to_selected = true;
            } else {
                self.push_toast(
                    ToastSeverity::Info,
                    "File is not in the current list view (filtered out?)",
                );
            }
        }
        if save_csv {
            if let Some(path) = self.pick_list_csv_save_dialog() {
                let mut path = path;
                let needs_ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| !s.eq_ignore_ascii_case("csv"))
                    .unwrap_or(true);
                if needs_ext {
                    path.set_extension("csv");
                }
                let report = self.inspection_report.as_ref().expect("report present");
                match crate::app::inspection::write_batch_inspection_report(
                    &path,
                    &report.rows,
                    &report.cfg,
                ) {
                    Ok(()) => self.push_toast(
                        ToastSeverity::Info,
                        format!("Inspection report saved: {}", path.display()),
                    ),
                    Err(err) => self.push_toast(
                        ToastSeverity::Error,
                        format!("Report save failed: {err}"),
                    ),
                }
            }
        }
        if !open {
            self.show_inspection_window = false;
        }
    }

    fn ui_inspection_result_row(ui: &mut egui::Ui, row: &InspectionRow) -> bool {
        let (sev_text, sev_color) = match row.severity {
            Some(IssueSeverity::Error) => ("ERROR", Color32::from_rgb(240, 100, 100)),
            Some(IssueSeverity::Warning) => ("WARN", Color32::from_rgb(235, 200, 90)),
            Some(IssueSeverity::Info) => ("INFO", Color32::from_rgb(120, 180, 240)),
            None => ("PASS", Color32::from_rgb(120, 200, 140)),
        };
        let mut clicked = false;
        let resp = ui.horizontal(|ui| {
            ui.label(RichText::new(sev_text).color(sev_color).monospace().strong());
            let name = ui.add(
                egui::Label::new(RichText::new(&row.file).monospace())
                    .sense(egui::Sense::click())
                    .truncate(),
            );
            if name.clicked() {
                clicked = true;
            }
            name.on_hover_text(&row.path);
            let mut vals: Vec<String> = Vec::new();
            if let Some(l) = row.effective_lufs {
                vals.push(format!("{l:+.1} LUFS"));
            }
            if let Some(tp) = row.effective_true_peak_db {
                vals.push(format!("{tp:+.1} dBTP"));
            }
            if let (Some(lead), Some(trail)) = (row.leading_silence_ms, row.trailing_silence_ms) {
                vals.push(format!("sil {lead:.0}/{trail:.0} ms"));
            }
            if let Some((s, e)) = row.loop_points {
                vals.push(format!("loop {s}..{e}"));
            }
            ui.label(RichText::new(vals.join("  ")).weak().small());
        });
        let summary = row
            .issues
            .iter()
            .map(|i| i.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        if !summary.is_empty() {
            ui.indent(("inspection_issue", &row.path), |ui| {
                ui.label(RichText::new(summary).small());
            });
        }
        ui.separator();
        let _ = resp;
        clicked
    }

    fn inspection_window_filter(&self) -> InspectionFilter {
        match self.inspection_window_filter_idx {
            1 => InspectionFilter::ErrorsOnly,
            2 => InspectionFilter::All,
            _ => InspectionFilter::IssuesOnly,
        }
    }

    fn set_inspection_window_filter(&mut self, filter: InspectionFilter) {
        self.inspection_window_filter_idx = match filter {
            InspectionFilter::IssuesOnly => 0,
            InspectionFilter::ErrorsOnly => 1,
            InspectionFilter::All => 2,
        };
    }
}
