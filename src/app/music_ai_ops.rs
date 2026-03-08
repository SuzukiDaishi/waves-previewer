use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::music_onnx::{
    analyze_music, download_music_model_snapshot_with_progress, has_required_music_model_files,
    is_cancel_error, load_or_demix_stems_for_preview, music_model_repo_root,
    resolve_demucs_model_path, resolve_music_model_dir, resolve_stem_paths, MusicAnalyzeOutput,
};
use super::types::{MusicAnalysisSourceKind, MusicStemSet, StemGainsDb, ToolKind};
use crate::markers::MarkerEntry;

fn fold_download_progress(
    prev_done: usize,
    prev_total: usize,
    next_done: usize,
    next_total: usize,
) -> (usize, usize) {
    let total = prev_total.max(next_total.max(1));
    let done = prev_done.min(total).max(next_done.min(total));
    (done, total)
}

impl crate::app::WavesPreviewer {
    pub(super) fn refresh_music_ai_status(&mut self) {
        self.music_ai_model_dir = resolve_music_model_dir();
        self.music_ai_available = self
            .music_ai_model_dir
            .as_ref()
            .map(|dir| has_required_music_model_files(dir))
            .unwrap_or(false);
    }

    pub(super) fn music_ai_has_model(&self) -> bool {
        self.music_ai_model_dir
            .as_ref()
            .map(|dir| has_required_music_model_files(dir))
            .unwrap_or(false)
    }

    pub(super) fn music_ai_can_uninstall(&self) -> bool {
        self.music_ai_state.is_none() && self.music_model_download_state.is_none()
    }

    pub(super) fn queue_music_model_download(&mut self) {
        if self.music_model_download_state.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<super::MusicModelDownloadEvent>();
        std::thread::spawn(move || {
            let result = match download_music_model_snapshot_with_progress(|done, total| {
                let _ = tx.send(super::MusicModelDownloadEvent::Progress {
                    done: done.min(total.max(1)),
                    total: total.max(1),
                });
            }) {
                Ok(dir) => super::MusicModelDownloadResult {
                    model_dir: Some(dir),
                    error: None,
                },
                Err(err) => super::MusicModelDownloadResult {
                    model_dir: None,
                    error: Some(err),
                },
            };
            let _ = tx.send(super::MusicModelDownloadEvent::Finished(result));
        });
        self.music_model_download_state = Some(super::MusicModelDownloadState {
            _started_at: std::time::Instant::now(),
            done: 0,
            total: 1,
            rx,
        });
        self.music_ai_last_error = None;
    }

    pub(super) fn uninstall_music_model_cache(&mut self) {
        if !self.music_ai_can_uninstall() {
            self.music_ai_last_error = Some(
                "Cannot uninstall music model while analysis/download is running.".to_string(),
            );
            return;
        }
        let dir = music_model_repo_root();
        if !dir.exists() {
            self.refresh_music_ai_status();
            self.music_ai_last_error = None;
            return;
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                self.debug_log(format!("music model cache removed: {}", dir.display()));
                self.music_ai_last_error = None;
            }
            Err(err) => {
                self.music_ai_last_error = Some(format!(
                    "Music model uninstall failed ({}): {err}",
                    dir.display()
                ));
            }
        }
        self.refresh_music_ai_status();
    }

    pub(super) fn start_music_analysis_for_tab(&mut self, tab_idx: usize) {
        self.cancel_music_preview_run();
        self.refresh_music_ai_status();
        if self.music_ai_state.is_some() || self.music_model_download_state.is_some() {
            return;
        }
        let Some(model_dir) = self.music_ai_model_dir.clone() else {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.music_analysis_draft.last_error =
                    Some("Music Analyze model is not installed.".to_string());
            }
            self.music_ai_last_error = Some("Music Analyze model is not installed.".to_string());
            return;
        };
        if !has_required_music_model_files(&model_dir) {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.music_analysis_draft.last_error =
                    Some("Music Analyze model files are incomplete.".to_string());
            }
            self.music_ai_last_error =
                Some("Music Analyze model files are incomplete.".to_string());
            return;
        }

        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let path = tab.path.clone();
        let stems_dir_override = tab.music_analysis_draft.stems_dir_override.clone();
        let stems = resolve_stem_paths(path.as_path(), stems_dir_override.as_deref());
        let can_demix = resolve_demucs_model_path(model_dir.as_path()).is_some();
        if !stems.is_ready() && !can_demix {
            if let Some(tab_mut) = self.tabs.get_mut(tab_idx) {
                tab_mut.music_analysis_draft.last_error = Some(format!(
                    "Missing stems: {} (Demucs model not found)",
                    stems.missing.join(", ")
                ));
            }
            return;
        }
        let source_kind = if stems.is_ready() {
            MusicAnalysisSourceKind::StemsDir
        } else {
            MusicAnalysisSourceKind::AutoDemucs
        };

        let target_sr = self.audio.shared.out_sample_rate.max(1);
        let (tx, rx) = std::sync::mpsc::channel::<super::MusicAnalyzeRunResult>();
        let cancel_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_flag = Arc::clone(&cancel_requested);

        let state_path = path.clone();
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            let _ = tx.send(super::MusicAnalyzeRunResult::Started(path.clone()));
            let progress_path = path.clone();
            let mut last_phase: Option<&'static str> = None;
            let mut send_progress = |message: String| {
                let phase = music_progress_phase(&message);
                if last_phase == Some(phase) {
                    return;
                }
                last_phase = Some(phase);
                let _ = tx.send(super::MusicAnalyzeRunResult::Progress {
                    path: progress_path.clone(),
                    message: phase.to_string(),
                });
            };
            if cancel_flag.load(Ordering::Relaxed) {
                let _ = tx.send(super::MusicAnalyzeRunResult::Finished);
                return;
            }
            let loaded = match load_or_demix_stems_for_preview(
                path.as_path(),
                &stems,
                model_dir.as_path(),
                target_sr,
                &cancel_flag,
                &mut send_progress,
            ) {
                Ok(v) => v,
                Err(err) => {
                    if cancel_flag.load(Ordering::Relaxed) || is_cancel_error(&err) {
                        let _ = tx.send(super::MusicAnalyzeRunResult::Finished);
                        return;
                    }
                    let _ = tx.send(super::MusicAnalyzeRunResult::Item(
                        super::MusicAnalyzeItemResult {
                            path,
                            result: None,
                            source_len_samples: 0,
                            source_kind,
                            stems: None,
                            error: Some(err),
                        },
                    ));
                    let _ = tx.send(super::MusicAnalyzeRunResult::Finished);
                    return;
                }
            };
            if cancel_flag.load(Ordering::Relaxed) {
                let _ = tx.send(super::MusicAnalyzeRunResult::Finished);
                return;
            }
            let analyzed = analyze_music(&model_dir, &loaded, &cancel_flag, &mut send_progress);
            let item = match analyzed {
                Ok(MusicAnalyzeOutput {
                    result,
                    source_len_samples,
                }) => super::MusicAnalyzeItemResult {
                    path,
                    result: Some(result),
                    source_len_samples,
                    source_kind,
                    stems: Some(loaded),
                    error: None,
                },
                Err(err) => {
                    if cancel_flag.load(Ordering::Relaxed) || is_cancel_error(&err) {
                        let _ = tx.send(super::MusicAnalyzeRunResult::Finished);
                        return;
                    }
                    super::MusicAnalyzeItemResult {
                        path,
                        result: None,
                        source_len_samples: loaded.len_samples(),
                        source_kind,
                        stems: Some(loaded),
                        error: Some(err),
                    }
                }
            };
            let _ = tx.send(super::MusicAnalyzeRunResult::Item(item));
            let _ = tx.send(super::MusicAnalyzeRunResult::Finished);
        });

        if let Some(tab_mut) = self.tabs.get_mut(tab_idx) {
            tab_mut.music_analysis_draft.analysis_inflight = true;
            tab_mut.music_analysis_draft.last_error = None;
            tab_mut.music_analysis_draft.analysis_source_kind = source_kind;
            tab_mut.music_analysis_draft.preview_peak_abs = 0.0;
            tab_mut.music_analysis_draft.preview_clip_applied = false;
            tab_mut.music_analysis_draft.preview_inflight = false;
            tab_mut.music_analysis_draft.preview_error = None;
            tab_mut.music_analysis_draft.analysis_process_message = "Queued".to_string();
        }

        let mut pending = HashSet::new();
        pending.insert(state_path.clone());
        self.music_ai_state = Some(super::MusicAnalyzeRunState {
            started_at: std::time::Instant::now(),
            total: 1,
            done: 0,
            pending,
            cancel_requested,
            current_step: "Queued".to_string(),
            rx,
        });
        self.music_ai_inflight.insert(state_path);
        self.music_ai_last_error = None;
    }

    pub(super) fn cancel_music_analysis_run(&mut self) {
        if let Some(state) = &self.music_ai_state {
            state.cancel_requested.store(true, Ordering::Relaxed);
            self.debug_log("music analyze cancel requested".to_string());
            for tab in self.tabs.iter_mut() {
                if tab.music_analysis_draft.analysis_inflight {
                    tab.music_analysis_draft.analysis_process_message = "Canceling...".to_string();
                }
            }
        }
    }

    pub(super) fn drain_music_model_download_results(&mut self, ctx: &egui::Context) {
        let Some(_) = &self.music_model_download_state else {
            return;
        };
        let mut finished: Option<super::MusicModelDownloadResult> = None;
        if let Some(state) = self.music_model_download_state.as_mut() {
            while let Ok(event) = state.rx.try_recv() {
                match event {
                    super::MusicModelDownloadEvent::Progress { done, total } => {
                        let (next_done, next_total) =
                            fold_download_progress(state.done, state.total, done, total);
                        state.done = next_done;
                        state.total = next_total;
                    }
                    super::MusicModelDownloadEvent::Finished(result) => {
                        finished = Some(result);
                    }
                }
            }
        }
        if let Some(result) = finished {
            self.music_model_download_state = None;
            if let Some(err) = result.error {
                self.music_ai_last_error = Some(err.clone());
                self.debug_log(format!("music model download failed: {err}"));
            } else if let Some(dir) = result.model_dir {
                self.debug_log(format!("music model ready: {}", dir.display()));
                self.music_ai_model_dir = Some(dir);
                self.music_ai_last_error = None;
            }
            self.refresh_music_ai_status();
        }
        if self.music_model_download_state.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        } else {
            ctx.request_repaint();
        }
    }

    pub(super) fn drain_music_ai_results(&mut self, ctx: &egui::Context) {
        let Some(_) = &self.music_ai_state else {
            return;
        };
        let mut started = Vec::new();
        let mut progress = Vec::new();
        let mut items = Vec::new();
        let mut finished = false;
        let mut queue_backlog = false;
        if let Some(state) = &self.music_ai_state {
            const MAX_DRAIN_PER_FRAME: usize = 64;
            let mut drained = 0usize;
            while drained < MAX_DRAIN_PER_FRAME {
                let Ok(msg) = state.rx.try_recv() else {
                    break;
                };
                drained += 1;
                match msg {
                    super::MusicAnalyzeRunResult::Started(path) => started.push(path),
                    super::MusicAnalyzeRunResult::Progress { path, message } => {
                        progress.push((path, message))
                    }
                    super::MusicAnalyzeRunResult::Item(item) => items.push(item),
                    super::MusicAnalyzeRunResult::Finished => finished = true,
                }
            }
            queue_backlog = drained >= MAX_DRAIN_PER_FRAME;
        }

        for path in started {
            if let Some(state) = self.music_ai_state.as_mut() {
                state.pending.remove(&path);
            }
        }

        let had_progress = !progress.is_empty();
        for (path, message) in progress {
            if let Some(state) = self.music_ai_state.as_mut() {
                state.current_step = message.clone();
            }
            if let Some(tab_idx) = self.tabs.iter().position(|t| t.path == path) {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.music_analysis_draft.analysis_process_message = message;
                }
            }
        }

        for item in items {
            self.music_ai_inflight.remove(&item.path);
            if let Some(state) = self.music_ai_state.as_mut() {
                state.done = state.done.saturating_add(1).min(state.total);
            }
            let Some(tab_idx) = self.tabs.iter().position(|t| t.path == item.path) else {
                continue;
            };
            let estimated_bpm = item
                .result
                .as_ref()
                .and_then(|r| r.estimated_bpm)
                .filter(|v| v.is_finite() && *v > 0.0);
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.music_analysis_draft.analysis_inflight = false;
                tab.music_analysis_draft.analysis_source_len = item.source_len_samples;
                tab.music_analysis_draft.analysis_source_kind = item.source_kind;
                tab.music_analysis_draft.analysis_process_message = estimated_bpm
                    .map(|v| format!("Done (BPM {v:.2})"))
                    .unwrap_or_else(|| "Done".to_string());
                if let Some(stems) = item.stems {
                    tab.music_analysis_draft.stems_audio = Some(Arc::new(stems));
                }
                if let Some(err) = item.error {
                    tab.music_analysis_draft.last_error = Some(err.clone());
                    self.music_ai_last_error = Some(err);
                    self.discard_music_provisional_markers(tab_idx);
                    continue;
                }
                tab.music_analysis_draft.last_error = None;
                tab.music_analysis_draft.preview_error = None;
                tab.music_analysis_draft.result = item.result;
                if let Some(bpm) = estimated_bpm {
                    if !tab.bpm_user_set && tab.bpm_value <= 0.0 {
                        tab.bpm_value = bpm;
                    }
                }
                self.rebuild_music_provisional_markers_for_tab(tab_idx);
            }
            if let Some(bpm) = estimated_bpm {
                if let Some(media) = self.item_for_path_mut(&item.path) {
                    if let Some(meta) = media.meta.as_mut() {
                        if meta.bpm.unwrap_or(0.0) <= 0.0 {
                            meta.bpm = Some(bpm);
                        }
                    }
                }
            }
        }

        if finished {
            let canceled = self
                .music_ai_state
                .as_ref()
                .map(|s| s.cancel_requested.load(Ordering::Relaxed))
                .unwrap_or(false);
            if canceled {
                self.music_ai_last_error = Some("Music analysis canceled.".to_string());
            }
            for tab in self.tabs.iter_mut() {
                if self.music_ai_inflight.contains(&tab.path)
                    || tab.music_analysis_draft.analysis_inflight
                {
                    tab.music_analysis_draft.analysis_inflight = false;
                    if canceled {
                        tab.music_analysis_draft.analysis_process_message =
                            "Canceled by user".to_string();
                    }
                }
            }
            self.music_ai_state = None;
            self.music_ai_inflight.clear();
            ctx.request_repaint();
            return;
        }
        if had_progress {
            ctx.request_repaint();
            return;
        }
        if queue_backlog {
            ctx.request_repaint();
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    pub(super) fn rebuild_music_provisional_markers_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let source_len = tab.music_analysis_draft.analysis_source_len.max(1);
        let target_len = tab.samples_len.max(1);

        let mut provisional = Vec::<MarkerEntry>::new();
        if let Some(result) = tab.music_analysis_draft.result.clone() {
            if tab.music_analysis_draft.show_beat {
                for pos in result.beats {
                    provisional.push(MarkerEntry {
                        sample: remap_sample(pos, source_len, target_len),
                        label: "AI_B".to_string(),
                    });
                }
            }
            if tab.music_analysis_draft.show_downbeat {
                for pos in result.downbeats {
                    provisional.push(MarkerEntry {
                        sample: remap_sample(pos, source_len, target_len),
                        label: "AI_D".to_string(),
                    });
                }
            }
            if tab.music_analysis_draft.show_section {
                for (pos, name) in result.sections {
                    provisional.push(MarkerEntry {
                        sample: remap_sample(pos, source_len, target_len),
                        label: format!("AI_S:{name}"),
                    });
                }
            }
        }

        provisional.sort_by(|a, b| match a.sample.cmp(&b.sample) {
            std::cmp::Ordering::Equal => a.label.cmp(&b.label),
            other => other,
        });
        provisional.dedup_by(|a, b| a.sample == b.sample && a.label == b.label);

        tab.music_analysis_draft.provisional_markers = provisional.clone();

        let mut merged: Vec<MarkerEntry> = tab
            .markers_committed
            .iter()
            .filter(|m| !is_ai_marker_label(m.label.as_str()))
            .cloned()
            .collect();
        merged.extend(provisional);
        merged.sort_by(|a, b| match a.sample.cmp(&b.sample) {
            std::cmp::Ordering::Equal => a.label.cmp(&b.label),
            other => other,
        });
        merged.dedup_by(|a, b| a.sample == b.sample && a.label == b.label);

        tab.markers = merged;
        tab.markers_dirty = tab.markers_committed != tab.markers_saved;
    }

    pub(super) fn discard_music_provisional_markers(&mut self, tab_idx: usize) {
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.music_analysis_draft.provisional_markers.clear();
            tab.markers = tab.markers_committed.clone();
            tab.markers_dirty = tab.markers_committed != tab.markers_saved;
        }
    }

    pub(super) fn apply_music_analysis_markers_to_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        let mut merged: Vec<MarkerEntry> = tab
            .markers_committed
            .iter()
            .filter(|m| !is_ai_marker_label(m.label.as_str()))
            .cloned()
            .collect();
        merged.extend(tab.music_analysis_draft.provisional_markers.clone());
        merged.sort_by(|a, b| match a.sample.cmp(&b.sample) {
            std::cmp::Ordering::Equal => a.label.cmp(&b.label),
            other => other,
        });
        merged.dedup_by(|a, b| a.sample == b.sample && a.label == b.label);
        tab.markers = merged.clone();
        tab.markers_committed = merged.clone();
        tab.markers_applied = merged;
        tab.markers_dirty = tab.markers_committed != tab.markers_saved;
    }

    pub(super) fn apply_music_preview_to_tab(&mut self, tab_idx: usize) {
        let (channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            if tab.music_analysis_draft.preview_inflight {
                return;
            }
            let Some(overlay) = tab.preview_overlay.as_ref() else {
                tab.music_analysis_draft.preview_error =
                    Some("preview audio is not ready".to_string());
                return;
            };
            if overlay.channels.is_empty() {
                tab.music_analysis_draft.preview_error = Some("preview audio is empty".to_string());
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let channels = overlay.channels.clone();
            let samples_len = channels
                .iter()
                .map(|c| c.len())
                .max()
                .unwrap_or(tab.samples_len);
            tab.ch_samples = channels.clone();
            tab.buffer_sample_rate = self.audio.shared.out_sample_rate.max(1);
            tab.samples_len = samples_len;
            let (waveform_minmax, waveform_pyramid) =
                Self::build_editor_waveform_cache(&tab.ch_samples, tab.samples_len);
            tab.waveform_minmax = waveform_minmax;
            tab.waveform_pyramid = waveform_pyramid;
            tab.dirty = true;
            tab.music_analysis_draft.preview_active = false;
            tab.music_analysis_draft.preview_error = None;
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
            (channels, undo_state)
        };

        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.cancel_music_preview_run();
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        self.on_audio_length_changed(tab_idx);
        if let Some((path, buffer_sr)) = self
            .tabs
            .get(tab_idx)
            .map(|tab| (tab.path.clone(), tab.buffer_sample_rate.max(1)))
        {
            self.playback_mark_source(
                crate::app::PlaybackSourceKind::EditorTab(path),
                buffer_sr,
            );
            if let Some(tab) = self.tabs.get(tab_idx) {
                self.apply_loop_mode_for_tab(tab);
            }
        }
    }

    pub(super) fn apply_music_preview_mix_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let Some(stems) = tab.music_analysis_draft.stems_audio.clone() else {
            return;
        };
        let tab_path = tab.path.clone();
        let gains = tab.music_analysis_draft.preview_gains_db;
        let selection_only = tab.music_analysis_draft.preview_selection_only;
        let selection = tab.selection;
        let base_channels = tab.ch_samples.clone();
        let samples_len = tab.samples_len;
        self.cancel_music_preview_run();
        self.music_preview_generation_counter =
            self.music_preview_generation_counter.wrapping_add(1);
        let generation = self.music_preview_generation_counter;
        self.music_preview_expected_generation = generation;
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let cancel_thread = Arc::clone(&cancel_requested);
        let tab_path_worker = tab_path.clone();
        let (tx, rx) = std::sync::mpsc::channel::<super::MusicPreviewResult>();
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            if cancel_thread.load(Ordering::Relaxed) {
                return;
            }
            let mixed =
                mix_stems_with_gains(stems.as_ref(), gains, base_channels.len(), samples_len);
            if mixed.is_empty() {
                let _ = tx.send(super::MusicPreviewResult {
                    tab_path: tab_path_worker.clone(),
                    generation,
                    overlay: None,
                    mono: None,
                    peak_abs: 0.0,
                    clip_applied: false,
                    error: Some("preview mix produced empty output".to_string()),
                });
                return;
            }

            let mut overlay = if selection_only {
                base_channels
            } else {
                mixed.clone()
            };
            if selection_only {
                if let Some((a0, b0)) = selection {
                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                    for (ch_idx, ch) in overlay.iter_mut().enumerate() {
                        if let Some(src) = mixed.get(ch_idx) {
                            let end = b.min(ch.len()).min(src.len());
                            let start = a.min(end);
                            ch[start..end].copy_from_slice(&src[start..end]);
                        }
                    }
                }
            }

            let (peak_abs, clip_applied) = sanitize_and_clip_channels(&mut overlay);
            if cancel_thread.load(Ordering::Relaxed) {
                return;
            }
            let overlay_len = overlay.first().map(|c| c.len()).unwrap_or(samples_len);
            let mono = Self::mixdown_channels(&overlay, samples_len.max(overlay_len));
            if mono.is_empty() {
                let _ = tx.send(super::MusicPreviewResult {
                    tab_path: tab_path_worker.clone(),
                    generation,
                    overlay: None,
                    mono: None,
                    peak_abs,
                    clip_applied,
                    error: Some("preview mono mix is empty".to_string()),
                });
                return;
            }
            let _ = tx.send(super::MusicPreviewResult {
                tab_path: tab_path_worker,
                generation,
                overlay: Some(overlay),
                mono: Some(mono),
                peak_abs,
                clip_applied,
                error: None,
            });
        });
        if let Some(tab_mut) = self.tabs.get_mut(tab_idx) {
            tab_mut.music_analysis_draft.preview_inflight = true;
            tab_mut.music_analysis_draft.preview_generation = generation;
            tab_mut.music_analysis_draft.preview_error = None;
        }
        self.music_preview_state = Some(super::MusicPreviewRunState {
            started_at: std::time::Instant::now(),
            tab_path: tab_path.clone(),
            generation,
            cancel_requested,
            rx,
        });
    }

    pub(super) fn cancel_music_preview_run(&mut self) {
        let tab_path = self
            .music_preview_state
            .as_ref()
            .map(|state| state.tab_path.clone());
        if let Some(state) = &self.music_preview_state {
            state.cancel_requested.store(true, Ordering::Relaxed);
        }
        self.music_preview_state = None;
        if let Some(path) = tab_path {
            if let Some(tab_idx) = self.tabs.iter().position(|t| t.path == path) {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.music_analysis_draft.preview_inflight = false;
                }
            }
        }
    }

    pub(super) fn cancel_music_preview_if_path(&mut self, path: &std::path::Path) {
        let should_cancel = self
            .music_preview_state
            .as_ref()
            .map(|state| state.tab_path.as_path() == path)
            .unwrap_or(false);
        if should_cancel {
            self.cancel_music_preview_run();
        }
    }

    pub(super) fn drain_music_preview_results(&mut self, ctx: &egui::Context) {
        let Some(state) = &self.music_preview_state else {
            return;
        };
        match state.rx.try_recv() {
            Ok(result) => {
                let completed_path = result.tab_path.clone();
                let state_generation = state.generation;
                let result_generation = result.generation;
                let _elapsed = state.started_at.elapsed();
                self.music_preview_state = None;

                if result_generation != self.music_preview_expected_generation
                    || result_generation != state_generation
                {
                    ctx.request_repaint();
                    return;
                }

                let Some(tab_idx) = self.tabs.iter().position(|t| t.path == completed_path) else {
                    ctx.request_repaint();
                    return;
                };
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.music_analysis_draft.preview_inflight = false;
                    tab.music_analysis_draft.preview_generation = result_generation;
                }
                if let Some(err) = result.error {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.music_analysis_draft.preview_error = Some(err);
                        tab.music_analysis_draft.preview_active = false;
                    }
                    ctx.request_repaint();
                    return;
                }
                let (Some(overlay), Some(mono)) = (result.overlay, result.mono) else {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.music_analysis_draft.preview_error =
                            Some("preview result missing audio".to_string());
                        tab.music_analysis_draft.preview_active = false;
                    }
                    ctx.request_repaint();
                    return;
                };
                let timeline_len = overlay
                    .first()
                    .map(|c| c.len())
                    .unwrap_or_else(|| self.tabs[tab_idx].samples_len)
                    .max(1);
                if let Some(tab_mut) = self.tabs.get_mut(tab_idx) {
                    tab_mut.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::MusicAnalyze,
                        timeline_len,
                    ));
                    tab_mut.music_analysis_draft.preview_active = true;
                    tab_mut.music_analysis_draft.preview_peak_abs = result.peak_abs;
                    tab_mut.music_analysis_draft.preview_clip_applied = result.clip_applied;
                    tab_mut.music_analysis_draft.preview_error = None;
                }
                self.set_preview_mono(tab_idx, ToolKind::MusicAnalyze, mono);
                ctx.request_repaint();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let path = state.tab_path.clone();
                self.music_preview_state = None;
                if let Some(tab_idx) = self.tabs.iter().position(|t| t.path == path) {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.music_analysis_draft.preview_inflight = false;
                        tab.music_analysis_draft.preview_active = false;
                        tab.music_analysis_draft.preview_error =
                            Some("preview worker disconnected".to_string());
                    }
                }
                ctx.request_repaint();
            }
        }
    }

    pub(super) fn enforce_music_stem_cache_policy(&mut self) {
        let active_idx = self.active_tab;
        for (idx, tab) in self.tabs.iter_mut().enumerate() {
            let keep_stems = active_idx == Some(idx) && tab.active_tool == ToolKind::MusicAnalyze;
            if !keep_stems {
                tab.music_analysis_draft.stems_audio = None;
                if tab.active_tool != ToolKind::MusicAnalyze {
                    tab.music_analysis_draft.preview_active = false;
                    tab.music_analysis_draft.preview_inflight = false;
                }
            }
        }

        if let Some(state) = &self.music_preview_state {
            let cancel = active_idx
                .and_then(|idx| self.tabs.get(idx))
                .map(|tab| tab.path != state.tab_path || tab.active_tool != ToolKind::MusicAnalyze)
                .unwrap_or(true);
            if cancel {
                self.cancel_music_preview_run();
            }
        }
    }

    pub(super) fn music_analysis_status_text(&self) -> Option<String> {
        let state = self.music_ai_state.as_ref()?;
        let elapsed = state.started_at.elapsed().as_secs_f32();
        Some(format!(
            "Music Analyze: {}/{} ({:.1}s) {}",
            state.done, state.total, elapsed, state.current_step
        ))
    }

    pub(super) fn music_analysis_process_text(&self) -> Option<String> {
        self.music_ai_state
            .as_ref()
            .map(|state| state.current_step.clone())
    }
}

fn is_ai_marker_label(label: &str) -> bool {
    label.starts_with("AI_B") || label.starts_with("AI_D") || label.starts_with("AI_S:")
}

fn music_progress_phase(message: &str) -> &'static str {
    let msg = message.trim().to_ascii_lowercase();
    if msg.contains("queued") {
        return "Queued";
    }
    if msg.contains("demucs") {
        return "Demucs";
    }
    if msg.contains("loading stems")
        || msg.contains("decoding source")
        || msg.contains("loading input")
    {
        return "LoadingInput";
    }
    if msg.contains("resolve model") || msg.contains("resolving model") {
        return "ResolveModel";
    }
    if msg.contains("spectrogram") {
        return "BuildSpectrogram";
    }
    if msg.contains("onnx inference") || msg.contains("inference") {
        return "Inference";
    }
    if msg.contains("postprocess") || msg.contains("postprocessing") {
        return "Postprocess";
    }
    if msg.contains("done") || msg.contains("finished") {
        return "Done";
    }
    "LoadingInput"
}

fn remap_sample(pos: usize, src_len: usize, dst_len: usize) -> usize {
    if src_len <= 1 || dst_len <= 1 {
        return 0;
    }
    (((pos as f64) * (dst_len as f64) / (src_len as f64)).round() as usize)
        .min(dst_len.saturating_sub(1))
}

fn gain_to_amp(db: f32) -> f32 {
    if !db.is_finite() || db <= -80.0 {
        0.0
    } else {
        (10.0f32).powf(db / 20.0)
    }
}

fn mix_stems_with_gains(
    stems: &MusicStemSet,
    gains: StemGainsDb,
    target_channels: usize,
    target_len: usize,
) -> Vec<Vec<f32>> {
    let target_channels = target_channels.max(1);
    let target_len = target_len.max(stems.len_samples()).max(1);
    let mut out = vec![vec![0.0f32; target_len]; target_channels];

    let stem_defs = [
        (&stems.bass, gains.bass),
        (&stems.drums, gains.drums),
        (&stems.other, gains.other),
        (&stems.vocals, gains.vocals),
    ];

    for (channels, gain_db) in stem_defs {
        let amp = gain_to_amp(gain_db);
        if channels.is_empty() || amp == 0.0 {
            continue;
        }
        for ch_idx in 0..target_channels {
            let src = &channels[ch_idx % channels.len()];
            let dst = &mut out[ch_idx];
            let n = src.len().min(dst.len());
            for i in 0..n {
                dst[i] += src[i] * amp;
            }
        }
    }
    out
}

fn sanitize_and_clip_channels(channels: &mut [Vec<f32>]) -> (f32, bool) {
    let mut peak_abs = 0.0f32;
    for ch in channels.iter_mut() {
        for sample in ch.iter_mut() {
            if !sample.is_finite() {
                *sample = 0.0;
            }
            peak_abs = peak_abs.max(sample.abs());
        }
    }
    let clip_applied = peak_abs > 1.0;
    if clip_applied {
        let scale = 0.999 / peak_abs.max(1.0e-12);
        for ch in channels.iter_mut() {
            for sample in ch.iter_mut() {
                *sample *= scale;
            }
        }
    }
    (peak_abs, clip_applied)
}

#[cfg(test)]
mod tests {
    use super::{
        fold_download_progress, gain_to_amp, is_ai_marker_label, mix_stems_with_gains,
        music_progress_phase, remap_sample, sanitize_and_clip_channels,
    };
    use crate::app::types::{MusicStemSet, StemGainsDb};

    #[test]
    fn ai_marker_label_detects_prefix() {
        assert!(is_ai_marker_label("AI_B"));
        assert!(is_ai_marker_label("AI_D"));
        assert!(is_ai_marker_label("AI_S:Verse"));
        assert!(!is_ai_marker_label("A1"));
    }

    #[test]
    fn remap_sample_bounds() {
        assert_eq!(remap_sample(0, 100, 200), 0);
        assert_eq!(remap_sample(99, 100, 200), 198);
        assert_eq!(remap_sample(120, 100, 200), 199);
    }

    #[test]
    fn mix_stems_applies_per_stem_gains() {
        let stems = MusicStemSet {
            sample_rate: 44_100,
            bass: vec![vec![1.0, 1.0]],
            drums: vec![vec![0.5, 0.5]],
            other: vec![vec![0.25, 0.25]],
            vocals: vec![vec![0.0, 0.0]],
        };
        let gains = StemGainsDb {
            bass: -6.0,
            drums: 0.0,
            other: -12.0,
            vocals: 0.0,
        };
        let out = mix_stems_with_gains(&stems, gains, 1, 2);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), 2);
        let expected = (1.0 * gain_to_amp(-6.0)) + 0.5 + (0.25 * gain_to_amp(-12.0));
        assert!((out[0][0] - expected).abs() < 1.0e-4);
        assert!((out[0][1] - expected).abs() < 1.0e-4);
    }

    #[test]
    fn sanitize_and_clip_channels_handles_non_finite_and_clips() {
        let mut ch = vec![vec![0.0, f32::NAN, 4.0], vec![f32::INFINITY, -2.0, 0.5]];
        let (peak, clipped) = sanitize_and_clip_channels(&mut ch);
        assert!(clipped);
        assert!(peak > 1.0);
        for lane in &ch {
            for &v in lane {
                assert!(v.is_finite());
                assert!(v.abs() <= 0.999_001);
            }
        }
    }

    #[test]
    fn gain_to_amp_minus_80_and_below_is_silent() {
        assert_eq!(gain_to_amp(-80.0), 0.0);
        assert_eq!(gain_to_amp(-96.0), 0.0);
        assert_eq!(gain_to_amp(f32::NEG_INFINITY), 0.0);
        assert!(gain_to_amp(-79.9) > 0.0);
    }

    #[test]
    fn progress_message_maps_to_phase() {
        assert_eq!(
            music_progress_phase("Analyze: resolving model files..."),
            "ResolveModel"
        );
        assert_eq!(
            music_progress_phase("Analyze: ONNX inference 1/4"),
            "Inference"
        );
        assert_eq!(
            music_progress_phase("Demucs: separating stems..."),
            "Demucs"
        );
        assert_eq!(
            music_progress_phase("Analyze: postprocessing..."),
            "Postprocess"
        );
    }

    #[test]
    fn download_progress_fold_is_monotonic() {
        let mut done = 0usize;
        let mut total = 1usize;
        for (next_done, next_total) in [(0, 4), (2, 4), (1, 4), (3, 4), (4, 4)] {
            (done, total) = fold_download_progress(done, total, next_done, next_total);
        }
        assert_eq!(total, 4);
        assert_eq!(done, 4);
    }
}
