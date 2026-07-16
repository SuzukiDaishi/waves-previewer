//! GUI batch loudness normalize: measure LUFS through the async meta pool,
//! then set per-file gain through the unified gain framework. No audio files
//! are written — files with an open loaded editor tab get a destructive tab
//! edit (editor undo), everything else gets a pending list gain (one list
//! undo action for the whole batch).

use std::collections::HashSet;
use std::path::PathBuf;

use super::types::{BatchLoudnormState, LoudnormPhase, MediaSource, ToastSeverity};
use super::WavesPreviewer;

const LOUDNORM_META_QUEUE_BUDGET: usize = 64;
const LOUDNORM_APPLY_BUDGET_MS: u64 = 6;
/// Gains closer than this to the target count as already normalized.
const LOUDNORM_SKIP_EPSILON_LU: f32 = 0.05;

impl WavesPreviewer {
    pub(super) fn open_loudnorm_dialog(&mut self) {
        if self.batch_loudnorm_state.is_some() {
            self.push_toast(ToastSeverity::Info, "A loudness batch is already running");
            return;
        }
        self.show_loudnorm_dialog = true;
    }

    /// Selection when non-empty, else every real file in the list.
    fn loudnorm_target_paths(&self) -> Vec<PathBuf> {
        let selected: Vec<PathBuf> = self
            .selected_paths()
            .into_iter()
            .filter(|p| !self.is_external_path(p))
            .filter(|p| {
                self.item_for_path(p)
                    .map(|item| item.source == MediaSource::File)
                    .unwrap_or(false)
            })
            .collect();
        if !selected.is_empty() {
            return selected;
        }
        self.files
            .iter()
            .filter_map(|id| self.item_for_id(*id))
            .filter(|item| item.source == MediaSource::File)
            .map(|item| item.path.clone())
            .collect()
    }

    pub(super) fn ui_loudnorm_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_loudnorm_dialog {
            return;
        }
        let mut open = true;
        let mut run_clicked = false;
        let target_count = self.loudnorm_target_paths().len();
        egui::Window::new("Normalize Loudness")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "Sets each file's gain so its integrated loudness hits the target.\n\
                     {target_count} file(s) (selection, or the whole list when nothing is selected)."
                ));
                ui.horizontal(|ui| {
                    ui.label("Target");
                    ui.add(
                        egui::DragValue::new(&mut self.loudnorm_dialog_target)
                            .range(-36.0..=0.0)
                            .speed(0.1)
                            .suffix(" LUFS"),
                    );
                });
                ui.label(
                    egui::RichText::new(
                        "Non-destructive: sets list gain (pending) — no audio files are \
                         written. Files open in an editor tab are edited in the tab instead \
                         (undo in the editor).",
                    )
                    .weak(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(target_count > 0, egui::Button::new("Normalize"))
                        .clicked()
                    {
                        run_clicked = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_loudnorm_dialog = false;
                    }
                });
            });
        if run_clicked {
            self.show_loudnorm_dialog = false;
            self.save_prefs();
            let targets = self.loudnorm_target_paths();
            let target_lufs = self.loudnorm_dialog_target;
            self.begin_batch_loudnorm(targets, target_lufs);
        } else if !open {
            self.show_loudnorm_dialog = false;
        }
    }

    fn loudnorm_meta_ready(&self, path: &std::path::Path) -> bool {
        if self.lufs_override.contains_key(path) {
            return true;
        }
        let Some(meta) = self.meta_for_path(path) else {
            return false;
        };
        meta.lufs_i.is_some() || meta.decode_error.is_some()
    }

    pub(super) fn begin_batch_loudnorm(&mut self, targets: Vec<PathBuf>, target_lufs: f32) {
        if targets.is_empty() || self.batch_loudnorm_state.is_some() {
            return;
        }
        let before = self.capture_list_selection_snapshot();
        let mut pending: HashSet<PathBuf> = HashSet::new();
        for path in &targets {
            if !self.loudnorm_meta_ready(path) {
                pending.insert(path.clone());
            }
        }
        let queue: Vec<PathBuf> = pending.iter().cloned().collect();
        self.batch_loudnorm_state = Some(BatchLoudnormState {
            targets,
            target_lufs,
            phase: LoudnormPhase::Measure,
            pending,
            queue,
            apply_index: 0,
            before,
            before_items: Vec::new(),
            cancel_requested: false,
            updated: 0,
            tab_edited: 0,
            skipped: 0,
            clip_risk: 0,
            failed: 0,
            started_at: std::time::Instant::now(),
        });
    }

    /// Called from the meta drain whenever a path's metadata updates, same
    /// hook point as the CSV export progress (incl. the re-queue fallback for
    /// finished jobs that still lack LUFS).
    pub(super) fn update_loudnorm_progress_for_path(&mut self, path: &std::path::Path) {
        let pending = self
            .batch_loudnorm_state
            .as_ref()
            .map(|s| s.pending.contains(path))
            .unwrap_or(false);
        if !pending {
            return;
        }
        if self.loudnorm_meta_ready(path) {
            if let Some(state) = &mut self.batch_loudnorm_state {
                state.pending.remove(path);
            }
        } else if !self.meta_inflight.contains(path) {
            if let Some(state) = &mut self.batch_loudnorm_state {
                state.queue.push(path.to_path_buf());
            }
        }
    }

    fn pump_loudnorm_meta_queue(&mut self) {
        let mut to_queue: Vec<PathBuf> = Vec::new();
        if let Some(state) = &mut self.batch_loudnorm_state {
            while to_queue.len() < LOUDNORM_META_QUEUE_BUDGET {
                let Some(p) = state.queue.pop() else {
                    break;
                };
                if !state.pending.contains(&p) || self.meta_inflight.contains(&p) {
                    continue;
                }
                to_queue.push(p);
            }
        }
        for p in to_queue {
            self.queue_full_meta_for_path(&p, false);
        }
    }

    pub(super) fn cancel_batch_loudnorm(&mut self) {
        if let Some(state) = &mut self.batch_loudnorm_state {
            state.cancel_requested = true;
        }
    }

    pub(super) fn tick_batch_loudnorm(&mut self) {
        let Some(state) = self.batch_loudnorm_state.as_ref() else {
            return;
        };
        if state.cancel_requested {
            let state = self.batch_loudnorm_state.take().expect("state present");
            self.restore_batch_loudnorm(&state);
            let msg = if state.apply_index > 0 {
                "Loudness normalize cancelled; pending gains restored \
                 (edits already applied in open tabs stay undoable there)"
            } else {
                "Loudness normalize cancelled"
            };
            self.push_toast(ToastSeverity::Info, msg);
            return;
        }
        match state.phase {
            LoudnormPhase::Measure => {
                self.pump_loudnorm_meta_queue();
                let ready = self
                    .batch_loudnorm_state
                    .as_ref()
                    .map(|s| s.pending.is_empty())
                    .unwrap_or(false);
                if ready {
                    if let Some(state) = &mut self.batch_loudnorm_state {
                        state.phase = LoudnormPhase::Apply;
                    }
                }
            }
            LoudnormPhase::Apply => self.tick_batch_loudnorm_apply(),
        }
    }

    fn tick_batch_loudnorm_apply(&mut self) {
        let budget = std::time::Duration::from_millis(LOUDNORM_APPLY_BUDGET_MS);
        let start = std::time::Instant::now();
        loop {
            let Some(state) = self.batch_loudnorm_state.as_ref() else {
                return;
            };
            if state.cancel_requested {
                return;
            }
            let idx = state.apply_index;
            if idx >= state.targets.len() {
                break;
            }
            if start.elapsed() >= budget {
                return;
            }
            let path = state.targets[idx].clone();
            let target = state.target_lufs;

            // Capture the pre-change item for the single batch undo action.
            let before_chunk = self.capture_list_undo_items_by_paths(&[path.clone()]);

            let pending_gain = self.pending_gain_db_for_path(&path);
            let effective = self
                .lufs_override
                .get(&path)
                .copied()
                .or_else(|| {
                    self.meta_for_path(&path)
                        .and_then(|m| m.lufs_i)
                        .map(|v| v + pending_gain)
                });
            enum Outcome {
                Failed,
                Skipped,
                Applied { to_tab: bool, clip_risk: bool },
            }
            let outcome = match effective {
                None => Outcome::Failed,
                Some(effective) if !effective.is_finite() => Outcome::Failed,
                Some(effective) => {
                    let delta = target - effective;
                    if delta.abs() < LOUDNORM_SKIP_EPSILON_LU {
                        Outcome::Skipped
                    } else {
                        let base_peak = self
                            .meta_for_path(&path)
                            .and_then(|m| m.true_peak_db.or(m.peak_db));
                        let clip_risk = base_peak
                            .map(|p| p + pending_gain + delta > 0.0)
                            .unwrap_or(false);
                        let to_tab = self.apply_file_gain_delta_unified(&path, delta);
                        if !to_tab {
                            // Pending route does not refresh effective LUFS
                            // on its own (the tab route does).
                            self.schedule_lufs_for_path(path.clone());
                        }
                        Outcome::Applied { to_tab, clip_risk }
                    }
                }
            };
            if let Some(state) = &mut self.batch_loudnorm_state {
                state.before_items.extend(before_chunk);
                state.apply_index += 1;
                match outcome {
                    Outcome::Failed => state.failed += 1,
                    Outcome::Skipped => state.skipped += 1,
                    Outcome::Applied { to_tab, clip_risk } => {
                        if to_tab {
                            state.tab_edited += 1;
                        } else {
                            state.updated += 1;
                        }
                        if clip_risk {
                            state.clip_risk += 1;
                        }
                    }
                }
            }
        }
        // Finalize: one undo action for the whole batch.
        let Some(state) = self.batch_loudnorm_state.take() else {
            return;
        };
        let after_items = self.capture_list_undo_items_by_paths(&state.targets);
        self.record_list_update_from_paths_with_after(
            state.before_items.clone(),
            after_items,
            state.before.clone(),
        );
        let severity = if state.clip_risk + state.failed > 0 {
            ToastSeverity::Warning
        } else {
            ToastSeverity::Info
        };
        self.push_toast(
            severity,
            format!(
                "Loudness → {:+.1} LUFS: {} gains set, {} edited in open tabs, {} already on target, {} clip risk, {} failed",
                state.target_lufs,
                state.updated,
                state.tab_edited,
                state.skipped,
                state.clip_risk,
                state.failed
            ),
        );
    }

    /// Cancel-time rollback for the pending-gain route: restore each captured
    /// item's pre-batch pending gain. Tab-route edits keep their editor undo.
    fn restore_batch_loudnorm(&mut self, state: &BatchLoudnormState) {
        for entry in &state.before_items {
            self.set_pending_gain_db_for_path(&entry.item.path, entry.item.pending_gain_db);
            self.schedule_lufs_for_path(entry.item.path.clone());
        }
    }
}
