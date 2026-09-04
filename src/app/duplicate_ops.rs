//! Duplicate / similar-sound detection run: worker pool over the target
//! files computing fingerprints (`crate::app::fingerprint`), then
//! clustering into exact / similar groups shown in a results window.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use super::fingerprint::{
    cluster_duplicates_with_options, FileFingerprint, MAX_SIMILAR_OFFSET_MS, SIMILARITY_THRESHOLD,
};
use super::types::ToastSeverity;
use super::WavesPreviewer;

const DUPLICATE_MAX_WORKERS: usize = 4;

pub(super) struct DuplicateScanState {
    pub total: usize,
    pub done: usize,
    pub rx: std::sync::mpsc::Receiver<(usize, Option<FileFingerprint>)>,
    pub cancel: Arc<AtomicBool>,
    pub paths: Arc<Vec<PathBuf>>,
    pub fps: Vec<(usize, FileFingerprint)>,
}

#[derive(Clone, Debug)]
pub(super) struct DuplicateReportGroup {
    pub max_offset_ms: f32,
    pub exact: bool,
    pub min_similarity: f32,
    pub paths: Vec<PathBuf>,
}

pub(super) struct DuplicateReportState {
    pub groups: Vec<DuplicateReportGroup>,
    pub scanned: usize,
    pub failed: usize,
    pub cancelled: bool,
}

pub(super) struct DuplicateFinalizeResult {
    report: DuplicateReportState,
    message: String,
}

impl WavesPreviewer {
    pub(super) fn start_duplicate_scan(&mut self) {
        if self.duplicate_scan_state.is_some() || self.duplicate_finalize_rx.is_some() {
            self.push_toast(ToastSeverity::Info, "A duplicate scan is already running");
            return;
        }
        let paths = self.inspection_target_paths();
        if paths.len() < 2 {
            self.push_toast(
                ToastSeverity::Warning,
                "Find Duplicates: need at least two files (selection or list)",
            );
            return;
        }
        let total = paths.len();
        // Share the one target snapshot with every worker. An atomic cursor
        // avoids duplicating every PathBuf into a second work queue and avoids
        // a contended queue mutex on fast machines.
        let paths = Arc::new(paths);
        let next_job = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let queue_capacity = self.perf.background_result_queue_capacity();
        let (tx, rx) =
            std::sync::mpsc::sync_channel::<(usize, Option<FileFingerprint>)>(queue_capacity);
        let workers = self
            .perf
            .scan_pool_workers(DUPLICATE_MAX_WORKERS)
            .min(total.max(1));
        for _ in 0..workers {
            let paths = Arc::clone(&paths);
            let next_job = Arc::clone(&next_job);
            let tx = tx.clone();
            let cancel = Arc::clone(&cancel);
            std::thread::spawn(move || {
                crate::app::threading::lower_current_thread_priority();
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let idx = next_job.fetch_add(1, Ordering::Relaxed);
                    let Some(path) = paths.get(idx) else {
                        break;
                    };
                    let fp = crate::app::fingerprint::fingerprint_file(path).ok();
                    if tx.send((idx, fp)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut fps = Vec::new();
        let _ = fps.try_reserve(queue_capacity.min(total));
        self.duplicate_scan_state = Some(DuplicateScanState {
            total,
            done: 0,
            rx,
            cancel,
            fps,
            paths,
        });
    }

    pub(super) fn cancel_duplicate_scan(&mut self) {
        if let Some(state) = &self.duplicate_scan_state {
            state.cancel.store(true, Ordering::Relaxed);
        }
        self.finish_duplicate_scan(true);
    }

    pub(super) fn drain_duplicate_scan(&mut self, ctx: &egui::Context) {
        if let Some(rx) = self.duplicate_finalize_rx.as_ref() {
            match super::loading_ops::poll_job(rx) {
                super::loading_ops::JobPoll::Waiting => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(
                        self.perf.background_repaint_ms(),
                    ));
                    return;
                }
                super::loading_ops::JobPoll::Ready(result) => {
                    self.duplicate_finalize_rx = None;
                    self.push_toast(ToastSeverity::Info, result.message);
                    self.duplicate_report = Some(result.report);
                    self.show_duplicates_window = true;
                    ctx.request_repaint();
                    return;
                }
                super::loading_ops::JobPoll::Gone => {
                    self.duplicate_finalize_rx = None;
                    self.push_toast(
                        ToastSeverity::Error,
                        "Duplicate result worker ended unexpectedly",
                    );
                    return;
                }
            }
        }
        let mut finished = false;
        let drain_limit = self.perf.background_result_drain_limit();
        if let Some(state) = &mut self.duplicate_scan_state {
            let budget = &mut self.frame_budget;
            for _ in 0..drain_limit {
                if !budget.should_continue() {
                    break;
                }
                match state.rx.try_recv() {
                    Ok((idx, fp)) => {
                        if let Some(fp) = fp {
                            state.fps.push((idx, fp));
                        }
                        state.done += 1;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
            if state.done >= state.total {
                finished = true;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if finished {
            self.finish_duplicate_scan(false);
        }
    }

    fn finish_duplicate_scan(&mut self, cancelled: bool) {
        let Some(state) = self.duplicate_scan_state.take() else {
            return;
        };
        let allow_offset = self.dup_allow_offset;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("neowaves-duplicate-finalize".to_string())
            .spawn(move || {
                crate::app::threading::lower_current_thread_priority();
                let mut ok_paths: Vec<PathBuf> = Vec::new();
                let mut ok_fps: Vec<FileFingerprint> = Vec::new();
                for (idx, fp) in state.fps {
                    if let Some(path) = state.paths.get(idx) {
                        ok_paths.push(path.clone());
                        ok_fps.push(fp);
                    }
                }
                let failed = state.total.saturating_sub(ok_fps.len());
                let scanned = ok_paths.len();
                let groups: Vec<DuplicateReportGroup> = cluster_duplicates_with_options(
                    &ok_fps,
                    SIMILARITY_THRESHOLD,
                    allow_offset,
                    MAX_SIMILAR_OFFSET_MS,
                )
                .into_iter()
                .map(|group| DuplicateReportGroup {
                    exact: group.exact,
                    max_offset_ms: group.max_offset_ms,
                    min_similarity: group.min_similarity,
                    paths: group
                        .members
                        .iter()
                        .map(|&member| ok_paths[member].clone())
                        .collect(),
                })
                .collect();
                let message = if cancelled {
                    format!("Duplicate scan cancelled ({scanned} scanned)")
                } else if groups.is_empty() {
                    format!("No duplicates found in {scanned} file(s)")
                } else {
                    format!(
                        "Found {} duplicate group(s) across {scanned} file(s)",
                        groups.len()
                    )
                };
                let _ = tx.send(DuplicateFinalizeResult {
                    report: DuplicateReportState {
                        groups,
                        scanned,
                        failed,
                        cancelled,
                    },
                    message,
                });
                crate::ui_wake::wake_ui();
            });
        if spawned.is_ok() {
            self.duplicate_finalize_rx = Some(rx);
        } else {
            self.push_toast(
                ToastSeverity::Error,
                "Could not start duplicate result worker",
            );
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_start_duplicate_scan(&mut self) -> bool {
        self.start_duplicate_scan();
        self.duplicate_scan_state.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_duplicate_scan_active(&self) -> bool {
        self.duplicate_scan_state.is_some() || self.duplicate_finalize_rx.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_duplicate_groups(&self) -> Vec<(bool, Vec<PathBuf>)> {
        self.duplicate_report
            .as_ref()
            .map(|r| {
                r.groups
                    .iter()
                    .map(|g| (g.exact, g.paths.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}
