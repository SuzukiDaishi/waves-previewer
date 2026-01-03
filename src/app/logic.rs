use std::collections::HashSet;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use super::helpers::num_order;
use super::types::{EditorTab, ListPreviewResult, ProcessingResult, ProcessingState, RateMode, SortDir, SortKey, ScanMessage};

const LIST_PREVIEW_PREFIX_SECS: f32 = 1.0;

impl super::WavesPreviewer {
    /// Helper: read WAV `smpl` loop markers and map to given output SR, set tab.loop_region if valid
    fn set_loop_region_from_wav_markers(tab: &mut EditorTab, path: &Path, in_sr: u32, out_sr: u32) {
        if let Some((ls, le)) = crate::wave::read_wav_loop_markers(path) {
            if let Some((s, e)) =
                crate::wave::map_wav_loop_markers(ls, le, in_sr, out_sr, tab.samples_len)
            {
                tab.loop_region = Some((s, e));
            }
        }
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
        // record as current playing target
        self.playing_path = Some(p_owned.clone());
        // stop looping for list preview
        self.audio.set_loop_enabled(false);
        // cancel any previous list preview job
        self.list_preview_rx = None;
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
                eprintln!("load error: {e:?}");
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
    pub fn rescan(&mut self) {
        self.files.clear();
        self.all_files.clear();
        self.original_files.clear();
        self.meta.clear();
        self.meta_inflight.clear();
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
        // 繧ｿ繝悶ｒ髢九￥/繧｢繧ｯ繝・ぅ繝門喧縺吶ｋ譎ゅ↓髻ｳ螢ｰ繧貞●豁｢
        self.audio.stop();

        if let Some(idx) = self.tabs.iter().position(|t| t.path.as_path() == path) {
            self.active_tab = Some(idx);
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
                // Load loop markers from WAV (smpl) if available into loop_region
                if let Some(tab) = self.tabs.last_mut() {
                    Self::set_loop_region_from_wav_markers(
                        tab,
                        path,
                        in_sr,
                        self.audio.shared.out_sample_rate,
                    );
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
                // Load loop markers into loop_region if present
                if let Some(tab) = self.tabs.last_mut() {
                    Self::set_loop_region_from_wav_markers(
                        tab,
                        path,
                        in_sr,
                        self.audio.shared.out_sample_rate,
                    );
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
                            if existing.insert(p.clone()) {
                                self.all_files.push(p);
                                added += 1;
                            }
                        }
                    }
                }
            }
        }
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
                        if existing.insert(p.clone()) {
                            self.all_files.push(p.clone());
                            added += 1;
                        }
                    }
                }
            } else if p.is_dir() {
                added += self.add_folder_merge(p.as_path());
            }
        }
        added
    }

    pub(super) fn after_add_refresh(&mut self) {
        self.apply_filter_from_search();
        self.apply_sort();
        self.ensure_meta_pool();
    }

    // Replace current list with explicit files (WAV only). Root is cleared.
    pub(super) fn replace_with_files(&mut self, paths: &[PathBuf]) {
        self.root = None;
        self.files.clear();
        self.all_files.clear();
        self.original_files.clear();
        self.meta.clear();
        self.meta_inflight.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.scan_rx = None;
        self.scan_in_progress = false;
        let mut set: HashSet<PathBuf> = HashSet::new();
        for p in paths {
            if p.is_file() {
                if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    if ext.eq_ignore_ascii_case("wav") {
                        if set.insert(p.clone()) {
                            self.all_files.push(p.clone());
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
        if self.search_query.trim().is_empty() {
            self.files = (0..self.all_files.len()).collect();
        } else {
            let q = self.search_query.to_lowercase();
            self.files = self
                .all_files
                .iter()
                .enumerate()
                .filter(|(_, p)| {
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
                    name.contains(&q) || parent.contains(&q)
                })
                .map(|(idx, _)| idx)
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
            self.files.sort_by(|a, b| {
                use std::cmp::Ordering;
                let pa = &self.all_files[*a];
                let pb = &self.all_files[*b];
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
                    SortKey::Length => num_order(
                        self.meta
                            .get(pa)
                            .and_then(|m| m.duration_secs)
                            .unwrap_or(0.0),
                        self.meta
                            .get(pb)
                            .and_then(|m| m.duration_secs)
                            .unwrap_or(0.0),
                    ),
                    SortKey::Channels => num_order(
                        self.meta.get(pa).map(|m| m.channels as f32).unwrap_or(0.0),
                        self.meta.get(pb).map(|m| m.channels as f32).unwrap_or(0.0),
                    ),
                    SortKey::SampleRate => num_order(
                        self.meta
                            .get(pa)
                            .map(|m| m.sample_rate as f32)
                            .unwrap_or(0.0),
                        self.meta
                            .get(pb)
                            .map(|m| m.sample_rate as f32)
                            .unwrap_or(0.0),
                    ),
                    SortKey::Bits => num_order(
                        self.meta
                            .get(pa)
                            .map(|m| m.bits_per_sample as f32)
                            .unwrap_or(0.0),
                        self.meta
                            .get(pb)
                            .map(|m| m.bits_per_sample as f32)
                            .unwrap_or(0.0),
                    ),
                    SortKey::Level => num_order(
                        self.meta
                            .get(pa)
                            .and_then(|m| m.peak_db)
                            .unwrap_or(f32::NEG_INFINITY),
                        self.meta
                            .get(pb)
                            .and_then(|m| m.peak_db)
                            .unwrap_or(f32::NEG_INFINITY),
                    ),
                    // LUFS sorting uses effective value: override if present, else base + gain
                    SortKey::Lufs => {
                        let ga = *self.pending_gains.get(pa).unwrap_or(&0.0);
                        let gb = *self.pending_gains.get(pb).unwrap_or(&0.0);
                        let va = if let Some(v) = self.lufs_override.get(pa) {
                            *v
                        } else {
                            self.meta
                                .get(pa)
                                .and_then(|m| m.lufs_i.map(|x| x + ga))
                                .unwrap_or(f32::NEG_INFINITY)
                        };
                        let vb = if let Some(v) = self.lufs_override.get(pb) {
                            *v
                        } else {
                            self.meta
                                .get(pb)
                                .and_then(|m| m.lufs_i.map(|x| x + gb))
                                .unwrap_or(f32::NEG_INFINITY)
                        };
                        num_order(va, vb)
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
                    self.audio.set_rate(1.0);
                    self.spawn_heavy_processing(&p);
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
            rx,
        });
    }

    pub(super) fn spawn_scan_worker(&self, root: PathBuf) -> std::sync::mpsc::Receiver<ScanMessage> {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut batch: Vec<PathBuf> = Vec::with_capacity(512);
            for entry in WalkDir::new(root).follow_links(false) {
                if let Ok(e) = entry {
                    if e.file_type().is_file() {
                        if let Some(ext) = e.path().extension().and_then(|s| s.to_str()) {
                            if ext.eq_ignore_ascii_case("wav") {
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
