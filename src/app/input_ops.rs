use super::types::{LoopMode, UndoScope, ViewMode};

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

        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            ctx.memory_mut(|m| m.request_focus(Self::search_box_id()));
            self.search_has_focus = true;
            self.list_has_focus = false;
        }

        if !search_focused {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space)) {
                // Keep preview audio/overlay when toggling playback.
                self.audio.toggle_play();
            }
        }

        if !wants_kb {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::A)) {
                self.adjust_volume_db(-1.0);
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::D)) {
                self.adjust_volume_db(1.0);
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

        if self.active_tab.is_none() && !wants_kb {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::P)) {
                self.auto_play_list_nav = !self.auto_play_list_nav;
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::R)) {
                self.search_use_regex = !self.search_use_regex;
                self.apply_filter_from_search();
            }
        }

        // Editor-specific shortcuts.
        if let Some(tab_idx) = self.active_tab {
            if !wants_kb {
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::K)) {
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
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::P)) {
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
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::L)) {
                    if self.has_selected_range(tab_idx) {
                        self.apply_loop_from_selection(tab_idx);
                    } else {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.loop_mode = if tab.loop_region.is_some() {
                                LoopMode::Marker
                            } else {
                                match tab.loop_mode {
                                    LoopMode::Off => LoopMode::OnWhole,
                                    LoopMode::OnWhole => LoopMode::Marker,
                                    LoopMode::Marker => LoopMode::Off,
                                }
                            };
                        }
                        if let Some(tab_ro) = self.tabs.get(tab_idx) {
                            self.apply_loop_mode_for_tab(tab_ro);
                        }
                    }
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::S)) {
                    let prev = self.tabs[tab_idx].view_mode;
                    let next = match prev {
                        ViewMode::Waveform => ViewMode::Spectrogram,
                        ViewMode::Spectrogram => ViewMode::Mel,
                        ViewMode::Mel => ViewMode::Waveform,
                    };
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.view_mode = next;
                        if prev == ViewMode::Waveform && next != ViewMode::Waveform {
                            tab.show_waveform_overlay = false;
                        }
                    }
                    if prev == ViewMode::Waveform && next != ViewMode::Waveform {
                        self.clear_preview_if_any(tab_idx);
                        self.audio.stop();
                    }
                    if next != ViewMode::Waveform {
                        let path = self.tabs[tab_idx].path.clone();
                        self.cancel_spectrogram_for_path(&path);
                    }
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::B)) {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.bpm_enabled = !tab.bpm_enabled;
                    }
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::M)) {
                    self.add_applied_marker_at_playhead(tab_idx);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::R)) {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.snap_zero_cross = !tab.snap_zero_cross;
                    }
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::C)) {
                    if let Some((s, e)) = self.selected_range(tab_idx) {
                        self.editor_delete_range_and_join(tab_idx, (s, e));
                    }
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::T)) {
                    if let Some((s, e)) = self.selected_range(tab_idx) {
                        self.editor_apply_trim_range(tab_idx, (s, e));
                    }
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num0)) {
                    self.seek_to_fraction_in_active_tab(1, 1);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num1)) {
                    self.seek_to_fraction_in_active_tab(1, 1);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num2)) {
                    self.seek_to_fraction_in_active_tab(1, 2);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num3)) {
                    self.seek_to_fraction_in_active_tab(1, 3);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num4)) {
                    self.seek_to_fraction_in_active_tab(1, 4);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num5)) {
                    self.seek_to_fraction_in_active_tab(1, 5);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num6)) {
                    self.seek_to_fraction_in_active_tab(1, 6);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num7)) {
                    self.seek_to_fraction_in_active_tab(1, 7);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num8)) {
                    self.seek_to_fraction_in_active_tab(1, 8);
                }
                if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Num9)) {
                    self.seek_to_fraction_in_active_tab(1, 9);
                }
            }
        }
    }

    fn adjust_volume_db(&mut self, delta_db: f32) {
        let next = (self.volume_db + delta_db).clamp(-80.0, 6.0);
        if (next - self.volume_db).abs() >= f32::EPSILON {
            self.volume_db = next;
            self.apply_effective_volume();
        }
    }

    fn selected_range(&self, tab_idx: usize) -> Option<(usize, usize)> {
        let tab = self.tabs.get(tab_idx)?;
        let (a0, b0) = tab.selection?;
        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
        if b > a { Some((a, b)) } else { None }
    }

    fn has_selected_range(&self, tab_idx: usize) -> bool {
        self.selected_range(tab_idx).is_some()
    }

    fn add_applied_marker_at_playhead(&mut self, tab_idx: usize) {
        let pos_audio = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        let Some(tab_ro) = self.tabs.get(tab_idx) else {
            return;
        };
        let pos = self.map_audio_to_display_sample(tab_ro, pos_audio);
        let mut undo_state = None;
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            if tab.markers.iter().any(|m| m.sample == pos) {
                return;
            }
            undo_state = Some(Self::capture_undo_state(tab));
            let label = Self::next_marker_label(&tab.markers);
            let marker = crate::markers::MarkerEntry { sample: pos, label };
            match tab.markers.binary_search_by_key(&pos, |m| m.sample) {
                Ok(idx) | Err(idx) => tab.markers.insert(idx, marker),
            }
            tab.markers_committed = tab.markers.clone();
            tab.markers_applied = tab.markers_committed.clone();
            tab.markers_dirty = tab.markers_committed != tab.markers_saved;
        }
        if let Some(state) = undo_state {
            self.push_editor_undo_state(tab_idx, state, true);
        }
    }

    fn apply_loop_from_selection(&mut self, tab_idx: usize) {
        let Some((s, e)) = self.selected_range(tab_idx) else {
            return;
        };
        let mut undo_state = None;
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            let will_change = tab.loop_region_committed != Some((s, e))
                || tab.loop_region_applied != Some((s, e));
            if will_change {
                undo_state = Some(Self::capture_undo_state(tab));
            }
            tab.loop_region = Some((s, e));
            tab.loop_region_committed = Some((s, e));
            tab.loop_region_applied = Some((s, e));
            tab.loop_mode = LoopMode::Marker;
            Self::update_loop_markers_dirty(tab);
        }
        if let Some(state) = undo_state {
            self.push_editor_undo_state(tab_idx, state, true);
        }
        if let Some(tab_ro) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab_ro);
        }
    }

    fn seek_to_fraction_in_active_tab(&mut self, numer: usize, denom: usize) {
        let Some(tab_idx) = self.active_tab else {
            return;
        };
        if denom == 0 {
            return;
        }
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let target_display = tab.samples_len.saturating_mul(numer) / denom;
        let target_audio = self.map_display_to_audio_sample(tab, target_display);
        self.audio.seek_to_sample(target_audio);
        if let Some(tab_mut) = self.tabs.get_mut(tab_idx) {
            let vis = (tab_mut.last_wave_w.max(1.0) * tab_mut.samples_per_px.max(0.0001)).ceil()
                as usize;
            let max_left = tab_mut.samples_len.saturating_sub(vis);
            let left = target_display.saturating_sub(vis / 2);
            tab_mut.view_offset = left.min(max_left);
        }
    }

    pub(super) fn stop_with_marker_if_needed(
        tab: &super::types::EditorTab,
        current_display: usize,
        target_display: usize,
        dir: i32,
    ) -> usize {
        if dir == 0 || target_display == current_display {
            return target_display;
        }
        if dir > 0 {
            if let Some(marker) = tab
                .markers
                .iter()
                .find(|m| m.sample > current_display && m.sample <= target_display)
            {
                return marker.sample;
            }
        } else if let Some(marker) = tab
            .markers
            .iter()
            .rev()
            .find(|m| m.sample < current_display && m.sample >= target_display)
        {
            return marker.sample;
        }
        target_display
    }

    pub(super) fn handle_undo_redo_hotkeys(&mut self, ctx: &egui::Context) {
        let search_focused = ctx.memory(|m| m.has_focus(Self::search_box_id()));
        if search_focused && ctx.wants_keyboard_input() {
            return;
        }
        let cmd_down = ctx.input(|i| i.modifiers.command);
        let shift_down = ctx.input(|i| i.modifiers.shift);
        let z_down = ctx.input(|i| i.key_down(egui::Key::Z));
        let y_down = ctx.input(|i| i.key_down(egui::Key::Y));
        let combo_down = cmd_down && (z_down || y_down);
        if combo_down && self.undo_z_was_down {
            return;
        }
        let undo = ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z)
        });
        let redo_z = ctx.input_mut(|i| {
            i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z)
        });
        let redo_y =
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y));
        let redo = redo_z || redo_y;
        self.undo_z_was_down = cmd_down && ((shift_down && z_down) || (!shift_down && z_down) || y_down);
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
        self.undo_z_was_down = combo_down;
    }
}
