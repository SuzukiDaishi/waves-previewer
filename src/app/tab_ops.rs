use std::path::{Path, PathBuf};

use super::*;

impl super::WavesPreviewer {
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
            return;
        }
        if let Some(cached) = self.edited_cache.remove(path) {
            let name = self
                .item_for_path(path)
                .map(|item| item.display_name.clone())
                .unwrap_or_else(|| "(virtual)".to_string());
            self.tabs.push(EditorTab {
                path: path.to_path_buf(),
                display_name: name,
                waveform_minmax: cached.waveform_minmax,
                waveform_pyramid: cached.waveform_pyramid,
                loop_enabled: false,
                loading: false,
                ch_samples: cached.ch_samples,
                buffer_sample_rate: cached.buffer_sample_rate.max(1),
                samples_len: cached.samples_len,
                samples_len_visual: cached.samples_len,
                loading_waveform_minmax: Vec::new(),
                view_offset: 0,
                view_offset_exact: 0.0,
                samples_per_px: 0.0,
                vertical_zoom: 1.0,
                vertical_view_center: 0.0,
                last_wave_w: 0.0,
                last_amplitude_nav_rect: None,
                last_amplitude_viewport_rect: None,
                last_amplitude_nav_click_at: 0.0,
                last_amplitude_nav_click_pos: None,
                viewport_source_generation: 1,
                viewport_render_requested_generation: 0,
                viewport_render_requested_key: None,
                viewport_render_pending_fine_at: None,
                viewport_render_inflight_coarse_generation: None,
                viewport_render_inflight_fine_generation: None,
                viewport_render_coarse: None,
                viewport_render_fine: None,
                viewport_render_last: None,
                dirty: cached.dirty,
                ops: Vec::new(),
                selection: None,
                markers: cached.markers,
                markers_committed: cached.markers_committed,
                markers_saved: cached.markers_saved,
                markers_applied: cached.markers_applied,
                markers_dirty: cached.markers_dirty,
                ab_loop: None,
                loop_region: cached.loop_region,
                loop_region_committed: cached.loop_region_committed,
                loop_region_applied: cached.loop_region_applied,
                loop_markers_saved: cached.loop_markers_saved,
                loop_markers_dirty: cached.loop_markers_dirty,
                trim_range: cached.trim_range,
                loop_xfade_samples: cached.loop_xfade_samples,
                loop_xfade_shape: cached.loop_xfade_shape,
                fade_in_range: cached.fade_in_range,
                fade_out_range: cached.fade_out_range,
                fade_in_shape: cached.fade_in_shape,
                fade_out_shape: cached.fade_out_shape,
                primary_view: crate::app::types::EditorPrimaryView::Wave,
                spec_sub_view: crate::app::types::EditorSpecSubView::Spec,
                other_sub_view: crate::app::types::EditorOtherSubView::Tempogram,
                show_waveform_overlay: cached.show_waveform_overlay,
                channel_view: ChannelView::mixdown(),
                bpm_enabled: cached.bpm_enabled,
                bpm_value: cached.bpm_value,
                bpm_user_set: cached.bpm_user_set,
                bpm_offset_sec: cached.bpm_offset_sec,
                seek_hold: None,
                snap_zero_cross: cached.snap_zero_cross,
                selection_anchor_sample: None,
                right_drag_mode: None,
                active_tool: cached.active_tool,
                tool_state: cached.tool_state,
                loop_mode: cached.loop_mode,
                dragging_marker: None,
                preview_audio_tool: None,
                active_tool_last: None,
                preview_offset_samples: None,
                preview_overlay: None,
                music_analysis_draft: crate::app::types::MusicAnalysisDraft::default(),
                plugin_fx_draft: cached.plugin_fx_draft,
                pending_loop_unwrap: None,
                undo_stack: Vec::new(),
                undo_bytes: 0,
                redo_stack: Vec::new(),
                redo_bytes: 0,
            });
            self.workspace_view = crate::app::types::WorkspaceView::Editor;
            self.active_tab = Some(self.tabs.len() - 1);
            self.playing_path = Some(path.to_path_buf());
            self.apply_dirty_tab_audio_with_mode(path);
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
        let mut chs = audio.channels.clone();
        self.apply_sample_rate_preview_for_path(path, &mut chs, virtual_in_sr);
        let samples_len = chs.get(0).map(|c| c.len()).unwrap_or(0);
        let default_bpm = self
            .meta_for_path(path)
            .and_then(|m| m.bpm)
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(0.0);
        let wf = if !self.mode_requires_offline_processing() {
            crate::wave::build_waveform_minmax_from_channels(&chs, samples_len, 2048)
        } else {
            Vec::new()
        };
        let waveform_pyramid = if !self.mode_requires_offline_processing() {
            Self::build_editor_waveform_cache(&chs, samples_len).1
        } else {
            None
        };
        self.tabs.push(EditorTab {
            path: path.to_path_buf(),
            display_name: name,
            waveform_minmax: wf,
            waveform_pyramid,
            loop_enabled: false,
            loading: false,
            ch_samples: chs.clone(),
            buffer_sample_rate: self.audio.shared.out_sample_rate.max(1),
            samples_len,
            samples_len_visual: samples_len,
            loading_waveform_minmax: Vec::new(),
            view_offset: 0,
            view_offset_exact: 0.0,
            samples_per_px: 0.0,
            vertical_zoom: 1.0,
            vertical_view_center: 0.0,
            last_wave_w: 0.0,
            last_amplitude_nav_rect: None,
            last_amplitude_viewport_rect: None,
            last_amplitude_nav_click_at: 0.0,
            last_amplitude_nav_click_pos: None,
            viewport_source_generation: 1,
            viewport_render_requested_generation: 0,
            viewport_render_requested_key: None,
            viewport_render_pending_fine_at: None,
            viewport_render_inflight_coarse_generation: None,
            viewport_render_inflight_fine_generation: None,
            viewport_render_coarse: None,
            viewport_render_fine: None,
            viewport_render_last: None,
            dirty: false,
            ops: Vec::new(),
            selection: None,
            markers: Vec::new(),
            markers_committed: Vec::new(),
            markers_saved: Vec::new(),
            markers_applied: Vec::new(),
            markers_dirty: false,
            ab_loop: None,
            loop_region: None,
            loop_region_committed: None,
            loop_region_applied: None,
            loop_markers_saved: None,
            loop_markers_dirty: false,
            trim_range: None,
            loop_xfade_samples: 0,
            loop_xfade_shape: crate::app::types::LoopXfadeShape::EqualPower,
            fade_in_range: None,
            fade_out_range: None,
            fade_in_shape: crate::app::types::FadeShape::SCurve,
            fade_out_shape: crate::app::types::FadeShape::SCurve,
            primary_view: crate::app::types::EditorPrimaryView::Wave,
            spec_sub_view: crate::app::types::EditorSpecSubView::Spec,
            other_sub_view: crate::app::types::EditorOtherSubView::Tempogram,
            show_waveform_overlay: false,
            channel_view: ChannelView::mixdown(),
            bpm_enabled: false,
            bpm_value: default_bpm,
            bpm_user_set: false,
            bpm_offset_sec: 0.0,
            seek_hold: None,
            snap_zero_cross: true,
            selection_anchor_sample: None,
            right_drag_mode: None,
            active_tool: crate::app::types::ToolKind::LoopEdit,
            tool_state: crate::app::types::ToolState {
                fade_in_ms: 0.0,
                fade_out_ms: 0.0,
                gain_db: 0.0,
                normalize_target_db: -6.0,
                loudness_target_lufs: -14.0,
                pitch_semitones: 0.0,
                stretch_rate: 1.0,
                loop_repeat: 2,
            },
            loop_mode: crate::app::types::LoopMode::Off,
            dragging_marker: None,
            preview_audio_tool: None,
            active_tool_last: None,
            preview_offset_samples: None,
            preview_overlay: None,
            music_analysis_draft: crate::app::types::MusicAnalysisDraft::default(),
            plugin_fx_draft: crate::app::types::PluginFxDraft::default(),
            pending_loop_unwrap: None,
            undo_stack: Vec::new(),
            undo_bytes: 0,
            redo_stack: Vec::new(),
            redo_bytes: 0,
        });
        self.workspace_view = crate::app::types::WorkspaceView::Editor;
        self.active_tab = Some(self.tabs.len() - 1);
        self.playing_path = Some(path.to_path_buf());
        if self.mode_requires_offline_processing() {
            self.audio.stop();
            self.audio.set_samples_mono(Vec::new());
            self.spawn_heavy_processing_from_channels(
                path.to_path_buf(),
                chs,
                ProcessingTarget::EditorTab(path.to_path_buf()),
            );
        } else {
            self.audio.set_samples_channels(chs);
            self.playback_mark_buffer_source(
                super::PlaybackSourceKind::EditorTab(path.to_path_buf()),
                self.audio.shared.out_sample_rate.max(1),
            );
        }
        self.apply_effective_volume();
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
        return;
    }
    if let Some(cached) = self.edited_cache.remove(path) {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string();
        self.tabs.push(EditorTab {
            path: path.to_path_buf(),
            display_name: name,
            waveform_minmax: cached.waveform_minmax,
            waveform_pyramid: cached.waveform_pyramid,
            loop_enabled: false,
            loading: false,
            ch_samples: cached.ch_samples,
            buffer_sample_rate: cached.buffer_sample_rate.max(1),
            samples_len: cached.samples_len,
            samples_len_visual: cached.samples_len,
            loading_waveform_minmax: Vec::new(),
            view_offset: 0,
            view_offset_exact: 0.0,
            samples_per_px: 0.0,
            vertical_zoom: 1.0,
            vertical_view_center: 0.0,
            last_wave_w: 0.0,
            last_amplitude_nav_rect: None,
            last_amplitude_viewport_rect: None,
            last_amplitude_nav_click_at: 0.0,
            last_amplitude_nav_click_pos: None,
            viewport_source_generation: 1,
            viewport_render_requested_generation: 0,
            viewport_render_requested_key: None,
            viewport_render_pending_fine_at: None,
            viewport_render_inflight_coarse_generation: None,
            viewport_render_inflight_fine_generation: None,
            viewport_render_coarse: None,
            viewport_render_fine: None,
            viewport_render_last: None,
            dirty: cached.dirty,
            ops: Vec::new(),
            selection: None,
            markers: cached.markers,
            markers_committed: cached.markers_committed,
            markers_saved: cached.markers_saved,
            markers_applied: cached.markers_applied,
            markers_dirty: cached.markers_dirty,
            ab_loop: None,
            loop_region: cached.loop_region,
            loop_region_committed: cached.loop_region_committed,
            loop_region_applied: cached.loop_region_applied,
            loop_markers_saved: cached.loop_markers_saved,
            loop_markers_dirty: cached.loop_markers_dirty,
            trim_range: cached.trim_range,
            loop_xfade_samples: cached.loop_xfade_samples,
            loop_xfade_shape: cached.loop_xfade_shape,
            fade_in_range: cached.fade_in_range,
            fade_out_range: cached.fade_out_range,
            fade_in_shape: cached.fade_in_shape,
            fade_out_shape: cached.fade_out_shape,
            primary_view: crate::app::types::EditorPrimaryView::Wave,
            spec_sub_view: crate::app::types::EditorSpecSubView::Spec,
            other_sub_view: crate::app::types::EditorOtherSubView::Tempogram,
            show_waveform_overlay: cached.show_waveform_overlay,
            channel_view: ChannelView::mixdown(),
            bpm_enabled: cached.bpm_enabled,
            bpm_value: cached.bpm_value,
            bpm_user_set: cached.bpm_user_set,
            bpm_offset_sec: cached.bpm_offset_sec,
            seek_hold: None,
            snap_zero_cross: cached.snap_zero_cross,
            selection_anchor_sample: None,
            right_drag_mode: None,
            active_tool: cached.active_tool,
            tool_state: cached.tool_state,
            loop_mode: cached.loop_mode,
            dragging_marker: None,
            preview_audio_tool: None,
            active_tool_last: None,
            preview_offset_samples: None,
            preview_overlay: None,
            music_analysis_draft: crate::app::types::MusicAnalysisDraft::default(),
            plugin_fx_draft: cached.plugin_fx_draft,
            pending_loop_unwrap: None,
            undo_stack: Vec::new(),
            undo_bytes: 0,
            redo_stack: Vec::new(),
            redo_bytes: 0,
        });
        self.workspace_view = crate::app::types::WorkspaceView::Editor;
        self.active_tab = Some(self.tabs.len() - 1);
        self.playing_path = Some(path.to_path_buf());
        self.apply_dirty_tab_audio_with_mode(path);
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
    self.tabs.push(EditorTab {
        path: path.to_path_buf(),
        display_name: name,
        waveform_minmax: Vec::new(),
        waveform_pyramid: None,
        loop_enabled: false,
        loading,
        ch_samples: Vec::new(),
        buffer_sample_rate: self.audio.shared.out_sample_rate.max(1),
        samples_len: 0,
        samples_len_visual: estimated_visual_frames.unwrap_or(0),
        loading_waveform_minmax: initial_loading_overview,
        view_offset: 0,
        view_offset_exact: 0.0,
        samples_per_px: 0.0,
        vertical_zoom: 1.0,
        vertical_view_center: 0.0,
        last_wave_w: 0.0,
        last_amplitude_nav_rect: None,
        last_amplitude_viewport_rect: None,
        last_amplitude_nav_click_at: 0.0,
        last_amplitude_nav_click_pos: None,
        viewport_source_generation: 1,
        viewport_render_requested_generation: 0,
        viewport_render_requested_key: None,
        viewport_render_pending_fine_at: None,
        viewport_render_inflight_coarse_generation: None,
        viewport_render_inflight_fine_generation: None,
        viewport_render_coarse: None,
        viewport_render_fine: None,
        viewport_render_last: None,
        dirty: false,
        ops: Vec::new(),
        selection: None,
        markers: Vec::new(),
        markers_committed: Vec::new(),
        markers_saved: Vec::new(),
        markers_applied: Vec::new(),
        markers_dirty: false,
        ab_loop: None,
        loop_region: None,
        loop_region_committed: None,
        loop_region_applied: None,
        loop_markers_saved: None,
        loop_markers_dirty: false,
        trim_range: None,
        loop_xfade_samples: 0,
        loop_xfade_shape: crate::app::types::LoopXfadeShape::EqualPower,
        fade_in_range: None,
        fade_out_range: None,
        fade_in_shape: crate::app::types::FadeShape::SCurve,
        fade_out_shape: crate::app::types::FadeShape::SCurve,
        primary_view: crate::app::types::EditorPrimaryView::Wave,
        spec_sub_view: crate::app::types::EditorSpecSubView::Spec,
        other_sub_view: crate::app::types::EditorOtherSubView::Tempogram,
        show_waveform_overlay: false,
        channel_view: ChannelView::mixdown(),
        bpm_enabled: false,
        bpm_value: default_bpm,
        bpm_user_set: false,
        bpm_offset_sec: 0.0,
        seek_hold: None,
        snap_zero_cross: true,
        selection_anchor_sample: None,
        right_drag_mode: None,
        active_tool: crate::app::types::ToolKind::LoopEdit,
        tool_state: crate::app::types::ToolState {
            fade_in_ms: 0.0,
            fade_out_ms: 0.0,
            gain_db: 0.0,
            normalize_target_db: -6.0,
            loudness_target_lufs: -14.0,
            pitch_semitones: 0.0,
            stretch_rate: 1.0,
            loop_repeat: 2,
        },
        loop_mode: crate::app::types::LoopMode::Off,
        dragging_marker: None,
        preview_audio_tool: None,
        active_tool_last: None,
        preview_offset_samples: None,
        preview_overlay: None,
        music_analysis_draft: crate::app::types::MusicAnalysisDraft::default(),
        plugin_fx_draft: crate::app::types::PluginFxDraft::default(),
        pending_loop_unwrap: None,
        undo_stack: Vec::new(),
        undo_bytes: 0,
        redo_stack: Vec::new(),
        redo_bytes: 0,
    });
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
        if let Some(item) = self.item_for_path(path) {
            if item.source == crate::app::types::MediaSource::External {
                continue;
            }
        }
        self.open_or_activate_tab(path);
    }
}
}
