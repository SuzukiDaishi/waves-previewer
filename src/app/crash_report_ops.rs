use super::*;

impl WavesPreviewer {
    pub(super) fn refresh_crash_reports_on_startup(&mut self) {
        self.refresh_crash_reports();
        if !self.crash_reports.reports.is_empty() {
            self.crash_reports.window_open = true;
        }
    }

    pub(super) fn open_crash_report_window(&mut self) {
        self.refresh_crash_reports();
        self.crash_reports.window_open = true;
    }

    pub(super) fn refresh_crash_reports(&mut self) {
        match crate::crash_report::list_unacknowledged_reports() {
            Ok(reports) => {
                self.crash_reports.reports = reports;
            }
            Err(err) => {
                self.crash_reports.reports.clear();
                self.crash_reports.status = Some(format!("Failed to load crash reports: {err}"));
            }
        }
    }

    pub(super) fn ui_crash_report_window(&mut self, ctx: &egui::Context) {
        if !self.crash_reports.window_open {
            return;
        }

        let mut open = self.crash_reports.window_open;
        let reports_len = self.crash_reports.reports.len();
        let latest = self.crash_reports.reports.first().cloned();
        let status = self.crash_reports.status.clone();
        let mut close_requested = false;
        let mut reviewed_id = None;

        egui::Window::new("Crash Reports")
            .open(&mut open)
            .resizable(true)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(format!("Unreviewed reports: {reports_len}"));
                ui.horizontal_wrapped(|ui| {
                    ui.label("Folder:");
                    ui.monospace(
                        crate::crash_report::crash_report_dir()
                            .display()
                            .to_string(),
                    );
                });
                ui.separator();

                if let Some(report) = latest.as_ref() {
                    ui.label("Latest report");
                    ui.monospace(format!("{}  {}", report.created_at, report.id));
                    ui.add_space(4.0);
                    ui.label(&report.summary);
                } else {
                    ui.label("No unreviewed crash reports.");
                }

                if let Some(status) = status.as_deref() {
                    ui.add_space(6.0);
                    ui.label(status);
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let has_report = latest.is_some();
                    if ui
                        .add_enabled(has_report, egui::Button::new("Copy Report"))
                        .clicked()
                    {
                        if let Some(report) = latest.as_ref() {
                            match crate::crash_report::copyable_report_text(&report.path) {
                                Ok(text) => {
                                    ctx.copy_text(text);
                                    self.crash_reports.status =
                                        Some("Crash report copied.".to_owned());
                                }
                                Err(err) => {
                                    self.crash_reports.status =
                                        Some(format!("Failed to copy crash report: {err}"));
                                }
                            }
                        }
                    }
                    if ui.button("Open Folder").clicked() {
                        let result = if let Some(report) = latest.as_ref() {
                            helpers::open_folder_with_file_selected(&report.path)
                        } else {
                            let dir = crate::crash_report::crash_report_dir();
                            let _ = std::fs::create_dir_all(&dir);
                            helpers::open_in_file_explorer(&dir)
                        };
                        if let Err(err) = result {
                            self.crash_reports.status =
                                Some(format!("Failed to open crash report folder: {err}"));
                        }
                    }
                    if ui
                        .add_enabled(has_report, egui::Button::new("Mark Reviewed"))
                        .clicked()
                    {
                        if let Some(report) = latest.as_ref() {
                            reviewed_id = Some(report.id.clone());
                        }
                    }
                    if ui.button("Dismiss").clicked() {
                        close_requested = true;
                    }
                });
            });

        if let Some(id) = reviewed_id {
            match crate::crash_report::acknowledge_report(&id) {
                Ok(()) => {
                    self.crash_reports.status = Some("Crash report marked reviewed.".to_owned());
                    self.refresh_crash_reports();
                    if self.crash_reports.reports.is_empty() {
                        open = false;
                    }
                }
                Err(err) => {
                    self.crash_reports.status =
                        Some(format!("Failed to mark crash report reviewed: {err}"));
                }
            }
        }

        if close_requested {
            open = false;
        }
        self.crash_reports.window_open = open;
    }
}
