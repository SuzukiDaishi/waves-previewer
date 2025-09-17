impl crate::app::WavesPreviewer {
    pub(super) fn fade_weight(shape: crate::app::types::FadeShape, t: f32) -> f32 {
        let x = t.clamp(0.0, 1.0);
        match shape {
            crate::app::types::FadeShape::Linear => x,
            crate::app::types::FadeShape::EqualPower => (core::f32::consts::PI * x / 2.0).sin(),
            crate::app::types::FadeShape::Cosine => (1.0 - (core::f32::consts::PI * (1.0 - x)).cos()) * 0.5,
            crate::app::types::FadeShape::SCurve => x * x * (3.0 - 2.0 * x),
            crate::app::types::FadeShape::Quadratic => x * x,
            crate::app::types::FadeShape::Cubic => x * x * x,
        }
    }

    pub(super) fn editor_apply_fade_in_explicit(&mut self, tab_idx: usize, range: (usize,usize), shape: crate::app::types::FadeShape) {
        let mono = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s,e) = range; if e<=s || e>tab.samples_len { return; }
                let dur = (e - s).max(1) as f32;
                for ch in tab.ch_samples.iter_mut() {
                    for i in s..e {
                        let t = (i - s) as f32 / dur;
                        let w = Self::fade_weight(shape, t);
                        ch[i] *= w;
                    }
                }
                tab.dirty = true;
                Self::editor_mixdown_mono(tab)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
    }

    pub(super) fn editor_apply_fade_out_explicit(&mut self, tab_idx: usize, range: (usize,usize), shape: crate::app::types::FadeShape) {
        let mono = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s,e) = range; if e<=s || e>tab.samples_len { return; }
                let dur = (e - s).max(1) as f32;
                for ch in tab.ch_samples.iter_mut() {
                    for i in s..e {
                        let t = (i - s) as f32 / dur;
                        let w = 1.0 - Self::fade_weight(shape, t);
                        ch[i] *= w;
                    }
                }
                tab.dirty = true;
                Self::editor_mixdown_mono(tab)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
    }

    #[allow(dead_code)]
    pub(super) fn editor_selected_range(tab: &crate::app::types::EditorTab) -> Option<(usize,usize)> {
        if let Some(r) = tab.selection { if r.1 > r.0 { return Some(r); } }
        None
    }

    pub(super) fn editor_apply_reverse_range(&mut self, tab_idx: usize, range: (usize,usize)) {
        let mono = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range; if e<=s || e>tab.samples_len { return; }
                for ch in tab.ch_samples.iter_mut() { ch[s..e].reverse(); }
                tab.dirty = true;
                Self::editor_mixdown_mono(tab)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
        self.audio.set_loop_crossfade(0, 0);
    }

    pub(super) fn editor_apply_trim_range(&mut self, tab_idx: usize, range: (usize,usize)) {
        let mono = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s,e) = range; if e<=s || e>tab.samples_len { return; }
                for ch in tab.ch_samples.iter_mut() { let mut seg = ch[s..e].to_vec(); std::mem::swap(ch, &mut seg); ch.truncate(e-s); }
                tab.samples_len = e - s;
                tab.view_offset = 0; tab.selection = None;
                tab.loop_region = None; tab.dirty = true;
                Self::editor_mixdown_mono(tab)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
    }

    pub(super) fn editor_apply_gain_range(&mut self, tab_idx: usize, range: (usize,usize), gain_db: f32) {
        let mono = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s,e) = range; if e<=s || e>tab.samples_len { return; }
                let g = crate::app::helpers::db_to_amp(gain_db);
                for ch in tab.ch_samples.iter_mut() { for i in s..e { ch[i] = (ch[i] * g).clamp(-1.0, 1.0); } }
                tab.dirty = true;
                Self::editor_mixdown_mono(tab)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
    }

    pub(super) fn editor_apply_normalize_range(&mut self, tab_idx: usize, range: (usize,usize), target_db: f32) {
        let mono = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s,e) = range; if e<=s || e>tab.samples_len { return; }
                let mut peak = 0.0f32; for ch in &tab.ch_samples { for &v in &ch[s..e] { peak = peak.max(v.abs()); } }
                if peak <= 0.0 { return; }
                let g = crate::app::helpers::db_to_amp(target_db) / peak.max(1e-12);
                for ch in tab.ch_samples.iter_mut() { for i in s..e { ch[i] = (ch[i] * g).clamp(-1.0, 1.0); } }
                tab.dirty = true;
                Self::editor_mixdown_mono(tab)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
    }

    #[allow(dead_code)]
    pub(super) fn editor_apply_fade_range(&mut self, tab_idx: usize, range: (usize,usize), in_ms: f32, out_ms: f32) {
        let mono = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s,e) = range; if e<=s || e>tab.samples_len { return; }
                let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                let nin = ((in_ms / 1000.0) * sr) as usize;
                let nout = ((out_ms / 1000.0) * sr) as usize;
                for ch in tab.ch_samples.iter_mut() {
                    for i in 0..nin.min(e-s) {
                        let t = i as f32 / nin.max(1) as f32; let w = Self::fade_weight(crate::app::types::FadeShape::SCurve, t);
                        ch[s + i] *= w;
                    }
                    for i in 0..nout.min(e-s) {
                        let t = i as f32 / nout.max(1) as f32; let w = 1.0 - Self::fade_weight(crate::app::types::FadeShape::SCurve, t);
                        ch[e - 1 - i] *= w;
                    }
                }
                tab.dirty = true;
                Self::editor_mixdown_mono(tab)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
    }

    pub(super) fn editor_apply_loop_xfade(&mut self, tab_idx: usize) {
        let mono = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = match tab.loop_region { Some((a,b)) if b> a => (a,b), _ => { return; } };
                let seg_len = e - s;
                let n = tab.loop_xfade_samples.min(seg_len / 2).min(tab.samples_len);
                if n == 0 { return; }
                for ch in tab.ch_samples.iter_mut() {
                    for i in 0..n {
                        let t = i as f32 / (n as f32);
                        let (w_out, w_in) = match tab.loop_xfade_shape {
                            crate::app::types::LoopXfadeShape::EqualPower => {
                                let a = core::f32::consts::FRAC_PI_2 * t; (a.cos(), a.sin())
                            }
                            crate::app::types::LoopXfadeShape::Linear => (1.0 - t, t),
                        };
                        let head_idx = s + i;
                        let tail_idx = e - n + i;
                        let head = ch[head_idx];
                        let tail = ch[tail_idx];
                        let mixed = tail * w_out + head * w_in;
                        ch[head_idx] = mixed;
                        ch[tail_idx] = mixed;
                    }
                }
                tab.loop_xfade_samples = 0;
                tab.dirty = true;
                Self::editor_mixdown_mono(tab)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
    }

    pub(super) fn editor_delete_range_and_join(&mut self, tab_idx: usize, range: (usize,usize)) {
        let (mono, loop_mode, lr, len) = {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let (s, e) = range; if e<=s || e>tab.samples_len { return; }
                let remove_len = e - s;
                for ch in tab.ch_samples.iter_mut() { ch.drain(s..e); }
                tab.samples_len = tab.samples_len.saturating_sub(remove_len);
                tab.loop_region = None;
                tab.dirty = true;
                (Self::editor_mixdown_mono(tab), tab.loop_mode, tab.loop_region, tab.samples_len)
            } else { return; }
        };
        self.audio.set_samples(std::sync::Arc::new(mono));
        self.audio.stop();
        match loop_mode {
            crate::app::types::LoopMode::OnWhole => { self.audio.set_loop_enabled(true); self.audio.set_loop_region(0, len); }
            crate::app::types::LoopMode::Marker => { if let Some((a,b)) = lr { let (s,e) = if a<=b {(a,b)} else {(b,a)}; self.audio.set_loop_enabled(true); self.audio.set_loop_region(s,e); } else { self.audio.set_loop_enabled(false); } }
            crate::app::types::LoopMode::Off => { self.audio.set_loop_enabled(false); }
        }
    }
}
