use std::path::{Path, PathBuf};

use crate::app::sort_filter_jobs::OwnedKey;
use crate::app::types::{ColumnKey, MetadataListColumn};
use crate::metadata::cache::{MetadataColumnDefinition, SummaryPriority};
use crate::metadata::{Completion, MetadataSummary, MetadataValue, ScanBudget, SummaryRequest};

use super::WavesPreviewer;

#[derive(Clone, Debug)]
pub(super) struct MetadataCell {
    pub text: String,
    pub tooltip: String,
    pub partial: bool,
    pub conflict: bool,
}

impl WavesPreviewer {
    pub(super) fn sanitize_list_column_layout(&mut self) {
        use crate::app::types::ColumnId;
        let known_metadata = self
            .metadata_list_columns
            .iter()
            .map(|column| column.key.clone())
            .collect::<Vec<_>>();
        let mut seen = std::collections::HashSet::new();
        let mut layout = self
            .list_column_layout
            .iter()
            .filter(|key| match key {
                ColumnKey::Builtin(column) => ColumnId::ALL.contains(column),
                _ => known_metadata.contains(key),
            })
            .filter(|key| seen.insert((*key).clone()))
            .cloned()
            .collect::<Vec<_>>();
        for column in ColumnId::ALL
            .iter()
            .copied()
            .filter(|column| *column != ColumnId::Note)
        {
            let key = ColumnKey::Builtin(column);
            if seen.insert(key.clone()) {
                layout.push(key);
            }
        }
        for key in known_metadata {
            if seen.insert(key.clone()) {
                layout.push(key);
            }
        }
        let note = ColumnKey::Builtin(ColumnId::Note);
        if seen.insert(note.clone()) {
            layout.push(note);
        }
        let layout_changed = layout != self.list_column_layout;
        self.list_column_layout = layout;
        if layout_changed {
            self.list_table_layout_revision = self.list_table_layout_revision.wrapping_add(1);
        }
        self.list_column_order = self
            .list_column_layout
            .iter()
            .filter_map(|key| match key {
                ColumnKey::Builtin(column) => Some(*column),
                _ => None,
            })
            .collect();
    }

    pub(super) fn metadata_list_column_for_key(
        &self,
        key: &ColumnKey,
    ) -> Option<(usize, &MetadataListColumn)> {
        self.metadata_list_columns
            .iter()
            .enumerate()
            .find(|(_, column)| &column.key == key)
    }

    fn metadata_column_registry_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        Some(
            PathBuf::from(base)
                .join("NeoWaves")
                .join("metadata-columns.json"),
        )
    }

    pub(super) fn load_metadata_column_registry(&mut self) {
        let Some(path) = Self::metadata_column_registry_path() else {
            return;
        };
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let Ok(definitions) = serde_json::from_slice::<Vec<MetadataColumnDefinition>>(&bytes)
        else {
            return;
        };
        for definition in definitions {
            let Some(key) = ColumnKey::parse(&definition.key) else {
                continue;
            };
            if matches!(key, ColumnKey::Builtin(_))
                || self
                    .metadata_list_columns
                    .iter()
                    .any(|column| column.key == key)
            {
                continue;
            }
            let layout_key = key.clone();
            self.metadata_list_columns.push(MetadataListColumn {
                key,
                label: definition.label,
                visible: false,
                width: 150.0,
            });
            if !self.list_column_layout.contains(&layout_key) {
                let note = ColumnKey::Builtin(crate::app::types::ColumnId::Note);
                let insert_at = self
                    .list_column_layout
                    .iter()
                    .position(|entry| *entry == note)
                    .unwrap_or(self.list_column_layout.len());
                self.list_column_layout.insert(insert_at, layout_key);
            }
        }
    }

    pub(super) fn save_metadata_column_registry(&self) {
        let Some(path) = Self::metadata_column_registry_path() else {
            return;
        };
        let definitions = self
            .metadata_list_columns
            .iter()
            .map(|column| MetadataColumnDefinition {
                key: column.key.serialized_name(),
                label: column.label.clone(),
            })
            .collect::<Vec<_>>();
        let Ok(bytes) = serde_json::to_vec_pretty(&definitions) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let temporary = path.with_extension("json.part");
        if std::fs::write(&temporary, bytes).is_ok() {
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
            let _ = std::fs::rename(temporary, path);
        }
    }

    pub(super) fn add_metadata_list_column(&mut self, key: ColumnKey, label: impl Into<String>) {
        if matches!(key, ColumnKey::Builtin(_)) {
            return;
        }
        if let Some(column) = self
            .metadata_list_columns
            .iter_mut()
            .find(|column| column.key == key)
        {
            column.visible = true;
        } else {
            let layout_key = key.clone();
            self.metadata_list_columns.push(MetadataListColumn {
                key,
                label: label.into(),
                visible: true,
                width: 150.0,
            });
            let note_key = ColumnKey::Builtin(crate::app::types::ColumnId::Note);
            let insert_at = self
                .list_column_layout
                .iter()
                .position(|key| *key == note_key)
                .unwrap_or(self.list_column_layout.len());
            self.list_column_layout.insert(insert_at, layout_key);
        }
        self.sanitize_list_column_layout();
        self.save_metadata_column_registry();
        self.metadata_summary_generation = self.metadata_summary_generation.wrapping_add(1);
    }

    pub(super) fn visible_metadata_columns(
        &self,
    ) -> impl Iterator<Item = (usize, &MetadataListColumn)> {
        self.metadata_list_columns
            .iter()
            .enumerate()
            .filter(|(_, column)| column.visible)
    }

    fn metadata_summary_request(&self) -> Option<SummaryRequest> {
        let mut request = SummaryRequest::default();
        for (_, column) in self.visible_metadata_columns() {
            match &column.key {
                ColumnKey::Normalized(key) => {
                    if !request.fields.contains(key) {
                        request.fields.push(key.clone());
                    }
                }
                ColumnKey::Raw(_) => request.include_raw = true,
                ColumnKey::Builtin(_) => {}
            }
        }
        (!request.fields.is_empty() || request.include_raw).then_some(request)
    }

    pub(super) fn queue_metadata_summary_for_path(
        &mut self,
        path: &Path,
        priority: SummaryPriority,
    ) {
        // Filesystem existence is validated by the worker. A synchronous
        // `is_file()` here stalls the UI on network/removable volumes.
        if self.is_virtual_path(path) {
            return;
        }
        let Some(request) = self.metadata_summary_request() else {
            return;
        };
        if self
            .metadata_summary_cache
            .peek(path)
            .is_some_and(|summary| summary_satisfies(summary, &request))
        {
            return;
        }
        if self.metadata_summary_inflight.contains_key(path) {
            if let Some(pool) = &self.metadata_summary_pool {
                pool.promote(path, priority);
            }
            return;
        }
        self.metadata_summary_generation = self.metadata_summary_generation.wrapping_add(1);
        let generation = self.metadata_summary_generation;
        self.metadata_summary_inflight
            .insert(path.to_path_buf(), generation);
        if let Some(pool) = &self.metadata_summary_pool {
            pool.enqueue(
                path.to_path_buf(),
                request,
                match priority {
                    SummaryPriority::Selected => ScanBudget::selected(),
                    _ => ScanBudget::list(),
                },
                generation,
                priority,
            );
        }
    }

    pub(super) fn pump_metadata_summary_prefetch(&mut self) {
        if !self.is_list_workspace_active()
            || self.scan_in_progress
            || self.metadata_summary_inflight.len() >= 512
            || self.metadata_summary_request().is_none()
            || self.items.is_empty()
        {
            return;
        }
        let priority = if matches!(self.sort_key, crate::app::types::SortKey::Metadata(_))
            || !self.search_query.trim().is_empty()
        {
            SummaryPriority::SortFilter
        } else {
            SummaryPriority::Background
        };
        let count = self.items.len().min(32);
        let paths = (0..count)
            .map(|step| {
                let index = (self.metadata_summary_prefetch_cursor + step) % self.items.len();
                self.items[index].path.clone()
            })
            .collect::<Vec<_>>();
        self.metadata_summary_prefetch_cursor =
            (self.metadata_summary_prefetch_cursor + count) % self.items.len();
        for path in paths {
            if self.metadata_summary_inflight.len() >= 512 {
                break;
            }
            self.queue_metadata_summary_for_path(&path, priority);
        }
    }

    pub(super) fn drain_metadata_summary_updates(&mut self, ctx: &egui::Context) {
        let max_updates = if self.playback_is_playing_now() || self.playback_session.is_playing {
            8
        } else {
            128
        };
        let mut drained = 0usize;
        let mut disconnected = false;
        while drained < max_updates && self.frame_budget.should_continue() {
            let next = {
                let Some(rx) = &self.metadata_summary_rx else {
                    break;
                };
                rx.try_recv()
            };
            let update = match next {
                Ok(update) => update,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            };
            drained += 1;
            if self.metadata_summary_inflight.get(&update.path).copied() != Some(update.generation)
            {
                continue;
            }
            self.metadata_summary_inflight.remove(&update.path);
            match update.summary {
                Ok(summary) => {
                    if update.cache_hit {
                        self.metadata_cache_hits = self.metadata_cache_hits.saturating_add(1);
                    }
                    self.metadata_summary_errors.remove(&update.path);
                    self.metadata_summary_cache.insert(update.path, summary);
                }
                Err(error) => {
                    self.metadata_summary_errors.insert(update.path, error);
                }
            }
        }
        if disconnected {
            self.metadata_summary_rx = None;
            self.metadata_summary_pool = None;
        }
        if drained == 0 {
            return;
        }
        if matches!(self.sort_key, crate::app::types::SortKey::Metadata(_))
            && self.sort_dir != crate::app::types::SortDir::None
        {
            self.request_sort();
        }
        if !self.search_query.trim().is_empty() {
            self.schedule_search_refresh();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(
            self.perf.background_repaint_ms(),
        ));
    }

    pub(super) fn metadata_cell_for_path(
        &self,
        path: &Path,
        key: &ColumnKey,
    ) -> Option<MetadataCell> {
        let summary = self.metadata_summary_cache.peek(path)?;
        let partial = summary.completion != Completion::Complete;
        match key {
            ColumnKey::Normalized(key) => {
                let field = summary.fields.iter().find(|field| &field.key == key)?;
                let resolved = field.resolved.as_ref()?.display();
                let mut text = String::new();
                if partial {
                    text.push('~');
                }
                if field.conflict {
                    text.push('!');
                }
                text.push_str(&resolved);
                let tooltip = field
                    .values
                    .iter()
                    .map(|value| {
                        format!(
                            "{}  ← {} @ 0x{:X}",
                            value.value.display(),
                            value.source_path,
                            value.source_range.offset
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(MetadataCell {
                    text,
                    tooltip,
                    partial,
                    conflict: field.conflict,
                })
            }
            ColumnKey::Raw(key) => {
                let values = summary.raw_fields.get(key)?;
                let value_text = values
                    .iter()
                    .map(MetadataValue::display)
                    .collect::<Vec<_>>();
                let mut text = String::new();
                if partial {
                    text.push('~');
                }
                text.push_str(&value_text.join(" | "));
                Some(MetadataCell {
                    text,
                    tooltip: value_text.join("\n"),
                    partial,
                    conflict: values.len() > 1,
                })
            }
            ColumnKey::Builtin(_) => None,
        }
    }

    pub(super) fn metadata_owned_sort_key(&self, path: &Path, index: usize) -> OwnedKey {
        let Some(column) = self.metadata_list_columns.get(index) else {
            return OwnedKey::Missing;
        };
        let Some(summary) = self.metadata_summary_cache.peek(path) else {
            return OwnedKey::Missing;
        };
        let value = match &column.key {
            ColumnKey::Normalized(key) => summary
                .fields
                .iter()
                .find(|field| &field.key == key)
                .and_then(|field| field.resolved.as_ref()),
            ColumnKey::Raw(key) => summary
                .raw_fields
                .get(key)
                .and_then(|values| values.first()),
            ColumnKey::Builtin(_) => None,
        };
        match value {
            Some(MetadataValue::Signed(value)) => OwnedKey::Num(Some(*value as f64)),
            Some(MetadataValue::Unsigned(value)) => OwnedKey::Num(Some(*value as f64)),
            Some(MetadataValue::Float(value)) if value.is_finite() => OwnedKey::Num(Some(*value)),
            Some(MetadataValue::Bool(value)) => OwnedKey::Num(Some(if *value { 1.0 } else { 0.0 })),
            Some(value) => OwnedKey::Str(value.display()),
            None => OwnedKey::Missing,
        }
    }

    pub(super) fn metadata_search_matches(
        &self,
        path: &Path,
        query_lower: &str,
        regex: Option<&regex::Regex>,
    ) -> bool {
        let Some(summary) = self.metadata_summary_cache.peek(path) else {
            return false;
        };
        let values = summary
            .fields
            .iter()
            .filter_map(|field| field.resolved.as_ref())
            .chain(summary.raw_fields.values().flatten());
        if let Some(regex) = regex {
            values
                .map(MetadataValue::display)
                .any(|value| regex.is_match(&value))
        } else {
            values
                .map(MetadataValue::display)
                .any(|value| value.to_lowercase().contains(query_lower))
        }
    }
}

fn summary_satisfies(summary: &MetadataSummary, request: &SummaryRequest) -> bool {
    let all = summary.coverage.iter().any(|key| key == "__all__");
    (all || request
        .fields
        .iter()
        .all(|field| summary.coverage.contains(field)))
        && (!request.include_raw || summary.coverage.iter().any(|key| key == "__raw__"))
}
