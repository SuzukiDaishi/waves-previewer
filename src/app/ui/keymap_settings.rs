use crate::app::keymap::{self, Action, Dispatch, KeyContext, KEYMAP};
use crate::app::types::ToastSeverity;

impl crate::app::WavesPreviewer {
    /// Runs at the top of the shortcut dispatch so a chord pressed while a
    /// row is capturing lands in the rebind and never fires an action.
    pub(in crate::app) fn keymap_capture_tick(&mut self, ctx: &egui::Context) {
        let Some(action) = self.keymap_capture else {
            return;
        };
        if !self.show_keymap_window {
            self.keymap_capture = None;
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.keymap_capture = None;
            return;
        }
        let Some((key, modifiers)) = Self::keymap_take_pressed_key(ctx) else {
            return;
        };
        self.keymap_capture = None;
        match keymap::Mods::from_modifiers(modifiers) {
            Some(mods) => {
                if let Err(msg) = self.keymap_assign(action, mods, key) {
                    self.push_toast(ToastSeverity::Warning, msg);
                }
            }
            None => {
                self.push_toast(
                    ToastSeverity::Info,
                    "Alt-based chords are not supported for rebinding",
                );
            }
        }
    }

    fn keymap_take_pressed_key(ctx: &egui::Context) -> Option<(egui::Key, egui::Modifiers)> {
        ctx.input_mut(|i| {
            let mut found = None;
            i.events.retain(|ev| {
                if found.is_none() {
                    if let egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } = ev
                    {
                        found = Some((*key, *modifiers));
                        return false;
                    }
                }
                true
            });
            found
        })
    }

    /// Set (or clear back to default) the chord for a Table action. Fails
    /// when the chord is already taken by another row in an overlapping
    /// context, mirroring the duplicate rule the built-in table is tested by.
    pub(in crate::app) fn keymap_assign(
        &mut self,
        action: Action,
        mods: keymap::Mods,
        key: egui::Key,
    ) -> Result<(), String> {
        let Some(target) = keymap::binding(action) else {
            return Err("unknown action".into());
        };
        for other in KEYMAP {
            if other.action == action {
                continue;
            }
            let contexts_overlap = other.context == target.context
                || other.context == KeyContext::Global
                || target.context == KeyContext::Global;
            if !contexts_overlap {
                continue;
            }
            let Some((omods, okey)) = self.keymap_effective_chord(other.action) else {
                continue;
            };
            if okey == key && omods.to_modifiers() == mods.to_modifiers() {
                return Err(format!(
                    "{} is already used by \"{}\"",
                    keymap::chord_text(mods, key),
                    other.desc
                ));
            }
        }
        if target.chord == Some((mods, key)) {
            self.keymap_overrides.remove(&action);
        } else {
            self.keymap_overrides.insert(action, (mods, key));
        }
        self.save_prefs();
        Ok(())
    }

    pub(crate) fn ui_keymap_window(&mut self, ctx: &egui::Context) {
        if !self.show_keymap_window {
            return;
        }
        let mut open = true;
        let mut start_capture: Option<Action> = None;
        let mut reset_action: Option<Action> = None;
        let mut reset_all = false;
        egui::Window::new("Customize Shortcuts")
            .open(&mut open)
            .default_width(520.0)
            .default_height(480.0)
            .vscroll(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Click a key cell, then press the new chord. Esc cancels.");
                    if ui.button("Reset All").clicked() {
                        reset_all = true;
                    }
                });
                ui.add_space(4.0);
                for (context, title) in [
                    (KeyContext::Global, "Global"),
                    (KeyContext::List, "List View"),
                    (KeyContext::Editor, "Editor"),
                ] {
                    ui.heading(title);
                    egui::Grid::new(("keymap_grid", title))
                        .num_columns(3)
                        .min_col_width(150.0)
                        .striped(true)
                        .show(ui, |ui| {
                            for binding in KEYMAP.iter().filter(|b| b.context == context) {
                                let rebindable = binding.dispatch == Dispatch::Table
                                    && binding.chord.is_some();
                                if !rebindable {
                                    // Manual rows keep their dedicated handlers;
                                    // shown grayed so the list stays complete.
                                    ui.add_enabled(
                                        false,
                                        egui::Button::new(
                                            egui::RichText::new(binding.keys_text()).monospace(),
                                        ),
                                    );
                                    ui.label(binding.desc);
                                    ui.label("");
                                    ui.end_row();
                                    continue;
                                }
                                let capturing = self.keymap_capture == Some(binding.action);
                                let overridden =
                                    self.keymap_overrides.contains_key(&binding.action);
                                let keys_text = if capturing {
                                    "press a key...".to_string()
                                } else {
                                    self.keymap_effective_chord(binding.action)
                                        .map(|(m, k)| keymap::chord_text(m, k))
                                        .unwrap_or_default()
                                };
                                let mut text = egui::RichText::new(keys_text).monospace();
                                if capturing || overridden {
                                    text = text.strong();
                                }
                                if ui
                                    .add(egui::Button::new(text))
                                    .on_hover_text("Click, then press the new key chord")
                                    .clicked()
                                {
                                    start_capture = Some(binding.action);
                                }
                                ui.label(binding.desc);
                                if overridden {
                                    if ui.small_button("Reset").clicked() {
                                        reset_action = Some(binding.action);
                                    }
                                } else {
                                    ui.label("");
                                }
                                ui.end_row();
                            }
                        });
                    ui.add_space(10.0);
                }
                ui.separator();
                ui.label("Gray rows have fixed keys; their chords are not checked for conflicts.");
            });
        if let Some(action) = start_capture {
            self.keymap_capture = Some(action);
        }
        if let Some(action) = reset_action {
            self.keymap_overrides.remove(&action);
            self.keymap_capture = None;
            self.save_prefs();
        }
        if reset_all && !self.keymap_overrides.is_empty() {
            self.keymap_overrides.clear();
            self.keymap_capture = None;
            self.save_prefs();
        }
        self.show_keymap_window = open;
        if !open {
            self.keymap_capture = None;
        }
    }
}
