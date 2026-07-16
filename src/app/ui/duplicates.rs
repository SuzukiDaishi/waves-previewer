use egui::{Color32, RichText};

impl crate::app::WavesPreviewer {
    /// Duplicate-scan results: one section per group, rows click to select
    /// the file in the list, CSV export of the whole report.
    pub(crate) fn ui_duplicates_window(&mut self, ctx: &egui::Context) {
        if !self.show_duplicates_window {
            return;
        }
        let mut open = true;
        let mut select_path: Option<std::path::PathBuf> = None;
        let mut save_csv = false;
        egui::Window::new("Duplicate Files")
            .open(&mut open)
            .default_width(560.0)
            .default_height(440.0)
            .show(ctx, |ui| {
                let Some(report) = &self.duplicate_report else {
                    ui.label(RichText::new("No duplicate scan has run yet.").weak());
                    return;
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} group(s) | {} file(s) scanned{}{}",
                            report.groups.len(),
                            report.scanned,
                            if report.failed > 0 {
                                format!(" | {} failed to decode", report.failed)
                            } else {
                                String::new()
                            },
                            if report.cancelled { " | cancelled" } else { "" },
                        ))
                        .weak(),
                    );
                    if !report.groups.is_empty() && ui.button("Save CSV...").clicked() {
                        save_csv = true;
                    }
                });
                ui.separator();
                if report.groups.is_empty() {
                    ui.label("No duplicate or similar files found.");
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("duplicates_scroll")
                    .show(ui, |ui| {
                        for (gi, group) in report.groups.iter().enumerate() {
                            let title = if group.exact {
                                format!("Group {} — exact duplicates", gi + 1)
                            } else {
                                format!(
                                    "Group {} — similar ({:.0}%)",
                                    gi + 1,
                                    group.min_similarity * 100.0
                                )
                            };
                            let color = if group.exact {
                                Color32::from_rgb(255, 140, 120)
                            } else {
                                Color32::from_rgb(255, 210, 120)
                            };
                            ui.label(RichText::new(title).strong().color(color));
                            for path in &group.paths {
                                let label = path.display().to_string();
                                if ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(&label).small().monospace(),
                                        )
                                        .sense(egui::Sense::click())
                                        .truncate(),
                                    )
                                    .on_hover_text(&label)
                                    .clicked()
                                {
                                    select_path = Some(path.clone());
                                }
                            }
                            ui.add_space(6.0);
                        }
                    });
            });
        if let Some(path) = select_path {
            if let Some(row) = self.row_for_path(&path) {
                self.select_and_load(row, true);
            }
        }
        if save_csv {
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name("duplicates.csv")
                .add_filter("CSV", &["csv"])
                .save_file()
            {
                if let Err(err) = self.write_duplicates_csv(&path) {
                    self.push_toast(
                        crate::app::types::ToastSeverity::Error,
                        format!("Save duplicates CSV failed: {err}"),
                    );
                } else {
                    self.push_toast(
                        crate::app::types::ToastSeverity::Info,
                        format!("Saved {}", path.display()),
                    );
                }
            }
        }
        self.show_duplicates_window = open;
    }

    pub(super) fn write_duplicates_csv(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let Some(report) = &self.duplicate_report else {
            anyhow::bail!("no duplicate report");
        };
        let mut out = String::from("group,kind,min_similarity,path\n");
        for (gi, group) in report.groups.iter().enumerate() {
            for p in &group.paths {
                out.push_str(&format!(
                    "{},{},{:.3},\"{}\"\n",
                    gi + 1,
                    if group.exact { "exact" } else { "similar" },
                    group.min_similarity,
                    p.display().to_string().replace('"', "\"\""),
                ));
            }
        }
        std::fs::write(path, out)?;
        Ok(())
    }
}
