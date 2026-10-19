//! What the Inspector asks for, carried out. The panels under
//! `ui/inspector/` only draw and collect [`InspectorAction`]s; everything that
//! touches more than the tab's settings -- switching tools, applying,
//! previewing, scanning -- happens here, after the panel has let go of the
//! tab. The same functions back the shortcuts (Ctrl+Enter, Esc) and the
//! kittest helpers, so a tool behaves the same however it is driven.

use std::time::Instant;

use super::editor_tool::PreviewKind;
use super::tool_render::RenderTarget;
use super::tool_session::DraftSnapshot;
use super::types::{EditorTab, SpectralBrushStamp, SpectralWarpPoint, ToolKind, ToolState};
use super::{WavesPreviewer, LIVE_PREVIEW_SAMPLE_LIMIT};

/// The strokes an apply took off a tab, kept until the apply is known to
/// have started so that a refused one can put them back.
pub(in crate::app) enum SpentStrokes {
    None,
    Warp(Vec<SpectralWarpPoint>),
    Brush(Vec<SpectralBrushStamp>),
}

impl SpentStrokes {
    pub(in crate::app) fn restore(self, tab: &mut EditorTab) {
        match self {
            SpentStrokes::None => {}
            SpentStrokes::Warp(points) => tab.spectral_warp_points = points,
            SpentStrokes::Brush(stamps) => tab.spectral_brush_stamps = stamps,
        }
    }
}

/// Something the Inspector asks for.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) enum InspectorAction {
    SelectTool(ToolKind),
    /// Render the current settings now (the Preview / Refresh button).
    Preview,
    /// Re-render on every change (`false`) or only on request (`true`).
    SetManual(bool),
    /// Hear the original instead of the preview, or back.
    ToggleAb,
    /// Esc: back to where the session started.
    Discard,
    Apply,
    /// The tool's settings back to their defaults.
    ResetSettings,
    /// Stop the apply running on this tab.
    CancelApply,
    /// Play the span the tool acts on.
    PlayTarget,
    Tool(ToolCommand),
}

/// A tool's own buttons.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) enum ToolCommand {
    DeclickScan,
    DeclipScan,
    ClearScan,
    DehumDetect,
    DenoiseLearn,
    DenoiseForget,
    PencilEdit,
    PencilReset,
    /// The Panner's live preview on or off.
    PannerLive(bool),
    /// A Panner gesture ended: redraw its green waveform.
    PannerSettled,
    /// The selection into the list as a new, unsaved item.
    TrimVirtual,
    AutoTrimStart,
    AutoTrimCancel,
    /// Loop points or the seam changed: tell playback.
    LoopChanged,
    /// A marker / loop gesture ended: one undo step for it.
    SettleAnnotations,
    LoopDetectStart,
    LoopDetectCancel,
    LoopDetectApply(usize),
    AddMarkerAtPlayhead,
    /// Move the playhead to a display sample.
    Seek(usize),
    /// Apply the active tool's built-in preset `n` (`tool_presets`).
    Preset(usize),
}

impl WavesPreviewer {
    pub(in crate::app) fn apply_inspector_actions(
        &mut self,
        tab_idx: usize,
        actions: Vec<InspectorAction>,
    ) {
        for action in actions {
            if tab_idx >= self.tabs.len() {
                return;
            }
            match action {
                InspectorAction::SelectTool(tool) => self.editor_select_tool(tab_idx, tool),
                InspectorAction::Preview => match self.tabs[tab_idx].active_tool {
                    ToolKind::MusicAnalyze => self.apply_music_preview_mix_for_tab(tab_idx),
                    ToolKind::PluginFx => self.spawn_plugin_preview_for_tab(tab_idx),
                    _ => self.tool_preview_request_now(tab_idx),
                },
                InspectorAction::SetManual(manual) => {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.tool_session.manual = manual;
                    }
                }
                InspectorAction::ToggleAb => self.toggle_tool_preview_ab(tab_idx),
                InspectorAction::Discard => {
                    let pencil = self
                        .tabs
                        .get(tab_idx)
                        .is_some_and(|tab| tab.pencil_draft.is_some());
                    if pencil {
                        self.editor_pencil_cancel_draft(tab_idx);
                    } else if !self.discard_tool_draft(tab_idx) {
                        self.clear_preview_if_any(tab_idx);
                    }
                }
                InspectorAction::Apply => {
                    self.apply_active_tool(tab_idx);
                }
                InspectorAction::ResetSettings => self.reset_active_tool_settings(tab_idx),
                InspectorAction::CancelApply => self.cancel_editor_apply_for_tab(tab_idx),
                InspectorAction::PlayTarget => {
                    let tool = self.tabs[tab_idx].active_tool;
                    if let RenderTarget::Range(..) = self.tool_target_for_tab(tab_idx, tool) {
                        self.editor_play_selection(tab_idx);
                    }
                }
                InspectorAction::Tool(command) => self.apply_tool_command(tab_idx, command),
            }
        }
    }

    fn apply_tool_command(&mut self, tab_idx: usize, command: ToolCommand) {
        match command {
            ToolCommand::Preset(index) => {
                let tool = self.tabs[tab_idx].active_tool;
                if let Some(preset) = super::tool_presets::presets(tool).get(index) {
                    let before = DraftSnapshot::capture(&self.tabs[tab_idx]);
                    (preset.apply)(&mut self.tabs[tab_idx].tool_state);
                    self.note_tool_draft_change(tab_idx, before);
                }
            }
            ToolCommand::DeclickScan => self.editor_declick_scan(tab_idx),
            ToolCommand::DeclipScan => self.editor_declip_scan(tab_idx),
            ToolCommand::ClearScan => {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.declick_scan = None;
                }
            }
            ToolCommand::DehumDetect => {
                let before = self.tabs.get(tab_idx).map(DraftSnapshot::capture);
                match self.editor_dehum_detect(tab_idx) {
                    Some(hz) => self.push_toast(
                        super::types::ToastSeverity::Info,
                        format!("Hum detected at {hz:.2} Hz"),
                    ),
                    None => self.push_toast(
                        super::types::ToastSeverity::Info,
                        "No hum line found in 45-65 Hz",
                    ),
                }
                if let Some(before) = before {
                    self.note_tool_draft_change(tab_idx, before);
                }
            }
            ToolCommand::DenoiseLearn => {
                self.editor_denoise_learn_profile(tab_idx);
                // A new profile is a new op: hear it if a preview is on.
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.tool_session.last_change_at = Some(Instant::now());
                }
            }
            ToolCommand::DenoiseForget => {
                self.clear_preview_if_any(tab_idx);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.noise_profile = None;
                }
            }
            ToolCommand::PencilEdit => {
                self.editor_pencil_start_editing(tab_idx);
            }
            ToolCommand::PencilReset => {
                self.editor_pencil_reset_draft(tab_idx);
            }
            ToolCommand::PannerLive(on) => {
                if on {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.preview_audio_tool = Some(ToolKind::Panner);
                    }
                    self.panner_refresh_overlay(tab_idx);
                } else {
                    self.clear_preview_if_any(tab_idx);
                }
            }
            ToolCommand::TrimVirtual => {
                let ranges = self.tabs[tab_idx].all_selected_ranges();
                let path = self.tabs[tab_idx].path.clone();
                let mut iter = ranges.into_iter();
                if let Some(first) = iter.next() {
                    self.begin_trim_virtual_job(tab_idx, first);
                }
                for (s, e) in iter {
                    self.virtual_trim_queue.push_back((path.clone(), s, e));
                }
            }
            ToolCommand::AutoTrimStart => self.start_auto_trim(tab_idx),
            ToolCommand::AutoTrimCancel => self.cancel_auto_trim(tab_idx),
            ToolCommand::LoopChanged => {
                if let Some(tab) = self.tabs.get(tab_idx) {
                    self.apply_loop_mode_for_tab(tab);
                }
            }
            ToolCommand::SettleAnnotations => self.editor_settle_annotation_gesture(tab_idx),
            ToolCommand::LoopDetectStart => self.start_loop_detect(tab_idx),
            ToolCommand::LoopDetectCancel => self.cancel_loop_detect(tab_idx),
            ToolCommand::LoopDetectApply(candidate) => {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    Self::annotation_begin(tab, "Loop Candidate");
                }
                self.apply_loop_detect_candidate(tab_idx, candidate);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    Self::editor_sync_annotation_layers(tab);
                }
                self.editor_settle_annotation_gesture(tab_idx);
            }
            ToolCommand::AddMarkerAtPlayhead => self.add_applied_marker_at_playhead(tab_idx),
            ToolCommand::Seek(sample) => self.editor_seek_display_sample(tab_idx, sample),
            ToolCommand::PannerSettled => {
                if self
                    .tabs
                    .get(tab_idx)
                    .is_some_and(|tab| tab.preview_audio_tool == Some(ToolKind::Panner))
                {
                    self.panner_refresh_overlay(tab_idx);
                }
            }
        }
    }

    /// Open the undo step of a marker / loop gesture, capturing the tab as
    /// it is before the gesture changes anything. Nested calls keep the first.
    pub(in crate::app) fn annotation_begin(tab: &mut super::types::EditorTab, label: &str) {
        if tab.annotation_undo.is_none() {
            tab.annotation_undo = Some(Box::new(Self::capture_undo_state_labeled(tab, label)));
        }
    }

    /// Markers and loop points are kept as they are edited: the layers that
    /// used to separate "editing" from "applied" follow every edit.
    pub(in crate::app) fn editor_sync_annotation_layers(tab: &mut super::types::EditorTab) {
        tab.markers_committed = tab.markers.clone();
        tab.markers_applied = tab.markers.clone();
        tab.loop_region_committed = tab.loop_region;
        tab.loop_region_applied = tab.loop_region;
        Self::update_markers_dirty(tab);
        Self::update_loop_markers_dirty(tab);
    }

    /// Close the marker / loop gesture: one undo step if it changed anything.
    pub(in crate::app) fn editor_settle_annotation_gesture(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let Some(state) = tab.annotation_undo.take() else {
            return;
        };
        Self::editor_sync_annotation_layers(tab);
        let changed = state.markers != tab.markers
            || state.regions != tab.regions
            || state.loop_region != tab.loop_region
            || state.loop_xfade_samples != tab.loop_xfade_samples
            || state.loop_xfade_shape != tab.loop_xfade_shape
            || state.loop_mode != tab.loop_mode;
        if changed {
            self.push_editor_undo_state(tab_idx, *state, true);
        }
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    /// Settle a marker / loop gesture whose end was not seen (the pointer
    /// was released outside the widget): nothing held, nothing focused.
    pub(in crate::app) fn editor_settle_annotation_if_idle(
        &mut self,
        tab_idx: usize,
        ctx: &egui::Context,
    ) {
        let open = self
            .tabs
            .get(tab_idx)
            .is_some_and(|tab| tab.annotation_undo.is_some());
        if !open {
            return;
        }
        let busy = ctx.input(|i| i.pointer.any_down()) || ctx.memory(|m| m.focused().is_some());
        if !busy {
            self.editor_settle_annotation_gesture(tab_idx);
        }
    }

    /// Where the playhead is, in the tab's display samples.
    pub(in crate::app) fn editor_playhead_display_sample(&self, tab_idx: usize) -> usize {
        let pos = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        self.tabs
            .get(tab_idx)
            .map(|tab| self.map_audio_to_display_sample(tab, pos))
            .unwrap_or(0)
    }

    /// Move the playhead to a display sample of the tab.
    pub(in crate::app) fn editor_seek_display_sample(&mut self, tab_idx: usize, display: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let audio = self.map_display_to_audio_sample(tab, display);
        self.audio.seek_to_sample(audio);
    }

    /// Music Analyze's markers into the file's, as one undo step.
    pub(in crate::app) fn apply_music_markers_with_undo(&mut self, tab_idx: usize) {
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            Self::annotation_begin(tab, "Music Markers");
        }
        self.apply_music_analysis_markers_to_tab(tab_idx);
        self.editor_settle_annotation_gesture(tab_idx);
        // They are the file's markers now; drawn again on top they would be
        // shown twice. A checkbox change shows a fresh set.
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.music_analysis_draft.provisional_markers.clear();
        }
    }

    /// A change the canvas made to the tool's settings this frame: part of a
    /// gesture while the pointer is held, a draft step when it lets go.
    pub(in crate::app) fn note_canvas_draft_change(
        &mut self,
        tab_idx: usize,
        before: &DraftSnapshot,
        pointer_down: bool,
    ) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let after = DraftSnapshot::capture(tab);
        if after == *before {
            return;
        }
        let now = Instant::now();
        let session = &mut tab.tool_session;
        if pointer_down {
            session.begin_gesture(before.clone());
            session.note_live_change(now);
        } else if session.gesture_open.is_some() {
            session.end_gesture(&after, now);
        } else {
            session.note_change(before.clone(), &after, now);
        }
    }

    /// Record a change made outside a panel gesture (a button that moved a
    /// setting) as one draft step.
    pub(in crate::app) fn note_tool_draft_change(&mut self, tab_idx: usize, before: DraftSnapshot) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let after = DraftSnapshot::capture(tab);
        tab.tool_session.note_change(before, &after, Instant::now());
    }

    /// Switch the tab's tool. Whatever the old tool was previewing goes
    /// away at the same place in the audio -- playback carries on -- and the
    /// new tool starts a fresh session: opening a tool changes nothing that
    /// is heard.
    pub(in crate::app) fn editor_select_tool(&mut self, tab_idx: usize, tool: ToolKind) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let old = tab.active_tool;
        if old == tool {
            return;
        }
        self.remember_tool_choice(tool);
        let mut cancel_music_preview = false;
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.active_tool_last = Some(old);
            match old {
                ToolKind::Pencil => {
                    // The Pencil lives entirely in its draft; leaving discards it.
                    tab.pencil_draft = None;
                    tab.pencil_last_point = None;
                    tab.pencil_stroke_channels.clear();
                }
                // De-click and De-clip share the scan storage; one tool's
                // spans must never be shown as the other's.
                ToolKind::DeClick | ToolKind::DeClip => tab.declick_scan = None,
                ToolKind::Trim => tab.trim_range = None,
                ToolKind::Fade => tab.fade_drag_edge = None,
                ToolKind::ChannelRouting => tab.channel_routing_draft.connecting_from = None,
                ToolKind::MusicAnalyze => {
                    tab.music_analysis_draft.provisional_markers.clear();
                    tab.music_analysis_draft.stems_audio = None;
                    tab.music_analysis_draft.preview_inflight = false;
                    tab.music_analysis_draft.preview_active = false;
                    cancel_music_preview = true;
                }
                _ => {}
            }
            tab.gain_env_drag = None;
            tab.pitch_env_drag = None;
            tab.pitch_drag_active = false;
            Self::update_markers_dirty(tab);
        }
        if cancel_music_preview {
            self.cancel_music_preview_run();
        }
        self.clear_preview_if_any(tab_idx);
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.active_tool = tool;
            tab.spectral_warp_edit = tool == ToolKind::SpectralWarp;
            tab.spectral_brush_edit = tool == ToolKind::SpectralBrush;
            tab.spectral_warp_drag = None;
            tab.spectral_brush_last = None;
            if tool == ToolKind::Fade
                && tab.tool_state.fade_in_ms <= 0.0
                && tab.tool_state.fade_out_ms <= 0.0
                && tab.samples_len > 0
            {
                // A first visit to Edge Fade starts with something to drag:
                // a tenth of the clip, at most a quarter second.
                let clip_ms =
                    tab.samples_len as f32 / tab.buffer_sample_rate.max(1) as f32 * 1000.0;
                let default_ms = (clip_ms * 0.1).min(250.0).max(1.0).min(clip_ms * 0.5);
                tab.tool_state.fade_in_ms = default_ms;
                tab.tool_state.fade_out_ms = default_ms;
            }
        }
        self.reset_tool_session(tab_idx);
    }

    /// The tool each palette category opens with: the last one used there.
    pub(in crate::app) fn remember_tool_choice(&mut self, tool: ToolKind) {
        self.inspector_last_tool.insert(tool.spec().category, tool);
    }

    /// Whether Apply would do anything for the tab's active tool now.
    pub(in crate::app) fn active_tool_can_apply(&self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        if tab.loading || tab.read_only || tab.paged_asset || tab.ch_samples.is_empty() {
            return false;
        }
        if self.editor_apply_busy_for_tab(tab_idx) {
            return false;
        }
        let tool = tab.active_tool;
        match tool.spec().preview {
            PreviewKind::Rendered => {
                Self::tool_op_for_tab(tab, tool).is_some_and(|op| !op.is_neutral())
            }
            PreviewKind::Own => match tool {
                ToolKind::MusicAnalyze => {
                    tab.music_analysis_draft.preview_active
                        && !tab.music_analysis_draft.preview_inflight
                }
                ToolKind::PluginFx => {
                    tab.plugin_fx_draft.plugin_key.is_some() && self.plugin_process_state.is_none()
                }
                _ => false,
            },
            PreviewKind::Live => {
                let view = self.panner_view(tab_idx);
                !view.layout.is_empty()
                    && (view.mono() || !tab.tool_state.pan_params(&view.layout).is_neutral())
            }
            PreviewKind::Draft => tab
                .pencil_draft
                .as_ref()
                .is_some_and(|draft| !draft.undo.is_empty()),
            PreviewKind::None => match tool {
                ToolKind::ChannelRouting => true,
                ToolKind::Trim => !tab.all_selected_ranges().is_empty(),
                _ => false,
            },
        }
    }

    /// The op the Loop tool's Apply runs: bake the seam crossfade, or write
    /// the loop out `loop_repeat` times.
    pub(in crate::app) fn loop_apply_op(
        tab: &super::types::EditorTab,
    ) -> Option<super::tool_render::ToolOp> {
        if tab.tool_state.loop_apply_unroll {
            Self::loop_unroll_op(tab, tab.tool_state.loop_repeat.max(2))
        } else {
            Self::loop_crossfade_op(tab)
        }
    }

    /// Apply the active tool's settings: one undo step, the preview's samples
    /// when they match, a fresh render on the same function otherwise.
    /// Returns whether an apply was made or started.
    pub(in crate::app) fn apply_active_tool(&mut self, tab_idx: usize) -> bool {
        if !self.active_tool_can_apply(tab_idx) {
            return false;
        }
        let tab = &self.tabs[tab_idx];
        let tool = tab.active_tool;
        match tool.spec().preview {
            PreviewKind::Own => {
                match tool {
                    ToolKind::MusicAnalyze => self.apply_music_preview_to_tab(tab_idx),
                    ToolKind::PluginFx => self.spawn_plugin_apply_for_tab(tab_idx),
                    _ => return false,
                }
                true
            }
            PreviewKind::Rendered => {
                let Some(op) = Self::tool_op_for_tab(tab, tool) else {
                    return false;
                };
                let long = tab.samples_len > LIVE_PREVIEW_SAMPLE_LIMIT;
                let heavy = tool.spec().heavy;
                let target = self.tool_target_for_tab(tab_idx, tool);
                let Some(req) = self.tool_request_for_tab(tab_idx, op, target) else {
                    return false;
                };
                // Warp points and brush stamps describe an edit to the audio
                // as it is now: once applied they are spent, and left on the
                // tab a second Apply would push or cut the same place again.
                // Taken before the commit, which records the settings as the
                // new starting point (Esc must not bring them back).
                let spent = self
                    .tabs
                    .get_mut(tab_idx)
                    .map(|tab| Self::spend_tool_strokes(tab, tool));
                let applied = if heavy || long {
                    self.spawn_editor_apply_op(tab_idx, req, None)
                } else {
                    self.editor_apply_op_now(tab_idx, req)
                };
                if !applied {
                    if let (Some(tab), Some(spent)) = (self.tabs.get_mut(tab_idx), spent) {
                        spent.restore(tab);
                    }
                }
                if applied && tool == ToolKind::LoopEdit {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        // The seam is in the audio now; fading it again on
                        // playback would fade it twice.
                        if !tab.tool_state.loop_apply_unroll {
                            tab.loop_xfade_samples = 0;
                        }
                    }
                    if let Some(tab) = self.tabs.get(tab_idx) {
                        self.apply_loop_mode_for_tab(tab);
                    }
                }
                if applied && matches!(tool, ToolKind::DeClick | ToolKind::DeClip) {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.declick_scan = None;
                    }
                }
                applied
            }
            PreviewKind::Live => {
                self.editor_apply_panner(tab_idx);
                true
            }
            PreviewKind::Draft => self.editor_pencil_commit(tab_idx),
            PreviewKind::None => match tool {
                ToolKind::ChannelRouting => {
                    self.editor_apply_channel_routing(tab_idx);
                    true
                }
                ToolKind::Trim => {
                    let ranges = tab.all_selected_ranges();
                    match (tab.tool_state.trim_mode, ranges.len()) {
                        (_, 0) => return false,
                        (0, 1) => self.editor_delete_range_and_join(tab_idx, ranges[0]),
                        (0, _) => self.editor_delete_multi_ranges_and_join(tab_idx, ranges),
                        (1, 1) => self.editor_apply_mute_range(tab_idx, ranges[0]),
                        (1, _) => self.editor_apply_mute_multi_ranges(tab_idx, ranges),
                        (_, 1) => self.editor_apply_trim_range(tab_idx, ranges[0]),
                        _ => self.editor_apply_trim_multi_ranges(tab_idx, ranges),
                    }
                    true
                }
                _ => false,
            },
        }
    }

    /// Take the strokes an apply of `tool` uses up off the tab.
    pub(in crate::app) fn spend_tool_strokes(tab: &mut EditorTab, tool: ToolKind) -> SpentStrokes {
        match tool {
            ToolKind::SpectralWarp => {
                tab.spectral_warp_drag = None;
                SpentStrokes::Warp(std::mem::take(&mut tab.spectral_warp_points))
            }
            ToolKind::SpectralBrush => {
                tab.spectral_brush_last = None;
                SpentStrokes::Brush(std::mem::take(&mut tab.spectral_brush_stamps))
            }
            _ => SpentStrokes::None,
        }
    }

    /// The active tool's settings back to their defaults, as one draft step.
    pub(in crate::app) fn reset_active_tool_settings(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let before = DraftSnapshot::capture(tab);
        let d = ToolState::default_values();
        let st = &mut tab.tool_state;
        match tab.active_tool {
            ToolKind::Gain => {
                st.gain_db = d.gain_db;
                tab.gain_env_points.clear();
                tab.gain_env_enabled = false;
            }
            ToolKind::Normalize => st.normalize_target_db = d.normalize_target_db,
            ToolKind::Loudness => st.loudness_target_lufs = d.loudness_target_lufs,
            ToolKind::InvertPolarity => st.invert_smooth_boundaries = d.invert_smooth_boundaries,
            ToolKind::Fade => {
                st.fade_in_ms = d.fade_in_ms;
                st.fade_out_ms = d.fade_out_ms;
            }
            ToolKind::InsertSilence => st.insert_silence_ms = d.insert_silence_ms,
            ToolKind::PitchShift => {
                st.pitch_semitones = d.pitch_semitones;
                tab.pitch_env_points.clear();
                tab.pitch_env_enabled = false;
            }
            ToolKind::TimeStretch => st.stretch_rate = d.stretch_rate,
            ToolKind::Speed => st.speed_rate = d.speed_rate,
            ToolKind::NoiseGate => st.gate = d.gate,
            // The tool's curves go with its settings.
            ToolKind::Eq => {
                st.eq = d.eq;
                tab.automation.retain(|l| l.param.tool() != ToolKind::Eq);
            }
            ToolKind::Compressor => {
                st.compressor = d.compressor;
                tab.automation.retain(|l| l.param.tool() != ToolKind::Compressor);
            }
            ToolKind::Limiter => {
                st.limiter = d.limiter;
                tab.automation.retain(|l| l.param.tool() != ToolKind::Limiter);
            }
            ToolKind::Clipper => {
                st.clipper = d.clipper;
                tab.automation.retain(|l| l.param.tool() != ToolKind::Clipper);
            }
            // The IR stays: Reset is about how it is used.
            ToolKind::Reverb => {
                st.reverb = crate::reverb::ReverbSettings {
                    ir: st.reverb.ir,
                    ..d.reverb
                };
                tab.automation.retain(|l| l.param.tool() != ToolKind::Reverb);
            }
            ToolKind::DeClick => st.declick_sensitivity = d.declick_sensitivity,
            ToolKind::DeClip => st.declip_sensitivity = d.declip_sensitivity,
            ToolKind::DeHum => {
                st.dehum_hz = d.dehum_hz;
                st.dehum_harmonics = d.dehum_harmonics;
                st.dehum_q = d.dehum_q;
                st.dehum_depth_db = d.dehum_depth_db;
            }
            ToolKind::DeNoise => {
                st.denoise_reduction_db = d.denoise_reduction_db;
                st.denoise_strength = d.denoise_strength;
            }
            ToolKind::Panner => *st = st.without_pan(),
            ToolKind::LoopEdit => {
                st.loop_repeat = d.loop_repeat;
                st.loop_apply_unroll = d.loop_apply_unroll;
                tab.loop_xfade_samples = 0;
            }
            ToolKind::SpectralWarp => {
                st.warp_time_radius_ms = d.warp_time_radius_ms;
                st.warp_freq_radius_hz = d.warp_freq_radius_hz;
                tab.spectral_warp_points.clear();
            }
            ToolKind::SpectralBrush => {
                st.brush_cut_db = d.brush_cut_db;
                st.brush_time_radius_ms = d.brush_time_radius_ms;
                st.brush_freq_radius_hz = d.brush_freq_radius_hz;
                tab.spectral_brush_stamps.clear();
            }
            ToolKind::SpectralMute | ToolKind::Heal => {
                st.spectral_time_fade_ms = d.spectral_time_fade_ms;
                st.spectral_freq_fade_hz = d.spectral_freq_fade_hz;
            }
            _ => {}
        }
        self.note_tool_draft_change(tab_idx, before);
        if self
            .tabs
            .get(tab_idx)
            .is_some_and(|tab| tab.active_tool == ToolKind::Panner)
        {
            self.apply_tool_command(tab_idx, ToolCommand::PannerSettled);
        }
    }
}
