use crate::app::keymap::{self, KeyContext, KEYMAP};

impl crate::app::WavesPreviewer {
    /// Read-only keyboard shortcut list, generated from the central KEYMAP.
    pub(crate) fn ui_shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.show_shortcuts_window {
            return;
        }
        let mut open = true;
        egui::Window::new("Keyboard Shortcuts")
            .open(&mut open)
            .default_width(460.0)
            .default_height(480.0)
            .vscroll(true)
            .show(ctx, |ui| {
                for (context, title) in [
                    (KeyContext::Global, "Global"),
                    (KeyContext::List, "List View"),
                    (KeyContext::Editor, "Editor"),
                ] {
                    ui.heading(title);
                    egui::Grid::new(("shortcuts_grid", title))
                        .num_columns(2)
                        .min_col_width(170.0)
                        .striped(true)
                        .show(ui, |ui| {
                            for binding in KEYMAP.iter().filter(|b| b.context == context) {
                                // Table rows honor user rebinds; manual rows
                                // keep their static labels.
                                let keys = if binding.chord.is_some() {
                                    self.keymap_effective_chord(binding.action)
                                        .map(|(m, k)| keymap::chord_text(m, k))
                                        .unwrap_or_else(|| binding.keys_text())
                                } else {
                                    binding.keys_text()
                                };
                                ui.monospace(keys);
                                ui.label(binding.desc);
                                ui.end_row();
                            }
                        });
                    ui.add_space(10.0);
                }
                ui.separator();
                ui.label("Tool-specific canvas gestures are described in docs/CONTROLS.md.");
            });
        self.show_shortcuts_window = open;
    }
}
