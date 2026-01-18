use std::path::PathBuf;

use super::helpers::db_to_amp;
use super::types::{PreviewOverlay, ToolKind, ViewMode};
use super::{WavesPreviewer, LIVE_PREVIEW_SAMPLE_LIMIT};

impl WavesPreviewer {
    pub(super) fn preview_restore_audio_for_tab(&self, tab_idx: usize) {
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.audio.stop();
            self.audio.set_samples_channels(tab.ch_samples.clone());
            // Reapply loop mode
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn set_preview_mono(&mut self, tab_idx: usize, tool: ToolKind, mono: Vec<f32>) {
        self.audio.stop();
        self.audio.set_samples_mono(mono);
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(tool);
        }
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn refresh_tool_preview_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.view_mode != ViewMode::Waveform {
            return;
        }
        if tab.preview_audio_tool.is_some() || tab.preview_overlay.is_some() {
            return;
        }
        if self.heavy_preview_rx.is_some() || self.heavy_overlay_rx.is_some() {
            return;
        }
        let tool = tab.active_tool;
        let st = tab.tool_state;
        let fade_in_ms = st.fade_in_ms;
        let fade_out_ms = st.fade_out_ms;
        let fade_in_shape = tab.fade_in_shape;
        let fade_out_shape = tab.fade_out_shape;
        let gain_db = st.gain_db;
        let normalize_db = st.normalize_target_db;
        let semitones = st.pitch_semitones;
        let stretch_rate = st.stretch_rate;
        let allow_light_preview = tab.samples_len <= LIVE_PREVIEW_SAMPLE_LIMIT;
        let use_path_preview = !allow_light_preview && !tab.dirty;
        let tab_path = tab.path.clone();
        let ch_samples = if use_path_preview {
            Vec::new()
        } else {
            tab.ch_samples.clone()
        };
        let samples_len = tab.samples_len;
        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
        let decode_failed = self.is_decode_failed_path(&tab.path);
        let _ = tab;

        match tool {
            ToolKind::PitchShift => {
                if semitones.abs() > 0.0001 && !decode_failed {
                    self.audio.stop();
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.preview_audio_tool = Some(ToolKind::PitchShift);
                        tab.preview_overlay = None;
                    }
                    if use_path_preview {
                        self.spawn_heavy_preview_from_path(
                            tab_path.clone(),
                            ToolKind::PitchShift,
                            semitones,
                        );
                        self.spawn_heavy_overlay_from_path(
                            tab_path,
                            ToolKind::PitchShift,
                            semitones,
                        );
                    } else {
                        let mono = Self::mixdown_channels(&ch_samples, samples_len);
                        if mono.is_empty() {
                            return;
                        }
                        self.spawn_heavy_preview_owned(mono, ToolKind::PitchShift, semitones);
                        self.spawn_heavy_overlay_for_tab(tab_idx, ToolKind::PitchShift, semitones);
                    }
                }
            }
            ToolKind::TimeStretch => {
                if (stretch_rate - 1.0).abs() > 0.0001 && !decode_failed {
                    self.audio.stop();
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        tab.preview_audio_tool = Some(ToolKind::TimeStretch);
                        tab.preview_overlay = None;
                    }
                    if use_path_preview {
                        self.spawn_heavy_preview_from_path(
                            tab_path.clone(),
                            ToolKind::TimeStretch,
                            stretch_rate,
                        );
                        self.spawn_heavy_overlay_from_path(
                            tab_path,
                            ToolKind::TimeStretch,
                            stretch_rate,
                        );
                    } else {
                        let mono = Self::mixdown_channels(&ch_samples, samples_len);
                        if mono.is_empty() {
                            return;
                        }
                        self.spawn_heavy_preview_owned(mono, ToolKind::TimeStretch, stretch_rate);
                        self.spawn_heavy_overlay_for_tab(
                            tab_idx,
                            ToolKind::TimeStretch,
                            stretch_rate,
                        );
                    }
                }
            }
            ToolKind::Fade => {
                if !allow_light_preview {
                    return;
                }
                if fade_in_ms <= 0.0 && fade_out_ms <= 0.0 {
                    return;
                }
                let mut overlay = ch_samples.clone();
                let len = samples_len.max(1);
                let n_in = ((fade_in_ms / 1000.0) * sr).round() as usize;
                let n_out = ((fade_out_ms / 1000.0) * sr).round() as usize;
                if n_in > 0 {
                    for ch in overlay.iter_mut() {
                        let nn = n_in.min(ch.len());
                        for i in 0..nn {
                            let t = i as f32 / nn.max(1) as f32;
                            let w = Self::fade_weight(fade_in_shape, t);
                            ch[i] *= w;
                        }
                    }
                }
                if n_out > 0 {
                    for ch in overlay.iter_mut() {
                        let len = ch.len();
                        let nn = n_out.min(len);
                        for i in 0..nn {
                            let t = i as f32 / nn.max(1) as f32;
                            let w = Self::fade_weight_out(fade_out_shape, t);
                            let idx = len - nn + i;
                            ch[idx] *= w;
                        }
                    }
                }
                let mono = Self::mixdown_channels(&overlay, len);
                if mono.is_empty() {
                    return;
                }
                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::Fade,
                        timeline_len,
                    ));
                }
                self.set_preview_mono(tab_idx, ToolKind::Fade, mono);
            }
            ToolKind::Gain => {
                if !allow_light_preview {
                    return;
                }
                if gain_db.abs() <= 1e-6 {
                    return;
                }
                let g = db_to_amp(gain_db);
                let mut overlay = ch_samples.clone();
                for ch in overlay.iter_mut() {
                    for v in ch.iter_mut() {
                        *v *= g;
                    }
                }
                let mono = Self::mixdown_channels(&overlay, samples_len);
                if mono.is_empty() {
                    return;
                }
                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::Gain,
                        timeline_len,
                    ));
                }
                self.set_preview_mono(tab_idx, ToolKind::Gain, mono);
            }
            ToolKind::Normalize => {
                if !allow_light_preview {
                    return;
                }
                const DEFAULT_NORMALIZE_DB: f32 = -6.0;
                if (normalize_db - DEFAULT_NORMALIZE_DB).abs() <= 1e-6 {
                    return;
                }
                let mut mono = Self::mixdown_channels(&ch_samples, samples_len);
                if mono.is_empty() {
                    return;
                }
                let mut peak = 0.0f32;
                for &v in &mono {
                    peak = peak.max(v.abs());
                }
                if peak <= 0.0 {
                    return;
                }
                let g = db_to_amp(normalize_db) / peak.max(1e-12);
                let mut overlay = ch_samples.clone();
                for ch in overlay.iter_mut() {
                    for v in ch.iter_mut() {
                        *v *= g;
                    }
                }
                for v in &mut mono {
                    *v *= g;
                }
                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(samples_len);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                        overlay,
                        ToolKind::Normalize,
                        timeline_len,
                    ));
                }
                self.set_preview_mono(tab_idx, ToolKind::Normalize, mono);
            }
            _ => {}
        }
    }

    pub(super) fn clear_preview_if_any(&mut self, tab_idx: usize) {
        let had_preview_audio = self
            .tabs
            .get(tab_idx)
            .and_then(|tab| tab.preview_audio_tool)
            .is_some();
        if had_preview_audio {
            self.audio.stop();
            self.preview_restore_audio_for_tab(tab_idx);
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
        }
        // also discard any in-flight preview/overlay job
        self.heavy_preview_rx = None;
        self.heavy_preview_tool = None;
        self.heavy_overlay_rx = None;
        self.overlay_expected_tool = None;
    }

    pub(super) fn spawn_heavy_preview_owned(&mut self, mono: Vec<f32>, tool: ToolKind, param: f32) {
        use std::sync::mpsc;
        let sr = self.audio.shared.out_sample_rate; // cancel previous job by dropping receiver
        self.heavy_preview_rx = None;
        self.heavy_preview_tool = None;
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        std::thread::spawn(move || {
            let out = match tool {
                ToolKind::PitchShift => {
                    crate::wave::process_pitchshift_offline(&mono, sr, sr, param)
                }
                ToolKind::TimeStretch => {
                    crate::wave::process_timestretch_offline(&mono, sr, sr, param)
                }
                _ => mono,
            };
            let _ = tx.send(out);
        });
        self.heavy_preview_rx = Some(rx);
        self.heavy_preview_tool = Some(tool);
    }

    pub(super) fn spawn_heavy_preview_from_path(
        &mut self,
        path: PathBuf,
        tool: ToolKind,
        param: f32,
    ) {
        use std::sync::mpsc;
        let sr = self.audio.shared.out_sample_rate;
        self.heavy_preview_rx = None;
        self.heavy_preview_tool = None;
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        std::thread::spawn(move || {
            let (mono, in_sr) = match crate::wave::decode_wav_mono(&path) {
                Ok(v) => v,
                Err(_) => return,
            };
            let mono = if in_sr != sr {
                crate::wave::resample_linear(&mono, in_sr, sr)
            } else {
                mono
            };
            let out = match tool {
                ToolKind::PitchShift => {
                    crate::wave::process_pitchshift_offline(&mono, sr, sr, param)
                }
                ToolKind::TimeStretch => {
                    crate::wave::process_timestretch_offline(&mono, sr, sr, param)
                }
                _ => mono,
            };
            let _ = tx.send(out);
        });
        self.heavy_preview_rx = Some(rx);
        self.heavy_preview_tool = Some(tool);
    }

    // Spawn per-channel overlay generator (Pitch/Stretch) in a worker thread.
    // Note: Call this ONLY after UI borrows end (see E0499 note) to avoid nested &mut self borrows.
    pub(super) fn spawn_heavy_overlay_for_tab(
        &mut self,
        tab_idx: usize,
        tool: ToolKind,
        param: f32,
    ) {
        use std::sync::mpsc;
        // Cancel previous overlay job by dropping receiver
        self.heavy_overlay_rx = None;
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_overlay = None;
            let path = tab.path.clone();
            let ch = tab.ch_samples.clone();
            let sr = self.audio.shared.out_sample_rate;
            let target_len = tab.samples_len;
            // generation guard
            self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
            let gen = self.overlay_gen_counter;
            self.overlay_expected_gen = gen;
            self.overlay_expected_tool = Some(tool);
            let (tx, rx) = mpsc::channel::<(std::path::PathBuf, Vec<Vec<f32>>, usize, u64)>();
            std::thread::spawn(move || {
                let mut out: Vec<Vec<f32>> = Vec::with_capacity(ch.len());
                let mut result_len = target_len;
                for chan in ch.iter() {
                    let processed = match tool {
                        ToolKind::PitchShift => {
                            crate::wave::process_pitchshift_offline(chan, sr, sr, param)
                        }
                        ToolKind::TimeStretch => {
                            crate::wave::process_timestretch_offline(chan, sr, sr, param)
                        }
                        _ => chan.clone(),
                    };
                    result_len = processed.len();
                    out.push(processed);
                }
                let timeline_len = out.get(0).map(|c| c.len()).unwrap_or(result_len).max(1);
                let _ = tx.send((path, out, timeline_len, gen));
            });
            self.heavy_overlay_rx = Some(rx);
        }
    }

    pub(super) fn spawn_heavy_overlay_from_path(
        &mut self,
        path: PathBuf,
        tool: ToolKind,
        param: f32,
    ) {
        use std::sync::mpsc;
        self.heavy_overlay_rx = None;
        self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
        let gen = self.overlay_gen_counter;
        self.overlay_expected_gen = gen;
        self.overlay_expected_tool = Some(tool);
        let out_sr = self.audio.shared.out_sample_rate;
        let (tx, rx) = mpsc::channel::<(std::path::PathBuf, Vec<Vec<f32>>, usize, u64)>();
        std::thread::spawn(move || {
            let (mut chs, in_sr) = match crate::wave::decode_wav_multi(&path) {
                Ok(v) => v,
                Err(_) => return,
            };
            if in_sr != out_sr {
                for c in chs.iter_mut() {
                    *c = crate::wave::resample_linear(c, in_sr, out_sr);
                }
            }
            let mut out: Vec<Vec<f32>> = Vec::with_capacity(chs.len());
            let mut result_len = 0;
            for chan in chs.iter() {
                let processed = match tool {
                    ToolKind::PitchShift => {
                        crate::wave::process_pitchshift_offline(chan, out_sr, out_sr, param)
                    }
                    ToolKind::TimeStretch => {
                        crate::wave::process_timestretch_offline(chan, out_sr, out_sr, param)
                    }
                    _ => chan.clone(),
                };
                result_len = processed.len();
                out.push(processed);
            }
            let timeline_len = out.get(0).map(|c| c.len()).unwrap_or(result_len).max(1);
            let _ = tx.send((path, out, timeline_len, gen));
        });
        self.heavy_overlay_rx = Some(rx);
    }

    pub(super) fn preview_overlay_from_channels(
        channels: Vec<Vec<f32>>,
        tool: ToolKind,
        timeline_len: usize,
    ) -> PreviewOverlay {
        let mixdown = if channels.len() > 1 {
            let len = channels.get(0).map(|c| c.len()).unwrap_or(0);
            Some(Self::mixdown_channels(&channels, len))
        } else {
            None
        };
        PreviewOverlay {
            channels,
            mixdown,
            source_tool: tool,
            timeline_len,
        }
    }
}
