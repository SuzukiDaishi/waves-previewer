//! GUI batch inspection run: dialog, worker pool, progress, and results
//! handoff. The actual checks live in `crate::app::inspection` (shared with
//! the CLI `batch inspect` command).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::inspection::{CachedAudioFacts, InspectionConfig, InspectionRow};
use super::types::{InspectionReportState, InspectionRunState, MediaSource, ToastSeverity};
use super::WavesPreviewer;

const INSPECTION_MAX_WORKERS: usize = 4;
const INSPECTION_DRAIN_PER_FRAME: usize = 64;

impl WavesPreviewer {
    /// Selection when non-empty, else every real (file-backed) list item.
    pub(super) fn inspection_target_paths(&self) -> Vec<PathBuf> {
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

    pub(super) fn open_inspection_dialog(&mut self) {
        if self.inspection_run_state.is_some() {
            self.push_toast(ToastSeverity::Info, "An inspection is already running");
            return;
        }
        self.show_inspection_dialog = true;
    }

    pub(super) fn ui_inspection_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_inspection_dialog {
            return;
        }
        let mut open = true;
        let mut run_clicked = false;
        let target_count = self.inspection_target_paths().len();
        egui::Window::new("Inspect Files (QA)")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "Checks {target_count} file(s) (selection, or the whole list when nothing is selected)."
                ));
                ui.separator();
                let cfg = &mut self.inspection_cfg;
                ui.checkbox(&mut cfg.check_true_peak, "True peak ceiling");
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        cfg.check_true_peak,
                        egui::DragValue::new(&mut cfg.tp_ceiling_db)
                            .range(-12.0..=0.0)
                            .speed(0.1)
                            .suffix(" dBTP"),
                    );
                });
                ui.checkbox(&mut cfg.check_loudness, "Loudness window");
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        cfg.check_loudness,
                        egui::DragValue::new(&mut cfg.target_lufs)
                            .range(-36.0..=0.0)
                            .speed(0.1)
                            .suffix(" LUFS"),
                    );
                    ui.label("±");
                    ui.add_enabled(
                        cfg.check_loudness,
                        egui::DragValue::new(&mut cfg.lufs_tolerance_lu)
                            .range(0.1..=12.0)
                            .speed(0.1)
                            .suffix(" LU"),
                    );
                });
                ui.checkbox(&mut cfg.check_silence, "Leading/trailing silence");
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        cfg.check_silence,
                        egui::DragValue::new(&mut cfg.max_leading_silence_ms)
                            .range(0.0..=10_000.0)
                            .speed(10.0)
                            .prefix("lead > ")
                            .suffix(" ms"),
                    );
                    ui.add_enabled(
                        cfg.check_silence,
                        egui::DragValue::new(&mut cfg.max_trailing_silence_ms)
                            .range(0.0..=60_000.0)
                            .speed(10.0)
                            .prefix("trail > ")
                            .suffix(" ms"),
                    );
                    ui.add_enabled(
                        cfg.check_silence,
                        egui::DragValue::new(&mut cfg.silence_threshold_dbfs)
                            .range(-120.0..=-20.0)
                            .speed(1.0)
                            .prefix("floor ")
                            .suffix(" dBFS"),
                    );
                });
                ui.checkbox(&mut cfg.check_loop, "Loop marker validity");
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(cfg.check_loop, |ui| {
                        ui.checkbox(&mut cfg.require_loop, "Require loop markers");
                    });
                });
                if cfg.check_silence {
                    ui.label(
                        egui::RichText::new(
                            "Silence check decodes each file once; large lists take a while.",
                        )
                        .weak(),
                    );
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(target_count > 0, egui::Button::new("Run Inspection"))
                        .clicked()
                    {
                        run_clicked = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_inspection_dialog = false;
                    }
                });
            });
        if run_clicked {
            self.show_inspection_dialog = false;
            self.save_prefs();
            let targets = self.inspection_target_paths();
            let cfg = self.inspection_cfg;
            self.begin_inspection_run(targets, cfg);
        } else if !open {
            self.show_inspection_dialog = false;
        }
    }

    pub(super) fn begin_inspection_run(&mut self, paths: Vec<PathBuf>, cfg: InspectionConfig) {
        if paths.is_empty() || self.inspection_run_state.is_some() {
            return;
        }
        // Snapshot cached facts on the UI thread so workers never touch app
        // state. peak_db only counts when it came from a full decode.
        let jobs: VecDeque<(PathBuf, f32, CachedAudioFacts)> = paths
            .iter()
            .map(|path| {
                let mut facts = CachedAudioFacts::default();
                if let Some(meta) = self.meta_for_path(path) {
                    facts.lufs_i = meta.lufs_i;
                    facts.true_peak_db = meta.true_peak_db;
                    if !meta.peak_db_estimate {
                        facts.peak_db = meta.peak_db;
                    }
                    facts.total_frames = meta.total_frames;
                }
                (path.clone(), self.pending_gain_db_for_path(path), facts)
            })
            .collect();
        let total = jobs.len();
        let queue = Arc::new(Mutex::new(jobs));
        let (tx, rx) = std::sync::mpsc::channel::<InspectionRow>();
        let cancel = Arc::new(AtomicBool::new(false));
        let workers = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).max(1))
            .unwrap_or(1)
            .min(INSPECTION_MAX_WORKERS)
            .min(total.max(1));
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let tx = tx.clone();
            let cancel = Arc::clone(&cancel);
            std::thread::spawn(move || {
                crate::app::threading::lower_current_thread_priority();
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let job = queue.lock().ok().and_then(|mut q| q.pop_front());
                    let Some((path, pending_gain, facts)) = job else {
                        break;
                    };
                    let row =
                        crate::app::inspection::inspect_file(&path, pending_gain, &facts, &cfg, &cancel);
                    if tx.send(row).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        self.inspection_run_state = Some(InspectionRunState {
            total,
            done: 0,
            rx,
            cancel,
            rows: Vec::with_capacity(total),
            started_at: std::time::Instant::now(),
        });
    }

    pub(super) fn cancel_inspection_run(&mut self) {
        if let Some(state) = &self.inspection_run_state {
            state.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn drain_inspection_results(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.inspection_run_state else {
            return;
        };
        let mut disconnected = false;
        for _ in 0..INSPECTION_DRAIN_PER_FRAME {
            match state.rx.try_recv() {
                Ok(row) => {
                    state.rows.push(row);
                    state.done += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        let finished = state.done >= state.total || disconnected;
        if finished {
            let state = self.inspection_run_state.take().expect("state present");
            let cancelled = state.cancel.load(Ordering::Relaxed);
            let mut rows = state.rows;
            // Stable order: errors first, then warnings, then passes; ties by path.
            rows.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.path.cmp(&b.path)));
            let errors = rows
                .iter()
                .filter(|r| r.severity == Some(super::inspection::IssueSeverity::Error))
                .count();
            let warnings = rows
                .iter()
                .filter(|r| r.severity == Some(super::inspection::IssueSeverity::Warning))
                .count();
            let passed = rows.len() - errors - warnings;
            let msg = if cancelled {
                format!(
                    "Inspection cancelled: {} of {} files checked ({errors} errors, {warnings} warnings)",
                    rows.len(),
                    state.total
                )
            } else {
                format!("Inspection finished: {errors} errors, {warnings} warnings, {passed} passed")
            };
            let severity = if errors > 0 {
                ToastSeverity::Warning
            } else {
                ToastSeverity::Info
            };
            self.push_toast(severity, msg);
            self.inspection_report = Some(InspectionReportState {
                rows,
                cfg: self.inspection_cfg,
                generated_at: std::time::SystemTime::now(),
                cancelled,
            });
            self.show_inspection_window = true;
        }
        ctx.request_repaint();
    }
}
