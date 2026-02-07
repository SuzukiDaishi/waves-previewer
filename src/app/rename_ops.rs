use std::path::{Path, PathBuf};

use super::types::MediaSource;
use super::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn open_rename_dialog(&mut self, path: PathBuf) {
        self.rename_input = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        self.rename_target = Some(path);
        self.rename_error = None;
        self.show_rename_dialog = true;
    }

    pub(super) fn open_batch_rename_dialog(&mut self, paths: Vec<PathBuf>) {
        self.batch_rename_targets = paths;
        self.batch_rename_pattern = "{name}_{n}".into();
        self.batch_rename_start = 1;
        self.batch_rename_pad = 2;
        self.batch_rename_error = None;
        self.show_batch_rename_dialog = true;
    }

    pub(super) fn replace_path_in_state(&mut self, from: &Path, to: &Path) {
        let Some(id) = self.path_index.get(from).copied() else {
            return;
        };
        let new_path = to.to_path_buf();
        let external = self.external_row_for_path(&new_path);
        if let Some(item) = self.item_for_id_mut(id) {
            item.path = new_path.clone();
            item.display_name = Self::display_name_for_path(&new_path);
            item.display_folder = Self::display_folder_for_path(&new_path);
            item.source = MediaSource::File;
            item.virtual_audio = None;
            item.transcript = None;
            item.external = external.unwrap_or_default();
        }
        self.path_index.remove(from);
        self.path_index.insert(new_path.clone(), id);
        if let Some(v) = self.spectro_cache.remove(from) {
            self.spectro_cache.insert(new_path.clone(), v);
        }
        if let Some(v) = self.spectro_cache_sizes.remove(from) {
            self.spectro_cache_sizes.insert(new_path.clone(), v);
        }
        if let Some(pos) = self
            .spectro_cache_order
            .iter()
            .position(|p| p.as_path() == from)
        {
            self.spectro_cache_order[pos] = new_path.clone();
        }
        if let Some(v) = self.edited_cache.remove(from) {
            self.edited_cache.insert(new_path.clone(), v);
        }
        self.meta_inflight.remove(from);
        self.transcript_inflight.remove(from);
        self.spectro_inflight.remove(from);
        if let Some(v) = self.spectro_progress.remove(from) {
            self.spectro_progress.insert(new_path.clone(), v);
        }
        if let Some(v) = self.spectro_cancel.remove(from) {
            self.spectro_cancel.insert(new_path.clone(), v);
        }
        if let Some(v) = self.lufs_override.remove(from) {
            self.lufs_override.insert(new_path.clone(), v);
        }
        if let Some(v) = self.lufs_recalc_deadline.remove(from) {
            self.lufs_recalc_deadline.insert(new_path.clone(), v);
        }
        if let Some(v) = self.sample_rate_override.remove(from) {
            self.sample_rate_override.insert(new_path.clone(), v);
        }
        if let Some(v) = self.sample_rate_probe_cache.remove(from) {
            self.sample_rate_probe_cache.insert(new_path.clone(), v);
        }
        if let Some(v) = self.bit_depth_override.remove(from) {
            self.bit_depth_override.insert(new_path.clone(), v);
        }
        for p in self.saving_sources.iter_mut() {
            if p.as_path() == from {
                *p = new_path.clone();
            }
        }
        if self.pending_activate_path.as_ref().map(|p| p.as_path()) == Some(from) {
            self.pending_activate_path = Some(new_path.clone());
        }
        for tab in self.tabs.iter_mut() {
            if tab.path.as_path() == from {
                tab.path = new_path.clone();
                tab.display_name = new_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(invalid)")
                    .to_string();
            }
        }
        if self.playing_path.as_ref().map(|p| p.as_path()) == Some(from) {
            self.playing_path = Some(new_path);
        }
    }

    pub(super) fn rename_file_path(&mut self, from: &PathBuf, new_name: &str) -> Result<PathBuf, String> {
        let name = new_name.trim();
        if name.is_empty() {
            return Err("Name is empty.".to_string());
        }
        let mut name = name.to_string();
        let has_ext = std::path::Path::new(&name).extension().is_some();
        if !has_ext {
            if let Some(ext) = from.extension().and_then(|s| s.to_str()) {
                name.push('.');
                name.push_str(ext);
            } else {
                name.push_str(".wav");
            }
        }
        let Some(parent) = from.parent() else {
            return Err("Missing parent folder.".to_string());
        };
        let to = parent.join(name);
        if to == *from {
            return Ok(to);
        }
        if to.exists() {
            return Err("Target already exists.".to_string());
        }
        std::fs::rename(from, &to).map_err(|e| format!("Rename failed: {e}"))?;
        self.replace_path_in_state(from, &to);
        self.apply_filter_from_search();
        self.apply_sort();
        Ok(to)
    }

    pub(super) fn batch_rename_paths(&mut self) -> Result<(), String> {
        if self.batch_rename_targets.is_empty() {
            return Err("No files selected.".to_string());
        }
        let pattern = self.batch_rename_pattern.trim().to_string();
        if pattern.is_empty() {
            return Err("Pattern is empty.".to_string());
        }
        let targets = self.batch_rename_targets.clone();
        let src_set: std::collections::HashSet<PathBuf> = targets.iter().cloned().collect();
        let mut mappings: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (i, src) in targets.iter().enumerate() {
            if !src.is_file() {
                self.remove_missing_path(src);
                continue;
            }
            let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let num = self.batch_rename_start.saturating_add(i as u32);
            let num_str = if self.batch_rename_pad > 0 {
                format!("{:0width$}", num, width = self.batch_rename_pad as usize)
            } else {
                num.to_string()
            };
            let mut name = pattern.replace("{name}", stem).replace("{n}", &num_str);
            if name.contains('/') || name.contains('\\') {
                return Err("Pattern must be a file name (no path separators).".to_string());
            }
            if name.trim().is_empty() {
                return Err("Generated name is empty.".to_string());
            }
            let has_ext = std::path::Path::new(&name).extension().is_some();
            if !has_ext {
                if let Some(ext) = src.extension().and_then(|s| s.to_str()) {
                    name.push('.');
                    name.push_str(ext);
                } else {
                    name.push_str(".wav");
                }
            }
            let parent = src.parent().unwrap_or_else(|| std::path::Path::new("."));
            let dst = parent.join(name);
            mappings.push((src.clone(), dst));
        }
        let mut seen = std::collections::HashSet::new();
        for (src, dst) in &mappings {
            if src == dst {
                continue;
            }
            if !seen.insert(dst.clone()) {
                return Err("Duplicate target names.".to_string());
            }
            if dst.exists() && !src_set.contains(dst) {
                return Err(format!("Target already exists: {}", dst.display()));
            }
        }
        let needs_temp = mappings
            .iter()
            .any(|(src, dst)| src != dst && src_set.contains(dst));
        if needs_temp {
            let mut temps: Vec<(PathBuf, PathBuf)> = Vec::new();
            for (i, (src, dst)) in mappings.iter().enumerate() {
                if src == dst {
                    continue;
                }
                let parent = src.parent().unwrap_or_else(|| std::path::Path::new("."));
                let mut tmp = parent.join(format!("._wvp_tmp_rename_{:03}.tmp", i));
                let mut bump = 0;
                while tmp.exists() {
                    bump += 1;
                    tmp = parent.join(format!("._wvp_tmp_rename_{:03}_{bump}.tmp", i));
                }
                std::fs::rename(src, &tmp).map_err(|e| format!("Rename failed: {e}"))?;
                self.replace_path_in_state(src, &tmp);
                temps.push((tmp, dst.clone()));
            }
            for (tmp, dst) in temps {
                std::fs::rename(&tmp, &dst).map_err(|e| format!("Rename failed: {e}"))?;
                self.replace_path_in_state(&tmp, &dst);
            }
        } else {
            for (src, dst) in &mappings {
                if src == dst {
                    continue;
                }
                std::fs::rename(src, dst).map_err(|e| format!("Rename failed: {e}"))?;
                self.replace_path_in_state(src, dst);
            }
        }
        self.apply_filter_from_search();
        self.apply_sort();
        self.selected_multi.clear();
        for (_, dst) in mappings {
            if let Some(row) = self.row_for_path(&dst) {
                self.selected_multi.insert(row);
            }
        }
        self.selected = self.selected_multi.iter().next().copied();
        if let Some(sel) = self.selected {
            self.select_anchor = Some(sel);
        }
        Ok(())
    }
}
