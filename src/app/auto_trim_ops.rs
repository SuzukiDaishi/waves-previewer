use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::*;

impl super::WavesPreviewer {
    pub(super) fn start_auto_trim(&mut self, tab_idx: usize) {
        // Must be computed before taking `&mut tab` (instance method needs `&self`).
        let selected_ranges = self.all_selected_ranges(tab_idx);

        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };

        // cancel any running job
        if let Some(state) = &tab.auto_trim_state {
            state.cancel.store(true, Ordering::Relaxed);
        }

        let generation = tab
            .auto_trim_state
            .as_ref()
            .map(|s| s.generation + 1)
            .unwrap_or(1);

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_thread = cancel.clone();

        let ch_samples: Vec<Vec<f32>> = tab.ch_samples.clone();
        let sample_rate = tab.buffer_sample_rate;
        let config = tab.auto_trim_config.clone();
        let source_len = ch_samples
            .iter()
            .filter(|ch| !ch.is_empty())
            .map(|ch| ch.len())
            .min()
            .unwrap_or(tab.samples_len);
        let ranges = if selected_ranges.is_empty() {
            vec![(0, source_len)]
        } else {
            selected_ranges
        };

        let (tx, rx) = std::sync::mpsc::channel::<AutoTrimWorkerResult>();

        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            // Whole-buffer level stats so the UI can show noise floor / peak /
            // effective threshold in dB regardless of which ranges were scanned.
            let stats = auto_trim::analyze_levels(&ch_samples, &config);
            let mut detected: Vec<(usize, usize)> = Vec::new();
            for &(start, end) in &ranges {
                if cancel_thread.load(Ordering::Relaxed) {
                    let _ = tx.send(AutoTrimWorkerResult {
                        generation,
                        outcome: Err("Cancelled".to_string()),
                        stats,
                    });
                    return;
                }
                if end <= start {
                    continue;
                }
                let slice: Vec<Vec<f32>> = ch_samples
                    .iter()
                    .map(|ch: &Vec<f32>| {
                        let lo = start.min(ch.len());
                        let hi = end.min(ch.len());
                        ch[lo..hi].to_vec()
                    })
                    .collect();
                match auto_trim::auto_trim_sections(
                    &slice,
                    sample_rate,
                    &config,
                    &cancel_thread,
                    &mut |_p| {},
                ) {
                    Ok(results) => {
                        for r in results {
                            if r.confidence <= 0.0 {
                                continue;
                            }
                            let s = start.saturating_add(r.start);
                            let e = start.saturating_add(r.end);
                            if e > s {
                                detected.push((s, e));
                            }
                        }
                    }
                    Err(msg) if msg == "Cancelled" => {
                        let _ = tx.send(AutoTrimWorkerResult {
                            generation,
                            outcome: Err(msg),
                            stats,
                        });
                        return;
                    }
                    Err(_) => {}
                }
            }
            detected.sort();
            detected.dedup();
            let outcome = if detected.is_empty() {
                Err("No active region detected".to_string())
            } else {
                Ok(auto_trim::AutoTrimOutcome::MultiRange(detected))
            };
            let _ = tx.send(AutoTrimWorkerResult {
                generation,
                outcome,
                stats,
            });
        });

        // Keep previous stats/result visible while the re-run is in flight so
        // live parameter tweaks don't blank the panel.
        let prev = tab.auto_trim_state.take();
        tab.auto_trim_state = Some(AutoTrimState {
            generation,
            running: true,
            progress: 0.0,
            message: "Analyzing…".to_string(),
            result: prev.as_ref().and_then(|s| s.result.clone()),
            stats: prev.as_ref().and_then(|s| s.stats),
            last_config: Some(tab.auto_trim_config.clone()),
            config_dirty_at: None,
            cancel,
            rx: Some(rx),
        });
    }

    /// Debounced live re-run: fires a fresh Auto Trim when the config was
    /// edited after a completed run and the user paused for a moment.
    pub(super) fn poll_auto_trim_live_rerun(&mut self) {
        const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
        let mut rerun: Vec<usize> = Vec::new();
        for (idx, tab) in self.tabs.iter_mut().enumerate() {
            let Some(state) = &mut tab.auto_trim_state else {
                continue;
            };
            if state.running {
                continue;
            }
            let Some(dirty_at) = state.config_dirty_at else {
                continue;
            };
            if dirty_at.elapsed() < DEBOUNCE {
                continue;
            }
            state.config_dirty_at = None;
            if state
                .last_config
                .as_ref()
                .map(|c| *c != tab.auto_trim_config)
                .unwrap_or(true)
            {
                rerun.push(idx);
            }
        }
        for idx in rerun {
            self.start_auto_trim(idx);
        }
    }

    pub(super) fn cancel_auto_trim(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        if let Some(state) = &mut tab.auto_trim_state {
            state.cancel.store(true, Ordering::Relaxed);
            state.running = false;
            state.message = "Cancelled".to_string();
        }
    }

    pub(super) fn drain_auto_trim_results(&mut self) {
        for tab in &mut self.tabs {
            let Some(state) = &mut tab.auto_trim_state else {
                continue;
            };
            if !state.running {
                continue;
            }
            let Some(rx) = &state.rx else {
                continue;
            };
            match rx.try_recv() {
                Ok(AutoTrimWorkerResult {
                    generation: gen,
                    outcome: result,
                    stats,
                }) => {
                    if gen == state.generation {
                        state.running = false;
                        if stats.is_some() {
                            state.stats = stats;
                        }
                        match result {
                            Ok(auto_trim::AutoTrimOutcome::Single(r)) => {
                                state.message = r.message.clone();
                                if r.confidence > 0.0 {
                                    tab.selection = Some((r.start, r.end));
                                    tab.extra_selections.clear();
                                    tab.trim_range = Some((r.start, r.end));
                                }
                                state.result = Some(auto_trim::AutoTrimOutcome::Single(r));
                                state.progress = 1.0;
                            }
                            Ok(auto_trim::AutoTrimOutcome::MultiRange(ranges)) => {
                                let range_count = ranges.len();
                                let mut iter = ranges.clone().into_iter();
                                tab.selection = iter.next();
                                tab.extra_selections = iter.collect();
                                tab.trim_range = if range_count == 1 {
                                    tab.selection
                                } else {
                                    None
                                };
                                state.message = if range_count == 1 {
                                    "Auto Trim selected 1 section".to_string()
                                } else {
                                    format!("Auto Trim selected {range_count} sections")
                                };
                                state.result = Some(auto_trim::AutoTrimOutcome::MultiRange(ranges));
                                state.progress = 1.0;
                            }
                            Err(msg) => {
                                state.message = msg;
                                state.progress = 0.0;
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    state.running = false;
                    if state.message.is_empty() || state.message == "Analyzing…" {
                        state.message = "Worker disconnected".to_string();
                    }
                }
            }
        }
    }
}
