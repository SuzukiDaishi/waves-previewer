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

    /// Warp each frame's spectral envelope along the frequency axis by
    /// `ratio` (formant shift: >1 moves formants up, <1 down). Backward
    /// mapping with linear interpolation and edge clamping; `env_db` is
    /// `frames * bins` row-major and the output has the same shape.
    pub(super) fn world_env_warp(
        env_db: &[f32],
        frames: usize,
        bins: usize,
        ratio: f32,
    ) -> Vec<f32> {
        if frames == 0 || bins == 0 || env_db.len() != frames * bins {
            return env_db.to_vec();
        }
        let ratio = ratio.clamp(0.25, 4.0);
        if (ratio - 1.0).abs() < 1e-3 {
            return env_db.to_vec();
        }
        let mut out = vec![0.0f32; env_db.len()];
        for f in 0..frames {
            let row = &env_db[f * bins..(f + 1) * bins];
            let dst = &mut out[f * bins..(f + 1) * bins];
            for (b, d) in dst.iter_mut().enumerate() {
                let src = (b as f32 / ratio).clamp(0.0, (bins - 1) as f32);
                let i0 = src.floor() as usize;
                let i1 = (i0 + 1).min(bins - 1);
                let t = src - i0 as f32;
                *d = row[i0] * (1.0 - t) + row[i1] * t;
            }
        }
        out
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
        let source_channels;
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
            source_channels = tab.ch_samples_arc.clone();
            undo = Some(Self::capture_undo_state(tab));
        }
        let formant_ratio = self
            .tabs
            .get(tab_idx)
            .map(|t| t.world_formant_ratio.clamp(0.25, 4.0))
            .unwrap_or(1.0);
        let formant_active = (formant_ratio - 1.0).abs() >= 1e-3;
        let f0_method = self.world_f0_method;
        let Some(cache) = self.editor_feature_cache.get(&key).cloned() else {
            return;
        };
        let EditorFeatureAnalysisData::World(job_data) = cache.as_ref() else {
            return;
        };
        let job_data = job_data.clone();
        if self.editor_apply_state.is_some() {
            return;
        }
        let Some((apply_tab_id, apply_tab_path)) =
            self.tabs.get(tab_idx).map(|t| (t.tab_id, t.path.clone()))
        else {
            return;
        };
        if matches!(&self.playback_session.source,
            crate::app::PlaybackSourceKind::EditorTab(p) if *p == apply_tab_path)
        {
            self.audio.stop();
        }
        // The applied audio becomes the new baseline; the re-analysis that
        // follows the apply yields a fresh curve to edit.
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.world_f0_draft = None;
            // The warp bakes into the new baseline audio.
            tab.world_formant_ratio = 1.0;
        }
        // The display analysis stretches its frame period on long clips to
        // bound the heatmap size, but synthesizing from a coarse grid audibly
        // smears the result. Above ~5.5 ms the worker re-analyzes the source
        // at WORLD's native 5 ms and maps the edited curve onto that grid, so
        // resynthesis quality is independent of clip length.
        const RESYNTH_FRAME_PERIOD_MS: f64 = 5.0;
        let fine_reanalysis = frame_period_ms > RESYNTH_FRAME_PERIOD_MS * 1.1;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            let data = job_data;
            let mono = if fine_reanalysis {
                let source = crate::app::WavesPreviewer::mixdown_channels(
                    &source_channels,
                    out_len,
                );
                let fine = crate::app::render::world_features::analyze_world_with_options(
                    &source,
                    data.sample_rate,
                    RESYNTH_FRAME_PERIOD_MS,
                    f0_method.estimator(),
                    None,
                );
                let fine_step =
                    data.sample_rate as f64 * RESYNTH_FRAME_PERIOD_MS / 1_000.0;
                let fine_f0 = Self::resample_f0_curve(
                    &f0,
                    data.frame_step.max(1),
                    fine.frames,
                    fine_step,
                );
                let env = if formant_active {
                    Self::world_env_warp(&fine.envelope_db, fine.frames, fine.bins, formant_ratio)
                } else {
                    fine.envelope_db.clone()
                };
                crate::app::render::world_features::synthesize_world(
                    &fine_f0,
                    &env,
                    &fine.aperiodicity,
                    fine.bins,
                    fine.fft_size,
                    data.sample_rate,
                    RESYNTH_FRAME_PERIOD_MS,
                    out_len,
                )
            } else {
                let env = if formant_active {
                    Self::world_env_warp(&data.env_db, data.frames, data.bins, formant_ratio)
                } else {
                    data.env_db.clone()
                };
                crate::app::render::world_features::synthesize_world(
                    &f0,
                    &env,
                    &data.aperiodicity,
                    data.bins,
                    data.fft_size,
                    data.sample_rate,
                    frame_period_ms,
                    out_len,
                )
            };
            let channels = vec![mono.clone(); n_ch];
            let (waveform_minmax, waveform_pyramid) =
                crate::app::WavesPreviewer::build_editor_waveform_cache(&channels, out_len);
            let channels_arc = std::sync::Arc::new(channels.clone());
            let _ = tx.send(EditorApplyResult {
                samples: mono,
                channels,
                channels_arc,
                waveform_minmax,
                waveform_pyramid,
                lufs_override: None,
                selection_after: None,
            });
        });
        self.editor_apply_state = Some(EditorApplyState {
            msg: match (fine_reanalysis, formant_active) {
                (true, _) => {
                    "Resynthesizing with WORLD (re-analyzing at 5 ms for quality)".to_string()
                }
                (false, true) => {
                    format!("Resynthesizing with WORLD (formant x{formant_ratio:.2})")
                }
                (false, false) => "Resynthesizing with WORLD (edited F0)".to_string(),
            },
            rx,
            tab_id: apply_tab_id,
            undo,
        });
    }

    /// Map an edited F0 curve from the display analysis grid onto a finer
    /// synthesis grid. Voiced spans interpolate geometrically (linear in
    /// log-frequency, matching how the pencil interpolates); at voicing
    /// boundaries the nearest source frame wins so user-erased regions stay
    /// unvoiced and drawn regions stay voiced.
    pub(super) fn resample_f0_curve(
        src: &[f32],
        src_step_samples: usize,
        dst_frames: usize,
        dst_step_samples: f64,
    ) -> Vec<f32> {
        if src.is_empty() || dst_frames == 0 {
            return vec![0.0; dst_frames];
        }
        let src_step = src_step_samples.max(1) as f64;
        (0..dst_frames)
            .map(|i| {
                let pos = i as f64 * dst_step_samples / src_step;
                let k = (pos.floor() as usize).min(src.len() - 1);
                let t = (pos - k as f64).clamp(0.0, 1.0) as f32;
                let a = src[k];
                let b = src.get(k + 1).copied().unwrap_or(a);
                if a > 0.0 && b > 0.0 {
                    (a.ln() + (b.ln() - a.ln()) * t).exp()
                } else if t < 0.5 {
                    a.max(0.0)
                } else {
                    b.max(0.0)
                }
            })
            .collect()
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
mod env_warp_tests {
    use crate::app::WavesPreviewer;

    #[test]
    fn ratio_one_is_identity_and_shape_is_preserved() {
        let frames = 3usize;
        let bins = 64usize;
        let env: Vec<f32> = (0..frames * bins).map(|i| (i % bins) as f32 * 0.5).collect();
        let out = WavesPreviewer::world_env_warp(&env, frames, bins, 1.0);
        assert_eq!(out, env);
        let out = WavesPreviewer::world_env_warp(&env, frames, bins, 1.5);
        assert_eq!(out.len(), env.len(), "warp must not change the shape");
    }

    #[test]
    fn formant_bump_moves_by_the_ratio() {
        let frames = 1usize;
        let bins = 128usize;
        // A single bump at bin 20 over a -60 dB floor.
        let mut env = vec![-60.0f32; bins];
        for (i, v) in env.iter_mut().enumerate() {
            let d = i as f32 - 20.0;
            *v = -60.0 + 40.0 * (-0.5 * (d / 2.0) * (d / 2.0)).exp();
        }
        let up = WavesPreviewer::world_env_warp(&env, frames, bins, 2.0);
        let peak_up = up
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            (peak_up as i32 - 40).abs() <= 1,
            "x2 warp should move the bump from bin 20 to ~40, got {peak_up}"
        );
        let down = WavesPreviewer::world_env_warp(&env, frames, bins, 0.5);
        let peak_down = down
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            (peak_down as i32 - 10).abs() <= 1,
            "x0.5 warp should move the bump from bin 20 to ~10, got {peak_down}"
        );
    }

    #[test]
    fn degenerate_inputs_pass_through() {
        assert!(WavesPreviewer::world_env_warp(&[], 0, 0, 2.0).is_empty());
        // Mismatched shape returns the input untouched.
        let env = vec![1.0f32; 10];
        assert_eq!(WavesPreviewer::world_env_warp(&env, 3, 4, 2.0), env);
    }

    #[test]
    fn warped_envelope_still_synthesizes_same_length() {
        let sr = 48_000u32;
        let len = (sr / 4) as usize;
        let source: Vec<f32> = (0..len)
            .map(|i| (i as f32 / sr as f32 * 220.0 * std::f32::consts::TAU).sin() * 0.4)
            .collect();
        let a = crate::app::render::world_features::analyze_world_with_options(
            &source,
            sr,
            5.0,
            crate::app::render::world_features::WorldF0Estimator::Dio,
            None,
        );
        if a.frames == 0 {
            return; // analysis unavailable in this build; nothing to check
        }
        let env = WavesPreviewer::world_env_warp(&a.envelope_db, a.frames, a.bins, 1.3);
        let out = crate::app::render::world_features::synthesize_world(
            &a.f0,
            &env,
            &a.aperiodicity,
            a.bins,
            a.fft_size,
            sr,
            5.0,
            len,
        );
        assert_eq!(out.len(), len, "resynthesis length must match the request");
        assert!(out.iter().all(|v| v.is_finite()));
        assert!(out.iter().any(|v| v.abs() > 1e-3), "resynthesis is silent");
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
            env_max_db: 0.0,
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
    fn resample_f0_curve_interpolates_and_respects_voicing() {
        // 10 ms grid -> 5 ms grid: midpoints interpolate geometrically.
        let src = vec![200.0, 400.0, 0.0, 300.0];
        let out = crate::app::WavesPreviewer::resample_f0_curve(&src, 480, 8, 120.0);
        assert!((out[0] - 200.0).abs() < 0.01);
        let expected_mid = (200.0f32.ln() * 0.5 + 400.0f32.ln() * 0.5).exp();
        assert!(
            (out[2] - expected_mid).abs() < 1.0,
            "geometric midpoint, got {}",
            out[2]
        );
        assert!((out[4] - 400.0).abs() < 0.01);
        // Voicing boundary: no interpolation into zero, nearest side wins.
        assert!((out[5] - 400.0).abs() < 0.01, "t<0.5 keeps voiced side");
        assert_eq!(out[6], 0.0, "t>=0.5 crosses to the unvoiced side");
        // Identity (within ln/exp rounding) when grids match.
        let same = crate::app::WavesPreviewer::resample_f0_curve(&src, 240, 4, 240.0);
        for (a, b) in same.iter().zip(src.iter()) {
            assert!((a - b).abs() < 0.001, "grid-match roundtrip: {a} vs {b}");
        }
    }

    /// Analysis→synthesis roundtrip; returns (rms delta dB, median F0 of the
    /// re-analyzed output) over the middle 3/4 of a one-second signal.
    fn world_roundtrip(mono: &[f32], sr: u32) -> (f64, f32) {
        use crate::app::render::world_features::{analyze_world, synthesize_world};
        let n = mono.len();
        let features = analyze_world(mono, sr, 5.0);
        let out = synthesize_world(
            &features.f0,
            &features.envelope_db,
            &features.aperiodicity,
            features.bins,
            features.fft_size,
            sr,
            5.0,
            n,
        );
        assert_eq!(out.len(), n);
        let rms = |xs: &[f32]| {
            (xs.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / xs.len() as f64).sqrt()
        };
        // Skip the edges where the vocoder windows fade in/out.
        let core_in = &mono[n / 8..n - n / 8];
        let core_out = &out[n / 8..n - n / 8];
        let db_delta = 20.0 * (rms(core_out) / rms(core_in).max(1e-12)).log10();
        let check = analyze_world(&out, sr, 5.0);
        let mut voiced: Vec<f32> = check.f0.iter().copied().filter(|v| *v > 0.0).collect();
        assert!(!voiced.is_empty());
        voiced.sort_by(f32::total_cmp);
        (db_delta, voiced[voiced.len() / 2])
    }

    /// Band-limited sawtooth: harmonics 1/k up to Nyquist, RMS-normalized.
    fn sawtooth(sr: u32, f0_hz: f32, rms_target: f32) -> Vec<f32> {
        let n = sr as usize;
        let mut x = vec![0.0f32; n];
        let mut k = 1.0f32;
        while k * f0_hz < sr as f32 / 2.0 {
            for (i, v) in x.iter_mut().enumerate() {
                *v += (std::f32::consts::TAU * k * f0_hz * i as f32 / sr as f32).sin() / k;
            }
            k += 1.0;
        }
        let rms = (x.iter().map(|v| v * v).sum::<f32>() / n as f32).sqrt();
        for v in x.iter_mut() {
            *v *= rms_target / rms;
        }
        x
    }

    #[test]
    fn resynthesis_preserves_level_and_pitch_at_44100() {
        // Guards the two "beyond vocoder loss" failure modes: a sample-rate
        // mix-up (pitch would shift) and a broken gain stage (level would
        // collapse or explode). Run at 44.1 kHz to exercise a non-48k rate.
        // Harmonic-rich material is WORLD's design case: the reference
        // implementation (pyworld 0.3.5) roundtrips this sawtooth at +0.23 dB.
        let sr = 44_100u32;
        let mono = sawtooth(sr, 220.0, 0.35);
        let (db_delta, median) = world_roundtrip(&mono, sr);
        assert!(
            db_delta.abs() < 1.5,
            "harmonic-rich resynthesis should stay within 1.5 dB, got {db_delta:.2} dB"
        );
        assert!(
            (median - 220.0).abs() < 8.0,
            "pitch must survive a 44.1 kHz roundtrip, got {median} Hz"
        );
    }

    #[test]
    fn pure_sine_roundtrip_matches_world_reference_gain() {
        // A lone sinusoid is a known pathological case for CheapTrick: the
        // envelope smoothing (2*f0/3 width) spreads the single harmonic's
        // energy, and resynthesis comes out ~+4 dB hot. This is NOT a port
        // bug — pyworld 0.3.5 measures +3.98 dB on this exact signal — so pin
        // the value to catch any future drift away from the reference.
        let sr = 44_100u32;
        let n = sr as usize;
        let mono: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * 220.0 * i as f32 / sr as f32).sin() * 0.5)
            .collect();
        let (db_delta, median) = world_roundtrip(&mono, sr);
        assert!(
            (db_delta - 3.98).abs() < 0.75,
            "pure-sine roundtrip should match the WORLD reference (+3.98 dB), got {db_delta:.2} dB"
        );
        assert!(
            (median - 220.0).abs() < 8.0,
            "pitch must survive, got {median} Hz"
        );
    }

    #[test]
    fn flat_envelope_synthesis_is_calibrated() {
        // WORLD's calibration contract: a flat 0 dB power envelope with
        // near-zero aperiodicity and constant F0 synthesizes to ~unit-RMS
        // output (pulse height sqrt(T) every T samples). Deviation here means
        // the synthesis normalization broke.
        use crate::app::render::world_features::synthesize_world;
        let sr = 48_000u32;
        let fft_size = 2048usize;
        let bins = fft_size / 2 + 1;
        let frames = 200usize; // 1 s at 5 ms
        let f0 = vec![220.0f32; frames];
        let env_db = vec![0.0f32; frames * bins];
        let ap = vec![0.001f32; frames * bins];
        let n = sr as usize;
        let out = synthesize_world(&f0, &env_db, &ap, bins, fft_size, sr, 5.0, n);
        let core = &out[n / 8..n - n / 8];
        let rms = (core.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>()
            / core.len() as f64)
            .sqrt();
        let db = 20.0 * rms.log10();
        assert!(
            db.abs() < 1.0,
            "flat-envelope synthesis should be ~unit RMS, got {db:+.2} dB"
        );
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
