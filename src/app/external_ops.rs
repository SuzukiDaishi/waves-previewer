use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{ExternalKeyRule, ExternalSource, MediaItem, MediaSource, WavesPreviewer};

#[derive(Clone, Debug)]
pub(crate) enum ExternalLoadTarget {
    New,
    Reload(usize),
}

pub(super) struct ExternalMappingState {
    item_cursor: usize,
    row_cursor: usize,
    rule: ExternalKeyRule,
    input: crate::app::types::ExternalRegexInput,
    regex: Option<regex::Regex>,
    replace: String,
    scope_regex: Option<regex::Regex>,
    matched: usize,
    unmatched: usize,
    matched_keys: std::collections::HashSet<String>,
    unmatched_rows: Vec<usize>,
}

pub(super) struct ExternalMergedData {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    key_index: Option<usize>,
    visible_columns: Vec<String>,
    lookup: HashMap<String, std::sync::Arc<HashMap<String, String>>>,
    key_row_index: HashMap<String, usize>,
}

impl WavesPreviewer {
    fn external_unmatched_path_for_row(&self, row_idx: usize) -> PathBuf {
        let key_idx = self.external_key_index.unwrap_or(0);
        let key = self
            .external_rows
            .get(row_idx)
            .and_then(|row| row.get(key_idx))
            .map(|v| v.trim())
            .unwrap_or("");
        if key.is_empty() {
            PathBuf::from(format!("external://row/{}", row_idx))
        } else {
            PathBuf::from(format!("external://row/{}", key))
        }
    }

    fn clear_external_unmatched_items(&mut self) {
        let mut paths: Vec<PathBuf> = self
            .items
            .iter()
            .filter(|item| item.source == MediaSource::External)
            .map(|item| item.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        paths.sort();
        paths.dedup();
        self.remove_paths_from_list(&paths);
    }

    pub(super) fn refresh_external_unmatched_items(&mut self) {
        self.clear_external_unmatched_items();
        if !self.external_show_unmatched {
            return;
        }
        let Some(key_idx) = self.external_key_index else {
            return;
        };
        let mut added_any = false;
        for &row_idx in &self.external_unmatched_rows {
            let Some(row) = self.external_rows.get(row_idx) else {
                continue;
            };
            let key = row.get(key_idx).map(|v| v.trim()).unwrap_or("").to_string();
            if key.is_empty() {
                continue;
            }
            let mut item = MediaItem {
                id: self.next_media_id,
                audio_asset: crate::audio_asset::AudioAssetDescriptor::external_unprobed(
                    self.external_unmatched_path_for_row(row_idx),
                ),
                path: self.external_unmatched_path_for_row(row_idx),
                display_name: key.clone(),
                display_folder: std::sync::Arc::from("(external)"),
                source: MediaSource::External,
                meta: None,
                pending_gain_db: 0.0,
                note: String::new(),
                editor_notes: Vec::new(),
                status: crate::app::types::MediaStatus::Ok,
                status_id: self.default_status.clone(),
                tags: None,
                transcript: None,
                transcript_document: None,
                transcript_language: None,
                external: None,
                virtual_audio: None,
                virtual_state: None,
            };
            self.next_media_id = self.next_media_id.wrapping_add(1);
            let mut external = HashMap::new();
            for (idx, header) in self.external_headers.iter().enumerate() {
                if let Some(val) = row.get(idx) {
                    let trimmed = val.trim();
                    if !trimmed.is_empty() {
                        external.insert(header.clone(), trimmed.to_string());
                    }
                }
            }
            item.set_external(external);
            self.items.push(item);
            added_any = true;
        }
        if added_any {
            self.rebuild_item_indexes();
            self.refresh_filter_then_sort();
        }
    }

    pub(super) fn fill_external_for_item(&self, item: &mut MediaItem) {
        if item.source == MediaSource::File {
            if let Some(row) = self.external_row_shared_for_path(&item.path) {
                item.set_external_shared(row);
            } else {
                item.clear_external();
            }
        } else {
            item.clear_external();
        }
    }

    pub(super) fn external_row_for_path(&self, path: &Path) -> Option<HashMap<String, String>> {
        self.external_row_shared_for_path(path)
            .map(|row| row.as_ref().clone())
    }

    fn external_row_shared_for_path(
        &self,
        path: &Path,
    ) -> Option<std::sync::Arc<HashMap<String, String>>> {
        if self.external_lookup.is_empty() {
            return None;
        }
        for key in self.external_keys_for_path(path) {
            if let Some(row) = self.external_lookup.get(&key) {
                return Some(std::sync::Arc::clone(row));
            }
        }
        None
    }

    fn external_keys_for_path(&self, path: &Path) -> Vec<String> {
        let pat = self.external_match_regex.trim();
        let re = if pat.is_empty() {
            None
        } else {
            regex::Regex::new(pat).ok()
        };
        Self::external_keys_for_path_with_rule(
            path,
            self.external_key_rule,
            self.external_match_input,
            re.as_ref(),
            &self.external_match_replace,
        )
    }

    fn external_keys_for_path_with_rule(
        path: &Path,
        rule: ExternalKeyRule,
        input: crate::app::types::ExternalRegexInput,
        re: Option<&regex::Regex>,
        replace: &str,
    ) -> Vec<String> {
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let full_path = path.to_string_lossy().to_string().to_ascii_lowercase();
        let dir = path
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match rule {
            ExternalKeyRule::FileName => {
                if file_name.is_empty() {
                    Vec::new()
                } else {
                    vec![file_name]
                }
            }
            ExternalKeyRule::Stem => {
                if stem.is_empty() {
                    Vec::new()
                } else {
                    vec![stem]
                }
            }
            ExternalKeyRule::Regex => {
                if let Some(re) = re {
                    let subject = match input {
                        crate::app::types::ExternalRegexInput::FileName => &file_name,
                        crate::app::types::ExternalRegexInput::Stem => &stem,
                        crate::app::types::ExternalRegexInput::Path => &full_path,
                        crate::app::types::ExternalRegexInput::Dir => &dir,
                    };
                    let replaced = re
                        .replace_all(subject, replace)
                        .to_string()
                        .to_ascii_lowercase();
                    if replaced.is_empty() {
                        Vec::new()
                    } else {
                        vec![replaced]
                    }
                } else if stem.is_empty() {
                    Vec::new()
                } else {
                    vec![stem]
                }
            }
        }
    }

    pub(super) fn apply_external_mapping(&mut self) {
        if self.external_merge_rx.is_some() {
            self.external_mapping_state = None;
            return;
        }
        self.external_match_count = 0;
        self.external_unmatched_count = 0;
        self.external_unmatched_rows.clear();
        let pat = self.external_match_regex.trim().to_string();
        let scope_pat = self.external_scope_regex.trim().to_string();
        self.external_mapping_state = Some(ExternalMappingState {
            item_cursor: 0,
            row_cursor: 0,
            rule: self.external_key_rule,
            input: self.external_match_input,
            regex: (!pat.is_empty())
                .then(|| regex::Regex::new(&pat).ok())
                .flatten(),
            replace: self.external_match_replace.clone(),
            scope_regex: (!scope_pat.is_empty())
                .then(|| regex::Regex::new(&scope_pat).ok())
                .flatten(),
            matched: 0,
            unmatched: 0,
            matched_keys: std::collections::HashSet::new(),
            unmatched_rows: Vec::new(),
        });
        if self.headless || self.items.len() <= self.list_sync_threshold() {
            while self.pump_external_mapping() {}
        }
    }

    /// Apply external rows in deadline-sized slices. Returns true while work
    /// remains, so the frame loop keeps repainting without monopolizing input.
    pub(super) fn pump_external_mapping(&mut self) -> bool {
        let Some(mut state) = self.external_mapping_state.take() else {
            return false;
        };
        let started = std::time::Instant::now();
        let budget = if self.headless {
            std::time::Duration::MAX
        } else {
            std::time::Duration::from_secs_f64(self.perf.list_job_frame_budget_ms() / 1_000.0)
        };
        let has_lookup = !self.external_sources.is_empty() && !self.external_lookup.is_empty();

        while state.item_cursor < self.items.len() {
            if !self.headless
                && (started.elapsed() >= budget || !self.frame_budget.should_continue())
            {
                self.external_mapping_state = Some(state);
                return true;
            }
            let index = state.item_cursor;
            state.item_cursor += 1;
            let Some(item) = self.items.get(index) else {
                continue;
            };
            if item.source == MediaSource::External {
                continue;
            }
            let path = item.path.clone();
            let in_scope = state
                .scope_regex
                .as_ref()
                .is_none_or(|scope| scope.is_match(&path.to_string_lossy()));
            let mut found = None;
            if has_lookup && in_scope {
                for key in Self::external_keys_for_path_with_rule(
                    &path,
                    state.rule,
                    state.input,
                    state.regex.as_ref(),
                    &state.replace,
                ) {
                    if let Some(row) = self.external_lookup.get(&key) {
                        found = Some(std::sync::Arc::clone(row));
                        state.matched_keys.insert(key);
                        break;
                    }
                }
            }
            if let Some(item) = self.items.get_mut(index) {
                if let Some(row) = found {
                    item.set_external_shared(row);
                    state.matched = state.matched.saturating_add(1);
                } else {
                    item.clear_external();
                    state.unmatched = state.unmatched.saturating_add(1);
                }
            }
        }

        if let Some(key_idx) = self.external_key_index {
            while state.row_cursor < self.external_rows.len() {
                if !self.headless
                    && (started.elapsed() >= budget || !self.frame_budget.should_continue())
                {
                    self.external_mapping_state = Some(state);
                    return true;
                }
                let row_idx = state.row_cursor;
                state.row_cursor += 1;
                let Some(row) = self.external_rows.get(row_idx) else {
                    continue;
                };
                let key_raw = row.get(key_idx).map(|value| value.trim()).unwrap_or("");
                if key_raw.is_empty() {
                    continue;
                }
                let key = key_raw.to_ascii_lowercase();
                let mapped_idx = self.external_key_row_index.get(&key).copied();
                if !state.matched_keys.contains(&key) || mapped_idx != Some(row_idx) {
                    state.unmatched_rows.push(row_idx);
                }
            }
        }

        self.external_match_count = state.matched;
        self.external_unmatched_count = state.unmatched;
        self.external_unmatched_rows = state.unmatched_rows;
        self.refresh_external_unmatched_items();
        self.refresh_filter_then_sort();
        false
    }

    pub(super) fn default_external_columns(headers: &[String], key_idx: usize) -> Vec<String> {
        headers
            .iter()
            .enumerate()
            .filter(|(idx, _)| *idx != key_idx)
            .take(3)
            .map(|(_, h)| h.clone())
            .collect()
    }

    pub(super) fn apply_external_table(
        &mut self,
        path: PathBuf,
        table: super::external::ExternalTable,
    ) -> std::result::Result<(), String> {
        if table.headers.is_empty() {
            return Err("No headers found in data source.".to_string());
        }
        let source = ExternalSource {
            path: path.clone(),
            headers: table.headers,
            rows: table.rows,
            sheet_names: table.sheet_names,
            sheet_name: table.sheet_name,
            has_header: self.external_has_header,
            header_row: self.external_header_row,
            data_row: self.external_data_row,
        };
        match self.external_load_target.take() {
            Some(ExternalLoadTarget::Reload(idx)) => {
                if idx < self.external_sources.len() {
                    self.external_sources[idx] = std::sync::Arc::new(source);
                    self.external_active_source = Some(idx);
                } else {
                    self.external_sources.push(std::sync::Arc::new(source));
                    self.external_active_source = Some(self.external_sources.len() - 1);
                }
            }
            _ => {
                self.external_sources.push(std::sync::Arc::new(source));
                self.external_active_source = Some(self.external_sources.len() - 1);
            }
        }
        self.sync_active_external_source();
        self.external_settings_dirty = false;
        // During session restore several sources arrive one by one. Building
        // the combined index after every source wastes work and the final
        // synchronous rebuild used to freeze the UI. The restore finalizer
        // starts one worker after the last source arrives.
        if self.pending_external_restore.is_none() {
            self.rebuild_external_merged();
            self.apply_external_mapping();
            self.refresh_filter_then_sort();
        }
        Ok(())
    }

    pub(super) fn clear_external_data(&mut self) {
        self.external_mapping_state = None;
        self.external_merge_generation = self.external_merge_generation.wrapping_add(1);
        self.external_merge_rx = None;
        self.external_sources.clear();
        self.external_active_source = None;
        self.external_source = None;
        self.external_headers.clear();
        self.external_rows.clear();
        self.external_key_index = None;
        self.external_visible_columns.clear();
        self.external_lookup.clear();
        self.external_key_row_index.clear();
        self.external_match_count = 0;
        self.external_unmatched_count = 0;
        self.external_load_error = None;
        self.external_unmatched_rows.clear();
        self.external_sheet_names.clear();
        self.external_sheet_selected = None;
        self.external_settings_dirty = false;
        self.external_load_target = None;
        self.external_load_queue.clear();
        self.pending_external_restore = None;
        self.clear_external_unmatched_items();
        for item in &mut self.items {
            item.clear_external();
        }
        self.refresh_filter_then_sort();
    }

    pub(super) fn sync_active_external_source(&mut self) {
        let Some(idx) = self.external_active_source else {
            self.external_source = None;
            self.external_sheet_names.clear();
            self.external_sheet_selected = None;
            return;
        };
        let Some(source) = self.external_sources.get(idx) else {
            self.external_source = None;
            self.external_sheet_names.clear();
            self.external_sheet_selected = None;
            return;
        };
        self.external_source = Some(source.path.clone());
        self.external_sheet_names = source.sheet_names.clone();
        self.external_sheet_selected = source.sheet_name.clone();
        self.external_has_header = source.has_header;
        self.external_header_row = source.header_row;
        self.external_data_row = source.data_row;
    }

    fn build_external_merged(
        sources: &[std::sync::Arc<ExternalSource>],
        requested_key_index: Option<usize>,
        requested_key_name: Option<String>,
        mut visible_columns: Vec<String>,
    ) -> ExternalMergedData {
        let mut external_headers = Vec::new();
        let mut external_rows = Vec::new();
        let mut header_map: HashMap<String, usize> = HashMap::new();
        for source in sources {
            for header in &source.headers {
                if !header_map.contains_key(header) {
                    let idx = external_headers.len();
                    external_headers.push(header.clone());
                    header_map.insert(header.clone(), idx);
                }
            }
        }
        if external_headers.is_empty() {
            return ExternalMergedData {
                headers: Vec::new(),
                rows: Vec::new(),
                key_index: None,
                visible_columns: Vec::new(),
                lookup: HashMap::new(),
                key_row_index: HashMap::new(),
            };
        }
        let requested_key_position = requested_key_name
            .as_ref()
            .and_then(|name| external_headers.iter().position(|header| header == name));
        let key_idx = requested_key_position
            .or(requested_key_index.filter(|&idx| idx < external_headers.len()))
            .unwrap_or(0);
        let key_name = external_headers[key_idx].clone();
        let mut key_to_row: HashMap<String, usize> = HashMap::new();
        for source in sources {
            let Some(src_key_idx) = source.headers.iter().position(|h| h == &key_name) else {
                continue;
            };
            for row in &source.rows {
                let key_raw = row.get(src_key_idx).map(|v| v.trim()).unwrap_or("");
                if key_raw.is_empty() {
                    continue;
                }
                let key = key_raw.to_ascii_lowercase();
                let row_idx = if let Some(&idx) = key_to_row.get(&key) {
                    idx
                } else {
                    let idx = external_rows.len();
                    external_rows.push(vec![String::new(); external_headers.len()]);
                    key_to_row.insert(key.clone(), idx);
                    external_rows[idx][key_idx] = key_raw.to_string();
                    idx
                };
                for (col_idx, header) in source.headers.iter().enumerate() {
                    let Some(&dst_idx) = header_map.get(header) else {
                        continue;
                    };
                    if let Some(val) = row.get(col_idx) {
                        let trimmed = val.trim();
                        if !trimmed.is_empty() {
                            external_rows[row_idx][dst_idx] = trimmed.to_string();
                        }
                    }
                }
            }
        }
        if visible_columns.is_empty() {
            visible_columns = Self::default_external_columns(&external_headers, key_idx);
        } else {
            visible_columns.retain(|c| header_map.contains_key(c) && c != &key_name);
            if visible_columns.is_empty() {
                visible_columns = Self::default_external_columns(&external_headers, key_idx);
            }
        }
        let mut lookup = HashMap::new();
        let mut key_row_index = HashMap::new();
        for (row_idx, row) in external_rows.iter().enumerate() {
            let key = row
                .get(key_idx)
                .map(|value| value.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if key.is_empty() {
                continue;
            }
            let mut map = HashMap::new();
            for (index, header) in external_headers.iter().enumerate() {
                if let Some(value) = row.get(index) {
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        map.insert(header.clone(), trimmed.to_string());
                    }
                }
            }
            lookup.insert(key.clone(), std::sync::Arc::new(map));
            key_row_index.insert(key, row_idx);
        }
        ExternalMergedData {
            headers: external_headers,
            rows: external_rows,
            key_index: Some(key_idx),
            visible_columns,
            lookup,
            key_row_index,
        }
    }

    fn install_external_merged(&mut self, merged: ExternalMergedData) {
        self.external_headers = merged.headers;
        self.external_rows = merged.rows;
        self.external_key_index = merged.key_index;
        self.external_visible_columns = merged.visible_columns;
        self.external_lookup = merged.lookup;
        self.external_key_row_index = merged.key_row_index;
    }

    pub(super) fn rebuild_external_merged(&mut self) {
        let requested_key_name = self
            .external_key_index
            .and_then(|index| self.external_headers.get(index))
            .cloned();
        self.rebuild_external_merged_with_preferences(
            self.external_key_index,
            requested_key_name,
            self.external_visible_columns.clone(),
        );
    }

    pub(super) fn rebuild_external_merged_with_preferences(
        &mut self,
        requested_key_index: Option<usize>,
        requested_key_name: Option<String>,
        visible_columns: Vec<String>,
    ) {
        let rows = self
            .external_sources
            .iter()
            .map(|source| source.rows.len())
            .sum::<usize>();
        if self.headless || rows <= self.perf.list_sync_threshold() {
            self.external_merge_rx = None;
            let merged = Self::build_external_merged(
                &self.external_sources,
                requested_key_index,
                requested_key_name,
                visible_columns,
            );
            self.install_external_merged(merged);
            return;
        }
        self.external_merge_generation = self.external_merge_generation.wrapping_add(1);
        let generation = self.external_merge_generation;
        let sources = self.external_sources.clone();
        let worker_requested_key_name = requested_key_name.clone();
        let worker_visible_columns = visible_columns.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("neowaves-external-index".to_string())
            .spawn(move || {
                crate::app::threading::lower_current_thread_priority();
                let merged = Self::build_external_merged(
                    &sources,
                    requested_key_index,
                    worker_requested_key_name,
                    worker_visible_columns,
                );
                let _ = tx.send((generation, merged));
                crate::ui_wake::wake_ui();
            });
        if spawned.is_ok() {
            self.external_merge_rx = Some(rx);
            self.external_mapping_state = None;
            self.external_lookup.clear();
            self.external_key_row_index.clear();
        } else {
            let merged = Self::build_external_merged(
                &self.external_sources,
                requested_key_index,
                requested_key_name,
                visible_columns,
            );
            self.install_external_merged(merged);
        }
    }

    pub(super) fn drain_external_merge_results(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.external_merge_rx.as_ref() else {
            return;
        };
        match super::loading_ops::poll_job(rx) {
            super::loading_ops::JobPoll::Waiting => {}
            super::loading_ops::JobPoll::Ready((generation, merged)) => {
                self.external_merge_rx = None;
                if generation == self.external_merge_generation {
                    self.install_external_merged(merged);
                    self.apply_external_mapping();
                    ctx.request_repaint();
                }
            }
            super::loading_ops::JobPoll::Gone => {
                self.external_merge_rx = None;
                self.append_external_load_error(
                    "External index worker ended before publishing a result".to_string(),
                );
            }
        }
    }
}
