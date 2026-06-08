use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;

impl super::WavesPreviewer {
    pub(super) fn start_loop_detect(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };

        // cancel any running job
        if let Some(state) = &tab.loop_detect_state {
            state.cancel.store(true, Ordering::Relaxed);
        }

        let generation = tab
            .loop_detect_state
            .as_ref()
            .map(|s| s.generation + 1)
            .unwrap_or(1);

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_thread = cancel.clone();

        let ch_samples: Vec<Vec<f32>> = tab.ch_samples.clone();
        let sample_rate = tab.buffer_sample_rate;
        let existing_loop = tab.loop_region_committed.or(tab.loop_region_applied);
        let selection = tab.selection;
        let config = tab
            .loop_detect_state
            .as_ref()
            .map(|s| s.config.clone())
            .unwrap_or_default();
        let config_thread = config.clone();

        let (tx, rx) = std::sync::mpsc::channel::<LoopDetectWorkerEvent>();

        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let mut last_progress = 0.0f32;
            let mut last_emit = Instant::now() - Duration::from_millis(250);
            let mut progress_cb = move |p: f32| {
                let progress = p.clamp(0.0, 1.0);
                let now = Instant::now();
                let should_emit = progress >= 1.0
                    || progress <= 0.0
                    || progress - last_progress >= 0.01
                    || now.duration_since(last_emit) >= Duration::from_millis(120);
                if !should_emit {
                    return;
                }
                last_progress = last_progress.max(progress);
                last_emit = now;
                let _ = progress_tx.send(LoopDetectWorkerEvent::Progress {
                    generation,
                    progress: last_progress,
                    message: loop_detect_progress_message(last_progress),
                });
            };
            let result = loop_detect::detect_loop(
                &ch_samples,
                sample_rate,
                &config_thread,
                existing_loop,
                selection,
                &cancel_thread,
                &mut progress_cb,
            );
            let _ = tx.send(LoopDetectWorkerEvent::Finished { generation, result });
        });

        tab.loop_detect_state = Some(LoopDetectState {
            generation,
            running: true,
            progress: 0.0,
            message: "Detecting loop... 0%".to_string(),
            candidates: Vec::new(),
            selected_idx: 0,
            config,
            cancel,
            rx: Some(rx),
        });
    }

    pub(super) fn cancel_loop_detect(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        if let Some(state) = &mut tab.loop_detect_state {
            state.cancel.store(true, Ordering::Relaxed);
            state.running = false;
            state.message = "Cancelled".to_string();
        }
    }

    pub(super) fn apply_loop_detect_candidate(&mut self, tab_idx: usize, candidate_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let Some(state) = &mut tab.loop_detect_state else {
            return;
        };
        if let Some(cand) = state.candidates.get(candidate_idx) {
            let (s, e) = (cand.start, cand.end);
            state.selected_idx = candidate_idx;
            tab.loop_region = Some((s, e));
        }
    }

    pub(super) fn drain_loop_detect_results(&mut self) {
        for tab in &mut self.tabs {
            let Some(state) = &mut tab.loop_detect_state else {
                continue;
            };
            if !state.running {
                continue;
            }
            let Some(rx) = &state.rx else {
                continue;
            };
            loop {
                match rx.try_recv() {
                    Ok(LoopDetectWorkerEvent::Progress {
                        generation,
                        progress,
                        message,
                    }) => {
                        if generation == state.generation && state.running {
                            let progress = progress.clamp(0.0, 1.0);
                            let progress = if progress >= 1.0 { 0.99 } else { progress };
                            state.progress = state.progress.max(progress).clamp(0.0, 0.99);
                            if !message.is_empty() {
                                state.message = message;
                            }
                        }
                    }
                    Ok(LoopDetectWorkerEvent::Finished { generation, result }) => {
                        if generation == state.generation {
                            state.running = false;
                            match result {
                                Ok(candidates) => {
                                    if candidates.is_empty() {
                                        state.message = "No candidates found".to_string();
                                    } else {
                                        state.message =
                                            format!("{} candidate(s) found", candidates.len());
                                        // auto-apply best High/Medium candidate to loop_region
                                        let best = &candidates[0];
                                        use loop_detect::LoopDetectConfidence;
                                        if best.confidence == LoopDetectConfidence::High
                                            || best.confidence == LoopDetectConfidence::Medium
                                        {
                                            tab.loop_region = Some((best.start, best.end));
                                        }
                                    }
                                    state.candidates = candidates;
                                    state.selected_idx = 0;
                                    state.progress = 1.0;
                                }
                                Err(msg) => {
                                    state.message = msg;
                                    state.progress = 0.0;
                                }
                            }
                        }
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        state.running = false;
                        if state.message.starts_with("Detecting loop") {
                            state.message = "Worker disconnected".to_string();
                        }
                        break;
                    }
                }
            }
        }
    }
}

fn loop_detect_progress_message(progress: f32) -> String {
    let progress = progress.clamp(0.0, 1.0);
    let pct = (progress * 100.0).round() as u32;
    let phase = if progress < 0.15 {
        "Preparing audio"
    } else if progress < 0.25 {
        "Building features"
    } else if progress < 0.40 {
        "Finding candidate points"
    } else if progress < 0.85 {
        "Scoring loop candidates"
    } else if progress < 1.0 {
        "Refining candidates"
    } else {
        "Finishing"
    };
    format!("{phase}... {pct}%")
}
