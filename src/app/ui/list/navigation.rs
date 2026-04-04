use egui::Sense;

use crate::app::WavesPreviewer;

use super::{ListInteractionState, ListViewMetrics};

impl WavesPreviewer {
    pub(super) fn handle_list_focus_and_keyboard(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        metrics: &ListViewMetrics,
    ) -> ListInteractionState {
        let list_focus_id = crate::app::WavesPreviewer::list_focus_id();
        let list_focus_now = ctx.memory(|m| m.has_focus(list_focus_id));
        let focused_id = ctx.memory(|m| m.focused());
        let search_focused = ctx.memory(|m| m.has_focus(crate::app::WavesPreviewer::search_box_id()));
        let has_non_list_focus = focused_id.is_some() && focused_id != Some(list_focus_id);
        let rename_modal_open = self.list_modal_open();
        let allow_focus_reclaim = !rename_modal_open && !search_focused && !has_non_list_focus;
        let focus_resp = ui.interact(metrics.list_rect, list_focus_id, Sense::click());
        if self.list_has_focus && !list_focus_now && allow_focus_reclaim {
            ctx.memory_mut(|m| m.request_focus(list_focus_id));
        }
        let _ = focus_resp;

        let mut list_has_focus = list_focus_now || self.list_has_focus;
        if !list_has_focus
            && self.is_list_workspace_active()
            && self.selected.is_some()
            && !self.search_has_focus
            && allow_focus_reclaim
        {
            ctx.memory_mut(|m| m.request_focus(list_focus_id));
            list_has_focus = true;
            self.list_has_focus = true;
        }

        let mut key_moved = false;
        let allow_list_keys = self.is_list_workspace_active()
            && !self.files.is_empty()
            && !search_focused
            && !rename_modal_open;
        if self.debug.cfg.enabled && self.is_list_workspace_active() && !self.files.is_empty() {
            let nav_key_pressed = ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowDown)
                    || i.key_pressed(egui::Key::ArrowUp)
                    || i.key_pressed(egui::Key::PageDown)
                    || i.key_pressed(egui::Key::PageUp)
                    || i.key_pressed(egui::Key::Home)
                    || i.key_pressed(egui::Key::End)
            });
            if nav_key_pressed && !allow_list_keys {
                self.debug_trace_input(&format!(
                    "list nav blocked (search_focused={search_focused}, has_non_list_focus={has_non_list_focus}, rename_modal_open={rename_modal_open})"
                ));
            }
        }
        let list_key_intent = if allow_list_keys {
            ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowDown)
                    || i.key_pressed(egui::Key::ArrowUp)
                    || i.key_pressed(egui::Key::Enter)
                    || i.key_pressed(egui::Key::ArrowLeft)
                    || i.key_pressed(egui::Key::ArrowRight)
                    || i.key_pressed(egui::Key::PageDown)
                    || i.key_pressed(egui::Key::PageUp)
                    || i.key_pressed(egui::Key::Home)
                    || i.key_pressed(egui::Key::End)
                    || i.key_pressed(egui::Key::Delete)
                    || ((i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::A))
            })
        } else {
            false
        };
        if allow_list_keys && list_key_intent && !rename_modal_open {
            ctx.memory_mut(|m| m.request_focus(list_focus_id));
            list_has_focus = true;
            self.list_has_focus = true;
        }
        if list_has_focus {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    list_focus_id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        tab: true,
                        ..Default::default()
                    },
                );
            });
        }

        let mut pressed_down = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
        } else {
            false
        };
        let mut pressed_up = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
        } else {
            false
        };
        if allow_list_keys && (!pressed_down || !pressed_up) {
            let raw_arrow = ctx.input(|i| {
                let mut down = false;
                let mut up = false;
                for ev in &i.raw.events {
                    if let egui::Event::Key {
                        key, pressed: true, ..
                    } = ev
                    {
                        if *key == egui::Key::ArrowDown {
                            down = true;
                        } else if *key == egui::Key::ArrowUp {
                            up = true;
                        }
                    }
                }
                (down, up)
            });
            pressed_down |= raw_arrow.0;
            pressed_up |= raw_arrow.1;
        }
        let pressed_enter = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        } else {
            false
        };
        let pressed_ctrl_a = if allow_list_keys {
            ctx.input(|i| (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::A))
        } else {
            false
        };
        let pressed_left = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft))
        } else {
            false
        };
        let pressed_right = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight))
        } else {
            false
        };
        let pressed_pgdown = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown))
        } else {
            false
        };
        let pressed_pgup = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp))
        } else {
            false
        };
        let pressed_home = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Home))
        } else {
            false
        };
        let pressed_end = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::End))
        } else {
            false
        };
        let pressed_delete = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
        } else {
            false
        };

        if self.is_list_workspace_active() && !self.files.is_empty() && allow_list_keys {
            if pressed_ctrl_a
                || pressed_home
                || pressed_end
                || pressed_pgdown
                || pressed_pgup
                || pressed_down
                || pressed_up
                || pressed_enter
                || pressed_delete
                || pressed_left
                || pressed_right
            {
                ctx.memory_mut(|m| m.request_focus(list_focus_id));
                list_has_focus = true;
                self.search_has_focus = false;
            }
            if pressed_ctrl_a {
                self.selected_multi.clear();
                for i in 0..self.files.len() {
                    self.selected_multi.insert(i);
                }
                if self.selected.is_none() {
                    self.selected = Some(0);
                }
            }
            if pressed_home || pressed_end {
                let len = self.files.len();
                let target = if pressed_home { 0 } else { len.saturating_sub(1) };
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            } else if pressed_pgdown || pressed_pgup {
                let len = self.files.len();
                let cur = self.selected.unwrap_or(0);
                let target = if pressed_pgdown {
                    (cur + metrics.visible_rows).min(len.saturating_sub(1))
                } else {
                    cur.saturating_sub(metrics.visible_rows)
                };
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            } else if pressed_down || pressed_up {
                let len = self.files.len();
                let cur = self.selected.unwrap_or(0);
                let target = if pressed_down {
                    (cur + 1).min(len.saturating_sub(1))
                } else {
                    cur.saturating_sub(1)
                };
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            }
            if pressed_enter && !self.suppress_list_enter {
                let selected = self.selected_paths();
                if !selected.is_empty() {
                    self.open_paths_in_tabs(&selected);
                }
            }
            if pressed_delete {
                let selected = self.selected_paths();
                if !selected.is_empty() {
                    self.remove_paths_from_list_with_undo(&selected);
                }
            }
            if key_moved && self.auto_play_list_nav {
                self.request_list_autoplay();
            }
            if pressed_left || pressed_right {
                let mods = ctx.input(|i| i.modifiers);
                let step = if mods.shift { 0.1 } else { 1.0 };
                let delta = if pressed_left { -step } else { step };
                let mut indices = self.selected_multi.clone();
                if indices.is_empty() {
                    if let Some(i) = self.selected {
                        indices.insert(i);
                    }
                }
                if !indices.is_empty() {
                    self.adjust_gain_for_indices(&indices, delta);
                }
            }
        }

        ListInteractionState {
            key_moved,
            list_focus_id,
            list_has_focus,
        }
    }

    fn list_modal_open(&self) -> bool {
        self.show_rename_dialog
            || self.show_batch_rename_dialog
            || self.show_export_settings
            || self.show_transcription_settings
            || self.show_resample_dialog
            || self.show_leave_prompt
            || self.show_external_dialog
            || self.show_list_art_window
    }
}
