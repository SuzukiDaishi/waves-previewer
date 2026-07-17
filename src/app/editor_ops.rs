use std::path::PathBuf;

use crate::app::types::{
    EditorApplyResult, EditorUndoState, ToolKind, VirtualTrimPhase, VirtualTrimResult,
    VirtualTrimState,
};

const VIRTUAL_TRIM_COPY_CHUNK_FRAMES: usize = 262_144;
const VIRTUAL_TRIM_COPY_FRAME_BUDGET_MS: u64 = 4;

impl crate::app::WavesPreviewer {
    pub(super) fn editor_sync_view_offset_exact(tab: &mut crate::app::types::EditorTab) {
        tab.view_offset_exact = tab.view_offset as f64;
    }

    pub(super) fn editor_visible_half_amplitude(vertical_zoom: f32) -> f32 {
        1.0 / vertical_zoom.clamp(
            crate::app::EDITOR_MIN_VERTICAL_ZOOM,
            crate::app::EDITOR_MAX_VERTICAL_ZOOM,
        )
    }

    pub(super) fn editor_clamped_vertical_view_center(
        vertical_zoom: f32,
        vertical_view_center: f32,
    ) -> f32 {
        let zoom = vertical_zoom.clamp(
            crate::app::EDITOR_MIN_VERTICAL_ZOOM,
            crate::app::EDITOR_MAX_VERTICAL_ZOOM,
        );
        if zoom <= 1.0 {
            0.0
        } else {
            let half = Self::editor_visible_half_amplitude(zoom).clamp(0.0, 1.0);
            let limit = (1.0 - half).max(0.0);
            vertical_view_center.clamp(-limit, limit)
        }
    }

    pub(super) fn editor_clamp_vertical_view(tab: &mut crate::app::types::EditorTab) {
        tab.vertical_zoom = tab.vertical_zoom.clamp(
            crate::app::EDITOR_MIN_VERTICAL_ZOOM,
            crate::app::EDITOR_MAX_VERTICAL_ZOOM,
        );
        tab.vertical_view_center =
            Self::editor_clamped_vertical_view_center(tab.vertical_zoom, tab.vertical_view_center);
    }

    pub(super) fn editor_clear_selection_anchor(tab: &mut crate::app::types::EditorTab) {
        tab.selection_anchor_sample = None;
        tab.right_drag_mode = None;
    }

    fn editor_rebuild_waveform_state(tab: &mut crate::app::types::EditorTab) {
        tab.samples_len = tab.ch_samples.get(0).map(|c| c.len()).unwrap_or(0);
        tab.samples_len_visual = tab.samples_len;
        tab.loading = false;
        tab.loading_waveform_minmax.clear();
        // Analysis workers (spectrogram / tempogram / chromagram / WORLD)
        // read this Arc mirror; a stale mirror would silently feed them
        // pre-edit audio.
        tab.ch_samples_arc = std::sync::Arc::new(tab.ch_samples.clone());
        // The overview + pyramid rebuild is O(n) and is deferred to the
        // waveform postprocess worker (queued by the caller); the stale
        // pyramid is dropped so zoomed-out rendering falls back to the raw
        // samples instead of pre-edit peaks.
        tab.waveform_pyramid = None;
    }

    /// Queue an off-thread rebuild of the waveform overview + pyramid for
    /// `tab_idx` from its current Arc mirror. Results are adopted by
    /// [`Self::drain_editor_wave_cache_jobs`]; a per-path generation drops
    /// results that arrive after a newer edit.
    pub(super) fn queue_editor_wave_cache_rebuild(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let path = tab.path.clone();
        let channels = tab.ch_samples_arc.clone();
        let samples_len = tab.samples_len;
        self.editor_wave_cache_generation_counter =
            self.editor_wave_cache_generation_counter.wrapping_add(1);
        let generation = self.editor_wave_cache_generation_counter;
        self.editor_wave_cache_generation
            .insert(path.clone(), generation);
        if self.editor_wave_cache_tx.is_none() || self.editor_wave_cache_rx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.editor_wave_cache_tx = Some(tx);
            self.editor_wave_cache_rx = Some(rx);
        }
        let Some(tx) = self.editor_wave_cache_tx.as_ref().cloned() else {
            return;
        };
        std::thread::spawn(move || {
            super::threading::lower_current_thread_priority();
            let (waveform_minmax, waveform_pyramid) =
                crate::app::WavesPreviewer::build_editor_waveform_cache(&channels, samples_len);
            let _ = tx.send((path, generation, waveform_minmax, waveform_pyramid));
        });
    }

    pub(super) fn drain_editor_wave_cache_jobs(&mut self, ctx: &egui::Context) {
        let mut results = Vec::new();
        if let Some(rx) = &self.editor_wave_cache_rx {
            while let Ok(msg) = rx.try_recv() {
                results.push(msg);
            }
        }
        for (path, generation, waveform_minmax, waveform_pyramid) in results {
            if self.editor_wave_cache_generation.get(&path).copied() != Some(generation) {
                continue;
            }
            self.editor_wave_cache_generation.remove(&path);
            for tab in self.tabs.iter_mut().filter(|t| t.path == path) {
                tab.waveform_minmax = waveform_minmax.clone();
                tab.waveform_pyramid = waveform_pyramid.clone();
                Self::invalidate_editor_viewport_cache(tab);
            }
            ctx.request_repaint();
        }
    }

    fn editor_invalidate_destructive_preview_state(tab: &mut crate::app::types::EditorTab) {
        tab.preview_audio_tool = None;
        tab.preview_overlay = None;
        tab.preview_offset_samples = None;
        tab.pending_loop_unwrap = None;
        tab.dragging_marker = None;
        // Scan markers describe the pre-edit buffer.
        tab.declick_scan = None;
        Self::editor_clear_selection_anchor(tab);
    }

    pub(super) fn editor_finish_destructive_apply(
        &mut self,
        tab_idx: usize,
        undo_state: EditorUndoState,
        stop_playback: bool,
    ) {
        let Some((path, buffer_sample_rate, channels)) = self.tabs.get_mut(tab_idx).map(|tab| {
            Self::editor_rebuild_waveform_state(tab);
            Self::editor_invalidate_destructive_preview_state(tab);
            Self::update_markers_dirty(tab);
            Self::update_loop_markers_dirty(tab);
            Self::editor_clamp_ranges(tab);
            Self::invalidate_editor_viewport_cache(tab);
            (
                tab.path.clone(),
                tab.buffer_sample_rate.max(1),
                tab.ch_samples.clone(),
            )
        }) else {
            return;
        };
        self.queue_editor_wave_cache_rebuild(tab_idx);

        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.clear_heavy_preview_state();
        self.clear_heavy_overlay_state();
        self.playback_session.last_play_start_display_sample = None;
        self.audio.set_samples_channels(channels);
        if stop_playback {
            self.audio.stop();
        }
        self.on_audio_length_changed(tab_idx);
        self.playback_mark_buffer_source(
            crate::app::PlaybackSourceKind::EditorTab(path.clone()),
            buffer_sample_rate,
        );
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
        self.cancel_spectrogram_for_path(&path);
        self.cancel_feature_analysis_for_path(&path);
    }

    /// Reverts a tab to the original file on disk, discarding all destructive
    /// edits (gain/fade/effects/etc.) made since it was opened. Selection,
    /// markers and loop-range annotations are left untouched — only the
    /// audio buffer and edit history are reset.
    pub(super) fn clear_edit_in_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let path = tab.path.clone();
        let target_sr = self
            .sample_rate_override
            .get(&path)
            .copied()
            .filter(|v| *v > 0);
        let bit_depth = self.bit_depth_override.get(&path).copied();
        let out_sr = self.audio.shared.out_sample_rate;
        let resample_quality = Self::to_wave_resample_quality(self.src_quality);
        let Ok((chans, in_sr)) = crate::audio_io::decode_audio_multi(&path) else {
            self.debug_log(format!("clear edit: decode failed for {}", path.display()));
            return;
        };
        let channels = Self::process_editor_decode_channels(
            chans,
            in_sr.max(1),
            out_sr,
            target_sr,
            bit_depth,
            resample_quality,
        );

        let undo_state = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let undo_state = Self::capture_undo_state(tab);
            tab.ch_samples = channels;
            tab.buffer_sample_rate = out_sr.max(1);
            Self::editor_clamp_ranges(tab);
            undo_state
        };
        self.edited_cache.remove(&path);
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
        // Clear Edit is a hard reset: unlike other destructive edits, it does
        // not leave behind an undo point back to the pre-clear (edited) state.
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.dirty = false;
            tab.undo_stack.clear();
            tab.undo_bytes = 0;
            tab.redo_stack.clear();
            tab.redo_bytes = 0;
        }
    }

    pub(super) fn fade_weight(shape: crate::app::types::FadeShape, t: f32) -> f32 {
        let x = t.clamp(0.0, 1.0);
        match shape {
            crate::app::types::FadeShape::Linear => x,
            crate::app::types::FadeShape::EqualPower => (core::f32::consts::FRAC_PI_2 * x).sin(),
            crate::app::types::FadeShape::Cosine => (1.0 - (core::f32::consts::PI * x).cos()) * 0.5,
            crate::app::types::FadeShape::SCurve => x * x * (3.0 - 2.0 * x),
            crate::app::types::FadeShape::Quadratic => x * x,
            crate::app::types::FadeShape::Cubic => x * x * x,
        }
    }

    pub(super) fn fade_weight_out(shape: crate::app::types::FadeShape, t: f32) -> f32 {
        let x = t.clamp(0.0, 1.0);
        match shape {
            crate::app::types::FadeShape::Linear => 1.0 - x,
            crate::app::types::FadeShape::EqualPower => (core::f32::consts::FRAC_PI_2 * x).cos(),
            crate::app::types::FadeShape::Cosine => (1.0 + (core::f32::consts::PI * x).cos()) * 0.5,
            crate::app::types::FadeShape::SCurve => 1.0 - Self::fade_weight(shape, x),
            crate::app::types::FadeShape::Quadratic => {
                let y = 1.0 - x;
                y * y
            }
            crate::app::types::FadeShape::Cubic => {
                let y = 1.0 - x;
                y * y * y
            }
        }
    }

    pub(super) fn editor_apply_fade_in_explicit(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        shape: crate::app::types::FadeShape,
    ) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let dur = (e - s).max(1) as f32;
            for ch in tab.ch_samples.iter_mut() {
                for i in s..e {
                    let t = (i - s) as f32 / dur;
                    let w = Self::fade_weight(shape, t);
                    ch[i] *= w;
                }
            }
            tab.dirty = true;
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn editor_apply_fade_out_explicit(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        shape: crate::app::types::FadeShape,
    ) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let dur = (e - s).max(1) as f32;
            for ch in tab.ch_samples.iter_mut() {
                for i in s..e {
                    let t = (i - s) as f32 / dur;
                    let w = Self::fade_weight_out(shape, t);
                    ch[i] *= w;
                }
            }
            tab.dirty = true;
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    #[allow(dead_code)]
    pub(super) fn editor_selected_range(
        tab: &crate::app::types::EditorTab,
    ) -> Option<(usize, usize)> {
        if let Some(r) = tab.selection {
            if r.1 > r.0 {
                return Some(r);
            }
        }
        None
    }

    pub(super) fn editor_clamp_ranges(tab: &mut crate::app::types::EditorTab) {
        let len = tab.samples_len;
        let clamp_range = |range: &mut Option<(usize, usize)>| {
            if let Some((mut s, mut e)) = *range {
                if s > len {
                    s = len;
                }
                if e > len {
                    e = len;
                }
                if e <= s {
                    *range = None;
                } else {
                    *range = Some((s, e));
                }
            }
        };
        clamp_range(&mut tab.selection);
        if tab.selection.is_none() {
            // Frequency band is meaningless without a time selection.
            tab.freq_selection = None;
        }
        if let Some((lo, hi)) = tab.freq_selection {
            let lo = lo.max(0.0);
            let hi = hi.max(0.0);
            let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
            tab.freq_selection = if hi - lo > f32::EPSILON {
                Some((lo, hi))
            } else {
                None
            };
        }
        clamp_range(&mut tab.ab_loop);
        clamp_range(&mut tab.loop_region);
        clamp_range(&mut tab.trim_range);
        clamp_range(&mut tab.fade_in_range);
        clamp_range(&mut tab.fade_out_range);
        let max_view = len.saturating_sub(1);
        if tab.view_offset > max_view {
            tab.view_offset = max_view;
        }
        Self::editor_sync_view_offset_exact(tab);
        if tab.loop_xfade_samples > len / 2 {
            tab.loop_xfade_samples = len / 2;
        }
        if tab
            .selection_anchor_sample
            .map(|v| v > len)
            .unwrap_or(false)
        {
            tab.selection_anchor_sample = None;
            tab.right_drag_mode = None;
        }
        Self::editor_clamp_vertical_view(tab);
        if tab.preview_offset_samples.map(|v| v > len).unwrap_or(false) {
            tab.preview_offset_samples = None;
        }
        Self::update_loop_markers_dirty(tab);
    }

    pub(super) fn on_audio_length_changed(&mut self, tab_idx: usize) {
        let len = if let Some(tab) = self.tabs.get_mut(tab_idx) {
            Self::editor_clamp_ranges(tab);
            tab.samples_len
        } else {
            0
        };
        let clamped_pos = if len == 0 {
            0usize
        } else {
            let pos = self
                .audio
                .shared
                .play_pos
                .load(std::sync::atomic::Ordering::Relaxed);
            pos.min(len.saturating_sub(1))
        };
        self.audio
            .shared
            .play_pos
            .store(clamped_pos, std::sync::atomic::Ordering::Relaxed);
        self.audio
            .shared
            .play_pos_f
            .store(clamped_pos as f64, std::sync::atomic::Ordering::Relaxed);
    }

    pub(super) fn editor_apply_reverse_range(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            // Sub-range reverse: smooth the joins with a short crossfade so
            // the transition into/out of the reversed span stays click-free.
            let xf = if s > 0 || e < tab.samples_len {
                crate::wave::splice_xfade_samples(tab.buffer_sample_rate.max(1), e - s, e - s)
                    .min(256)
            } else {
                0
            };
            for ch in tab.ch_samples.iter_mut() {
                crate::wave::reverse_range_with_crossfade(ch, s, e, xf);
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
        self.audio.set_loop_crossfade(0, 0);
    }

    /// Insert `insert` (one Vec per channel, equal lengths, matching the
    /// tab's channel count) at buffer position `pos`. Markers, loop regions,
    /// selections, and fade ranges at or after `pos` shift right by the
    /// inserted length so existing annotations keep pointing at the same
    /// audio. Returns false if the shape doesn't match.
    pub(super) fn editor_insert_channels_at(
        &mut self,
        tab_idx: usize,
        pos: usize,
        insert: Vec<Vec<f32>>,
    ) -> bool {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return false;
            };
            let ins_len = insert.first().map(|c| c.len()).unwrap_or(0);
            if ins_len == 0
                || insert.len() != tab.ch_samples.len()
                || insert.iter().any(|c| c.len() != ins_len)
            {
                return false;
            }
            let pos = pos.min(tab.samples_len);
            let undo_state = Self::capture_undo_state(tab);
            for (ch, ins) in tab.ch_samples.iter_mut().zip(insert.iter()) {
                ch.splice(pos..pos, ins.iter().copied());
            }
            tab.samples_len += ins_len;
            let shift_markers = |markers: &mut Vec<crate::markers::MarkerEntry>| {
                for mk in markers.iter_mut() {
                    if mk.sample >= pos {
                        mk.sample += ins_len;
                    }
                }
            };
            shift_markers(&mut tab.markers);
            shift_markers(&mut tab.markers_committed);
            shift_markers(&mut tab.markers_applied);
            let shift_range = |range: &mut Option<(usize, usize)>| {
                if let Some((a, b)) = range.as_mut() {
                    if *a >= pos {
                        *a += ins_len;
                    }
                    if *b >= pos {
                        *b += ins_len;
                    }
                }
            };
            shift_range(&mut tab.selection);
            shift_range(&mut tab.ab_loop);
            shift_range(&mut tab.loop_region);
            shift_range(&mut tab.loop_region_applied);
            shift_range(&mut tab.loop_region_committed);
            shift_range(&mut tab.loop_markers_saved);
            shift_range(&mut tab.trim_range);
            shift_range(&mut tab.fade_in_range);
            shift_range(&mut tab.fade_out_range);
            for (a, b) in tab.extra_selections.iter_mut() {
                if *a >= pos {
                    *a += ins_len;
                }
                if *b >= pos {
                    *b += ins_len;
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
        true
    }

    /// Insert `ms` of silence at `pos` (buffer samples).
    pub(super) fn editor_insert_silence_at(&mut self, tab_idx: usize, pos: usize, ms: f32) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let sr = tab.buffer_sample_rate.max(1);
        let n = ((f64::from(ms.max(0.0)) / 1000.0) * f64::from(sr)).round() as usize;
        if n == 0 {
            return false;
        }
        let channels = tab.ch_samples.len().max(1);
        self.editor_insert_channels_at(tab_idx, pos, vec![vec![0.0f32; n]; channels])
    }

    /// Silence-insert target: selection start when a selection exists,
    /// otherwise the current playhead position.
    pub(super) fn editor_insert_position(&self, tab_idx: usize) -> usize {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return 0;
        };
        if let Some((s, _)) = Self::editor_selected_range(tab) {
            return s;
        }
        let play_pos = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        self.map_audio_to_display_sample(tab, play_pos)
            .min(tab.samples_len)
    }

    /// Copy the selected range into the in-app audio clipboard. Returns the
    /// copied length in samples (0 = nothing copied).
    pub(super) fn editor_copy_selection_to_audio_clipboard(
        &mut self,
        tab_idx: usize,
        notify: bool,
    ) -> usize {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return 0;
        };
        let Some((s, e)) = Self::editor_selected_range(tab) else {
            if notify {
                self.push_toast(
                    super::types::ToastSeverity::Info,
                    "Select a range to copy audio",
                );
            }
            return 0;
        };
        let sr = tab.buffer_sample_rate.max(1);
        let channels: Vec<Vec<f32>> = tab
            .ch_samples
            .iter()
            .map(|ch| {
                let end = e.min(ch.len());
                let start = s.min(end);
                ch[start..end].to_vec()
            })
            .collect();
        let len = channels.first().map(|c| c.len()).unwrap_or(0);
        if len == 0 {
            return 0;
        }
        self.editor_audio_clipboard = Some(super::types::EditorAudioClip {
            channels,
            sample_rate: sr,
        });
        if notify {
            self.push_toast(
                super::types::ToastSeverity::Info,
                format!("Copied {:.2} s of audio", len as f64 / f64::from(sr)),
            );
        }
        len
    }

    /// Cut = copy selection to the audio clipboard, then delete+join it.
    pub(super) fn editor_cut_selection_to_audio_clipboard(&mut self, tab_idx: usize) -> bool {
        let range = self
            .tabs
            .get(tab_idx)
            .and_then(|tab| Self::editor_selected_range(tab));
        let Some((s, e)) = range else {
            self.push_toast(
                super::types::ToastSeverity::Info,
                "Select a range to cut audio",
            );
            return false;
        };
        if self.editor_copy_selection_to_audio_clipboard(tab_idx, false) == 0 {
            return false;
        }
        self.editor_delete_range_and_join(tab_idx, (s, e));
        self.push_toast(
            super::types::ToastSeverity::Info,
            "Cut selection (Ctrl+Z to undo)",
        );
        true
    }

    /// Paste-insert the audio clipboard at the selection start / playhead.
    /// The clip is resampled to the tab's buffer rate when needed and its
    /// channel layout is adapted (repeat modulo) to the tab's channel count.
    pub(super) fn editor_paste_insert_from_audio_clipboard(&mut self, tab_idx: usize) -> bool {
        self.editor_paste_from_audio_clipboard(tab_idx, super::types::PasteMode::Insert)
    }

    /// Prepare the clipboard contents for pasting into `tab_idx`: resample to
    /// the buffer rate, adapt the channel layout, trim to rectangular.
    fn editor_prepared_clipboard_channels(&mut self, tab_idx: usize) -> Option<Vec<Vec<f32>>> {
        let clip = match self.editor_audio_clipboard.clone() {
            Some(clip) => clip,
            None => {
                self.push_toast(
                    super::types::ToastSeverity::Info,
                    "Audio clipboard is empty (Ctrl+C/X in the editor copies audio)",
                );
                return None;
            }
        };
        let (target_sr, target_ch) = {
            let tab = self.tabs.get(tab_idx)?;
            (tab.buffer_sample_rate.max(1), tab.ch_samples.len().max(1))
        };
        let mut channels = clip.channels;
        if clip.sample_rate != target_sr {
            channels = crate::wave::resample_channels_quality(
                &channels,
                clip.sample_rate,
                target_sr,
                crate::wave::ResampleQuality::Best,
            );
        }
        if channels.is_empty() {
            return None;
        }
        if channels.len() != target_ch {
            channels = (0..target_ch)
                .map(|i| channels[i % channels.len()].clone())
                .collect();
        }
        // Resamplers can differ by a sample or two across channels; trim to
        // the shortest so the paste stays rectangular.
        let min_len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        if min_len == 0 {
            return None;
        }
        for ch in channels.iter_mut() {
            ch.truncate(min_len);
        }
        Some(channels)
    }

    /// Paste the audio clipboard at the selection start / playhead using the
    /// given mode: Insert (splice in, markers shift), Mix (sum into the
    /// existing audio, length unchanged), or CrossfadeInsert (insert with
    /// equal-power crossfaded joins).
    pub(super) fn editor_paste_from_audio_clipboard(
        &mut self,
        tab_idx: usize,
        mode: super::types::PasteMode,
    ) -> bool {
        let Some(channels) = self.editor_prepared_clipboard_channels(tab_idx) else {
            return false;
        };
        let paste_len = channels.first().map(|c| c.len()).unwrap_or(0);
        let pos = self.editor_insert_position(tab_idx);
        let sr = self
            .tabs
            .get(tab_idx)
            .map(|t| t.buffer_sample_rate.max(1))
            .unwrap_or(1);
        let ok = match mode {
            super::types::PasteMode::Insert => {
                self.editor_insert_channels_at(tab_idx, pos, channels)
            }
            super::types::PasteMode::Mix => self.editor_mix_channels_at(tab_idx, pos, &channels),
            super::types::PasteMode::CrossfadeInsert => {
                let xfade = crate::wave::splice_xfade_samples(sr, paste_len, paste_len).min(2048);
                self.editor_insert_channels_at_with_xfade(tab_idx, pos, channels, xfade)
            }
        };
        if ok {
            let verb = match mode {
                super::types::PasteMode::Insert => "Pasted",
                super::types::PasteMode::Mix => "Mix-pasted",
                super::types::PasteMode::CrossfadeInsert => "Crossfade-pasted",
            };
            self.push_toast(
                super::types::ToastSeverity::Info,
                format!(
                    "{verb} {:.2} s of audio (Ctrl+Z to undo)",
                    paste_len as f64 / f64::from(sr)
                ),
            );
        }
        ok
    }

    /// Sum `mix` into the existing audio starting at `pos` (length unchanged;
    /// samples past the end of the buffer are dropped). No marker shifts.
    pub(super) fn editor_mix_channels_at(
        &mut self,
        tab_idx: usize,
        pos: usize,
        mix: &[Vec<f32>],
    ) -> bool {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return false;
            };
            let mix_len = mix.first().map(|c| c.len()).unwrap_or(0);
            if mix_len == 0 || mix.len() != tab.ch_samples.len() || pos >= tab.samples_len {
                return false;
            }
            let undo_state = Self::capture_undo_state(tab);
            for (ch, add) in tab.ch_samples.iter_mut().zip(mix.iter()) {
                let end = (pos + add.len()).min(ch.len());
                for (dst, src) in ch[pos..end].iter_mut().zip(add.iter()) {
                    *dst += *src;
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
        true
    }

    /// Insert with equal-power crossfaded joins: the first/last `xfade`
    /// samples of the inserted material blend against the audio that used to
    /// sit at the insert point (start joins with the preceding samples'
    /// continuation, end joins into the following samples).
    pub(super) fn editor_insert_channels_at_with_xfade(
        &mut self,
        tab_idx: usize,
        pos: usize,
        mut insert: Vec<Vec<f32>>,
        xfade: usize,
    ) -> bool {
        if xfade == 0 {
            return self.editor_insert_channels_at(tab_idx, pos, insert);
        }
        // Blend the insert edges against the neighbouring original audio
        // BEFORE splicing: the start of the clip fades in over the tail of
        // what precedes `pos`, the end fades out into what follows `pos`.
        {
            let Some(tab) = self.tabs.get(tab_idx) else {
                return false;
            };
            let ins_len = insert.first().map(|c| c.len()).unwrap_or(0);
            if ins_len == 0 || insert.len() != tab.ch_samples.len() {
                return false;
            }
            let pos = pos.min(tab.samples_len);
            let xf = xfade.min(ins_len / 2);
            for (ci, ins) in insert.iter_mut().enumerate() {
                let orig = &tab.ch_samples[ci];
                // Fade-in against the samples immediately before pos.
                for k in 0..xf {
                    let t = (k as f32 + 0.5) / xf as f32;
                    let (win, wout) = ((t * std::f32::consts::FRAC_PI_2).sin(), (t * std::f32::consts::FRAC_PI_2).cos());
                    let prev = if pos >= xf - k {
                        orig.get(pos + k - xf).copied().unwrap_or(0.0)
                    } else {
                        0.0
                    };
                    ins[k] = ins[k] * win + prev * wout;
                }
                // Fade-out into the samples at/after pos.
                for k in 0..xf {
                    let t = (k as f32 + 0.5) / xf as f32;
                    let (wout, win) = ((t * std::f32::consts::FRAC_PI_2).cos(), (t * std::f32::consts::FRAC_PI_2).sin());
                    let next = orig.get(pos + k).copied().unwrap_or(0.0);
                    let idx = ins_len - xf + k;
                    ins[idx] = ins[idx] * wout + next * win;
                }
            }
        }
        self.editor_insert_channels_at(tab_idx, pos, insert)
    }

    /// Write a linearly interpolated pencil segment into the target channels.
    pub(crate) fn editor_pencil_write_segment(
        tab: &mut super::types::EditorTab,
        channels: &[usize],
        from: (usize, f32),
        to: (usize, f32),
    ) {
        let len = tab.samples_len;
        if len == 0 {
            return;
        }
        let (a, av, b, bv) = if from.0 <= to.0 {
            (from.0, from.1, to.0, to.1)
        } else {
            (to.0, to.1, from.0, from.1)
        };
        let a = a.min(len - 1);
        let b = b.min(len - 1);
        let n = b - a;
        for &ci in channels {
            let Some(ch) = tab.ch_samples.get_mut(ci) else {
                continue;
            };
            for i in a..=b {
                let t = if n == 0 {
                    1.0
                } else {
                    (i - a) as f32 / n as f32
                };
                let v = av + (bv - av) * t;
                if let Some(s) = ch.get_mut(i) {
                    // Keep float headroom, but bound runaway values.
                    *s = v.clamp(-4.0, 4.0);
                }
            }
        }
    }

    /// Finish a pencil stroke: push the undo state captured at stroke start
    /// and run the shared destructive-apply pipeline.
    pub(super) fn editor_pencil_commit(&mut self, tab_idx: usize) {
        let undo_state = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let Some(undo) = tab.pencil_undo.take() else {
                return;
            };
            tab.pencil_last_point = None;
            tab.pencil_stroke_channels.clear();
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            *undo
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    #[cfg(feature = "kittest")]
    pub fn test_pencil_stroke(&mut self, from_frac: f32, amp0: f32, to_frac: f32, amp1: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return false;
            };
            if tab.samples_len == 0 {
                return false;
            }
            let a = ((tab.samples_len as f32) * from_frac.clamp(0.0, 1.0)) as usize;
            let b = ((tab.samples_len as f32) * to_frac.clamp(0.0, 1.0)) as usize;
            tab.pencil_undo = Some(Box::new(Self::capture_undo_state(tab)));
            let channels: Vec<usize> = (0..tab.ch_samples.len()).collect();
            Self::editor_pencil_write_segment(tab, &channels, (a, amp0), (b, amp1));
        }
        self.editor_pencil_commit(tab_idx);
        true
    }

    /// Mean of a sample slice in f64 (stable for long buffers).
    pub(super) fn dc_mean_over(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = samples.iter().map(|&v| f64::from(v)).sum();
        (sum / samples.len() as f64) as f32
    }

    /// Subtract the mean of `ch[s..e]` from that range in place.
    pub(super) fn dc_remove_range(ch: &mut [f32], s: usize, e: usize) {
        let end = e.min(ch.len());
        let start = s.min(end);
        if start >= end {
            return;
        }
        let mean = Self::dc_mean_over(&ch[start..end]);
        if mean == 0.0 {
            return;
        }
        for v in &mut ch[start..end] {
            *v -= mean;
        }
    }

    /// Channels a destructive range edit applies to. A Custom channel view
    /// scopes edits to its visible channels; Mixdown/All (or a Custom view
    /// covering every channel) edits all of them, returned as `None`.
    pub(super) fn editor_channel_mask(tab: &crate::app::types::EditorTab) -> Option<Vec<bool>> {
        if !matches!(
            tab.channel_view.mode,
            crate::app::types::ChannelViewMode::Custom
        ) {
            return None;
        }
        let total = tab.ch_samples.len();
        let vis = tab.channel_view.visible_indices(total);
        if vis.is_empty() || vis.len() >= total {
            return None;
        }
        let mut mask = vec![false; total];
        for i in vis {
            mask[i] = true;
        }
        Some(mask)
    }

    /// Inspector caption for the current edit scope ("ch 1, 3"), or `None`
    /// when edits apply to every channel.
    pub(super) fn editor_channel_mask_label(tab: &crate::app::types::EditorTab) -> Option<String> {
        let mask = Self::editor_channel_mask(tab)?;
        let chans: Vec<String> = mask
            .iter()
            .enumerate()
            .filter(|(_, &on)| on)
            .map(|(i, _)| (i + 1).to_string())
            .collect();
        Some(format!("ch {}", chans.join(", ")))
    }

    /// Run the de-click detector over the active selection (or the whole
    /// file) and store the spans for the red marker overlay. Synchronous:
    /// the detector is O(n) with a small constant, fine for UI-thread use.
    /// Scan for clipped (flat-at-the-rail) runs; results share the de-click
    /// red-band overlay via `tab.declick_scan`.
    pub(super) fn editor_declip_scan(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        if tab.loading || tab.samples_len == 0 {
            return;
        }
        let sens = tab.tool_state.declip_sensitivity.clamp(0.0, 1.0);
        let range = Self::editor_selected_range(tab);
        let sr = tab.buffer_sample_rate.max(1);
        let cfg = crate::app::declip::DeclipConfig {
            sensitivity: sens,
            ..Default::default()
        };
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for ch in &tab.ch_samples {
            spans.extend(crate::app::declip::detect_clipped(ch, sr, &cfg, range));
        }
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (s, e) in spans {
            match merged.last_mut() {
                Some((_, pe)) if s <= *pe => *pe = (*pe).max(e),
                _ => merged.push((s, e)),
            }
        }
        tab.declick_scan = Some(crate::app::types::DeclickScan {
            sensitivity: sens,
            spans: merged,
            range,
        });
    }

    pub(super) fn editor_declick_scan(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        if tab.loading || tab.samples_len == 0 {
            return;
        }
        let sens = tab.tool_state.declick_sensitivity.clamp(0.0, 1.0);
        let range = Self::editor_selected_range(tab);
        let sr = tab.buffer_sample_rate.max(1);
        let cfg = crate::app::declick::DeclickConfig {
            sensitivity: sens,
            ..Default::default()
        };
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for ch in &tab.ch_samples {
            spans.extend(crate::app::declick::detect_clicks(ch, sr, &cfg, range));
        }
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (s, e) in spans {
            match merged.last_mut() {
                Some((_, pe)) if s <= *pe => *pe = (*pe).max(e),
                _ => merged.push((s, e)),
            }
        }
        tab.declick_scan = Some(crate::app::types::DeclickScan {
            sensitivity: sens,
            spans: merged,
            range,
        });
    }

    #[cfg(feature = "kittest")]
    pub fn test_declick_scan(&mut self) -> usize {
        let Some(tab_idx) = self.active_tab else {
            return 0;
        };
        self.editor_declick_scan(tab_idx);
        self.tabs
            .get(tab_idx)
            .and_then(|t| t.declick_scan.as_ref())
            .map(|s| s.spans.len())
            .unwrap_or(0)
    }

    #[cfg(feature = "kittest")]
    pub fn test_declick_apply(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let sens = self
            .tabs
            .get(tab_idx)
            .map(|t| t.tool_state.declick_sensitivity)
            .unwrap_or(0.5);
        let range = self
            .tabs
            .get(tab_idx)
            .and_then(Self::editor_selected_range);
        self.spawn_editor_apply_for_tab_range(
            tab_idx,
            crate::app::types::ToolKind::DeClick,
            sens,
            range,
        );
        self.editor_apply_state.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_declip_scan(&mut self) -> usize {
        let Some(tab_idx) = self.active_tab else {
            return 0;
        };
        self.editor_declip_scan(tab_idx);
        self.tabs
            .get(tab_idx)
            .and_then(|t| t.declick_scan.as_ref())
            .map(|s| s.spans.len())
            .unwrap_or(0)
    }

    #[cfg(feature = "kittest")]
    pub fn test_declip_apply(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let sens = self
            .tabs
            .get(tab_idx)
            .map(|t| t.tool_state.declip_sensitivity)
            .unwrap_or(0.5);
        let range = self
            .tabs
            .get(tab_idx)
            .and_then(Self::editor_selected_range);
        self.spawn_editor_apply_for_tab_range(
            tab_idx,
            crate::app::types::ToolKind::DeClip,
            sens,
            range,
        );
        self.editor_apply_state.is_some()
    }

    /// Remove per-channel DC bias over `range` (subtract the range mean).
    pub(super) fn editor_apply_remove_dc_range(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let mask = Self::editor_channel_mask(tab);
            for (ci, ch) in tab.ch_samples.iter_mut().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                Self::dc_remove_range(ch, s, e);
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    /// Flip waveform polarity over `range` (sample-exact, no smoothing —
    /// use zero-cross snap for click-free boundaries on partial ranges).
    /// Invert `[s, e)` of one channel. With `fade > 0`, interior boundaries
    /// (not touching the buffer edges) ramp the gain 1 -> -1 over `fade`
    /// samples so the flip doesn't step-discontinue against untouched audio.
    pub(crate) fn invert_polarity_channel_range(ch: &mut [f32], s: usize, e: usize, fade: usize) {
        let e = e.min(ch.len());
        if e <= s {
            return;
        }
        let len = e - s;
        let fade = fade.min(len / 2);
        let fade_at_start = fade > 0 && s > 0;
        let fade_at_end = fade > 0 && e < ch.len();
        for i in s..e {
            let rel = i - s;
            let from_end = e - 1 - i;
            let g = if fade_at_start && rel < fade {
                1.0 - 2.0 * ((rel + 1) as f32 / (fade + 1) as f32)
            } else if fade_at_end && from_end < fade {
                1.0 - 2.0 * ((from_end + 1) as f32 / (fade + 1) as f32)
            } else {
                -1.0
            };
            ch[i] *= g;
        }
    }

    pub(super) fn editor_apply_invert_polarity_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
    ) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let mask = Self::editor_channel_mask(tab);
            let fade = if tab.tool_state.invert_smooth_boundaries {
                // ~2 ms polarity crossfade at interior boundaries.
                ((tab.buffer_sample_rate.max(1) as f32) * 0.002).round() as usize
            } else {
                0
            };
            for (ci, ch) in tab.ch_samples.iter_mut().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                Self::invert_polarity_channel_range(ch, s, e, fade);
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn editor_apply_trim_range(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            for ch in tab.ch_samples.iter_mut() {
                *ch = ch[s..e].to_vec();
            }
            tab.samples_len = e - s;
            let remap_trim_markers = |markers: &[crate::markers::MarkerEntry]| {
                let mut out: Vec<crate::markers::MarkerEntry> = markers
                    .iter()
                    .filter_map(|marker| {
                        if marker.sample < s || marker.sample >= e {
                            return None;
                        }
                        Some(crate::markers::MarkerEntry {
                            sample: marker.sample.saturating_sub(s),
                            label: marker.label.clone(),
                        })
                    })
                    .collect();
                out.sort_by_key(|marker| marker.sample);
                out.dedup_by(|a, b| a.sample == b.sample && a.label == b.label);
                out
            };
            tab.markers = remap_trim_markers(&tab.markers);
            tab.markers_committed = remap_trim_markers(&tab.markers_committed);
            tab.markers_applied = remap_trim_markers(&tab.markers_applied);
            tab.view_offset = 0;
            Self::editor_sync_view_offset_exact(tab);
            tab.selection = None;
            tab.extra_selections.clear();
            tab.ab_loop = None;
            tab.loop_region = None;
            tab.loop_region_committed = None;
            tab.loop_region_applied = None;
            tab.trim_range = None;
            Self::editor_invalidate_destructive_preview_state(tab);
            tab.dirty = true;
            Self::update_markers_dirty(tab);
            Self::update_loop_markers_dirty(tab);
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    /// Trim to multiple ranges: keep only the audio inside each range, concatenated in order.
    pub(super) fn editor_apply_trim_multi_ranges(
        &mut self,
        tab_idx: usize,
        ranges: Vec<(usize, usize)>,
    ) {
        let undo_state = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            if ranges.is_empty() {
                return;
            }
            if ranges.iter().any(|&(_s, e)| e > tab.samples_len) {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let new_len: usize = ranges.iter().map(|&(s, e)| e - s).sum();
            for ch in tab.ch_samples.iter_mut() {
                let new_ch: Vec<f32> = ranges
                    .iter()
                    .flat_map(|&(s, e)| {
                        let end = e.min(ch.len());
                        let start = s.min(end);
                        ch[start..end].to_vec()
                    })
                    .collect();
                *ch = new_ch;
            }
            tab.samples_len = new_len;
            let remap_markers =
                |markers: &[crate::markers::MarkerEntry]| -> Vec<crate::markers::MarkerEntry> {
                    let mut out = Vec::new();
                    let mut offset = 0usize;
                    for &(s, e) in &ranges {
                        for m in markers {
                            if m.sample >= s && m.sample < e {
                                out.push(crate::markers::MarkerEntry {
                                    sample: offset + (m.sample - s),
                                    label: m.label.clone(),
                                });
                            }
                        }
                        offset += e - s;
                    }
                    out.sort_by_key(|m| m.sample);
                    out.dedup_by(|a, b| a.sample == b.sample && a.label == b.label);
                    out
                };
            tab.markers = remap_markers(&tab.markers);
            tab.markers_committed = remap_markers(&tab.markers_committed);
            tab.markers_applied = remap_markers(&tab.markers_applied);
            tab.view_offset = 0;
            Self::editor_sync_view_offset_exact(tab);
            tab.selection = None;
            tab.extra_selections.clear();
            tab.ab_loop = None;
            tab.loop_region = None;
            tab.loop_region_committed = None;
            tab.loop_region_applied = None;
            tab.trim_range = None;
            Self::editor_invalidate_destructive_preview_state(tab);
            tab.dirty = true;
            Self::update_markers_dirty(tab);
            Self::update_loop_markers_dirty(tab);
            Self::editor_clamp_ranges(tab);
            undo_state
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    /// Cut multiple ranges: delete audio inside each range, join remaining parts.
    pub(super) fn editor_delete_multi_ranges_and_join(
        &mut self,
        tab_idx: usize,
        ranges: Vec<(usize, usize)>,
    ) {
        let undo_state = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            if ranges.is_empty() {
                return;
            }
            if ranges.iter().any(|&(_s, e)| e > tab.samples_len) {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let removed: usize = ranges.iter().map(|&(s, e)| e - s).sum();
            for ch in tab.ch_samples.iter_mut() {
                let mut new_ch: Vec<f32> = Vec::with_capacity(ch.len().saturating_sub(removed));
                let mut prev_end = 0usize;
                for &(s, e) in &ranges {
                    let seg_end = s.min(ch.len());
                    let seg_start = prev_end.min(seg_end);
                    new_ch.extend_from_slice(&ch[seg_start..seg_end]);
                    prev_end = e;
                }
                let tail_start = prev_end.min(ch.len());
                new_ch.extend_from_slice(&ch[tail_start..]);
                *ch = new_ch;
            }
            tab.samples_len = tab.samples_len.saturating_sub(removed);
            tab.selection = None;
            tab.extra_selections.clear();
            tab.loop_region = None;
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            undo_state
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn begin_trim_virtual_job(&mut self, tab_idx: usize, range: (usize, usize)) -> bool {
        if self.virtual_trim_state.is_some() {
            return false;
        }
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        if tab.loading || tab.ch_samples.is_empty() {
            return false;
        }
        let (s, e) = if range.0 <= range.1 {
            range
        } else {
            (range.1, range.0)
        };
        if e <= s || e > tab.samples_len {
            return false;
        }
        let source_path = tab.path.clone();
        let source_name = tab.display_name.clone();
        let source_channel_count = tab.ch_samples.len();
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let source_sr = self
            .item_for_path(&source_path)
            .and_then(|item| {
                if item.source == crate::app::types::MediaSource::Virtual {
                    item.virtual_state.as_ref().map(|state| state.sample_rate)
                } else {
                    None
                }
            })
            .or_else(|| self.effective_sample_rate_for_path(&source_path))
            .filter(|v| *v > 0)
            .unwrap_or(out_sr);
        let bits_per_sample = self
            .bit_depth_override
            .get(&source_path)
            .copied()
            .map(|d| d.bits_per_sample())
            .or_else(|| {
                self.item_for_path(&source_path).and_then(|item| {
                    if item.source == crate::app::types::MediaSource::Virtual {
                        item.virtual_state
                            .as_ref()
                            .map(|state| state.bits_per_sample)
                    } else {
                        None
                    }
                })
            })
            .or_else(|| self.meta_for_path(&source_path).map(|m| m.bits_per_sample))
            .filter(|v| *v > 0)
            .unwrap_or(32);
        let map_to_source = |pos: usize| -> usize {
            if source_sr == out_sr {
                return pos;
            }
            ((pos as u128)
                .saturating_mul(source_sr as u128)
                .saturating_add((out_sr / 2) as u128)
                / (out_sr as u128)) as usize
        };
        let source_start = map_to_source(s);
        let mut source_end = map_to_source(e);
        if source_end <= source_start {
            source_end = source_start.saturating_add(1);
        }
        let source_ref = if self.is_virtual_path(&source_path) {
            crate::app::types::VirtualSourceRef::VirtualPath(source_path.clone())
        } else {
            crate::app::types::VirtualSourceRef::FilePath(source_path.clone())
        };
        let insert_idx = self.selected.map(|row| row.saturating_add(1));

        self.clear_preview_if_any(tab_idx);
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.selection = None;
            tab.selection_anchor_sample = None;
            tab.trim_range = None;
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
        }
        self.audio.stop();

        self.virtual_trim_state = Some(VirtualTrimState {
            source_path,
            source_name,
            range: (s, e),
            copied_frames: 0,
            total_frames: e.saturating_sub(s),
            channels: (0..source_channel_count).map(|_| Vec::new()).collect(),
            out_sr,
            source_sr,
            bits_per_sample,
            source_start,
            source_end,
            source_ref,
            insert_idx,
            phase: VirtualTrimPhase::Copying,
            rx: None,
            started_at: std::time::Instant::now(),
        });
        true
    }

    pub(super) fn cancel_virtual_trim_job(&mut self) {
        self.virtual_trim_state = None;
    }

    pub(super) fn virtual_trim_status_for_tab(&self, tab_idx: usize) -> Option<(String, f32)> {
        let state = self.virtual_trim_state.as_ref()?;
        let tab = self.tabs.get(tab_idx)?;
        if tab.path != state.source_path {
            return None;
        }
        let progress = if state.total_frames == 0 {
            0.0
        } else {
            (state.copied_frames as f32 / state.total_frames as f32).clamp(0.0, 1.0)
        };
        let base_msg = match state.phase {
            VirtualTrimPhase::Copying => "Creating virtual trim...",
            VirtualTrimPhase::Processing => "Finalizing virtual trim...",
        };
        let elapsed = state.started_at.elapsed().as_secs_f32();
        let msg = format!("{base_msg} ({elapsed:.1}s)");
        Some((msg.to_string(), progress))
    }

    pub(super) fn tick_virtual_trim_state(&mut self, ctx: &egui::Context) {
        let mut spawn_processing = false;
        let mut ready: Option<VirtualTrimResult> = None;
        let mut disconnected = false;
        if let Some(state) = self.virtual_trim_state.as_mut() {
            match state.phase {
                VirtualTrimPhase::Copying => {
                    let Some(tab) = self.tabs.iter().find(|tab| tab.path == state.source_path)
                    else {
                        self.virtual_trim_state = None;
                        ctx.request_repaint();
                        return;
                    };
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_millis(VIRTUAL_TRIM_COPY_FRAME_BUDGET_MS);
                    while state.copied_frames < state.total_frames {
                        let next = state
                            .copied_frames
                            .saturating_add(VIRTUAL_TRIM_COPY_CHUNK_FRAMES)
                            .min(state.total_frames);
                        let src_start = state.range.0.saturating_add(state.copied_frames);
                        let src_end = state.range.0.saturating_add(next);
                        for (ch_idx, dst) in state.channels.iter_mut().enumerate() {
                            if let Some(src) = tab.ch_samples.get(ch_idx) {
                                let end = src_end.min(src.len());
                                let start = src_start.min(end);
                                dst.extend_from_slice(&src[start..end]);
                            }
                        }
                        state.copied_frames = next;
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                    }
                    if state.copied_frames >= state.total_frames {
                        spawn_processing = true;
                    }
                }
                VirtualTrimPhase::Processing => {
                    if let Some(rx) = state.rx.as_ref() {
                        match rx.try_recv() {
                            Ok(result) => ready = Some(result),
                            Err(std::sync::mpsc::TryRecvError::Empty) => {}
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                disconnected = true;
                            }
                        }
                    }
                }
            }
        }

        if spawn_processing {
            if let Some(state) = self.virtual_trim_state.as_mut() {
                let mut channels = std::mem::take(&mut state.channels);
                let source_path = state.source_path.clone();
                let source_name = state.source_name.clone();
                let out_sr = state.out_sr;
                let source_sr = state.source_sr;
                let bits_per_sample = state.bits_per_sample;
                let source_start = state.source_start;
                let source_end = state.source_end;
                let source_ref = state.source_ref.clone();
                let quality = Self::to_wave_resample_quality(self.src_quality);
                let (tx, rx) = std::sync::mpsc::channel::<VirtualTrimResult>();
                std::thread::spawn(move || {
                    if source_sr != out_sr {
                        for ch in channels.iter_mut() {
                            *ch = crate::wave::resample_quality(ch, out_sr, source_sr, quality);
                        }
                    }
                    let quantize_depth = match bits_per_sample {
                        0..=16 => Some(crate::wave::WavBitDepth::Pcm16),
                        17..=24 => Some(crate::wave::WavBitDepth::Pcm24),
                        _ => Some(crate::wave::WavBitDepth::Float32),
                    };
                    if let Some(depth) = quantize_depth {
                        crate::wave::quantize_channels_in_place(&mut channels, depth);
                    }
                    let audio =
                        std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(channels));
                    let meta = crate::app::WavesPreviewer::build_meta_from_audio(
                        &audio.channels,
                        source_sr,
                        bits_per_sample,
                    );
                    let _ = tx.send(VirtualTrimResult {
                        source_path,
                        source_name,
                        audio,
                        meta,
                        source_sr,
                        bits_per_sample,
                        source_start,
                        source_end,
                        source_ref,
                    });
                });
                state.phase = VirtualTrimPhase::Processing;
                state.rx = Some(rx);
            }
        }

        if let Some(result) = ready {
            let insert_idx = self.virtual_trim_state.as_ref().and_then(|s| s.insert_idx);
            let base = std::path::Path::new(&result.source_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("clip");
            let ext = std::path::Path::new(&result.source_name)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("wav");
            let name = self.unique_virtual_display_name(&format!("{base} (trim).{ext}"));
            let virtual_state = Some(crate::app::types::VirtualState {
                source: result.source_ref,
                op_chain: vec![crate::app::types::VirtualOp::Trim {
                    start: result.source_start,
                    end: result.source_end,
                }],
                sample_rate: result.source_sr,
                channels: result.audio.channels.len().max(1) as u16,
                bits_per_sample: result.bits_per_sample,
            });
            let added_path = {
                let item = self.make_virtual_item_with_meta(
                    name,
                    result.audio,
                    Some(result.meta),
                    virtual_state,
                );
                let before = self.capture_list_selection_snapshot();
                let added_path = item.path.clone();
                self.add_virtual_item(item, insert_idx);
                self.after_add_refresh();
                self.record_list_insert_from_paths(&[added_path.clone()], before);
                added_path
            };
            if let Some(row) = self.row_for_path(&added_path) {
                self.update_selection_on_click(row, egui::Modifiers::NONE);
            }
            if self.debug.cfg.enabled {
                self.debug_log(format!(
                    "virtual_trim_create_async source={} new_virtual={}",
                    result.source_path.display(),
                    added_path.display()
                ));
            }
            self.virtual_trim_state = None;
            // Process next queued virtual trim job, if any.
            if let Some((path, s, e)) = self.virtual_trim_queue.pop_front() {
                if let Some(tab_idx) = self.tabs.iter().position(|t| t.path == path) {
                    self.begin_trim_virtual_job(tab_idx, (s, e));
                }
            }
        } else if disconnected {
            self.virtual_trim_state = None;
        }
        if self.virtual_trim_state.is_some() {
            ctx.request_repaint();
        }
    }

    #[cfg_attr(not(feature = "kittest"), allow(dead_code))]
    pub(super) fn add_trim_range_as_virtual(&mut self, tab_idx: usize, range: (usize, usize)) {
        // "Set" in Trim inspector can route transport to preview mono.
        // Restore the visible editor waveform before creating/selecting the virtual item.
        self.clear_preview_if_any(tab_idx);
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let (s, e) = range;
        if e <= s || e > tab.samples_len {
            return;
        }
        let source_path = tab.path.clone();
        let source_name = tab.display_name.clone();
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let source_meta = self.meta_for_path(&source_path).cloned();
        let explicit_sr_override = self.sample_rate_override.contains_key(&source_path);
        let explicit_bits_override = self.bit_depth_override.contains_key(&source_path);
        let source_sr = self
            .item_for_path(&source_path)
            .and_then(|item| {
                if item.source == crate::app::types::MediaSource::Virtual {
                    item.virtual_state.as_ref().map(|state| state.sample_rate)
                } else {
                    None
                }
            })
            .or_else(|| self.effective_sample_rate_for_path(&source_path))
            .filter(|v| *v > 0)
            .unwrap_or(out_sr);
        let bits_per_sample = self
            .bit_depth_override
            .get(&source_path)
            .copied()
            .map(|d| d.bits_per_sample())
            .or_else(|| {
                self.item_for_path(&source_path).and_then(|item| {
                    if item.source == crate::app::types::MediaSource::Virtual {
                        item.virtual_state
                            .as_ref()
                            .map(|state| state.bits_per_sample)
                    } else {
                        None
                    }
                })
            })
            .or_else(|| source_meta.as_ref().map(|m| m.bits_per_sample))
            .filter(|v| *v > 0)
            .unwrap_or(32);
        let sample_value_kind = self
            .bit_depth_override
            .get(&source_path)
            .copied()
            .map(|depth| match depth {
                crate::wave::WavBitDepth::Float32 => crate::app::types::SampleValueKind::Float,
                crate::wave::WavBitDepth::Pcm16 | crate::wave::WavBitDepth::Pcm24 => {
                    crate::app::types::SampleValueKind::Int
                }
            })
            .or_else(|| source_meta.as_ref().map(|m| m.sample_value_kind))
            .unwrap_or(if bits_per_sample == 32 {
                crate::app::types::SampleValueKind::Float
            } else {
                crate::app::types::SampleValueKind::Int
            });
        let mut channels = Vec::with_capacity(tab.ch_samples.len());
        for ch in tab.ch_samples.iter() {
            channels.push(ch[s..e].to_vec());
        }
        if source_sr != out_sr {
            let quality = Self::to_wave_resample_quality(self.src_quality);
            for ch in channels.iter_mut() {
                *ch = crate::wave::resample_quality(ch, out_sr, source_sr, quality);
            }
        }
        let quantize_depth = match bits_per_sample {
            0..=16 => Some(crate::wave::WavBitDepth::Pcm16),
            17..=24 => Some(crate::wave::WavBitDepth::Pcm24),
            _ => Some(crate::wave::WavBitDepth::Float32),
        };
        if let Some(depth) = quantize_depth {
            crate::wave::quantize_channels_in_place(&mut channels, depth);
        }
        let audio = std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(channels.clone()));
        let map_to_source = |pos: usize| -> usize {
            if source_sr == out_sr {
                return pos;
            }
            ((pos as u128)
                .saturating_mul(source_sr as u128)
                .saturating_add((out_sr / 2) as u128)
                / (out_sr as u128)) as usize
        };
        let source_start = map_to_source(s);
        let mut source_end = map_to_source(e);
        if source_end <= source_start {
            source_end = source_start.saturating_add(1);
        }
        let source_ref = if self.is_virtual_path(&source_path) {
            crate::app::types::VirtualSourceRef::VirtualPath(source_path.clone())
        } else {
            crate::app::types::VirtualSourceRef::FilePath(source_path.clone())
        };
        let virtual_state = Some(crate::app::types::VirtualState {
            source: source_ref,
            op_chain: vec![crate::app::types::VirtualOp::Trim {
                start: source_start,
                end: source_end,
            }],
            sample_rate: source_sr,
            channels: audio.channels.len().max(1) as u16,
            bits_per_sample,
        });
        if self.debug.cfg.enabled {
            self.debug_log(format!(
                "virtual create source={} trim={}..{} sr={} ch={} bits={}",
                source_path.display(),
                source_start,
                source_end,
                source_sr,
                audio.channels.len().max(1),
                bits_per_sample
            ));
        }
        let base = std::path::Path::new(&source_name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("clip");
        let ext = std::path::Path::new(&source_name)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("wav");
        let name = self.unique_virtual_display_name(&format!("{base} (trim).{ext}"));
        let mut item =
            self.make_virtual_item(name, audio, source_sr, bits_per_sample, virtual_state);
        if let Some(meta) = item.meta.as_mut() {
            meta.sample_rate = source_sr;
            meta.bits_per_sample = bits_per_sample;
            meta.sample_value_kind = sample_value_kind;
            if !explicit_sr_override && !explicit_bits_override {
                meta.bit_rate_bps = source_meta.as_ref().and_then(|m| m.bit_rate_bps);
            } else {
                meta.bit_rate_bps = None;
            }
        }
        let before = self.capture_list_selection_snapshot();
        let insert_idx = self.selected.map(|row| row.saturating_add(1));
        let added_path = item.path.clone();
        self.add_virtual_item(item, insert_idx);
        self.after_add_refresh();
        self.record_list_insert_from_paths(&[added_path.clone()], before);
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.selection = None;
            tab.selection_anchor_sample = None;
            tab.trim_range = None;
        }
        if let Some(row) = self.row_for_path(&added_path) {
            self.update_selection_on_click(row, egui::Modifiers::NONE);
        }
        if self.debug.cfg.enabled {
            self.debug_log(format!(
                "virtual_trim_create source={} trim={}..{} new_virtual={}",
                source_path.display(),
                source_start,
                source_end,
                added_path.display()
            ));
        }
    }

    pub(super) fn editor_apply_gain_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        gain_db: f32,
    ) {
        self.editor_apply_gain_range_opts(tab_idx, range, gain_db, true);
    }

    /// `respect_channel_view=false` forces all channels regardless of the
    /// tab's channel view — used by file-level gain (unified list gain).
    pub(super) fn editor_apply_gain_range_opts(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        gain_db: f32,
        respect_channel_view: bool,
    ) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let g = crate::app::helpers::db_to_amp(gain_db);
            let mask = if respect_channel_view {
                Self::editor_channel_mask(tab)
            } else {
                None
            };
            // Editing buffers keep float headroom; no clamp here.
            for (ci, ch) in tab.ch_samples.iter_mut().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                for i in s..e {
                    ch[i] *= g;
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
        self.notify_if_tab_over_fs(tab_idx);
    }

    pub(super) fn editor_apply_normalize_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        target_db: f32,
    ) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            // Peak across the edited channels only, so a channel-scoped
            // normalize hits the target on the channels it changes.
            let mask = Self::editor_channel_mask(tab);
            let mut peak = 0.0f32;
            for (ci, ch) in tab.ch_samples.iter().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                for &v in &ch[s..e] {
                    peak = peak.max(v.abs());
                }
            }
            if peak <= 0.0 {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let g = crate::app::helpers::db_to_amp(target_db) / peak.max(1e-12);
            // Editing buffers keep float headroom; no clamp here.
            for (ci, ch) in tab.ch_samples.iter_mut().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                for i in s..e {
                    ch[i] *= g;
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
        self.notify_if_tab_over_fs(tab_idx);
    }

    pub(super) fn editor_apply_noise_gate_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        threshold_db: f32,
        attack_ms: f32,
        release_ms: f32,
    ) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let sample_rate = tab.buffer_sample_rate.max(1);
            let params = crate::wave::NoiseGateParams {
                threshold_db,
                attack_ms,
                release_ms,
            };
            let mask = Self::editor_channel_mask(tab);
            for (ci, ch) in tab.ch_samples.iter_mut().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                let processed = crate::wave::process_noise_gate_offline(&ch[s..e], sample_rate, &params);
                ch[s..e].copy_from_slice(&processed);
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn editor_apply_eq_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        params: crate::wave::ThreeBandEqParams,
    ) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let sample_rate = tab.buffer_sample_rate.max(1);
            let mask = Self::editor_channel_mask(tab);
            for (ci, ch) in tab.ch_samples.iter_mut().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                let processed = crate::wave::process_three_band_eq_offline(&ch[s..e], sample_rate, &params);
                ch[s..e].copy_from_slice(&processed);
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn editor_apply_compressor_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        params: crate::wave::CompressorParams,
    ) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let sample_rate = tab.buffer_sample_rate.max(1);
            let mask = Self::editor_channel_mask(tab);
            for (ci, ch) in tab.ch_samples.iter_mut().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                let processed = crate::wave::process_compressor_offline(&ch[s..e], sample_rate, &params);
                ch[s..e].copy_from_slice(&processed);
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn editor_apply_mute_range(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let mask = Self::editor_channel_mask(tab);
            for (ci, ch) in tab.ch_samples.iter_mut().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                for i in s..e {
                    ch[i] = 0.0;
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    #[allow(dead_code)]
    pub(super) fn editor_apply_fade_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        in_ms: f32,
        out_ms: f32,
    ) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let sr = self.audio.shared.out_sample_rate.max(1) as f32;
            let nin = ((in_ms / 1000.0) * sr) as usize;
            let nout = ((out_ms / 1000.0) * sr) as usize;
            let mask = Self::editor_channel_mask(tab);
            for (ci, ch) in tab.ch_samples.iter_mut().enumerate() {
                if mask.as_ref().is_some_and(|m| !m[ci]) {
                    continue;
                }
                for i in 0..nin.min(e - s) {
                    let t = i as f32 / nin.max(1) as f32;
                    let w = Self::fade_weight(crate::app::types::FadeShape::SCurve, t);
                    ch[s + i] *= w;
                }
                for i in 0..nout.min(e - s) {
                    let t = i as f32 / nout.max(1) as f32;
                    let w = Self::fade_weight_out(crate::app::types::FadeShape::SCurve, t);
                    ch[e - 1 - i] *= w;
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn apply_loop_xfade_to_channels(
        channels: &mut [Vec<f32>],
        loop_start: usize,
        loop_end: usize,
        xfade: usize,
        shape: crate::app::types::LoopXfadeShape,
    ) {
        if loop_end <= loop_start || xfade == 0 {
            return;
        }
        let uses_dip = Self::loop_xfade_uses_through_zero(shape);
        let denom = (xfade.saturating_sub(1)).max(1) as f32;
        for ch in channels.iter_mut() {
            for i in 0..xfade {
                let head_idx = loop_start.saturating_add(i);
                let tail_idx = loop_end.saturating_sub(xfade).saturating_add(i);
                if head_idx >= ch.len() || tail_idx >= ch.len() {
                    break;
                }
                let t = (i as f32) / denom;
                let (w_out, w_in) = Self::loop_xfade_weights(shape, t);
                let head = ch[head_idx];
                let tail = ch[tail_idx];
                if uses_dip {
                    ch[tail_idx] = tail * w_out;
                    ch[head_idx] = head * w_in;
                } else {
                    let mixed = tail * w_out + head * w_in;
                    ch[tail_idx] = mixed;
                    ch[head_idx] = mixed;
                }
            }
        }
    }

    pub(super) fn editor_apply_loop_xfade(&mut self, tab_idx: usize) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = match tab.loop_region {
                Some((a, b)) if b > a => (a, b),
                _ => {
                    return;
                }
            };
            let half =
                Self::effective_loop_xfade_samples(s, e, tab.samples_len, tab.loop_xfade_samples);
            if half == 0 {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            Self::apply_loop_xfade_to_channels(
                &mut tab.ch_samples,
                s,
                e,
                half,
                tab.loop_xfade_shape,
            );
            tab.loop_xfade_samples = 0;
            tab.dirty = true;
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn editor_preview_loop_unwrap(
        &self,
        tab: &crate::app::types::EditorTab,
        repeats: u32,
    ) -> Option<Vec<Vec<f32>>> {
        if repeats < 2 {
            return None;
        }
        let (s, e) = match tab.loop_region {
            Some((a, b)) if b > a => (a, b),
            _ => {
                return None;
            }
        };
        let repeat_count = repeats as usize;
        let loop_len = e - s;
        if loop_len == 0 {
            return None;
        }
        let shift = loop_len.saturating_mul(repeat_count.saturating_sub(1));
        let mut channels = tab.ch_samples.clone();
        for ch in channels.iter_mut() {
            let mut out = Vec::with_capacity(ch.len().saturating_add(shift));
            out.extend_from_slice(&ch[..s]);
            let seg = &ch[s..e];
            for _ in 0..repeat_count {
                out.extend_from_slice(seg);
            }
            out.extend_from_slice(&ch[e..]);
            *ch = out;
        }
        Some(channels)
    }

    pub(super) fn build_loop_unwrap_markers(
        markers: &[crate::markers::MarkerEntry],
        loop_start: usize,
        loop_end: usize,
        samples_len: usize,
        repeat_count: usize,
    ) -> Vec<crate::markers::MarkerEntry> {
        if loop_end <= loop_start || repeat_count < 2 {
            return markers.to_vec();
        }
        let loop_len = loop_end - loop_start;
        let shift = loop_len.saturating_mul(repeat_count.saturating_sub(1));
        let max_len = samples_len.saturating_add(shift);
        let mut out: Vec<crate::markers::MarkerEntry> = Vec::new();
        for m in markers.iter() {
            let label = m.label.as_str();
            if label.eq_ignore_ascii_case("loop_end") || label.starts_with("loop_") {
                continue;
            }
            let mut sample = m.sample;
            if sample >= loop_end {
                sample = sample.saturating_add(shift);
            }
            out.push(crate::markers::MarkerEntry {
                sample: sample.min(max_len),
                label: m.label.clone(),
            });
        }
        for i in 0..repeat_count {
            let sample = loop_start.saturating_add(loop_len.saturating_mul(i));
            out.push(crate::markers::MarkerEntry {
                sample: sample.min(max_len),
                label: format!("loop_{}", i + 1),
            });
        }
        let end_sample = loop_start.saturating_add(loop_len.saturating_mul(repeat_count));
        out.push(crate::markers::MarkerEntry {
            sample: end_sample.min(max_len),
            label: "loop_end".to_string(),
        });
        out.sort_by_key(|m| m.sample);
        out.dedup_by(|a, b| a.sample == b.sample && a.label == b.label);
        out
    }

    pub(super) fn editor_apply_loop_unwrap(&mut self, tab_idx: usize, repeats: u32) {
        if repeats < 2 {
            return;
        }
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = match tab.loop_region {
                Some((a, b)) if b > a => (a, b),
                _ => {
                    return;
                }
            };
            let repeat_count = repeats as usize;
            let loop_len = e - s;
            if loop_len == 0 {
                return;
            }
            let shift = loop_len.saturating_mul(repeat_count.saturating_sub(1));
            let undo_state = Self::capture_undo_state(tab);
            for ch in tab.ch_samples.iter_mut() {
                let mut out = Vec::with_capacity(ch.len().saturating_add(shift));
                out.extend_from_slice(&ch[..s]);
                let seg = &ch[s..e];
                for _ in 0..repeat_count {
                    out.extend_from_slice(seg);
                }
                out.extend_from_slice(&ch[e..]);
                *ch = out;
            }
            let markers =
                Self::build_loop_unwrap_markers(&tab.markers, s, e, tab.samples_len, repeat_count);
            tab.markers = markers.clone();
            tab.markers_committed = markers.clone();
            tab.markers_applied = markers;
            Self::update_markers_dirty(tab);
            tab.samples_len = tab.samples_len.saturating_add(shift);
            if tab.view_offset >= e {
                tab.view_offset = tab.view_offset.saturating_add(shift);
            }
            Self::editor_sync_view_offset_exact(tab);
            if let Some(off) = tab.preview_offset_samples {
                if off >= e {
                    tab.preview_offset_samples = Some(off.saturating_add(shift));
                }
            }
            if let Some(anchor) = tab.selection_anchor_sample {
                if anchor >= e {
                    tab.selection_anchor_sample = Some(anchor.saturating_add(shift));
                } else if anchor >= s {
                    tab.selection_anchor_sample = None;
                    tab.right_drag_mode = None;
                }
            }
            tab.loop_region = None;
            tab.loop_region_committed = None;
            tab.loop_region_applied = None;
            tab.loop_mode = crate::app::types::LoopMode::Off;
            tab.loop_xfade_samples = 0;
            tab.selection = None;
            tab.ab_loop = None;
            tab.trim_range = None;
            tab.fade_in_range = None;
            tab.fade_out_range = None;
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
            tab.pending_loop_unwrap = None;
            tab.dirty = true;
            Self::update_loop_markers_dirty(tab);
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn editor_delete_range_and_join(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let remove_len = e - s;
            for ch in tab.ch_samples.iter_mut() {
                ch.drain(s..e);
            }
            tab.samples_len = tab.samples_len.saturating_sub(remove_len);
            tab.loop_region = None;
            tab.selection = None;
            tab.extra_selections.clear();
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    /// Run a Pitch/Stretch/Speed tool over `chan`, optionally restricted to
    /// `range`. Range mode processes just the selection and splices it back
    /// with click-free crossfades (shared by preview and apply so what you
    /// hear in preview is exactly what gets applied).
    pub(super) fn process_tool_segment_spliced(
        chan: &[f32],
        tool: ToolKind,
        param: f32,
        sr: u32,
        range: Option<(usize, usize)>,
    ) -> Vec<f32> {
        let process = |input: &[f32]| -> Vec<f32> {
            match tool {
                ToolKind::PitchShift => {
                    crate::wave::process_pitchshift_offline(input, sr, sr, param)
                }
                ToolKind::TimeStretch => {
                    crate::wave::process_timestretch_offline(input, sr, sr, param)
                }
                ToolKind::Speed => crate::wave::process_speed_offline(input, param),
                _ => input.to_vec(),
            }
        };
        match range {
            Some((s, e)) if e > s && e <= chan.len() && (e - s) < chan.len() => {
                let processed = process(&chan[s..e]);
                let xf = crate::wave::splice_xfade_samples(sr, e - s, processed.len());
                crate::wave::splice_range_with_crossfade(chan, s, e, &processed, xf)
            }
            _ => process(chan),
        }
    }

    /// Destructively apply a breakpoint gain envelope (DAW-style automation
    /// polyline) to the whole buffer.
    pub(super) fn editor_apply_gain_envelope(&mut self, tab_idx: usize, points: &[(usize, f32)]) {
        if points.is_empty() {
            return;
        }
        let (_channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let undo_state = Self::capture_undo_state(tab);
            for ch in tab.ch_samples.iter_mut() {
                crate::wave::apply_gain_envelope_in_place(ch, points, 0.0, true);
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    pub(super) fn spawn_editor_apply_for_tab(
        &mut self,
        tab_idx: usize,
        tool: ToolKind,
        param: f32,
    ) {
        self.spawn_editor_apply_for_tab_range(tab_idx, tool, param, None);
    }

    /// Heavy async apply. When `range` is set (and the tool supports it), only
    /// `[start, end)` is processed and the result is spliced back with short
    /// equal-power crossfades at both joins so the audio connects cleanly —
    /// including when the segment shrinks or grows (Speed / TimeStretch).
    pub(super) fn spawn_editor_apply_for_tab_range(
        &mut self,
        tab_idx: usize,
        tool: ToolKind,
        param: f32,
        range: Option<(usize, usize)>,
    ) {
        use std::sync::mpsc;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if matches!(
            tool,
            ToolKind::PitchShift | ToolKind::TimeStretch | ToolKind::Speed
        ) && self.is_decode_failed_path(&tab.path)
        {
            return;
        }
        let range = range.filter(|(s, e)| *e > *s && *e <= tab.samples_len);
        let undo = Some(Self::capture_undo_state(tab));
        // Single apply slot: the UI disables further applies on the busy tab;
        // races (hotkeys, other tabs) refuse instead of cancelling the job.
        if self.editor_apply_state.is_some() {
            return;
        }
        let tab_id = tab.tab_id;
        // Stop playback only when this tab is the audible source; playback of
        // other tabs / list previews keeps running during the apply.
        if matches!(&self.playback_session.source,
            crate::app::PlaybackSourceKind::EditorTab(p) if *p == tab.path)
        {
            self.audio.stop();
        }
        let ch = tab.ch_samples.clone();
        let buffer_sr = tab.buffer_sample_rate.max(1);
        let sr = self.audio.shared.out_sample_rate;
        let (tx, rx) = mpsc::channel::<EditorApplyResult>();
        std::thread::spawn(move || {
            let mut out: Vec<Vec<f32>> = Vec::with_capacity(ch.len());
            let mut lufs_override = None;
            match tool {
                ToolKind::PitchShift | ToolKind::TimeStretch | ToolKind::Speed => {
                    for chan in ch.iter() {
                        let processed =
                            Self::process_tool_segment_spliced(chan, tool, param, sr, range);
                        out.push(processed);
                    }
                }
                ToolKind::DeClick => {
                    let cfg = crate::app::declick::DeclickConfig {
                        sensitivity: param.clamp(0.0, 1.0),
                        ..Default::default()
                    };
                    for chan in ch.iter() {
                        let (processed, _count) =
                            crate::app::declick::declick_channel(chan, buffer_sr, &cfg, range);
                        out.push(processed);
                    }
                }
                ToolKind::DeClip => {
                    let cfg = crate::app::declip::DeclipConfig {
                        sensitivity: param.clamp(0.0, 1.0),
                        ..Default::default()
                    };
                    for chan in ch.iter() {
                        let (processed, _count) =
                            crate::app::declip::declip_channel(chan, buffer_sr, &cfg, range);
                        out.push(processed);
                    }
                }
                ToolKind::Loudness => {
                    let lufs = crate::wave::lufs_integrated_from_multi(&ch, sr)
                        .unwrap_or(f32::NEG_INFINITY);
                    if lufs.is_finite() {
                        let gain_db = param - lufs;
                        let gain = 10.0f32.powf(gain_db / 20.0);
                        for chan in ch.iter() {
                            let mut processed = chan.clone();
                            // Editing buffers keep float headroom; no clamp.
                            for v in processed.iter_mut() {
                                *v *= gain;
                            }
                            out.push(processed);
                        }
                        lufs_override = Some(param);
                    } else {
                        out = ch.clone();
                    }
                }
                _ => {
                    out = ch.clone();
                }
            }
            let len = out.get(0).map(|c| c.len()).unwrap_or(0);
            // Keep the selection over the processed span (its length may have
            // changed for Speed / TimeStretch range applies).
            let selection_after = range.and_then(|(s, e)| {
                let orig_len = ch.get(0).map(|c| c.len()).unwrap_or(0);
                let suffix = orig_len.saturating_sub(e);
                let new_end = len.saturating_sub(suffix);
                (new_end > s).then_some((s, new_end))
            });
            let mut mono = vec![0.0f32; len];
            let chn = out.len() as f32;
            if chn > 0.0 {
                for ch in &out {
                    for (i, v) in ch.iter().enumerate() {
                        if let Some(dst) = mono.get_mut(i) {
                            *dst += *v;
                        }
                    }
                }
                for v in &mut mono {
                    *v /= chn;
                }
            }
            // Build the waveform cache + Arc mirror here so adopting the
            // result on the UI thread is (nearly) copy-free.
            let (waveform_minmax, waveform_pyramid) =
                crate::app::WavesPreviewer::build_editor_waveform_cache(&out, len);
            let channels_arc = std::sync::Arc::new(out.clone());
            let _ = tx.send(EditorApplyResult {
                samples: mono,
                channels: out,
                channels_arc,
                waveform_minmax,
                waveform_pyramid,
                lufs_override,
                selection_after,
            });
        });
        let msg = match tool {
            ToolKind::PitchShift => "Applying PitchShift...".to_string(),
            ToolKind::TimeStretch => "Applying TimeStretch...".to_string(),
            ToolKind::Speed => "Applying Speed...".to_string(),
            ToolKind::Loudness => "Applying Loudness Normalize...".to_string(),
            ToolKind::DeClick => "Removing clicks...".to_string(),
            ToolKind::DeClip => "Repairing clipping...".to_string(),
            _ => "Applying...".to_string(),
        };
        self.editor_apply_state = Some(crate::app::types::EditorApplyState {
            msg,
            rx,
            tab_id,
            undo,
        });
    }

    /// Cancel the pending heavy apply only when it targets `tab_idx`'s tab.
    /// Other tabs' jobs keep running (undo/clear in one tab must not kill a
    /// job started from another).
    pub(super) fn cancel_editor_apply_for_tab(&mut self, tab_idx: usize) {
        let matches_tab = self
            .editor_apply_state
            .as_ref()
            .zip(self.tabs.get(tab_idx))
            .map(|(state, tab)| state.tab_id == tab.tab_id)
            .unwrap_or(false);
        if matches_tab {
            self.editor_apply_state = None;
        }
    }

    pub(super) fn drain_editor_apply_jobs(&mut self, ctx: &egui::Context) {
        let mut apply_done: Option<(EditorApplyResult, Option<EditorUndoState>, u64)> = None;
        if let Some(state) = &mut self.editor_apply_state {
            if let Ok(res) = state.rx.try_recv() {
                let undo = state.undo.take();
                let tab_id = state.tab_id;
                apply_done = Some((res, undo, tab_id));
            }
        }
        if let Some((mut res, undo, tab_id)) = apply_done {
            // Resolve identity -> index at completion time; the tab may have
            // moved (another tab closed) or be gone entirely.
            let cur_idx = self.tabs.iter().position(|t| t.tab_id == tab_id);
            let mut spectro_reset_path: Option<PathBuf> = None;
            if let Some(cur_idx) = cur_idx {
                let mut applied_channels = std::mem::take(&mut res.channels);
                if applied_channels.is_empty() && !res.samples.is_empty() {
                    applied_channels = vec![res.samples.clone()];
                }
                if let Some(tab) = self.tabs.get_mut(cur_idx) {
                    let old_len = tab.samples_len.max(1);
                    let old_view = tab.view_offset;
                    let old_spp = tab.samples_per_px;
                    if let Some(undo_state) = undo {
                        Self::push_undo_state_from(tab, undo_state, true);
                    }
                    tab.preview_audio_tool = None;
                    tab.preview_overlay = None;
                    tab.declick_scan = None;
                    tab.ch_samples = applied_channels;
                    // Adopt the worker-built mirror + waveform cache instead
                    // of re-cloning and re-scanning the buffers here.
                    tab.ch_samples_arc = if res.channels_arc.len() == tab.ch_samples.len()
                        && !tab.ch_samples.is_empty()
                    {
                        res.channels_arc.clone()
                    } else {
                        std::sync::Arc::new(tab.ch_samples.clone())
                    };
                    tab.buffer_sample_rate = self.audio.shared.out_sample_rate.max(1);
                    tab.samples_len = tab.ch_samples.get(0).map(|c| c.len()).unwrap_or(0);
                    if res.waveform_minmax.is_empty() && tab.samples_len > 0 {
                        let (waveform_minmax, waveform_pyramid) =
                            Self::build_editor_waveform_cache(&tab.ch_samples, tab.samples_len);
                        tab.waveform_minmax = waveform_minmax;
                        tab.waveform_pyramid = waveform_pyramid;
                    } else {
                        tab.waveform_minmax = std::mem::take(&mut res.waveform_minmax);
                        tab.waveform_pyramid = res.waveform_pyramid.take();
                    }
                    Self::invalidate_editor_viewport_cache(tab);
                    let new_len = tab.samples_len.max(1);
                    if old_len > 0 && new_len > 0 {
                        let ratio = (new_len as f32) / (old_len as f32);
                        if old_spp > 0.0 {
                            tab.samples_per_px =
                                (old_spp * ratio).max(crate::app::EDITOR_MIN_SAMPLES_PER_PX);
                        }
                        tab.view_offset = ((old_view as f32) * ratio).round() as usize;
                        tab.view_offset_exact = tab.view_offset as f64;
                        tab.loop_xfade_samples =
                            ((tab.loop_xfade_samples as f32) * ratio).round() as usize;
                    }
                    Self::editor_clamp_vertical_view(tab);
                    tab.dirty = true;
                    if let Some(sel) = res.selection_after {
                        tab.selection = Some(sel);
                    }
                    Self::editor_clamp_ranges(tab);
                    if let Some(v) = res.lufs_override {
                        self.lufs_override.insert(tab.path.clone(), v);
                    }
                    spectro_reset_path = Some(tab.path.clone());
                }
                self.clear_heavy_preview_state();
                self.clear_heavy_overlay_state();
                // Re-target the audio engine only when this tab is what the
                // user is hearing (or looking at); playback of another tab or
                // a list preview must survive the apply untouched.
                let tab_path = self.tabs.get(cur_idx).map(|t| t.path.clone());
                let adopt_audio = self.active_tab == Some(cur_idx)
                    || matches!(
                        (&self.playback_session.source, &tab_path),
                        (crate::app::PlaybackSourceKind::EditorTab(p), Some(tp)) if p == tp
                    );
                if adopt_audio {
                    self.audio.stop();
                    if let Some((path, buffer_sr, channels)) = self.tabs.get(cur_idx).map(|tab| {
                        (
                            tab.path.clone(),
                            tab.buffer_sample_rate.max(1),
                            tab.ch_samples.clone(),
                        )
                    }) {
                        self.audio.set_samples_channels(channels);
                        self.playback_mark_buffer_source(
                            crate::app::PlaybackSourceKind::EditorTab(path),
                            buffer_sr,
                        );
                        if let Some(tab) = self.tabs.get(cur_idx) {
                            self.apply_loop_mode_for_tab(tab);
                        }
                    } else if !res.samples.is_empty() {
                        self.audio.set_samples_mono(res.samples);
                    }
                }
                self.notify_if_tab_over_fs(cur_idx);
            }
            if let Some(path) = spectro_reset_path {
                self.cancel_spectrogram_for_path(&path);
                self.cancel_feature_analysis_for_path(&path);
            }
            self.editor_apply_state = None;
            ctx.request_repaint();
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_trim_frac(&mut self, start: f32, end: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(range) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        self.editor_apply_trim_range(tab_idx, range);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_invert_polarity_frac(&mut self, start: f32, end: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(range) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        self.editor_apply_invert_polarity_range(tab_idx, range);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_editor_copy_selection(&mut self) -> usize {
        let Some(tab_idx) = self.active_tab else {
            return 0;
        };
        self.editor_copy_selection_to_audio_clipboard(tab_idx, true)
    }

    #[cfg(feature = "kittest")]
    pub fn test_editor_cut_selection(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.editor_cut_selection_to_audio_clipboard(tab_idx)
    }

    #[cfg(feature = "kittest")]
    pub fn test_editor_paste_mode(&mut self, mode: super::types::PasteMode) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.editor_paste_from_audio_clipboard(tab_idx, mode)
    }

    #[cfg(feature = "kittest")]
    pub fn test_editor_paste_insert(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.editor_paste_insert_from_audio_clipboard(tab_idx)
    }

    #[cfg(feature = "kittest")]
    pub fn test_editor_audio_clipboard_len(&self) -> usize {
        self.editor_audio_clipboard
            .as_ref()
            .and_then(|c| c.channels.first())
            .map(|c| c.len())
            .unwrap_or(0)
    }

    #[cfg(feature = "kittest")]
    pub fn test_insert_silence_at_frac(&mut self, frac: f32, ms: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let pos = ((tab.samples_len as f32) * frac.clamp(0.0, 1.0)).round() as usize;
        self.editor_insert_silence_at(tab_idx, pos, ms)
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_remove_dc_frac(&mut self, start: f32, end: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(range) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        self.editor_apply_remove_dc_range(tab_idx, range);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_delete_range_frac(&mut self, start: f32, end: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(range) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        self.editor_delete_range_and_join(tab_idx, range);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_fade_in(
        &mut self,
        start: f32,
        end: f32,
        shape: crate::app::types::FadeShape,
    ) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(range) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        self.editor_apply_fade_in_explicit(tab_idx, range, shape);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_fade_out(
        &mut self,
        start: f32,
        end: f32,
        shape: crate::app::types::FadeShape,
    ) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(range) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        self.editor_apply_fade_out_explicit(tab_idx, range, shape);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_gain(&mut self, start: f32, end: f32, db: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(range) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        self.editor_apply_gain_range(tab_idx, range, db);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_normalize(&mut self, start: f32, end: f32, db: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(range) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        self.editor_apply_normalize_range(tab_idx, range, db);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_reverse(&mut self, start: f32, end: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let Some(range) = Self::test_range_from_frac(tab, start, end) else {
            return false;
        };
        self.editor_apply_reverse_range(tab_idx, range);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_loop_unwrap(&mut self, repeats: u32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        if tab.loop_region.map(|(a, b)| b > a).unwrap_or(false) {
            self.editor_apply_loop_unwrap(tab_idx, repeats.max(2));
            true
        } else {
            false
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_pitch_shift(&mut self, semitones: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.spawn_editor_apply_for_tab(tab_idx, ToolKind::PitchShift, semitones);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_apply_time_stretch(&mut self, rate: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.spawn_editor_apply_for_tab(tab_idx, ToolKind::TimeStretch, rate);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_editor_apply_active(&self) -> bool {
        self.editor_apply_state.is_some()
    }

    #[cfg(feature = "kittest")]
    pub(super) fn test_range_from_frac(
        tab: &crate::app::types::EditorTab,
        start: f32,
        end: f32,
    ) -> Option<(usize, usize)> {
        if tab.samples_len == 0 {
            return None;
        }
        let mut s = (tab.samples_len as f32 * start.clamp(0.0, 1.0)).floor() as usize;
        let mut e = (tab.samples_len as f32 * end.clamp(0.0, 1.0)).ceil() as usize;
        if s > e {
            std::mem::swap(&mut s, &mut e);
        }
        if e <= s {
            e = (s + 1).min(tab.samples_len);
        }
        if s >= tab.samples_len {
            return None;
        }
        Some((s, e.min(tab.samples_len)))
    }
}

#[cfg(test)]
mod clear_edit_tests {
    use crate::app::WavesPreviewer;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_clear_edit_test_{tag}_{}_{}",
            std::process::id(),
            ts
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn wait_for_decode(app: &mut WavesPreviewer, tab_idx: usize) {
        let started = Instant::now();
        loop {
            app.drain_editor_decode();
            if let Some(tab) = app.tabs.get(tab_idx) {
                if !tab.loading {
                    return;
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "editor decode timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn clear_edit_reverts_gain_and_resets_edit_state() {
        let dir = temp_dir("gain");
        let wav = dir.join("source.wav");
        crate::wave::export_channels_audio(&[vec![0.2, 0.2, 0.2, 0.2]], 48_000, &wav)
            .expect("write wav");

        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        app.open_or_activate_tab(&wav);
        let tab_idx = app
            .tabs
            .iter()
            .position(|t| t.path == wav)
            .expect("tab opened");
        wait_for_decode(&mut app, tab_idx);

        let len = app.tabs[tab_idx].samples_len;
        app.editor_apply_gain_range(tab_idx, (0, len), -6.0);
        assert!(app.tabs[tab_idx].dirty, "gain apply should dirty the tab");
        assert!(!app.tabs[tab_idx].undo_stack.is_empty());
        let gained = app.tabs[tab_idx].ch_samples[0][0];
        assert!(
            (gained - 0.2).abs() > 1e-4,
            "gain should have changed the sample value"
        );

        app.clear_edit_in_tab(tab_idx);

        let tab = &app.tabs[tab_idx];
        assert!(!tab.dirty, "clear edit should mark the tab clean");
        assert!(tab.undo_stack.is_empty(), "clear edit should wipe undo history");
        assert!(tab.redo_stack.is_empty(), "clear edit should wipe redo history");
        assert!(
            (tab.ch_samples[0][0] - 0.2).abs() < 1e-4,
            "clear edit should restore the original sample value, got {}",
            tab.ch_samples[0][0]
        );
    }

    #[test]
    fn inspector_noise_gate_eq_compressor_apply_and_undo() {
        let dir = temp_dir("inspector_tools");
        let wav = dir.join("source.wav");
        let sr = 48_000u32;
        let tone: Vec<f32> = (0..sr as usize)
            .map(|i| (i as f32 / sr as f32 * 440.0 * std::f32::consts::TAU).sin() * 0.4)
            .collect();
        crate::wave::export_channels_audio(&[tone.clone()], sr, &wav).expect("write wav");

        let mut app = WavesPreviewer::new_headless(Default::default()).expect("app");
        app.open_or_activate_tab(&wav);
        let tab_idx = app
            .tabs
            .iter()
            .position(|t| t.path == wav)
            .expect("tab opened");
        wait_for_decode(&mut app, tab_idx);
        let len = app.tabs[tab_idx].samples_len;

        // Noise Gate
        let before = app.tabs[tab_idx].ch_samples[0].clone();
        app.editor_apply_noise_gate_range(tab_idx, (0, len), -10.0, 1.0, 20.0);
        assert!(app.tabs[tab_idx].dirty);
        assert_ne!(app.tabs[tab_idx].ch_samples[0], before);
        assert!(app.undo_in_tab(tab_idx));
        assert_eq!(app.tabs[tab_idx].ch_samples[0], before);

        // EQ
        app.editor_apply_eq_range(
            tab_idx,
            (0, len),
            crate::wave::ThreeBandEqParams {
                low_shelf_freq_hz: 120.0,
                low_shelf_gain_db: 0.0,
                mid_freq_hz: 440.0,
                mid_gain_db: 12.0,
                mid_q: 1.0,
                high_shelf_freq_hz: 8000.0,
                high_shelf_gain_db: 0.0,
            },
        );
        assert!(app.tabs[tab_idx].dirty);
        assert_ne!(app.tabs[tab_idx].ch_samples[0], before);
        assert!(app.undo_in_tab(tab_idx));
        assert_eq!(app.tabs[tab_idx].ch_samples[0], before);

        // Compressor
        app.editor_apply_compressor_range(
            tab_idx,
            (0, len),
            crate::wave::CompressorParams {
                threshold_db: -12.0,
                ratio: 4.0,
                attack_ms: 1.0,
                release_ms: 50.0,
                makeup_db: 0.0,
            },
        );
        assert!(app.tabs[tab_idx].dirty);
        assert_ne!(app.tabs[tab_idx].ch_samples[0], before);
        assert!(app.undo_in_tab(tab_idx));
        assert_eq!(app.tabs[tab_idx].ch_samples[0], before);
    }

    #[test]
    fn invert_polarity_smoothing_ramps_interior_boundaries_only() {
        // Hard flip (fade = 0): exact negation.
        let mut ch: Vec<f32> = (0..100).map(|i| (i as f32 * 0.37).sin()).collect();
        let orig = ch.clone();
        crate::app::WavesPreviewer::invert_polarity_channel_range(&mut ch, 10, 90, 0);
        for i in 10..90 {
            assert_eq!(ch[i], -orig[i]);
        }
        assert_eq!(&ch[..10], &orig[..10]);
        assert_eq!(&ch[90..], &orig[90..]);

        // Smoothed interior range: gain walks from ~+1 to -1 at the start
        // edge (no sign step against the untouched neighbor) and back at the
        // end edge; the middle is fully inverted.
        let mut ch: Vec<f32> = vec![1.0; 100];
        crate::app::WavesPreviewer::invert_polarity_channel_range(&mut ch, 10, 90, 8);
        assert!(ch[9] == 1.0 && ch[10] > 0.0, "start edge must stay continuous");
        assert!(ch[10] > ch[11], "gain must descend across the start fade");
        for v in &ch[18..82] {
            assert_eq!(*v, -1.0);
        }
        assert!(ch[89] > 0.0 && ch[90] == 1.0, "end edge must stay continuous");

        // Range touching the buffer edges: no fade there (nothing to join).
        let mut ch: Vec<f32> = vec![1.0; 50];
        crate::app::WavesPreviewer::invert_polarity_channel_range(&mut ch, 0, 50, 8);
        assert!(ch.iter().all(|v| *v == -1.0));
    }
}
