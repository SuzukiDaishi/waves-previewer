use std::path::PathBuf;

use crate::app::types::{EditorApplyResult, EditorUndoState, ToolKind};

impl crate::app::WavesPreviewer {
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
        let (channels, undo_state) = {
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
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        self.on_audio_length_changed(tab_idx);
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn editor_apply_fade_out_explicit(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        shape: crate::app::types::FadeShape,
    ) {
        let (channels, undo_state) = {
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
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
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
        clamp_range(&mut tab.ab_loop);
        clamp_range(&mut tab.loop_region);
        clamp_range(&mut tab.trim_range);
        clamp_range(&mut tab.fade_in_range);
        clamp_range(&mut tab.fade_out_range);
        let max_view = len.saturating_sub(1);
        if tab.view_offset > max_view {
            tab.view_offset = max_view;
        }
        if tab.loop_xfade_samples > len / 2 {
            tab.loop_xfade_samples = len / 2;
        }
        if tab.drag_select_anchor.map(|v| v > len).unwrap_or(false) {
            tab.drag_select_anchor = None;
        }
        if tab.preview_offset_samples.map(|v| v > len).unwrap_or(false) {
            tab.preview_offset_samples = None;
        }
        Self::update_loop_markers_dirty(tab);
    }

    fn on_audio_length_changed(&mut self, tab_idx: usize) {
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
            .store(clamped_pos as f32, std::sync::atomic::Ordering::Relaxed);
    }

    pub(super) fn editor_apply_reverse_range(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            for ch in tab.ch_samples.iter_mut() {
                ch[s..e].reverse();
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
        self.audio.set_loop_crossfade(0, 0);
    }

    pub(super) fn editor_apply_trim_range(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            for ch in tab.ch_samples.iter_mut() {
                let mut seg = ch[s..e].to_vec();
                std::mem::swap(ch, &mut seg);
                ch.truncate(e - s);
            }
            tab.samples_len = e - s;
            tab.view_offset = 0;
            tab.selection = None;
            tab.loop_region = None;
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        self.on_audio_length_changed(tab_idx);
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn add_trim_range_as_virtual(&mut self, tab_idx: usize, range: (usize, usize)) {
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
        if let Some(row) = self.row_for_path(&added_path) {
            self.update_selection_on_click(row, egui::Modifiers::NONE);
            self.select_and_load(row, true);
        }
    }

    pub(super) fn editor_apply_gain_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        gain_db: f32,
    ) {
        let (channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let g = crate::app::helpers::db_to_amp(gain_db);
            for ch in tab.ch_samples.iter_mut() {
                for i in s..e {
                    ch[i] = (ch[i] * g).clamp(-1.0, 1.0);
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn editor_apply_normalize_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        target_db: f32,
    ) {
        let (channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let mut peak = 0.0f32;
            for ch in &tab.ch_samples {
                for &v in &ch[s..e] {
                    peak = peak.max(v.abs());
                }
            }
            if peak <= 0.0 {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            let g = crate::app::helpers::db_to_amp(target_db) / peak.max(1e-12);
            for ch in tab.ch_samples.iter_mut() {
                for i in s..e {
                    ch[i] = (ch[i] * g).clamp(-1.0, 1.0);
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn editor_apply_mute_range(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (channels, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let (s, e) = range;
            if e <= s || e > tab.samples_len {
                return;
            }
            let undo_state = Self::capture_undo_state(tab);
            for ch in tab.ch_samples.iter_mut() {
                for i in s..e {
                    ch[i] = 0.0;
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    #[allow(dead_code)]
    pub(super) fn editor_apply_fade_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        in_ms: f32,
        out_ms: f32,
    ) {
        let (channels, undo_state) = {
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
            for ch in tab.ch_samples.iter_mut() {
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
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn editor_apply_loop_xfade(&mut self, tab_idx: usize) {
        let (channels, undo_state) = {
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
            let win_len = half.saturating_mul(2);
            let denom = (win_len.saturating_sub(1)).max(1) as f32;
            let s_start = s.saturating_sub(half);
            let e_start = e.saturating_sub(half);
            for ch in tab.ch_samples.iter_mut() {
                for i in 0..win_len {
                    let t = (i as f32) / denom;
                    let (w_out, w_in) = match tab.loop_xfade_shape {
                        crate::app::types::LoopXfadeShape::EqualPower => {
                            let a = core::f32::consts::FRAC_PI_2 * t;
                            (a.cos(), a.sin())
                        }
                        crate::app::types::LoopXfadeShape::Linear => (1.0 - t, t),
                    };
                    let s_idx = s_start + i;
                    let e_idx = e_start + i;
                    if s_idx >= ch.len() || e_idx >= ch.len() {
                        break;
                    }
                    let s = ch[s_idx];
                    let e = ch[e_idx];
                    let mixed = e * w_out + s * w_in;
                    ch[s_idx] = mixed;
                    ch[e_idx] = mixed;
                }
            }
            tab.loop_xfade_samples = 0;
            tab.dirty = true;
            (tab.ch_samples.clone(), undo_state)
        };
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
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
        let (channels, undo_state) = {
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
            tab.markers_dirty = tab.markers_committed != tab.markers_saved;
            tab.samples_len = tab.samples_len.saturating_add(shift);
            if tab.view_offset >= e {
                tab.view_offset = tab.view_offset.saturating_add(shift);
            }
            if let Some(off) = tab.preview_offset_samples {
                if off >= e {
                    tab.preview_offset_samples = Some(off.saturating_add(shift));
                }
            }
            if let Some(anchor) = tab.drag_select_anchor {
                if anchor >= e {
                    tab.drag_select_anchor = Some(anchor.saturating_add(shift));
                } else if anchor >= s {
                    tab.drag_select_anchor = None;
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
            Self::editor_clamp_ranges(tab);
            (tab.ch_samples.clone(), undo_state)
        };
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn editor_delete_range_and_join(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (channels, loop_mode, lr, len, undo_state) = {
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
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            (
                tab.ch_samples.clone(),
                tab.loop_mode,
                tab.loop_region,
                tab.samples_len,
                undo_state,
            )
        };
        self.push_editor_undo_state(tab_idx, undo_state, true);
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        self.on_audio_length_changed(tab_idx);
        match loop_mode {
            crate::app::types::LoopMode::OnWhole => {
                self.audio.set_loop_enabled(true);
                self.audio.set_loop_region(0, len);
            }
            crate::app::types::LoopMode::Marker => {
                if let Some((a, b)) = lr {
                    let (s, e) = if a <= b { (a, b) } else { (b, a) };
                    self.audio.set_loop_enabled(true);
                    self.audio.set_loop_region(s, e);
                } else {
                    self.audio.set_loop_enabled(false);
                }
            }
            crate::app::types::LoopMode::Off => {
                self.audio.set_loop_enabled(false);
            }
        }
    }

    pub(super) fn spawn_editor_apply_for_tab(
        &mut self,
        tab_idx: usize,
        tool: ToolKind,
        param: f32,
    ) {
        use std::sync::mpsc;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if matches!(tool, ToolKind::PitchShift | ToolKind::TimeStretch)
            && self.is_decode_failed_path(&tab.path)
        {
            return;
        }
        let undo = Some(Self::capture_undo_state(tab));
        // Cancel any previous apply job
        self.editor_apply_state = None;
        self.audio.stop();
        let ch = tab.ch_samples.clone();
        let sr = self.audio.shared.out_sample_rate;
        let (tx, rx) = mpsc::channel::<EditorApplyResult>();
        std::thread::spawn(move || {
            let mut out: Vec<Vec<f32>> = Vec::with_capacity(ch.len());
            let mut lufs_override = None;
            match tool {
                ToolKind::PitchShift | ToolKind::TimeStretch => {
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
                            for v in processed.iter_mut() {
                                *v = (*v * gain).clamp(-1.0, 1.0);
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
            let _ = tx.send(EditorApplyResult {
                tab_idx,
                samples: mono,
                channels: out,
                lufs_override,
            });
        });
        let msg = match tool {
            ToolKind::PitchShift => "Applying PitchShift...".to_string(),
            ToolKind::TimeStretch => "Applying TimeStretch...".to_string(),
            ToolKind::Loudness => "Applying Loudness Normalize...".to_string(),
            _ => "Applying...".to_string(),
        };
        self.editor_apply_state = Some(crate::app::types::EditorApplyState {
            msg,
            rx,
            tab_idx,
            undo,
        });
    }

    pub(super) fn drain_editor_apply_jobs(&mut self, ctx: &egui::Context) {
        let mut apply_done: Option<(EditorApplyResult, Option<EditorUndoState>)> = None;
        if let Some(state) = &mut self.editor_apply_state {
            if let Ok(res) = state.rx.try_recv() {
                let undo = state.undo.take();
                apply_done = Some((res, undo));
            }
        }
        if let Some((res, undo)) = apply_done {
            let mut spectro_reset_path: Option<PathBuf> = None;
            if res.tab_idx < self.tabs.len() {
                let mut applied_channels = res.channels;
                if applied_channels.is_empty() && !res.samples.is_empty() {
                    applied_channels = vec![res.samples.clone()];
                }
                if let Some(tab) = self.tabs.get_mut(res.tab_idx) {
                    let old_len = tab.samples_len.max(1);
                    let old_view = tab.view_offset;
                    let old_spp = tab.samples_per_px;
                    if let Some(undo_state) = undo {
                        Self::push_undo_state_from(tab, undo_state, true);
                    }
                    tab.preview_audio_tool = None;
                    tab.preview_overlay = None;
                    tab.ch_samples = applied_channels;
                    tab.samples_len = tab.ch_samples.get(0).map(|c| c.len()).unwrap_or(0);
                    let new_len = tab.samples_len.max(1);
                    if old_len > 0 && new_len > 0 {
                        let ratio = (new_len as f32) / (old_len as f32);
                        if old_spp > 0.0 {
                            tab.samples_per_px = (old_spp * ratio).max(0.0001);
                        }
                        tab.view_offset = ((old_view as f32) * ratio).round() as usize;
                        tab.loop_xfade_samples =
                            ((tab.loop_xfade_samples as f32) * ratio).round() as usize;
                    }
                    tab.dirty = true;
                    Self::editor_clamp_ranges(tab);
                    if let Some(v) = res.lufs_override {
                        self.lufs_override.insert(tab.path.clone(), v);
                    }
                    spectro_reset_path = Some(tab.path.clone());
                }
                self.heavy_preview_rx = None;
                self.heavy_preview_tool = None;
                self.heavy_overlay_rx = None;
                self.overlay_expected_tool = None;
                self.audio.stop();
                if let Some(tab) = self.tabs.get(res.tab_idx) {
                    self.audio.set_samples_channels(tab.ch_samples.clone());
                    self.apply_loop_mode_for_tab(tab);
                } else if !res.samples.is_empty() {
                    self.audio.set_samples_mono(res.samples);
                }
            }
            if let Some(path) = spectro_reset_path {
                self.cancel_spectrogram_for_path(&path);
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
