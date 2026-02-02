use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::audio::AudioEngine;
use crate::mcp;
use crate::ipc;
use crate::wave::{build_minmax, prepare_for_speed};
use anyhow::Result;
use egui::{Align, Color32, Key, RichText, Sense, TextStyle};
use egui_extras::TableBuilder;
// use walkdir::WalkDir; // unused here (used in logic.rs)

mod capture;
mod audio_ops;
mod clipboard_ops;
mod debug_ops;
mod dialogs;
mod editor_ops;
mod export_ops;
mod external_load_ops;
mod external_load_jobs;
mod external;
mod external_ops;
mod gain_ops;
mod helpers;
mod input_ops;
#[cfg(feature = "kittest")]
mod kittest_ops;
mod list_ops;
mod list_state_ops;
mod list_undo;
mod list_preview_ops;
mod loudnorm_ops;
mod loading_ops;
mod logic;
mod mcp_ops;
mod meta;
mod meta_ops;
mod preview;
mod preview_ops;
mod project;
mod render;
mod rename_ops;
mod resample_ops;
mod scan_ops;
mod search_ops;
mod session_ops;
mod spectrogram;
mod spectrogram_jobs;
mod startup;
mod temp_audio_ops;
mod theme_ops;
mod tool_ops;
mod tooling;
mod transcript;
mod transcript_ops;
mod types;
mod ui;
#[cfg(feature = "kittest")]
use self::dialogs::TestDialogQueue;
use self::session_ops::ProjectOpenState;
use self::tooling::{ToolDef, ToolJob, ToolLogEntry, ToolRunResult};
pub use self::types::{
    ExternalKeyRule, ExternalRegexInput, FadeShape, LoopMode, LoopXfadeShape, StartupConfig,
    ViewMode,
};
use self::{helpers::*, types::*};

const LIVE_PREVIEW_SAMPLE_LIMIT: usize = 2_000_000;
const UNDO_STACK_LIMIT: usize = 20;
const UNDO_STACK_MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_EDITOR_TABS: usize = 12;
const SPECTRO_TILE_FRAMES: usize = 64;
const SPECTRO_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;
const BULK_RESAMPLE_THRESHOLD: usize = 10_000;
const BULK_RESAMPLE_CHUNK: usize = 200;
const BULK_RESAMPLE_BLOCK_SECS: u64 = 2;
const BULK_RESAMPLE_FRAME_BUDGET_MS: u64 = 3;

// moved to types.rs

pub struct WavesPreviewer {
    pub audio: AudioEngine,
    pub root: Option<PathBuf>,
    pub items: Vec<MediaItem>,
    pub item_index: HashMap<MediaId, usize>,
    pub path_index: HashMap<PathBuf, MediaId>,
    pub files: Vec<MediaId>,
    pub next_media_id: MediaId,
    pub selected: Option<usize>,
    pub volume_db: f32,
    pub playback_rate: f32,
    // unified numeric control via DragValue; no string normalization
    pub pitch_semitones: f32,
    pub meter_db: f32,
    pub tabs: Vec<EditorTab>,
    pub active_tab: Option<usize>,
    pub meta_rx: Option<std::sync::mpsc::Receiver<meta::MetaUpdate>>,
    pub meta_pool: Option<meta::MetaPool>,
    pub meta_inflight: HashSet<PathBuf>,
    pub transcript_inflight: HashSet<PathBuf>,
    pub show_transcript_window: bool,
    pub pending_transcript_seek: Option<(PathBuf, u64)>,
    pub external_sources: Vec<ExternalSource>,
    pub external_active_source: Option<usize>,
    pub external_source: Option<PathBuf>,
    pub external_headers: Vec<String>,
    pub external_rows: Vec<Vec<String>>,
    pub external_key_index: Option<usize>,
    pub external_key_rule: ExternalKeyRule,
    pub external_match_input: ExternalRegexInput,
    pub external_visible_columns: Vec<String>,
    pub external_lookup: HashMap<String, HashMap<String, String>>,
    pub external_key_row_index: HashMap<String, usize>,
    pub external_match_count: usize,
    pub external_unmatched_count: usize,
    pub external_show_unmatched: bool,
    pub external_unmatched_rows: Vec<usize>,
    pub external_sheet_names: Vec<String>,
    pub external_sheet_selected: Option<String>,
    pub external_has_header: bool,
    pub external_header_row: Option<usize>,
    pub external_data_row: Option<usize>,
    pub external_scope_regex: String,
    pub external_settings_dirty: bool,
    pub show_external_dialog: bool,
    pub external_load_error: Option<String>,
    pub external_match_regex: String,
    pub external_match_replace: String,
    pub external_load_rx: Option<std::sync::mpsc::Receiver<external::ExternalLoadMsg>>,
    pub external_load_inflight: bool,
    pub external_load_rows: usize,
    pub external_load_started_at: Option<std::time::Instant>,
    pub external_load_path: Option<PathBuf>,
    external_load_target: Option<external_ops::ExternalLoadTarget>,
    pub external_load_queue: VecDeque<PathBuf>,
    pub tool_defs: Vec<ToolDef>,
    pub tool_queue: std::collections::VecDeque<ToolJob>,
    pub tool_run_rx: Option<std::sync::mpsc::Receiver<ToolRunResult>>,
    pub tool_worker_busy: bool,
    pub tool_log: std::collections::VecDeque<ToolLogEntry>,
    pub tool_log_max: usize,
    pub show_tool_palette: bool,
    pub tool_search: String,
    pub tool_selected: Option<String>,
    pub tool_args_overrides: HashMap<String, String>,
    pub tool_config_error: Option<String>,
    pub pending_tool_confirm: Option<ToolJob>,
    pub spectro_cache: HashMap<PathBuf, std::sync::Arc<Vec<SpectrogramData>>>,
    pub spectro_inflight: HashSet<PathBuf>,
    pub spectro_progress: HashMap<PathBuf, SpectrogramProgress>,
    pub spectro_cancel: HashMap<PathBuf, std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub spectro_cache_order: VecDeque<PathBuf>,
    pub spectro_cache_sizes: HashMap<PathBuf, usize>,
    pub spectro_cache_bytes: usize,
    pub spectro_cfg: SpectrogramConfig,
    pub spectro_tx: Option<std::sync::mpsc::Sender<SpectrogramJobMsg>>,
    pub spectro_rx: Option<std::sync::mpsc::Receiver<SpectrogramJobMsg>>,
    pub scan_rx: Option<std::sync::mpsc::Receiver<ScanMessage>>,
    pub scan_in_progress: bool,
    pub scan_started_at: Option<std::time::Instant>,
    pub scan_found_count: usize,
    // dynamic row height for wave thumbnails (list view)
    pub wave_row_h: f32,
    pub list_columns: ListColumnConfig,
    // multi-selection (list view)
    pub selected_multi: std::collections::BTreeSet<usize>,
    pub select_anchor: Option<usize>,
    // clipboard (list copy/paste)
    pub clipboard_payload: Option<ClipboardPayload>,
    pub clipboard_temp_files: Vec<PathBuf>,
    clipboard_c_was_down: bool,
    clipboard_v_was_down: bool,
    undo_z_was_down: bool,
    // list undo/redo
    pub list_undo_stack: Vec<ListUndoAction>,
    pub list_redo_stack: Vec<ListUndoAction>,
    pub last_undo_scope: UndoScope,
    // sorting
    sort_key: SortKey,
    sort_dir: SortDir,
    // scroll behavior
    scroll_to_selected: bool,
    last_list_scroll_at: Option<std::time::Instant>,
    auto_play_list_nav: bool,
    suppress_list_enter: bool,
    list_has_focus: bool,
    search_has_focus: bool,
    // original order snapshot for tri-state sort
    original_files: Vec<MediaId>,
    // search
    search_query: String,
    search_use_regex: bool,
    search_dirty: bool,
    search_deadline: Option<std::time::Instant>,
    // list filtering
    skip_dotfiles: bool,
    zero_cross_epsilon: f32,
    // processing mode
    mode: RateMode,
    // heavy processing state (overlay)
    processing: Option<ProcessingState>,
    // background full load for list preview
    list_preview_rx: Option<std::sync::mpsc::Receiver<ListPreviewResult>>,
    list_preview_job_id: u64,
    // background heavy apply for editor (pitch/stretch)
    editor_apply_state: Option<EditorApplyState>,
    // background decode for editor (prefix + full)
    editor_decode_state: Option<EditorDecodeState>,
    editor_decode_job_id: u64,
    // cached edited audio when tabs are closed (kept until save)
    edited_cache: HashMap<PathBuf, CachedEdit>,
    // background export state (gains)
    export_state: Option<ExportState>,
    // blocking CSV export (waits for full metadata)
    csv_export_state: Option<CsvExportState>,
    // currently loaded/playing file path (for effective volume calc)
    playing_path: Option<PathBuf>,
    // export/save settings (simple, in-memory)
    export_cfg: ExportConfig,
    show_export_settings: bool,
    show_first_save_prompt: bool,
    project_path: Option<PathBuf>,
    project_open_pending: Option<PathBuf>,
    project_open_state: Option<ProjectOpenState>,
    theme_mode: ThemeMode,
    show_rename_dialog: bool,
    rename_target: Option<PathBuf>,
    rename_input: String,
    rename_error: Option<String>,
    show_batch_rename_dialog: bool,
    batch_rename_targets: Vec<PathBuf>,
    batch_rename_pattern: String,
    batch_rename_start: u32,
    batch_rename_pad: u32,
    batch_rename_error: Option<String>,
    saving_sources: Vec<PathBuf>,
    saving_virtual: Vec<(PathBuf, PathBuf)>,
    saving_edit_sources: Vec<PathBuf>,
    saving_mode: Option<SaveMode>,

    // LUFS with Gain recompute support
    lufs_override: HashMap<PathBuf, f32>,
    lufs_recalc_deadline: HashMap<PathBuf, std::time::Instant>,
    lufs_rx2: Option<std::sync::mpsc::Receiver<(PathBuf, f32)>>,
    lufs_worker_busy: bool,
    // Sample rate conversion (non-destructive)
    sample_rate_override: HashMap<PathBuf, u32>,
    show_resample_dialog: bool,
    resample_targets: Vec<PathBuf>,
    resample_target_sr: u32,
    resample_error: Option<String>,
    bulk_resample_state: Option<BulkResampleState>,
    // leaving dirty editor confirmation
    leave_intent: Option<LeaveIntent>,
    show_leave_prompt: bool,
    pending_activate_path: Option<PathBuf>,
    // Heavy preview worker for Pitch/Stretch (mono)
    heavy_preview_rx: Option<std::sync::mpsc::Receiver<Vec<f32>>>,
    heavy_preview_tool: Option<ToolKind>,
    // Heavy overlay worker (per-channel preview for Pitch/Stretch) with generation guard
    heavy_overlay_rx:
        Option<std::sync::mpsc::Receiver<(std::path::PathBuf, Vec<Vec<f32>>, usize, u64)>>,
    overlay_gen_counter: u64,
    overlay_expected_gen: u64,
    overlay_expected_tool: Option<ToolKind>,

    // startup automation/screenshot
    startup: StartupState,
    pending_screenshot: Option<PathBuf>,
    exit_after_screenshot: bool,
    screenshot_seq: u64,

    // debug/automation
    debug: DebugState,
    debug_summary_seq: u64,
    ipc_rx: Option<std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<ipc::IpcRequest>>>>,
    mcp_cmd_rx: Option<std::sync::mpsc::Receiver<crate::mcp::UiCommand>>,
    mcp_resp_tx: Option<std::sync::mpsc::Sender<crate::mcp::UiCommandResult>>,
    #[cfg(feature = "kittest")]
    test_dialogs: TestDialogQueue,
}

impl WavesPreviewer {
    fn close_tab_at(&mut self, idx: usize, ctx: &egui::Context) {
        self.clear_preview_if_any(idx);
        self.cache_dirty_tab_at(idx);
        self.audio.stop();
        self.tabs.remove(idx);
        if !self.tabs.is_empty() {
            let new_active = if idx < self.tabs.len() {
                idx
            } else {
                self.tabs.len() - 1
            };
            self.active_tab = Some(new_active);
        } else {
            self.active_tab = None;
            self.request_list_focus(ctx);
        }
    }

    fn mixdown_channels(chs: &[Vec<f32>], len: usize) -> Vec<f32> {
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
    fn editor_mixdown_mono(tab: &EditorTab) -> Vec<f32> {
        Self::mixdown_channels(&tab.ch_samples, tab.samples_len)
    }
    fn draw_spectrogram(
        painter: &egui::Painter,
        area: egui::Rect,
        tab: &EditorTab,
        spec: &SpectrogramData,
        view_mode: ViewMode,
        cfg: &SpectrogramConfig,
    ) {
        if spec.frames == 0 || spec.bins == 0 {
            return;
        }
        let width_px = area.width().max(1.0);
        let height_px = area.height().max(1.0);
        let spp = tab.samples_per_px.max(0.0001);
        let vis = (width_px * spp).ceil() as usize;
        let start = tab.view_offset.min(tab.samples_len);
        let end = (start + vis).min(tab.samples_len);
        let frame_step = spec.frame_step.max(1);
        let f0 = (start / frame_step).min(spec.frames.saturating_sub(1));
        let mut f1 = (end / frame_step).min(spec.frames);
        if f1 <= f0 {
            f1 = (f0 + 1).min(spec.frames);
        }
        let frame_count = f1.saturating_sub(f0).max(1);
        let target_w = (width_px / 3.0).clamp(64.0, 256.0) as usize;
        let target_h = (height_px / 3.0).clamp(64.0, 192.0) as usize;
        let cell_w = width_px / target_w as f32;
        let cell_h = height_px / target_h as f32;
        let max_bin = spec.bins.saturating_sub(1).max(1);
        let sr = spec.sample_rate.max(1) as f32;
        let mut max_freq = sr * 0.5;
        if cfg.max_freq_hz > 0.0 {
            max_freq = cfg.max_freq_hz.min(max_freq).max(1.0);
        }
        let mel_max = 2595.0 * (1.0 + max_freq / 700.0).log10();
        let mel_min = 1.0_f32;
        let log_min = 20.0_f32.min(max_freq).max(1.0);
        for x in 0..target_w {
            let frame_idx = f0 + ((x * frame_count) / target_w).min(frame_count - 1);
            let base = frame_idx * spec.bins;
            for y in 0..target_h {
                // y=0 is bottom row; map low frequency to bottom, high to top.
                let frac = y as f32 / (target_h.saturating_sub(1)) as f32;
                let bin = match view_mode {
                    ViewMode::Spectrogram | ViewMode::Waveform => {
                        let freq = match cfg.scale {
                            SpectrogramScale::Linear => frac * max_freq,
                            SpectrogramScale::Log => {
                                if max_freq <= log_min {
                                    frac * max_freq
                                } else {
                                    let ratio = max_freq / log_min;
                                    log_min * ratio.powf(frac)
                                }
                            }
                        };
                        let pos = (freq / max_freq).clamp(0.0, 1.0);
                        (pos * max_bin as f32).round() as usize
                    }
                    ViewMode::Mel => {
                        let mel = match cfg.mel_scale {
                            SpectrogramScale::Linear => mel_max * frac,
                            SpectrogramScale::Log => {
                                if mel_max <= mel_min {
                                    mel_max * frac
                                } else {
                                    let ratio = mel_max / mel_min;
                                    mel_min * ratio.powf(frac)
                                }
                            }
                        };
                        let freq = 700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0);
                        let pos = (freq / max_freq).clamp(0.0, 1.0);
                        (pos * max_bin as f32).round() as usize
                    }
                };
                let idx = base + bin.min(max_bin);
                let db_raw = spec
                    .values_db
                    .get(idx)
                    .copied()
                    .unwrap_or(-120.0)
                    .clamp(cfg.db_floor, 0.0);
                let norm = if (0.0 - cfg.db_floor).abs() < f32::EPSILON {
                    0.0
                } else {
                    (db_raw - cfg.db_floor) / (0.0 - cfg.db_floor)
                };
                let db_mapped = -80.0 + norm.clamp(0.0, 1.0) * 80.0;
                let col = db_to_color(db_mapped);
                let x0 = area.left() + x as f32 * cell_w;
                let y0 = area.bottom() - (y as f32 + 1.0) * cell_h;
                let r = egui::Rect::from_min_size(
                    egui::pos2(x0, y0),
                    egui::vec2(cell_w + 0.5, cell_h + 0.5),
                );
                painter.rect_filled(r, 0.0, col);
            }
        }
    }
    // editor operations moved to editor_ops.rs
    fn effective_loop_xfade_samples(
        loop_start: usize,
        loop_end: usize,
        total_len: usize,
        requested: usize,
    ) -> usize {
        if loop_end <= loop_start || loop_end > total_len || total_len == 0 {
            return 0;
        }
        let loop_len = loop_end - loop_start;
        let mut cf = requested.min(loop_len / 2);
        cf = cf.min(loop_start);
        cf = cf.min(total_len.saturating_sub(loop_end));
        cf
    }
    fn apply_loop_mode_for_tab(&self, tab: &EditorTab) {
        match tab.loop_mode {
            LoopMode::Off => {
                self.audio.set_loop_enabled(false);
            }
            LoopMode::OnWhole => {
                self.audio.set_loop_enabled(true);
                if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                    let len = buf.len();
                    self.audio.set_loop_region(0, len);
                    let cf =
                        Self::effective_loop_xfade_samples(0, len, len, tab.loop_xfade_samples);
                    self.audio.set_loop_crossfade(
                        cf,
                        match tab.loop_xfade_shape {
                            crate::app::types::LoopXfadeShape::Linear => 0,
                            crate::app::types::LoopXfadeShape::EqualPower => 1,
                        },
                    );
                }
            }
            LoopMode::Marker => {
                if let Some((a, b)) = tab.loop_region {
                    if a != b {
                        let (s, e) = if a <= b { (a, b) } else { (b, a) };
                        self.audio.set_loop_enabled(true);
                        self.audio.set_loop_region(s, e);
                        let cf = Self::effective_loop_xfade_samples(
                            s,
                            e,
                            tab.samples_len,
                            tab.loop_xfade_samples,
                        );
                        self.audio.set_loop_crossfade(
                            cf,
                            match tab.loop_xfade_shape {
                                crate::app::types::LoopXfadeShape::Linear => 0,
                                crate::app::types::LoopXfadeShape::EqualPower => 1,
                            },
                        );
                        return;
                    }
                }
                self.audio.set_loop_enabled(false);
            }
        }
    }
    #[allow(dead_code)]
    fn set_marker_sample(tab: &mut EditorTab, idx: usize) {
        match tab.loop_region {
            None => tab.loop_region = Some((idx, idx)),
            Some((a, b)) => {
                if a == b {
                    tab.loop_region = Some((a.min(idx), a.max(idx)));
                } else {
                    let da = a.abs_diff(idx);
                    let db = b.abs_diff(idx);
                    if da <= db {
                        tab.loop_region = Some((idx, b));
                    } else {
                        tab.loop_region = Some((a, idx));
                    }
                }
            }
        }
        Self::update_loop_markers_dirty(tab);
    }

    fn update_loop_markers_dirty(tab: &mut EditorTab) {
        tab.loop_markers_dirty = tab.loop_region_committed != tab.loop_markers_saved;
    }

    fn next_marker_label(markers: &[crate::markers::MarkerEntry]) -> String {
        let mut idx = markers.len() + 1;
        loop {
            let label = format!("M{:02}", idx);
            if !markers.iter().any(|m| m.label == label) {
                return label;
            }
            idx = idx.saturating_add(1);
        }
    }

    fn clear_all_pending_gains_with_undo(&mut self) {
        let mut paths: Vec<PathBuf> = self
            .items
            .iter()
            .filter(|item| {
                item.pending_gain_db.abs() > 0.0001
                    || self.lufs_override.contains_key(&item.path)
                    || self.lufs_recalc_deadline.contains_key(&item.path)
            })
            .map(|item| item.path.clone())
            .collect();
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            return;
        }
        let before = self.capture_list_selection_snapshot();
        let before_items = self.capture_list_undo_items_by_paths(&paths);
        for item in &mut self.items {
            item.pending_gain_db = 0.0;
        }
        self.lufs_override.clear();
        self.lufs_recalc_deadline.clear();
        self.record_list_update_from_paths(&paths, before_items, before);
    }

    fn make_media_item(&mut self, path: PathBuf) -> MediaItem {
        let id = self.next_media_id;
        self.next_media_id = self.next_media_id.wrapping_add(1);
        let display_name = Self::display_name_for_path(&path);
        let display_folder = Self::display_folder_for_path(&path);
        let mut item = MediaItem {
            id,
            path,
            display_name,
            display_folder,
            source: MediaSource::File,
            meta: None,
            pending_gain_db: 0.0,
            status: MediaStatus::Ok,
            transcript: None,
            external: HashMap::new(),
            virtual_audio: None,
        };
        self.fill_external_for_item(&mut item);
        item
    }

    fn build_meta_from_audio(
        channels: &[Vec<f32>],
        sample_rate: u32,
        bits_per_sample: u16,
    ) -> FileMeta {
        let frames = channels.get(0).map(|c| c.len()).unwrap_or(0);
        let mut mono = Vec::with_capacity(frames);
        if frames > 0 {
            for i in 0..frames {
                let mut acc = 0.0f32;
                let mut c = 0usize;
                for ch in channels.iter() {
                    if let Some(&v) = ch.get(i) {
                        acc += v;
                        c += 1;
                    }
                }
                mono.push(if c > 0 { acc / (c as f32) } else { 0.0 });
            }
        }
        let mut sum_sq = 0.0f64;
        for &v in &mono {
            sum_sq += (v as f64) * (v as f64);
        }
        let n = mono.len().max(1) as f64;
        let rms = (sum_sq / n).sqrt() as f32;
        let rms_db = if rms > 0.0 {
            20.0 * rms.log10()
        } else {
            -120.0
        };
        let mut peak_abs = 0.0f32;
        for ch in channels {
            for &v in ch {
                let a = v.abs();
                if a > peak_abs {
                    peak_abs = a;
                }
            }
        }
        let silent_thresh = 10.0_f32.powf(-80.0 / 20.0);
        let peak_db = if peak_abs > silent_thresh {
            20.0 * peak_abs.log10()
        } else {
            f32::NEG_INFINITY
        };
        let mut thumb = Vec::new();
        build_minmax(&mut thumb, &mono, 128);
        let lufs_i = crate::wave::lufs_integrated_from_multi(channels, sample_rate).ok();
        let bpm = None;
        let duration_secs = if sample_rate > 0 {
            Some(frames as f32 / sample_rate as f32)
        } else {
            None
        };
        FileMeta {
            channels: channels.len().max(1) as u16,
            sample_rate,
            bits_per_sample,
            bit_rate_bps: None,
            duration_secs,
            rms_db: Some(rms_db),
            peak_db: Some(peak_db),
            lufs_i,
            bpm,
            created_at: None,
            modified_at: None,
            thumb,
            decode_error: None,
        }
    }

    fn make_virtual_item(
        &mut self,
        display_name: String,
        audio: std::sync::Arc<crate::audio::AudioBuffer>,
        sample_rate: u32,
        bits_per_sample: u16,
    ) -> MediaItem {
        let id = self.next_media_id;
        self.next_media_id = self.next_media_id.wrapping_add(1);
        let safe = crate::app::helpers::sanitize_filename_component(&display_name);
        let path = PathBuf::from("__virtual__").join(format!("{id}_{safe}"));
        MediaItem {
            id,
            path,
            display_name,
            display_folder: "(virtual)".to_string(),
            source: MediaSource::Virtual,
            meta: Some(Self::build_meta_from_audio(
                &audio.channels,
                sample_rate,
                bits_per_sample,
            )),
            pending_gain_db: 0.0,
            status: MediaStatus::Ok,
            transcript: None,
            external: HashMap::new(),
            virtual_audio: Some(audio),
        }
    }

    fn add_virtual_item(&mut self, item: MediaItem, insert_idx: Option<usize>) {
        let id = item.id;
        let path = item.path.clone();
        let idx = insert_idx.unwrap_or(self.items.len()).min(self.items.len());
        self.items.insert(idx, item);
        self.path_index.insert(path, id);
        for i in idx..self.items.len() {
            let id = self.items[i].id;
            self.item_index.insert(id, i);
        }
    }

    fn unique_virtual_display_name(&self, base: &str) -> String {
        let existing: std::collections::HashSet<String> = self
            .items
            .iter()
            .map(|i| i.display_name.to_lowercase())
            .collect();
        if !existing.contains(&base.to_lowercase()) {
            return base.to_string();
        }
        let path = std::path::Path::new(base);
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(base);
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        for i in 1.. {
            let name = if ext.is_empty() {
                format!("{stem} ({i})")
            } else {
                format!("{stem} ({i}).{ext}")
            };
            if !existing.contains(&name.to_lowercase()) {
                return name;
            }
        }
        base.to_string()
    }

    fn time_stretch_ratio_for_tab(&self, tab: &EditorTab) -> Option<f32> {
        let time_stretch_active = self.mode == RateMode::TimeStretch
            || tab.preview_audio_tool == Some(ToolKind::TimeStretch);
        if !time_stretch_active {
            return None;
        }
        let audio_len = self
            .audio
            .shared
            .samples
            .load()
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);
        if audio_len == 0 || tab.samples_len == 0 {
            return None;
        }
        let ratio = audio_len as f32 / tab.samples_len as f32;
        if (ratio - 1.0).abs() < 1.0e-4 {
            None
        } else {
            Some(ratio)
        }
    }
    fn map_audio_to_display_sample(&self, tab: &EditorTab, audio_pos: usize) -> usize {
        if let Some(ratio) = self.time_stretch_ratio_for_tab(tab) {
            let mapped = ((audio_pos as f32) / ratio).round() as usize;
            mapped.min(tab.samples_len)
        } else {
            audio_pos.min(tab.samples_len)
        }
    }
    fn map_display_to_audio_sample(&self, tab: &EditorTab, display_pos: usize) -> usize {
        if let Some(ratio) = self.time_stretch_ratio_for_tab(tab) {
            let mapped = ((display_pos as f32) * ratio).round() as usize;
            let audio_len = self
                .audio
                .shared
                .samples
                .load()
                .as_ref()
                .map(|s| s.len())
                .unwrap_or(0);
            if audio_len > 0 {
                mapped.min(audio_len)
            } else {
                mapped
            }
        } else {
            display_pos
        }
    }

    fn export_list_csv(
        &self,
        path: &Path,
        ids: &[MediaId],
        cols: ListColumnConfig,
        external_cols: &[String],
    ) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("csv export mkdir failed: {e}"))?;
            }
        }
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)
            .map_err(|e| format!("csv export open failed: {e}"))?;
        let mut header: Vec<String> = Vec::new();
        if cols.edited {
            header.push("Edited".to_string());
        }
        if cols.file {
            header.push("File".to_string());
        }
        if cols.folder {
            header.push("Folder".to_string());
        }
        if cols.transcript {
            header.push("Transcript".to_string());
        }
        if cols.external {
            for name in external_cols.iter() {
                header.push(name.clone());
            }
        }
        if cols.length {
            header.push("Length".to_string());
        }
        if cols.channels {
            header.push("Ch".to_string());
        }
        if cols.sample_rate {
            header.push("SR".to_string());
        }
        if cols.bits {
            header.push("Bits".to_string());
        }
        if cols.bit_rate {
            header.push("Bitrate (kbps)".to_string());
        }
        if cols.peak {
            header.push("dBFS (Peak)".to_string());
        }
        if cols.lufs {
            header.push("LUFS (I)".to_string());
        }
        if cols.bpm {
            header.push("BPM".to_string());
        }
        if cols.created_at {
            header.push("Created".to_string());
        }
        if cols.modified_at {
            header.push("Modified".to_string());
        }
        if cols.gain {
            header.push("Gain (dB)".to_string());
        }
        if !header.is_empty() {
            writer
                .write_record(header)
                .map_err(|e| format!("csv export header failed: {e}"))?;
        }

        for id in ids.iter().copied() {
            let Some(item) = self.item_for_id(id) else {
                continue;
            };
            let meta = item.meta.as_ref();
            let mut row: Vec<String> = Vec::new();
            if cols.edited {
                let edited = self.has_edits_for_paths(&[item.path.clone()]);
                row.push(if edited {
                    "\u{25CF}".to_string()
                } else {
                    "".to_string()
                });
            }
            if cols.file {
                row.push(item.display_name.clone());
            }
            if cols.folder {
                row.push(item.display_folder.clone());
            }
            if cols.transcript {
                row.push(
                    item.transcript
                        .as_ref()
                        .map(|t| t.full_text.clone())
                        .unwrap_or_default(),
                );
            }
            if cols.external {
                for name in external_cols.iter() {
                    row.push(item.external.get(name).cloned().unwrap_or_default());
                }
            }
            if cols.length {
                let text = meta
                    .and_then(|m| m.duration_secs)
                    .map(crate::app::helpers::format_duration)
                    .unwrap_or_default();
                row.push(text);
            }
            if cols.channels {
                let text = meta
                    .map(|m| m.channels)
                    .filter(|v| *v > 0)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string());
                row.push(text);
            }
            if cols.sample_rate {
                let text = self
                    .effective_sample_rate_for_path(&item.path)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string());
                row.push(text);
            }
            if cols.bits {
                let text = meta
                    .map(|m| m.bits_per_sample)
                    .filter(|v| *v > 0)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string());
                row.push(text);
            }
            if cols.bit_rate {
                let text = meta
                    .and_then(|m| m.bit_rate_bps)
                    .filter(|v| *v > 0)
                    .map(|v| format!("{:.0}", (v as f32) / 1000.0))
                    .unwrap_or_else(|| "-".to_string());
                row.push(text);
            }
            if cols.peak {
                let gain_db = item.pending_gain_db;
                let adj = meta.and_then(|m| m.peak_db).map(|db| db + gain_db);
                row.push(adj.map(|db| format!("{:.1}", db)).unwrap_or_default());
            }
            if cols.lufs {
                let gain_db = item.pending_gain_db;
                let base = meta.and_then(|m| m.lufs_i);
                let eff = self
                    .lufs_override
                    .get(&item.path)
                    .copied()
                    .or_else(|| base.map(|v| v + gain_db));
                row.push(eff.map(|db| format!("{:.1}", db)).unwrap_or_default());
            }
            if cols.bpm {
                let bpm = meta
                    .and_then(|m| m.bpm)
                    .filter(|v| v.is_finite() && *v > 0.0);
                row.push(
                    bpm.map(|v| format!("{:.2}", v))
                        .unwrap_or_else(|| "-".to_string()),
                );
            }
            if cols.created_at {
                let text = meta
                    .and_then(|m| m.created_at)
                    .map(crate::app::helpers::format_system_time_local)
                    .unwrap_or_else(|| "-".to_string());
                row.push(text);
            }
            if cols.modified_at {
                let text = meta
                    .and_then(|m| m.modified_at)
                    .map(crate::app::helpers::format_system_time_local)
                    .unwrap_or_else(|| "-".to_string());
                row.push(text);
            }
            if cols.gain {
                row.push(format!("{:.1}", item.pending_gain_db));
            }
            writer
                .write_record(row)
                .map_err(|e| format!("csv export row failed: {e}"))?;
        }
        writer
            .flush()
            .map_err(|e| format!("csv export flush failed: {e}"))?;
        Ok(())
    }

    fn csv_meta_ready(&self, path: &Path, needs_peak: bool, needs_lufs: bool) -> bool {
        let Some(meta) = self.meta_for_path(path) else {
            return false;
        };
        if meta.decode_error.is_some() {
            return true;
        }
        if needs_peak && meta.peak_db.is_none() {
            return false;
        }
        if needs_lufs && meta.lufs_i.is_none() {
            return false;
        }
        true
    }

    fn begin_export_list_csv(&mut self, path: PathBuf) {
        if self.csv_export_state.is_some() {
            self.debug_log("csv export already running".to_string());
            return;
        }
        let ids = self.files.clone();
        let cols = self.list_columns;
        let external_cols = if cols.external {
            self.external_visible_columns.clone()
        } else {
            Vec::new()
        };
        let needs_peak = cols.peak;
        let needs_lufs = cols.lufs;
        let needs_full_decode = needs_peak || needs_lufs;
        let needs_meta = cols.length
            || cols.channels
            || cols.sample_rate
            || cols.bits
            || cols.bit_rate
            || needs_peak
            || needs_lufs
            || cols.created_at
            || cols.modified_at;
        let mut pending = HashSet::new();
        let mut total = 0usize;
        let mut done = 0usize;
        for id in ids.iter().copied() {
            let Some(item) = self.item_for_id(id) else {
                continue;
            };
            if item.source == MediaSource::Virtual {
                total += 1;
                done += 1;
                continue;
            }
            if !item.path.is_file() {
                total += 1;
                done += 1;
                continue;
            }
            total += 1;
            if needs_meta && !self.csv_meta_ready(&item.path, needs_peak, needs_lufs) {
                pending.insert(item.path.clone());
            } else {
                done += 1;
            }
        }

        if !needs_meta || pending.is_empty() {
            if let Err(err) = self.export_list_csv(&path, &ids, cols, &external_cols) {
                self.debug_log(format!("csv export error: {err}"));
            }
            return;
        }

        for p in pending.iter() {
            if needs_full_decode {
                self.queue_full_meta_for_path(p, false);
            } else {
                self.queue_meta_for_path(p, false);
            }
        }
        self.csv_export_state = Some(CsvExportState {
            path,
            ids,
            cols,
            external_cols,
            total,
            done,
            pending,
            needs_peak,
            needs_lufs,
            started_at: std::time::Instant::now(),
        });
    }

    fn update_csv_export_progress_for_path(&mut self, path: &Path) {
        let (needs_peak, needs_lufs, pending) = match self.csv_export_state.as_ref() {
            Some(state) => (
                state.needs_peak,
                state.needs_lufs,
                state.pending.contains(path),
            ),
            None => return,
        };
        if !pending {
            return;
        }
        let ready = self.csv_meta_ready(path, needs_peak, needs_lufs);
        if ready {
            if let Some(state) = &mut self.csv_export_state {
                if state.pending.remove(path) {
                    state.done = state.done.saturating_add(1);
                }
            }
        }
    }

    fn check_csv_export_completion(&mut self) {
        let ready = self
            .csv_export_state
            .as_ref()
            .map(|state| state.pending.is_empty())
            .unwrap_or(false);
        if !ready {
            return;
        }
        let Some(state) = self.csv_export_state.take() else {
            return;
        };
        if let Err(err) = self.export_list_csv(&state.path, &state.ids, state.cols, &state.external_cols) {
            self.debug_log(format!("csv export error: {err}"));
        }
    }

    fn trigger_save_selected(&mut self) {
        if self.export_cfg.first_prompt {
            self.show_first_save_prompt = true;
            return;
        }
        let mut set = self.selected_multi.clone();
        if set.is_empty() {
            if let Some(i) = self.selected {
                set.insert(i);
            }
        }
        self.spawn_save_selected(set);
    }

    fn clamp_gain_db(val: f32) -> f32 {
        if !val.is_finite() {
            return 0.0;
        }
        let mut g = val.clamp(-24.0, 24.0);
        if g.abs() < 0.001 {
            g = 0.0;
        }
        g
    }

    fn adjust_gain_for_indices(
        &mut self,
        indices: &std::collections::BTreeSet<usize>,
        delta_db: f32,
    ) {
        if indices.is_empty() {
            return;
        }
        let mut paths: Vec<PathBuf> = indices
            .iter()
            .filter_map(|&i| self.path_for_row(i).cloned())
            .collect();
        paths.sort();
        paths.dedup();
        let before = self.capture_list_selection_snapshot();
        let before_items = self.capture_list_undo_items_by_paths(&paths);
        let mut affect_playing = false;
        for &i in indices {
            if let Some(p) = self.path_for_row(i).cloned() {
                let cur = self.pending_gain_db_for_path(&p);
                let new = Self::clamp_gain_db(cur + delta_db);
                self.set_pending_gain_db_for_path(&p, new);
                if self.playing_path.as_ref() == Some(&p) {
                    affect_playing = true;
                }
                // schedule LUFS recompute for each affected path
                self.schedule_lufs_for_path(p.clone());
            }
        }
        if affect_playing {
            self.apply_effective_volume();
        }
        self.record_list_update_from_paths(&paths, before_items, before);
    }

    fn handle_mcp_command(
        &mut self,
        cmd: mcp::UiCommand,
        ctx: &egui::Context,
    ) -> mcp::UiCommandResult {
        use serde_json::{json, to_value, Value};
        let ok = |payload: Value| mcp::UiCommandResult {
            ok: true,
            payload,
            error: None,
        };
        let err = |msg: String| mcp::UiCommandResult {
            ok: false,
            payload: Value::Null,
            error: Some(msg),
        };
        match cmd {
            mcp::UiCommand::ListFiles(args) => match self.mcp_list_files(args) {
                Ok(res) => ok(to_value(res).unwrap_or(Value::Null)),
                Err(e) => err(e),
            },
            mcp::UiCommand::GetSelection => {
                let selected_paths: Vec<String> = self
                    .selected_paths()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                let active_tab_path = self
                    .active_tab
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| t.path.display().to_string());
                ok(to_value(mcp::types::SelectionResult {
                    selected_paths,
                    active_tab_path,
                })
                .unwrap_or(Value::Null))
            }
            mcp::UiCommand::SetSelection(args) => {
                let mut found_rows: Vec<usize> = Vec::new();
                for p in &args.paths {
                    let path = PathBuf::from(p);
                    if let Some(row) = self.row_for_path(&path) {
                        found_rows.push(row);
                    }
                }
                if found_rows.is_empty() {
                    return err("NOT_FOUND: no matching paths in list".to_string());
                }
                found_rows.sort_unstable();
                self.selected_multi.clear();
                for row in &found_rows {
                    self.selected_multi.insert(*row);
                }
                self.selected = Some(found_rows[0]);
                self.select_anchor = Some(found_rows[0]);
                self.select_and_load(found_rows[0], true);
                if args.open_tab.unwrap_or(false) {
                    if let Some(path) = self.path_for_row(found_rows[0]).cloned() {
                        self.open_or_activate_tab(&path);
                    }
                }
                let selected_paths: Vec<String> = found_rows
                    .iter()
                    .filter_map(|row| self.path_for_row(*row))
                    .map(|p| p.display().to_string())
                    .collect();
                let active_tab_path = self
                    .active_tab
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| t.path.display().to_string());
                ok(to_value(mcp::types::SelectionResult {
                    selected_paths,
                    active_tab_path,
                })
                .unwrap_or(Value::Null))
            }
            mcp::UiCommand::Play => {
                let playing = self
                    .audio
                    .shared
                    .playing
                    .load(std::sync::atomic::Ordering::Relaxed);
                if !playing {
                    self.audio.toggle_play();
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::Stop => {
                let playing = self
                    .audio
                    .shared
                    .playing
                    .load(std::sync::atomic::Ordering::Relaxed);
                if playing {
                    self.audio.toggle_play();
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetVolume(args) => {
                self.volume_db = args.db;
                self.apply_effective_volume();
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetMode(args) => {
                let prev = self.mode;
                self.mode = match args.mode.as_str() {
                    "Speed" => RateMode::Speed,
                    "PitchShift" => RateMode::PitchShift,
                    "TimeStretch" => RateMode::TimeStretch,
                    _ => prev,
                };
                if self.mode != prev {
                    match self.mode {
                        RateMode::Speed => {
                            self.audio.set_rate(self.playback_rate);
                        }
                        _ => {
                            self.audio.set_rate(1.0);
                            self.rebuild_current_buffer_with_mode();
                        }
                    }
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetSpeed(args) => {
                self.playback_rate = args.rate;
                match self.mode {
                    RateMode::Speed => {
                        self.audio.set_rate(self.playback_rate);
                    }
                    RateMode::TimeStretch => {
                        self.audio.set_rate(1.0);
                        self.rebuild_current_buffer_with_mode();
                    }
                    _ => {}
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetPitch(args) => {
                self.pitch_semitones = args.semitones;
                if self.mode == RateMode::PitchShift {
                    self.audio.set_rate(1.0);
                    self.rebuild_current_buffer_with_mode();
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::SetStretch(args) => {
                self.playback_rate = args.rate;
                if self.mode == RateMode::TimeStretch {
                    self.audio.set_rate(1.0);
                    self.rebuild_current_buffer_with_mode();
                }
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::ApplyGain(args) => {
                let path = PathBuf::from(args.path);
                if self.path_index.contains_key(&path) {
                    let new = Self::clamp_gain_db(args.db);
                    self.set_pending_gain_db_for_path(&path, new);
                    if self.playing_path.as_ref() == Some(&path) {
                        self.apply_effective_volume();
                    }
                    self.schedule_lufs_for_path(path);
                    ok(json!({"ok": true}))
                } else {
                    err("NOT_FOUND: file not in list".to_string())
                }
            }
            mcp::UiCommand::ClearGain(args) => {
                let path = PathBuf::from(args.path);
                if self.path_index.contains_key(&path) {
                    self.set_pending_gain_db_for_path(&path, 0.0);
                    self.lufs_override.remove(&path);
                    self.lufs_recalc_deadline.remove(&path);
                    if self.playing_path.as_ref() == Some(&path) {
                        self.apply_effective_volume();
                    }
                    ok(json!({"ok": true}))
                } else {
                    err("NOT_FOUND: file not in list".to_string())
                }
            }
            mcp::UiCommand::SetLoopMarkers(args) => {
                let path = PathBuf::from(args.path);
                if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
                    if let Some(tab) = self.tabs.get_mut(idx) {
                        let s = args.start_samples as usize;
                        let e = args.end_samples as usize;
                        if s < e && e <= tab.samples_len {
                            tab.loop_region = Some((s, e));
                            Self::update_loop_markers_dirty(tab);
                        }
                    }
                    ok(json!({"ok": true}))
                } else {
                    err("NOT_FOUND: tab not open".to_string())
                }
            }
            mcp::UiCommand::WriteLoopMarkers(args) => {
                let path = PathBuf::from(args.path);
                if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
                    if self.write_loop_markers_for_tab(idx) {
                        ok(json!({"ok": true}))
                    } else {
                        err("FAILED: write loop markers".to_string())
                    }
                } else {
                    err("NOT_FOUND: tab not open".to_string())
                }
            }
            mcp::UiCommand::Export(args) => {
                match args.mode.as_str() {
                    "Overwrite" => self.export_cfg.save_mode = SaveMode::Overwrite,
                    "NewFile" => self.export_cfg.save_mode = SaveMode::NewFile,
                    _ => {}
                }
                if let Some(dest) = args.dest_folder {
                    self.export_cfg.dest_folder = Some(PathBuf::from(dest));
                }
                if let Some(template) = args.name_template {
                    self.export_cfg.name_template = template;
                }
                if let Some(conflict) = args.conflict {
                    self.export_cfg.conflict = match conflict.as_str() {
                        "Overwrite" => ConflictPolicy::Overwrite,
                        "Skip" => ConflictPolicy::Skip,
                        _ => ConflictPolicy::Rename,
                    };
                }
                self.export_cfg.first_prompt = false;
                self.trigger_save_selected();
                ok(json!({"queued": true}))
            }
            mcp::UiCommand::OpenFolder(args) => {
                let path = PathBuf::from(args.path);
                if path.is_dir() {
                    self.root = Some(path);
                    self.rescan();
                    ok(json!({"ok": true}))
                } else {
                    err("NOT_FOUND: folder not found".to_string())
                }
            }
            mcp::UiCommand::OpenFiles(args) => {
                let paths: Vec<PathBuf> = args.paths.into_iter().map(PathBuf::from).collect();
                self.replace_with_files(&paths);
                self.after_add_refresh();
                ok(json!({"ok": true}))
            }
            mcp::UiCommand::Screenshot(args) => {
                let path = args
                    .path
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.default_screenshot_path());
                self.request_screenshot(ctx, path.clone(), false);
                ok(json!({"path": path.display().to_string()}))
            }
            mcp::UiCommand::DebugSummary => {
                let selected_paths: Vec<String> = self
                    .selected_paths()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                let active_tab_path = self
                    .active_tab
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| t.path.display().to_string());
                let mode = Some(format!("{:?}", self.mode));
                let playing = self
                    .audio
                    .shared
                    .playing
                    .load(std::sync::atomic::Ordering::Relaxed);
                ok(to_value(mcp::types::DebugSummary {
                    selected_paths,
                    active_tab_path,
                    mode,
                    playing,
                })
                .unwrap_or(Value::Null))
            }
        }
    }

    fn populate_dummy_list(&mut self, count: usize) {
        self.audio.stop();
        self.tabs.clear();
        self.active_tab = None;
        self.playing_path = None;
        self.root = None;
        self.scan_rx = None;
        self.scan_in_progress = false;
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.reset_meta_pool();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
        self.lufs_override.clear();
        self.lufs_recalc_deadline.clear();
        self.selected = None;
        self.selected_multi.clear();
        self.select_anchor = None;
        self.search_query.clear();
        self.search_dirty = false;
        self.search_deadline = None;
        self.files.clear();
        self.original_files.clear();
        if count == 0 {
            return;
        }
        self.items.reserve(count);
        let prefix = "C:\\_dummy\\waves";
        for i in 0..count {
            let name = format!("wav_{:06}.wav", i);
            let path = PathBuf::from(prefix).join(name);
            let mut item = self.make_media_item(path.clone());
            // Keep dummy entries from being removed as missing files.
            item.source = MediaSource::Virtual;
            self.path_index.insert(path, item.id);
            self.item_index.insert(item.id, self.items.len());
            self.items.push(item);
        }
        self.files.extend(self.items.iter().map(|item| item.id));
        self.original_files = self.files.clone();
        self.apply_sort();
        self.debug_log(format!("dummy list populated: {count}"));
    }

    fn setup_mcp_server(&mut self, cfg: &StartupConfig) {
        let http_addr = cfg.mcp_http_addr.clone();
        let use_stdio = cfg.mcp_stdio && http_addr.is_none();
        if http_addr.is_none() && !use_stdio {
            return;
        }
        use std::sync::mpsc;
        let (cmd_tx, cmd_rx) = mpsc::channel::<mcp::UiCommand>();
        let (resp_tx, resp_rx) = mpsc::channel::<mcp::UiCommandResult>();
        self.mcp_cmd_rx = Some(cmd_rx);
        self.mcp_resp_tx = Some(resp_tx);
        let mut state = mcp::McpState::new();
        state.allow_paths = cfg.mcp_allow_paths.clone();
        state.allow_write = cfg.mcp_allow_write;
        state.allow_export = cfg.mcp_allow_export;
        state.read_only = cfg.mcp_read_only;
        let bridge = mcp::UiBridge::new(cmd_tx, resp_rx);
        std::thread::spawn(move || {
            let server = mcp::McpServer::new(state, bridge);
            if let Some(addr) = http_addr {
                let _ = server.run_http(&addr);
            } else {
                let _ = server.run_stdio();
            }
        });
    }

    fn start_mcp_from_ui(&mut self) {
        if self.mcp_cmd_rx.is_some() {
            return;
        }
        let mut cfg = self.startup.cfg.clone();
        cfg.mcp_stdio = true;
        if cfg.mcp_allow_paths.is_empty() {
            if let Some(root) = self.root.clone() {
                cfg.mcp_allow_paths = vec![root];
            }
        }
        self.setup_mcp_server(&cfg);
    }

    fn start_mcp_http_from_ui(&mut self) {
        if self.mcp_cmd_rx.is_some() {
            return;
        }
        let mut cfg = self.startup.cfg.clone();
        cfg.mcp_http_addr = Some(mcp::DEFAULT_HTTP_ADDR.to_string());
        if cfg.mcp_allow_paths.is_empty() {
            if let Some(root) = self.root.clone() {
                cfg.mcp_allow_paths = vec![root];
            }
        }
        self.setup_mcp_server(&cfg);
    }

}
// moved to types.rs

impl WavesPreviewer {
    fn build_app(startup: StartupConfig, audio: AudioEngine) -> Self {
        // Disable loop in list view at startup.
        audio.set_loop_enabled(false);
        let ipc_rx = startup.ipc_rx.clone();
        let startup_state = StartupState::new(startup.clone());
        let debug_state = DebugState::new(startup.debug.clone());
        let mut app = Self {
            audio,
            root: None,
            items: Vec::new(),
            item_index: HashMap::new(),
            path_index: HashMap::new(),
            files: Vec::new(),
            next_media_id: 1,
            selected: None,
            volume_db: -12.0,
            playback_rate: 1.0,
            pitch_semitones: 0.0,
            meter_db: -80.0,
            tabs: Vec::new(),
            active_tab: None,
            meta_rx: None,
            meta_pool: None,
            meta_inflight: HashSet::new(),
            transcript_inflight: HashSet::new(),
            show_transcript_window: false,
            pending_transcript_seek: None,
            external_sources: Vec::new(),
            external_active_source: None,
            external_source: None,
            external_headers: Vec::new(),
            external_rows: Vec::new(),
            external_key_index: None,
            external_key_rule: ExternalKeyRule::FileName,
            external_match_input: ExternalRegexInput::FileName,
            external_visible_columns: Vec::new(),
            external_lookup: HashMap::new(),
            external_key_row_index: HashMap::new(),
            external_match_count: 0,
            external_unmatched_count: 0,
            external_show_unmatched: false,
            external_unmatched_rows: Vec::new(),
            external_sheet_names: Vec::new(),
            external_sheet_selected: None,
            external_has_header: true,
            external_header_row: None,
            external_data_row: None,
            external_scope_regex: String::new(),
            external_settings_dirty: false,
            show_external_dialog: false,
            external_load_error: None,
            external_match_regex: String::new(),
            external_match_replace: String::new(),
            external_load_rx: None,
            external_load_inflight: false,
            external_load_rows: 0,
            external_load_started_at: None,
            external_load_path: None,
            external_load_target: None,
            external_load_queue: VecDeque::new(),
            tool_defs: Vec::new(),
            tool_queue: std::collections::VecDeque::new(),
            tool_run_rx: None,
            tool_worker_busy: false,
            tool_log: std::collections::VecDeque::new(),
            tool_log_max: 200,
            show_tool_palette: false,
            tool_search: String::new(),
            tool_selected: None,
            tool_args_overrides: HashMap::new(),
            tool_config_error: None,
            pending_tool_confirm: None,
            spectro_cache: HashMap::new(),
            spectro_inflight: HashSet::new(),
            spectro_progress: HashMap::new(),
            spectro_cancel: HashMap::new(),
            spectro_cache_order: VecDeque::new(),
            spectro_cache_sizes: HashMap::new(),
            spectro_cache_bytes: 0,
            spectro_cfg: SpectrogramConfig::default(),
            spectro_tx: None,
            spectro_rx: None,
            scan_rx: None,
            scan_in_progress: false,
            scan_started_at: None,
            scan_found_count: 0,
            wave_row_h: 26.0,
            list_columns: ListColumnConfig::default(),
            selected_multi: std::collections::BTreeSet::new(),
            select_anchor: None,
            clipboard_payload: None,
            clipboard_temp_files: Vec::new(),
            clipboard_c_was_down: false,
            clipboard_v_was_down: false,
            undo_z_was_down: false,
            list_undo_stack: Vec::new(),
            list_redo_stack: Vec::new(),
            last_undo_scope: UndoScope::Editor,
            sort_key: SortKey::File,
            sort_dir: SortDir::None,
            scroll_to_selected: false,
            last_list_scroll_at: None,
            auto_play_list_nav: false,
            suppress_list_enter: false,
            list_has_focus: false,
            search_has_focus: false,
            original_files: Vec::new(),
            search_query: String::new(),
            search_use_regex: false,
            search_dirty: false,
            search_deadline: None,
            skip_dotfiles: true,
            zero_cross_epsilon: 1.0e-4,
            mode: RateMode::Speed,
            processing: None,
            list_preview_rx: None,
            list_preview_job_id: 0,
            editor_apply_state: None,
            editor_decode_state: None,
            editor_decode_job_id: 0,
            edited_cache: HashMap::new(),
            export_state: None,
            csv_export_state: None,
            playing_path: None,

            export_cfg: ExportConfig {
                first_prompt: true,
                save_mode: SaveMode::NewFile,
                dest_folder: None,
                name_template: "{name} (gain{gain:+.1}dB)".into(),
                conflict: ConflictPolicy::Rename,
                backup_bak: true,
            },
            show_export_settings: false,
            show_first_save_prompt: false,
            project_path: None,
            project_open_pending: None,
            project_open_state: None,
            theme_mode: ThemeMode::Dark,
            show_rename_dialog: false,
            rename_target: None,
            rename_input: String::new(),
            rename_error: None,
            show_batch_rename_dialog: false,
            batch_rename_targets: Vec::new(),
            batch_rename_pattern: "{name}_{n}".into(),
            batch_rename_start: 1,
            batch_rename_pad: 2,
            batch_rename_error: None,
            saving_sources: Vec::new(),
            saving_virtual: Vec::new(),
            saving_edit_sources: Vec::new(),
            saving_mode: None,

            lufs_override: HashMap::new(),
            lufs_recalc_deadline: HashMap::new(),
            lufs_rx2: None,
            lufs_worker_busy: false,
            sample_rate_override: HashMap::new(),
            show_resample_dialog: false,
            resample_targets: Vec::new(),
            resample_target_sr: 48_000,
            resample_error: None,
            bulk_resample_state: None,
            leave_intent: None,
            show_leave_prompt: false,
            pending_activate_path: None,
            heavy_preview_rx: None,
            heavy_preview_tool: None,
            heavy_overlay_rx: None,
            overlay_gen_counter: 0,
            overlay_expected_gen: 0,
            overlay_expected_tool: None,

            startup: startup_state,
            pending_screenshot: None,
            exit_after_screenshot: false,
            screenshot_seq: 0,

            debug: debug_state,
            debug_summary_seq: 0,
            ipc_rx,
            mcp_cmd_rx: None,
            mcp_resp_tx: None,
            #[cfg(feature = "kittest")]
            test_dialogs: TestDialogQueue::default(),
        };
        app.load_prefs();
        app.load_tools_config();
        app.apply_startup_paths();
        app.setup_debug_automation();
        app.setup_mcp_server(&startup);
        app
    }

    fn estimate_state_bytes(tab: &EditorTab) -> usize {
        let sample_bytes =
            tab.ch_samples.iter().map(|c| c.len()).sum::<usize>() * std::mem::size_of::<f32>();
        sample_bytes.saturating_add(256)
    }

    fn capture_undo_state(tab: &EditorTab) -> EditorUndoState {
        let approx_bytes = Self::estimate_state_bytes(tab);
        EditorUndoState {
            ch_samples: tab.ch_samples.clone(),
            samples_len: tab.samples_len,
            view_offset: tab.view_offset,
            samples_per_px: tab.samples_per_px,
            selection: tab.selection,
            ab_loop: tab.ab_loop,
            loop_region: tab.loop_region,
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
            show_waveform_overlay: tab.show_waveform_overlay,
            dirty: tab.dirty,
            approx_bytes,
            markers: tab.markers.clone(),
            markers_committed: tab.markers_committed.clone(),
            markers_applied: tab.markers_applied.clone(),
            loop_region_applied: tab.loop_region_applied,
            loop_region_committed: tab.loop_region_committed,
        }
    }

    fn push_state_to_stack(
        stack: &mut Vec<EditorUndoState>,
        bytes: &mut usize,
        state: EditorUndoState,
    ) {
        *bytes = bytes.saturating_add(state.approx_bytes);
        stack.push(state);
        while stack.len() > UNDO_STACK_LIMIT || *bytes > UNDO_STACK_MAX_BYTES {
            if stack.is_empty() {
                break;
            }
            let removed = stack.remove(0);
            *bytes = bytes.saturating_sub(removed.approx_bytes);
        }
    }

    fn pop_state_from_stack(
        stack: &mut Vec<EditorUndoState>,
        bytes: &mut usize,
    ) -> Option<EditorUndoState> {
        let state = stack.pop();
        if let Some(st) = &state {
            *bytes = bytes.saturating_sub(st.approx_bytes);
        }
        state
    }

    fn push_editor_undo_state(
        &mut self,
        tab_idx: usize,
        state: EditorUndoState,
        clear_redo: bool,
    ) {
        self.last_undo_scope = UndoScope::Editor;
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            Self::push_undo_state_from(tab, state, clear_redo);
        }
    }

    fn push_undo_state_from(tab: &mut EditorTab, state: EditorUndoState, clear_redo: bool) {
        if clear_redo {
            tab.redo_stack.clear();
            tab.redo_bytes = 0;
        }
        Self::push_state_to_stack(&mut tab.undo_stack, &mut tab.undo_bytes, state);
    }

    fn push_redo_state(tab: &mut EditorTab, state: EditorUndoState) {
        Self::push_state_to_stack(&mut tab.redo_stack, &mut tab.redo_bytes, state);
    }

    fn restore_state_in_tab(&mut self, tab_idx: usize, state: EditorUndoState) -> bool {
        {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return false;
            };
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
            tab.ch_samples = state.ch_samples;
            tab.samples_len = state.samples_len;
            tab.view_offset = state.view_offset;
            tab.samples_per_px = state.samples_per_px;
            tab.selection = state.selection;
            tab.ab_loop = state.ab_loop;
            tab.loop_region = state.loop_region;
            tab.trim_range = state.trim_range;
            tab.loop_xfade_samples = state.loop_xfade_samples;
            tab.loop_xfade_shape = state.loop_xfade_shape;
            tab.fade_in_range = state.fade_in_range;
            tab.fade_out_range = state.fade_out_range;
            tab.fade_in_shape = state.fade_in_shape;
            tab.fade_out_shape = state.fade_out_shape;
            tab.loop_mode = state.loop_mode;
            tab.snap_zero_cross = state.snap_zero_cross;
            tab.tool_state = state.tool_state;
            tab.active_tool = state.active_tool;
            tab.show_waveform_overlay = state.show_waveform_overlay;
            tab.markers = state.markers;
            tab.markers_committed = state.markers_committed;
            tab.markers_applied = state.markers_applied;
            tab.loop_region_applied = state.loop_region_applied;
            tab.loop_region_committed = state.loop_region_committed;
            tab.drag_select_anchor = None;
            tab.dragging_marker = None;
            tab.preview_offset_samples = None;
            tab.dirty = state.dirty;
            Self::update_loop_markers_dirty(tab);
        }
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        self.audio.stop();
        self.audio.set_samples_channels(tab.ch_samples.clone());
        self.apply_loop_mode_for_tab(tab);
        true
    }

    fn undo_in_tab(&mut self, tab_idx: usize) -> bool {
        let (undo_state, redo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return false;
            };
            let undo_state = Self::pop_state_from_stack(&mut tab.undo_stack, &mut tab.undo_bytes);
            let Some(undo_state) = undo_state else {
                return false;
            };
            let redo_state = Self::capture_undo_state(tab);
            (undo_state, redo_state)
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            Self::push_redo_state(tab, redo_state);
        }
        self.restore_state_in_tab(tab_idx, undo_state)
    }

    fn redo_in_tab(&mut self, tab_idx: usize) -> bool {
        let (redo_state, undo_state) = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return false;
            };
            let redo_state = Self::pop_state_from_stack(&mut tab.redo_stack, &mut tab.redo_bytes);
            let Some(redo_state) = redo_state else {
                return false;
            };
            let undo_state = Self::capture_undo_state(tab);
            (redo_state, undo_state)
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            Self::push_undo_state_from(tab, undo_state, false);
        }
        self.restore_state_in_tab(tab_idx, redo_state)
    }

    pub fn new(cc: &eframe::CreationContext<'_>, startup: StartupConfig) -> Result<Self> {
        Self::init_egui_style(&cc.egui_ctx);
        let audio = AudioEngine::new()?;
        let app = Self::build_app(startup, audio);
        Self::apply_theme_visuals(&cc.egui_ctx, app.theme_mode);
        Ok(app)
    }

    #[cfg(any(test, feature = "kittest"))]
    pub fn new_for_test(cc: &eframe::CreationContext<'_>, startup: StartupConfig) -> Result<Self> {
        Self::init_egui_style(&cc.egui_ctx);
        let audio = AudioEngine::new_for_test();
        let app = Self::build_app(startup, audio);
        Self::apply_theme_visuals(&cc.egui_ctx, app.theme_mode);
        Ok(app)
    }

}

impl eframe::App for WavesPreviewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.suppress_list_enter = false;
        if ctx.dragged_id().is_some() && !ctx.input(|i| i.pointer.any_down()) {
            if self.debug.cfg.enabled {
                self.debug_trace_input("force stop_dragging (pointer released outside)");
            }
            ctx.stop_dragging();
        }
        self.ensure_theme_visuals(ctx);
        self.tick_project_open();
        // Update meter from audio RMS (approximate dBFS)
        {
            let rms = self
                .audio
                .shared
                .meter_rms
                .load(std::sync::atomic::Ordering::Relaxed);
            let db = if rms > 0.0 {
                20.0 * rms.max(1e-8).log10()
            } else {
                -80.0
            };
            self.meter_db = db.clamp(-80.0, 6.0);
        }
        // Ensure effective volume (global vol x per-file gain) is always applied
        self.apply_effective_volume();
        // Drain scan results (background folder scan)
        self.process_scan_messages();
        self.process_ipc_requests();
        self.process_mcp_commands(ctx);
        self.apply_pending_transcript_seek();
        self.process_tool_results();
        self.process_tool_queue();
        // Debounced search apply (avoid per-keystroke full scan)
        self.apply_search_if_due();
        // Handle screenshot results from the backend
        self.handle_screenshot_events(ctx);
        // Manual screenshot trigger (F9)
        if ctx.input(|i| i.key_pressed(Key::F9)) {
            let path = self.default_screenshot_path();
            self.request_screenshot(ctx, path, false);
        }
        // Startup automation (open first file, auto screenshot)
        self.run_startup_actions(ctx);
        // Debug automation + checks
        self.debug_tick(ctx);
        // Drain heavy preview results
        self.drain_heavy_preview_results();
        // Drain list preview full-load results
        self.drain_list_preview_results();
        self.drain_editor_decode();
        // Drain heavy per-channel overlay results
        self.drain_heavy_overlay_results();
        // Drain editor apply jobs (pitch/stretch)
        self.drain_editor_apply_jobs(ctx);
        // Drain metadata updates
        self.drain_meta_updates(ctx);
        self.drain_external_load_results(ctx);
        self.check_csv_export_completion();
        self.tick_bulk_resample();
        if self.bulk_resample_state.is_some() {
            ctx.request_repaint();
        }
        // Drain spectrogram jobs (tiled)
        self.apply_spectrogram_updates(ctx);

        // Drain export results
        self.drain_export_results(ctx);

        // Drain LUFS (with gain) recompute results
        self.drain_lufs_recalc_results();

        // Pump LUFS recompute worker (debounced)
        self.pump_lufs_recalc_worker();

        // Drain heavy processing result
        self.tick_processing_state(ctx);

        // Top controls (always visible)
        self.ui_top_bar(ctx);
        // Drag & Drop: merge dropped files/folders into the list (supported audio)
        self.handle_dropped_files(ctx);
        let mut activate_path: Option<PathBuf> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            // Tabs
            ui.horizontal_wrapped(|ui| {
                let is_list = self.active_tab.is_none();
                let list_label = if is_list {
                    RichText::new("[List]").strong()
                } else {
                    RichText::new("List")
                };
                if ui.selectable_label(is_list, list_label).clicked() {
                    if let Some(idx) = self.active_tab {
                        self.clear_preview_if_any(idx);
                    }
                    self.active_tab = None;
                    self.audio.stop();
                    self.audio.set_loop_enabled(false);
                    self.request_list_focus(ctx);
                }
                let mut to_close: Option<usize> = None;
                let tabs_len = self.tabs.len();
                for i in 0..tabs_len {
                    // avoid holding immutable borrow over calls that mutate self inside closure
                    let active = self.active_tab == Some(i);
                    let tab = &self.tabs[i];
                    let mut display = tab.display_name.clone();
                    if tab.dirty || tab.loop_markers_dirty || tab.markers_dirty {
                        display = format!("\u{25CF} {display}");
                    }
                    let path_for_activate = tab.path.clone();
                    let text = if active {
                        RichText::new(format!("[{}]", display)).strong()
                    } else {
                        RichText::new(display)
                    };
                    ui.horizontal(|ui| {
                        if ui.selectable_label(active, text).clicked() {
                            // Leaving previous tab: discard any un-applied preview
                            if let Some(prev) = self.active_tab {
                                if prev != i {
                                    self.clear_preview_if_any(prev);
                                }
                            }
                            // mutate self safely here
                            self.active_tab = Some(i);
                            activate_path = Some(path_for_activate.clone());
                            self.audio.stop();
                        }
                        if ui.button("x").on_hover_text("Close").clicked() {
                            self.clear_preview_if_any(i);
                            to_close = Some(i);
                            self.audio.stop();
                        }
                    });
                }
                if let Some(i) = to_close {
                    self.close_tab_at(i, ctx);
                }
            });
            ui.separator();
            if let Some(tab_idx) = self.active_tab {
                self.ui_editor_view(ui, ctx, tab_idx);
            } else {
                // List view
                // extracted implementation:
                {
                    self.ui_list_view(ui, ctx);
                }
                // legacy path kept under an always-false guard for transition
                if false {
                    let mut to_open: Option<PathBuf> = None;
                    let text_height = egui::TextStyle::Body.resolve(ui.style()).size;
                    let header_h = text_height * 1.6;
                    let row_h = self.wave_row_h.max(text_height * 1.3);
                    let avail_h = ui.available_height();
                    // Build table directly; size the scrolled body to fill remaining height
                    // Also expand to full width so the scroll bar is at the right edge
                    ui.set_min_width(ui.available_width());
                    let mut sort_changed = false;
                    let table = TableBuilder::new(ui)
                        .striped(true)
                        .resizable(true)
                        .sense(egui::Sense::click())
                        .cell_layout(egui::Layout::left_to_right(Align::Center))
                        .column(egui_extras::Column::initial(200.0).resizable(true)) // File (resizable)
                        .column(egui_extras::Column::initial(250.0).resizable(true)) // Folder (resizable)
                        .column(egui_extras::Column::initial(60.0).resizable(true)) // Length (resizable)
                        .column(egui_extras::Column::initial(40.0).resizable(true)) // Ch (resizable)
                        .column(egui_extras::Column::initial(70.0).resizable(true)) // SampleRate (resizable)
                        .column(egui_extras::Column::initial(50.0).resizable(true)) // Bits (resizable)
                        .column(egui_extras::Column::initial(90.0).resizable(true)) // Level (original)
                        .column(egui_extras::Column::initial(90.0).resizable(true)) // LUFS (Integrated)
                        .column(egui_extras::Column::initial(80.0).resizable(true)) // Gain (editable)
                        .column(egui_extras::Column::initial(150.0).resizable(true)) // Wave (resizable)
                        .column(egui_extras::Column::remainder()) // Spacer (fills remainder)
                        .min_scrolled_height((avail_h - header_h).max(0.0));

                    table
                        .header(header_h, |mut header| {
                            header.col(|ui| {
                                sort_changed |= sortable_header(
                                    ui,
                                    "File",
                                    &mut self.sort_key,
                                    &mut self.sort_dir,
                                    SortKey::File,
                                    true,
                                );
                            });
                            header.col(|ui| {
                                sort_changed |= sortable_header(
                                    ui,
                                    "Folder",
                                    &mut self.sort_key,
                                    &mut self.sort_dir,
                                    SortKey::Folder,
                                    true,
                                );
                            });
                            header.col(|ui| {
                                sort_changed |= sortable_header(
                                    ui,
                                    "Length",
                                    &mut self.sort_key,
                                    &mut self.sort_dir,
                                    SortKey::Length,
                                    true,
                                );
                            });
                            header.col(|ui| {
                                sort_changed |= sortable_header(
                                    ui,
                                    "Ch",
                                    &mut self.sort_key,
                                    &mut self.sort_dir,
                                    SortKey::Channels,
                                    true,
                                );
                            });
                            header.col(|ui| {
                                sort_changed |= sortable_header(
                                    ui,
                                    "SR",
                                    &mut self.sort_key,
                                    &mut self.sort_dir,
                                    SortKey::SampleRate,
                                    true,
                                );
                            });
                            header.col(|ui| {
                                sort_changed |= sortable_header(
                                    ui,
                                    "Bits",
                                    &mut self.sort_key,
                                    &mut self.sort_dir,
                                    SortKey::Bits,
                                    true,
                                );
                            });
                            header.col(|ui| {
                                sort_changed |= sortable_header(
                                    ui,
                                    "dBFS (Peak)",
                                    &mut self.sort_key,
                                    &mut self.sort_dir,
                                    SortKey::Level,
                                    false,
                                );
                            });
                            header.col(|ui| {
                                sort_changed |= sortable_header(
                                    ui,
                                    "LUFS (I)",
                                    &mut self.sort_key,
                                    &mut self.sort_dir,
                                    SortKey::Lufs,
                                    false,
                                );
                            });
                            header.col(|ui| {
                                ui.label(RichText::new("Gain (dB)").strong());
                            });
                            header.col(|ui| {
                                ui.label(RichText::new("Wave").strong());
                            });
                            header.col(|_ui| { /* spacer */ });
                        })
                        .body(|body| {
                            let data_len = self.files.len();
                            // Ensure the table body fills the remaining height
                            let min_rows_for_height =
                                ((avail_h - header_h).max(0.0) / row_h).ceil() as usize;
                            let total_rows = data_len.max(min_rows_for_height);

                            // Use virtualized rows for performance with large lists
                            body.rows(row_h, total_rows, |mut row| {
                                let row_idx = row.index();
                                let is_data = row_idx < data_len;
                                let is_selected = self.selected_multi.contains(&row_idx);
                                row.set_selected(is_selected);

                                if is_data {
                                    let Some(path_owned) = self.path_for_row(row_idx).cloned()
                                    else {
                                        return;
                                    };
                                    let name = path_owned
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("(invalid)");
                                    let parent =
                                        path_owned.parent().and_then(|p| p.to_str()).unwrap_or("");
                                    let mut clicked_to_load = false;
                                    let mut clicked_to_select = false;
                                    // Ensure quick header meta is present when row is shown
                                    if self.meta_for_path(&path_owned).is_none() {
                                        if let Ok(info) =
                                            crate::audio_io::read_audio_info(&path_owned)
                                        {
                                            let _ = self.set_meta_for_path(
                                                &path_owned,
                                                FileMeta {
                                                    channels: info.channels,
                                                    sample_rate: info.sample_rate,
                                                    bits_per_sample: info.bits_per_sample,
                                                    bit_rate_bps: info.bit_rate_bps,
                                                    duration_secs: info.duration_secs,
                                                    rms_db: None,
                                                    peak_db: None,
                                                    lufs_i: None,
                                                    bpm: crate::audio_io::read_audio_bpm(&path_owned),
                                                    created_at: info.created_at,
                                                    modified_at: info.modified_at,
                                                    thumb: Vec::new(),
                                                    decode_error: None,
                                                },
                                            );
                                        }
                                    }
                                    let meta = self.meta_for_path(&path_owned).cloned();

                                    // col 0: File (clickable label with clipping)
                                    row.col(|ui| {
                                        ui.with_layout(
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                                let mark = if self.has_pending_gain(&path_owned) {
                                                    " ?"
                                                } else {
                                                    ""
                                                };
                                                let resp = ui
                                                    .add(
                                                        egui::Label::new(
                                                            RichText::new(format!(
                                                                "{}{}",
                                                                name, mark
                                                            ))
                                                            .size(text_height * 1.05),
                                                        )
                                                        .sense(Sense::click())
                                                        .truncate()
                                                        .show_tooltip_when_elided(false),
                                                    )
                                                    .on_hover_cursor(
                                                        egui::CursorIcon::PointingHand,
                                                    );

                                                // NOTE: invalid-encoding comment removed

                                                // NOTE: invalid-encoding comment removed
                                                if resp.double_clicked() {
                                                    clicked_to_select = true;
                                                    to_open = Some(path_owned.clone());
                                                }

                                                if resp.hovered() {
                                                    resp.on_hover_text(name);
                                                }
                                            },
                                        );
                                    });
                                    // col 1: Folder (clickable label with clipping)
                                    row.col(|ui| {
                                        ui.with_layout(
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                                let resp = ui
                                                    .add(
                                                        egui::Label::new(
                                                            RichText::new(parent)
                                                                .monospace()
                                                                .size(text_height * 1.0),
                                                        )
                                                        .sense(Sense::click())
                                                        .truncate()
                                                        .show_tooltip_when_elided(false),
                                                    )
                                                    .on_hover_cursor(
                                                        egui::CursorIcon::PointingHand,
                                                    );

                                                // NOTE: invalid-encoding comment removed

                                                // NOTE: invalid-encoding comment removed

                                                if resp.hovered() {
                                                    resp.on_hover_text(parent);
                                                }
                                            },
                                        );
                                    });
                                    // col 2: Length (mm:ss) - clickable
                                    row.col(|ui| {
                                        let secs = meta
                                            .as_ref()
                                            .and_then(|m| m.duration_secs)
                                            .unwrap_or(f32::NAN);
                                        let text = if secs.is_finite() {
                                            format_duration(secs)
                                        } else {
                                            "...".into()
                                        };
                                        let resp = ui
                                            .add(
                                                egui::Label::new(RichText::new(text).monospace())
                                                    .sense(Sense::click()),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                                        if resp.clicked() {
                                            clicked_to_load = true;
                                        }
                                    });
                                    // col 3: Channels - clickable
                                    row.col(|ui| {
                                        let ch = meta.as_ref().map(|m| m.channels).unwrap_or(0);
                                        let resp = ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(format!("{}", ch)).monospace(),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                                        if resp.clicked() {
                                            clicked_to_load = true;
                                        }
                                    });
                                    // col 4: Sample rate - clickable
                                    row.col(|ui| {
                                        let sr = meta.as_ref().map(|m| m.sample_rate).unwrap_or(0);
                                        let resp = ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(format!("{}", sr)).monospace(),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                                        if resp.clicked() {
                                            clicked_to_load = true;
                                        }
                                    });
                                    // col 5: Bits per sample - clickable
                                    row.col(|ui| {
                                        let bits =
                                            meta.as_ref().map(|m| m.bits_per_sample).unwrap_or(0);
                                        let resp = ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(format!("{}", bits)).monospace(),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                                        if resp.clicked() {
                                            clicked_to_load = true;
                                        }
                                    });
                                    // NOTE: invalid-encoding comment removed
                                    row.col(|ui| {
                                        let (rect2, resp2) = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::click(),
                                        );
                                        let gain_db = self.pending_gain_db_for_path(&path_owned);
                                        let orig = meta.as_ref().and_then(|m| m.peak_db);
                                        let adj = orig.map(|db| db + gain_db);
                                        if let Some(db) = adj {
                                            ui.painter().rect_filled(rect2, 4.0, db_to_color(db));
                                        }
                                        let text = adj
                                            .map(|db| format!("{:.1}", db))
                                            .unwrap_or_else(|| "...".into());
                                        let fid = TextStyle::Monospace.resolve(ui.style());
                                        ui.painter().text(
                                            rect2.center(),
                                            egui::Align2::CENTER_CENTER,
                                            text,
                                            fid,
                                            Color32::WHITE,
                                        );
                                        if resp2.clicked() {
                                            clicked_to_load = true;
                                        }
                                        // (optional tooltip removed to avoid borrow and unused warnings)
                                    });
                                    // col 7: LUFS (Integrated) with background color (same palette as dBFS)
                                    row.col(|ui| {
                                        let base = meta.as_ref().and_then(|m| m.lufs_i);
                                        let gain_db = self.pending_gain_db_for_path(&path_owned);
                                        let eff =
                                            if let Some(v) = self.lufs_override.get(&path_owned) {
                                                Some(*v)
                                            } else {
                                                base.map(|v| v + gain_db)
                                            };
                                        let (rect2, resp2) = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::click(),
                                        );
                                        if let Some(db) = eff {
                                            ui.painter().rect_filled(rect2, 4.0, db_to_color(db));
                                        }
                                        let text = eff
                                            .map(|v| format!("{:.1}", v))
                                            .unwrap_or_else(|| "...".into());
                                        let fid = TextStyle::Monospace.resolve(ui.style());
                                        ui.painter().text(
                                            rect2.center(),
                                            egui::Align2::CENTER_CENTER,
                                            text,
                                            fid,
                                            Color32::WHITE,
                                        );
                                        if resp2.clicked() {
                                            clicked_to_load = true;
                                        }
                                    });
                                    // col 8: Gain (dB) editable
                                    row.col(|ui| {
                                        let old = self.pending_gain_db_for_path(&path_owned);
                                        let mut g = old;
                                        if !g.is_finite() {
                                            g = 0.0;
                                        }
                                        let resp = ui.add(
                                            egui::DragValue::new(&mut g)
                                                .range(-24.0..=24.0)
                                                .speed(0.1)
                                                .fixed_decimals(1)
                                                .suffix(" dB"),
                                        );
                                        if resp.changed() {
                                            let new = Self::clamp_gain_db(g);
                                            let delta = new - old;
                                            if self.selected_multi.len() > 1
                                                && self.selected_multi.contains(&row_idx)
                                            {
                                                let indices = self.selected_multi.clone();
                                                self.adjust_gain_for_indices(&indices, delta);
                                            } else {
                                                self.set_pending_gain_db_for_path(&path_owned, new);
                                                if self.playing_path.as_ref() == Some(&path_owned) {
                                                    self.apply_effective_volume();
                                                }
                                            }
                                            // schedule LUFS recompute (debounced)
                                            self.schedule_lufs_for_path(path_owned.clone());
                                        }
                                    });
                                    // col 9: Wave thumbnail - clickable
                                    row.col(|ui| {
                                        let desired_w = ui.available_width().max(80.0);
                                        let thumb_h = (desired_w * 0.22)
                                            .clamp(text_height * 1.2, text_height * 4.0);
                                        let (rect, painter) = ui.allocate_painter(
                                            egui::vec2(desired_w, thumb_h),
                                            Sense::click(),
                                        );
                                        if row_idx == 0 {
                                            self.wave_row_h = thumb_h;
                                        }
                                        if let Some(m) = meta.as_ref() {
                                            let w = rect.rect.width();
                                            let h = rect.rect.height();
                                            let n = m.thumb.len().max(1) as f32;
                                            let gain_db =
                                                self.pending_gain_db_for_path(&path_owned);
                                            let scale = db_to_amp(gain_db);
                                            for (idx, &(mn0, mx0)) in m.thumb.iter().enumerate() {
                                                let mn = (mn0 * scale).clamp(-1.0, 1.0);
                                                let mx = (mx0 * scale).clamp(-1.0, 1.0);
                                                let x = rect.rect.left() + (idx as f32 / n) * w;
                                                let y0 = rect.rect.center().y - mx * (h * 0.45);
                                                let y1 = rect.rect.center().y - mn * (h * 0.45);
                                                let a = (mn.abs().max(mx.abs())).clamp(0.0, 1.0);
                                                let col = amp_to_color(a);
                                                painter.line_segment(
                                                    [
                                                        egui::pos2(x, y0.min(y1)),
                                                        egui::pos2(x, y0.max(y1)),
                                                    ],
                                                    egui::Stroke::new(1.0, col),
                                                );
                                            }
                                        }
                                        if rect.clicked() {
                                            clicked_to_load = true;
                                        }
                                    });
                                    // col 10: Spacer (fills remainder so scrollbar stays at right edge)
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    });

                                    // Row-level click handling (background/any non-interactive area)
                                    let resp = row.response();
                                    if resp.clicked() {
                                        clicked_to_load = true;
                                    }
                                    if is_selected && self.scroll_to_selected {
                                        resp.scroll_to_me(Some(Align::Center));
                                    }
                                    if clicked_to_load {
                                        // multi-select aware selection update (read modifiers from ctx to avoid UI borrow conflict)
                                        let mods = ctx.input(|i| i.modifiers);
                                        self.update_selection_on_click(row_idx, mods);
                                        // load clicked row regardless of modifiers
                                        self.select_and_load(row_idx, true);
                                    } else if clicked_to_select {
                                        self.selected = Some(row_idx);
                                        self.scroll_to_selected = false;
                                        self.selected_multi.clear();
                                        self.selected_multi.insert(row_idx);
                                        self.select_anchor = Some(row_idx);
                                    }
                                } else {
                                    // filler row to extend frame
                                    row.col(|_ui| {});
                                    row.col(|_ui| {});
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    }); // Length
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    }); // Ch
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    }); // SR
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    }); // Bits
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    }); // Level
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    }); // LUFS
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    }); // Gain
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    }); // Wave
                                    row.col(|ui| {
                                        let _ = ui.allocate_exact_size(
                                            egui::vec2(ui.available_width(), row_h * 0.9),
                                            Sense::hover(),
                                        );
                                    }); // Spacer
                                }
                            });
                        });
                    if sort_changed {
                        self.apply_sort();
                    }
                    if let Some(p) = to_open.as_ref() {
                        self.open_or_activate_tab(p);
                    }
                    // moved to ui_list_view; do not draw here to avoid stray text
                    // if self.files.is_empty() { ui.label("Select a folder to show list"); }
                }
            }
        });
        // When switching tabs, ensure the active tab's audio is loaded and loop state applied.
        let mut activated_tab_idx: Option<usize> = None;
        if activate_path.is_none() {
            if let Some(pending) = self.pending_activate_path.take() {
                activate_path = Some(pending);
            }
        }
        if let Some(p) = activate_path {
            if !self.apply_dirty_tab_audio_with_mode(&p) {
                // Reload audio for the activated tab only; do not touch stored waveform
                match self.mode {
                    RateMode::Speed => {
                        let _ =
                            prepare_for_speed(&p, &self.audio, &mut Vec::new(), self.playback_rate);
                        self.audio.set_rate(self.playback_rate);
                    }
                    _ => {
                        self.audio.set_rate(1.0);
                        self.spawn_heavy_processing(&p);
                    }
                }
                if let Some(idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get(idx) {
                        self.apply_loop_mode_for_tab(tab);
                    }
                }
                // Update effective volume to include per-file gain for the activated tab
                self.apply_effective_volume();
            }
            activated_tab_idx = self.active_tab;
        }
        if let Some(tab_idx) = activated_tab_idx {
            self.refresh_tool_preview_for_tab(tab_idx);
        }
        // List auto-scroll flag is cleared by list view when consumed.

        if let Some(tab_idx) = self.active_tab {
            self.queue_spectrogram_for_tab(tab_idx);
        }

        // Busy overlay (only for blocking operations like export/apply)
        self.ui_busy_overlay(ctx);
        ctx.request_repaint_after(Duration::from_millis(16));

        // Leave dirty editor confirmation
        if self.show_leave_prompt {
            egui::Window::new("Leave Editor?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label("The waveform has been modified in memory. Leave this editor?");
                    ui.horizontal(|ui| {
                        if ui.button("Leave").clicked() {
                            match self.leave_intent.take() {
                                Some(LeaveIntent::CloseTab(i)) => {
                                    if i < self.tabs.len() {
                                        self.close_tab_at(i, ctx);
                                    }
                                }
                                Some(LeaveIntent::ToTab(i)) => {
                                    if let Some(t) = self.tabs.get(i) {
                                        self.active_tab = Some(i);
                                        self.audio.stop();
                                        self.pending_activate_path = Some(t.path.clone());
                                    }
                                    self.rebuild_current_buffer_with_mode();
                                }
                                Some(LeaveIntent::ToList) => {
                                    self.active_tab = None;
                                    self.audio.stop();
                                    self.audio.set_loop_enabled(false);
                                    self.request_list_focus(ctx);
                                }
                                None => {}
                            }
                            self.show_leave_prompt = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.leave_intent = None;
                            self.show_leave_prompt = false;
                        }
                    });
                });
        }

        // First save prompt window
        if self.show_first_save_prompt {
            egui::Window::new("First Export Option")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label("Choose default export behavior for Ctrl+E:");
                    ui.horizontal(|ui| {
                        if ui.button("Overwrite").clicked() {
                            self.export_cfg.save_mode = SaveMode::Overwrite;
                            self.export_cfg.first_prompt = false;
                            self.show_first_save_prompt = false;
                            self.trigger_save_selected();
                        }
                        if ui.button("New File").clicked() {
                            self.export_cfg.save_mode = SaveMode::NewFile;
                            self.export_cfg.first_prompt = false;
                            self.show_first_save_prompt = false;
                            self.trigger_save_selected();
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_first_save_prompt = false;
                        }
                    });
                });
        }

        // Export settings window (in separate UI module)
        self.ui_export_settings_window(ctx);
        self.ui_external_data_window(ctx);
        self.ui_transcript_window(ctx);
        self.ui_tool_palette_window(ctx);
        self.ui_tool_confirm_dialog(ctx);
        // Rename dialog
        if self.show_rename_dialog {
            let mut do_rename = false;
            egui::Window::new("Rename File")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    if let Some(path) = self.rename_target.as_ref() {
                        ui.label(path.display().to_string());
                    }
                    let resp = ui.text_edit_singleline(&mut self.rename_input);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        do_rename = true;
                    }
                    if let Some(err) = self.rename_error.as_ref() {
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    }
                    ui.horizontal(|ui| {
                        let can = !self.rename_input.trim().is_empty();
                        if ui.add_enabled(can, egui::Button::new("Rename")).clicked() {
                            do_rename = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_rename_dialog = false;
                        }
                    });
                });
            if do_rename {
                let name = self.rename_input.clone();
                if let Some(path) = self.rename_target.clone() {
                    match self.rename_file_path(&path, &name) {
                        Ok(_) => {
                            self.show_rename_dialog = false;
                            self.rename_target = None;
                            self.rename_error = None;
                        }
                        Err(err) => {
                            self.rename_error = Some(err);
                        }
                    }
                } else {
                    self.show_rename_dialog = false;
                }
            }
        }
        if self.show_batch_rename_dialog {
            let mut do_rename = false;
            egui::Window::new("Batch Rename")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(format!("{} files", self.batch_rename_targets.len()));
                    ui.horizontal(|ui| {
                        ui.label("Pattern:");
                        ui.text_edit_singleline(&mut self.batch_rename_pattern);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Start:");
                        ui.add(
                            egui::DragValue::new(&mut self.batch_rename_start).range(0..=1_000_000),
                        );
                        ui.label("Zero pad:");
                        ui.add(egui::DragValue::new(&mut self.batch_rename_pad).range(0..=6));
                    });
                    ui.label("Tokens: {name} (original stem), {n} (sequence)");
                    if let Some(err) = self.batch_rename_error.as_ref() {
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    }
                    let preview_count = 4usize;
                    ui.separator();
                    ui.label("Preview:");
                    for (i, src) in self
                        .batch_rename_targets
                        .iter()
                        .take(preview_count)
                        .enumerate()
                    {
                        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        let num = self.batch_rename_start.saturating_add(i as u32);
                        let num_str = if self.batch_rename_pad > 0 {
                            format!("{:0width$}", num, width = self.batch_rename_pad as usize)
                        } else {
                            num.to_string()
                        };
                        let mut name = self
                            .batch_rename_pattern
                            .replace("{name}", stem)
                            .replace("{n}", &num_str);
                        let has_ext = std::path::Path::new(&name).extension().is_some();
                        if !has_ext {
                            if let Some(ext) = src.extension().and_then(|s| s.to_str()) {
                                name.push('.');
                                name.push_str(ext);
                            }
                        }
                        ui.label(format!("{} -> {}", src.display(), name));
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked() {
                            do_rename = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_batch_rename_dialog = false;
                            self.batch_rename_targets.clear();
                            self.batch_rename_error = None;
                        }
                    });
                });
            if do_rename {
                match self.batch_rename_paths() {
                    Ok(()) => {
                        self.show_batch_rename_dialog = false;
                        self.batch_rename_targets.clear();
                        self.batch_rename_error = None;
                    }
                    Err(err) => {
                        self.batch_rename_error = Some(err);
                    }
                }
            }
        }
        if self.show_resample_dialog {
            let mut do_apply = false;
            egui::Window::new("Sample Rate Convert")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(format!("{} files", self.resample_targets.len()));
                    ui.horizontal(|ui| {
                        ui.label("Target sample rate (Hz):");
                        ui.add(
                            egui::DragValue::new(&mut self.resample_target_sr)
                                .range(8000..=384_000)
                                .speed(100.0),
                        );
                    });
                    if let Some(err) = self.resample_error.as_ref() {
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            do_apply = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_resample_dialog = false;
                            self.resample_targets.clear();
                            self.resample_error = None;
                        }
                    });
                });
            if do_apply {
                match self.apply_resample_dialog() {
                    Ok(()) => {
                        self.show_resample_dialog = false;
                        self.resample_targets.clear();
                        self.resample_error = None;
                    }
                    Err(err) => {
                        self.resample_error = Some(err);
                    }
                }
            }
        }
        // Debug window
        self.ui_debug_window(ctx);
        // Global shortcuts after UI so focus state is accurate
        self.handle_global_shortcuts(ctx);
        // Hotkeys after UI so focus state is accurate
        self.handle_clipboard_hotkeys(ctx);
        self.handle_undo_redo_hotkeys(ctx);
    }
}
