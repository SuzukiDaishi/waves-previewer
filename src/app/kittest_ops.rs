use std::path::{Path, PathBuf};

use crate::app::types::{LoopMode, LoopXfadeShape, RateMode, SortDir, SortKey, ToolKind, ViewMode};

#[cfg(feature = "kittest")]
impl super::WavesPreviewer {
    pub fn test_playing_path(&self) -> Option<&PathBuf> {
        self.playing_path.as_ref()
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
        self.audio
            .shared
            .samples
            .load()
            .as_ref()
            .map(|buf| buf.len() > 0)
            .unwrap_or(false)
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

    pub fn test_pending_gain_count(&self) -> usize {
        self.pending_gain_count()
    }

    pub fn test_auto_play_list_nav(&self) -> bool {
        self.auto_play_list_nav
    }

    pub fn test_volume_db(&self) -> f32 {
        self.volume_db
    }

    pub fn test_select_and_load_row(&mut self, row: usize) -> bool {
        if row >= self.files.len() {
            return false;
        }
        self.select_and_load(row, false);
        true
    }

    pub fn test_force_load_selected_list_preview_for_play(&mut self) -> bool {
        self.force_load_selected_list_preview_for_play()
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

    pub fn test_processing_autoplay_when_ready(&self) -> bool {
        self.processing
            .as_ref()
            .map(|p| p.autoplay_when_ready)
            .unwrap_or(false)
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
        tab.drag_select_anchor = None;
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
        let pos = ((tab.samples_len as f32) * frac)
            .round()
            .clamp(0.0, (tab.samples_len - 1) as f32) as usize;
        let label = Self::next_marker_label(&tab.markers);
        let entry = crate::markers::MarkerEntry { label, sample: pos };
        match tab.markers.binary_search_by_key(&pos, |m| m.sample) {
            Ok(idx) => tab.markers[idx] = entry,
            Err(idx) => tab.markers.insert(idx, entry),
        }
        tab.markers_dirty = true;
        true
    }

    pub fn test_clear_markers(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.markers.clear();
            tab.markers_dirty = true;
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
            tab.view_mode = mode;
            true
        } else {
            false
        }
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
        let Some(path) = self.selected_path_buf() else {
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
}
