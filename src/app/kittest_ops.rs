use std::path::{Path, PathBuf};

use crate::app::types::{
    LoopMode, LoopXfadeShape, MusicAnalysisResult, ProcessingResult, ProcessingState,
    ProcessingTarget, RateMode, SortDir, SortKey, ToolKind, ToolState, ViewMode,
};

#[cfg(feature = "kittest")]
impl super::WavesPreviewer {
    pub fn test_playing_path(&self) -> Option<&PathBuf> {
        self.playing_path.as_ref()
    }

    pub fn test_is_editor_workspace_active(&self) -> bool {
        self.is_editor_workspace_active()
    }

    pub fn test_selected_path(&self) -> Option<&PathBuf> {
        self.selected.and_then(|row| self.path_for_row(row))
    }

    pub fn test_audio_is_playing(&self) -> bool {
        self.audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn test_audio_has_samples(&self) -> bool {
        self.audio.has_audio_source()
    }

    pub fn test_audio_is_streaming_wav(&self, path: &Path) -> bool {
        self.audio.is_streaming_wav_path(path)
    }

    pub fn test_set_auto_play_list_nav(&mut self, enabled: bool) {
        self.auto_play_list_nav = enabled;
    }

    pub fn test_mode_name(&self) -> &'static str {
        match self.mode {
            crate::app::types::RateMode::Speed => "Speed",
            crate::app::types::RateMode::PitchShift => "PitchShift",
            crate::app::types::RateMode::TimeStretch => "TimeStretch",
        }
    }

    pub fn test_has_pending_gain(&self, path: &PathBuf) -> bool {
        self.pending_gain_db_for_path(path).abs() > 0.0001
    }

    pub fn test_show_export_settings(&self) -> bool {
        self.show_export_settings
    }

    pub fn test_show_transcription_settings(&self) -> bool {
        self.show_transcription_settings
    }

    pub fn test_set_show_export_settings(&mut self, show: bool) {
        self.show_export_settings = show;
    }

    pub fn test_set_show_transcription_settings(&mut self, show: bool) {
        self.show_transcription_settings = show;
    }

    pub fn test_audio_output_device_pref(&self) -> Option<String> {
        self.audio_output_device_name.clone()
    }

    pub fn test_audio_output_devices(&self) -> Vec<String> {
        self.audio_output_devices.clone()
    }

    pub fn test_audio_output_error(&self) -> Option<String> {
        self.audio_output_error.clone()
    }

    pub fn test_set_audio_output_device_pref(&mut self, name: Option<&str>) {
        self.audio_output_device_name = name.map(|v| v.to_string());
    }

    pub fn test_set_audio_output_devices(&mut self, devices: Vec<String>) {
        self.audio_output_devices = devices;
    }

    pub fn test_apply_audio_output_device_selection(
        &mut self,
        next: Option<&str>,
        persist: bool,
    ) -> bool {
        self.apply_audio_output_device_selection(next.map(|v| v.to_string()), persist)
    }

    pub fn test_save_prefs_to_path(&self, path: &Path) {
        self.save_prefs_to_path(path);
    }

    pub fn test_load_prefs_from_path(&mut self, path: &Path) {
        self.load_prefs_from_path(path);
    }

    pub fn test_pending_gain_count(&self) -> usize {
        self.pending_gain_count()
    }

    pub fn test_selected_multi_len(&self) -> usize {
        self.selected_multi.len()
    }

    pub fn test_files_len(&self) -> usize {
        self.files.len()
    }

    pub fn test_auto_play_list_nav(&self) -> bool {
        self.auto_play_list_nav
    }

    pub fn test_volume_db(&self) -> f32 {
        self.volume_db
    }

    pub fn test_set_volume_db(&mut self, db: f32) {
        self.volume_db = db;
        self.apply_effective_volume();
    }

    pub fn test_audio_output_volume_linear(&self) -> f32 {
        self.audio
            .shared
            .vol
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn test_audio_buffer_ptr(&self) -> usize {
        self.audio
            .shared
            .samples
            .load_full()
            .map(|buffer| std::sync::Arc::as_ptr(&buffer) as usize)
            .unwrap_or(0)
    }

    pub fn test_audio_buffer_sample(&self, channel: usize, index: usize) -> Option<f32> {
        let buffer = self.audio.shared.samples.load_full()?;
        let channel = buffer.channels.get(channel)?;
        channel.get(index).copied()
    }

    pub fn test_seed_prepared_audio_buffer(&mut self, mono: Vec<f32>) {
        let buffer = std::sync::Arc::new(crate::audio::AudioBuffer::from_mono(mono));
        self.audio.set_samples_buffer(buffer.clone());
        self.playback_session.dry_audio = Some(buffer);
        self.playback_session.last_applied_master_gain_db = self.volume_db;
        self.playback_session.last_applied_file_gain_db = 0.0;
    }

    pub fn test_set_list_gain_column_visible(&mut self, visible: bool) {
        self.list_columns.gain = visible;
    }

    pub fn test_select_and_load_row(&mut self, row: usize) -> bool {
        if row >= self.files.len() {
            return false;
        }
        self.select_and_load(row, false);
        true
    }

    pub fn test_force_load_selected_list_preview_for_play(&mut self) -> bool {
        let ready = self.force_load_selected_list_preview_for_play();
        if ready {
            self.audio.play();
            if let Some(path) = self.selected_path_buf() {
                self.debug_mark_list_play_start(&path);
            }
        }
        ready
    }

    pub fn test_set_rate_mode(&mut self, mode: RateMode) {
        self.mode = mode;
    }

    pub fn test_set_mode_pitch_shift(&mut self) {
        self.mode = RateMode::PitchShift;
    }

    pub fn test_set_mode_time_stretch(&mut self) {
        self.mode = RateMode::TimeStretch;
    }

    pub fn test_set_mode_speed(&mut self) {
        self.mode = RateMode::Speed;
    }

    pub fn test_set_pitch_semitones(&mut self, semitones: f32) {
        self.pitch_semitones = semitones;
    }

    pub fn test_set_playback_rate(&mut self, rate: f32) {
        self.playback_rate = rate;
    }

    pub fn test_refresh_playback_rate(&mut self) {
        self.playback_refresh_rate_for_current_source();
    }

    pub fn test_rebuild_current_buffer_with_mode(&mut self) {
        self.rebuild_current_buffer_with_mode();
    }

    pub fn test_audio_seek_to_sample(&mut self, pos: usize) {
        self.audio.seek_to_sample(pos);
    }

    pub fn test_playback_seek_to_source_time(&self, source_time_sec: f64) {
        self.playback_seek_to_source_time(self.mode, source_time_sec);
    }

    pub fn test_force_preview_restore_active_tab(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.preview_restore_audio_for_tab(tab_idx);
        true
    }

    pub fn test_playback_rate(&self) -> f32 {
        self.playback_rate
    }

    pub fn test_audio_rate(&self) -> f32 {
        self.audio
            .shared
            .rate
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn test_audio_out_sample_rate(&self) -> u32 {
        self.audio.shared.out_sample_rate.max(1)
    }

    pub fn test_playback_transport_name(&self) -> &'static str {
        match self.playback_session.transport {
            super::PlaybackTransportKind::Buffer => "Buffer",
            super::PlaybackTransportKind::ExactStreamWav => "ExactStreamWav",
        }
    }

    pub fn test_playback_transport_sr(&self) -> u32 {
        self.playback_session.transport_sr.max(1)
    }

    pub fn test_playback_current_source_time_sec(&self) -> Option<f64> {
        self.playback_current_source_time_sec()
    }

    pub fn test_selected_pending_gain_db(&self) -> Option<f32> {
        let path = self.test_selected_path()?;
        Some(self.pending_gain_db_for_path(path))
    }

    pub fn test_set_pending_gain_db_for_current_source(&mut self, db: f32) -> bool {
        let path = self.selected_path_buf().or_else(|| {
            self.active_tab
                .and_then(|idx| self.tabs.get(idx).map(|tab| tab.path.clone()))
        });
        let Some(path) = path else {
            return false;
        };
        self.set_pending_gain_db_for_path(&path, db);
        self.apply_effective_volume();
        true
    }

    pub fn test_processing_autoplay_when_ready(&self) -> bool {
        self.processing
            .as_ref()
            .map(|p| p.autoplay_when_ready)
            .unwrap_or(false)
    }

    pub fn test_processing_active(&self) -> bool {
        self.processing.is_some()
    }

    pub fn test_inject_processing_result(
        &mut self,
        path: &Path,
        state_target_editor: bool,
        result_target_editor: bool,
        state_mode: RateMode,
        result_mode: RateMode,
        state_job_id: u64,
        result_job_id: u64,
    ) {
        use std::sync::mpsc;
        let make_target = |editor: bool| {
            if editor {
                ProcessingTarget::EditorTab(path.to_path_buf())
            } else {
                ProcessingTarget::ListPreview(path.to_path_buf())
            }
        };
        let state_target = make_target(state_target_editor);
        let result_target = make_target(result_target_editor);
        let (tx, rx) = mpsc::channel();
        let channels = vec![vec![0.0; 1024], vec![0.0; 1024]];
        let _ = tx.send(ProcessingResult {
            path: path.to_path_buf(),
            job_id: result_job_id,
            mode: result_mode,
            target: result_target,
            samples: vec![0.0; 1024],
            waveform: Vec::new(),
            channels,
        });
        self.processing = Some(ProcessingState {
            msg: "Test processing".to_string(),
            path: path.to_path_buf(),
            job_id: state_job_id,
            mode: state_mode,
            target: state_target,
            autoplay_when_ready: false,
            source_time_sec: None,
            started_at: std::time::Instant::now(),
            rx,
        });
    }

    pub fn test_spawn_heavy_processing_from_active_tab(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let path = tab.path.clone();
        let channels = tab.ch_samples.clone();
        self.spawn_heavy_processing_from_channels(
            path.clone(),
            channels,
            ProcessingTarget::EditorTab(path),
        );
        self.processing.is_some()
    }

    pub fn test_set_sort(&mut self, key: SortKey, dir: SortDir) {
        self.sort_key = key;
        self.sort_dir = dir;
        self.apply_sort();
    }

    pub fn test_sort_sample_rate_asc(&mut self) {
        self.sort_key = SortKey::SampleRate;
        self.sort_dir = SortDir::Asc;
        self.apply_sort();
    }

    pub fn test_sort_sample_rate_desc(&mut self) {
        self.sort_key = SortKey::SampleRate;
        self.sort_dir = SortDir::Desc;
        self.apply_sort();
    }

    pub fn test_row_path(&self, row: usize) -> Option<PathBuf> {
        self.path_for_row(row).cloned()
    }

    pub fn test_evict_selected_list_preview_cache(&mut self) -> bool {
        let Some(path) = self.test_selected_path().cloned() else {
            return false;
        };
        self.evict_list_preview_cache_path(&path);
        true
    }

    pub fn test_sort_key_name(&self) -> &'static str {
        match self.sort_key {
            SortKey::File => "File",
            SortKey::Folder => "Folder",
            SortKey::Transcript => "Transcript",
            SortKey::Type => "Type",
            SortKey::Length => "Length",
            SortKey::Channels => "Channels",
            SortKey::SampleRate => "SampleRate",
            SortKey::Bits => "Bits",
            SortKey::BitRate => "BitRate",
            SortKey::Level => "Level",
            SortKey::Lufs => "Lufs",
            SortKey::Bpm => "Bpm",
            SortKey::CreatedAt => "CreatedAt",
            SortKey::ModifiedAt => "ModifiedAt",
            SortKey::External(_) => "External",
        }
    }

    pub fn test_sort_dir_name(&self) -> &'static str {
        match self.sort_dir {
            SortDir::Asc => "Asc",
            SortDir::Desc => "Desc",
            SortDir::None => "None",
        }
    }

    pub fn test_cycle_sort_file(&mut self) {
        self.test_cycle_sort(SortKey::File, true);
    }

    fn test_cycle_sort(&mut self, key: SortKey, default_asc: bool) {
        if self.sort_key != key {
            self.sort_key = key;
            self.sort_dir = if default_asc {
                SortDir::Asc
            } else {
                SortDir::Desc
            };
        } else {
            self.sort_dir = match self.sort_dir {
                SortDir::Asc => {
                    if default_asc {
                        SortDir::Desc
                    } else {
                        SortDir::None
                    }
                }
                SortDir::Desc => {
                    if default_asc {
                        SortDir::None
                    } else {
                        SortDir::Asc
                    }
                }
                SortDir::None => {
                    if default_asc {
                        SortDir::Asc
                    } else {
                        SortDir::Desc
                    }
                }
            };
        }
        self.apply_sort();
    }

    pub fn test_set_search_query(&mut self, query: &str) {
        self.search_query = query.to_string();
        self.apply_filter_from_search();
        self.apply_sort();
        self.search_dirty = false;
        self.search_deadline = None;
    }

    pub fn test_replace_with_files(&mut self, paths: &[PathBuf]) {
        self.replace_with_files(paths);
        self.after_add_refresh();
    }

    pub fn test_add_paths(&mut self, paths: &[PathBuf]) -> usize {
        let added = self.add_files_merge(paths);
        self.after_add_refresh();
        added
    }

    pub fn test_open_first_tab(&mut self) -> bool {
        if self.files.is_empty() {
            return false;
        }
        let row = 0;
        self.select_and_load(row, true);
        let Some(path) = self.path_for_row(row).cloned() else {
            return false;
        };
        self.open_or_activate_tab(&path);
        true
    }

    pub fn test_open_tab_for_path(&mut self, path: &Path) -> bool {
        if self.row_for_path(path).is_none() {
            return false;
        }
        self.open_or_activate_tab(path);
        true
    }

    pub fn test_clear_meta_for_path(&mut self, path: &Path) {
        self.clear_meta_for_path(path);
        self.meta_inflight.remove(path);
    }

    pub fn test_show_list_art_window_placeholder(&mut self, path: &Path) {
        self.show_list_art_window = true;
        self.list_art_window_path = Some(path.to_path_buf());
        self.list_art_window_texture = None;
        self.list_art_window_error = None;
    }

    pub fn test_set_active_tool(&mut self, tool: ToolKind) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.active_tool = tool;
            true
        } else {
            false
        }
    }

    pub fn test_active_tool(&self) -> Option<ToolKind> {
        let tab_idx = self.active_tab?;
        self.tabs.get(tab_idx).map(|tab| tab.active_tool)
    }

    pub fn test_preview_audio_tool(&self) -> Option<ToolKind> {
        let tab_idx = self.active_tab?;
        self.tabs.get(tab_idx).and_then(|tab| tab.preview_audio_tool)
    }

    pub fn test_preview_overlay_tool(&self) -> Option<ToolKind> {
        let tab_idx = self.active_tab?;
        self.tabs
            .get(tab_idx)
            .and_then(|tab| tab.preview_overlay.as_ref().map(|overlay| overlay.source_tool))
    }

    pub fn test_preview_overlay_present(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs
            .get(tab_idx)
            .map(|tab| tab.preview_overlay.is_some())
            .unwrap_or(false)
    }

    pub fn test_preview_busy_for_active_tab(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.current_tab_preview_busy(tab_idx)
    }

    pub fn test_refresh_tool_preview_active_tab(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.refresh_tool_preview_for_tab(tab_idx);
        true
    }

    pub fn test_set_tool_gain_db(&mut self, gain_db: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.tool_state = ToolState {
            gain_db,
            ..tab.tool_state
        };
        true
    }

    pub fn test_set_tool_normalize_target_db(&mut self, target_db: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.tool_state = ToolState {
            normalize_target_db: target_db,
            ..tab.tool_state
        };
        true
    }

    pub fn test_set_tool_fade_ms(&mut self, fade_in_ms: f32, fade_out_ms: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.tool_state = ToolState {
            fade_in_ms,
            fade_out_ms,
            ..tab.tool_state
        };
        true
    }

    pub fn test_set_tool_pitch_semitones(&mut self, semitones: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.tool_state = ToolState {
            pitch_semitones: semitones,
            ..tab.tool_state
        };
        true
    }

    pub fn test_set_tool_stretch_rate(&mut self, stretch_rate: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.tool_state = ToolState {
            stretch_rate,
            ..tab.tool_state
        };
        true
    }

    pub fn test_set_bpm_offset_sec(&mut self, offset_sec: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.bpm_offset_sec = offset_sec;
            true
        } else {
            false
        }
    }

    pub fn test_bpm_offset_sec(&self) -> Option<f32> {
        let tab_idx = self.active_tab?;
        self.tabs.get(tab_idx).map(|tab| tab.bpm_offset_sec)
    }

    pub fn test_set_selection_frac(&mut self, start: f32, end: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        let Some((s, e)) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        tab.selection = Some((s, e));
        tab.selection_anchor_sample = None;
        tab.right_drag_mode = None;
        true
    }

    pub fn test_tab_selection(&self) -> Option<(usize, usize)> {
        let tab_idx = self.active_tab?;
        self.tabs.get(tab_idx).and_then(|tab| tab.selection)
    }

    pub fn test_tab_selection_anchor(&self) -> Option<usize> {
        let tab_idx = self.active_tab?;
        self.tabs
            .get(tab_idx)
            .and_then(|tab| tab.selection_anchor_sample)
    }

    pub fn test_tab_right_drag_mode(&self) -> Option<&'static str> {
        let tab_idx = self.active_tab?;
        let tab = self.tabs.get(tab_idx)?;
        tab.right_drag_mode.map(|mode| match mode {
            crate::app::types::RightDragMode::Seek => "Seek",
            crate::app::types::RightDragMode::SelectRange => "SelectRange",
        })
    }

    pub fn test_simulate_right_drag_from_frac(
        &mut self,
        start_frac: f32,
        shift: bool,
        to_frac: f32,
    ) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        if tab.samples_len == 0 {
            return false;
        }
        let max_idx = tab.samples_len.saturating_sub(1);
        let anchor = ((tab.samples_len as f32) * start_frac.clamp(0.0, 1.0))
            .round()
            .clamp(0.0, max_idx as f32) as usize;
        let target = ((tab.samples_len as f32) * to_frac.clamp(0.0, 1.0))
            .round()
            .clamp(0.0, max_idx as f32) as usize;
        tab.right_drag_mode = Some(if shift {
            crate::app::types::RightDragMode::SelectRange
        } else {
            crate::app::types::RightDragMode::Seek
        });
        if shift {
            tab.selection_anchor_sample = Some(anchor);
            let (s, e) = if target >= anchor {
                (anchor, target)
            } else {
                (target, anchor)
            };
            tab.selection = Some((s, e));
        } else {
            self.audio.seek_to_sample(target);
        }
        tab.right_drag_mode = None;
        true
    }

    pub fn test_simulate_right_drag(&mut self, shift: bool, to_frac: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        if tab.samples_len == 0 {
            return false;
        }
        let max_idx = tab.samples_len.saturating_sub(1).max(1);
        let anchor = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(max_idx) as f32
            / max_idx as f32;
        self.test_simulate_right_drag_from_frac(anchor, shift, to_frac)
    }

    pub fn test_last_play_start_display_sample(&self) -> Option<usize> {
        self.playback_session.last_play_start_display_sample
    }

    pub fn test_tab_vertical_zoom(&self) -> Option<f32> {
        let tab_idx = self.active_tab?;
        self.tabs.get(tab_idx).map(|tab| tab.vertical_zoom)
    }

    pub fn test_tab_vertical_view_center(&self) -> Option<f32> {
        let tab_idx = self.active_tab?;
        self.tabs.get(tab_idx).map(|tab| tab.vertical_view_center)
    }

    pub fn test_set_tab_vertical_zoom(&mut self, zoom: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.vertical_zoom = zoom.clamp(crate::app::EDITOR_MIN_VERTICAL_ZOOM, crate::app::EDITOR_MAX_VERTICAL_ZOOM);
        Self::editor_clamp_vertical_view(tab);
        true
    }

    pub fn test_set_tab_vertical_view_center(&mut self, center: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.vertical_view_center = center.clamp(-1.0, 1.0);
        Self::editor_clamp_vertical_view(tab);
        true
    }

    pub fn test_tab_amplitude_nav_rect(&self) -> Option<egui::Rect> {
        let tab_idx = self.active_tab?;
        self.tabs
            .get(tab_idx)
            .and_then(|tab| tab.last_amplitude_nav_rect)
    }

    pub fn test_tab_amplitude_nav_viewport_rect(&self) -> Option<egui::Rect> {
        let tab_idx = self.active_tab?;
        self.tabs
            .get(tab_idx)
            .and_then(|tab| tab.last_amplitude_viewport_rect)
    }

    pub fn test_tab_amplitude_nav_reserved_width(&self) -> Option<f32> {
        let tab_idx = self.active_tab?;
        self.tabs.get(tab_idx).and_then(|tab| {
            let nav = tab.last_amplitude_nav_rect?;
            Some((tab.last_wave_w + nav.width() + 30.0 - 18.0) - tab.last_wave_w)
        })
    }

    pub fn test_tab_amplitude_nav_strip_width(&self) -> Option<f32> {
        let tab_idx = self.active_tab?;
        self.tabs
            .get(tab_idx)
            .and_then(|tab| tab.last_amplitude_nav_rect.map(|rect| rect.width()))
    }

    pub fn test_clear_tab_amplitude_nav_rects(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.last_amplitude_nav_rect = None;
        tab.last_amplitude_viewport_rect = None;
        true
    }

    pub fn test_tab_view_offset(&self) -> Option<usize> {
        let tab_idx = self.active_tab?;
        self.tabs.get(tab_idx).map(|tab| tab.view_offset)
    }

    pub fn test_set_tab_view_offset(&mut self, view_offset: usize) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.view_offset = view_offset.min(tab.samples_len.saturating_sub(1));
        tab.view_offset_exact = tab.view_offset as f64;
        true
    }

    pub fn test_tab_samples_per_px(&self) -> Option<f32> {
        let tab_idx = self.active_tab?;
        self.tabs.get(tab_idx).map(|tab| tab.samples_per_px)
    }

    pub fn test_meter_db(&self) -> f32 {
        self.meter_db
    }

    pub fn test_editor_pref_invert_wave_zoom_wheel(&self) -> bool {
        self.invert_wave_zoom_wheel
    }

    pub fn test_set_editor_pref_invert_wave_zoom_wheel(&mut self, enabled: bool) {
        self.invert_wave_zoom_wheel = enabled;
    }

    pub fn test_editor_pref_invert_shift_wheel_pan(&self) -> bool {
        self.invert_shift_wheel_pan
    }

    pub fn test_set_editor_pref_invert_shift_wheel_pan(&mut self, enabled: bool) {
        self.invert_shift_wheel_pan = enabled;
    }

    pub fn test_editor_pref_horizontal_zoom_anchor(&self) -> &'static str {
        match self.horizontal_zoom_anchor_mode {
            crate::app::types::EditorHorizontalZoomAnchorMode::Pointer => "pointer",
            crate::app::types::EditorHorizontalZoomAnchorMode::Playhead => "playhead",
        }
    }

    pub fn test_set_editor_pref_horizontal_zoom_anchor(&mut self, mode: &str) -> bool {
        self.horizontal_zoom_anchor_mode = match mode {
            "pointer" => crate::app::types::EditorHorizontalZoomAnchorMode::Pointer,
            "playhead" => crate::app::types::EditorHorizontalZoomAnchorMode::Playhead,
            _ => return false,
        };
        true
    }

    pub fn test_editor_pref_pause_resume_mode(&self) -> &'static str {
        match self.editor_pause_resume_mode {
            crate::app::types::EditorPauseResumeMode::ReturnToLastStart => "return_to_last_start",
            crate::app::types::EditorPauseResumeMode::ContinueFromPause => "continue_from_pause",
        }
    }

    pub fn test_set_editor_pref_pause_resume_mode(&mut self, mode: &str) -> bool {
        self.editor_pause_resume_mode = match mode {
            "return_to_last_start" => crate::app::types::EditorPauseResumeMode::ReturnToLastStart,
            "continue_from_pause" => crate::app::types::EditorPauseResumeMode::ContinueFromPause,
            _ => return false,
        };
        true
    }

    pub fn test_set_trim_range_frac(&mut self, start: f32, end: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        let Some((s, e)) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        tab.trim_range = Some((s, e));
        true
    }

    pub fn test_set_loop_region_frac(&mut self, start: f32, end: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        let Some((s, e)) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        tab.loop_region = Some((s, e));
        Self::update_loop_markers_dirty(tab);
        true
    }

    pub fn test_set_loop_mode(&mut self, mode: LoopMode) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.loop_mode = mode;
        }
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
            true
        } else {
            false
        }
    }

    pub fn test_set_loop_xfade_ms(&mut self, ms: f32, shape: LoopXfadeShape) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
        let samp = ((ms / 1000.0) * sr).round().max(0.0) as usize;
        tab.loop_xfade_samples = samp.min(tab.samples_len / 2);
        tab.loop_xfade_shape = shape;
        true
    }

    pub fn test_add_marker_frac(&mut self, frac: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        if tab.samples_len == 0 {
            return false;
        }
        let mut pos = ((tab.samples_len as f32) * frac)
            .round()
            .clamp(0.0, (tab.samples_len - 1) as f32) as usize;
        while pos < tab.samples_len && tab.markers.iter().any(|m| m.sample == pos) {
            pos = pos.saturating_add(1);
        }
        if pos >= tab.samples_len {
            return false;
        }
        let label = Self::next_marker_label(&tab.markers);
        let entry = crate::markers::MarkerEntry { label, sample: pos };
        match tab.markers.binary_search_by_key(&pos, |m| m.sample) {
            Ok(idx) => tab.markers[idx] = entry,
            Err(idx) => tab.markers.insert(idx, entry),
        }
        Self::update_markers_dirty(tab);
        true
    }

    pub fn test_clear_markers(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.markers.clear();
            Self::update_markers_dirty(tab);
            true
        } else {
            false
        }
    }

    pub fn test_marker_count(&self) -> usize {
        let Some(tab_idx) = self.active_tab else {
            return 0;
        };
        self.tabs.get(tab_idx).map(|t| t.markers.len()).unwrap_or(0)
    }

    pub fn test_loop_region(&self) -> Option<(usize, usize)> {
        let Some(tab_idx) = self.active_tab else {
            return None;
        };
        self.tabs.get(tab_idx).and_then(|t| t.loop_region)
    }

    pub fn test_write_markers(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.write_markers_for_tab(tab_idx)
    }

    pub fn test_write_loop_markers(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.write_loop_markers_for_tab(tab_idx)
    }

    pub fn test_set_view_mode(&mut self, mode: ViewMode) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.set_leaf_view_mode(mode);
            true
        } else {
            false
        }
    }

    pub fn test_set_music_preview_gains_db(
        &mut self,
        bass: f32,
        drums: f32,
        other: f32,
        vocals: f32,
    ) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.music_analysis_draft.preview_gains_db.bass = bass;
        tab.music_analysis_draft.preview_gains_db.drums = drums;
        tab.music_analysis_draft.preview_gains_db.other = other;
        tab.music_analysis_draft.preview_gains_db.vocals = vocals;
        true
    }

    pub fn test_set_music_analysis_result_mock(&mut self, enabled: bool) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.music_analysis_draft.result = if enabled {
            Some(MusicAnalysisResult::default())
        } else {
            None
        };
        true
    }

    pub fn test_music_preview_gains_db(&self) -> Option<(f32, f32, f32, f32)> {
        let tab_idx = self.active_tab?;
        let tab = self.tabs.get(tab_idx)?;
        Some((
            tab.music_analysis_draft.preview_gains_db.bass,
            tab.music_analysis_draft.preview_gains_db.drums,
            tab.music_analysis_draft.preview_gains_db.other,
            tab.music_analysis_draft.preview_gains_db.vocals,
        ))
    }

    pub fn test_set_waveform_overlay(&mut self, enabled: bool) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.show_waveform_overlay = enabled;
            true
        } else {
            false
        }
    }

    pub fn test_tab_samples_len(&self) -> usize {
        let Some(tab_idx) = self.active_tab else {
            return 0;
        };
        self.tabs.get(tab_idx).map(|t| t.samples_len).unwrap_or(0)
    }

    pub fn test_tab_loading(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs.get(tab_idx).map(|t| t.loading).unwrap_or(false)
    }

    pub fn test_editor_decode_progress(&self) -> Option<f32> {
        self.editor_decode_ui_status(None)
            .map(|status| status.progress)
    }

    pub fn test_editor_decode_message(&self) -> Option<String> {
        self.editor_decode_ui_status(None)
            .map(|status| status.message)
    }

    pub fn test_active_tab_loading_waveform_ready(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs
            .get(tab_idx)
            .map(|tab| !tab.loading_waveform_minmax.is_empty())
            .unwrap_or(false)
    }

    pub fn test_active_tab_loading_waveform_nonflat(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs
            .get(tab_idx)
            .map(|tab| {
                tab.loading_waveform_minmax.iter().any(|(mn, mx)| {
                    mn.abs() > 1.0e-5 || mx.abs() > 1.0e-5 || (mx - mn).abs() > 1.0e-5
                })
            })
            .unwrap_or(false)
    }

    pub fn test_request_workspace_play_toggle(&mut self) {
        self.request_workspace_play_toggle();
    }

    pub fn test_active_editor_exact_audio_ready(&self) -> bool {
        self.active_editor_exact_audio_ready()
    }

    pub fn test_active_tab_samples_len_visual(&self) -> usize {
        let Some(tab_idx) = self.active_tab else {
            return 0;
        };
        self.tabs
            .get(tab_idx)
            .map(|tab| tab.samples_len_visual)
            .unwrap_or(0)
    }

    pub fn test_active_tab_path(&self) -> Option<PathBuf> {
        let Some(tab_idx) = self.active_tab else {
            return None;
        };
        self.tabs.get(tab_idx).map(|t| t.path.clone())
    }

    pub fn test_tab_dirty(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs.get(tab_idx).map(|t| t.dirty).unwrap_or(false)
    }

    pub fn test_marker_dirty(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs
            .get(tab_idx)
            .map(|t| t.markers_dirty)
            .unwrap_or(false)
    }

    pub fn test_loop_marker_dirty(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs
            .get(tab_idx)
            .map(|t| t.loop_markers_dirty)
            .unwrap_or(false)
    }

    pub fn test_marker_preview_pending(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs
            .get(tab_idx)
            .map(|t| t.markers != t.markers_committed)
            .unwrap_or(false)
    }

    pub fn test_loop_preview_pending(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs
            .get(tab_idx)
            .map(|t| t.loop_region != t.loop_region_committed)
            .unwrap_or(false)
    }

    pub fn test_audio_loop_xfade_samples(&self) -> usize {
        self.audio
            .shared
            .loop_xfade_samples
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn test_add_trim_virtual_frac(&mut self, start: f32, end: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some((s, e)) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        if e <= s {
            return false;
        }
        self.add_trim_range_as_virtual(tab_idx, (s, e));
        true
    }

    pub fn test_virtual_item_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.source == crate::app::types::MediaSource::Virtual)
            .count()
    }

    pub fn test_set_selected_sample_rate_override(&mut self, sample_rate: u32) -> bool {
        let path = self.selected_path_buf().or_else(|| {
            self.active_tab
                .and_then(|idx| self.tabs.get(idx).map(|tab| tab.path.clone()))
        });
        let Some(path) = path else {
            return false;
        };
        if sample_rate == 0 {
            self.sample_rate_override.remove(&path);
        } else {
            self.sample_rate_override.insert(path, sample_rate);
        }
        true
    }

    pub fn test_sample_rate_override_count(&self) -> usize {
        self.sample_rate_override.len()
    }

    pub fn test_selected_sample_rate_override(&self) -> Option<u32> {
        let path = self.test_selected_path()?;
        self.sample_rate_override.get(path).copied()
    }

    pub fn test_apply_selected_resample_override(&mut self, target_sr: u32) -> bool {
        let selected = self.selected_paths();
        if selected.is_empty() {
            return false;
        }
        self.open_resample_dialog(selected);
        self.resample_target_sr = target_sr.max(1);
        self.apply_resample_dialog().is_ok()
    }

    pub fn test_convert_bits_selected_to(&mut self, depth: crate::wave::WavBitDepth) -> bool {
        let selected = self.selected_paths();
        if selected.is_empty() {
            return false;
        }
        self.spawn_convert_bits_selected(selected, depth);
        true
    }

    pub fn test_selected_bit_depth_override(&self) -> Option<crate::wave::WavBitDepth> {
        let path = self.test_selected_path()?;
        self.bit_depth_override.get(path).copied()
    }

    pub fn test_convert_format_selected_to(&mut self, ext: &str) -> bool {
        let selected = self.selected_paths();
        if selected.is_empty() {
            return false;
        }
        self.spawn_convert_format_selected(selected, ext);
        true
    }

    pub fn test_selected_format_override(&self) -> Option<String> {
        let path = self.test_selected_path()?;
        self.format_override.get(path).cloned()
    }

    pub fn test_selected_display_name(&self) -> Option<String> {
        let path = self.test_selected_path()?;
        self.item_for_path(path)
            .map(|item| item.display_name.clone())
    }

    pub fn test_selected_bits_label(&self) -> Option<String> {
        let path = self.test_selected_path()?;
        self.effective_bits_label_for_path(path)
    }

    pub fn test_rename_selected_to(&mut self, new_name: &str) -> bool {
        let Some(path) = self.test_selected_path().cloned() else {
            return false;
        };
        self.rename_file_path(&path, new_name).is_ok()
    }

    pub fn test_list_undo(&mut self) -> bool {
        self.list_undo()
    }

    pub fn test_select_path(&mut self, path: &Path) -> bool {
        let Some(row) = self.row_for_path(path) else {
            return false;
        };
        self.select_and_load(row, true);
        true
    }

    pub fn test_select_paths_multi(&mut self, paths: &[PathBuf]) -> bool {
        if paths.is_empty() {
            return false;
        }
        let mut rows = Vec::new();
        for path in paths {
            if let Some(row) = self.row_for_path(path) {
                rows.push(row);
            }
        }
        if rows.is_empty() {
            return false;
        }
        rows.sort_unstable();
        rows.dedup();
        self.selected_multi.clear();
        for row in &rows {
            self.selected_multi.insert(*row);
        }
        let first = *rows.first().unwrap_or(&rows[0]);
        self.selected = Some(first);
        self.select_anchor = Some(first);
        if let Some(path) = self.path_for_row(first).cloned() {
            self.open_or_activate_tab(&path);
        }
        true
    }

    pub fn test_switch_to_list(&mut self) {
        if let Some(active) = self.active_tab {
            self.clear_preview_if_any(active);
        }
        self.active_tab = None;
        self.audio.stop();
        self.audio.set_loop_enabled(false);
    }

    pub fn test_close_active_tab(&mut self) -> bool {
        let Some(active_idx) = self.active_tab else {
            return false;
        };
        let ctx = egui::Context::default();
        self.close_tab_at(active_idx, &ctx);
        true
    }

    pub fn test_close_tab_for_path(&mut self, path: &Path) -> bool {
        let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == path) else {
            return false;
        };
        let ctx = egui::Context::default();
        self.close_tab_at(idx, &ctx);
        true
    }

    pub fn test_audio_buffer_len(&self) -> usize {
        self.audio
            .shared
            .samples
            .load()
            .as_ref()
            .map(|b| b.len())
            .unwrap_or(0)
    }

    pub fn test_audio_play_pos(&self) -> usize {
        self.audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn test_audio_play_pos_f(&self) -> f64 {
        self.audio
            .shared
            .play_pos_f
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn test_tab_ranges_in_bounds(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let len = tab.samples_len;
        let valid_range = |r: Option<(usize, usize)>| -> bool {
            match r {
                None => true,
                Some((s, e)) => s < e && e <= len,
            }
        };
        if !valid_range(tab.selection) {
            return false;
        }
        if !valid_range(tab.trim_range) {
            return false;
        }
        if !valid_range(tab.fade_in_range) {
            return false;
        }
        if !valid_range(tab.fade_out_range) {
            return false;
        }
        if !valid_range(tab.loop_region) {
            return false;
        }
        tab.view_offset <= len.saturating_sub(1)
    }

    pub fn test_set_external_show_unmatched(&mut self, enabled: bool) {
        self.external_show_unmatched = enabled;
    }

    pub fn test_external_show_unmatched(&self) -> bool {
        self.external_show_unmatched
    }

    pub fn test_save_session_to(&mut self, path: &Path) -> bool {
        self.save_project_as(path.to_path_buf()).is_ok()
    }

    pub fn test_open_session_from(&mut self, path: &Path) -> bool {
        self.open_project_file(path.to_path_buf()).is_ok()
    }

    pub fn test_set_channel_view_mixdown(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.channel_view = crate::app::types::ChannelView::mixdown();
        true
    }

    pub fn test_set_channel_view_all(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.channel_view = crate::app::types::ChannelView {
            mode: crate::app::types::ChannelViewMode::All,
            selected: Vec::new(),
        };
        true
    }

    pub fn test_set_channel_view_custom(&mut self, selected: Vec<usize>) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        tab.channel_view = crate::app::types::ChannelView {
            mode: crate::app::types::ChannelViewMode::Custom,
            selected,
        };
        true
    }

    pub fn test_waveform_lod_counts(&self) -> (u64, u64, u64) {
        (
            self.debug.waveform_lod_raw_count,
            self.debug.waveform_lod_visible_count,
            self.debug.waveform_lod_pyramid_count,
        )
    }

    pub fn test_active_tab_waveform_pyramid_ready(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs
            .get(tab_idx)
            .and_then(|tab| tab.waveform_pyramid.as_ref())
            .is_some()
    }

    pub fn test_set_export_save_mode_overwrite(&mut self, overwrite: bool) {
        self.export_cfg.save_mode = if overwrite {
            crate::app::types::SaveMode::Overwrite
        } else {
            crate::app::types::SaveMode::NewFile
        };
    }

    pub fn test_set_export_first_prompt(&mut self, enabled: bool) {
        self.export_cfg.first_prompt = enabled;
    }

    pub fn test_export_save_mode_name(&self) -> &'static str {
        match self.export_cfg.save_mode {
            crate::app::types::SaveMode::Overwrite => "Overwrite",
            crate::app::types::SaveMode::NewFile => "NewFile",
        }
    }

    pub fn test_set_export_conflict(&mut self, name: &str) {
        self.export_cfg.conflict = match name.trim().to_ascii_lowercase().as_str() {
            "overwrite" => crate::app::types::ConflictPolicy::Overwrite,
            "skip" => crate::app::types::ConflictPolicy::Skip,
            _ => crate::app::types::ConflictPolicy::Rename,
        };
    }

    pub fn test_export_conflict_name(&self) -> &'static str {
        match self.export_cfg.conflict {
            crate::app::types::ConflictPolicy::Rename => "Rename",
            crate::app::types::ConflictPolicy::Overwrite => "Overwrite",
            crate::app::types::ConflictPolicy::Skip => "Skip",
        }
    }

    pub fn test_set_export_backup_bak(&mut self, enabled: bool) {
        self.export_cfg.backup_bak = enabled;
    }

    pub fn test_export_backup_bak(&self) -> bool {
        self.export_cfg.backup_bak
    }

    pub fn test_set_export_name_template(&mut self, template: &str) {
        self.export_cfg.name_template = template.to_string();
    }

    pub fn test_export_name_template(&self) -> &str {
        &self.export_cfg.name_template
    }

    pub fn test_set_transcript_language(&mut self, language: &str) {
        self.transcript_ai_cfg.language = language.to_string();
    }

    pub fn test_transcript_language(&self) -> &str {
        &self.transcript_ai_cfg.language
    }

    pub fn test_set_selected_item_transcript_language(&mut self, lang: Option<&str>) -> bool {
        let Some(path) = self.test_selected_path().cloned() else {
            return false;
        };
        self.set_transcript_language_for_path(&path, lang.map(|v| v.to_string()))
    }

    pub fn test_selected_item_transcript_language(&self) -> Option<String> {
        let path = self.test_selected_path()?;
        self.transcript_language_for_path(path)
            .map(|v| v.to_string())
    }

    pub fn test_set_export_dest_folder(&mut self, dest: Option<&Path>) {
        self.export_cfg.dest_folder = dest.map(|p| p.to_path_buf());
    }

    pub fn test_set_export_format_override(&mut self, ext: Option<&str>) {
        self.export_cfg.format_override = ext.map(|v| v.to_string());
    }

    pub fn test_export_dest_folder(&self) -> Option<&PathBuf> {
        self.export_cfg.dest_folder.as_ref()
    }

    pub fn test_trigger_save_selected(&mut self) {
        self.trigger_save_selected();
    }

    pub fn test_export_in_progress(&self) -> bool {
        self.export_state.is_some()
    }

    pub fn test_undo_last_overwrite_export(&mut self) -> bool {
        self.undo_last_overwrite_export()
    }

    pub fn test_debug_summary_text(&self) -> String {
        self.debug_summary()
    }

    pub fn test_effect_graph_workspace_open(&self) -> bool {
        self.effect_graph.workspace_open
    }

    pub fn test_effect_graph_target_path(&self) -> Option<PathBuf> {
        self.effect_graph.tester.target_path.clone()
    }

    pub fn test_open_effect_graph_workspace(&mut self) {
        self.open_effect_graph_workspace();
    }

    pub fn test_add_effect_graph_plugin_node(&mut self) -> bool {
        let Some(node_id) = self.effect_graph_add_node(
            crate::app::types::EffectGraphNodeKind::PluginFx,
            [180.0, 180.0],
        ) else {
            return false;
        };
        self.effect_graph.canvas.selected_nodes.clear();
        self.effect_graph.canvas.selected_nodes.insert(node_id);
        true
    }

    pub fn test_set_spectro_hop_size(&mut self, hop_size: usize) {
        let mut next = self.spectro_cfg.clone();
        next.hop_size = hop_size.max(1);
        self.apply_spectro_config(next);
    }

    pub fn test_spectro_hop_size(&self) -> usize {
        self.spectro_cfg.hop_size
    }

    pub fn test_spectro_overlap(&self) -> f32 {
        self.spectro_cfg.overlap
    }

    pub fn test_set_mock_transcript_model_download_progress(&mut self, done: usize, total: usize) {
        let (_tx, rx) = std::sync::mpsc::channel();
        let total = total.max(1);
        self.transcript_model_download_state = Some(super::TranscriptModelDownloadState {
            _started_at: std::time::Instant::now(),
            done: done.min(total),
            total,
            rx,
        });
    }

    pub fn test_set_mock_music_model_download_progress(&mut self, done: usize, total: usize) {
        let (_tx, rx) = std::sync::mpsc::channel();
        let total = total.max(1);
        self.music_model_download_state = Some(super::MusicModelDownloadState {
            _started_at: std::time::Instant::now(),
            done: done.min(total),
            total,
            rx,
        });
    }

    pub fn test_clear_mock_model_download_progress(&mut self) {
        self.transcript_model_download_state = None;
        self.music_model_download_state = None;
    }
}
