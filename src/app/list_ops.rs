use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::audio_io;
use walkdir::WalkDir;

/// What a merge did with the paths it was handed.
///
/// `missing` covers paths that are neither a file nor a directory — a stale
/// clipboard from a since-ejected volume, most often.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct FileMergeCounts {
    pub added: usize,
    pub duplicates: usize,
    pub unsupported: usize,
    pub missing: usize,
}

impl FileMergeCounts {
    /// How the result reads in a toast, or `None` when there is nothing worth
    /// interrupting the user for.
    pub fn summary(&self) -> Option<String> {
        let mut skipped: Vec<String> = Vec::new();
        if self.duplicates > 0 {
            skipped.push(format!("{} already in the list", self.duplicates));
        }
        if self.unsupported > 0 {
            skipped.push(format!("{} not audio", self.unsupported));
        }
        if self.missing > 0 {
            skipped.push(format!("{} not found", self.missing));
        }
        match (self.added, skipped.is_empty()) {
            (0, true) => None,
            (0, false) => Some(format!("Nothing to add: {}", skipped.join(", "))),
            (n, true) => Some(format!("Added {n} file(s)")),
            (n, false) => Some(format!(
                "Added {n} file(s) ({} skipped)",
                skipped.join(", ")
            )),
        }
    }
}

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

    pub(super) fn resolve_pending_list_load_target(
        &self,
        paths: &[PathBuf],
        kind: crate::app::types::PendingListLoadTargetKind,
        auto_scroll: bool,
    ) -> Option<crate::app::types::PendingListLoadTarget> {
        self.resolve_last_open_target_path(paths)
            .cloned()
            .map(|path| crate::app::types::PendingListLoadTarget {
                path,
                kind,
                auto_scroll,
            })
    }

    pub(super) fn select_loaded_target_path(&mut self, path: &Path, auto_scroll: bool) -> bool {
        let Some(row) = self.row_for_path(path) else {
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

    pub(super) fn open_loaded_target_in_editor(&mut self, path: &Path, auto_scroll: bool) -> bool {
        let Some(row) = self.row_for_path(path) else {
            return false;
        };
        self.selected = Some(row);
        self.scroll_to_selected = auto_scroll;
        self.selected_multi.clear();
        self.selected_multi.insert(row);
        self.select_anchor = Some(row);
        self.open_or_activate_tab(path);
        self.pending_editor_autoplay_path =
            if self.auto_play_list_nav && !self.editor_playback_handoff_matches(path) {
                Some(path.to_path_buf())
            } else {
                None
            };
        true
    }

    #[allow(dead_code)]
    pub(super) fn select_open_target_path(&mut self, paths: &[PathBuf], auto_scroll: bool) -> bool {
        let Some(target_path) = self.resolve_last_open_target_path(paths).cloned() else {
            return false;
        };
        self.select_loaded_target_path(&target_path, auto_scroll)
    }

    #[allow(dead_code)]
    pub(super) fn open_shell_target_in_editor(
        &mut self,
        paths: &[PathBuf],
        auto_scroll: bool,
    ) -> bool {
        let Some(target_path) = self.resolve_last_open_target_path(paths).cloned() else {
            return false;
        };
        self.open_loaded_target_in_editor(&target_path, auto_scroll)
    }

    // Merge helper: add a folder recursively (supported audio only)
    pub(super) fn add_folder_merge(&mut self, dir: &Path) -> usize {
        let mut added = 0usize;
        let skip_dotfiles = self.skip_dotfiles;
        for entry in WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                !Self::is_internal_temp_cache_path(e.path())
                    && (!skip_dotfiles || !Self::is_dotfile_path(e.path()))
            })
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
        self.add_files_merge_counted(paths).added
    }

    /// Same merge, but says what happened to the paths that did not make it.
    ///
    /// The plain `add_files_merge` returns only the number added, which leaves
    /// its callers unable to tell "nothing happened because they were already
    /// in the list" from "nothing happened because none of them were audio".
    /// Paste needs that distinction: with drag and drop the user can see what
    /// they dropped, but a clipboard is invisible until something appears.
    pub(super) fn add_files_merge_counted(&mut self, paths: &[PathBuf]) -> FileMergeCounts {
        let mut counts = FileMergeCounts::default();
        for p in paths {
            if p.is_file() {
                if self.should_skip_path(p) {
                    continue;
                }
                match p.extension().and_then(|s| s.to_str()) {
                    Some(ext) if audio_io::is_supported_extension(ext) => {
                        if self.path_index.contains_key(p) {
                            counts.duplicates += 1;
                            continue;
                        }
                        let item = self.make_media_item(p.clone());
                        let id = item.id;
                        self.path_index.insert(p.clone(), id);
                        self.item_index.insert(id, self.items.len());
                        self.items.push(item);
                        counts.added += 1;
                    }
                    _ => counts.unsupported += 1,
                }
            } else if p.is_dir() {
                // The folder walk reports only what it added; a paste of a
                // folder is rare enough not to warrant threading counts through
                // it as well.
                counts.added += self.add_folder_merge(p.as_path());
            } else {
                counts.missing += 1;
            }
        }
        counts
    }

    pub(super) fn after_add_refresh(&mut self) {
        if !self.external_sources.is_empty() {
            self.apply_external_mapping();
        }
        self.refresh_filter_then_sort();
        self.refresh_root_locality();
        self.ensure_meta_pool();
    }

    // Replace current list with explicit files (supported audio only). Root is cleared.
    pub(super) fn replace_with_files(&mut self, paths: &[PathBuf]) {
        self.root = None;
        self.note_files_membership_changed();
        self.drop_list_contents_in_background();
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
        self.clear_scan_state();
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
        self.refresh_root_locality();
        self.ensure_meta_pool();
    }
}
