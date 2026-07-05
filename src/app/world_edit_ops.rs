//! WORLD F0 edit draft helpers: building the editable curve from the
//! cached analysis, applying curve transforms (shift / smooth / flatten),
//! and pencil edits from the World-view canvas. The resynthesis job that
//! consumes the draft lives here too.

use super::types::{
    EditorAnalysisKey, EditorAnalysisKind, EditorApplyResult, EditorApplyState, EditorTab,
    EditorFeatureAnalysisData, WorldF0Draft, WorldFeatureData,
};

/// Lowest F0 the editor lets you paint, in Hz.
pub(super) const WORLD_EDIT_MIN_F0_HZ: f32 = 30.0;
/// Highest F0 the editor lets you paint, in Hz.
pub(super) const WORLD_EDIT_MAX_F0_HZ: f32 = 1_600.0;

impl super::WavesPreviewer {
    /// The editable F0 draft for `tab`, (re)built from `data` when missing
    /// or when the analysis frame count changed under it.
    pub(super) fn world_f0_draft_mut<'a>(
        tab: &'a mut EditorTab,
        data: &WorldFeatureData,
    ) -> &'a mut WorldF0Draft {
        let stale = tab
            .world_f0_draft
            .as_ref()
            .map(|draft| draft.source_frames != data.frames)
            .unwrap_or(true);
        if stale {
            tab.world_f0_draft = Some(WorldF0Draft {
                values: data.f0_values.clone(),
                source_frames: data.frames,
                ..Default::default()
            });
        }
        tab.world_f0_draft.as_mut().expect("draft just ensured")
    }

    /// Multiply every voiced frame by the given semitone offset.
    pub(super) fn world_f0_shift_semitones(draft: &mut WorldF0Draft, semitones: f32) {
        if semitones == 0.0 {
            return;
        }
        let ratio = 2.0f32.powf(semitones / 12.0);
        for value in draft.values.iter_mut() {
            if *value > 0.0 {
                *value = (*value * ratio).clamp(WORLD_EDIT_MIN_F0_HZ, WORLD_EDIT_MAX_F0_HZ);
            }
        }
        draft.dirty = true;
    }

    /// 5-point median smoothing over voiced runs (unvoiced gaps stay put
    /// and are not smeared across).
    pub(super) fn world_f0_smooth(draft: &mut WorldF0Draft) {
        let n = draft.values.len();
        if n < 3 {
            return;
        }
        let src = draft.values.clone();
        let mut window = [0.0f32; 5];
        for i in 0..n {
            if src[i] <= 0.0 {
                continue;
            }
            let mut count = 0;
            for j in i.saturating_sub(2)..(i + 3).min(n) {
                if src[j] > 0.0 {
                    window[count] = src[j];
                    count += 1;
                }
            }
            if count >= 3 {
                window[..count].sort_by(f32::total_cmp);
                draft.values[i] = window[count / 2];
            }
        }
        draft.dirty = true;
    }

    /// Set every voiced frame to the median voiced F0 (monotone/robot).
    pub(super) fn world_f0_flatten(draft: &mut WorldF0Draft) {
        let mut voiced: Vec<f32> = draft.values.iter().copied().filter(|v| *v > 0.0).collect();
        if voiced.is_empty() {
            return;
        }
        voiced.sort_by(f32::total_cmp);
        let median = voiced[voiced.len() / 2];
        for value in draft.values.iter_mut() {
            if *value > 0.0 {
                *value = median;
            }
        }
        draft.dirty = true;
    }

    /// Restore the analyzed curve.
    pub(super) fn world_f0_reset(draft: &mut WorldF0Draft, data: &WorldFeatureData) {
        draft.values = data.f0_values.clone();
        draft.source_frames = data.frames;
        draft.dirty = false;
        draft.last_drag_frame = None;
    }

    /// Kick a background WORLD resynthesis of the tab audio using the
    /// edited F0 draft (or the analyzed curve when no draft exists). The
    /// job feeds the shared editor-apply pipeline, which handles undo,
    /// the busy overlay + cancel, engine buffer swap, and cache
    /// invalidation; the resynthesized mono is written to every channel
    /// so the tab keeps its channel count.
    pub(super) fn spawn_world_resynth_for_tab(&mut self, tab_idx: usize) {
        if self.editor_apply_state.is_some() {
            return;
        }
        let key;
        let f0;
        let out_len;
        let n_ch;
        let frame_period_ms;
        let undo;
        {
            let Some(tab) = self.tabs.get(tab_idx) else {
                return;
            };
            key = EditorAnalysisKey {
                path: tab.path.clone(),
                kind: EditorAnalysisKind::World,
            };
            let Some(cache) = self.editor_feature_cache.get(&key) else {
                return;
            };
            let EditorFeatureAnalysisData::World(data) = cache.as_ref() else {
                return;
            };
            if data.frames == 0
                || data.bins == 0
                || data.aperiodicity.len() != data.frames * data.bins
                || data.sample_rate != tab.buffer_sample_rate.max(1)
            {
                return;
            }
            f0 = tab
                .world_f0_draft
                .as_ref()
                .filter(|draft| draft.source_frames == data.frames)
                .map(|draft| draft.values.clone())
                .unwrap_or_else(|| data.f0_values.clone());
            out_len = tab.samples_len.max(1);
            n_ch = tab.ch_samples.len().max(1);
            frame_period_ms =
                data.frame_step.max(1) as f64 * 1_000.0 / data.sample_rate.max(1) as f64;
            undo = Some(Self::capture_undo_state(tab));
        }
        let Some(cache) = self.editor_feature_cache.get(&key).cloned() else {
            return;
        };
        self.audio.stop();
        // The applied audio becomes the new baseline; the re-analysis that
        // follows the apply yields a fresh curve to edit.
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.world_f0_draft = None;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            let EditorFeatureAnalysisData::World(data) = cache.as_ref() else {
                return;
            };
            let mono = crate::app::render::world_features::synthesize_world(
                &f0,
                &data.env_db,
                &data.aperiodicity,
                data.bins,
                data.fft_size,
                data.sample_rate,
                frame_period_ms,
                out_len,
            );
            let channels = vec![mono.clone(); n_ch];
            let _ = tx.send(EditorApplyResult {
                tab_idx,
                samples: mono,
                channels,
                lufs_override: None,
            });
        });
        self.editor_apply_state = Some(EditorApplyState {
            msg: "Resynthesizing with WORLD (edited F0)".to_string(),
            rx,
            tab_idx,
            undo,
        });
    }

    /// Pencil edit: set the curve at `frame` to `freq_hz` (0 = paint
    /// unvoiced), linearly interpolating (in log frequency) from the
    /// previous drag position so fast drags leave no gaps.
    pub(super) fn world_f0_paint(draft: &mut WorldF0Draft, frame: usize, freq_hz: f32) {
        let n = draft.values.len();
        if n == 0 {
            return;
        }
        let frame = frame.min(n - 1);
        let freq = if freq_hz > 0.0 {
            freq_hz.clamp(WORLD_EDIT_MIN_F0_HZ, WORLD_EDIT_MAX_F0_HZ)
        } else {
            0.0
        };
        match draft.last_drag_frame {
            Some((prev_frame, prev_freq)) if prev_frame != frame => {
                let (a, b) = if prev_frame < frame {
                    ((prev_frame, prev_freq), (frame, freq))
                } else {
                    ((frame, freq), (prev_frame, prev_freq))
                };
                let span = (b.0 - a.0) as f32;
                for i in a.0..=b.0 {
                    let t = (i - a.0) as f32 / span;
                    draft.values[i] = if a.1 <= 0.0 || b.1 <= 0.0 {
                        // Erasing (or entering from an erase): no blend.
                        if t < 0.5 {
                            a.1.max(0.0)
                        } else {
                            b.1.max(0.0)
                        }
                    } else {
                        (a.1.ln() + (b.1.ln() - a.1.ln()) * t).exp()
                    };
                }
            }
            _ => {
                draft.values[frame] = freq;
            }
        }
        draft.last_drag_frame = Some((frame, freq));
        draft.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{WorldF0Draft, WorldFeatureData};

    fn data_with_f0(f0: Vec<f32>) -> WorldFeatureData {
        WorldFeatureData {
            frames: f0.len(),
            bins: 4,
            frame_step: 240,
            sample_rate: 48_000,
            fft_size: 8,
            f0_floor: 71.0,
            f0_ceil: 800.0,
            f0_values: f0.clone(),
            env_db: vec![0.0; f0.len() * 4],
            aperiodicity: vec![1.0; f0.len() * 4],
            median_f0: None,
            voiced_ratio: 0.0,
        }
    }

    fn draft_from(data: &WorldFeatureData) -> WorldF0Draft {
        WorldF0Draft {
            values: data.f0_values.clone(),
            source_frames: data.frames,
            ..Default::default()
        }
    }

    #[test]
    fn shift_scales_voiced_frames_only() {
        let data = data_with_f0(vec![220.0, 0.0, 440.0]);
        let mut draft = draft_from(&data);
        crate::app::WavesPreviewer::world_f0_shift_semitones(&mut draft, 12.0);
        assert!((draft.values[0] - 440.0).abs() < 0.01);
        assert_eq!(draft.values[1], 0.0);
        assert!((draft.values[2] - 880.0).abs() < 0.01);
        assert!(draft.dirty);
    }

    #[test]
    fn flatten_sets_voiced_to_median() {
        let data = data_with_f0(vec![100.0, 200.0, 300.0, 0.0]);
        let mut draft = draft_from(&data);
        crate::app::WavesPreviewer::world_f0_flatten(&mut draft);
        assert_eq!(draft.values, vec![200.0, 200.0, 200.0, 0.0]);
    }

    #[test]
    fn smooth_removes_single_frame_spikes() {
        let data = data_with_f0(vec![200.0, 200.0, 400.0, 200.0, 200.0]);
        let mut draft = draft_from(&data);
        crate::app::WavesPreviewer::world_f0_smooth(&mut draft);
        assert!((draft.values[2] - 200.0).abs() < 0.01, "{:?}", draft.values);
    }

    #[test]
    fn paint_interpolates_between_drag_events() {
        let data = data_with_f0(vec![100.0; 9]);
        let mut draft = draft_from(&data);
        crate::app::WavesPreviewer::world_f0_paint(&mut draft, 0, 100.0);
        crate::app::WavesPreviewer::world_f0_paint(&mut draft, 8, 400.0);
        assert!((draft.values[4] - 200.0).abs() < 2.0, "{:?}", draft.values);
        // Erase drag paints zeros without log-blend panics.
        draft.last_drag_frame = None;
        crate::app::WavesPreviewer::world_f0_paint(&mut draft, 2, 0.0);
        crate::app::WavesPreviewer::world_f0_paint(&mut draft, 5, 0.0);
        assert_eq!(&draft.values[2..6], &[0.0, 0.0, 0.0, 0.0]);
    }
}
