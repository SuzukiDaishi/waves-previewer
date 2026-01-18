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
        let channels = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range;
                if e <= s || e > tab.samples_len {
                    return;
                }
                Self::push_undo_state(tab, true);
                let dur = (e - s).max(1) as f32;
                for ch in tab.ch_samples.iter_mut() {
                    for i in s..e {
                        let t = (i - s) as f32 / dur;
                        let w = Self::fade_weight(shape, t);
                        ch[i] *= w;
                    }
                }
                tab.dirty = true;
                tab.ch_samples.clone()
            } else {
                return;
            }
        };
        self.audio.set_samples_channels(channels);
        self.audio.stop();
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
        let channels = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range;
                if e <= s || e > tab.samples_len {
                    return;
                }
                Self::push_undo_state(tab, true);
                let dur = (e - s).max(1) as f32;
                for ch in tab.ch_samples.iter_mut() {
                    for i in s..e {
                        let t = (i - s) as f32 / dur;
                        let w = Self::fade_weight_out(shape, t);
                        ch[i] *= w;
                    }
                }
                tab.dirty = true;
                tab.ch_samples.clone()
            } else {
                return;
            }
        };
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

    pub(super) fn editor_apply_reverse_range(&mut self, tab_idx: usize, range: (usize, usize)) {
        let channels = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range;
                if e <= s || e > tab.samples_len {
                    return;
                }
                Self::push_undo_state(tab, true);
                for ch in tab.ch_samples.iter_mut() {
                    ch[s..e].reverse();
                }
                tab.dirty = true;
                Self::editor_clamp_ranges(tab);
                tab.ch_samples.clone()
            } else {
                return;
            }
        };
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
        self.audio.set_loop_crossfade(0, 0);
    }

    pub(super) fn editor_apply_trim_range(&mut self, tab_idx: usize, range: (usize, usize)) {
        let channels = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range;
                if e <= s || e > tab.samples_len {
                    return;
                }
                Self::push_undo_state(tab, true);
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
                tab.ch_samples.clone()
            } else {
                return;
            }
        };
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn editor_apply_gain_range(
        &mut self,
        tab_idx: usize,
        range: (usize, usize),
        gain_db: f32,
    ) {
        let channels = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range;
                if e <= s || e > tab.samples_len {
                    return;
                }
                Self::push_undo_state(tab, true);
                let g = crate::app::helpers::db_to_amp(gain_db);
                for ch in tab.ch_samples.iter_mut() {
                    for i in s..e {
                        ch[i] = (ch[i] * g).clamp(-1.0, 1.0);
                    }
                }
                tab.dirty = true;
                Self::editor_clamp_ranges(tab);
                tab.ch_samples.clone()
            } else {
                return;
            }
        };
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
        let channels = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
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
                Self::push_undo_state(tab, true);
                let g = crate::app::helpers::db_to_amp(target_db) / peak.max(1e-12);
                for ch in tab.ch_samples.iter_mut() {
                    for i in s..e {
                        ch[i] = (ch[i] * g).clamp(-1.0, 1.0);
                    }
                }
                tab.dirty = true;
                Self::editor_clamp_ranges(tab);
                tab.ch_samples.clone()
            } else {
                return;
            }
        };
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
        let channels = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range;
                if e <= s || e > tab.samples_len {
                    return;
                }
                Self::push_undo_state(tab, true);
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
                tab.ch_samples.clone()
            } else {
                return;
            }
        };
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn editor_apply_loop_xfade(&mut self, tab_idx: usize) {
        let channels = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = match tab.loop_region {
                    Some((a, b)) if b > a => (a, b),
                    _ => {
                        return;
                    }
                };
                let half = Self::effective_loop_xfade_samples(
                    s,
                    e,
                    tab.samples_len,
                    tab.loop_xfade_samples,
                );
                if half == 0 {
                    return;
                }
                Self::push_undo_state(tab, true);
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
                tab.ch_samples.clone()
            } else {
                return;
            }
        };
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
    }

    pub(super) fn editor_delete_range_and_join(&mut self, tab_idx: usize, range: (usize, usize)) {
        let (channels, loop_mode, lr, len) = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range;
                if e <= s || e > tab.samples_len {
                    return;
                }
                Self::push_undo_state(tab, true);
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
                )
            } else {
                return;
            }
        };
        self.audio.set_samples_channels(channels);
        self.audio.stop();
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
}
