use std::path::{Path, PathBuf};

use super::*;

impl super::WavesPreviewer {
    pub(super) fn editor_playback_handoff_matches(&self, path: &Path) -> bool {
        self.pending_editor_playback_handoff
            .as_ref()
            .is_some_and(|handoff| handoff.path.as_path() == path)
    }

    fn begin_editor_playback_handoff(&mut self, path: &Path) -> bool {
        let list_source_matches = matches!(
            &self.playback_session.source,
            super::PlaybackSourceKind::ListPreview(source) if source.as_path() == path
        );
        let pending_matches = self
            .list_seek_pending
            .as_ref()
            .is_some_and(|pending| pending.path.as_path() == path);
        let gesture_matches = self
            .list_seek_gesture
            .as_ref()
            .is_some_and(|gesture| gesture.path.as_path() == path);
        if !list_source_matches && !pending_matches && !gesture_matches {
            self.pending_editor_playback_handoff = None;
            return false;
        }
        let desired_playing = self.playback_is_playing_now()
            || self
                .list_seek_pending
                .as_ref()
                .filter(|pending| pending.path.as_path() == path)
                .is_some_and(|pending| pending.resume_playing)
            || self
                .list_seek_gesture
                .as_ref()
                .filter(|gesture| gesture.path.as_path() == path)
                .is_some_and(|gesture| gesture.resume_playing);
        self.pending_editor_playback_handoff = Some(super::PendingEditorPlaybackHandoff {
            path: path.to_path_buf(),
            desired_playing,
        });
        true
    }

    pub(super) fn finish_editor_playback_handoff(&mut self, path: &Path, source_replaced: bool) {
        let Some(handoff) = self
            .pending_editor_playback_handoff
            .take()
            .filter(|handoff| handoff.path.as_path() == path)
        else {
            return;
        };
        self.cancel_list_preview_job();
        self.list_seek_gesture = None;
        if handoff.desired_playing {
            if source_replaced || !self.playback_is_playing_now() {
                self.audio
                    .play_declicked(crate::app::list_seek_ops::LIST_TRANSPORT_FADE_IN_MS);
            }
        } else {
            self.audio.stop();
        }
    }

    pub(super) fn cancel_editor_playback_handoff_for_path(&mut self, path: &Path) {
        if self.editor_playback_handoff_matches(path) {
            self.pending_editor_playback_handoff = None;
        }
    }

    pub(super) fn maintain_editor_playback_handoff(&mut self) {
        let Some(path) = self
            .pending_editor_playback_handoff
            .as_ref()
            .map(|handoff| handoff.path.clone())
        else {
            return;
        };
        let active_matches = self.is_editor_workspace_active()
            && self
                .active_tab
                .and_then(|idx| self.tabs.get(idx))
                .is_some_and(|tab| tab.path == path);
        let activation_matches = self.pending_activate_path.as_ref() == Some(&path);
        if !active_matches && !activation_matches {
            self.pending_editor_playback_handoff = None;
            return;
        }
        let naturally_finished = self
            .pending_editor_playback_handoff
            .as_ref()
            .is_some_and(|handoff| handoff.desired_playing)
            && !self.playback_is_playing_now()
            && self.list_seek_pending.is_none()
            && self.audio.current_source_len() > 0
            && self
                .audio
                .shared
                .play_pos
                .load(std::sync::atomic::Ordering::Relaxed)
                >= self.audio.current_source_len();
        if naturally_finished {
            if let Some(handoff) = &mut self.pending_editor_playback_handoff {
                handoff.desired_playing = false;
            }
        }
    }

    fn reset_editor_transport_unless_handoff(&mut self, path: &Path, sample_rate: u32) {
        if self.editor_playback_handoff_matches(path) {
            return;
        }
        self.audio.stop();
        self.audio.set_samples_channels(Vec::new());
        self.playback_mark_buffer_source(
            super::PlaybackSourceKind::EditorTab(path.to_path_buf()),
            sample_rate.max(1),
        );
    }

    fn seed_editor_notes_for_tab(&self, tab: &mut EditorTab) {
        if let Some(item) = self.item_for_path(&tab.path) {
            tab.editor_notes = item.editor_notes.clone();
        }
    }

    fn tool_for_new_editor_tab(&self) -> crate::app::types::ToolKind {
        self.tabs
            .last()
            .map(|tab| tab.active_tool)
            .unwrap_or(crate::app::types::ToolKind::LoopEdit)
    }

    pub(super) fn open_or_activate_tab(&mut self, path: &Path) {
        if let Some(item) = self.item_for_path(path) {
            if item.source == crate::app::types::MediaSource::External {
                return;
            }
        }
        let preserve_list_transport = self.begin_editor_playback_handoff(path);
        if self.is_virtual_path(path) {
            if !preserve_list_transport {
                self.audio.stop();
            }
            if let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == path) {
                self.workspace_view = crate::app::types::WorkspaceView::Editor;
                self.active_tab = Some(idx);
                self.debug_mark_tab_switch_start(path);
                self.queue_tab_activation(path.to_path_buf());
                return;
            }
            if self.tabs.len() >= crate::app::MAX_EDITOR_TABS {
                self.cancel_editor_playback_handoff_for_path(path);
                self.debug_log(format!(
                    "tab limit reached ({}); skipping {}",
                    crate::app::MAX_EDITOR_TABS,
                    path.display()
                ));
                self.push_toast(
                    crate::app::types::ToastSeverity::Warning,
                    format!(
                        "Tab limit ({}) reached — not opening more editors",
                        crate::app::MAX_EDITOR_TABS
                    ),
                );
                return;
            }
            if let Some(cached) = self.edited_cache.remove(path) {
                let name = self
                    .item_for_path(path)
                    .map(|item| item.display_name.clone())
                    .unwrap_or_else(|| "(virtual)".to_string());
                let cached_sr = cached.buffer_sample_rate.max(1);
                let cached_samples_len = cached.samples_len;
                let cached_channels = cached.ch_samples;
                let cached_loading_overview = cached.waveform_minmax;
                let mut tab = EditorTab::new_base(path.to_path_buf(), name);
                self.seed_editor_notes_for_tab(&mut tab);
                tab.buffer_sample_rate = cached_sr;
                tab.samples_len_visual = cached_samples_len;
                tab.loading_waveform_minmax = cached_loading_overview;
                tab.dirty = cached.dirty;
                tab.markers = cached.markers;
                tab.regions = cached.regions;
                tab.markers_committed = cached.markers_committed;
                tab.markers_saved = cached.markers_saved;
                tab.markers_applied = cached.markers_applied;
                tab.markers_dirty = cached.markers_dirty;
                tab.loop_region = cached.loop_region;
                tab.loop_region_committed = cached.loop_region_committed;
                tab.loop_region_applied = cached.loop_region_applied;
                tab.loop_markers_saved = cached.loop_markers_saved;
                tab.loop_markers_dirty = cached.loop_markers_dirty;
                tab.trim_range = cached.trim_range;
                tab.loop_xfade_samples = cached.loop_xfade_samples;
                tab.loop_xfade_shape = cached.loop_xfade_shape;
                tab.fade_in_range = cached.fade_in_range;
                tab.fade_out_range = cached.fade_out_range;
                tab.fade_in_shape = cached.fade_in_shape;
                tab.fade_out_shape = cached.fade_out_shape;
                tab.show_waveform_overlay = cached.show_waveform_overlay;
                tab.bpm_enabled = cached.bpm_enabled;
                tab.bpm_value = cached.bpm_value;
                tab.bpm_user_set = cached.bpm_user_set;
                tab.bpm_offset_sec = cached.bpm_offset_sec;
                tab.time_sig_numerator = cached.time_sig_numerator;
                tab.time_sig_denominator = cached.time_sig_denominator;
                tab.active_tool = cached.active_tool;
                tab.tool_state = cached.tool_state;
                tab.loop_mode = cached.loop_mode;
                tab.plugin_fx_draft = cached.plugin_fx_draft;
                tab.plugin_fx_chain = cached.plugin_fx_chain;
                self.tabs.push(tab);
                self.workspace_view = crate::app::types::WorkspaceView::Editor;
                self.active_tab = Some(self.tabs.len() - 1);
                self.playing_path = Some(path.to_path_buf());
                self.reset_editor_transport_unless_handoff(path, cached_sr);
                self.apply_effective_volume();
                self.spawn_editor_decode_from_ready_channels(
                    path.to_path_buf(),
                    cached_channels,
                    cached_sr,
                );
                return;
            }
            let Some(item) = self.item_for_path(path) else {
                return;
            };
            let name = item.display_name.clone();
            let asset = item.audio_asset.clone();
            if item.virtual_audio.is_none() {
                let Some(source_path) = asset.backing.file_path().map(Path::to_path_buf) else {
                    return;
                };
                let out_sr = self.audio.shared.out_sample_rate.max(1);
                let visual_len = asset
                    .frame_count
                    .map(|frames| {
                        Self::convert_source_frames_to_output_frames(
                            usize::try_from(frames).unwrap_or(usize::MAX),
                            asset.sample_rate.max(1),
                            out_sr,
                        )
                    })
                    .unwrap_or(0);
                let initial_tool = self.tool_for_new_editor_tab();
                let mut tab = EditorTab::new_base(path.to_path_buf(), name);
                self.seed_editor_notes_for_tab(&mut tab);
                tab.loading = true;
                tab.buffer_sample_rate = out_sr;
                tab.samples_len_visual = visual_len;
                tab.loading_waveform_minmax = self.initial_editor_loading_overview(&source_path);
                tab.active_tool = initial_tool;
                tab.tool_state = crate::app::types::ToolState::default_values();
                self.tabs.push(tab);
                self.workspace_view = crate::app::types::WorkspaceView::Editor;
                self.active_tab = Some(self.tabs.len() - 1);
                self.playing_path = Some(path.to_path_buf());
                self.reset_editor_transport_unless_handoff(path, out_sr);
                self.apply_effective_volume();
                self.spawn_editor_decode(path.to_path_buf());
                return;
            }
            let audio = item.virtual_audio.clone().expect("checked resident asset");
            let virtual_in_sr = item
                .virtual_state
                .as_ref()
                .map(|v| v.sample_rate)
                .or_else(|| item.meta.as_ref().map(|m| m.sample_rate))
                .filter(|v| *v > 0)
                .unwrap_or(self.audio.shared.out_sample_rate.max(1));
            let default_bpm = self
                .meta_for_path(path)
                .and_then(|m| m.bpm)
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(0.0);
            let visual_len = audio.len();
            let initial_tool = self.tool_for_new_editor_tab();
            let mut tab = EditorTab::new_base(path.to_path_buf(), name);
            self.seed_editor_notes_for_tab(&mut tab);
            tab.buffer_sample_rate = self.audio.shared.out_sample_rate.max(1);
            tab.samples_len_visual = visual_len;
            tab.bpm_value = default_bpm;
            tab.active_tool = initial_tool;
            tab.tool_state = crate::app::types::ToolState::default_values();
            self.tabs.push(tab);
            self.workspace_view = crate::app::types::WorkspaceView::Editor;
            self.active_tab = Some(self.tabs.len() - 1);
            self.playing_path = Some(path.to_path_buf());
            self.reset_editor_transport_unless_handoff(
                path,
                self.audio.shared.out_sample_rate.max(1),
            );
            self.apply_effective_volume();
            self.spawn_editor_decode_from_audio_buffer(path.to_path_buf(), audio, virtual_in_sr);
            return;
        }
        if !path.is_file() {
            self.cancel_editor_playback_handoff_for_path(path);
            self.remove_missing_path(path);
            return;
        }
        // An explicit shell/CLI open can enter the editor before the list has
        // ever rendered a row for this path. Video duration and the
        // no-audio/AAC classification come from header metadata, so request it
        // here as well; otherwise the tab can remain a zero-length shell and
        // never create a seekable silent timeline.
        if crate::media_kind::is_video_path(path) && self.meta_for_path(path).is_none() {
            self.queue_header_meta_for_path(&path.to_path_buf(), true);
        }
        let decode_failed = self.is_decode_failed_path(path);
        let audio_track_absent = self
            .meta_for_path(path)
            .is_some_and(|meta| meta.audio_track_absent);
        let audio_track_unsupported = self
            .meta_for_path(path)
            .is_some_and(|meta| meta.audio_track_unsupported);
        let silent_video_timeline = audio_track_absent || audio_track_unsupported;
        // 郢ｧ・ｿ郢晄じ・帝ｫ｢荵晢ｿ･/郢ｧ・｢郢ｧ・ｯ郢昴・縺・ｹ晞摩蝟ｧ邵ｺ蜷ｶ・玖ｭ弱ｅ竊馴ｫｻ・ｳ陞｢・ｰ郢ｧ雋樞酪雎・ｽ｢
        if let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == path) {
            self.workspace_view = crate::app::types::WorkspaceView::Editor;
            self.active_tab = Some(idx);
            self.debug_mark_tab_switch_start(path);
            self.queue_tab_activation(path.to_path_buf());
            return;
        }
        if self.tabs.len() >= crate::app::MAX_EDITOR_TABS {
            self.cancel_editor_playback_handoff_for_path(path);
            self.debug_log(format!(
                "tab limit reached ({}); skipping {}",
                crate::app::MAX_EDITOR_TABS,
                path.display()
            ));
            self.push_toast(
                crate::app::types::ToastSeverity::Warning,
                format!(
                    "Tab limit ({}) reached — not opening more editors",
                    crate::app::MAX_EDITOR_TABS
                ),
            );
            return;
        }
        if let Some(cached) = self.edited_cache.remove(path) {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("(invalid)")
                .to_string();
            let cached_sr = cached.buffer_sample_rate.max(1);
            let cached_samples_len = cached.samples_len;
            let cached_channels = cached.ch_samples;
            let cached_loading_overview = cached.waveform_minmax;
            let mut tab = EditorTab::new_base(path.to_path_buf(), name);
            self.seed_editor_notes_for_tab(&mut tab);
            tab.buffer_sample_rate = cached_sr;
            tab.samples_len_visual = cached_samples_len;
            tab.loading_waveform_minmax = cached_loading_overview;
            tab.dirty = cached.dirty;
            tab.markers = cached.markers;
            tab.regions = cached.regions;
            tab.markers_committed = cached.markers_committed;
            tab.markers_saved = cached.markers_saved;
            tab.markers_applied = cached.markers_applied;
            tab.markers_dirty = cached.markers_dirty;
            tab.loop_region = cached.loop_region;
            tab.loop_region_committed = cached.loop_region_committed;
            tab.loop_region_applied = cached.loop_region_applied;
            tab.loop_markers_saved = cached.loop_markers_saved;
            tab.loop_markers_dirty = cached.loop_markers_dirty;
            tab.trim_range = cached.trim_range;
            tab.loop_xfade_samples = cached.loop_xfade_samples;
            tab.loop_xfade_shape = cached.loop_xfade_shape;
            tab.fade_in_range = cached.fade_in_range;
            tab.fade_out_range = cached.fade_out_range;
            tab.fade_in_shape = cached.fade_in_shape;
            tab.fade_out_shape = cached.fade_out_shape;
            tab.show_waveform_overlay = cached.show_waveform_overlay;
            tab.bpm_enabled = cached.bpm_enabled;
            tab.bpm_value = cached.bpm_value;
            tab.bpm_user_set = cached.bpm_user_set;
            tab.bpm_offset_sec = cached.bpm_offset_sec;
            tab.time_sig_numerator = cached.time_sig_numerator;
            tab.time_sig_denominator = cached.time_sig_denominator;
            tab.active_tool = cached.active_tool;
            tab.tool_state = cached.tool_state;
            tab.loop_mode = cached.loop_mode;
            tab.plugin_fx_draft = cached.plugin_fx_draft;
            tab.plugin_fx_chain = cached.plugin_fx_chain;
            self.tabs.push(tab);
            self.workspace_view = crate::app::types::WorkspaceView::Editor;
            self.active_tab = Some(self.tabs.len() - 1);
            self.playing_path = Some(path.to_path_buf());
            self.reset_editor_transport_unless_handoff(path, cached_sr);
            self.apply_effective_volume();
            self.spawn_editor_decode_from_ready_channels(
                path.to_path_buf(),
                cached_channels,
                cached_sr,
            );
            return;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string();
        let loading = !decode_failed && !silent_video_timeline;
        self.debug_mark_editor_open_start(path);
        let estimated_visual_frames = self
            .estimate_editor_total_frames_cached(path, self.audio.shared.out_sample_rate.max(1));
        let default_bpm = self
            .meta_for_path(path)
            .and_then(|m| m.bpm)
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(0.0);
        let initial_loading_overview = if loading {
            self.initial_editor_loading_overview(path)
        } else {
            Vec::new()
        };
        let initial_tool = self.tool_for_new_editor_tab();
        let mut tab = EditorTab::new_base(path.to_path_buf(), name);
        self.seed_editor_notes_for_tab(&mut tab);
        tab.loading = loading;
        tab.audio_track_absent = audio_track_absent;
        tab.audio_track_unsupported = audio_track_unsupported;
        tab.buffer_sample_rate = self.audio.shared.out_sample_rate.max(1);
        tab.samples_len_visual = estimated_visual_frames.unwrap_or(0);
        if silent_video_timeline {
            tab.samples_len = tab.samples_len_visual;
        }
        tab.loading_waveform_minmax = initial_loading_overview;
        tab.bpm_value = default_bpm;
        tab.active_tool = initial_tool;
        tab.tool_state = crate::app::types::ToolState::default_values();
        self.tabs.push(tab);
        self.workspace_view = crate::app::types::WorkspaceView::Editor;
        self.active_tab = Some(self.tabs.len() - 1);
        self.playing_path = Some(path.to_path_buf());
        self.reset_editor_transport_unless_handoff(path, self.audio.shared.out_sample_rate.max(1));
        self.apply_effective_volume();
        self.queue_tab_activation_with_kind(
            path.to_path_buf(),
            super::PendingTabActivationKind::InitialOpen,
        );
        if !decode_failed && !silent_video_timeline {
            self.spawn_editor_decode(path.to_path_buf());
        }
    }

    pub(super) fn open_paths_in_tabs(&mut self, paths: &[PathBuf]) {
        for path in paths {
            // Select-all + Enter on a huge list: once the tab limit is
            // reached, skip paths without an existing tab up front instead of
            // funneling every remaining path through open_or_activate_tab
            // (which logs a skip line per path).
            if self.tabs.len() >= crate::app::MAX_EDITOR_TABS
                && !self.tabs.iter().any(|t| t.path.as_path() == path.as_path())
            {
                continue;
            }
            if let Some(item) = self.item_for_path(path) {
                if item.source == crate::app::types::MediaSource::External {
                    continue;
                }
            }
            self.open_or_activate_tab(path);
        }
    }

    /// Bring an editor tab to the front.
    ///
    /// The one sequence a tab switch has to run: drop the outgoing tab's
    /// preview, point the workspace at the new tab, and queue its activation.
    /// Returns `false` for an index out of range, or for a tab that is already
    /// in front -- re-activating re-targets playback and decoding for nothing.
    pub(super) fn activate_editor_tab(&mut self, tab_idx: usize) -> bool {
        let already_active = self.workspace_view == super::types::WorkspaceView::Editor
            && self.active_tab == Some(tab_idx);
        if already_active {
            return false;
        }
        let Some(tab_path) = self.tabs.get(tab_idx).map(|tab| tab.path.clone()) else {
            return false;
        };
        if let Some(prev) = self.active_tab {
            if prev != tab_idx {
                self.clear_preview_if_any(prev);
            }
        }
        self.workspace_view = super::types::WorkspaceView::Editor;
        self.active_tab = Some(tab_idx);
        self.debug_mark_tab_switch_start(&tab_path);
        self.queue_tab_activation(tab_path);
        true
    }

    /// The tab `steps` away from the one in front, wrapping at both ends.
    pub(super) fn editor_tab_index_offset_by(&self, steps: isize) -> Option<usize> {
        let len = self.tabs.len();
        if len == 0 {
            return None;
        }
        let current = self.active_tab? as isize;
        let len_i = len as isize;
        Some(((current + steps).rem_euclid(len_i)) as usize)
    }
}
