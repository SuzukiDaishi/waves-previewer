use std::path::{Path, PathBuf};

use super::types::{
    FileMeta, MediaId, MediaItem, MediaSource, SampleValueKind, SortDir, SortKey, Transcript,
};
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn is_dotfile_path(path: &Path) -> bool {
        path.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.starts_with('.'))
            .unwrap_or(false)
    }

    pub(super) fn is_decode_failed_path(&self, path: &Path) -> bool {
        self.meta_for_path(path)
            .and_then(|m| m.decode_error.as_ref())
            .is_some()
    }

    pub(super) fn item_for_id(&self, id: MediaId) -> Option<&MediaItem> {
        self.item_index
            .get(&id)
            .and_then(|&idx| self.items.get(idx))
    }

    pub(super) fn item_for_id_mut(&mut self, id: MediaId) -> Option<&mut MediaItem> {
        let idx = *self.item_index.get(&id)?;
        self.items.get_mut(idx)
    }

    pub(super) fn item_for_row(&self, row_idx: usize) -> Option<&MediaItem> {
        let id = *self.files.get(row_idx)?;
        self.item_for_id(id)
    }

    pub(super) fn item_for_path(&self, path: &Path) -> Option<&MediaItem> {
        let id = *self.path_index.get(path)?;
        self.item_for_id(id)
    }

    pub(super) fn item_for_path_mut(&mut self, path: &Path) -> Option<&mut MediaItem> {
        let id = *self.path_index.get(path)?;
        self.item_for_id_mut(id)
    }

    pub(super) fn is_virtual_path(&self, path: &Path) -> bool {
        self.item_for_path(path)
            .map(|item| matches!(item.source, MediaSource::Virtual | MediaSource::External))
            .unwrap_or(false)
    }

    pub(super) fn is_external_path(&self, path: &Path) -> bool {
        self.item_for_path(path)
            .map(|item| item.source == MediaSource::External)
            .unwrap_or(false)
    }

    pub(super) fn meta_for_path(&self, path: &Path) -> Option<&FileMeta> {
        self.item_for_path(path).and_then(|item| item.meta.as_ref())
    }

    pub(super) fn effective_sample_rate_for_path(&self, path: &Path) -> Option<u32> {
        self.sample_rate_override
            .get(path)
            .copied()
            .or_else(|| self.meta_for_path(path).map(|m| m.sample_rate))
            .filter(|v| *v > 0)
    }

    pub(super) fn effective_bits_for_path(&self, path: &Path) -> Option<u16> {
        self.bit_depth_override
            .get(path)
            .copied()
            .map(|v| v.bits_per_sample())
            .or_else(|| self.meta_for_path(path).map(|m| m.bits_per_sample))
            .filter(|v| *v > 0)
    }

    pub(super) fn effective_bits_label_for_path(&self, path: &Path) -> Option<String> {
        let override_depth = self.bit_depth_override.get(path).copied();
        let bits = override_depth
            .map(|v| v.bits_per_sample())
            .or_else(|| self.meta_for_path(path).map(|m| m.bits_per_sample))
            .filter(|v| *v > 0)?;
        let kind = if let Some(depth) = override_depth {
            match depth {
                crate::wave::WavBitDepth::Float32 => SampleValueKind::Float,
                crate::wave::WavBitDepth::Pcm16 | crate::wave::WavBitDepth::Pcm24 => {
                    SampleValueKind::Int
                }
            }
        } else {
            self.meta_for_path(path)
                .map(|m| m.sample_value_kind)
                .unwrap_or(SampleValueKind::Unknown)
        };
        if bits == 32 {
            let label = match kind {
                SampleValueKind::Float => "32f",
                SampleValueKind::Int => "32i",
                SampleValueKind::Unknown => "32",
            };
            return Some(label.to_string());
        }
        Some(bits.to_string())
    }

    pub(super) fn effective_format_override_for_path(&self, path: &Path) -> Option<&str> {
        self.format_override.get(path).map(|v| v.as_str())
    }

    pub(super) fn display_name_for_path_with_format_override(
        path: &Path,
        format_override: Option<&str>,
    ) -> String {
        let mut base = PathBuf::from(Self::display_name_for_path(path));
        if let Some(ext) = format_override {
            let ext = ext.trim().trim_start_matches('.');
            if !ext.is_empty() {
                base.set_extension(ext);
            }
        }
        base.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string()
    }

    pub(super) fn refresh_display_name_for_path(&mut self, path: &Path) {
        let format_override = self.effective_format_override_for_path(path);
        let display = Self::display_name_for_path_with_format_override(path, format_override);
        if let Some(item) = self.item_for_path_mut(path) {
            item.display_name = display.clone();
        }
        for tab in self.tabs.iter_mut() {
            if tab.path.as_path() == path {
                tab.display_name = display.clone();
            }
        }
    }

    pub(super) fn set_meta_for_path(&mut self, path: &Path, meta: FileMeta) -> bool {
        let bpm_hint = meta.bpm.filter(|v| v.is_finite() && *v > 0.0);
        let sr_hint = (meta.sample_rate > 0).then_some(meta.sample_rate);
        if let Some(item) = self.item_for_path_mut(path) {
            item.meta = Some(meta);
            if let Some(sr) = sr_hint {
                self.sample_rate_probe_cache.insert(path.to_path_buf(), sr);
            }
            if let Some(bpm) = bpm_hint {
                for tab in self.tabs.iter_mut() {
                    if tab.path == path && !tab.bpm_user_set {
                        tab.bpm_value = bpm;
                    }
                }
            }
            return true;
        }
        false
    }

    pub(super) fn clear_meta_for_path(&mut self, path: &Path) {
        if let Some(item) = self.item_for_path_mut(path) {
            item.meta = None;
        }
        self.sample_rate_probe_cache.remove(path);
    }

    pub(super) fn transcript_for_path(&self, path: &Path) -> Option<&Transcript> {
        self.item_for_path(path)
            .and_then(|item| item.transcript.as_ref())
    }

    pub(super) fn set_transcript_for_path(
        &mut self,
        path: &Path,
        transcript: Option<Transcript>,
    ) -> bool {
        if let Some(item) = self.item_for_path_mut(path) {
            item.transcript = transcript;
            if item.transcript.is_some() && !self.list_columns.transcript {
                self.list_columns.transcript = true;
            }
            return true;
        }
        false
    }

    pub(super) fn clear_transcript_for_path(&mut self, path: &Path) {
        if let Some(item) = self.item_for_path_mut(path) {
            item.transcript = None;
        }
    }

    pub(super) fn display_name_for_path(path: &Path) -> String {
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string()
    }

    pub(super) fn display_folder_for_path(path: &Path) -> String {
        path.parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_string()
    }

    pub(super) fn rebuild_item_indexes(&mut self) {
        self.item_index.clear();
        self.path_index.clear();
        for (idx, item) in self.items.iter().enumerate() {
            self.item_index.insert(item.id, idx);
            self.path_index.insert(item.path.clone(), item.id);
        }
    }

    pub(super) fn path_for_row(&self, row_idx: usize) -> Option<&PathBuf> {
        let id = *self.files.get(row_idx)?;
        self.item_for_id(id).map(|item| &item.path)
    }

    pub(super) fn row_for_path(&self, path: &Path) -> Option<usize> {
        let id = *self.path_index.get(path)?;
        self.files.iter().position(|&i| i == id)
    }

    pub(super) fn selected_path_buf(&self) -> Option<PathBuf> {
        self.selected.and_then(|i| self.path_for_row(i).cloned())
    }

    pub(super) fn selected_paths(&self) -> Vec<PathBuf> {
        let mut rows: Vec<usize> = self.selected_multi.iter().copied().collect();
        if rows.is_empty() {
            if let Some(sel) = self.selected {
                rows.push(sel);
            } else if let Some(idx) = self.active_tab {
                if let Some(tab) = self.tabs.get(idx) {
                    return vec![tab.path.clone()];
                }
            }
        }
        rows.sort_unstable();
        rows.into_iter()
            .filter_map(|row| self.path_for_row(row).cloned())
            .collect()
    }

    pub(super) fn selected_real_paths(&self) -> Vec<PathBuf> {
        self.selected_paths()
            .into_iter()
            .filter(|p| !self.is_virtual_path(p))
            .collect()
    }

    pub(super) fn selected_renameable_paths(&self) -> Vec<PathBuf> {
        self.selected_paths()
            .into_iter()
            .filter(|p| !self.is_external_path(p))
            .collect()
    }

    pub(super) fn selected_item_ids(&self) -> Vec<MediaId> {
        let mut rows: Vec<usize> = self.selected_multi.iter().copied().collect();
        if rows.is_empty() {
            if let Some(sel) = self.selected {
                rows.push(sel);
            } else if let Some(idx) = self.active_tab {
                if let Some(tab) = self.tabs.get(idx) {
                    if let Some(id) = self.path_index.get(&tab.path) {
                        return vec![*id];
                    }
                }
            }
        }
        rows.sort_unstable();
        rows.into_iter()
            .filter_map(|row| self.files.get(row).copied())
            .collect()
    }

    pub(super) fn ensure_sort_key_visible(&mut self) {
        let cols = self.list_columns;
        let external_visible = cols.external && !self.external_visible_columns.is_empty();
        let key_visible = match self.sort_key {
            SortKey::File => cols.file,
            SortKey::Folder => cols.folder,
            SortKey::Transcript => cols.transcript,
            SortKey::Length => cols.length,
            SortKey::Channels => cols.channels,
            SortKey::SampleRate => cols.sample_rate,
            SortKey::Bits => cols.bits,
            SortKey::BitRate => cols.bit_rate,
            SortKey::Level => cols.peak,
            SortKey::Lufs => cols.lufs,
            SortKey::Bpm => cols.bpm,
            SortKey::CreatedAt => cols.created_at,
            SortKey::ModifiedAt => cols.modified_at,
            SortKey::External(idx) => external_visible && idx < self.external_visible_columns.len(),
        };
        if key_visible {
            return;
        }
        let fallback = if cols.file {
            SortKey::File
        } else if cols.folder {
            SortKey::Folder
        } else if cols.transcript {
            SortKey::Transcript
        } else if external_visible {
            SortKey::External(0)
        } else if cols.length {
            SortKey::Length
        } else if cols.channels {
            SortKey::Channels
        } else if cols.sample_rate {
            SortKey::SampleRate
        } else if cols.bits {
            SortKey::Bits
        } else if cols.bit_rate {
            SortKey::BitRate
        } else if cols.peak {
            SortKey::Level
        } else if cols.lufs {
            SortKey::Lufs
        } else if cols.bpm {
            SortKey::Bpm
        } else if cols.created_at {
            SortKey::CreatedAt
        } else if cols.modified_at {
            SortKey::ModifiedAt
        } else {
            SortKey::File
        };
        self.sort_key = fallback;
        self.sort_dir = SortDir::None;
    }

    pub(super) fn request_list_autoplay(&mut self) {
        if !self.auto_play_list_nav {
            if let Some(state) = &mut self.processing {
                state.autoplay_when_ready = false;
            }
            return;
        }
        let Some(path) = self.selected_path_buf() else {
            if let Some(state) = &mut self.processing {
                state.autoplay_when_ready = false;
            }
            return;
        };
        if let Some(state) = &mut self.processing {
            if state.path == path {
                // Defer playback until heavy processing (pitch/time) finishes.
                state.autoplay_when_ready = true;
                return;
            }
            state.autoplay_when_ready = false;
        }
        if self.list_preview_rx.is_some() {
            // Keep autoplay intent while async preview is still loading.
            self.list_play_pending = true;
            self.debug.autoplay_pending_count = self.debug.autoplay_pending_count.saturating_add(1);
            return;
        }
        if self.list_preview_pending_path.is_some() {
            self.list_play_pending = true;
            self.debug.autoplay_pending_count = self.debug.autoplay_pending_count.saturating_add(1);
            return;
        }
        self.audio.play();
        self.debug_mark_list_play_start(&path);
    }

    pub(super) fn current_active_path(&self) -> Option<&PathBuf> {
        if let Some(i) = self.active_tab {
            return self.tabs.get(i).map(|t| &t.path);
        }
        if let Some(i) = self.selected {
            return self.path_for_row(i);
        }
        None
    }
}
