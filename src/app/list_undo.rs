use std::collections::HashSet;
use std::path::PathBuf;

use crate::app::types::{
    ListSelectionSnapshot, ListUndoAction, ListUndoActionKind, ListUndoItem, UndoScope,
};

impl crate::app::WavesPreviewer {
    pub(super) fn capture_list_selection_snapshot(&self) -> ListSelectionSnapshot {
        let selected_path = self.selected_path_buf();
        let selected_paths: Vec<PathBuf> = self
            .selected_multi
            .iter()
            .filter_map(|&row| self.path_for_row(row).cloned())
            .collect();
        let anchor_path = self
            .select_anchor
            .and_then(|row| self.path_for_row(row).cloned());
        ListSelectionSnapshot {
            selected_path,
            selected_paths,
            anchor_path,
            playing_path: self.playing_path.clone(),
        }
    }

    pub(super) fn restore_list_selection_snapshot(&mut self, snap: &ListSelectionSnapshot) {
        self.selected = snap
            .selected_path
            .as_ref()
            .and_then(|p| self.row_for_path(p));
        self.selected_multi.clear();
        for p in &snap.selected_paths {
            if let Some(row) = self.row_for_path(p) {
                self.selected_multi.insert(row);
            }
        }
        if let Some(sel) = self.selected {
            if self.selected_multi.is_empty() {
                self.selected_multi.insert(sel);
            }
        } else if let Some(first) = self.selected_multi.iter().next().copied() {
            self.selected = Some(first);
        }
        self.select_anchor = snap
            .anchor_path
            .as_ref()
            .and_then(|p| self.row_for_path(p));
        self.playing_path = snap.playing_path.clone();
        if self.files.is_empty() {
            self.selected = None;
            self.selected_multi.clear();
            self.select_anchor = None;
        }
    }

    pub(super) fn capture_list_undo_items(&self, paths: &[PathBuf]) -> Vec<ListUndoItem> {
        let mut unique: HashSet<PathBuf> = HashSet::new();
        let mut out = Vec::new();
        for path in paths {
            if !unique.insert(path.clone()) {
                continue;
            }
            let Some(id) = self.path_index.get(path).copied() else {
                continue;
            };
            let Some(item_idx) = self.item_index.get(&id).copied() else {
                continue;
            };
            let Some(item) = self.items.get(item_idx).cloned() else {
                continue;
            };
            let edited_cache = self.edited_cache.get(path).cloned();
            let lufs_override = self.lufs_override.get(path).copied();
            let lufs_deadline = self.lufs_recalc_deadline.get(path).copied();
            out.push(ListUndoItem {
                item,
                item_index: item_idx,
                edited_cache,
                lufs_override,
                lufs_deadline,
            });
        }
        out
    }

    pub(super) fn capture_list_undo_items_by_paths(&self, paths: &[PathBuf]) -> Vec<ListUndoItem> {
        let mut out = Vec::new();
        for path in paths {
            let Some(id) = self.path_index.get(path).copied() else {
                continue;
            };
            let Some(item_idx) = self.item_index.get(&id).copied() else {
                continue;
            };
            let Some(item) = self.items.get(item_idx).cloned() else {
                continue;
            };
            let edited_cache = self.edited_cache.get(path).cloned();
            let lufs_override = self.lufs_override.get(path).copied();
            let lufs_deadline = self.lufs_recalc_deadline.get(path).copied();
            out.push(ListUndoItem {
                item,
                item_index: item_idx,
                edited_cache,
                lufs_override,
                lufs_deadline,
            });
        }
        out
    }

    fn apply_list_insert_items(&mut self, items: &[ListUndoItem]) {
        if items.is_empty() {
            return;
        }
        let mut sorted = items.to_vec();
        sorted.sort_by_key(|i| i.item_index);
        for entry in &sorted {
            let pos = entry.item_index.min(self.items.len());
            self.items.insert(pos, entry.item.clone());
        }
        self.rebuild_item_indexes();
        for entry in items {
            if let Some(cache) = entry.edited_cache.clone() {
                self.edited_cache.insert(entry.item.path.clone(), cache);
            }
            if let Some(v) = entry.lufs_override {
                self.lufs_override.insert(entry.item.path.clone(), v);
            }
            if let Some(v) = entry.lufs_deadline {
                self.lufs_recalc_deadline.insert(entry.item.path.clone(), v);
            }
        }
        if self.external_source.is_some() {
            self.apply_external_mapping();
        }
        self.apply_filter_from_search();
        self.apply_sort();
    }

    fn apply_list_remove_items(&mut self, items: &[ListUndoItem]) {
        if items.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = items.iter().map(|i| i.item.path.clone()).collect();
        self.remove_paths_from_list(&paths);
    }

    fn apply_list_update_items(&mut self, items: &[ListUndoItem]) {
        if items.is_empty() {
            return;
        }
        for entry in items {
            let idx = self
                .path_index
                .get(&entry.item.path)
                .copied()
                .and_then(|id| self.item_index.get(&id).copied());
            if let Some(item_idx) = idx {
                if let Some(slot) = self.items.get_mut(item_idx) {
                    *slot = entry.item.clone();
                }
            }
            if let Some(cache) = entry.edited_cache.clone() {
                self.edited_cache.insert(entry.item.path.clone(), cache);
            } else {
                self.edited_cache.remove(&entry.item.path);
            }
            match entry.lufs_override {
                Some(v) => {
                    self.lufs_override.insert(entry.item.path.clone(), v);
                }
                None => {
                    self.lufs_override.remove(&entry.item.path);
                }
            }
            match entry.lufs_deadline {
                Some(v) => {
                    self.lufs_recalc_deadline.insert(entry.item.path.clone(), v);
                }
                None => {
                    self.lufs_recalc_deadline.remove(&entry.item.path);
                }
            }
        }
        self.apply_filter_from_search();
        self.apply_sort();
    }

    fn push_list_undo_action(&mut self, action: ListUndoAction) {
        self.list_redo_stack.clear();
        self.list_undo_stack.push(action);
        while self.list_undo_stack.len() > 20 {
            self.list_undo_stack.remove(0);
        }
        self.last_undo_scope = UndoScope::List;
    }

    pub(super) fn list_undo(&mut self) -> bool {
        let Some(action) = self.list_undo_stack.pop() else {
            return false;
        };
        self.apply_list_action(&action, true);
        self.list_redo_stack.push(action);
        self.last_undo_scope = UndoScope::List;
        true
    }

    pub(super) fn list_redo(&mut self) -> bool {
        let Some(action) = self.list_redo_stack.pop() else {
            return false;
        };
        self.apply_list_action(&action, false);
        self.list_undo_stack.push(action);
        self.last_undo_scope = UndoScope::List;
        true
    }

    fn apply_list_action(&mut self, action: &ListUndoAction, undo: bool) {
        match &action.kind {
            ListUndoActionKind::Remove { items } => {
                if undo {
                    self.apply_list_insert_items(items);
                    self.restore_list_selection_snapshot(&action.before);
                } else {
                    self.apply_list_remove_items(items);
                    self.restore_list_selection_snapshot(&action.after);
                }
            }
            ListUndoActionKind::Insert { items } => {
                if undo {
                    self.apply_list_remove_items(items);
                    self.restore_list_selection_snapshot(&action.before);
                } else {
                    self.apply_list_insert_items(items);
                    self.restore_list_selection_snapshot(&action.after);
                }
            }
            ListUndoActionKind::Update { before, after } => {
                if undo {
                    self.apply_list_update_items(before);
                    self.restore_list_selection_snapshot(&action.before);
                } else {
                    self.apply_list_update_items(after);
                    self.restore_list_selection_snapshot(&action.after);
                }
            }
        }
    }

    pub(super) fn remove_paths_from_list_with_undo(&mut self, paths: &[PathBuf]) {
        let before = self.capture_list_selection_snapshot();
        let removed = self.capture_list_undo_items(paths);
        if removed.is_empty() {
            return;
        }
        self.remove_paths_from_list(paths);
        let after = self.capture_list_selection_snapshot();
        self.push_list_undo_action(ListUndoAction {
            kind: ListUndoActionKind::Remove { items: removed },
            before,
            after,
        });
    }

    pub(super) fn record_list_insert_from_paths(
        &mut self,
        paths: &[PathBuf],
        before: ListSelectionSnapshot,
    ) {
        let items = self.capture_list_undo_items_by_paths(paths);
        if items.is_empty() {
            return;
        }
        let after = self.capture_list_selection_snapshot();
        self.push_list_undo_action(ListUndoAction {
            kind: ListUndoActionKind::Insert { items },
            before,
            after,
        });
    }

    pub(super) fn record_list_update_from_paths(
        &mut self,
        paths: &[PathBuf],
        before_items: Vec<ListUndoItem>,
        before: ListSelectionSnapshot,
    ) {
        if before_items.is_empty() {
            return;
        }
        let after_items = self.capture_list_undo_items_by_paths(paths);
        use std::collections::HashMap;
        let mut before_map: HashMap<&PathBuf, (f32, Option<f32>, Option<std::time::Instant>)> =
            HashMap::new();
        for item in &before_items {
            before_map.insert(
                &item.item.path,
                (item.item.pending_gain_db, item.lufs_override, item.lufs_deadline),
            );
        }
        let mut changed = false;
        for item in &after_items {
            if let Some((gain, lufs, dl)) = before_map.get(&item.item.path) {
                if (item.item.pending_gain_db - gain).abs() > 1e-6
                    || item.lufs_override != *lufs
                    || item.lufs_deadline != *dl
                {
                    changed = true;
                    break;
                }
            } else {
                changed = true;
                break;
            }
        }
        if !changed {
            return;
        }
        let after = self.capture_list_selection_snapshot();
        self.push_list_undo_action(ListUndoAction {
            kind: ListUndoActionKind::Update {
                before: before_items,
                after: after_items,
            },
            before,
            after,
        });
    }
}
