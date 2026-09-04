use super::input_focus::UiSurface;
use super::keymap::{self, Action};
use super::types::{EditorPrimaryView, EditorTab, LoopMode, ToolKind, UndoScope, ViewMode};

impl super::WavesPreviewer {
    pub(super) fn list_focus_id() -> egui::Id {
        egui::Id::new("list_focus")
    }

    pub(super) fn search_box_id() -> egui::Id {
        egui::Id::new("search_box")
    }

    pub(super) fn topbar_volume_id() -> egui::Id {
        egui::Id::new("topbar_volume")
    }

    pub(super) fn request_list_focus(&mut self, ctx: &egui::Context) {
        self.list_has_focus = true;
        self.search_has_focus = false;
        Self::focus_list_widget(ctx);
    }

    /// Give the list keyboard focus without disturbing the lock filter it
    /// already holds.
    ///
    /// egui only lets a widget keep the arrow keys through
    /// `Memory::set_focus_lock_filter`, and that refuses unless the widget
    /// also held focus on the *previous* frame. A fresh `request_focus`
    /// therefore resets the filter to the default one, and for the frame that
    /// follows, egui reads the list's arrows as focus *navigation* instead:
    /// focus walks out of the list into whatever widget sits above or below
    /// it -- the search box, a topbar drag value -- and both of those are text
    /// entry, which stands every list key down until the user clicks back.
    /// That is the row the selection appears to stick on.
    ///
    /// Two rules close it: never re-take focus that is already ours, and when
    /// we do take it, ask for one more frame so the lock is in place *before*
    /// the next key rather than one frame after it.
    pub(super) fn focus_list_widget(ctx: &egui::Context) {
        let id = Self::list_focus_id();
        if ctx.memory(|m| m.has_focus(id)) {
            return;
        }
        ctx.memory_mut(|m| m.request_focus(id));
        ctx.request_repaint();
    }

    pub(super) fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        // A pending rebind capture swallows the pressed chord before any
        // dispatch below (including raw consume_key families) can see it.
        self.keymap_capture_tick(ctx);
        // Modified chords that cannot be typed as text: allowed from anywhere
        // except while a caret is live.
        let allow_global = self.global_keys_allowed();
        // Unmodified keys and per-surface commands: only inside their surface,
        // so an open dialog owns them without blocking the background.
        let allow_workspace = self.workspace_keys_allowed();

        if allow_global && self.keymap_consume(ctx, Action::FocusSearch) {
            ctx.memory_mut(|m| m.request_focus(Self::search_box_id()));
            self.search_has_focus = true;
            self.list_has_focus = false;
        }

        // Space: a transport control, so it works from any workspace surface,
        // but never while typing and never while a dialog is up.
        if allow_workspace {
            if self.keymap_consume(ctx, Action::TogglePlay) {
                self.request_workspace_play_toggle();
            }
        }

        let allow_list_shortcuts = self.surface_keys_allowed(UiSurface::List);
        let allow_volume_shortcuts = allow_workspace;
        if allow_volume_shortcuts {
            if self.keymap_consume(ctx, Action::VolumeDown) {
                self.adjust_volume_db(-1.0);
            }
            if self.keymap_consume(ctx, Action::VolumeUp) {
                self.adjust_volume_db(1.0);
            }
        }

        // Tab switching: Ctrl+1 = List, Ctrl+2.. = editor tabs
        if allow_global {
            let mut target: Option<usize> = None;
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num1)) {
                target = Some(0);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num2)) {
                target = Some(1);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num3)) {
                target = Some(2);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num4)) {
                target = Some(3);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num5)) {
                target = Some(4);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num6)) {
                target = Some(5);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num7)) {
                target = Some(6);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num8)) {
                target = Some(7);
            } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num9)) {
                target = Some(8);
            }
            if let Some(idx) = target {
                if idx == 0 {
                    self.activate_list_workspace(ctx);
                } else {
                    self.activate_editor_tab(idx - 1);
                }
            }
        }

        // Tab / Shift+Tab step through the tab strip: slot 0 is the List and
        // slots 1.. are the editor tabs, the same order `Ctrl+1..` uses.
        //
        // With no editor tab open there is nowhere to step, so Tab stays egui's
        // focus traversal, which is what it is for there; the same goes for the
        // Recording and Effect Graph workspaces, which are not in this cycle.
        // `workspace_keys_allowed` keeps it out of a text field, a metadata
        // field, a modal and the shortcut-capture box -- all of which own their
        // own Tab.
        let in_tab_cycle = self.is_list_workspace_active() || self.is_editor_workspace_active();
        if allow_workspace && in_tab_cycle && !self.tabs.is_empty() {
            // Shift first, and this order is load-bearing: egui matches
            // modifiers *logically*, so a pattern of `NONE` also accepts an
            // event with Shift held. Asking for the bare Tab first would
            // swallow Shift+Tab and step forwards for it.
            let back = ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab));
            let forward =
                !back && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab));
            if forward || back {
                // egui reads Tab for focus traversal at the very start of the
                // pass, before any of this runs, so by now it has already moved
                // focus to some widget in the outgoing tab. Left there it would
                // swallow the *next* Tab press -- and if it landed on a text
                // field, every unmodified key after it. Drop it *before* the
                // switch: the List slot asks for the list's focus on its way
                // in, and surrendering afterwards would take it straight back.
                if let Some(id) = ctx.memory(|m| m.focused()) {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
                let slots = self.tabs.len() as isize + 1;
                let current = if self.is_list_workspace_active() {
                    0
                } else {
                    self.active_tab.map_or(0, |i| i as isize + 1)
                };
                let next = (current + if forward { 1 } else { -1 }).rem_euclid(slots);
                if next == 0 {
                    self.activate_list_workspace(ctx);
                } else {
                    self.activate_editor_tab(next as usize - 1);
                }
            }
        }

        let save_as = allow_global && self.keymap_consume(ctx, Action::SaveSessionAs);
        if allow_global && self.keymap_consume(ctx, Action::NewWindow) {
            self.open_new_window();
        }
        let save = allow_global && self.keymap_consume(ctx, Action::SaveSession);
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
                    self.push_toast(
                        super::types::ToastSeverity::Error,
                        format!("Session save-as failed: {err}"),
                    );
                }
            }
        } else if save {
            if self.is_effect_graph_workspace_active() {
                if let Err(err) = self.save_effect_graph_draft(false) {
                    self.push_effect_graph_console(
                        super::types::EffectGraphSeverity::Error,
                        "library",
                        err,
                        None,
                    );
                }
            } else if let Err(err) = self.save_project() {
                self.debug_log(format!("session save error: {err}"));
                self.push_toast(
                    super::types::ToastSeverity::Error,
                    format!("Session save failed: {err}"),
                );
            }
        }

        if allow_global && self.keymap_consume(ctx, Action::ExportSelected) {
            self.trigger_save_selected();
        }

        if allow_global && self.keymap_consume(ctx, Action::ToggleComments) {
            if self.show_comments_window {
                self.show_comments_window = false;
            } else {
                self.open_comments_window();
            }
        }

        if allow_global && self.keymap_consume(ctx, Action::CloseTab) {
            if self.is_effect_graph_workspace_active() {
                self.request_close_effect_graph_workspace();
            } else if let Some(active_idx) = self.active_tab {
                let dirty = self.tabs.get(active_idx).map(|t| t.dirty).unwrap_or(false);
                if dirty {
                    self.leave_intent = Some(crate::app::LeaveIntent::CloseTab(active_idx));
                    self.show_leave_prompt = true;
                } else {
                    self.close_tab_at(active_idx, ctx);
                }
            }
        }

        if allow_list_shortcuts {
            if self.keymap_consume(ctx, Action::ListToggleAutoplay) {
                self.auto_play_list_nav = !self.auto_play_list_nav;
                self.save_prefs();
            }
            if self.keymap_consume(ctx, Action::ListToggleRegex) {
                self.search_use_regex = !self.search_use_regex;
                self.refresh_filter_then_sort();
            }
        }

        // Editor-specific shortcuts.
        if let Some(tab_idx) = self.active_tab {
            if self.surface_keys_allowed(UiSurface::Editor) {
                if self.keymap_consume(ctx, Action::EditorSetLoopStart) {
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
                    let mut undo_state = None;
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let end = tab.loop_region.map(|(_, e)| e).unwrap_or(pos_now);
                        let s = pos_now.min(end);
                        let e = end.max(s);
                        if tab.loop_region != Some((s, e)) {
                            undo_state =
                                Some(Self::capture_undo_state_labeled(tab, "Set Loop Start"));
                        }
                        tab.loop_region = Some((s, e));
                        Self::update_loop_markers_dirty(tab);
                    }
                    if let Some(state) = undo_state {
                        self.push_editor_undo_state(tab_idx, state, true);
                    }
                }
                if self.keymap_consume(ctx, Action::EditorSetLoopEnd) {
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
                    let mut undo_state = None;
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let start = tab.loop_region.map(|(s, _)| s).unwrap_or(pos_now);
                        let s = start.min(pos_now);
                        let e = pos_now.max(start);
                        if tab.loop_region != Some((s, e)) {
                            undo_state =
                                Some(Self::capture_undo_state_labeled(tab, "Set Loop End"));
                        }
                        tab.loop_region = Some((s, e));
                        Self::update_loop_markers_dirty(tab);
                    }
                    if let Some(state) = undo_state {
                        self.push_editor_undo_state(tab_idx, state, true);
                    }
                }
                // Shift+L before L: egui matches modifiers logically, so the
                // bare-`L` pattern also accepts an event with Shift held and
                // would swallow the cycle.
                if self.keymap_consume(ctx, Action::EditorCycleLoopMode) {
                    self.editor_cycle_loop_mode(tab_idx);
                } else if self.keymap_consume(ctx, Action::EditorToggleMarkerLoop) {
                    if !self.apply_current_loop_region(tab_idx) {
                        if self.has_selected_range(tab_idx) {
                            self.apply_loop_from_selection(tab_idx);
                        } else {
                            // Nothing to make a loop out of: fall back to the
                            // mode cycle, which is what this key did before the
                            // two were separated.
                            self.editor_cycle_loop_mode(tab_idx);
                        }
                    }
                }
                if self.keymap_consume(ctx, Action::EditorCycleViewMode) {
                    let prev = self.tabs[tab_idx].leaf_view_mode();
                    let next = match prev {
                        ViewMode::Waveform => ViewMode::Spectrogram,
                        ViewMode::Spectrogram => ViewMode::Log,
                        ViewMode::Log => ViewMode::Mel,
                        ViewMode::Mel => ViewMode::Tempogram,
                        ViewMode::Tempogram => ViewMode::Chromagram,
                        ViewMode::Chromagram => ViewMode::World,
                        ViewMode::World => ViewMode::Waveform,
                    };
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.set_leaf_view_mode(next);
                        if prev == ViewMode::Waveform && next != ViewMode::Waveform {
                            tab.show_waveform_overlay = false;
                        }
                    }
                    if prev == ViewMode::Waveform && next != ViewMode::Waveform {
                        self.clear_preview_if_any(tab_idx);
                    }
                }
                if self.keymap_consume(ctx, Action::EditorToggleBpm) {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.bpm_enabled = !tab.bpm_enabled;
                    }
                }
                if self.keymap_consume(ctx, Action::EditorAddMarker) {
                    self.add_applied_marker_at_playhead(tab_idx);
                }
                // Digit seek: keyboard row order 1..9,0 spans start -> end
                // (1 = 0%, 2 = 1/9, ..., 9 = 8/9, 0 = 100%). See CONTROLS.md.
                const DIGIT_SEEK: [(egui::Key, usize); 10] = [
                    (egui::Key::Num1, 0),
                    (egui::Key::Num2, 1),
                    (egui::Key::Num3, 2),
                    (egui::Key::Num4, 3),
                    (egui::Key::Num5, 4),
                    (egui::Key::Num6, 5),
                    (egui::Key::Num7, 6),
                    (egui::Key::Num8, 7),
                    (egui::Key::Num9, 8),
                    (egui::Key::Num0, 9),
                ];
                for (key, numer) in DIGIT_SEEK {
                    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                        self.seek_to_fraction_in_active_tab(numer, 9);
                    }
                }
                // Keys below are new additions: gate on the editor workspace
                // actually being visible so a background tab never swallows
                // list-context keys (Home/End) or Escape.
                if self.is_editor_workspace_active() {
                    // The destructive selection keys additionally stand down
                    // while the Metadata inspector is up: the waveform is not
                    // on screen there, and Delete in particular is a key
                    // people press reflexively in a metadata table. They also
                    // stand down on a read-only source (a video), where the
                    // whole tool panel is greyed out and a key must not be the
                    // one way around that.
                    let audio_visible = self.tabs.get(tab_idx).is_some_and(|t| {
                        t.primary_view != EditorPrimaryView::Metadata && !t.read_only
                    });
                    // Select all is not a destructive edit, so unlike the
                    // block below it applies to a read-only source too. It does
                    // need the waveform to be the thing on screen: in the
                    // Metadata inspector, Ctrl+A belongs to whatever table or
                    // field is in front.
                    let waveform_visible = self
                        .tabs
                        .get(tab_idx)
                        .is_some_and(|t| t.primary_view != EditorPrimaryView::Metadata);
                    if waveform_visible && self.keymap_consume(ctx, Action::EditorSelectAll) {
                        let selected = self.tabs.get_mut(tab_idx).and_then(|tab| {
                            (tab.samples_len > 0).then(|| {
                                tab.extra_selections.clear();
                                Self::editor_set_selection_from_anchor(tab, 0, tab.samples_len);
                                tab.samples_len
                            })
                        });
                        if let Some(len) = selected {
                            // Zoomed out, a whole-file selection looks the same
                            // as one that stops just short of either end, so say
                            // outright what happened.
                            let out_sr = self.audio.shared.out_sample_rate.max(1);
                            let sr = self
                                .tabs
                                .get(tab_idx)
                                .map(|tab| Self::editor_display_sample_rate_for_tab(tab, out_sr))
                                .unwrap_or(out_sr)
                                .max(1) as f32;
                            self.push_toast(
                                super::types::ToastSeverity::Info,
                                format!(
                                    "Selected the entire file ({})",
                                    super::helpers::format_time_s(len as f32 / sr)
                                ),
                            );
                        }
                    }
                    if audio_visible {
                        if self.keymap_consume(ctx, Action::EditorTrimSelection)
                            && !self.editor_apply_busy_toast_for_tab(tab_idx)
                        {
                            let ranges = self.all_selected_ranges(tab_idx);
                            let fired = if ranges.len() > 1 {
                                self.editor_apply_trim_multi_ranges(tab_idx, ranges);
                                true
                            } else if let Some((s, e)) = self.selected_range(tab_idx) {
                                self.editor_apply_trim_range(tab_idx, (s, e));
                                true
                            } else {
                                false
                            };
                            if fired {
                                self.push_toast(
                                    super::types::ToastSeverity::Info,
                                    "Trimmed to selection (Ctrl+Z to undo)",
                                );
                            }
                        }
                        if self.keymap_consume(ctx, Action::EditorVirtualTrim)
                            && !self.editor_apply_busy_toast_for_tab(tab_idx)
                        {
                            let ranges = self.all_selected_ranges(tab_idx);
                            if ranges.len() > 1 {
                                if let Some(path) = self.tabs.get(tab_idx).map(|t| t.path.clone()) {
                                    let mut iter = ranges.into_iter();
                                    if let Some((s, e)) = iter.next() {
                                        self.begin_trim_virtual_job(tab_idx, (s, e));
                                    }
                                    for (s, e) in iter {
                                        self.virtual_trim_queue.push_back((path.clone(), s, e));
                                    }
                                }
                            } else {
                                self.try_add_trim_range_as_virtual_shortcut(tab_idx);
                            }
                        }
                        // Delete is shared with the list and the effect graph,
                        // whose handlers run earlier in the frame under their own
                        // workspace guards. This one must be scoped the same way
                        // or a background editor tab would eat the list's Delete.
                        if self.keymap_consume(ctx, Action::EditorDeleteSelection)
                            && !self.editor_apply_busy_toast_for_tab(tab_idx)
                        {
                            let ranges = self.all_selected_ranges(tab_idx);
                            let fired = if ranges.len() > 1 {
                                self.editor_delete_multi_ranges_and_join(tab_idx, ranges);
                                true
                            } else if let Some((s, e)) = self.selected_range(tab_idx) {
                                self.editor_delete_range_and_join(tab_idx, (s, e));
                                true
                            } else {
                                false
                            };
                            if fired {
                                self.push_toast(
                                    super::types::ToastSeverity::Info,
                                    "Deleted selection (Ctrl+Z to undo)",
                                );
                            }
                        }
                        // Mute every selected range, matching what the Trim
                        // inspector's Mode=Mute Apply does (primary + extras) so
                        // the key and the button cannot disagree.
                        if self.keymap_consume(ctx, Action::EditorMuteSelection)
                            && !self.editor_apply_busy_toast_for_tab(tab_idx)
                        {
                            let ranges = self.all_selected_ranges(tab_idx);
                            let fired = if ranges.len() > 1 {
                                self.editor_apply_mute_multi_ranges(tab_idx, ranges);
                                true
                            } else if let Some((s, e)) = self.selected_range(tab_idx) {
                                self.editor_apply_mute_range(tab_idx, (s, e));
                                true
                            } else {
                                false
                            };
                            if fired {
                                self.push_toast(
                                    super::types::ToastSeverity::Info,
                                    "Muted selection (Ctrl+Z to undo)",
                                );
                            }
                        }
                    }
                    if self.keymap_consume(ctx, Action::EditorSeekStart) {
                        self.seek_to_fraction_in_active_tab(0, 9);
                    }
                    if self.keymap_consume(ctx, Action::EditorSeekEnd) {
                        self.seek_to_fraction_in_active_tab(9, 9);
                    }
                    // Shift+Z before Z: egui matches modifiers logically, so
                    // the bare-`Z` pattern also accepts an event with Shift
                    // held and would swallow this one.
                    if self.keymap_consume(ctx, Action::EditorZoomToLoopRegion) {
                        self.editor_zoom_to_loop_region(tab_idx);
                    } else if self.keymap_consume(ctx, Action::EditorZoomToSelection) {
                        self.editor_zoom_to_selection(tab_idx);
                    }
                    // `=` shares the physical key with `+` on many layouts, so
                    // accept it as an unshifted zoom-in fallback — but only
                    // for the BUILT-IN chord: a user override replaces both
                    // keys, and a pending rebind capture must swallow it.
                    let zoom_in_fallback = self.keymap_capture.is_none()
                        && !self.keymap_overrides.contains_key(&Action::EditorZoomIn)
                        && ctx
                            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Equals));
                    if self.keymap_consume(ctx, Action::EditorZoomIn) || zoom_in_fallback {
                        self.editor_zoom_step_at_playhead(tab_idx, true);
                    }
                    if self.keymap_consume(ctx, Action::EditorZoomOut) {
                        self.editor_zoom_step_at_playhead(tab_idx, false);
                    }
                    if self.keymap_consume(ctx, Action::EditorViewPageBack) {
                        self.editor_view_page(tab_idx, false);
                    }
                    if self.keymap_consume(ctx, Action::EditorViewPageForward) {
                        self.editor_view_page(tab_idx, true);
                    }
                    let has_preview = self
                        .tabs
                        .get(tab_idx)
                        .map(|t| t.preview_audio_tool.is_some() || t.preview_overlay.is_some())
                        .unwrap_or(false);
                    if has_preview && self.keymap_consume(ctx, Action::EditorCancelPreview) {
                        let pencil_draft = self
                            .tabs
                            .get(tab_idx)
                            .is_some_and(|tab| tab.pencil_draft.is_some());
                        if pencil_draft {
                            self.editor_pencil_cancel_draft(tab_idx);
                        } else {
                            self.clear_preview_if_any(tab_idx);
                        }
                    }
                }
            }
        }
    }

    /// Consume the effective chord for a Table-dispatched action: the user
    /// override when present, the built-in chord otherwise. While the rebind
    /// window is capturing a key, nothing dispatches so the pressed chord
    /// only lands in the capture field.
    pub(super) fn keymap_consume(&self, ctx: &egui::Context, action: Action) -> bool {
        if self.keymap_capture.is_some() {
            return false;
        }
        if let Some(&(mods, key)) = self.keymap_overrides.get(&action) {
            return ctx.input_mut(|i| i.consume_key(mods.to_modifiers(), key));
        }
        keymap::consume(ctx, action)
    }

    pub(super) fn keymap_effective_chord(
        &self,
        action: Action,
    ) -> Option<(keymap::Mods, egui::Key)> {
        if let Some(&chord) = self.keymap_overrides.get(&action) {
            return Some(chord);
        }
        keymap::binding(action).and_then(|b| b.chord)
    }

    fn adjust_volume_db(&mut self, delta_db: f32) {
        let next = (self.volume_db + delta_db).clamp(-80.0, 6.0);
        if (next - self.volume_db).abs() >= f32::EPSILON {
            self.volume_db = next;
            self.apply_effective_volume();
            self.save_prefs();
        }
    }

    pub(super) fn all_selected_ranges(&self, tab_idx: usize) -> Vec<(usize, usize)> {
        self.tabs
            .get(tab_idx)
            .map(EditorTab::all_selected_ranges)
            .unwrap_or_default()
    }

    fn selected_range(&self, tab_idx: usize) -> Option<(usize, usize)> {
        let tab = self.tabs.get(tab_idx)?;
        let (a0, b0) = tab.selection?;
        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
        if b > a {
            Some((a, b))
        } else {
            None
        }
    }

    fn has_selected_range(&self, tab_idx: usize) -> bool {
        self.selected_range(tab_idx).is_some()
    }

    fn try_add_trim_range_as_virtual_shortcut(&mut self, tab_idx: usize) -> bool {
        if !self.is_editor_workspace_active() {
            return false;
        }
        let selected_range = self.selected_range(tab_idx);
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let range = if let Some(range) = selected_range {
            Some(range)
        } else if tab.active_tool == ToolKind::Trim {
            tab.trim_range
        } else {
            None
        };
        let Some((a, b)) = range else {
            return false;
        };
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        if e <= s {
            return false;
        }
        self.begin_trim_virtual_job(tab_idx, (s, e))
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
            undo_state = Some(Self::capture_undo_state_labeled(tab, "Add Marker"));
            let label = Self::next_marker_label(&tab.markers);
            let marker = crate::markers::MarkerEntry { sample: pos, label };
            match tab.markers.binary_search_by_key(&pos, |m| m.sample) {
                Ok(idx) | Err(idx) => tab.markers.insert(idx, marker),
            }
            Self::update_markers_dirty(tab);
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
            let will_change = tab.loop_region != Some((s, e)) || tab.loop_mode != LoopMode::Marker;
            if will_change {
                undo_state = Some(Self::capture_undo_state_labeled(tab, "Set Marker Loop"));
            }
            tab.loop_region = Some((s, e));
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

    fn apply_current_loop_region(&mut self, tab_idx: usize) -> bool {
        let mut undo_state = None;
        let Some(current) = self.tabs.get(tab_idx).and_then(|tab| tab.loop_region) else {
            return false;
        };
        let should_apply = self
            .tabs
            .get(tab_idx)
            .map(|tab| {
                tab.loop_region_committed != Some(current)
                    || tab.loop_region_applied != Some(current)
                    || tab.pending_loop_unwrap.is_some()
            })
            .unwrap_or(false);
        if !should_apply {
            return false;
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            let will_change = tab.loop_region_committed != Some(current)
                || tab.loop_region_applied != Some(current)
                || tab.loop_mode != LoopMode::Marker
                || tab.pending_loop_unwrap.is_some();
            if will_change {
                undo_state = Some(Self::capture_undo_state_labeled(tab, "Apply Loop"));
            }
            tab.loop_region_committed = Some(current);
            tab.loop_region_applied = Some(current);
            tab.loop_mode = LoopMode::Marker;
            tab.pending_loop_unwrap = None;
            Self::update_loop_markers_dirty(tab);
        }
        if let Some(state) = undo_state {
            self.push_editor_undo_state(tab_idx, state, true);
        }
        if let Some(tab_ro) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab_ro);
        }
        true
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
            let vis =
                (tab_mut.last_wave_w.max(1.0) * tab_mut.samples_per_px.max(0.0001)).ceil() as usize;
            let max_left = tab_mut.samples_len.saturating_sub(vis);
            let left = target_display.saturating_sub(vis / 2);
            tab_mut.view_offset = left.min(max_left);
            tab_mut.view_offset_exact = tab_mut.view_offset as f64;
        }
    }

    /// Clip an arrow-key seek to the first landmark it would have stepped over.
    ///
    /// The arrow keys move by a grid step, not from landmark to landmark, so
    /// this is what makes them land *on* things: if the step would jump the
    /// playhead across a landmark, it stops there instead.
    ///
    /// Loop start and end count as landmarks alongside markers. They are the
    /// two positions people most often need the playhead exactly on -- to
    /// audition a seam, or to check where a loop was placed -- and before this
    /// the only way to reach one exactly was to zoom in far enough that a grid
    /// step was a single sample.
    ///
    /// Returns one position, so a marker sitting on a loop point stops the
    /// playhead once rather than twice.
    /// Step the loop mode: off -> whole file -> marker loop -> off.
    ///
    /// A loop region forces marker loop, since that is the only mode that uses
    /// one and landing anywhere else with a region set reads as the key having
    /// done nothing.
    pub(super) fn editor_cycle_loop_mode(&mut self, tab_idx: usize) {
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

    pub(super) fn stop_at_landmark_if_needed(
        tab: &super::types::EditorTab,
        current_display: usize,
        target_display: usize,
        dir: i32,
    ) -> usize {
        if dir == 0 || target_display == current_display {
            return target_display;
        }
        // Markers are kept sorted, but folding the loop points in breaks that,
        // so take the nearest in the direction of travel rather than the first
        // one the iterator offers.
        let landmarks = tab.markers.iter().map(|m| m.sample).chain(
            crate::app::WavesPreviewer::normalized_loop_range(tab.loop_region)
                .into_iter()
                .flat_map(|(a, b)| [a, b]),
        );
        if dir > 0 {
            landmarks
                .filter(|&s| s > current_display && s <= target_display)
                .min()
                .unwrap_or(target_display)
        } else {
            landmarks
                .filter(|&s| s < current_display && s >= target_display)
                .max()
                .unwrap_or(target_display)
        }
    }

    pub(super) fn handle_undo_redo_hotkeys(&mut self, ctx: &egui::Context) {
        // Ctrl+Z/Y belong to whatever text field has the caret, not to the app.
        if !self.global_keys_allowed() {
            return;
        }
        let cmd_down = ctx.input(|i| i.modifiers.command);
        let z_down = ctx.input(|i| i.key_down(egui::Key::Z));
        let y_down = ctx.input(|i| i.key_down(egui::Key::Y));
        let combo_down = cmd_down && (z_down || y_down);
        if combo_down && self.undo_z_was_down {
            return;
        }
        let undo = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Z));
        let redo_z = ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            )
        });
        let redo_y = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Y));
        let redo = redo_z || redo_y;
        self.undo_z_was_down = cmd_down && (z_down || y_down);
        if !(undo || redo) {
            return;
        }
        let handled = self.trigger_undo_redo(redo);
        if handled {
            if self.debug.cfg.enabled && self.debug.input_trace_enabled {
                let tag = if redo { "redo" } else { "undo" };
                self.debug_trace_input(format!("{tag} triggered via hotkey"));
            }
            ctx.request_repaint();
        }
        self.undo_z_was_down = combo_down;
    }

    /// Scope-aware undo/redo dispatch shared by the Ctrl+Z/Y hotkeys and
    /// the Edit menu: effect graph when active, then the last-used scope,
    /// then the active editor tab, then the list, then overwrite-export.
    pub(super) fn trigger_undo_redo(&mut self, redo: bool) -> bool {
        let mut handled = false;
        let prefer_graph = self.is_effect_graph_workspace_active()
            || self.last_undo_scope == UndoScope::EffectGraph;
        if prefer_graph {
            handled = if redo {
                self.effect_graph_redo()
            } else {
                self.effect_graph_undo()
            };
        }
        let prefer_list = self.last_undo_scope == UndoScope::List;
        if !handled && prefer_list {
            handled = if redo {
                self.list_redo()
            } else {
                self.list_undo()
            };
        }
        if !handled {
            if let Some(tab_idx) = self.active_tab {
                let pencil_draft_active = self
                    .tabs
                    .get(tab_idx)
                    .is_some_and(|tab| tab.pencil_draft.is_some());
                if pencil_draft_active {
                    let _changed = if redo {
                        self.editor_pencil_redo_draft(tab_idx)
                    } else {
                        self.editor_pencil_undo_draft(tab_idx)
                    };
                    self.last_undo_scope = UndoScope::Editor;
                    // While a Pencil draft exists, Ctrl+Z/Y belongs only to
                    // its stroke history. Do not leak through to committed
                    // editor history when the local stack reaches an end.
                    return true;
                }
                self.clear_preview_if_any(tab_idx);
                self.cancel_editor_apply_for_tab(tab_idx);
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
            handled = if redo {
                self.list_redo()
            } else {
                self.list_undo()
            };
        }
        if !handled && !redo {
            handled = self.undo_last_overwrite_export();
        }
        handled
    }

    /// Whether any undo (or redo) scope currently has something to apply —
    /// drives the Edit menu enabled state without mutating anything.
    pub(super) fn undo_redo_available(&self, redo: bool) -> bool {
        let graph = if redo {
            !self.effect_graph.redo_stack.is_empty()
        } else {
            !self.effect_graph.undo_stack.is_empty()
        };
        let editor = self
            .active_tab
            .and_then(|idx| self.tabs.get(idx))
            .map(|tab| {
                if redo {
                    !tab.redo_stack.is_empty()
                } else {
                    !tab.undo_stack.is_empty()
                }
            })
            .unwrap_or(false);
        let list = if redo {
            !self.list_redo_stack.is_empty()
        } else {
            !self.list_undo_stack.is_empty()
        };
        // Ctrl+Z's final fallback restores overwrite-export backups; the
        // Edit menu must not gray Undo out while that path would fire.
        let overwrite_export = !redo && !self.overwrite_undo_stack.is_empty();
        graph || editor || list || overwrite_export
    }
}

#[cfg(test)]
mod landmark_tests {
    use crate::app::types::EditorTab;
    use crate::app::WavesPreviewer;
    use crate::markers::MarkerEntry;

    fn tab(markers: &[usize], loop_region: Option<(usize, usize)>) -> EditorTab {
        let mut tab = EditorTab::new_base(std::path::PathBuf::from("/t.wav"), "t.wav".to_string());
        tab.markers = markers
            .iter()
            .map(|&sample| MarkerEntry {
                sample,
                label: String::new(),
            })
            .collect();
        tab.loop_region = loop_region;
        tab
    }

    fn stop(tab: &EditorTab, from: usize, to: usize, dir: i32) -> usize {
        WavesPreviewer::stop_at_landmark_if_needed(tab, from, to, dir)
    }

    #[test]
    fn a_step_that_clears_everything_lands_where_it_meant_to() {
        let t = tab(&[500], Some((600, 700)));
        assert_eq!(stop(&t, 100, 200, 1), 200);
        assert_eq!(stop(&t, 900, 800, -1), 800);
        // dir 0 and a step of nothing are both pass-throughs.
        assert_eq!(stop(&t, 100, 900, 0), 900);
        assert_eq!(stop(&t, 100, 100, 1), 100);
    }

    #[test]
    fn markers_still_stop_the_step() {
        let t = tab(&[150, 400], None);
        assert_eq!(stop(&t, 100, 300, 1), 150);
        assert_eq!(stop(&t, 500, 300, -1), 400);
    }

    #[test]
    fn loop_points_stop_the_step_too() {
        let t = tab(&[], Some((300, 700)));
        assert_eq!(stop(&t, 100, 500, 1), 300, "loop start going right");
        assert_eq!(stop(&t, 500, 900, 1), 700, "loop end going right");
        assert_eq!(stop(&t, 900, 500, -1), 700, "loop end going left");
        assert_eq!(stop(&t, 500, 100, -1), 300, "loop start going left");
    }

    #[test]
    fn the_nearest_landmark_wins_whichever_kind_it_is() {
        // The markers are sorted but the loop points are not folded into that
        // order, so taking the first candidate the iterator offers would step
        // straight past the loop start to the marker beyond it.
        let t = tab(&[250, 900], Some((400, 800)));
        assert_eq!(stop(&t, 100, 950, 1), 250, "marker before the loop start");
        assert_eq!(stop(&t, 300, 950, 1), 400, "loop start before the marker");
        assert_eq!(stop(&t, 950, 100, -1), 900, "marker after the loop end");
        assert_eq!(stop(&t, 850, 100, -1), 800, "loop end before the marker");
    }

    #[test]
    fn a_marker_on_a_loop_point_stops_the_playhead_once() {
        let t = tab(&[400], Some((400, 800)));
        // Both name sample 400, and one position comes back, so the next press
        // continues past it instead of stopping on the same sample again.
        assert_eq!(stop(&t, 100, 600, 1), 400);
        assert_eq!(stop(&t, 400, 600, 1), 600);
    }

    #[test]
    fn a_landmark_exactly_on_the_target_is_where_the_step_ends_anyway() {
        let t = tab(&[], Some((300, 700)));
        assert_eq!(stop(&t, 100, 300, 1), 300);
        assert_eq!(stop(&t, 900, 700, -1), 700);
        // A landmark the step starts on must not hold it there.
        assert_eq!(stop(&t, 300, 500, 1), 500);
        assert_eq!(stop(&t, 700, 500, -1), 500);
    }

    #[test]
    fn a_reversed_loop_region_is_normalized_before_it_is_used() {
        let t = tab(&[], Some((700, 300)));
        assert_eq!(stop(&t, 100, 500, 1), 300);
        assert_eq!(stop(&t, 900, 500, -1), 700);
    }
}
