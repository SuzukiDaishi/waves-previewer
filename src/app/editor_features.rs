use std::path::{Path, PathBuf};

use super::types::{
    AnalysisProgress, EditorAnalysisKey, EditorAnalysisKind, EditorFeatureAnalysisData,
    EditorFeatureAnalysisJobMsg, ViewMode,
};

impl super::WavesPreviewer {
    fn ensure_feature_analysis_channel(&mut self) {
        if self.editor_feature_tx.is_none() || self.editor_feature_rx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<EditorFeatureAnalysisJobMsg>();
            self.editor_feature_tx = Some(tx);
            self.editor_feature_rx = Some(rx);
        }
    }

    fn bump_feature_analysis_generation(&mut self, key: &EditorAnalysisKey) -> u64 {
        self.editor_feature_generation_counter =
            self.editor_feature_generation_counter.wrapping_add(1);
        let generation = self.editor_feature_generation_counter;
        self.editor_feature_generation
            .insert(key.clone(), generation);
        generation
    }

    fn queue_tempogram_data(
        &mut self,
        path: PathBuf,
        mono: Vec<f32>,
        sample_rate: u32,
        generation: u64,
    ) {
        self.ensure_feature_analysis_channel();
        let Some(tx) = self.editor_feature_tx.as_ref().cloned() else {
            return;
        };
        let key = EditorAnalysisKey {
            path: path.clone(),
            kind: EditorAnalysisKind::Tempogram,
        };
        let cancel = self
            .editor_feature_cancel
            .get(&key)
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let cfg = self.spectro_cfg.clone();
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let data =
                crate::app::render::music_features::compute_tempogram(&mono, sample_rate, &cfg);
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let _ = tx.send(EditorFeatureAnalysisJobMsg::TempogramDone {
                path,
                generation,
                data,
            });
        });
    }

    fn queue_chromagram_data(
        &mut self,
        path: PathBuf,
        mono: Vec<f32>,
        sample_rate: u32,
        generation: u64,
    ) {
        self.ensure_feature_analysis_channel();
        let Some(tx) = self.editor_feature_tx.as_ref().cloned() else {
            return;
        };
        let key = EditorAnalysisKey {
            path: path.clone(),
            kind: EditorAnalysisKind::Chromagram,
        };
        let cancel = self
            .editor_feature_cancel
            .get(&key)
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        let cfg = self.spectro_cfg.clone();
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let data =
                crate::app::render::music_features::compute_chromagram(&mono, sample_rate, &cfg);
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let _ = tx.send(EditorFeatureAnalysisJobMsg::ChromagramDone {
                path,
                generation,
                data,
            });
        });
    }

    pub(super) fn queue_feature_analysis_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let kind = match tab.leaf_view_mode() {
            ViewMode::Tempogram => EditorAnalysisKind::Tempogram,
            ViewMode::Chromagram => EditorAnalysisKind::Chromagram,
            _ => return,
        };
        let key = EditorAnalysisKey {
            path: tab.path.clone(),
            kind,
        };
        if self.editor_feature_cache.contains_key(&key)
            || self.editor_feature_inflight.contains(&key)
        {
            return;
        }
        let mono = super::WavesPreviewer::mixdown_channels(&tab.ch_samples, tab.samples_len);
        let sample_rate = tab.buffer_sample_rate.max(1);
        self.editor_feature_progress.insert(
            key.clone(),
            AnalysisProgress {
                done_units: 0,
                total_units: 1,
                started_at: std::time::Instant::now(),
            },
        );
        self.editor_feature_cancel.insert(
            key.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let generation = self.bump_feature_analysis_generation(&key);
        self.editor_feature_inflight.insert(key.clone());
        match kind {
            EditorAnalysisKind::Tempogram => {
                self.queue_tempogram_data(key.path, mono, sample_rate, generation);
            }
            EditorAnalysisKind::Chromagram => {
                self.queue_chromagram_data(key.path, mono, sample_rate, generation);
            }
            EditorAnalysisKind::Spectrogram => {}
        }
    }

    pub(super) fn queue_editor_analysis_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        match tab.leaf_view_mode() {
            ViewMode::Waveform => {}
            ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => {
                self.queue_spectrogram_for_tab(tab_idx);
            }
            ViewMode::Tempogram | ViewMode::Chromagram => {
                self.queue_feature_analysis_for_tab(tab_idx);
            }
        }
    }

    pub(super) fn apply_feature_analysis_updates(&mut self, ctx: &egui::Context) {
        let mut messages = Vec::new();
        if let Some(rx) = &self.editor_feature_rx {
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }
        for msg in messages {
            match msg {
                EditorFeatureAnalysisJobMsg::TempogramDone {
                    path,
                    generation,
                    data,
                } => {
                    let key = EditorAnalysisKey {
                        path,
                        kind: EditorAnalysisKind::Tempogram,
                    };
                    self.finish_feature_analysis(
                        key,
                        generation,
                        EditorFeatureAnalysisData::Tempogram(data),
                    );
                }
                EditorFeatureAnalysisJobMsg::ChromagramDone {
                    path,
                    generation,
                    data,
                } => {
                    let key = EditorAnalysisKey {
                        path,
                        kind: EditorAnalysisKind::Chromagram,
                    };
                    self.finish_feature_analysis(
                        key,
                        generation,
                        EditorFeatureAnalysisData::Chromagram(data),
                    );
                }
            }
            ctx.request_repaint();
        }
    }

    fn finish_feature_analysis(
        &mut self,
        key: EditorAnalysisKey,
        generation: u64,
        data: EditorFeatureAnalysisData,
    ) {
        if self.editor_feature_generation.get(&key).copied() != Some(generation) {
            if self.debug.cfg.enabled {
                self.debug_log(format!(
                    "feature_analysis_drop_stale kind={:?} path={} gen={} expected={:?}",
                    key.kind,
                    key.path.display(),
                    generation,
                    self.editor_feature_generation.get(&key).copied()
                ));
            }
            return;
        }
        self.editor_feature_cache
            .insert(key.clone(), std::sync::Arc::new(data));
        self.editor_feature_inflight.remove(&key);
        if let Some(progress) = self.editor_feature_progress.get_mut(&key) {
            progress.done_units = progress.total_units;
        }
        self.editor_feature_progress.remove(&key);
        self.editor_feature_cancel.remove(&key);
    }

    pub(super) fn cancel_feature_analysis_for_key(&mut self, key: &EditorAnalysisKey) {
        if let Some(flag) = self.editor_feature_cancel.remove(key) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.editor_feature_inflight.remove(key);
        self.editor_feature_progress.remove(key);
        self.editor_feature_generation.remove(key);
        self.editor_feature_cache.remove(key);
    }

    pub(super) fn cancel_feature_analysis_for_path(&mut self, path: &Path) {
        let keys: Vec<EditorAnalysisKey> = self
            .editor_feature_cache
            .keys()
            .chain(self.editor_feature_inflight.iter())
            .filter(|key| key.path.as_path() == path)
            .cloned()
            .collect();
        for key in keys {
            self.cancel_feature_analysis_for_key(&key);
        }
    }

    pub(super) fn cancel_all_feature_analysis(&mut self) {
        let keys: Vec<EditorAnalysisKey> = self
            .editor_feature_cache
            .keys()
            .chain(self.editor_feature_inflight.iter())
            .cloned()
            .collect();
        for key in keys {
            self.cancel_feature_analysis_for_key(&key);
        }
    }

    pub(super) fn reset_all_feature_analysis_state(&mut self) {
        self.cancel_all_feature_analysis();
        self.editor_feature_generation_counter = 0;
    }

    pub(super) fn total_editor_analysis_progress(&self) -> (usize, usize) {
        let mut done = 0usize;
        let mut total = 0usize;
        for progress in self.spectro_progress.values() {
            done = done.saturating_add(progress.done_tiles);
            total = total.saturating_add(progress.total_tiles);
        }
        for progress in self.editor_feature_progress.values() {
            done = done.saturating_add(progress.done_units);
            total = total.saturating_add(progress.total_units);
        }
        (done, total)
    }

    pub(super) fn total_editor_analysis_inflight(&self) -> usize {
        self.spectro_inflight.len() + self.editor_feature_inflight.len()
    }

    pub(super) fn cancel_all_editor_analyses(&mut self) {
        self.cancel_all_spectrograms();
        self.cancel_all_feature_analysis();
    }
}
