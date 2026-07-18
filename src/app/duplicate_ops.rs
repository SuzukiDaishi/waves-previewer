//! Duplicate / similar-sound detection run: worker pool over the target
//! files computing fingerprints (`crate::app::fingerprint`), then
//! clustering into exact / similar groups shown in a results window.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::fingerprint::{
    cluster_duplicates_with_options, FileFingerprint, MAX_SIMILAR_OFFSET_MS, SIMILARITY_THRESHOLD,
};
use super::types::ToastSeverity;
use super::WavesPreviewer;

const DUPLICATE_MAX_WORKERS: usize = 4;
const DUPLICATE_DRAIN_PER_FRAME: usize = 32;

pub(super) struct DuplicateScanState {
    pub total: usize,
    pub done: usize,
    pub rx: std::sync::mpsc::Receiver<(usize, Option<FileFingerprint>)>,
    pub cancel: Arc<AtomicBool>,
    pub paths: Vec<PathBuf>,
    pub fps: Vec<Option<FileFingerprint>>,
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

impl WavesPreviewer {
    pub(super) fn start_duplicate_scan(&mut self) {
        if self.duplicate_scan_state.is_some() {
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
        let queue: Arc<Mutex<VecDeque<(usize, PathBuf)>>> = Arc::new(Mutex::new(
            paths.iter().cloned().enumerate().collect(),
        ));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<(usize, Option<FileFingerprint>)>();
        let workers = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).max(1))
            .unwrap_or(1)
            .min(DUPLICATE_MAX_WORKERS)
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
                    let Some((idx, path)) = job else {
                        break;
                    };
                    let fp = crate::app::fingerprint::fingerprint_file(&path).ok();
                    if tx.send((idx, fp)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        self.duplicate_scan_state = Some(DuplicateScanState {
            total,
            done: 0,
            rx,
            cancel,
            fps: vec![None; total],
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
        let mut finished = false;
        if let Some(state) = &mut self.duplicate_scan_state {
            for _ in 0..DUPLICATE_DRAIN_PER_FRAME {
                match state.rx.try_recv() {
                    Ok((idx, fp)) => {
                        if let Some(slot) = state.fps.get_mut(idx) {
                            *slot = fp;
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
        // Cluster only successfully fingerprinted files, keeping their paths.
        let mut ok_paths: Vec<PathBuf> = Vec::new();
        let mut ok_fps: Vec<FileFingerprint> = Vec::new();
        let mut failed = 0usize;
        for (path, fp) in state.paths.iter().zip(state.fps.into_iter()) {
            match fp {
                Some(fp) => {
                    ok_paths.push(path.clone());
                    ok_fps.push(fp);
                }
                None => failed += 1,
            }
        }
        let scanned = ok_paths.len();
        let groups: Vec<DuplicateReportGroup> = cluster_duplicates_with_options(
            &ok_fps,
            SIMILARITY_THRESHOLD,
            self.dup_allow_offset,
            MAX_SIMILAR_OFFSET_MS,
        )
        .into_iter()
        .map(|g| DuplicateReportGroup {
            exact: g.exact,
            max_offset_ms: g.max_offset_ms,
            min_similarity: g.min_similarity,
            paths: g.members.iter().map(|&m| ok_paths[m].clone()).collect(),
        })
        .collect();
        let msg = if cancelled {
            format!("Duplicate scan cancelled ({scanned} scanned)")
        } else if groups.is_empty() {
            format!("No duplicates found in {scanned} file(s)")
        } else {
            format!(
                "Found {} duplicate group(s) across {scanned} file(s)",
                groups.len()
            )
        };
        self.push_toast(ToastSeverity::Info, msg);
        self.duplicate_report = Some(DuplicateReportState {
            groups,
            scanned,
            failed,
            cancelled,
        });
        self.show_duplicates_window = true;
    }

    #[cfg(feature = "kittest")]
    pub fn test_start_duplicate_scan(&mut self) -> bool {
        self.start_duplicate_scan();
        self.duplicate_scan_state.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_duplicate_scan_active(&self) -> bool {
        self.duplicate_scan_state.is_some()
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
