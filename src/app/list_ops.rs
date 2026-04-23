use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::audio_io;
use walkdir::WalkDir;

impl super::WavesPreviewer {
    fn is_open_target_audio_path(&self, path: &Path) -> bool {
        if !path.is_file() || self.should_skip_path(path) {
            return false;
        }
        path.extension()
            .and_then(|s| s.to_str())
            .map(audio_io::is_supported_extension)
            .unwrap_or(false)
    }

    pub(super) fn resolve_last_open_target_path<'a>(
        &self,
        paths: &'a [PathBuf],
    ) -> Option<&'a PathBuf> {
        paths
            .iter()
            .rev()
            .find(|path| self.is_open_target_audio_path(path))
    }

    pub(super) fn select_open_target_path(&mut self, paths: &[PathBuf], auto_scroll: bool) -> bool {
        let Some(target_path) = self.resolve_last_open_target_path(paths).cloned() else {
            return false;
        };
        let Some(row) = self.row_for_path(&target_path) else {
            return false;
        };
        self.select_and_load(row, auto_scroll);
        self.selected_multi.clear();
        self.selected_multi.insert(row);
        self.select_anchor = Some(row);
        if self.auto_play_list_nav {
            self.request_list_autoplay();
        }
        true
    }

    pub(super) fn open_shell_target_in_editor(
        &mut self,
        paths: &[PathBuf],
        auto_scroll: bool,
    ) -> bool {
        let Some(target_path) = self.resolve_last_open_target_path(paths).cloned() else {
            return false;
        };
        let Some(row) = self.row_for_path(&target_path) else {
            return false;
        };
        self.selected = Some(row);
        self.scroll_to_selected = auto_scroll;
        self.selected_multi.clear();
        self.selected_multi.insert(row);
        self.select_anchor = Some(row);
        self.open_or_activate_tab(&target_path);
        self.pending_editor_autoplay_path = if self.auto_play_list_nav {
            Some(target_path)
        } else {
            None
        };
        true
    }

    // Merge helper: add a folder recursively (supported audio only)
    pub(super) fn add_folder_merge(&mut self, dir: &Path) -> usize {
        let mut added = 0usize;
        let skip_dotfiles = self.skip_dotfiles;
        for entry in WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !skip_dotfiles || !Self::is_dotfile_path(e.path()))
        {
            if let Ok(e) = entry {
                if e.file_type().is_file() {
                    let p = e.into_path();
                    if self.should_skip_path(&p) {
                        continue;
                    }
                    if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                        if audio_io::is_supported_extension(ext) {
                            if self.path_index.contains_key(&p) {
                                continue;
                            }
                            let item = self.make_media_item(p.clone());
                            let id = item.id;
                            self.path_index.insert(p.clone(), id);
                            self.item_index.insert(id, self.items.len());
                            self.items.push(item);
                            added += 1;
                        }
                    }
                }
            }
        }
        added
    }

    // Merge helper: add explicit files (supported audio only)
    pub(super) fn add_files_merge(&mut self, paths: &[PathBuf]) -> usize {
        let mut added = 0usize;
        for p in paths {
            if p.is_file() {
                if self.should_skip_path(p) {
                    continue;
                }
                if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    if audio_io::is_supported_extension(ext) {
                        if self.path_index.contains_key(p) {
                            continue;
                        }
                        let item = self.make_media_item(p.clone());
                        let id = item.id;
                        self.path_index.insert(p.clone(), id);
                        self.item_index.insert(id, self.items.len());
                        self.items.push(item);
                        added += 1;
                    }
                }
            } else if p.is_dir() {
                added += self.add_folder_merge(p.as_path());
            }
        }
        added
    }

    pub(super) fn after_add_refresh(&mut self) {
        if !self.external_sources.is_empty() {
            self.apply_external_mapping();
        }
        self.apply_filter_from_search();
        self.apply_sort();
        self.ensure_meta_pool();
    }

    // Replace current list with explicit files (supported audio only). Root is cleared.
    pub(super) fn replace_with_files(&mut self, paths: &[PathBuf]) {
        self.root = None;
        self.files.clear();
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.transcript_ai_inflight.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
        self.reset_all_feature_analysis_state();
        self.scan_rx = None;
        self.scan_in_progress = false;
        let mut set: HashSet<PathBuf> = HashSet::new();
        for p in paths {
            if p.is_file() {
                if self.should_skip_path(p) {
                    continue;
                }
                if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    if audio_io::is_supported_extension(ext) {
                        if set.insert(p.clone()) {
                            let item = self.make_media_item(p.clone());
                            let id = item.id;
                            self.path_index.insert(p.clone(), id);
                            self.item_index.insert(id, self.items.len());
                            self.items.push(item);
                        }
                    }
                }
            }
        }
        self.ensure_meta_pool();
    }
}
