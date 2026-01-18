use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{ExternalKeyRule, MediaItem, MediaSource, WavesPreviewer};

impl WavesPreviewer {
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
            re.as_ref(),
            &self.external_match_replace,
        )
    }

    fn external_keys_for_path_with_rule(
        path: &Path,
        rule: ExternalKeyRule,
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
                    let replaced = re
                        .replace_all(&stem, replace)
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
        let Some(key_idx) = self.external_key_index else {
            return;
        };
        for row in &self.external_rows {
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
            self.external_lookup.insert(key, map);
        }
    }

    pub(super) fn apply_external_mapping(&mut self) {
        self.external_match_count = 0;
        self.external_unmatched_count = 0;
        if self.external_source.is_none() {
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
        let lookup = self.external_lookup.clone();
        let rule = self.external_key_rule;
        let pat = self.external_match_regex.trim().to_string();
        let replace = self.external_match_replace.clone();
        let re = if pat.is_empty() {
            None
        } else {
            regex::Regex::new(&pat).ok()
        };
        for item in &mut self.items {
            let mut matched = false;
            let mut row = None;
            for key in
                Self::external_keys_for_path_with_rule(&item.path, rule, re.as_ref(), &replace)
            {
                if let Some(found) = lookup.get(&key) {
                    row = Some(found.clone());
                    break;
                }
            }
            if let Some(found) = row {
                item.external = found;
                matched = true;
            } else {
                item.external.clear();
            }
            if matched {
                self.external_match_count += 1;
            } else {
                self.external_unmatched_count += 1;
            }
        }
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

    pub(super) fn load_external_source(
        &mut self,
        path: PathBuf,
    ) -> std::result::Result<(), String> {
        let Some(table) = super::external::load_table(&path) else {
            return Err("Unsupported or empty data source.".to_string());
        };
        if table.headers.is_empty() {
            return Err("No headers found in data source.".to_string());
        }
        self.external_source = Some(path);
        self.external_headers = table.headers;
        self.external_rows = table.rows;
        let key_idx = self
            .external_key_index
            .filter(|&idx| idx < self.external_headers.len())
            .unwrap_or(0);
        self.external_key_index = Some(key_idx);
        self.external_visible_columns =
            Self::default_external_columns(&self.external_headers, key_idx);
        self.rebuild_external_lookup();
        self.apply_external_mapping();
        self.apply_filter_from_search();
        self.apply_sort();
        Ok(())
    }

    pub(super) fn clear_external_data(&mut self) {
        self.external_source = None;
        self.external_headers.clear();
        self.external_rows.clear();
        self.external_key_index = None;
        self.external_visible_columns.clear();
        self.external_lookup.clear();
        self.external_match_count = 0;
        self.external_unmatched_count = 0;
        self.external_load_error = None;
        for item in &mut self.items {
            item.external.clear();
        }
        self.apply_filter_from_search();
        self.apply_sort();
    }
}
