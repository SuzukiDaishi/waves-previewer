use std::path::PathBuf;

use super::types::BulkResampleState;
use super::{BULK_RESAMPLE_CHUNK, BULK_RESAMPLE_FRAME_BUDGET_MS, BULK_RESAMPLE_THRESHOLD};

impl super::WavesPreviewer {
    pub(super) fn open_resample_dialog(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let mut picked: Option<u32> = None;
        for p in &paths {
            let sr = self.effective_sample_rate_for_path(p).unwrap_or(out_sr);
            match picked {
                None => picked = Some(sr),
                Some(prev) if prev == sr => {}
                _ => {
                    picked = Some(out_sr);
                    break;
                }
            }
        }
        self.resample_target_sr = picked.unwrap_or(out_sr).max(1);
        self.resample_targets = paths;
        self.resample_error = None;
        self.show_resample_dialog = true;
    }

    pub(super) fn apply_resample_dialog(&mut self) -> Result<(), String> {
        if self.resample_targets.is_empty() {
            return Ok(());
        }
        let target = self.resample_target_sr.max(1);
        if target < 8000 || target > 384_000 {
            return Err("Sample rate must be between 8000 and 384000 Hz.".to_string());
        }
        let targets = self.resample_targets.clone();
        if targets.len() >= BULK_RESAMPLE_THRESHOLD {
            // Large batches are applied over multiple frames to keep the UI responsive.
            let before = self.capture_list_selection_snapshot();
            self.begin_bulk_resample(targets, target, before);
            return Ok(());
        }
        let before = self.capture_list_selection_snapshot();
        let before_items = self.capture_list_undo_items_by_paths(&targets);
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        for p in &targets {
            let file_sr = self.sample_rate_for_path(p, out_sr);
            if target == file_sr {
                self.sample_rate_override.remove(p);
            } else {
                self.sample_rate_override.insert(p.clone(), target);
            }
        }
        self.record_list_update_from_paths(&targets, before_items, before);
        self.refresh_audio_after_sample_rate_change(&targets);
        Ok(())
    }

    pub(super) fn tick_bulk_resample(&mut self) {
        let Some(mut state) = self.bulk_resample_state.take() else {
            return;
        };
        if state.cancel_requested {
            // Restore original overrides if the user cancels mid-flight.
            self.restore_bulk_resample_state(&state);
            return;
        }
        let total = state.targets.len();
        if total == 0 {
            return;
        }
        let budget = std::time::Duration::from_millis(BULK_RESAMPLE_FRAME_BUDGET_MS);
        let start = std::time::Instant::now();
        if !state.finalizing {
            // Phase 1: apply overrides in chunks under a frame budget.
            self.bulk_resample_apply_overrides(&mut state, start, budget);
            if state.index >= total {
                state.finalizing = true;
            }
        }
        if state.finalizing {
            // Phase 2: collect "after" snapshots for undo/history without blocking.
            let done = self.bulk_resample_collect_after(&mut state, start, budget);
            if done {
                self.finalize_bulk_resample(state);
                return;
            }
        }
        self.bulk_resample_state = Some(state);
    }

    pub(super) fn refresh_audio_after_sample_rate_change(&mut self, targets: &[PathBuf]) {
        if targets.is_empty() {
            return;
        }
        if let Some(tab_idx) = self.active_tab {
            if let Some(tab) = self.tabs.get(tab_idx) {
                if targets.iter().any(|p| p == &tab.path) {
                    self.rebuild_current_buffer_with_mode();
                    return;
                }
            }
        }
        if self.active_tab.is_none() {
            if let Some(row_idx) = self.selected {
                if let Some(item) = self.item_for_row(row_idx) {
                    if targets.iter().any(|p| p == &item.path) {
                        self.select_and_load(row_idx, false);
                    }
                }
            }
        }
    }

    fn begin_bulk_resample(
        &mut self,
        targets: Vec<PathBuf>,
        target_sr: u32,
        before: super::types::ListSelectionSnapshot,
    ) {
        self.bulk_resample_state = Some(BulkResampleState {
            targets,
            target_sr,
            index: 0,
            before,
            before_items: Vec::new(),
            started_at: std::time::Instant::now(),
            chunk: BULK_RESAMPLE_CHUNK,
            cancel_requested: false,
            finalizing: false,
            after_items: Vec::new(),
            after_index: 0,
        });
    }

    fn restore_bulk_resample_state(&mut self, state: &BulkResampleState) {
        for entry in &state.before_items {
            match entry.sample_rate_override {
                Some(v) => {
                    self.sample_rate_override.insert(entry.item.path.clone(), v);
                }
                None => {
                    self.sample_rate_override.remove(&entry.item.path);
                }
            }
        }
        self.refresh_audio_after_sample_rate_change(&state.targets);
    }

    fn bulk_resample_apply_overrides(
        &mut self,
        state: &mut BulkResampleState,
        start: std::time::Instant,
        budget: std::time::Duration,
    ) {
        let total = state.targets.len();
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        while state.index < total && start.elapsed() < budget {
            let end = (state.index + state.chunk).min(total);
            let slice = &state.targets[state.index..end];
            let before_chunk = self.capture_list_undo_items_by_paths(slice);
            state.before_items.extend(before_chunk);
            for p in slice {
                let file_sr = self.sample_rate_for_path(p, out_sr);
                if state.target_sr == file_sr {
                    self.sample_rate_override.remove(p);
                } else {
                    self.sample_rate_override.insert(p.clone(), state.target_sr);
                }
            }
            state.index = end;
        }
    }

    fn bulk_resample_collect_after(
        &mut self,
        state: &mut BulkResampleState,
        start: std::time::Instant,
        budget: std::time::Duration,
    ) -> bool {
        let total = state.targets.len();
        while state.after_index < total && start.elapsed() < budget {
            let end = (state.after_index + state.chunk).min(total);
            let slice = &state.targets[state.after_index..end];
            let after_chunk = self.capture_list_undo_items_by_paths(slice);
            state.after_items.extend(after_chunk);
            state.after_index = end;
        }
        if state.after_index >= total {
            return true;
        }
        false
    }

    fn finalize_bulk_resample(&mut self, mut state: BulkResampleState) {
        let targets = state.targets.clone();
        let before_items = std::mem::take(&mut state.before_items);
        let after_items = std::mem::take(&mut state.after_items);
        let before = state.before.clone();
        self.record_list_update_from_paths_with_after(before_items, after_items, before);
        self.refresh_audio_after_sample_rate_change(&targets);
    }
}
