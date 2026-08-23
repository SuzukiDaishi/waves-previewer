use crate::app::keymap::{self, KeyBinding, KeyCategory, KeyContext, KEYMAP};

impl crate::app::WavesPreviewer {
    /// The keys as they are actually bound right now, generated from KEYMAP.
    ///
    /// Grouped by context and then by category. It used to be one flat table
    /// per context, which put the Editor's forty-odd rows in a single run and
    /// left the loop keys somewhere in the middle of it -- findable only by
    /// already knowing which key you wanted.
    pub(crate) fn ui_shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.show_shortcuts_window {
            return;
        }
        let mut open = true;
        let scroll_target = self.begin_floating_scroll_surface("keyboard_shortcuts_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let shown = egui::Window::new("Keyboard Shortcuts")
            .open(&mut open)
            .default_width(520.0)
            .default_height(560.0)
            .vscroll(true)
            .show(ctx, |ui| {
                for (context, title) in [
                    (KeyContext::Global, "Global"),
                    (KeyContext::List, "List View"),
                    (KeyContext::Editor, "Editor"),
                ] {
                    ui.heading(title);
                    for category in KeyCategory::ALL {
                        let rows: Vec<&KeyBinding> = KEYMAP
                            .iter()
                            .filter(|b| b.context == context && b.category == category)
                            .collect();
                        if rows.is_empty() {
                            continue;
                        }
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(category.title())
                                .strong()
                                .color(ui.visuals().weak_text_color()),
                        );
                        egui::Grid::new(("shortcuts_grid", title, category.title()))
                            .num_columns(2)
                            .min_col_width(150.0)
                            .striped(true)
                            .show(ui, |ui| {
                                for binding in rows {
                                    ui.monospace(self.shortcut_keys_text(binding));
                                    ui.vertical(|ui| {
                                        ui.label(binding.desc);
                                        if !binding.detail.is_empty() {
                                            ui.label(
                                                egui::RichText::new(binding.detail).small().weak(),
                                            );
                                        }
                                    });
                                    ui.end_row();
                                }
                            });
                    }
                    ui.add_space(10.0);
                }
                ui.separator();
                ui.label("Tool-specific canvas gestures are described in docs/CONTROLS.md.");
            });
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        self.show_shortcuts_window = open;
    }

    /// The keys this binding answers to right now.
    ///
    /// Table rows honour a user's rebinding; manual rows describe a family of
    /// keys and keep their static label.
    pub(crate) fn shortcut_keys_text(&self, binding: &KeyBinding) -> String {
        if binding.chord.is_some() {
            self.keymap_effective_chord(binding.action)
                .map(|(m, k)| keymap::chord_text(m, k))
                .unwrap_or_else(|| binding.keys_text())
        } else {
            binding.keys_text()
        }
    }
}
