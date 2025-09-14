use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use super::helpers::num_order;
use super::meta::spawn_meta_worker;
use super::types::{EditorTab, ProcessingResult, ProcessingState, RateMode, SortDir, SortKey};

impl super::WavesPreviewer {
    // multi-select aware selection update for list clicks (moved from app.rs)
    pub(super) fn update_selection_on_click(&mut self, row_idx: usize, mods: egui::Modifiers) {
        let len = self.files.len();
        if row_idx >= len { return; }
        if mods.shift {
            let anchor = self.select_anchor.or(self.selected).unwrap_or(row_idx);
            let (a,b) = if anchor <= row_idx { (anchor, row_idx) } else { (row_idx, anchor) };
            self.selected_multi.clear();
            for i in a..=b { self.selected_multi.insert(i); }
            self.selected = Some(row_idx);
            self.select_anchor = Some(anchor);
        } else if mods.ctrl || mods.command {
            if self.selected_multi.contains(&row_idx) { self.selected_multi.remove(&row_idx); } else { self.selected_multi.insert(row_idx); }
            self.selected = Some(row_idx);
            if self.select_anchor.is_none() { self.select_anchor = Some(row_idx); }
        } else {
            self.selected_multi.clear();
            self.selected_multi.insert(row_idx);
            self.selected = Some(row_idx);
            self.select_anchor = Some(row_idx);
        }
    }
    /// Select a row and load audio buffer accordingly.
    /// Used when any cell in the row is clicked so Space can play immediately.
    pub(super) fn select_and_load(&mut self, row_idx: usize) {
        if row_idx >= self.files.len() { return; }
        self.selected = Some(row_idx);
        self.scroll_to_selected = true;
        let p_owned = self.files[row_idx].clone();
        // record as current playing target
        self.playing_path = Some(p_owned.clone());
        // 繝ｪ繧ｹ繝郁｡ｨ遉ｺ譎ゅ・蟶ｸ縺ｫ繝ｫ繝ｼ繝励ｒ辟｡蜉ｹ縺ｫ縺吶ｋ
        self.audio.set_loop_enabled(false);
        match self.mode {
            RateMode::Speed => {
                let _ = crate::wave::prepare_for_speed(&p_owned, &self.audio, &mut Vec::new(), self.playback_rate);
                self.audio.set_rate(self.playback_rate);
            }
            _ => {
                self.audio.set_rate(1.0);
                self.spawn_heavy_processing(&p_owned);
            }
        }
        // apply effective volume including per-file gain
        self.apply_effective_volume();
    }
    pub fn rescan(&mut self) {
        self.files.clear();
        self.all_files.clear();
        if let Some(root) = &self.root {
            for entry in WalkDir::new(root).follow_links(false) {
                if let Ok(e) = entry {
                    if e.file_type().is_file() {
                        if let Some(ext) = e.path().extension().and_then(|s| s.to_str()) {
                            if ext.eq_ignore_ascii_case("wav") { self.all_files.push(e.into_path()); }
                        }
                    }
                }
            }
            self.all_files.sort();
        }
        // apply search filter and initialize files/original order
        self.apply_filter_from_search();
        self.meta.clear();
        if !self.all_files.is_empty() { self.meta_rx = Some(spawn_meta_worker(self.all_files.clone())); }
        // keep selection mapped to same path after rescan (best-effort)
        self.apply_sort();
    }

    pub(super) fn open_or_activate_tab(&mut self, path: &Path) {
        // 繧ｿ繝悶ｒ髢九￥/繧｢繧ｯ繝・ぅ繝門喧縺吶ｋ譎ゅ↓髻ｳ螢ｰ繧貞●豁｢
        self.audio.stop();

        if let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == path) {
            self.active_tab = Some(idx); return;
        }
        match self.mode {
            RateMode::Speed => {
                let mut wf = Vec::new();
                if let Err(e) = crate::wave::prepare_for_speed(path, &self.audio, &mut wf, self.playback_rate) { eprintln!("load error: {e:?}") }
                self.audio.set_rate(self.playback_rate);
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("(invalid)").to_string();
                // Multi-channel visualization at device SR
                let (mut chs, in_sr) = match crate::wave::decode_wav_multi(path) { Ok(v) => v, Err(_) => (Vec::new(), self.audio.shared.out_sample_rate) };
                if in_sr != self.audio.shared.out_sample_rate { for c in chs.iter_mut() { *c = crate::wave::resample_linear(c, in_sr, self.audio.shared.out_sample_rate); } }
                let samples_len = chs.get(0).map(|c| c.len()).unwrap_or(0);
                self.tabs.push(EditorTab {
                    path: path.to_path_buf(),
                    display_name: name,
                    waveform_minmax: wf,
                    loop_enabled: false,
                    ch_samples: chs,
                    samples_len,
                    view_offset: 0,
                    samples_per_px: 0.0,
                    dirty: false,
                    ops: Vec::new(),
                    selection: None,
                    ab_loop: None,
                    loop_region: None,
                    trim_range: None,
                    loop_xfade_samples: 0,
                    loop_xfade_shape: crate::app::types::LoopXfadeShape::EqualPower,
                    fade_in_range: None,
                    fade_out_range: None,
                    fade_in_shape: crate::app::types::FadeShape::SCurve,
                    fade_out_shape: crate::app::types::FadeShape::SCurve,
                    view_mode: crate::app::types::ViewMode::Waveform,
                    snap_zero_cross: true,
                    drag_select_anchor: None,
                    active_tool: crate::app::types::ToolKind::LoopEdit,
                    tool_state: crate::app::types::ToolState{ fade_in_ms: 20.0, fade_out_ms: 20.0, gain_db: 0.0, normalize_target_db: -6.0, pitch_semitones: 0.0, stretch_rate: 1.0 },
                    loop_mode: crate::app::types::LoopMode::Off,
                    dragging_marker: None,
                    preview_audio_tool: None,
                    active_tool_last: None,
                    preview_offset_samples: None,
                    preview_overlay_ch: None,
                });
                self.active_tab = Some(self.tabs.len() - 1);
                // Load loop markers from WAV (smpl) if available into loop_region
                if let Some(tab) = self.tabs.last_mut() {
                    if let Some((ls, le)) = crate::wave::read_wav_loop_markers(path) {
                        // Convert positions from source SR to device SR if needed
                        let out_sr = self.audio.shared.out_sample_rate.max(1);
                        let s = ((ls as u64) * (out_sr as u64) + (in_sr as u64/2)) / (in_sr as u64);
                        let e = ((le as u64) * (out_sr as u64) + (in_sr as u64/2)) / (in_sr as u64);
                        let s = (s as usize).min(tab.samples_len);
                        let e = (e as usize).min(tab.samples_len);
                        if e > s { tab.loop_region = Some((s,e)); }
                    }
                }
                self.playing_path = Some(path.to_path_buf());
            }
            _ => {
                // Heavy: create tab immediately with empty waveform, then spawn processing
                self.audio.set_rate(1.0);
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("(invalid)").to_string();
                let (mut chs, in_sr) = match crate::wave::decode_wav_multi(path) { Ok(v) => v, Err(_) => (Vec::new(), self.audio.shared.out_sample_rate) };
                if in_sr != self.audio.shared.out_sample_rate { for c in chs.iter_mut() { *c = crate::wave::resample_linear(c, in_sr, self.audio.shared.out_sample_rate); } }
                let samples_len = chs.get(0).map(|c| c.len()).unwrap_or(0);
                self.tabs.push(EditorTab {
                    path: path.to_path_buf(),
                    display_name: name,
                    waveform_minmax: Vec::new(),
                    loop_enabled: false,
                    ch_samples: chs,
                    samples_len,
                    view_offset: 0,
                    samples_per_px: 0.0,
                    dirty: false,
                    ops: Vec::new(),
                    selection: None,
                    ab_loop: None,
                    loop_region: None,
                    trim_range: None,
                    loop_xfade_samples: 0,
                    loop_xfade_shape: crate::app::types::LoopXfadeShape::EqualPower,
                    fade_in_range: None,
                    fade_out_range: None,
                    fade_in_shape: crate::app::types::FadeShape::SCurve,
                    fade_out_shape: crate::app::types::FadeShape::SCurve,
                    view_mode: crate::app::types::ViewMode::Waveform,
                    snap_zero_cross: true,
                    drag_select_anchor: None,
                    active_tool: crate::app::types::ToolKind::LoopEdit,
                    tool_state: crate::app::types::ToolState{ fade_in_ms: 20.0, fade_out_ms: 20.0, gain_db: 0.0, normalize_target_db: -6.0, pitch_semitones: 0.0, stretch_rate: 1.0 },
                    loop_mode: crate::app::types::LoopMode::Off,
                    dragging_marker: None,
                    preview_audio_tool: None,
                    active_tool_last: None,
                    preview_offset_samples: None,
                    preview_overlay_ch: None,
                });
                self.active_tab = Some(self.tabs.len() - 1);
                // Load loop markers into loop_region if present
                if let Some(tab) = self.tabs.last_mut() {
                    if let Some((ls, le)) = crate::wave::read_wav_loop_markers(path) {
                        let out_sr = self.audio.shared.out_sample_rate.max(1);
                        let s = ((ls as u64) * (out_sr as u64) + (in_sr as u64/2)) / (in_sr as u64);
                        let e = ((le as u64) * (out_sr as u64) + (in_sr as u64/2)) / (in_sr as u64);
                        let s = (s as usize).min(tab.samples_len);
                        let e = (e as usize).min(tab.samples_len);
                        if e > s { tab.loop_region = Some((s,e)); }
                    }
                }
                self.spawn_heavy_processing(path);
                self.playing_path = Some(path.to_path_buf());
            }
        }
    }

    // Merge helper: add a folder recursively (WAV only)
    pub(super) fn add_folder_merge(&mut self, dir: &Path) -> usize {
        let mut added = 0usize;
        let mut existing: HashSet<PathBuf> = self.all_files.iter().cloned().collect();
        for entry in WalkDir::new(dir).follow_links(false) {
            if let Ok(e) = entry {
                if e.file_type().is_file() {
                    let p = e.into_path();
                    if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                        if ext.eq_ignore_ascii_case("wav") {
                            if existing.insert(p.clone()) { self.all_files.push(p); added += 1; }
                        }
                    }
                }
            }
        }
        self.all_files.sort();
        added
    }

    // Merge helper: add explicit files (WAV only)
    pub(super) fn add_files_merge(&mut self, paths: &[PathBuf]) -> usize {
        let mut added = 0usize;
        let mut existing: HashSet<PathBuf> = self.all_files.iter().cloned().collect();
        for p in paths {
            if p.is_file() {
                if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    if ext.eq_ignore_ascii_case("wav") {
                        if existing.insert(p.clone()) { self.all_files.push(p.clone()); added += 1; }
                    }
                }
            } else if p.is_dir() {
                added += self.add_folder_merge(p.as_path());
            }
        }
        self.all_files.sort();
        added
    }

    pub(super) fn after_add_refresh(&mut self) {
        self.apply_filter_from_search();
        self.apply_sort();
        if !self.all_files.is_empty() { self.meta_rx = Some(spawn_meta_worker(self.all_files.clone())); }
    }

    // Replace current list with explicit files (WAV only). Root is cleared.
    pub(super) fn replace_with_files(&mut self, paths: &[PathBuf]) {
        self.root = None;
        self.files.clear();
        self.all_files.clear();
        let mut set: HashSet<PathBuf> = HashSet::new();
        for p in paths {
            if p.is_file() {
                if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    if ext.eq_ignore_ascii_case("wav") {
                        if set.insert(p.clone()) { self.all_files.push(p.clone()); }
                    }
                }
            }
        }
        self.all_files.sort();
    }

    pub(super) fn apply_filter_from_search(&mut self) {
        // Preserve selection path if possible
        let selected_path: Option<PathBuf> = self.selected.and_then(|i| self.files.get(i).cloned());
        if self.search_query.trim().is_empty() {
            self.files = self.all_files.clone();
        } else {
            let q = self.search_query.to_lowercase();
            self.files = self.all_files.iter().filter(|p| {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
                let parent = p.parent().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
                name.contains(&q) || parent.contains(&q)
            }).cloned().collect();
        }
        self.original_files = self.files.clone();
        // restore selected index
        self.selected = selected_path.and_then(|p| self.files.iter().position(|x| *x == p));
    }

    pub(super) fn apply_sort(&mut self) {
        if self.files.is_empty() { return; }
        let selected_path: Option<PathBuf> = self.selected.and_then(|i| self.files.get(i).cloned());
        let key = self.sort_key;
        let dir = self.sort_dir;
        if dir == SortDir::None {
            self.files = self.original_files.clone();
        } else {
            self.files.sort_by(|a, b| {
                use std::cmp::Ordering;
                let ord = match key {
                    SortKey::File => {
                        let sa = a.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        let sb = b.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        sa.cmp(sb)
                    }
                    SortKey::Folder => {
                        let sa = a.parent().and_then(|p| p.to_str()).unwrap_or("");
                        let sb = b.parent().and_then(|p| p.to_str()).unwrap_or("");
                        sa.cmp(sb)
                    }
                    SortKey::Length => num_order(self.meta.get(a).and_then(|m| m.duration_secs).unwrap_or(0.0),
                                                 self.meta.get(b).and_then(|m| m.duration_secs).unwrap_or(0.0)),
                    SortKey::Channels => num_order(self.meta.get(a).map(|m| m.channels as f32).unwrap_or(0.0),
                                                   self.meta.get(b).map(|m| m.channels as f32).unwrap_or(0.0)),
                    SortKey::SampleRate => num_order(self.meta.get(a).map(|m| m.sample_rate as f32).unwrap_or(0.0),
                                                     self.meta.get(b).map(|m| m.sample_rate as f32).unwrap_or(0.0)),
                    SortKey::Bits => num_order(self.meta.get(a).map(|m| m.bits_per_sample as f32).unwrap_or(0.0),
                                               self.meta.get(b).map(|m| m.bits_per_sample as f32).unwrap_or(0.0)),
                    SortKey::Level => num_order(self.meta.get(a).and_then(|m| m.peak_db).unwrap_or(f32::NEG_INFINITY),
                                                self.meta.get(b).and_then(|m| m.peak_db).unwrap_or(f32::NEG_INFINITY)),
                    // LUFS sorting uses effective value: override if present, else base + gain
                    SortKey::Lufs => {
                        let ga = *self.pending_gains.get(a).unwrap_or(&0.0);
                        let gb = *self.pending_gains.get(b).unwrap_or(&0.0);
                        let va = if let Some(v) = self.lufs_override.get(a) { *v } else { self.meta.get(a).and_then(|m| m.lufs_i.map(|x| x + ga)).unwrap_or(f32::NEG_INFINITY) };
                        let vb = if let Some(v) = self.lufs_override.get(b) { *v } else { self.meta.get(b).and_then(|m| m.lufs_i.map(|x| x + gb)).unwrap_or(f32::NEG_INFINITY) };
                        num_order(va, vb)
                    }
                };
                match dir { SortDir::Asc => ord, SortDir::Desc => ord.reverse(), SortDir::None => Ordering::Equal }
            });
        }

        // restore selection to the same path if possible
        self.selected = selected_path.and_then(|p| self.files.iter().position(|x| *x == p));
    }

    pub(super) fn current_path_for_rebuild(&self) -> Option<PathBuf> {
        if let Some(i) = self.active_tab { return self.tabs.get(i).map(|t| t.path.clone()); }
        if let Some(i) = self.selected { return self.files.get(i).cloned(); }
        None
    }

    pub(super) fn rebuild_current_buffer_with_mode(&mut self) {
        if let Some(p) = self.current_path_for_rebuild() {
            match self.mode {
                RateMode::Speed => { let _ = crate::wave::prepare_for_speed(&p, &self.audio, &mut Vec::new(), self.playback_rate); self.audio.set_rate(self.playback_rate); }
                _ => { self.audio.set_rate(1.0); self.spawn_heavy_processing(&p); }
            }
        }
    }

    pub(super) fn spawn_heavy_processing(&mut self, path: &Path) {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<ProcessingResult>();
        let path_buf = path.to_path_buf();
        let mode = self.mode;
        let rate = self.playback_rate;
        let sem = self.pitch_semitones;
        let out_sr = self.audio.shared.out_sample_rate;
        let path_for_thread = path_buf.clone();
        std::thread::spawn(move || {
            // heavy decode and process
            if let Ok((mono, in_sr)) = crate::wave::decode_wav_mono(&path_for_thread) {
                let samples = match mode {
                    RateMode::PitchShift => crate::wave::process_pitchshift_offline(&mono, in_sr, out_sr, sem),
                    RateMode::TimeStretch => crate::wave::process_timestretch_offline(&mono, in_sr, out_sr, rate),
                    RateMode::Speed => mono, // not used
                };
                let mut waveform = Vec::new();
                crate::wave::build_minmax(&mut waveform, &samples, 2048);
                let _ = tx.send(ProcessingResult { path: path_for_thread.clone(), samples, waveform });
            }
        });
        self.processing = Some(ProcessingState { msg: match mode { RateMode::PitchShift => "Pitch-shifting...".to_string(), RateMode::TimeStretch => "Time-stretching...".to_string(), RateMode::Speed => "Processing...".to_string() }, path: path_buf, rx });
    }
}
