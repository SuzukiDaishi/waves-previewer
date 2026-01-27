use crate::audio_io;
use crate::loop_markers;
use crate::wave::prepare_for_speed;
use regex::RegexBuilder;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use super::helpers::num_order;
use super::types::{
    ChannelView, EditorDecodeResult, EditorDecodeState, EditorTab, ListPreviewResult,
    ProcessingResult, ProcessingState, RateMode, ScanMessage, SortDir, SortKey,
};

const LIST_PREVIEW_PREFIX_SECS: f32 = 1.0;
const EDITOR_PREVIEW_PREFIX_SECS: f32 = 8.0;

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

    pub(super) fn should_skip_path(&self, path: &Path) -> bool {
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
                    loop_region_committed: tab.loop_region_committed,
                    loop_region_applied: tab.loop_region_applied,
                    loop_markers_saved: tab.loop_markers_saved,
                    loop_markers_dirty: tab.loop_markers_dirty,
                    markers: tab.markers.clone(),
                    markers_committed: tab.markers_committed.clone(),
                    markers_saved: tab.markers_saved.clone(),
                    markers_applied: tab.markers_applied.clone(),
                    markers_dirty: tab.markers_dirty,
                    trim_range: tab.trim_range,
                    loop_xfade_samples: tab.loop_xfade_samples,
                    loop_xfade_shape: tab.loop_xfade_shape,
                    fade_in_range: tab.fade_in_range,
                    fade_out_range: tab.fade_out_range,
                    fade_in_shape: tab.fade_in_shape,
                    fade_out_shape: tab.fade_out_shape,
                    loop_mode: tab.loop_mode,
                    bpm_enabled: tab.bpm_enabled,
                    bpm_value: tab.bpm_value,
                    bpm_user_set: tab.bpm_user_set,
                    snap_zero_cross: tab.snap_zero_cross,
                    tool_state: tab.tool_state,
                    active_tool: tab.active_tool,
                    show_waveform_overlay: tab.show_waveform_overlay,
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
            .position(|t| {
                (t.dirty || t.loop_markers_dirty || t.markers_dirty) && t.path.as_path() == path
            })
        {
            Some(i) => i,
            None => {
                let channels = {
                    let cached = match self.edited_cache.get(path) {
                        Some(v) => v,
                        None => return false,
                    };
                    cached.ch_samples.clone()
                };
                self.playing_path = Some(path.to_path_buf());
                match self.mode {
                    RateMode::Speed => {
                        self.audio.set_samples_channels(channels);
                        self.audio.stop();
                        self.audio.set_rate(self.playback_rate);
                    }
                    _ => {
                        if decode_failed {
                            self.audio.set_samples_channels(channels);
                            self.audio.stop();
                            self.audio.set_rate(1.0);
                        } else {
                            self.audio.set_rate(1.0);
                            self.spawn_heavy_processing_from_channels(path.to_path_buf(), channels);
                        }
                    }
                }
                self.apply_effective_volume();
                return true;
            }
        };
        let (channels, tab_path) = {
            let tab = &self.tabs[idx];
            (tab.ch_samples.clone(), tab.path.clone())
        };
        self.playing_path = Some(tab_path.clone());
        match self.mode {
            RateMode::Speed => {
                self.audio.set_samples_channels(channels);
                self.audio.stop();
                self.audio.set_rate(self.playback_rate);
            }
            _ => {
                if decode_failed {
                    self.audio.set_samples_channels(channels);
                    self.audio.stop();
                    self.audio.set_rate(1.0);
                } else {
                    self.audio.set_rate(1.0);
                    self.spawn_heavy_processing_from_channels(tab_path.clone(), channels);
                }
            }
        }
        if let Some(tab) = self.tabs.get(idx) {
            self.apply_loop_mode_for_tab(tab);
        }
        self.apply_effective_volume();
        true
    }

    fn reset_tab_from_virtual(&mut self, idx: usize, update_audio: bool) -> bool {
        let path = match self.tabs.get(idx) {
            Some(t) => t.path.clone(),
            None => return false,
        };
        let (display_name, audio) = {
            let Some(item) = self.item_for_path(&path) else {
                return false;
            };
            let Some(audio) = item.virtual_audio.clone() else {
                return false;
            };
            (item.display_name.clone(), audio)
        };
        let samples_len = audio.len();
        let mono = Self::mixdown_channels_mono(&audio.channels, samples_len);
        let mut waveform = Vec::new();
        crate::wave::build_minmax(&mut waveform, &mono, 2048);
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.display_name = display_name;
            tab.waveform_minmax = waveform;
            tab.ch_samples = audio.channels.clone();
            tab.samples_len = samples_len;
            Self::reset_tab_defaults(tab);
        }
        if update_audio {
            self.audio.set_rate(1.0);
            self.audio.set_samples_channels(audio.channels.clone());
            self.apply_effective_volume();
        }
        true
    }

    fn apply_dirty_tab_preview_for_list(&mut self, path: &Path) -> bool {
        let idx = match self
            .tabs
            .iter()
            .position(|t| {
                (t.dirty || t.loop_markers_dirty || t.markers_dirty) && t.path.as_path() == path
            })
        {
            Some(i) => i,
            None => {
                let channels = {
                    let cached = match self.edited_cache.get(path) {
                        Some(v) => v,
                        None => return false,
                    };
                    cached.ch_samples.clone()
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
                self.audio.set_samples_channels(channels);
                self.audio.stop();
                self.apply_effective_volume();
                return true;
            }
        };
        let channels = {
            let tab = &self.tabs[idx];
            tab.ch_samples.clone()
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
        self.audio.set_samples_channels(channels);
        self.audio.stop();
        self.apply_effective_volume();
        true
    }

    pub(super) fn spawn_heavy_processing_from_channels(
        &mut self,
        path: PathBuf,
        channels: Vec<Vec<f32>>,
    ) {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<ProcessingResult>();
        let mode = self.mode;
        let rate = self.playback_rate;
        let sem = self.pitch_semitones;
        let out_sr = self.audio.shared.out_sample_rate;
        let path_for_thread = path.clone();
        std::thread::spawn(move || {
            let mut processed: Vec<Vec<f32>> = Vec::with_capacity(channels.len());
            for chan in channels.iter() {
                let out = match mode {
                    RateMode::PitchShift => {
                        crate::wave::process_pitchshift_offline(chan, out_sr, out_sr, sem)
                    }
                    RateMode::TimeStretch => {
                        crate::wave::process_timestretch_offline(chan, out_sr, out_sr, rate)
                    }
                    RateMode::Speed => chan.clone(),
                };
                processed.push(out);
            }
            let len = processed.get(0).map(|c| c.len()).unwrap_or(0);
            let samples = Self::mixdown_channels_mono(&processed, len);
            let mut waveform = Vec::new();
            crate::wave::build_minmax(&mut waveform, &samples, 2048);
            let _ = tx.send(ProcessingResult {
                path: path_for_thread,
                samples,
                waveform,
                channels: processed,
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
            started_at: std::time::Instant::now(),
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
        tab.markers_committed.clear();
        tab.markers_saved.clear();
        tab.markers_applied.clear();
        tab.markers_dirty = false;
        tab.ab_loop = None;
        tab.loop_region = None;
        tab.loop_region_committed = None;
        tab.loop_region_applied = None;
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
            loop_repeat: 2,
        };
        tab.loop_mode = crate::app::types::LoopMode::Off;
        tab.dragging_marker = None;
        tab.preview_audio_tool = None;
        tab.active_tool_last = None;
        tab.preview_offset_samples = None;
        tab.preview_overlay = None;
        tab.pending_loop_unwrap = None;
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
                        let _ = prepare_for_speed(
                            &path,
                            &self.audio,
                            &mut Vec::new(),
                            self.playback_rate,
                        );
                        self.audio.set_rate(self.playback_rate);
                    }
                } else if update_audio {
                    let _ =
                        prepare_for_speed(&path, &self.audio, &mut Vec::new(), self.playback_rate);
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
        let mut unique_paths: Vec<PathBuf> = Vec::new();
        let mut reload_playing = false;
        let mut affect_playing = false;
        for p in paths {
            if !unique.insert(p.clone()) {
                continue;
            }
            unique_paths.push(p.clone());
        }
        unique_paths.sort();
        unique_paths.dedup();
        let before = self.capture_list_selection_snapshot();
        let before_items = self.capture_list_undo_items_by_paths(&unique_paths);
        for p in &unique_paths {
            self.set_pending_gain_db_for_path(p, 0.0);
            self.lufs_override.remove(p);
            self.lufs_recalc_deadline.remove(p);
            if self.playing_path.as_ref() == Some(p) {
                affect_playing = true;
            }
            self.edited_cache.remove(p);
            if let Some(idx) = self
                .tabs
                .iter()
                .position(|t| t.path.as_path() == p.as_path())
            {
                let update_audio = self.active_tab == Some(idx);
                if self.is_virtual_path(p) {
                    self.reset_tab_from_virtual(idx, update_audio);
                } else {
                    self.reset_tab_from_disk(idx, update_audio);
                }
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
        self.record_list_update_from_paths(&unique_paths, before_items, before);
    }

    /// Helper: read loop markers and map to given output SR, set tab.loop_region if valid
    pub(super) fn set_loop_region_from_file_markers(
        tab: &mut EditorTab,
        path: &Path,
        in_sr: u32,
        out_sr: u32,
    ) {
        let mut saved = None;
        if let Some((ls, le)) = loop_markers::read_loop_markers(path) {
            let ls = (ls.min(u32::MAX as u64)) as u32;
            let le = (le.min(u32::MAX as u64)) as u32;
            if let Some((s, e)) =
                crate::wave::map_loop_markers_between_sr(ls, le, in_sr, out_sr, tab.samples_len)
            {
                tab.loop_region = Some((s, e));
                tab.loop_region_applied = Some((s, e));
                saved = Some((s, e));
            } else {
                tab.loop_region = None;
                tab.loop_region_applied = None;
            }
        } else {
            tab.loop_region = None;
            tab.loop_region_applied = None;
        }
        tab.loop_region_committed = tab.loop_region;
        tab.loop_markers_saved = saved;
        tab.loop_markers_dirty = false;
    }

    pub(super) fn sample_rate_for_path(&self, path: &Path, fallback: u32) -> u32 {
        self.meta_for_path(path)
            .map(|m| m.sample_rate)
            .filter(|&sr| sr > 0)
            .or_else(|| audio_io::read_audio_info(path).ok().map(|i| i.sample_rate))
            .unwrap_or(fallback)
    }

    pub(super) fn load_markers_for_tab(
        tab: &mut EditorTab,
        path: &Path,
        out_sr: u32,
        file_sr: u32,
    ) {
        let out_sr = out_sr.max(1);
        match crate::markers::read_markers(path, out_sr, file_sr) {
            Ok(mut markers) => {
                markers.retain(|m| m.sample <= tab.samples_len);
                tab.markers = markers.clone();
                tab.markers_committed = markers.clone();
                tab.markers_saved = markers;
                tab.markers_applied = tab.markers_committed.clone();
                tab.markers_dirty = false;
            }
            Err(err) => {
                eprintln!("read markers failed {}: {err:?}", path.display());
                tab.markers.clear();
                tab.markers_committed.clear();
                tab.markers_saved.clear();
                tab.markers_applied.clear();
                tab.markers_dirty = false;
            }
        }
    }

    pub(super) fn write_markers_for_tab(&mut self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let path = tab.path.clone();
        if !path.is_file() {
            self.remove_missing_path(&path);
            return false;
        }
        // Non-destructive: keep in memory and defer file writes until Save Selected.
        self.debug_log(format!(
            "markers queued for save (path: {})",
            path.display()
        ));
        true
    }

    pub(super) fn write_loop_markers_for_tab(&mut self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let path = tab.path.clone();
        if !path.is_file() {
            self.remove_missing_path(&path);
            return false;
        }
        // Non-destructive: keep in memory and defer file writes until Save Selected.
        self.debug_log(format!(
            "loop markers queued for save (path: {})",
            path.display()
        ));
        true
    }

    pub(super) fn mark_edit_saved_for_path(&mut self, path: &Path) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.path.as_path() == path) {
            tab.dirty = false;
            tab.markers_saved = tab.markers_committed.clone();
            tab.markers_applied = tab.markers_committed.clone();
            tab.markers_dirty = false;
            tab.loop_markers_saved = tab.loop_region_committed;
            tab.loop_region_applied = tab.loop_region_committed;
            tab.loop_markers_dirty = false;
        }
        self.edited_cache.remove(path);
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
        let Some(item_snapshot) = self.item_for_row(row_idx).cloned() else {
            return;
        };
        let p_owned = item_snapshot.path.clone();
        if item_snapshot.source == crate::app::types::MediaSource::External {
            self.selected = Some(row_idx);
            self.scroll_to_selected = auto_scroll;
            return;
        }
        let is_virtual = item_snapshot.source == crate::app::types::MediaSource::Virtual;
        if !is_virtual && !p_owned.is_file() {
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
        let decode_failed = if is_virtual {
            false
        } else {
            self.is_decode_failed_path(&p_owned)
        };
        // record as current playing target
        self.playing_path = Some(p_owned.clone());
        // stop looping for list preview
        self.audio.set_loop_enabled(false);
        // cancel any previous list preview job
        self.list_preview_rx = None;
        if is_virtual {
            let Some(audio) = item_snapshot.virtual_audio else {
                return;
            };
            let channels = audio.channels.clone();
            if need_heavy {
                self.audio.set_rate(1.0);
                self.audio.stop();
                self.audio.set_samples_mono(Vec::new());
                self.spawn_heavy_processing_from_channels(p_owned.clone(), channels);
                self.apply_effective_volume();
                return;
            }
            let rate = if self.mode == RateMode::Speed {
                self.playback_rate
            } else {
                1.0
            };
            self.audio.set_rate(rate);
            self.audio.set_samples_channels(channels);
            self.audio.stop();
            self.apply_effective_volume();
            return;
        }
        if need_heavy && !decode_failed {
            self.audio.set_rate(1.0);
            self.audio.stop();
            self.audio.set_samples_mono(Vec::new());
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
        match crate::wave::prepare_for_list_preview(&p_owned, &self.audio, LIST_PREVIEW_PREFIX_SECS)
        {
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
                    self.audio.set_samples_mono(Vec::new());
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
                let (mut chans, in_sr) = crate::wave::decode_wav_multi(&path)?;
                for c in chans.iter_mut() {
                    *c = crate::wave::resample_linear(c, in_sr, out_sr);
                }
                Ok(ListPreviewResult {
                    path,
                    channels: chans,
                    job_id,
                })
            })();
            if let Ok(result) = res {
                let _ = tx.send(result);
            }
        });
        self.list_preview_rx = Some(rx);
    }

    fn spawn_editor_decode(&mut self, path: PathBuf) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc};
        self.cancel_editor_decode();
        self.editor_decode_job_id = self.editor_decode_job_id.wrapping_add(1);
        let job_id = self.editor_decode_job_id;
        let out_sr = self.audio.shared.out_sample_rate;
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_thread = cancel.clone();
        let path_for_thread = path.clone();
        let (tx, rx) = mpsc::channel::<EditorDecodeResult>();
        std::thread::spawn(move || {
            let prefix =
                crate::wave::decode_wav_multi_prefix(&path_for_thread, EDITOR_PREVIEW_PREFIX_SECS);
            let (mut chans, in_sr, truncated) = match prefix {
                Ok(v) => v,
                Err(err) => {
                    let _ = tx.send(EditorDecodeResult {
                        path: path_for_thread,
                        channels: Vec::new(),
                        is_final: true,
                        job_id,
                        error: Some(err.to_string()),
                    });
                    return;
                }
            };
            if cancel_thread.load(Ordering::Relaxed) {
                return;
            }
            if in_sr != out_sr {
                for c in chans.iter_mut() {
                    *c = crate::wave::resample_linear(c, in_sr, out_sr);
                }
            }
            let _ = tx.send(EditorDecodeResult {
                path: path_for_thread.clone(),
                channels: chans,
                is_final: !truncated,
                job_id,
                error: None,
            });
            if !truncated || cancel_thread.load(Ordering::Relaxed) {
                return;
            }
            let full = crate::wave::decode_wav_multi(&path_for_thread);
            let (mut chans, in_sr) = match full {
                Ok(v) => v,
                Err(err) => {
                    let _ = tx.send(EditorDecodeResult {
                        path: path_for_thread,
                        channels: Vec::new(),
                        is_final: true,
                        job_id,
                        error: Some(err.to_string()),
                    });
                    return;
                }
            };
            if cancel_thread.load(Ordering::Relaxed) {
                return;
            }
            if in_sr != out_sr {
                for c in chans.iter_mut() {
                    *c = crate::wave::resample_linear(c, in_sr, out_sr);
                }
            }
            if cancel_thread.load(Ordering::Relaxed) {
                return;
            }
            let _ = tx.send(EditorDecodeResult {
                path: path_for_thread,
                channels: chans,
                is_final: true,
                job_id,
                error: None,
            });
        });
        self.editor_decode_state = Some(EditorDecodeState {
            path,
            started_at: std::time::Instant::now(),
            rx,
            cancel,
            job_id,
            partial_ready: false,
        });
    }

    pub(super) fn drain_editor_decode(&mut self) {
        let mut decode_update_tab: Option<usize> = None;
        let mut decode_refresh_preview: Option<usize> = None;
        let mut decode_cancel_preview = false;
        let mut decode_error: Option<(PathBuf, String)> = None;
        let mut decode_done = false;
        let mut marker_updates: Vec<(usize, PathBuf)> = Vec::new();
        if let Some(state) = &mut self.editor_decode_state {
            while let Ok(res) = state.rx.try_recv() {
                if res.job_id != state.job_id {
                    continue;
                }
                if let Some(err) = res.error {
                    decode_error = Some((res.path.clone(), err));
                    if let Some(idx) = self.tabs.iter().position(|t| t.path == res.path) {
                        if let Some(tab) = self.tabs.get_mut(idx) {
                            tab.loading = false;
                        }
                    }
                    decode_done = true;
                    continue;
                }
                if let Some(idx) = self.tabs.iter().position(|t| t.path == res.path) {
                    if let Some(tab) = self.tabs.get_mut(idx) {
                        let had_preview =
                            tab.preview_audio_tool.is_some() || tab.preview_overlay.is_some();
                        tab.preview_audio_tool = None;
                        tab.preview_overlay = None;
                        let old_len = tab.samples_len;
                        let old_view = tab.view_offset;
                        let old_spp = tab.samples_per_px;
                        tab.ch_samples = res.channels;
                        tab.samples_len = tab.ch_samples.get(0).map(|c| c.len()).unwrap_or(0);
                        if res.is_final {
                            marker_updates.push((idx, res.path.clone()));
                        }
                        if tab.samples_len == 0 {
                            tab.samples_per_px = 0.0;
                            tab.view_offset = 0;
                        } else if old_len > 0 && tab.samples_len != old_len {
                            let ratio = tab.samples_len as f32 / old_len as f32;
                            if old_spp > 0.0 {
                                tab.samples_per_px = (old_spp * ratio).max(0.0001);
                            } else {
                                tab.samples_per_px = 0.0;
                            }
                            tab.view_offset = ((old_view as f32) * ratio).round() as usize;
                            tab.loop_xfade_samples =
                                ((tab.loop_xfade_samples as f32) * ratio).round() as usize;
                        } else if old_spp <= 0.0 {
                            tab.samples_per_px = 0.0;
                        }
                        tab.loading = !res.is_final;
                        decode_update_tab = Some(idx);
                        decode_refresh_preview = Some(idx);
                        if had_preview && self.active_tab == Some(idx) {
                            decode_cancel_preview = true;
                        }
                    }
                }
                if res.is_final {
                    decode_done = true;
                } else {
                    state.partial_ready = true;
                }
            }
        }
        if let Some((path, err)) = decode_error {
            self.debug_log(format!("editor decode failed: {} ({err})", path.display()));
        }
        if decode_cancel_preview {
            self.cancel_heavy_preview();
        }
        if !marker_updates.is_empty() {
            let out_sr = self.audio.shared.out_sample_rate;
            for (idx, path) in marker_updates {
                let file_sr = self.sample_rate_for_path(&path, out_sr);
                if let Some(tab) = self.tabs.get_mut(idx) {
                    Self::set_loop_region_from_file_markers(tab, &path, file_sr, out_sr);
                    Self::load_markers_for_tab(tab, &path, out_sr, file_sr);
                }
            }
        }
        if let Some(idx) = decode_update_tab {
            if self.active_tab == Some(idx) {
                if let Some(tab) = self.tabs.get(idx) {
                    if !tab.ch_samples.is_empty() {
                        let buf = crate::audio::AudioBuffer::from_channels(tab.ch_samples.clone());
                        let playing = self
                            .audio
                            .shared
                            .playing
                            .load(std::sync::atomic::Ordering::Relaxed);
                        if playing {
                            self.audio
                                .replace_samples_keep_pos(std::sync::Arc::new(buf));
                        } else {
                            self.audio.set_samples_channels(tab.ch_samples.clone());
                        }
                        self.apply_loop_mode_for_tab(tab);
                    }
                }
            }
        }
        if let Some(idx) = decode_refresh_preview {
            if self.active_tab == Some(idx) {
                self.refresh_tool_preview_for_tab(idx);
            }
        }
        if decode_done {
            self.editor_decode_state = None;
        }
    }

    pub(super) fn remove_missing_path(&mut self, path: &Path) {
        if self.is_virtual_path(path) {
            return;
        }
        if path.exists() {
            return;
        }
        let Some(id) = self.path_index.get(path).copied() else {
            return;
        };
        let selected_path = self.selected_path_buf();
        let selected_row_before = self.selected;
        let selected_removed = selected_path
            .as_ref()
            .map(|p| p.as_path() == path)
            .unwrap_or(false);
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
        self.purge_spectro_cache_entry(&path_buf);
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
        } else if self.selected.is_none() && selected_removed {
            let len = self.files.len();
            let target = selected_row_before
                .unwrap_or(0)
                .saturating_sub(1)
                .min(len.saturating_sub(1));
            self.selected = Some(target);
            self.selected_multi.clear();
            self.selected_multi.insert(target);
            self.select_anchor = Some(target);
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
        let selected_row_before = self.selected;
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
        let selected_removed = selected_path
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
            self.purge_spectro_cache_entry(path);
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
        } else if self.selected.is_none() && selected_removed {
            let len = self.files.len();
            let target = selected_row_before
                .unwrap_or(0)
                .saturating_sub(1)
                .min(len.saturating_sub(1));
            self.selected = Some(target);
            self.selected_multi.clear();
            self.selected_multi.insert(target);
            self.select_anchor = Some(target);
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
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
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
        if let Some(item) = self.item_for_path(path) {
            if item.source == crate::app::types::MediaSource::External {
                return;
            }
        }
        if self.is_virtual_path(path) {
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
                let name = self
                    .item_for_path(path)
                    .map(|item| item.display_name.clone())
                    .unwrap_or_else(|| "(virtual)".to_string());
                self.tabs.push(EditorTab {
                    path: path.to_path_buf(),
                    display_name: name,
                    waveform_minmax: cached.waveform_minmax,
                    loop_enabled: false,
                    loading: false,
                    ch_samples: cached.ch_samples,
                    samples_len: cached.samples_len,
                    view_offset: 0,
                    samples_per_px: 0.0,
                    last_wave_w: 0.0,
                    dirty: cached.dirty,
                    ops: Vec::new(),
                    selection: None,
                    markers: cached.markers,
                    markers_committed: cached.markers_committed,
                    markers_saved: cached.markers_saved,
                    markers_applied: cached.markers_applied,
                    markers_dirty: cached.markers_dirty,
                    ab_loop: None,
                    loop_region: cached.loop_region,
                    loop_region_committed: cached.loop_region_committed,
                    loop_region_applied: cached.loop_region_applied,
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
                    show_waveform_overlay: cached.show_waveform_overlay,
                    channel_view: ChannelView::mixdown(),
                    bpm_enabled: cached.bpm_enabled,
                    bpm_value: cached.bpm_value,
                    bpm_user_set: cached.bpm_user_set,
                    seek_hold: None,
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
                    pending_loop_unwrap: None,
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
            let Some(item) = self.item_for_path(path) else {
                return;
            };
            let Some(audio) = item.virtual_audio.clone() else {
                return;
            };
            let name = item.display_name.clone();
            let chs = audio.channels.clone();
            let samples_len = chs.get(0).map(|c| c.len()).unwrap_or(0);
            let default_bpm = self
                .meta_for_path(path)
                .and_then(|m| m.bpm)
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(0.0);
            let mut wf = Vec::new();
            if self.mode == RateMode::Speed {
                let mono = Self::mixdown_channels_mono(&chs, samples_len);
                crate::wave::build_minmax(&mut wf, &mono, 2048);
            }
            self.tabs.push(EditorTab {
                path: path.to_path_buf(),
                display_name: name,
                waveform_minmax: wf,
                loop_enabled: false,
                loading: false,
                ch_samples: chs.clone(),
                samples_len,
                view_offset: 0,
                samples_per_px: 0.0,
                last_wave_w: 0.0,
                dirty: false,
                ops: Vec::new(),
                selection: None,
                markers: Vec::new(),
                markers_committed: Vec::new(),
                markers_saved: Vec::new(),
                markers_applied: Vec::new(),
                markers_dirty: false,
                ab_loop: None,
                loop_region: None,
                loop_region_committed: None,
                loop_region_applied: None,
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
                show_waveform_overlay: true,
                channel_view: ChannelView::mixdown(),
                bpm_enabled: false,
                bpm_value: default_bpm,
                bpm_user_set: false,
                seek_hold: None,
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
                    loop_repeat: 2,
                },
                loop_mode: crate::app::types::LoopMode::Off,
                dragging_marker: None,
                preview_audio_tool: None,
                active_tool_last: None,
                preview_offset_samples: None,
                preview_overlay: None,
                pending_loop_unwrap: None,
                undo_stack: Vec::new(),
                undo_bytes: 0,
                redo_stack: Vec::new(),
                redo_bytes: 0,
            });
            self.active_tab = Some(self.tabs.len() - 1);
            self.playing_path = Some(path.to_path_buf());
            match self.mode {
                RateMode::Speed => {
                    self.audio.set_rate(self.playback_rate);
                    self.audio.set_samples_channels(chs);
                }
                _ => {
                    self.audio.set_rate(1.0);
                    self.spawn_heavy_processing_from_channels(path.to_path_buf(), chs);
                }
            }
            self.apply_effective_volume();
            return;
        }
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
                loading: false,
                ch_samples: cached.ch_samples,
                samples_len: cached.samples_len,
                view_offset: 0,
                samples_per_px: 0.0,
                last_wave_w: 0.0,
                dirty: cached.dirty,
                ops: Vec::new(),
                selection: None,
                markers: cached.markers,
                markers_committed: cached.markers_committed,
                markers_saved: cached.markers_saved,
                markers_applied: cached.markers_applied,
                markers_dirty: cached.markers_dirty,
                ab_loop: None,
                loop_region: cached.loop_region,
                loop_region_committed: cached.loop_region_committed,
                loop_region_applied: cached.loop_region_applied,
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
                show_waveform_overlay: cached.show_waveform_overlay,
                channel_view: ChannelView::mixdown(),
                bpm_enabled: cached.bpm_enabled,
                bpm_value: cached.bpm_value,
                bpm_user_set: cached.bpm_user_set,
                seek_hold: None,
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
                pending_loop_unwrap: None,
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
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string();
        let loading = !decode_failed;
        let default_bpm = self
            .meta_for_path(path)
            .and_then(|m| m.bpm)
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(0.0);
        self.tabs.push(EditorTab {
            path: path.to_path_buf(),
            display_name: name,
            waveform_minmax: Vec::new(),
            loop_enabled: false,
            loading,
            ch_samples: Vec::new(),
            samples_len: 0,
            view_offset: 0,
            samples_per_px: 0.0,
            last_wave_w: 0.0,
            dirty: false,
            ops: Vec::new(),
            selection: None,
            markers: Vec::new(),
            markers_committed: Vec::new(),
            markers_saved: Vec::new(),
            markers_applied: Vec::new(),
            markers_dirty: false,
            ab_loop: None,
            loop_region: None,
            loop_region_committed: None,
            loop_region_applied: None,
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
            show_waveform_overlay: true,
            channel_view: ChannelView::mixdown(),
            bpm_enabled: false,
            bpm_value: default_bpm,
            bpm_user_set: false,
            seek_hold: None,
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
                loop_repeat: 2,
            },
            loop_mode: crate::app::types::LoopMode::Off,
            dragging_marker: None,
            preview_audio_tool: None,
            active_tool_last: None,
            preview_offset_samples: None,
            preview_overlay: None,
            pending_loop_unwrap: None,
            undo_stack: Vec::new(),
            undo_bytes: 0,
            redo_stack: Vec::new(),
            redo_bytes: 0,
        });
        self.active_tab = Some(self.tabs.len() - 1);
        self.playing_path = Some(path.to_path_buf());
        match self.mode {
            RateMode::Speed => self.audio.set_rate(self.playback_rate),
            _ => self.audio.set_rate(1.0),
        }
        self.audio.set_samples_channels(Vec::new());
        self.audio.stop();
        self.apply_effective_volume();
        if !decode_failed {
            self.spawn_editor_decode(path.to_path_buf());
        }
    }

    pub(super) fn open_paths_in_tabs(&mut self, paths: &[PathBuf]) {
        for path in paths {
            if let Some(item) = self.item_for_path(path) {
                if item.source == crate::app::types::MediaSource::External {
                    continue;
                }
            }
            self.open_or_activate_tab(path);
        }
    }

    pub(super) fn apply_filter_from_search(&mut self) {
        // Preserve selection index if possible
        let selected_idx = self.selected.and_then(|i| self.files.get(i).copied());
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            self.files = self.items.iter().map(|item| item.id).collect();
        } else if self.search_use_regex {
            let re = RegexBuilder::new(&query).case_insensitive(true).build();
            if let Ok(re) = re {
                self.files = self
                    .items
                    .iter()
                    .filter(|item| {
                        let name = item.display_name.as_str();
                        let parent = item.display_folder.as_str();
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
                                    "sr:{} bits:{} br:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1} bpm:{:.1}",
                                    m.sample_rate,
                                    m.bits_per_sample,
                                    m.bit_rate_bps.unwrap_or(0),
                                    m.channels,
                                    m.duration_secs.unwrap_or(0.0),
                                    m.peak_db.unwrap_or(0.0),
                                    m.lufs_i.unwrap_or(0.0),
                                    m.bpm.unwrap_or(0.0)
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
                        let name = item.display_name.to_lowercase();
                        let parent = item.display_folder.to_lowercase();
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
                                    "sr:{} bits:{} br:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1} bpm:{:.1}",
                                    m.sample_rate,
                                    m.bits_per_sample,
                                    m.bit_rate_bps.unwrap_or(0),
                                    m.channels,
                                    m.duration_secs.unwrap_or(0.0),
                                    m.peak_db.unwrap_or(0.0),
                                    m.lufs_i.unwrap_or(0.0),
                                    m.bpm.unwrap_or(0.0)
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
                    let name = item.display_name.to_lowercase();
                    let parent = item.display_folder.to_lowercase();
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
                                "sr:{} bits:{} br:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1} bpm:{:.1}",
                                m.sample_rate,
                                m.bits_per_sample,
                                m.bit_rate_bps.unwrap_or(0),
                                m.channels,
                                m.duration_secs.unwrap_or(0.0),
                                m.peak_db.unwrap_or(0.0),
                                m.lufs_i.unwrap_or(0.0),
                                m.bpm.unwrap_or(0.0)
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
            use std::time::UNIX_EPOCH;
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
                let ma = pa_item.meta.as_ref();
                let mb = pb_item.meta.as_ref();
                let ord = match key {
                    SortKey::File => pa_item.display_name.cmp(&pb_item.display_name),
                    SortKey::Folder => pa_item.display_folder.cmp(&pb_item.display_folder),
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
                        ma.and_then(|m| m.duration_secs).unwrap_or(0.0),
                        mb.and_then(|m| m.duration_secs).unwrap_or(0.0),
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
                    SortKey::BitRate => num_order(
                        ma.and_then(|m| m.bit_rate_bps)
                            .map(|v| v as f32)
                            .unwrap_or(0.0),
                        mb.and_then(|m| m.bit_rate_bps)
                            .map(|v| v as f32)
                            .unwrap_or(0.0),
                    ),
                    SortKey::Level => num_order(
                        ma.and_then(|m| m.peak_db).unwrap_or(f32::NEG_INFINITY),
                        mb.and_then(|m| m.peak_db).unwrap_or(f32::NEG_INFINITY),
                    ),
                    // LUFS sorting uses effective value: override if present, else base + gain
                    SortKey::Lufs => {
                        let ga = pa_item.pending_gain_db;
                        let gb = pb_item.pending_gain_db;
                        let va = if let Some(v) = lufs_override.get(&pa_item.path) {
                            *v
                        } else {
                            ma.and_then(|m| m.lufs_i.map(|x| x + ga))
                                .unwrap_or(f32::NEG_INFINITY)
                        };
                        let vb = if let Some(v) = lufs_override.get(&pb_item.path) {
                            *v
                        } else {
                            mb.and_then(|m| m.lufs_i.map(|x| x + gb))
                                .unwrap_or(f32::NEG_INFINITY)
                        };
                        num_order(va, vb)
                    }
                    SortKey::Bpm => num_order(
                        ma.and_then(|m| m.bpm).unwrap_or(0.0),
                        mb.and_then(|m| m.bpm).unwrap_or(0.0),
                    ),
                    SortKey::CreatedAt => num_order(
                        ma.and_then(|m| m.created_at)
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64() as f32)
                            .unwrap_or(0.0),
                        mb.and_then(|m| m.created_at)
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64() as f32)
                            .unwrap_or(0.0),
                    ),
                    SortKey::ModifiedAt => num_order(
                        ma.and_then(|m| m.modified_at)
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64() as f32)
                            .unwrap_or(0.0),
                        mb.and_then(|m| m.modified_at)
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64() as f32)
                            .unwrap_or(0.0),
                    ),
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
            if let Some(tab) = self.tabs.get(tab_idx) {
                if tab.loading {
                    return;
                }
                if !tab.ch_samples.is_empty() {
                    let channels = tab.ch_samples.clone();
                    match self.mode {
                        RateMode::Speed => {
                            self.audio.set_samples_channels(channels);
                            self.audio.set_rate(self.playback_rate);
                        }
                        _ => {
                            self.audio.set_rate(1.0);
                            self.spawn_heavy_processing_from_channels(tab.path.clone(), channels);
                        }
                    }
                    self.apply_effective_volume();
                    return;
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
            if self.is_virtual_path(&p) {
                let Some(audio) = self.edited_audio_for_path(&p) else {
                    return;
                };
                let channels = audio.channels.clone();
                match self.mode {
                    RateMode::Speed => {
                        self.audio.set_samples_channels(channels);
                        self.audio.set_rate(self.playback_rate);
                    }
                    _ => {
                        self.audio.set_rate(1.0);
                        self.spawn_heavy_processing_from_channels(p, channels);
                    }
                }
                self.apply_effective_volume();
                return;
            }
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
                        let _ =
                            crate::wave::prepare_for_speed(&p, &self.audio, &mut Vec::new(), 1.0);
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
                        if let Ok((chs, multi_sr)) = crate::wave::decode_wav_multi(&path_for_thread)
                        {
                            let mut processed = Vec::with_capacity(chs.len());
                            for ch in chs {
                                let out = match mode {
                                    RateMode::PitchShift => {
                                        crate::wave::process_pitchshift_offline(
                                            &ch, multi_sr, out_sr, sem,
                                        )
                                    }
                                    RateMode::TimeStretch => {
                                        crate::wave::process_timestretch_offline(
                                            &ch, multi_sr, out_sr, rate,
                                        )
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
            started_at: std::time::Instant::now(),
            rx,
        });
    }

    pub(super) fn spawn_scan_worker(
        &self,
        root: PathBuf,
        skip_dotfiles: bool,
    ) -> std::sync::mpsc::Receiver<ScanMessage> {
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
                                    if tx
                                        .send(ScanMessage::Batch(std::mem::take(&mut batch)))
                                        .is_err()
                                    {
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
