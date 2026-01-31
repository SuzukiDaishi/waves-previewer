use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{ExternalKeyRule, ExternalSource, MediaItem, MediaSource, WavesPreviewer};

#[derive(Clone, Debug)]
pub(crate) enum ExternalLoadTarget {
    New,
    Reload(usize),
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
            let key = row
                .get(key_idx)
                .map(|v| v.trim())
                .unwrap_or("")
                .to_string();
            if key.is_empty() {
                continue;
            }
            let mut item = MediaItem {
                id: self.next_media_id,
                path: self.external_unmatched_path_for_row(row_idx),
                display_name: key.clone(),
                display_folder: "(external)".to_string(),
                source: MediaSource::External,
                meta: None,
                pending_gain_db: 0.0,
                status: crate::app::types::MediaStatus::Ok,
                transcript: None,
                external: HashMap::new(),
                virtual_audio: None,
            };
            self.next_media_id = self.next_media_id.wrapping_add(1);
            for (idx, header) in self.external_headers.iter().enumerate() {
                if let Some(val) = row.get(idx) {
                    let trimmed = val.trim();
                    if !trimmed.is_empty() {
                        item.external.insert(header.clone(), trimmed.to_string());
                    }
                }
            }
            self.items.push(item);
            added_any = true;
        }
        if added_any {
            self.rebuild_item_indexes();
            self.apply_filter_from_search();
            self.apply_sort();
        }
    }

    pub(super) fn fill_external_for_item(&self, item: &mut MediaItem) {
        if item.source == MediaSource::File {
            item.external = self.external_row_for_path(&item.path).unwrap_or_default();
        } else {
            item.external.clear();
        }
    }

    pub(super) fn external_row_for_path(&self, path: &Path) -> Option<HashMap<String, String>> {
        if self.external_lookup.is_empty() {
            return None;
        }
        for key in self.external_keys_for_path(path) {
            if let Some(row) = self.external_lookup.get(&key) {
                return Some(row.clone());
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

    pub(super) fn rebuild_external_lookup(&mut self) {
        self.external_lookup.clear();
        self.external_key_row_index.clear();
        let Some(key_idx) = self.external_key_index else {
            return;
        };
        for (row_idx, row) in self.external_rows.iter().enumerate() {
            let Some(key_raw) = row.get(key_idx) else {
                continue;
            };
            let key = key_raw.trim().to_ascii_lowercase();
            if key.is_empty() {
                continue;
            }
            let mut map = HashMap::new();
            for (idx, header) in self.external_headers.iter().enumerate() {
                if let Some(val) = row.get(idx) {
                    let trimmed = val.trim();
                    if !trimmed.is_empty() {
                        map.insert(header.clone(), trimmed.to_string());
                    }
                }
            }
            self.external_lookup.insert(key.clone(), map);
            self.external_key_row_index.insert(key, row_idx);
        }
    }

    pub(super) fn apply_external_mapping(&mut self) {
        self.external_match_count = 0;
        self.external_unmatched_count = 0;
        self.external_unmatched_rows.clear();
        if self.external_sources.is_empty() {
            for item in &mut self.items {
                item.external.clear();
            }
            return;
        }
        if self.external_lookup.is_empty() {
            for item in &mut self.items {
                item.external.clear();
            }
            self.external_unmatched_count = self.items.len();
            return;
        }
        let rule = self.external_key_rule;
        let pat = self.external_match_regex.trim().to_string();
        let replace = self.external_match_replace.clone();
        let scope_pat = self.external_scope_regex.trim().to_string();
        let re = if pat.is_empty() {
            None
        } else {
            regex::Regex::new(&pat).ok()
        };
        let scope_re = if scope_pat.is_empty() {
            None
        } else {
            regex::Regex::new(&scope_pat).ok()
        };
        let lookup = &self.external_lookup;
        let mut matched = 0usize;
        let mut unmatched = 0usize;
        let mut matched_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        for item in &mut self.items {
            if item.source == MediaSource::External {
                continue;
            }
            if let Some(scope) = &scope_re {
                let path_str = item.path.to_string_lossy().to_string();
                if !scope.is_match(&path_str) {
                    item.external.clear();
                    unmatched += 1;
                    continue;
                }
            }
            let mut hit = false;
            let mut row = None;
            for key in
                Self::external_keys_for_path_with_rule(
                    &item.path,
                    rule,
                    self.external_match_input,
                    re.as_ref(),
                    &replace,
                )
            {
                if let Some(found) = lookup.get(&key) {
                    row = Some(found.clone());
                    matched_keys.insert(key);
                    break;
                }
            }
            if let Some(found) = row {
                item.external = found;
                hit = true;
            } else {
                item.external.clear();
            }
            if hit {
                matched += 1;
            } else {
                unmatched += 1;
            }
        }
        self.external_match_count = matched;
        self.external_unmatched_count = unmatched;
        if let Some(key_idx) = self.external_key_index {
            for (row_idx, row) in self.external_rows.iter().enumerate() {
                let key_raw = row.get(key_idx).map(|v| v.trim()).unwrap_or("");
                if key_raw.is_empty() {
                    continue;
                }
                let key = key_raw.to_ascii_lowercase();
                let mapped_idx = self.external_key_row_index.get(&key).copied();
                if mapped_idx == Some(row_idx) && matched_keys.contains(&key) {
                    continue;
                }
                if !matched_keys.contains(&key) {
                    self.external_unmatched_rows.push(row_idx);
                } else if mapped_idx != Some(row_idx) {
                    self.external_unmatched_rows.push(row_idx);
                }
            }
        }
        self.refresh_external_unmatched_items();
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

    pub(super) fn begin_external_load(&mut self, path: PathBuf) {
        if self.external_load_inflight {
            return;
        }
        self.external_load_error = None;
        self.external_load_rows = 0;
        self.external_load_started_at = Some(std::time::Instant::now());
        self.external_load_path = Some(path.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        self.external_load_rx = Some(rx);
        self.external_load_inflight = true;
        let cfg = super::external::ExternalLoadConfig {
            path,
            sheet_name: self.external_sheet_selected.clone(),
            has_header: self.external_has_header,
            header_row: self.external_header_row,
            data_row: self.external_data_row,
        };
        super::external::spawn_load_table(cfg, tx);
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
                    self.external_sources[idx] = source;
                    self.external_active_source = Some(idx);
                } else {
                    self.external_sources.push(source);
                    self.external_active_source = Some(self.external_sources.len() - 1);
                }
            }
            _ => {
                self.external_sources.push(source);
                self.external_active_source = Some(self.external_sources.len() - 1);
            }
        }
        self.sync_active_external_source();
        self.rebuild_external_merged();
        self.external_settings_dirty = false;
        self.apply_external_mapping();
        self.apply_filter_from_search();
        self.apply_sort();
        Ok(())
    }

    pub(super) fn clear_external_data(&mut self) {
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
        self.clear_external_unmatched_items();
        for item in &mut self.items {
            item.external.clear();
        }
        self.apply_filter_from_search();
        self.apply_sort();
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

    pub(super) fn rebuild_external_merged(&mut self) {
        self.external_headers.clear();
        self.external_rows.clear();
        self.external_key_row_index.clear();
        self.external_lookup.clear();
        if self.external_sources.is_empty() {
            self.external_visible_columns.clear();
            return;
        }
        let mut header_map: HashMap<String, usize> = HashMap::new();
        for source in &self.external_sources {
            for header in &source.headers {
                if !header_map.contains_key(header) {
                    let idx = self.external_headers.len();
                    self.external_headers.push(header.clone());
                    header_map.insert(header.clone(), idx);
                }
            }
        }
        if self.external_headers.is_empty() {
            return;
        }
        let key_idx = self
            .external_key_index
            .filter(|&idx| idx < self.external_headers.len())
            .unwrap_or(0);
        self.external_key_index = Some(key_idx);
        let key_name = self.external_headers[key_idx].clone();
        let mut key_to_row: HashMap<String, usize> = HashMap::new();
        for source in &self.external_sources {
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
                    let idx = self.external_rows.len();
                    self.external_rows
                        .push(vec![String::new(); self.external_headers.len()]);
                    key_to_row.insert(key.clone(), idx);
                    self.external_rows[idx][key_idx] = key_raw.to_string();
                    idx
                };
                for (col_idx, header) in source.headers.iter().enumerate() {
                    let Some(&dst_idx) = header_map.get(header) else {
                        continue;
                    };
                    if let Some(val) = row.get(col_idx) {
                        let trimmed = val.trim();
                        if !trimmed.is_empty() {
                            self.external_rows[row_idx][dst_idx] = trimmed.to_string();
                        }
                    }
                }
            }
        }
        if self.external_visible_columns.is_empty() {
            self.external_visible_columns =
                Self::default_external_columns(&self.external_headers, key_idx);
        } else {
            self.external_visible_columns
                .retain(|c| header_map.contains_key(c));
            if self.external_visible_columns.is_empty() {
                self.external_visible_columns =
                    Self::default_external_columns(&self.external_headers, key_idx);
            }
        }
        self.rebuild_external_lookup();
    }
}
