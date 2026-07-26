use std::path::{Path, PathBuf};

use super::*;

impl super::WavesPreviewer {
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
        if self.is_virtual_path(path) {
            self.audio.stop();
            if let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == path) {
                self.workspace_view = crate::app::types::WorkspaceView::Editor;
                self.active_tab = Some(idx);
                self.debug_mark_tab_switch_start(path);
                self.queue_tab_activation(path.to_path_buf());
                return;
            }
            if self.tabs.len() >= crate::app::MAX_EDITOR_TABS {
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
                tab.snap_zero_cross = cached.snap_zero_cross;
                tab.active_tool = cached.active_tool;
                tab.tool_state = cached.tool_state;
                tab.loop_mode = cached.loop_mode;
                tab.plugin_fx_draft = cached.plugin_fx_draft;
                self.tabs.push(tab);
                self.workspace_view = crate::app::types::WorkspaceView::Editor;
                self.active_tab = Some(self.tabs.len() - 1);
                self.playing_path = Some(path.to_path_buf());
                self.audio.stop();
                self.audio.set_samples_channels(Vec::new());
                self.playback_mark_buffer_source(
                    super::PlaybackSourceKind::EditorTab(path.to_path_buf()),
                    cached_sr,
                );
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
            let Some(audio) = item.virtual_audio.clone() else {
                return;
            };
            let name = item.display_name.clone();
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
            tab.buffer_sample_rate = self.audio.shared.out_sample_rate.max(1);
            tab.samples_len_visual = visual_len;
            tab.bpm_value = default_bpm;
            tab.active_tool = initial_tool;
            tab.tool_state = crate::app::types::ToolState::default_values();
            self.tabs.push(tab);
            self.workspace_view = crate::app::types::WorkspaceView::Editor;
            self.active_tab = Some(self.tabs.len() - 1);
            self.playing_path = Some(path.to_path_buf());
            self.audio.stop();
            self.audio.set_samples_channels(Vec::new());
            self.playback_mark_buffer_source(
                super::PlaybackSourceKind::EditorTab(path.to_path_buf()),
                self.audio.shared.out_sample_rate.max(1),
            );
            self.apply_effective_volume();
            self.spawn_editor_decode_from_audio_buffer(path.to_path_buf(), audio, virtual_in_sr);
            return;
        }
        if !path.is_file() {
            self.remove_missing_path(path);
            return;
        }
        let decode_failed = self.is_decode_failed_path(path);
        // 郢ｧ・ｿ郢晄じ・帝ｫ｢荵晢ｿ･/郢ｧ・｢郢ｧ・ｯ郢昴・縺・ｹ晞摩蝟ｧ邵ｺ蜷ｶ・玖ｭ弱ｅ竊馴ｫｻ・ｳ陞｢・ｰ郢ｧ雋樞酪雎・ｽ｢
        if let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == path) {
            self.workspace_view = crate::app::types::WorkspaceView::Editor;
            self.active_tab = Some(idx);
            self.debug_mark_tab_switch_start(path);
            self.queue_tab_activation(path.to_path_buf());
            return;
        }
        if self.tabs.len() >= crate::app::MAX_EDITOR_TABS {
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
            tab.snap_zero_cross = cached.snap_zero_cross;
            tab.active_tool = cached.active_tool;
            tab.tool_state = cached.tool_state;
            tab.loop_mode = cached.loop_mode;
            tab.plugin_fx_draft = cached.plugin_fx_draft;
            self.tabs.push(tab);
            self.workspace_view = crate::app::types::WorkspaceView::Editor;
            self.active_tab = Some(self.tabs.len() - 1);
            self.playing_path = Some(path.to_path_buf());
            self.audio.stop();
            self.audio.set_samples_channels(Vec::new());
            self.playback_mark_buffer_source(
                super::PlaybackSourceKind::EditorTab(path.to_path_buf()),
                cached_sr,
            );
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
        let loading = !decode_failed;
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
        tab.loading = loading;
        tab.buffer_sample_rate = self.audio.shared.out_sample_rate.max(1);
        tab.samples_len_visual = estimated_visual_frames.unwrap_or(0);
        tab.loading_waveform_minmax = initial_loading_overview;
        tab.bpm_value = default_bpm;
        tab.active_tool = initial_tool;
        tab.tool_state = crate::app::types::ToolState::default_values();
        self.tabs.push(tab);
        self.workspace_view = crate::app::types::WorkspaceView::Editor;
        self.active_tab = Some(self.tabs.len() - 1);
        self.playing_path = Some(path.to_path_buf());
        self.audio.set_samples_channels(Vec::new());
        self.playback_mark_buffer_source(
            super::PlaybackSourceKind::EditorTab(path.to_path_buf()),
            self.audio.shared.out_sample_rate.max(1),
        );
        self.apply_effective_volume();
        self.queue_tab_activation_with_kind(
            path.to_path_buf(),
            super::PendingTabActivationKind::InitialOpen,
        );
        if !decode_failed {
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
}
