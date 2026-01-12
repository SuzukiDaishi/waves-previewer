use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::audio_io;
use crate::loop_markers;
use crate::wave::prepare_for_speed;
use regex::RegexBuilder;

use walkdir::WalkDir;

use super::helpers::num_order;
use super::types::{EditorTab, ListPreviewResult, ProcessingResult, ProcessingState, RateMode, SortDir, SortKey, ScanMessage};

const LIST_PREVIEW_PREFIX_SECS: f32 = 1.0;

impl super::WavesPreviewer {
    fn mixdown_channels_mono(chs: &[Vec<f32>], len: usize) -> Vec<f32> {
        if len == 0 {
            return Vec::new();
        }
        if chs.is_empty() {
            return vec![0.0; len];
        }
        let chn = chs.len() as f32;
        let mut out = vec![0.0f32; len];
        for ch in chs {
            for i in 0..len {
                if let Some(&v) = ch.get(i) {
                    out[i] += v;
                }
            }
        }
        for v in &mut out {
            *v /= chn;
        }
        out
    }

    fn should_skip_path(&self, path: &Path) -> bool {
        self.skip_dotfiles && Self::is_dotfile_path(path)
    }

    pub(super) fn cache_dirty_tab_at(&mut self, idx: usize) {
        let (path, cached) = {
            let Some(tab) = self.tabs.get(idx) else {
                return;
            };
            if !tab.dirty && !tab.loop_markers_dirty && !tab.markers_dirty {
                return;
            }
            let mut waveform = tab.waveform_minmax.clone();
            if waveform.is_empty() {
                let mono = Self::mixdown_channels_mono(&tab.ch_samples, tab.samples_len);
                crate::wave::build_minmax(&mut waveform, &mono, 2048);
            }
            (
                tab.path.clone(),
                crate::app::types::CachedEdit {
                    ch_samples: tab.ch_samples.clone(),
                    samples_len: tab.samples_len,
                    waveform_minmax: waveform,
                    dirty: tab.dirty,
                    loop_region: tab.loop_region,
                    loop_markers_saved: tab.loop_markers_saved,
                    loop_markers_dirty: tab.loop_markers_dirty,
                    markers: tab.markers.clone(),
                    markers_saved: tab.markers_saved.clone(),
                    markers_dirty: tab.markers_dirty,
                    trim_range: tab.trim_range,
                    loop_xfade_samples: tab.loop_xfade_samples,
                    loop_xfade_shape: tab.loop_xfade_shape,
                    fade_in_range: tab.fade_in_range,
                    fade_out_range: tab.fade_out_range,
                    fade_in_shape: tab.fade_in_shape,
                    fade_out_shape: tab.fade_out_shape,
                    loop_mode: tab.loop_mode,
                    snap_zero_cross: tab.snap_zero_cross,
                    tool_state: tab.tool_state,
                    active_tool: tab.active_tool,
                },
            )
        };
        self.edited_cache.insert(path, cached);
    }

    pub(super) fn apply_dirty_tab_audio_with_mode(&mut self, path: &Path) -> bool {
        let decode_failed = self.is_decode_failed_path(path);
        let idx = match self
            .tabs
            .iter()
            .position(|t| (t.dirty || t.loop_markers_dirty) && t.path.as_path() == path)
        {
            Some(i) => i,
            None => {
                let mono = {
                    let cached = match self.edited_cache.get(path) {
                        Some(v) => v,
                        None => return false,
                    };
                    Self::mixdown_channels_mono(&cached.ch_samples, cached.samples_len)
                };
                self.playing_path = Some(path.to_path_buf());
                match self.mode {
                    RateMode::Speed => {
                        self.audio.set_samples(Arc::new(mono));
                        self.audio.stop();
                        self.audio.set_rate(self.playback_rate);
                    }
                    _ => {
                        if decode_failed {
                            self.audio.set_samples(Arc::new(mono));
                            self.audio.stop();
                            self.audio.set_rate(1.0);
                        } else {
                            self.audio.set_rate(1.0);
                            self.spawn_heavy_processing_from_mono(path.to_path_buf(), mono);
                        }
                    }
                }
                self.apply_effective_volume();
                return true;
            }
        };
        let (mono, tab_path) = {
            let tab = &self.tabs[idx];
            (Self::editor_mixdown_mono(tab), tab.path.clone())
        };
        self.playing_path = Some(tab_path.clone());
        match self.mode {
            RateMode::Speed => {
                self.audio.set_samples(Arc::new(mono));
                self.audio.stop();
                self.audio.set_rate(self.playback_rate);
            }
            _ => {
                if decode_failed {
                    self.audio.set_samples(Arc::new(mono));
                    self.audio.stop();
                    self.audio.set_rate(1.0);
                } else {
                    self.audio.set_rate(1.0);
                    self.spawn_heavy_processing_from_mono(tab_path.clone(), mono);
                }
            }
        }
        if let Some(tab) = self.tabs.get(idx) {
            self.apply_loop_mode_for_tab(tab);
        }
        self.apply_effective_volume();
        true
    }

    fn apply_dirty_tab_preview_for_list(&mut self, path: &Path) -> bool {
        let idx = match self
            .tabs
            .iter()
            .position(|t| (t.dirty || t.loop_markers_dirty) && t.path.as_path() == path)
        {
            Some(i) => i,
            None => {
                let mono = {
                    let cached = match self.edited_cache.get(path) {
                        Some(v) => v,
                        None => return false,
                    };
                    Self::mixdown_channels_mono(&cached.ch_samples, cached.samples_len)
                };
                self.playing_path = Some(path.to_path_buf());
                self.audio.set_loop_enabled(false);
                self.list_preview_rx = None;
                let rate = if self.mode == RateMode::Speed {
                    self.playback_rate
                } else {
                    1.0
                };
                self.audio.set_rate(rate);
                self.audio.set_samples(Arc::new(mono));
                self.audio.stop();
                self.apply_effective_volume();
                return true;
            }
        };
        let mono = {
            let tab = &self.tabs[idx];
            Self::editor_mixdown_mono(tab)
        };
        self.playing_path = Some(path.to_path_buf());
        self.audio.set_loop_enabled(false);
        self.list_preview_rx = None;
        let rate = if self.mode == RateMode::Speed {
            self.playback_rate
        } else {
            1.0
        };
        self.audio.set_rate(rate);
        self.audio.set_samples(Arc::new(mono));
        self.audio.stop();
        self.apply_effective_volume();
        true
    }

    pub(super) fn spawn_heavy_processing_from_mono(
        &mut self,
        path: PathBuf,
        mono: Vec<f32>,
    ) {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<ProcessingResult>();
        let mode = self.mode;
        let rate = self.playback_rate;
        let sem = self.pitch_semitones;
        let out_sr = self.audio.shared.out_sample_rate;
        let path_for_thread = path.clone();
        std::thread::spawn(move || {
            let samples = match mode {
                RateMode::PitchShift => {
                    crate::wave::process_pitchshift_offline(&mono, out_sr, out_sr, sem)
                }
                RateMode::TimeStretch => {
                    crate::wave::process_timestretch_offline(&mono, out_sr, out_sr, rate)
                }
                RateMode::Speed => mono,
            };
            let mut waveform = Vec::new();
            crate::wave::build_minmax(&mut waveform, &samples, 2048);
            let _ = tx.send(ProcessingResult {
                path: path_for_thread,
                samples,
                waveform,
                channels: Vec::new(),
            });
        });
        self.processing = Some(ProcessingState {
            msg: match mode {
                RateMode::PitchShift => "Pitch-shifting...".to_string(),
                RateMode::TimeStretch => "Time-stretching...".to_string(),
                RateMode::Speed => "Processing...".to_string(),
            },
            path,
            autoplay_when_ready: false,
            rx,
        });
    }

    pub(super) fn has_edits_for_paths(&self, paths: &[PathBuf]) -> bool {
        paths.iter().any(|p| {
            self.has_pending_gain(p)
                || self
                    .edited_cache
                    .get(p)
                    .map(|c| c.dirty || c.loop_markers_dirty || c.markers_dirty)
                    .unwrap_or(false)
                || self.tabs.iter().any(|t| {
                    (t.dirty || t.loop_markers_dirty || t.markers_dirty)
                        && t.path.as_path() == p.as_path()
                })
        })
    }

    fn reset_tab_defaults(tab: &mut EditorTab) {
        tab.view_offset = 0;
        tab.samples_per_px = 0.0;
        tab.last_wave_w = 0.0;
        tab.dirty = false;
        tab.ops.clear();
        tab.selection = None;
        tab.markers.clear();
        tab.markers_saved.clear();
        tab.markers_dirty = false;
        tab.ab_loop = None;
        tab.loop_region = None;
        tab.loop_markers_saved = None;
        tab.loop_markers_dirty = false;
        tab.trim_range = None;
        tab.loop_xfade_samples = 0;
        tab.loop_xfade_shape = crate::app::types::LoopXfadeShape::EqualPower;
        tab.fade_in_range = None;
        tab.fade_out_range = None;
        tab.fade_in_shape = crate::app::types::FadeShape::SCurve;
        tab.fade_out_shape = crate::app::types::FadeShape::SCurve;
        tab.view_mode = crate::app::types::ViewMode::Waveform;
        tab.snap_zero_cross = true;
        tab.drag_select_anchor = None;
        tab.active_tool = crate::app::types::ToolKind::LoopEdit;
        tab.tool_state = crate::app::types::ToolState {
            fade_in_ms: 0.0,
            fade_out_ms: 0.0,
            gain_db: 0.0,
            normalize_target_db: -6.0,
            pitch_semitones: 0.0,
            stretch_rate: 1.0,
        };
        tab.loop_mode = crate::app::types::LoopMode::Off;
        tab.dragging_marker = None;
        tab.preview_audio_tool = None;
        tab.active_tool_last = None;
        tab.preview_offset_samples = None;
        tab.preview_overlay = None;
        tab.undo_stack.clear();
        tab.undo_bytes = 0;
        tab.redo_stack.clear();
        tab.redo_bytes = 0;
    }

    fn reset_tab_from_disk(&mut self, idx: usize, update_audio: bool) -> bool {
        let path = match self.tabs.get(idx) {
            Some(t) => t.path.clone(),
            None => return false,
        };
        if !path.is_file() {
            self.remove_missing_path(&path);
            return false;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string();
        let out_sr = self.audio.shared.out_sample_rate;
        match self.mode {
            RateMode::Speed => {
                let mut waveform = Vec::new();
                if let Ok((mono, _in_sr)) = crate::wave::decode_wav_mono(&path) {
                    crate::wave::build_minmax(&mut waveform, &mono, 2048);
                    if update_audio {
                        let _ = prepare_for_speed(&path, &self.audio, &mut Vec::new(), self.playback_rate);
                        self.audio.set_rate(self.playback_rate);
                    }
                } else if update_audio {
                    let _ = prepare_for_speed(&path, &self.audio, &mut Vec::new(), self.playback_rate);
                    self.audio.set_rate(self.playback_rate);
                }
                let (mut chs, in_sr) = match crate::wave::decode_wav_multi(&path) {
                    Ok(v) => v,
                    Err(_) => (Vec::new(), out_sr),
                };
                if in_sr != out_sr {
                    for c in chs.iter_mut() {
                        *c = crate::wave::resample_linear(c, in_sr, out_sr);
                    }
                }
                let samples_len = chs.get(0).map(|c| c.len()).unwrap_or(0);
                let file_sr = self.sample_rate_for_path(&path, in_sr);
                if let Some(tab) = self.tabs.get_mut(idx) {
                    tab.display_name = name;
                    tab.waveform_minmax = waveform;
                    tab.ch_samples = chs;
                    tab.samples_len = samples_len;
                    Self::reset_tab_defaults(tab);
                    Self::set_loop_region_from_file_markers(tab, &path, in_sr, out_sr);
                    Self::load_markers_for_tab(tab, &path, out_sr, file_sr);
                }
            }
            _ => {
                let (mut chs, in_sr) = match crate::wave::decode_wav_multi(&path) {
                    Ok(v) => v,
                    Err(_) => (Vec::new(), out_sr),
                };
                if in_sr != out_sr {
                    for c in chs.iter_mut() {
                        *c = crate::wave::resample_linear(c, in_sr, out_sr);
                    }
                }
                let samples_len = chs.get(0).map(|c| c.len()).unwrap_or(0);
                let file_sr = self.sample_rate_for_path(&path, in_sr);
                if let Some(tab) = self.tabs.get_mut(idx) {
                    tab.display_name = name;
                    tab.waveform_minmax.clear();
                    tab.ch_samples = chs;
                    tab.samples_len = samples_len;
                    Self::reset_tab_defaults(tab);
                    Self::set_loop_region_from_file_markers(tab, &path, in_sr, out_sr);
                    Self::load_markers_for_tab(tab, &path, out_sr, file_sr);
                }
                if update_audio {
                    self.audio.set_rate(1.0);
                    self.spawn_heavy_processing(&path);
                }
            }
        }
        if update_audio {
            self.playing_path = Some(path.clone());
            if let Some(tab) = self.tabs.get(idx) {
                self.apply_loop_mode_for_tab(tab);
            }
            self.apply_effective_volume();
        }
        true
    }

    pub(super) fn clear_edits_for_paths(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let mut unique: HashSet<PathBuf> = HashSet::new();
        let mut reload_playing = false;
        let mut affect_playing = false;
        for p in paths {
            if !unique.insert(p.clone()) {
                continue;
            }
            self.set_pending_gain_db_for_path(p, 0.0);
            self.lufs_override.remove(p);
            self.lufs_recalc_deadline.remove(p);
            if self.playing_path.as_ref() == Some(p) {
                affect_playing = true;
            }
            self.edited_cache.remove(p);
            if let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == p.as_path()) {
                let update_audio = self.active_tab == Some(idx);
                self.reset_tab_from_disk(idx, update_audio);
            }
            if self.active_tab.is_none() && self.playing_path.as_ref() == Some(p) {
                reload_playing = true;
            }
        }
        if reload_playing {
            if let Some(p) = self.playing_path.clone() {
                if let Some(row) = self.row_for_path(&p) {
                    self.select_and_load(row, false);
                }
            }
        }
        if affect_playing {
            self.apply_effective_volume();
        }
    }

    /// Helper: read loop markers and map to given output SR, set tab.loop_region if valid
    fn set_loop_region_from_file_markers(tab: &mut EditorTab, path: &Path, in_sr: u32, out_sr: u32) {
        let mut saved = None;
        if let Some((ls, le)) = loop_markers::read_loop_markers(path) {
            let ls = (ls.min(u32::MAX as u64)) as u32;
            let le = (le.min(u32::MAX as u64)) as u32;
            if let Some((s, e)) =
                crate::wave::map_loop_markers_between_sr(ls, le, in_sr, out_sr, tab.samples_len)
            {
                tab.loop_region = Some((s, e));
                saved = Some((s, e));
            } else {
                tab.loop_region = None;
            }
        } else {
            tab.loop_region = None;
        }
        tab.loop_markers_saved = saved;
        tab.loop_markers_dirty = false;
    }

    fn sample_rate_for_path(&self, path: &Path, fallback: u32) -> u32 {
        self.meta_for_path(path)
            .map(|m| m.sample_rate)
            .filter(|&sr| sr > 0)
            .or_else(|| audio_io::read_audio_info(path).ok().map(|i| i.sample_rate))
            .unwrap_or(fallback)
    }

    fn load_markers_for_tab(tab: &mut EditorTab, path: &Path, out_sr: u32, file_sr: u32) {
        let out_sr = out_sr.max(1);
        match crate::markers::read_markers(path, out_sr, file_sr) {
            Ok(mut markers) => {
                markers.retain(|m| m.sample <= tab.samples_len);
                tab.markers = markers.clone();
                tab.markers_saved = markers;
                tab.markers_dirty = false;
            }
            Err(err) => {
                eprintln!("read markers failed {}: {err:?}", path.display());
                tab.markers.clear();
                tab.markers_saved.clear();
                tab.markers_dirty = false;
            }
        }
    }

    pub(super) fn write_markers_for_tab(&mut self, tab_idx: usize) -> bool {
        let (path, markers, file_sr) = {
            let Some(tab) = self.tabs.get(tab_idx) else { return false; };
            let file_sr = self.sample_rate_for_path(&tab.path, self.audio.shared.out_sample_rate);
            (tab.path.clone(), tab.markers.clone(), file_sr)
        };
        if !path.is_file() {
            self.remove_missing_path(&path);
            return false;
        }
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        if let Err(err) = crate::markers::write_markers(&path, out_sr, file_sr, &markers) {
            eprintln!("write markers failed {}: {err:?}", path.display());
            return false;
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.markers_saved = tab.markers.clone();
            tab.markers_dirty = false;
        }
        true
    }

    pub(super) fn write_loop_markers_for_tab(&mut self, tab_idx: usize) -> bool {
        let (path, loop_region, out_sr) = {
            let Some(tab) = self.tabs.get(tab_idx) else { return false; };
            (tab.path.clone(), tab.loop_region, self.audio.shared.out_sample_rate)
        };
        if !path.is_file() {
            self.remove_missing_path(&path);
            return false;
        }
        let file_sr = self
            .meta_for_path(&path)
            .map(|m| m.sample_rate)
            .filter(|&sr| sr > 0)
            .or_else(|| audio_io::read_audio_info(&path).ok().map(|i| i.sample_rate))
            .unwrap_or(out_sr);
        let mut loop_opt: Option<(u64, u64)> = None;
        if let Some((s, e)) = loop_region {
            if let Some((mut ls, mut le)) =
                crate::wave::map_loop_markers_to_file_sr(s, e, out_sr, file_sr)
            {
                if let Some(meta) = self.meta_for_path(&path) {
                    if let Some(secs) = meta.duration_secs {
                        let max = (secs * file_sr as f32).round().max(0.0) as u64;
                        if max > 0 {
                            ls = (ls as u64).min(max) as u32;
                            le = (le as u64).min(max) as u32;
                        }
                    }
                }
                if le > ls {
                    loop_opt = Some((ls as u64, le as u64));
                }
            }
        }
        if let Err(err) = loop_markers::write_loop_markers(&path, loop_opt) {
            eprintln!("write loop markers failed {}: {err:?}", path.display());
            return false;
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.loop_markers_saved = tab.loop_region;
            tab.loop_markers_dirty = false;
        }
        true
    }
    // multi-select aware selection update for list clicks (moved from app.rs)
    pub(super) fn update_selection_on_click(&mut self, row_idx: usize, mods: egui::Modifiers) {
        let len = self.files.len();
        if row_idx >= len {
            return;
        }
        if mods.shift {
            let anchor = self.select_anchor.or(self.selected).unwrap_or(row_idx);
            let (a, b) = if anchor <= row_idx {
                (anchor, row_idx)
            } else {
                (row_idx, anchor)
            };
            self.selected_multi.clear();
            for i in a..=b {
                self.selected_multi.insert(i);
            }
            self.selected = Some(row_idx);
            self.select_anchor = Some(anchor);
        } else if mods.ctrl || mods.command {
            if self.selected_multi.contains(&row_idx) {
                self.selected_multi.remove(&row_idx);
            } else {
                self.selected_multi.insert(row_idx);
            }
            self.selected = Some(row_idx);
            if self.select_anchor.is_none() {
                self.select_anchor = Some(row_idx);
            }
        } else {
            self.selected_multi.clear();
            self.selected_multi.insert(row_idx);
            self.selected = Some(row_idx);
            self.select_anchor = Some(row_idx);
        }
    }
    /// Select a row and load audio buffer accordingly.
    /// Used when any cell in the row is clicked so Space can play immediately.
    pub(super) fn select_and_load(&mut self, row_idx: usize, auto_scroll: bool) {
        if row_idx >= self.files.len() {
            return;
        }
        self.selected = Some(row_idx);
        self.scroll_to_selected = auto_scroll;
        let Some(p_owned) = self.path_for_row(row_idx).cloned() else {
            return;
        };
        if !p_owned.is_file() {
            self.remove_missing_path(&p_owned);
            return;
        }
        if self.apply_dirty_tab_preview_for_list(&p_owned) {
            return;
        }
        let need_heavy = match self.mode {
            RateMode::PitchShift => self.pitch_semitones.abs() > 0.0001,
            RateMode::TimeStretch => (self.playback_rate - 1.0).abs() > 0.0001,
            RateMode::Speed => false,
        };
        let decode_failed = self.is_decode_failed_path(&p_owned);
        // record as current playing target
        self.playing_path = Some(p_owned.clone());
        // stop looping for list preview
        self.audio.set_loop_enabled(false);
        // cancel any previous list preview job
        self.list_preview_rx = None;
        if need_heavy && !decode_failed {
            self.audio.set_rate(1.0);
            self.audio.stop();
            self.audio.set_samples(Arc::new(Vec::new()));
            self.spawn_heavy_processing(&p_owned);
            self.apply_effective_volume();
            return;
        }
        let rate = if self.mode == RateMode::Speed {
            self.playback_rate
        } else {
            1.0
        };
        self.audio.set_rate(rate);
        match crate::wave::prepare_for_list_preview(
            &p_owned,
            &self.audio,
            LIST_PREVIEW_PREFIX_SECS,
        ) {
            Ok(truncated) => {
                if truncated {
                    self.spawn_list_preview_full(p_owned.clone());
                }
            }
            Err(e) => {
                if !p_owned.is_file() {
                    self.remove_missing_path(&p_owned);
                } else {
                    eprintln!("load error: {e:?}");
                    self.audio.stop();
                    self.audio.set_samples(Arc::new(Vec::new()));
                }
            }
        }
        // apply effective volume including per-file gain
        self.apply_effective_volume();
    }

    fn spawn_list_preview_full(&mut self, path: PathBuf) {
        use std::sync::mpsc;
        self.list_preview_job_id = self.list_preview_job_id.wrapping_add(1);
        let job_id = self.list_preview_job_id;
        let out_sr = self.audio.shared.out_sample_rate;
        let (tx, rx) = mpsc::channel::<ListPreviewResult>();
        std::thread::spawn(move || {
            let res = (|| -> anyhow::Result<ListPreviewResult> {
                let (mono, in_sr) = crate::wave::decode_wav_mono(&path)?;
                let resampled = crate::wave::resample_linear(&mono, in_sr, out_sr);
                Ok(ListPreviewResult {
                    path,
                    samples: resampled,
                    job_id,
                })
            })();
            if let Ok(result) = res {
                let _ = tx.send(result);
            }
        });
        self.list_preview_rx = Some(rx);
    }

    pub(super) fn remove_missing_path(&mut self, path: &Path) {
        if path.exists() {
            return;
        }
        let Some(id) = self.path_index.get(path).copied() else {
            return;
        };
        let selected_path = self.selected_path_buf();
        let selected_paths: Vec<PathBuf> = self
            .selected_multi
            .iter()
            .filter_map(|&row| self.path_for_row(row).cloned())
            .collect();
        let anchor_path = self
            .select_anchor
            .and_then(|row| self.path_for_row(row).cloned());
        let path_buf = path.to_path_buf();
        let was_playing = self.playing_path.as_ref() == Some(&path_buf);

        if let Some(idx) = self.item_index.remove(&id) {
            self.items.remove(idx);
            for i in idx..self.items.len() {
                let id = self.items[i].id;
                self.item_index.insert(id, i);
            }
        }
        self.path_index.remove(&path_buf);
        self.files.retain(|&fid| fid != id);
        self.original_files.retain(|&fid| fid != id);

        self.meta_inflight.remove(&path_buf);
        self.transcript_inflight.remove(&path_buf);
        self.spectro_cache.remove(&path_buf);
        self.spectro_inflight.remove(&path_buf);
        self.edited_cache.remove(&path_buf);
        self.lufs_override.remove(&path_buf);
        self.lufs_recalc_deadline.remove(&path_buf);
        if was_playing {
            self.playing_path = None;
            self.list_preview_rx = None;
            self.audio.stop();
        }
        if self.external_source.is_some() {
            self.apply_external_mapping();
        }
        self.apply_filter_from_search();
        self.apply_sort();
        self.selected = selected_path.and_then(|p| self.row_for_path(&p));
        self.selected_multi.clear();
        for p in selected_paths {
            if let Some(row) = self.row_for_path(&p) {
                self.selected_multi.insert(row);
            }
        }
        if let Some(sel) = self.selected {
            if self.selected_multi.is_empty() {
                self.selected_multi.insert(sel);
            }
        }
        self.select_anchor = anchor_path.and_then(|p| self.row_for_path(&p));
        if self.files.is_empty() {
            self.selected = None;
            self.selected_multi.clear();
            self.select_anchor = None;
        }
    }

    pub(super) fn remove_paths_from_list(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let unique: HashSet<PathBuf> = paths.iter().cloned().collect();
        if unique.is_empty() {
            return;
        }
        let selected_path = self.selected_path_buf();
        let selected_paths: Vec<PathBuf> = self
            .selected_multi
            .iter()
            .filter_map(|&row| self.path_for_row(row).cloned())
            .collect();
        let anchor_path = self
            .select_anchor
            .and_then(|row| self.path_for_row(row).cloned());
        let was_playing = self
            .playing_path
            .as_ref()
            .map(|p| unique.contains(p))
            .unwrap_or(false);

        let mut removed_ids = HashSet::new();
        for path in unique.iter() {
            if let Some(id) = self.path_index.get(path).copied() {
                removed_ids.insert(id);
            }
        }
        if removed_ids.is_empty() {
            return;
        }
        self.items.retain(|item| !removed_ids.contains(&item.id));
        self.rebuild_item_indexes();
        self.files.retain(|id| !removed_ids.contains(id));
        self.original_files.retain(|id| !removed_ids.contains(id));

        for path in unique.iter() {
            self.meta_inflight.remove(path);
            self.transcript_inflight.remove(path);
            self.spectro_cache.remove(path);
            self.spectro_inflight.remove(path);
            self.edited_cache.remove(path);
            self.lufs_override.remove(path);
            self.lufs_recalc_deadline.remove(path);
        }
        if was_playing {
            self.playing_path = None;
            self.list_preview_rx = None;
            self.audio.stop();
        }
        if self.external_source.is_some() {
            self.apply_external_mapping();
        }
        self.apply_filter_from_search();
        self.apply_sort();
        self.selected = selected_path.and_then(|p| self.row_for_path(&p));
        self.selected_multi.clear();
        for p in selected_paths {
            if let Some(row) = self.row_for_path(&p) {
                self.selected_multi.insert(row);
            }
        }
        if let Some(sel) = self.selected {
            if self.selected_multi.is_empty() {
                self.selected_multi.insert(sel);
            }
        }
        self.select_anchor = anchor_path.and_then(|p| self.row_for_path(&p));
        if self.files.is_empty() {
            self.selected = None;
            self.selected_multi.clear();
            self.select_anchor = None;
        }
    }
    pub fn rescan(&mut self) {
        self.files.clear();
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.scan_rx = None;
        self.scan_in_progress = false;
        if let Some(root) = &self.root {
            self.start_scan_folder(root.clone());
        } else {
            self.apply_filter_from_search();
            self.apply_sort();
        }
    }

    pub(super) fn open_or_activate_tab(&mut self, path: &Path) {
        if !path.is_file() {
            self.remove_missing_path(path);
            return;
        }
        let decode_failed = self.is_decode_failed_path(path);
        // 繧ｿ繝悶ｒ髢九￥/繧｢繧ｯ繝・ぅ繝門喧縺吶ｋ譎ゅ↓髻ｳ螢ｰ繧貞●豁｢
        self.audio.stop();

        if let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == path) {
            self.active_tab = Some(idx);
            return;
        }
        if self.tabs.len() >= crate::app::MAX_EDITOR_TABS {
            self.debug_log(format!(
                "tab limit reached ({}); skipping {}",
                crate::app::MAX_EDITOR_TABS,
                path.display()
            ));
            return;
        }
        if let Some(cached) = self.edited_cache.remove(path) {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("(invalid)")
                .to_string();
            self.tabs.push(EditorTab {
                path: path.to_path_buf(),
                display_name: name,
                waveform_minmax: cached.waveform_minmax,
                loop_enabled: false,
                ch_samples: cached.ch_samples,
                samples_len: cached.samples_len,
                view_offset: 0,
                samples_per_px: 0.0,
                last_wave_w: 0.0,
                dirty: cached.dirty,
                ops: Vec::new(),
                selection: None,
                markers: cached.markers,
                markers_saved: cached.markers_saved,
                markers_dirty: cached.markers_dirty,
                ab_loop: None,
                loop_region: cached.loop_region,
                loop_markers_saved: cached.loop_markers_saved,
                loop_markers_dirty: cached.loop_markers_dirty,
                trim_range: cached.trim_range,
                loop_xfade_samples: cached.loop_xfade_samples,
                loop_xfade_shape: cached.loop_xfade_shape,
                fade_in_range: cached.fade_in_range,
                fade_out_range: cached.fade_out_range,
                fade_in_shape: cached.fade_in_shape,
                fade_out_shape: cached.fade_out_shape,
                view_mode: crate::app::types::ViewMode::Waveform,
                snap_zero_cross: cached.snap_zero_cross,
                drag_select_anchor: None,
                active_tool: cached.active_tool,
                tool_state: cached.tool_state,
                loop_mode: cached.loop_mode,
                dragging_marker: None,
                preview_audio_tool: None,
                active_tool_last: None,
                preview_offset_samples: None,
                preview_overlay: None,
                undo_stack: Vec::new(),
                undo_bytes: 0,
                redo_stack: Vec::new(),
                redo_bytes: 0,
            });
            self.active_tab = Some(self.tabs.len() - 1);
            self.playing_path = Some(path.to_path_buf());
            self.apply_dirty_tab_audio_with_mode(path);
            return;
        }
        match self.mode {
            RateMode::Speed => {
                let mut wf = Vec::new();
                if let Err(e) =
                    crate::wave::prepare_for_speed(path, &self.audio, &mut wf, self.playback_rate)
                {
                    eprintln!("load error: {e:?}")
                }
                self.audio.set_rate(self.playback_rate);
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(invalid)")
                    .to_string();
                // Multi-channel visualization at device SR
                let (mut chs, in_sr) = match crate::wave::decode_wav_multi(path) {
                    Ok(v) => v,
                    Err(_) => (Vec::new(), self.audio.shared.out_sample_rate),
                };
                if in_sr != self.audio.shared.out_sample_rate {
                    for c in chs.iter_mut() {
                        *c = crate::wave::resample_linear(
                            c,
                            in_sr,
                            self.audio.shared.out_sample_rate,
                        );
                    }
                }
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
                    last_wave_w: 0.0,
                    dirty: false,
                    ops: Vec::new(),
                    selection: None,
                    markers: Vec::new(),
                    markers_saved: Vec::new(),
                    markers_dirty: false,
                    ab_loop: None,
                    loop_region: None,
                    loop_markers_saved: None,
                    loop_markers_dirty: false,
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
                    tool_state: crate::app::types::ToolState {
                        fade_in_ms: 0.0,
                        fade_out_ms: 0.0,
                        gain_db: 0.0,
                        normalize_target_db: -6.0,
                        pitch_semitones: 0.0,
                        stretch_rate: 1.0,
                    },
                    loop_mode: crate::app::types::LoopMode::Off,
                    dragging_marker: None,
                    preview_audio_tool: None,
                    active_tool_last: None,
                    preview_offset_samples: None,
                    preview_overlay: None,
                    undo_stack: Vec::new(),
                    undo_bytes: 0,
                    redo_stack: Vec::new(),
                    redo_bytes: 0,
                });
                self.active_tab = Some(self.tabs.len() - 1);
                let out_sr = self.audio.shared.out_sample_rate;
                let file_sr = self.sample_rate_for_path(path, in_sr);
                // Load loop markers from file if available into loop_region
                if let Some(tab) = self.tabs.last_mut() {
                    Self::set_loop_region_from_file_markers(tab, path, in_sr, out_sr);
                    Self::load_markers_for_tab(tab, path, out_sr, file_sr);
                }
                self.playing_path = Some(path.to_path_buf());
            }
            _ => {
                // Heavy: create tab immediately with empty waveform, then spawn processing
                self.audio.set_rate(1.0);
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(invalid)")
                    .to_string();
                let (mut chs, in_sr) = match crate::wave::decode_wav_multi(path) {
                    Ok(v) => v,
                    Err(_) => (Vec::new(), self.audio.shared.out_sample_rate),
                };
                if in_sr != self.audio.shared.out_sample_rate {
                    for c in chs.iter_mut() {
                        *c = crate::wave::resample_linear(
                            c,
                            in_sr,
                            self.audio.shared.out_sample_rate,
                        );
                    }
                }
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
                    last_wave_w: 0.0,
                    dirty: false,
                    ops: Vec::new(),
                    selection: None,
                    markers: Vec::new(),
                    markers_saved: Vec::new(),
                    markers_dirty: false,
                    ab_loop: None,
                    loop_region: None,
                    loop_markers_saved: None,
                    loop_markers_dirty: false,
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
                    tool_state: crate::app::types::ToolState {
                        fade_in_ms: 0.0,
                        fade_out_ms: 0.0,
                        gain_db: 0.0,
                        normalize_target_db: -6.0,
                        pitch_semitones: 0.0,
                        stretch_rate: 1.0,
                    },
                    loop_mode: crate::app::types::LoopMode::Off,
                    dragging_marker: None,
                    preview_audio_tool: None,
                    active_tool_last: None,
                    preview_offset_samples: None,
                    preview_overlay: None,
                    undo_stack: Vec::new(),
                    undo_bytes: 0,
                    redo_stack: Vec::new(),
                    redo_bytes: 0,
                });
                self.active_tab = Some(self.tabs.len() - 1);
                let out_sr = self.audio.shared.out_sample_rate;
                let file_sr = self.sample_rate_for_path(path, in_sr);
                // Load loop markers into loop_region if present
                if let Some(tab) = self.tabs.last_mut() {
                    Self::set_loop_region_from_file_markers(tab, path, in_sr, out_sr);
                    Self::load_markers_for_tab(tab, path, out_sr, file_sr);
                }
                if decode_failed {
                    let _ = crate::wave::prepare_for_speed(
                        path,
                        &self.audio,
                        &mut Vec::new(),
                        1.0,
                    );
                    self.audio.set_rate(1.0);
                } else {
                    self.spawn_heavy_processing(path);
                }
                self.playing_path = Some(path.to_path_buf());
            }
        }
    }

    pub(super) fn open_paths_in_tabs(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.open_or_activate_tab(path);
        }
    }

    // Merge helper: add a folder recursively (supported audio only)
    pub(super) fn add_folder_merge(&mut self, dir: &Path) -> usize {
        let mut added = 0usize;
        let skip_dotfiles = self.skip_dotfiles;
        for entry in WalkDir::new(dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !skip_dotfiles || !Self::is_dotfile_path(e.path()))
        {
            if let Ok(e) = entry {
                if e.file_type().is_file() {
                    let p = e.into_path();
                    if self.should_skip_path(&p) {
                        continue;
                    }
                    if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                        if audio_io::is_supported_extension(ext) {
                            if self.path_index.contains_key(&p) {
                                continue;
                            }
                            let item = self.make_media_item(p.clone());
                            let id = item.id;
                            self.path_index.insert(p.clone(), id);
                            self.item_index.insert(id, self.items.len());
                            self.items.push(item);
                            added += 1;
                        }
                    }
                }
            }
        }
        added
    }

    // Merge helper: add explicit files (supported audio only)
    pub(super) fn add_files_merge(&mut self, paths: &[PathBuf]) -> usize {
        let mut added = 0usize;
        for p in paths {
            if p.is_file() {
                if self.should_skip_path(p) {
                    continue;
                }
                if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    if audio_io::is_supported_extension(ext) {
                        if self.path_index.contains_key(p) {
                            continue;
                        }
                        let item = self.make_media_item(p.clone());
                        let id = item.id;
                        self.path_index.insert(p.clone(), id);
                        self.item_index.insert(id, self.items.len());
                        self.items.push(item);
                        added += 1;
                    }
                }
            } else if p.is_dir() {
                added += self.add_folder_merge(p.as_path());
            }
        }
        added
    }

    pub(super) fn after_add_refresh(&mut self) {
        if self.external_source.is_some() {
            self.apply_external_mapping();
        }
        self.apply_filter_from_search();
        self.apply_sort();
        self.ensure_meta_pool();
    }

    // Replace current list with explicit files (supported audio only). Root is cleared.
    pub(super) fn replace_with_files(&mut self, paths: &[PathBuf]) {
        self.root = None;
        self.files.clear();
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.scan_rx = None;
        self.scan_in_progress = false;
        let mut set: HashSet<PathBuf> = HashSet::new();
        for p in paths {
            if p.is_file() {
                if self.should_skip_path(p) {
                    continue;
                }
                if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    if audio_io::is_supported_extension(ext) {
                        if set.insert(p.clone()) {
                            let item = self.make_media_item(p.clone());
                            let id = item.id;
                            self.path_index.insert(p.clone(), id);
                            self.item_index.insert(id, self.items.len());
                            self.items.push(item);
                        }
                    }
                }
            }
        }
        self.ensure_meta_pool();
    }

    pub(super) fn apply_filter_from_search(&mut self) {
        // Preserve selection index if possible
        let selected_idx = self.selected.and_then(|i| self.files.get(i).copied());
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            self.files = self.items.iter().map(|item| item.id).collect();
        } else if self.search_use_regex {
            let re = RegexBuilder::new(&query)
                .case_insensitive(true)
                .build();
            if let Ok(re) = re {
                self.files = self
                    .items
                    .iter()
                    .filter(|item| {
                        let p = &item.path;
                        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        let parent = p.parent().and_then(|s| s.to_str()).unwrap_or("");
                        let transcript = item
                            .transcript
                            .as_ref()
                            .map(|t| t.full_text.as_str())
                            .unwrap_or("");
                        let meta_text = item
                            .meta
                            .as_ref()
                            .map(|m| {
                                format!(
                                    "sr:{} bits:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1}",
                                    m.sample_rate,
                                    m.bits_per_sample,
                                    m.channels,
                                    m.duration_secs.unwrap_or(0.0),
                                    m.peak_db.unwrap_or(0.0),
                                    m.lufs_i.unwrap_or(0.0)
                                )
                            })
                            .unwrap_or_default();
                        let external_hit = item.external.values().any(|v| re.is_match(v));
                        re.is_match(name)
                            || re.is_match(parent)
                            || re.is_match(transcript)
                            || re.is_match(&meta_text)
                            || external_hit
                    })
                    .map(|item| item.id)
                    .collect();
            } else {
                let q = query.to_lowercase();
                self.files = self
                    .items
                    .iter()
                    .filter(|item| {
                        let p = &item.path;
                        let name = p
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let parent = p
                            .parent()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let transcript = item
                            .transcript
                            .as_ref()
                            .map(|t| t.full_text.to_lowercase())
                            .unwrap_or_default();
                        let meta_text = item
                            .meta
                            .as_ref()
                            .map(|m| {
                                format!(
                                    "sr:{} bits:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1}",
                                    m.sample_rate,
                                    m.bits_per_sample,
                                    m.channels,
                                    m.duration_secs.unwrap_or(0.0),
                                    m.peak_db.unwrap_or(0.0),
                                    m.lufs_i.unwrap_or(0.0)
                                )
                            })
                            .unwrap_or_default();
                        let external_hit = item
                            .external
                            .values()
                            .any(|v| v.to_lowercase().contains(&q));
                        name.contains(&q)
                            || parent.contains(&q)
                            || transcript.contains(&q)
                            || meta_text.to_lowercase().contains(&q)
                            || external_hit
                    })
                    .map(|item| item.id)
                    .collect();
            }
        } else {
            let q = query.to_lowercase();
            self.files = self
                .items
                .iter()
                .filter(|item| {
                    let p = &item.path;
                    let name = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let parent = p
                        .parent()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                let transcript = item
                    .transcript
                    .as_ref()
                    .map(|t| t.full_text.to_lowercase())
                    .unwrap_or_default();
                let meta_text = item
                    .meta
                    .as_ref()
                    .map(|m| {
                        format!(
                            "sr:{} bits:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1}",
                            m.sample_rate,
                            m.bits_per_sample,
                            m.channels,
                            m.duration_secs.unwrap_or(0.0),
                            m.peak_db.unwrap_or(0.0),
                            m.lufs_i.unwrap_or(0.0)
                        )
                    })
                    .unwrap_or_default();
                let external_hit = item
                    .external
                    .values()
                    .any(|v| v.to_lowercase().contains(&q));
                name.contains(&q)
                    || parent.contains(&q)
                    || transcript.contains(&q)
                    || meta_text.to_lowercase().contains(&q)
                    || external_hit
            })
            .map(|item| item.id)
            .collect();
        }
        self.original_files = self.files.clone();
        // restore selected index
        self.selected = selected_idx.and_then(|idx| self.files.iter().position(|&x| x == idx));
        self.search_dirty = false;
        self.search_deadline = None;
    }

    pub(super) fn apply_sort(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let selected_idx = self.selected.and_then(|i| self.files.get(i).copied());
        let key = self.sort_key;
        let dir = self.sort_dir;
        if dir == SortDir::None {
            self.files = self.original_files.clone();
        } else {
            let items = &self.items;
            let item_index = &self.item_index;
            let lufs_override = &self.lufs_override;
            let external_cols = &self.external_visible_columns;
            self.files.sort_by(|a, b| {
                use std::cmp::Ordering;
                let pa_idx = match item_index.get(a) {
                    Some(idx) => *idx,
                    None => return Ordering::Equal,
                };
                let pb_idx = match item_index.get(b) {
                    Some(idx) => *idx,
                    None => return Ordering::Equal,
                };
                let pa_item = &items[pa_idx];
                let pb_item = &items[pb_idx];
                let pa = &pa_item.path;
                let pb = &pb_item.path;
                let ma = pa_item.meta.as_ref();
                let mb = pb_item.meta.as_ref();
                let ord = match key {
                    SortKey::File => {
                        let sa = pa.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        let sb = pb.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        sa.cmp(sb)
                    }
                    SortKey::Folder => {
                        let sa = pa.parent().and_then(|p| p.to_str()).unwrap_or("");
                        let sb = pb.parent().and_then(|p| p.to_str()).unwrap_or("");
                        sa.cmp(sb)
                    }
                    SortKey::Transcript => {
                        let sa = pa_item
                            .transcript
                            .as_ref()
                            .map(|t| t.full_text.as_str())
                            .unwrap_or("");
                        let sb = pb_item
                            .transcript
                            .as_ref()
                            .map(|t| t.full_text.as_str())
                            .unwrap_or("");
                        sa.cmp(sb)
                    }
                    SortKey::Length => num_order(
                        ma.and_then(|m| m.duration_secs)
                            .unwrap_or(0.0),
                        mb.and_then(|m| m.duration_secs)
                            .unwrap_or(0.0),
                    ),
                    SortKey::Channels => num_order(
                        ma.map(|m| m.channels as f32).unwrap_or(0.0),
                        mb.map(|m| m.channels as f32).unwrap_or(0.0),
                    ),
                    SortKey::SampleRate => num_order(
                        ma.map(|m| m.sample_rate as f32).unwrap_or(0.0),
                        mb.map(|m| m.sample_rate as f32).unwrap_or(0.0),
                    ),
                    SortKey::Bits => num_order(
                        ma.map(|m| m.bits_per_sample as f32).unwrap_or(0.0),
                        mb.map(|m| m.bits_per_sample as f32).unwrap_or(0.0),
                    ),
                    SortKey::Level => num_order(
                        ma.and_then(|m| m.peak_db).unwrap_or(f32::NEG_INFINITY),
                        mb.and_then(|m| m.peak_db).unwrap_or(f32::NEG_INFINITY),
                    ),
                    // LUFS sorting uses effective value: override if present, else base + gain
                    SortKey::Lufs => {
                        let ga = pa_item.pending_gain_db;
                        let gb = pb_item.pending_gain_db;
                        let va = if let Some(v) = lufs_override.get(pa) {
                            *v
                        } else {
                            ma.and_then(|m| m.lufs_i.map(|x| x + ga))
                                .unwrap_or(f32::NEG_INFINITY)
                        };
                        let vb = if let Some(v) = lufs_override.get(pb) {
                            *v
                        } else {
                            mb.and_then(|m| m.lufs_i.map(|x| x + gb))
                                .unwrap_or(f32::NEG_INFINITY)
                        };
                        num_order(va, vb)
                    }
                    SortKey::External(idx) => {
                        let Some(col) = external_cols.get(idx) else {
                            return Ordering::Equal;
                        };
                        let sa = pa_item.external.get(col).map(|v| v.as_str()).unwrap_or("");
                        let sb = pb_item.external.get(col).map(|v| v.as_str()).unwrap_or("");
                        sa.cmp(sb)
                    }
                };
                match dir {
                    SortDir::Asc => ord,
                    SortDir::Desc => ord.reverse(),
                    SortDir::None => Ordering::Equal,
                }
            });
        }

        // restore selection to the same path if possible
        self.selected = selected_idx.and_then(|idx| self.files.iter().position(|&x| x == idx));
    }

    pub(super) fn current_path_for_rebuild(&self) -> Option<PathBuf> {
        if let Some(i) = self.active_tab {
            return self.tabs.get(i).map(|t| t.path.clone());
        }
        if let Some(i) = self.selected {
            return self.path_for_row(i).cloned();
        }
        None
    }

    pub(super) fn rebuild_current_buffer_with_mode(&mut self) {
        if let Some(tab_idx) = self.active_tab {
            if let Some(tab) = self.tabs.get(tab_idx) {
                if tab.dirty {
                    let path = tab.path.clone();
                    if self.apply_dirty_tab_audio_with_mode(&path) {
                        return;
                    }
                }
            }
        } else if let Some(sel) = self.selected {
            if let Some(path) = self.path_for_row(sel).cloned() {
                if self.apply_dirty_tab_preview_for_list(&path) {
                    return;
                }
            }
        }
        if let Some(p) = self.current_path_for_rebuild() {
            match self.mode {
                RateMode::Speed => {
                    let _ = crate::wave::prepare_for_speed(
                        &p,
                        &self.audio,
                        &mut Vec::new(),
                        self.playback_rate,
                    );
                    self.audio.set_rate(self.playback_rate);
                }
                _ => {
                    if self.is_decode_failed_path(&p) {
                        let _ = crate::wave::prepare_for_speed(&p, &self.audio, &mut Vec::new(), 1.0);
                        self.audio.set_rate(1.0);
                    } else {
                        self.audio.set_rate(1.0);
                        self.spawn_heavy_processing(&p);
                    }
                }
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
                    RateMode::PitchShift => {
                        crate::wave::process_pitchshift_offline(&mono, in_sr, out_sr, sem)
                    }
                    RateMode::TimeStretch => {
                        crate::wave::process_timestretch_offline(&mono, in_sr, out_sr, rate)
                    }
                    RateMode::Speed => mono, // not used
                };
                let channels = match mode {
                    RateMode::PitchShift | RateMode::TimeStretch => {
                        if let Ok((chs, multi_sr)) = crate::wave::decode_wav_multi(&path_for_thread) {
                            let mut processed = Vec::with_capacity(chs.len());
                            for ch in chs {
                                let out = match mode {
                                    RateMode::PitchShift => {
                                        crate::wave::process_pitchshift_offline(&ch, multi_sr, out_sr, sem)
                                    }
                                    RateMode::TimeStretch => {
                                        crate::wave::process_timestretch_offline(&ch, multi_sr, out_sr, rate)
                                    }
                                    RateMode::Speed => ch,
                                };
                                processed.push(out);
                            }
                            processed
                        } else {
                            Vec::new()
                        }
                    }
                    RateMode::Speed => Vec::new(),
                };
                let mut waveform = Vec::new();
                crate::wave::build_minmax(&mut waveform, &samples, 2048);
                let _ = tx.send(ProcessingResult {
                    path: path_for_thread.clone(),
                    samples,
                    waveform,
                    channels,
                });
            }
        });
        self.processing = Some(ProcessingState {
            msg: match mode {
                RateMode::PitchShift => "Pitch-shifting...".to_string(),
                RateMode::TimeStretch => "Time-stretching...".to_string(),
                RateMode::Speed => "Processing...".to_string(),
            },
            path: path_buf,
            autoplay_when_ready: false,
            rx,
        });
    }

    pub(super) fn spawn_scan_worker(&self, root: PathBuf, skip_dotfiles: bool) -> std::sync::mpsc::Receiver<ScanMessage> {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut batch: Vec<PathBuf> = Vec::with_capacity(512);
            for entry in WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| !skip_dotfiles || !Self::is_dotfile_path(e.path()))
            {
                if let Ok(e) = entry {
                    if e.file_type().is_file() {
                        if let Some(ext) = e.path().extension().and_then(|s| s.to_str()) {
                            if audio_io::is_supported_extension(ext) {
                                if skip_dotfiles && Self::is_dotfile_path(e.path()) {
                                    continue;
                                }
                                batch.push(e.into_path());
                                if batch.len() >= 512 {
                                    if tx.send(ScanMessage::Batch(std::mem::take(&mut batch))).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !batch.is_empty() {
                let _ = tx.send(ScanMessage::Batch(batch));
            }
            let _ = tx.send(ScanMessage::Done);
        });
        rx
    }
}
