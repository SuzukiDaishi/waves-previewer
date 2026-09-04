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

pub(super) struct InspectionFinalizeResult {
    report: InspectionReportState,
    message: String,
    severity: ToastSeverity,
}

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
        if self.inspection_run_state.is_some() || self.inspection_finalize_rx.is_some() {
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
        let scroll_target = self.begin_floating_scroll_surface("inspection_dialog_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let shown = egui::Window::new("Inspect Files (QA)")
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
                ui.checkbox(&mut cfg.check_naming, "Naming rule (regex on file stem)");
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        cfg.check_naming,
                        egui::TextEdit::singleline(&mut cfg.naming_pattern)
                            .hint_text("^(se|bgm|vo)_[a-z0-9_]+$")
                            .desired_width(260.0),
                    );
                });
                if cfg.check_naming {
                    match regex::Regex::new(cfg.naming_pattern.trim()) {
                        Ok(_) => {}
                        Err(_) => {
                            ui.label(
                                egui::RichText::new("Pattern does not compile — rows will report a config error")
                                    .color(egui::Color32::from_rgb(235, 200, 90)),
                            );
                        }
                    }
                }
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
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        if run_clicked {
            self.show_inspection_dialog = false;
            self.save_prefs();
            let targets = self.inspection_target_paths();
            let cfg = self.inspection_cfg.clone();
            self.begin_inspection_run(targets, cfg);
        } else if !open {
            self.show_inspection_dialog = false;
        }
    }

    pub(super) fn begin_inspection_run(&mut self, paths: Vec<PathBuf>, cfg: InspectionConfig) {
        if paths.is_empty()
            || self.inspection_run_state.is_some()
            || self.inspection_finalize_rx.is_some()
        {
            return;
        }
        // Snapshot cached facts on the UI thread so workers never touch app
        // state. peak_db only counts when it came from a full decode.
        let mut jobs: VecDeque<(PathBuf, f32, CachedAudioFacts)> = VecDeque::new();
        for path in paths {
            let mut facts = CachedAudioFacts::default();
            if let Some(meta) = self.meta_for_path(&path) {
                facts.lufs_i = meta.lufs_i;
                facts.true_peak_db = meta.true_peak_db;
                if !meta.peak_db_estimate {
                    facts.peak_db = meta.peak_db;
                }
                facts.total_frames = meta.total_frames;
            }
            let pending_gain = self.pending_gain_db_for_path(&path);
            jobs.push_back((path, pending_gain, facts));
        }
        let total = jobs.len();
        let queue = Arc::new(Mutex::new(jobs));
        let queue_capacity = self.perf.background_result_queue_capacity();
        let (tx, rx) = std::sync::mpsc::sync_channel::<InspectionRow>(queue_capacity);
        let cancel = Arc::new(AtomicBool::new(false));
        let workers = self
            .perf
            .scan_pool_workers(INSPECTION_MAX_WORKERS)
            .min(total.max(1));
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let tx = tx.clone();
            let cancel = Arc::clone(&cancel);
            let cfg = cfg.clone();
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
                    let row = crate::app::inspection::inspect_file(
                        &path,
                        pending_gain,
                        &facts,
                        &cfg,
                        &cancel,
                    );
                    if tx.send(row).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut result_rows = Vec::new();
        let _ = result_rows.try_reserve(queue_capacity.min(total));
        self.inspection_run_state = Some(InspectionRunState {
            total,
            done: 0,
            rx,
            cancel,
            rows: result_rows,
            started_at: std::time::Instant::now(),
        });
    }

    pub(super) fn cancel_inspection_run(&mut self) {
        if let Some(state) = &self.inspection_run_state {
            state.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn drain_inspection_results(&mut self, ctx: &egui::Context) {
        if let Some(rx) = self.inspection_finalize_rx.as_ref() {
            match super::loading_ops::poll_job(rx) {
                super::loading_ops::JobPoll::Waiting => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(
                        self.perf.background_repaint_ms(),
                    ));
                    return;
                }
                super::loading_ops::JobPoll::Ready(result) => {
                    self.inspection_finalize_rx = None;
                    self.push_toast(result.severity, result.message);
                    self.inspection_report = Some(result.report);
                    self.show_inspection_window = true;
                    ctx.request_repaint();
                    return;
                }
                super::loading_ops::JobPoll::Gone => {
                    self.inspection_finalize_rx = None;
                    self.push_toast(
                        ToastSeverity::Error,
                        "Inspection result worker ended unexpectedly",
                    );
                    return;
                }
            }
        }
        let drain_limit = self.perf.background_result_drain_limit();
        let mut disconnected = false;
        let finished = {
            let budget = &mut self.frame_budget;
            let Some(state) = &mut self.inspection_run_state else {
                return;
            };
            for _ in 0..drain_limit {
                if !budget.should_continue() {
                    break;
                }
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
            state.done >= state.total || disconnected
        };
        if finished {
            let state = self.inspection_run_state.take().expect("state present");
            let cancelled = state.cancel.load(Ordering::Relaxed);
            let total = state.total;
            let mut rows = state.rows;
            let cfg = self.inspection_cfg.clone();
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let spawned = std::thread::Builder::new()
                .name("neowaves-inspection-finalize".to_string())
                .spawn(move || {
                    crate::app::threading::lower_current_thread_priority();
                    // Sorting a report with hundreds of thousands of rows is
                    // itself a batch job; never run it in the result drain.
                    rows.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.path.cmp(&b.path)));
                    let errors = rows
                        .iter()
                        .filter(|row| {
                            row.severity == Some(super::inspection::IssueSeverity::Error)
                        })
                        .count();
                    let warnings = rows
                        .iter()
                        .filter(|row| {
                            row.severity == Some(super::inspection::IssueSeverity::Warning)
                        })
                        .count();
                    let passed = rows.len().saturating_sub(errors + warnings);
                    let message = if cancelled {
                        format!(
                            "Inspection cancelled: {} of {total} files checked ({errors} errors, {warnings} warnings)",
                            rows.len()
                        )
                    } else {
                        format!(
                            "Inspection finished: {errors} errors, {warnings} warnings, {passed} passed"
                        )
                    };
                    let severity = if errors > 0 {
                        ToastSeverity::Warning
                    } else {
                        ToastSeverity::Info
                    };
                    let _ = tx.send(InspectionFinalizeResult {
                        report: InspectionReportState {
                            rows,
                            cfg,
                            cancelled,
                        },
                        message,
                        severity,
                    });
                    crate::ui_wake::wake_ui();
                });
            if spawned.is_ok() {
                self.inspection_finalize_rx = Some(rx);
            } else {
                self.push_toast(
                    ToastSeverity::Error,
                    "Could not start inspection result worker",
                );
            }
        }
        ctx.request_repaint();
    }
}
