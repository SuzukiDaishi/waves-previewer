use egui::Key;

use super::types::{LoopMode, UndoScope};

impl super::WavesPreviewer {
    pub(super) fn list_focus_id() -> egui::Id {
        egui::Id::new("list_focus")
    }

    pub(super) fn search_box_id() -> egui::Id {
        egui::Id::new("search_box")
    }

    pub(super) fn request_list_focus(&mut self, ctx: &egui::Context) {
        self.list_has_focus = true;
        self.search_has_focus = false;
        ctx.memory_mut(|m| {
            m.request_focus(Self::list_focus_id());
        });
    }

    pub(super) fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        let wants_kb = ctx.wants_keyboard_input();
        let search_focused = ctx.memory(|m| m.has_focus(Self::search_box_id()));

        if !search_focused {
            if ctx
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space))
            {
                // Keep preview audio/overlay when toggling playback.
                self.audio.toggle_play();
            }
        }

        // Tab switching: Ctrl+1 = List, Ctrl+2.. = editor tabs
        if !search_focused || !wants_kb {
            let mut target: Option<usize> = None;
            if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num1)
            }) {
                target = Some(0);
            } else if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num2)
            }) {
                target = Some(1);
            } else if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num3)
            }) {
                target = Some(2);
            } else if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num4)
            }) {
                target = Some(3);
            } else if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num5)
            }) {
                target = Some(4);
            } else if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num6)
            }) {
                target = Some(5);
            } else if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num7)
            }) {
                target = Some(6);
            } else if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num8)
            }) {
                target = Some(7);
            } else if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num9)
            }) {
                target = Some(8);
            }
            if let Some(idx) = target {
                if idx == 0 {
                    if let Some(prev) = self.active_tab {
                        self.clear_preview_if_any(prev);
                    }
                    self.active_tab = None;
                    self.audio.stop();
                    self.audio.set_loop_enabled(false);
                    self.request_list_focus(ctx);
                } else {
                    let tab_idx = idx - 1;
                    if tab_idx < self.tabs.len() {
                        if let Some(prev) = self.active_tab {
                            if prev != tab_idx {
                                self.clear_preview_if_any(prev);
                            }
                        }
                        if let Some(tab) = self.tabs.get(tab_idx) {
                            self.active_tab = Some(tab_idx);
                            self.audio.stop();
                            self.pending_activate_path = Some(tab.path.clone());
                        }
                    }
                }
            }
        }

        let save_as = ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::S,
            )
        });
        let save = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
        if save_as {
            if let Some(mut path) = self.pick_project_save_dialog() {
                let needs_ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| !s.eq_ignore_ascii_case("nwsess"))
                    .unwrap_or(true);
                if needs_ext {
                    path.set_extension("nwsess");
                }
                if let Err(err) = self.save_project_as(path) {
                    self.debug_log(format!("session save-as error: {err}"));
                }
            }
        } else if save {
            if let Err(err) = self.save_project() {
                self.debug_log(format!("session save error: {err}"));
            }
        }

        if ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::E)
        }) {
            self.trigger_save_selected();
        }

        if ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)
        }) {
            if let Some(active_idx) = self.active_tab {
                self.close_tab_at(active_idx, ctx);
            }
        }

        // Editor-specific shortcuts: Loop region setters, Loop toggle (L), Zero-cross snap (S)
        if let Some(tab_idx) = self.active_tab {
            if !wants_kb {
                if ctx
                    .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::K))
                {
                    // Set Loop Start
                    let pos_audio = self
                        .audio
                        .shared
                        .play_pos
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let pos_now = self
                        .tabs
                        .get(tab_idx)
                        .map(|tab_ro| self.map_audio_to_display_sample(tab_ro, pos_audio))
                        .unwrap_or(0);
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let end = tab.loop_region.map(|(_, e)| e).unwrap_or(pos_now);
                        let s = pos_now.min(end);
                        let e = end.max(s);
                        tab.loop_region = Some((s, e));
                        Self::update_loop_markers_dirty(tab);
                    }
                }
                if ctx
                    .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::P))
                {
                    // Set Loop End
                    let pos_audio = self
                        .audio
                        .shared
                        .play_pos
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let pos_now = self
                        .tabs
                        .get(tab_idx)
                        .map(|tab_ro| self.map_audio_to_display_sample(tab_ro, pos_audio))
                        .unwrap_or(0);
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let start = tab.loop_region.map(|(s, _)| s).unwrap_or(pos_now);
                        let s = start.min(pos_now);
                        let e = pos_now.max(start);
                        tab.loop_region = Some((s, e));
                        Self::update_loop_markers_dirty(tab);
                    }
                }
                if ctx
                    .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::L))
                {
                    // Toggle loop mode without holding a mutable borrow across &self call
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.loop_mode = match tab.loop_mode {
                            LoopMode::Off => LoopMode::OnWhole,
                            _ => LoopMode::Off,
                        };
                    }
                    if let Some(tab_ro) = self.tabs.get(tab_idx) {
                        self.apply_loop_mode_for_tab(tab_ro);
                    }
                }
                if ctx
                    .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::S))
                {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.snap_zero_cross = !tab.snap_zero_cross;
                    }
                }
            }
        }
    }

    pub(super) fn handle_undo_redo_hotkeys(&mut self, ctx: &egui::Context) {
        let search_focused = ctx.memory(|m| m.has_focus(Self::search_box_id()));
        if search_focused {
            return;
        }
        let allow = self.list_has_focus || self.active_tab.is_some() || self.selected.is_some();
        if !allow {
            return;
        }
        let undo = ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z)
        });
        let redo = ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z)
        });
        self.undo_z_was_down = ctx.input(|i| i.key_down(Key::Z));
        if !(undo || redo) {
            return;
        }
        let mut handled = false;
        let prefer_list = self.last_undo_scope == UndoScope::List;
        if prefer_list {
            handled = if redo { self.list_redo() } else { self.list_undo() };
        }
        if !handled {
            if let Some(tab_idx) = self.active_tab {
                self.clear_preview_if_any(tab_idx);
                self.editor_apply_state = None;
                let changed = if redo {
                    self.redo_in_tab(tab_idx)
                } else {
                    self.undo_in_tab(tab_idx)
                };
                if changed {
                    self.last_undo_scope = UndoScope::Editor;
                    handled = true;
                }
            }
        }
        if !handled {
            handled = if redo { self.list_redo() } else { self.list_undo() };
        }
        if handled {
            if self.debug.cfg.enabled && self.debug.input_trace_enabled {
                let tag = if redo { "redo" } else { "undo" };
                self.debug_trace_input(format!("{tag} triggered via hotkey"));
            }
            ctx.request_repaint();
        }
    }
}
