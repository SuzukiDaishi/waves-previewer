use std::path::PathBuf;

use crate::app::types::{LoopMode, LoopXfadeShape, SortDir, SortKey, ToolKind, ViewMode};

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
            self.sort_dir = if default_asc { SortDir::Asc } else { SortDir::Desc };
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

    pub fn test_tab_dirty(&self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.tabs.get(tab_idx).map(|t| t.dirty).unwrap_or(false)
    }
}
