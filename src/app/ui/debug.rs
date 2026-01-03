use egui::RichText;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_debug_window(&mut self, ctx: &egui::Context) {
        if !self.debug.cfg.enabled {
            return;
        }
        let mut open = self.debug.show_window;
        egui::Window::new("Debug")
            .open(&mut open)
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Screenshot").clicked() {
                        let path = self.default_screenshot_path();
                        self.request_screenshot(ctx, path, false);
                    }
                    if ui.button("Copy Summary").clicked() {
                        let summary = self.debug_summary();
                        ctx.copy_text(summary);
                    }
                    if ui.button("Save Summary").clicked() {
                        let path = self.default_debug_summary_path();
                        self.save_debug_summary(path);
                    }
                    if ui.button("Run Checks").clicked() {
                        self.debug_check_invariants();
                    }
                });
                ui.separator();
                ui.label(RichText::new("Summary").strong());
                let summary = self.debug_summary();
                for line in summary.lines() {
                    ui.monospace(line);
                }
                ui.separator();
                ui.label(RichText::new("List Perf").strong());
                ui.horizontal_wrapped(|ui| {
                    ui.label("Dummy files");
                    ui.add(
                        egui::DragValue::new(&mut self.debug.dummy_list_count)
                            .range(0..=1_000_000)
                            .speed(5000),
                    );
                    if ui.button("Populate").clicked() {
                        let count = self.debug.dummy_list_count as usize;
                        self.populate_dummy_list(count);
                    }
                });
                ui.checkbox(&mut self.debug.overlay_trace, "Overlay trace logs");
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Logs").strong());
                    if ui.button("Clear").clicked() {
                        self.debug.logs.clear();
                    }
                });
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for line in &self.debug.logs {
                        ui.monospace(line);
                    }
                });
                if let Some(auto) = &self.debug.auto {
                    ui.separator();
                    ui.label(format!("auto-run steps: {}", auto.steps.len()));
                }
            });
        self.debug.show_window = open;
    }
}
