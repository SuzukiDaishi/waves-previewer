use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::audio::AudioEngine;
use crate::mcp;
use crate::wave::{build_minmax, prepare_for_speed};
use anyhow::Result;
use egui::{
    Align, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, RichText, Sense, TextStyle,
    Visuals,
};
use egui_extras::TableBuilder;
// use walkdir::WalkDir; // unused here (used in logic.rs)

mod capture;
mod debug_ops;
mod dialogs;
mod editor_ops;
mod external;
mod external_ops;
mod helpers;
mod list_ops;
mod logic;
mod meta;
mod preview;
mod project;
mod project_ops;
mod render;
mod spectrogram;
mod startup;
mod tool_ops;
mod tooling;
mod transcript;
mod types;
mod ui;
#[cfg(feature = "kittest")]
use self::dialogs::TestDialogQueue;
use self::project_ops::ProjectOpenState;
use self::tooling::{ToolDef, ToolJob, ToolLogEntry, ToolRunResult};
pub use self::types::{StartupConfig, ViewMode};
use self::{helpers::*, types::*};

const LIVE_PREVIEW_SAMPLE_LIMIT: usize = 2_000_000;
const UNDO_STACK_LIMIT: usize = 20;
const UNDO_STACK_MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_EDITOR_TABS: usize = 12;
const SPECTRO_TILE_FRAMES: usize = 64;
const SPECTRO_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;

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
    pub external_source: Option<PathBuf>,
    pub external_headers: Vec<String>,
    pub external_rows: Vec<Vec<String>>,
    pub external_key_index: Option<usize>,
    pub external_key_rule: ExternalKeyRule,
    pub external_visible_columns: Vec<String>,
    pub external_lookup: HashMap<String, HashMap<String, String>>,
    pub external_match_count: usize,
    pub external_unmatched_count: usize,
    pub show_external_dialog: bool,
    pub external_load_error: Option<String>,
    pub external_match_regex: String,
    pub external_match_replace: String,
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
    // sorting
    sort_key: SortKey,
    sort_dir: SortDir,
    // scroll behavior
    scroll_to_selected: bool,
    last_list_scroll_at: Option<std::time::Instant>,
    auto_play_list_nav: bool,
    suppress_list_enter: bool,
    // original order snapshot for tri-state sort
    original_files: Vec<MediaId>,
    // search
    search_query: String,
    search_use_regex: bool,
    search_dirty: bool,
    search_deadline: Option<std::time::Instant>,
    // list filtering
    skip_dotfiles: bool,
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
    saving_mode: Option<SaveMode>,

    // LUFS with Gain recompute support
    lufs_override: HashMap<PathBuf, f32>,
    lufs_recalc_deadline: HashMap<PathBuf, std::time::Instant>,
    lufs_rx2: Option<std::sync::mpsc::Receiver<(PathBuf, f32)>>,
    lufs_worker_busy: bool,
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
    mcp_cmd_rx: Option<std::sync::mpsc::Receiver<crate::mcp::UiCommand>>,
    mcp_resp_tx: Option<std::sync::mpsc::Sender<crate::mcp::UiCommandResult>>,
    #[cfg(feature = "kittest")]
    test_dialogs: TestDialogQueue,
}

impl WavesPreviewer {
    fn theme_visuals(theme: ThemeMode) -> Visuals {
        let mut visuals = match theme {
            ThemeMode::Dark => Visuals::dark(),
            ThemeMode::Light => Visuals::light(),
        };
        match theme {
            ThemeMode::Dark => {
                visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(20, 20, 23);
                visuals.widgets.inactive.bg_fill = Color32::from_rgb(28, 28, 32);
                visuals.panel_fill = Color32::from_rgb(18, 18, 20);
            }
            ThemeMode::Light => {
                visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(245, 245, 248);
                visuals.widgets.inactive.bg_fill = Color32::from_rgb(235, 235, 240);
                visuals.panel_fill = Color32::from_rgb(250, 250, 252);
            }
        }
        // Remove hover brightening to avoid sluggish tracking effect
        visuals.widgets.hovered = visuals.widgets.inactive.clone();
        visuals.widgets.active = visuals.widgets.inactive.clone();
        visuals
    }

    fn apply_theme_visuals(ctx: &egui::Context, theme: ThemeMode) {
        ctx.set_visuals(Self::theme_visuals(theme));
    }

    fn set_theme(&mut self, ctx: &egui::Context, theme: ThemeMode) {
        if self.theme_mode != theme {
            self.theme_mode = theme;
            Self::apply_theme_visuals(ctx, theme);
            self.save_prefs();
        }
    }

    fn init_egui_style(ctx: &egui::Context) {
        let mut fonts = FontDefinitions::default();
        let candidates = [
            "C:/Windows/Fonts/meiryo.ttc",
            "C:/Windows/Fonts/YuGothM.ttc",
            "C:/Windows/Fonts/msgothic.ttc",
        ];
        for p in candidates {
            if let Ok(bytes) = std::fs::read(p) {
                fonts
                    .font_data
                    .insert("jp".into(), FontData::from_owned(bytes).into());
                fonts
                    .families
                    .get_mut(&FontFamily::Proportional)
                    .unwrap()
                    .insert(0, "jp".into());
                fonts
                    .families
                    .get_mut(&FontFamily::Monospace)
                    .unwrap()
                    .insert(0, "jp".into());
                break;
            }
        }
        ctx.set_fonts(fonts);

        let mut style = (*ctx.style()).clone();
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(16.0));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(14.0));
        style.visuals = Self::theme_visuals(ThemeMode::Dark);
        ctx.set_style(style);
    }

    fn ensure_theme_visuals(&self, ctx: &egui::Context) {
        let want_dark = self.theme_mode == ThemeMode::Dark;
        if ctx.style().visuals.dark_mode != want_dark {
            Self::apply_theme_visuals(ctx, self.theme_mode);
        }
    }

    fn prefs_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        let mut path = PathBuf::from(base);
        path.push("NeoWaves");
        let _ = std::fs::create_dir_all(&path);
        path.push("prefs.txt");
        Some(path)
    }

    fn normalize_spectro_cfg(cfg: &mut SpectrogramConfig) {
        if !cfg.fft_size.is_power_of_two() {
            cfg.fft_size = cfg.fft_size.next_power_of_two();
        }
        cfg.fft_size = cfg.fft_size.clamp(256, 65536);
        if !cfg.overlap.is_finite() {
            cfg.overlap = 0.875;
        }
        cfg.overlap = cfg.overlap.clamp(0.0, 0.95);
        if cfg.max_frames == 0 {
            cfg.max_frames = 4096;
        }
        cfg.max_frames = cfg.max_frames.clamp(256, 8192);
        if !cfg.db_floor.is_finite() {
            cfg.db_floor = -120.0;
        }
        cfg.db_floor = cfg.db_floor.clamp(-160.0, -20.0);
        if !cfg.max_freq_hz.is_finite() || cfg.max_freq_hz < 0.0 {
            cfg.max_freq_hz = 0.0;
        }
    }

    fn apply_spectro_config(&mut self, mut next: SpectrogramConfig) {
        Self::normalize_spectro_cfg(&mut next);
        if next == self.spectro_cfg {
            return;
        }
        self.spectro_cfg = next;
        self.save_prefs();
        self.cancel_all_spectrograms();
        self.spectro_cache.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
        self.spectro_inflight.clear();
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
    }

    fn load_prefs(&mut self) {
        let Some(path) = Self::prefs_path() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("theme=") {
                self.theme_mode = match rest {
                    "light" => ThemeMode::Light,
                    _ => ThemeMode::Dark,
                };
            } else if let Some(rest) = line.strip_prefix("skip_dotfiles=") {
                let v = matches!(rest.trim(), "1" | "true" | "yes" | "on");
                self.skip_dotfiles = v;
            } else if let Some(rest) = line.strip_prefix("spectro_fft=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.spectro_cfg.fft_size = v;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_window=") {
                self.spectro_cfg.window = match rest.trim() {
                    "hann" => WindowFunction::Hann,
                    _ => WindowFunction::BlackmanHarris,
                };
            } else if let Some(rest) = line.strip_prefix("spectro_overlap=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    self.spectro_cfg.overlap = v;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_max_frames=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    self.spectro_cfg.max_frames = v;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_scale=") {
                self.spectro_cfg.scale = match rest.trim() {
                    "linear" => SpectrogramScale::Linear,
                    _ => SpectrogramScale::Log,
                };
            } else if let Some(rest) = line.strip_prefix("spectro_mel_scale=") {
                self.spectro_cfg.mel_scale = match rest.trim() {
                    "log" => SpectrogramScale::Log,
                    _ => SpectrogramScale::Linear,
                };
            } else if let Some(rest) = line.strip_prefix("spectro_db_floor=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    self.spectro_cfg.db_floor = v;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_max_hz=") {
                if let Ok(v) = rest.trim().parse::<f32>() {
                    self.spectro_cfg.max_freq_hz = v;
                }
            } else if let Some(rest) = line.strip_prefix("spectro_note_labels=") {
                let v = matches!(rest.trim(), "1" | "true" | "yes" | "on");
                self.spectro_cfg.show_note_labels = v;
            }
        }
        Self::normalize_spectro_cfg(&mut self.spectro_cfg);
    }

    fn save_prefs(&self) {
        let Some(path) = Self::prefs_path() else {
            return;
        };
        let theme = match self.theme_mode {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        };
        let skip = if self.skip_dotfiles { "1" } else { "0" };
        let window = match self.spectro_cfg.window {
            WindowFunction::Hann => "hann",
            WindowFunction::BlackmanHarris => "blackman_harris",
        };
        let scale = match self.spectro_cfg.scale {
            SpectrogramScale::Linear => "linear",
            SpectrogramScale::Log => "log",
        };
        let mel_scale = match self.spectro_cfg.mel_scale {
            SpectrogramScale::Linear => "linear",
            SpectrogramScale::Log => "log",
        };
        let note_labels = if self.spectro_cfg.show_note_labels { "1" } else { "0" };
        let _ = std::fs::write(
            path,
            format!(
                "theme={}\nskip_dotfiles={}\n\
spectro_fft={}\n\
spectro_window={}\n\
spectro_overlap={:.4}\n\
spectro_max_frames={}\n\
spectro_scale={}\n\
spectro_mel_scale={}\n\
spectro_db_floor={:.1}\n\
spectro_max_hz={:.1}\n\
spectro_note_labels={}\n",
                theme,
                skip,
                self.spectro_cfg.fft_size,
                window,
                self.spectro_cfg.overlap,
                self.spectro_cfg.max_frames,
                scale,
                mel_scale,
                self.spectro_cfg.db_floor,
                self.spectro_cfg.max_freq_hz,
                note_labels
            ),
        );
    }

    fn is_dotfile_path(path: &Path) -> bool {
        path.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.starts_with('.'))
            .unwrap_or(false)
    }

    fn is_decode_failed_path(&self, path: &Path) -> bool {
        self.meta_for_path(path)
            .and_then(|m| m.decode_error.as_ref())
            .is_some()
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
    fn spawn_editor_apply_for_tab(&mut self, tab_idx: usize, tool: ToolKind, param: f32) {
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
            });
        });
        let msg = match tool {
            ToolKind::PitchShift => "Applying PitchShift...".to_string(),
            ToolKind::TimeStretch => "Applying TimeStretch...".to_string(),
            _ => "Applying...".to_string(),
        };
        self.editor_apply_state = Some(EditorApplyState {
            msg,
            rx,
            tab_idx,
            undo,
        });
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
                        let freq = match cfg.mel_scale {
                            SpectrogramScale::Linear => {
                                let mel = frac * mel_max;
                                700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0)
                            }
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
        tab.loop_markers_dirty = tab.loop_region != tab.loop_markers_saved;
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

    fn item_for_id(&self, id: MediaId) -> Option<&MediaItem> {
        self.item_index
            .get(&id)
            .and_then(|&idx| self.items.get(idx))
    }

    fn item_for_id_mut(&mut self, id: MediaId) -> Option<&mut MediaItem> {
        let idx = *self.item_index.get(&id)?;
        self.items.get_mut(idx)
    }

    fn item_for_row(&self, row_idx: usize) -> Option<&MediaItem> {
        let id = *self.files.get(row_idx)?;
        self.item_for_id(id)
    }

    fn item_for_path(&self, path: &Path) -> Option<&MediaItem> {
        let id = *self.path_index.get(path)?;
        self.item_for_id(id)
    }

    fn item_for_path_mut(&mut self, path: &Path) -> Option<&mut MediaItem> {
        let id = *self.path_index.get(path)?;
        self.item_for_id_mut(id)
    }

    fn is_virtual_path(&self, path: &Path) -> bool {
        self.item_for_path(path)
            .map(|item| item.source == MediaSource::Virtual)
            .unwrap_or(false)
    }

    fn meta_for_path(&self, path: &Path) -> Option<&FileMeta> {
        self.item_for_path(path).and_then(|item| item.meta.as_ref())
    }

    fn set_meta_for_path(&mut self, path: &Path, meta: FileMeta) -> bool {
        if let Some(item) = self.item_for_path_mut(path) {
            item.meta = Some(meta);
            return true;
        }
        false
    }

    fn clear_meta_for_path(&mut self, path: &Path) {
        if let Some(item) = self.item_for_path_mut(path) {
            item.meta = None;
        }
    }

    fn transcript_for_path(&self, path: &Path) -> Option<&Transcript> {
        self.item_for_path(path)
            .and_then(|item| item.transcript.as_ref())
    }

    fn set_transcript_for_path(&mut self, path: &Path, transcript: Option<Transcript>) -> bool {
        if let Some(item) = self.item_for_path_mut(path) {
            item.transcript = transcript;
            if item.transcript.is_some() && !self.list_columns.transcript {
                self.list_columns.transcript = true;
            }
            return true;
        }
        false
    }

    fn clear_transcript_for_path(&mut self, path: &Path) {
        if let Some(item) = self.item_for_path_mut(path) {
            item.transcript = None;
        }
    }

    fn pending_gain_db_for_path(&self, path: &Path) -> f32 {
        self.item_for_path(path)
            .map(|item| item.pending_gain_db)
            .unwrap_or(0.0)
    }

    fn set_pending_gain_db_for_path(&mut self, path: &Path, db: f32) {
        if let Some(item) = self.item_for_path_mut(path) {
            item.pending_gain_db = db;
        }
    }

    fn has_pending_gain(&self, path: &Path) -> bool {
        self.pending_gain_db_for_path(path).abs() > 0.0001
    }

    fn pending_gain_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.pending_gain_db.abs() > 0.0001)
            .count()
    }

    fn clear_all_pending_gains(&mut self) {
        for item in &mut self.items {
            item.pending_gain_db = 0.0;
        }
    }

    fn display_name_for_path(path: &Path) -> String {
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string()
    }

    fn display_folder_for_path(path: &Path) -> String {
        path.parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_string()
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
        let duration_secs = if sample_rate > 0 {
            Some(frames as f32 / sample_rate as f32)
        } else {
            None
        };
        FileMeta {
            channels: channels.len().max(1) as u16,
            sample_rate,
            bits_per_sample,
            duration_secs,
            rms_db: Some(rms_db),
            peak_db: Some(peak_db),
            lufs_i,
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

    fn clear_clipboard_temp_files(&mut self) {
        for path in self.clipboard_temp_files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }

    fn export_audio_to_temp_wav(
        &mut self,
        display_name: &str,
        audio: &crate::audio::AudioBuffer,
        sample_rate: u32,
    ) -> Option<PathBuf> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let dir = std::env::temp_dir().join("NeoWaves").join("clipboard");
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let safe = crate::app::helpers::sanitize_filename_component(display_name);
        let base = std::path::Path::new(&safe)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("clip");
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let filename = format!("{base}_{ts}.wav");
        let path = dir.join(filename);
        let range = (0, audio.len());
        if crate::wave::export_selection_wav(&audio.channels, sample_rate, range, &path).is_err() {
            return None;
        }
        self.clipboard_temp_files.push(path.clone());
        Some(path)
    }

    fn edited_audio_for_path(
        &self,
        path: &Path,
    ) -> Option<std::sync::Arc<crate::audio::AudioBuffer>> {
        if let Some(tab) = self.tabs.iter().find(|t| {
            (t.dirty || t.loop_markers_dirty || t.markers_dirty) && t.path.as_path() == path
        }) {
            return Some(std::sync::Arc::new(
                crate::audio::AudioBuffer::from_channels(tab.ch_samples.clone()),
            ));
        }
        if let Some(cached) = self.edited_cache.get(path) {
            return Some(std::sync::Arc::new(
                crate::audio::AudioBuffer::from_channels(cached.ch_samples.clone()),
            ));
        }
        if let Some(item) = self.item_for_path(path) {
            if item.source == MediaSource::Virtual {
                return item.virtual_audio.clone();
            }
        }
        None
    }

    fn decode_audio_for_virtual(
        &self,
        path: &Path,
    ) -> Option<(std::sync::Arc<crate::audio::AudioBuffer>, u32, u16)> {
        let (mut chans, in_sr) = crate::audio_io::decode_audio_multi(path).ok()?;
        let out_sr = self.audio.shared.out_sample_rate;
        if in_sr != out_sr {
            for c in chans.iter_mut() {
                *c = crate::wave::resample_linear(c, in_sr, out_sr);
            }
        }
        let bits = crate::audio_io::read_audio_info(path)
            .map(|info| info.bits_per_sample)
            .unwrap_or(32);
        let audio = std::sync::Arc::new(crate::audio::AudioBuffer::from_channels(chans));
        Some((audio, out_sr, bits))
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

    fn rebuild_item_indexes(&mut self) {
        self.item_index.clear();
        self.path_index.clear();
        for (idx, item) in self.items.iter().enumerate() {
            self.item_index.insert(item.id, idx);
            self.path_index.insert(item.path.clone(), item.id);
        }
    }

    fn path_for_row(&self, row_idx: usize) -> Option<&PathBuf> {
        let id = *self.files.get(row_idx)?;
        self.item_for_id(id).map(|item| &item.path)
    }

    fn row_for_path(&self, path: &std::path::Path) -> Option<usize> {
        let id = *self.path_index.get(path)?;
        self.files.iter().position(|&i| i == id)
    }

    fn selected_path_buf(&self) -> Option<PathBuf> {
        self.selected.and_then(|i| self.path_for_row(i).cloned())
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        let mut rows: Vec<usize> = self.selected_multi.iter().copied().collect();
        if rows.is_empty() {
            if let Some(sel) = self.selected {
                rows.push(sel);
            } else if let Some(idx) = self.active_tab {
                if let Some(tab) = self.tabs.get(idx) {
                    return vec![tab.path.clone()];
                }
            }
        }
        rows.sort_unstable();
        rows.into_iter()
            .filter_map(|row| self.path_for_row(row).cloned())
            .collect()
    }

    fn selected_real_paths(&self) -> Vec<PathBuf> {
        self.selected_paths()
            .into_iter()
            .filter(|p| !self.is_virtual_path(p))
            .collect()
    }

    fn selected_item_ids(&self) -> Vec<MediaId> {
        let mut rows: Vec<usize> = self.selected_multi.iter().copied().collect();
        if rows.is_empty() {
            if let Some(sel) = self.selected {
                rows.push(sel);
            } else if let Some(idx) = self.active_tab {
                if let Some(tab) = self.tabs.get(idx) {
                    if let Some(id) = self.path_index.get(&tab.path) {
                        return vec![*id];
                    }
                }
            }
        }
        rows.sort_unstable();
        rows.into_iter()
            .filter_map(|row| self.files.get(row).copied())
            .collect()
    }

    fn ensure_sort_key_visible(&mut self) {
        let cols = self.list_columns;
        let external_visible = cols.external && !self.external_visible_columns.is_empty();
        let key_visible = match self.sort_key {
            SortKey::File => cols.file,
            SortKey::Folder => cols.folder,
            SortKey::Transcript => cols.transcript,
            SortKey::Length => cols.length,
            SortKey::Channels => cols.channels,
            SortKey::SampleRate => cols.sample_rate,
            SortKey::Bits => cols.bits,
            SortKey::Level => cols.peak,
            SortKey::Lufs => cols.lufs,
            SortKey::External(idx) => external_visible && idx < self.external_visible_columns.len(),
        };
        if key_visible {
            return;
        }
        let fallback = if cols.file {
            SortKey::File
        } else if cols.folder {
            SortKey::Folder
        } else if cols.transcript {
            SortKey::Transcript
        } else if external_visible {
            SortKey::External(0)
        } else if cols.length {
            SortKey::Length
        } else if cols.channels {
            SortKey::Channels
        } else if cols.sample_rate {
            SortKey::SampleRate
        } else if cols.bits {
            SortKey::Bits
        } else if cols.peak {
            SortKey::Level
        } else if cols.lufs {
            SortKey::Lufs
        } else {
            SortKey::File
        };
        self.sort_key = fallback;
        self.sort_dir = SortDir::None;
    }

    fn request_list_autoplay(&mut self) {
        if !self.auto_play_list_nav {
            if let Some(state) = &mut self.processing {
                state.autoplay_when_ready = false;
            }
            return;
        }
        let Some(path) = self.selected_path_buf() else {
            if let Some(state) = &mut self.processing {
                state.autoplay_when_ready = false;
            }
            return;
        };
        if let Some(state) = &mut self.processing {
            if state.path == path {
                // Defer playback until heavy processing (pitch/time) finishes.
                state.autoplay_when_ready = true;
                return;
            }
            state.autoplay_when_ready = false;
        }
        self.audio.play();
    }

    fn current_active_path(&self) -> Option<&PathBuf> {
        if let Some(i) = self.active_tab {
            return self.tabs.get(i).map(|t| &t.path);
        }
        if let Some(i) = self.selected {
            return self.path_for_row(i);
        }
        None
    }
    pub(super) fn apply_effective_volume(&self) {
        // Global output volume (0..1)
        let base = db_to_amp(self.volume_db);
        self.audio.set_volume(base);
        // Per-file gain (can be >1)
        let path_opt = self
            .playing_path
            .as_ref()
            .or_else(|| self.current_active_path());
        let gain_db = if let Some(p) = path_opt {
            self.pending_gain_db_for_path(p)
        } else {
            0.0
        };
        let fg = db_to_amp(gain_db);
        self.audio.set_file_gain(fg);
    }

    fn open_rename_dialog(&mut self, path: PathBuf) {
        self.rename_input = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        self.rename_target = Some(path);
        self.rename_error = None;
        self.show_rename_dialog = true;
    }

    fn open_batch_rename_dialog(&mut self, paths: Vec<PathBuf>) {
        self.batch_rename_targets = paths;
        self.batch_rename_pattern = "{name}_{n}".into();
        self.batch_rename_start = 1;
        self.batch_rename_pad = 2;
        self.batch_rename_error = None;
        self.show_batch_rename_dialog = true;
    }

    fn replace_path_in_state(&mut self, from: &std::path::Path, to: &std::path::Path) {
        let Some(id) = self.path_index.get(from).copied() else {
            return;
        };
        let new_path = to.to_path_buf();
        let external = self.external_row_for_path(&new_path);
        if let Some(item) = self.item_for_id_mut(id) {
            item.path = new_path.clone();
            item.display_name = Self::display_name_for_path(&new_path);
            item.display_folder = Self::display_folder_for_path(&new_path);
            item.source = MediaSource::File;
            item.virtual_audio = None;
            item.transcript = None;
            item.external = external.unwrap_or_default();
        }
        self.path_index.remove(from);
        self.path_index.insert(new_path.clone(), id);
        if let Some(v) = self.spectro_cache.remove(from) {
            self.spectro_cache.insert(new_path.clone(), v);
        }
        if let Some(v) = self.spectro_cache_sizes.remove(from) {
            self.spectro_cache_sizes.insert(new_path.clone(), v);
        }
        if let Some(pos) = self
            .spectro_cache_order
            .iter()
            .position(|p| p.as_path() == from)
        {
            self.spectro_cache_order[pos] = new_path.clone();
        }
        if let Some(v) = self.edited_cache.remove(from) {
            self.edited_cache.insert(new_path.clone(), v);
        }
        self.meta_inflight.remove(from);
        self.transcript_inflight.remove(from);
        self.spectro_inflight.remove(from);
        if let Some(v) = self.spectro_progress.remove(from) {
            self.spectro_progress.insert(new_path.clone(), v);
        }
        if let Some(v) = self.spectro_cancel.remove(from) {
            self.spectro_cancel.insert(new_path.clone(), v);
        }
        if let Some(v) = self.lufs_override.remove(from) {
            self.lufs_override.insert(new_path.clone(), v);
        }
        if let Some(v) = self.lufs_recalc_deadline.remove(from) {
            self.lufs_recalc_deadline.insert(new_path.clone(), v);
        }
        for p in self.saving_sources.iter_mut() {
            if p.as_path() == from {
                *p = new_path.clone();
            }
        }
        if self.pending_activate_path.as_ref().map(|p| p.as_path()) == Some(from) {
            self.pending_activate_path = Some(new_path.clone());
        }
        for tab in self.tabs.iter_mut() {
            if tab.path.as_path() == from {
                tab.path = new_path.clone();
                tab.display_name = new_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(invalid)")
                    .to_string();
            }
        }
        if self.playing_path.as_ref().map(|p| p.as_path()) == Some(from) {
            self.playing_path = Some(new_path);
        }
    }

    fn rename_file_path(&mut self, from: &PathBuf, new_name: &str) -> Result<PathBuf, String> {
        let name = new_name.trim();
        if name.is_empty() {
            return Err("Name is empty.".to_string());
        }
        let mut name = name.to_string();
        let has_ext = std::path::Path::new(&name).extension().is_some();
        if !has_ext {
            if let Some(ext) = from.extension().and_then(|s| s.to_str()) {
                name.push('.');
                name.push_str(ext);
            } else {
                name.push_str(".wav");
            }
        }
        let Some(parent) = from.parent() else {
            return Err("Missing parent folder.".to_string());
        };
        let to = parent.join(name);
        if to == *from {
            return Ok(to);
        }
        if to.exists() {
            return Err("Target already exists.".to_string());
        }
        std::fs::rename(from, &to).map_err(|e| format!("Rename failed: {e}"))?;
        self.replace_path_in_state(from, &to);
        self.apply_filter_from_search();
        self.apply_sort();
        Ok(to)
    }

    fn batch_rename_paths(&mut self) -> Result<(), String> {
        if self.batch_rename_targets.is_empty() {
            return Err("No files selected.".to_string());
        }
        let pattern = self.batch_rename_pattern.trim().to_string();
        if pattern.is_empty() {
            return Err("Pattern is empty.".to_string());
        }
        let targets = self.batch_rename_targets.clone();
        let src_set: std::collections::HashSet<PathBuf> = targets.iter().cloned().collect();
        let mut mappings: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (i, src) in targets.iter().enumerate() {
            if !src.is_file() {
                self.remove_missing_path(src);
                continue;
            }
            let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let num = self.batch_rename_start.saturating_add(i as u32);
            let num_str = if self.batch_rename_pad > 0 {
                format!("{:0width$}", num, width = self.batch_rename_pad as usize)
            } else {
                num.to_string()
            };
            let mut name = pattern.replace("{name}", stem).replace("{n}", &num_str);
            if name.contains('/') || name.contains('\\') {
                return Err("Pattern must be a file name (no path separators).".to_string());
            }
            if name.trim().is_empty() {
                return Err("Generated name is empty.".to_string());
            }
            let has_ext = std::path::Path::new(&name).extension().is_some();
            if !has_ext {
                if let Some(ext) = src.extension().and_then(|s| s.to_str()) {
                    name.push('.');
                    name.push_str(ext);
                } else {
                    name.push_str(".wav");
                }
            }
            let parent = src.parent().unwrap_or_else(|| std::path::Path::new("."));
            let dst = parent.join(name);
            mappings.push((src.clone(), dst));
        }
        let mut seen = std::collections::HashSet::new();
        for (src, dst) in &mappings {
            if src == dst {
                continue;
            }
            if !seen.insert(dst.clone()) {
                return Err("Duplicate target names.".to_string());
            }
            if dst.exists() && !src_set.contains(dst) {
                return Err(format!("Target already exists: {}", dst.display()));
            }
        }
        let needs_temp = mappings
            .iter()
            .any(|(src, dst)| src != dst && src_set.contains(dst));
        if needs_temp {
            let mut temps: Vec<(PathBuf, PathBuf)> = Vec::new();
            for (i, (src, dst)) in mappings.iter().enumerate() {
                if src == dst {
                    continue;
                }
                let parent = src.parent().unwrap_or_else(|| std::path::Path::new("."));
                let mut tmp = parent.join(format!("._wvp_tmp_rename_{:03}.tmp", i));
                let mut bump = 0;
                while tmp.exists() {
                    bump += 1;
                    tmp = parent.join(format!("._wvp_tmp_rename_{:03}_{bump}.tmp", i));
                }
                std::fs::rename(src, &tmp).map_err(|e| format!("Rename failed: {e}"))?;
                self.replace_path_in_state(src, &tmp);
                temps.push((tmp, dst.clone()));
            }
            for (tmp, dst) in temps {
                std::fs::rename(&tmp, &dst).map_err(|e| format!("Rename failed: {e}"))?;
                self.replace_path_in_state(&tmp, &dst);
            }
        } else {
            for (src, dst) in &mappings {
                if src == dst {
                    continue;
                }
                std::fs::rename(src, dst).map_err(|e| format!("Rename failed: {e}"))?;
                self.replace_path_in_state(src, dst);
            }
        }
        self.apply_filter_from_search();
        self.apply_sort();
        self.selected_multi.clear();
        for (_, dst) in mappings {
            if let Some(row) = self.row_for_path(&dst) {
                self.selected_multi.insert(row);
            }
        }
        self.selected = self.selected_multi.iter().next().copied();
        if let Some(sel) = self.selected {
            self.select_anchor = Some(sel);
        }
        Ok(())
    }

    #[cfg(windows)]
    fn set_clipboard_files(&self, paths: &[PathBuf]) -> Result<(), String> {
        use clipboard_win::formats::FileList;
        use clipboard_win::{Clipboard, Setter};
        let list: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let _clip = Clipboard::new_attempts(10).map_err(|e| e.to_string())?;
        FileList.write_clipboard(&list).map_err(|e| e.to_string())
    }

    #[cfg(not(windows))]
    fn set_clipboard_files(&self, _paths: &[PathBuf]) -> Result<(), String> {
        Err("Clipboard file list is not supported on this platform".to_string())
    }

    #[cfg(windows)]
    fn get_clipboard_files(&self) -> Vec<PathBuf> {
        use clipboard_win::formats::FileList;
        let list: Vec<String> = clipboard_win::get_clipboard(FileList).unwrap_or_default();
        list.into_iter().map(PathBuf::from).collect()
    }

    #[cfg(not(windows))]
    fn get_clipboard_files(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn copy_selected_to_clipboard(&mut self) {
        let ids = self.selected_item_ids();
        if ids.is_empty() {
            return;
        }
        self.clear_clipboard_temp_files();
        let out_sr = self.audio.shared.out_sample_rate;
        let mut payload_items: Vec<ClipboardItem> = Vec::new();
        let mut os_paths: Vec<PathBuf> = Vec::new();
        for id in ids {
            let Some(item) = self.item_for_id(id) else {
                continue;
            };
            let display_name = item.display_name.clone();
            let source_path = if item.source == MediaSource::File {
                Some(item.path.clone())
            } else {
                None
            };
            let edited_audio = self.edited_audio_for_path(&item.path);
            let (audio, sample_rate, bits_per_sample) = if let Some(audio) = edited_audio.clone() {
                (Some(audio), out_sr, 32)
            } else {
                let meta = item.meta.as_ref();
                (
                    None,
                    meta.map(|m| m.sample_rate).unwrap_or(0),
                    meta.map(|m| m.bits_per_sample).unwrap_or(0),
                )
            };
            if let Some(audio_ref) = edited_audio {
                if audio_ref.len() > 0 {
                    if let Some(tmp) =
                        self.export_audio_to_temp_wav(&display_name, &audio_ref, out_sr)
                    {
                        os_paths.push(tmp);
                    }
                }
            } else if let Some(path) = &source_path {
                if path.is_file() {
                    os_paths.push(path.clone());
                }
            }
            payload_items.push(ClipboardItem {
                display_name,
                source_path,
                audio,
                sample_rate,
                bits_per_sample,
            });
        }
        self.clipboard_payload = Some(ClipboardPayload {
            items: payload_items,
            created_at: std::time::Instant::now(),
        });
        if self.debug.cfg.enabled {
            let count = self
                .clipboard_payload
                .as_ref()
                .map(|p| p.items.len())
                .unwrap_or(0);
            self.debug.last_copy_at = Some(std::time::Instant::now());
            self.debug.last_copy_count = count;
            self.debug_trace_input(format!("copy_selected_to_clipboard items={count}"));
        }
        if !os_paths.is_empty() {
            if let Err(err) = self.set_clipboard_files(&os_paths) {
                self.debug_log(format!("clipboard error: {err}"));
            }
        }
    }

    fn paste_clipboard_to_list(&mut self) {
        let payload = self.clipboard_payload.clone();
        let mut added_any = false;
        let mut added_paths: Vec<PathBuf> = Vec::new();
        if let Some(payload) = payload {
            let mut insert_idx = self.items.len();
            if let Some(row) = self
                .selected_multi
                .iter()
                .next_back()
                .copied()
                .or(self.selected)
            {
                if let Some(id) = self.files.get(row) {
                    if let Some(item_idx) = self.item_index.get(id) {
                        insert_idx = (*item_idx + 1).min(self.items.len());
                    }
                }
            }
            for item in payload.items {
                let mut audio = item.audio.clone();
                let mut sample_rate = item.sample_rate;
                let mut bits_per_sample = item.bits_per_sample;
                if audio.is_none() {
                    if let Some(path) = item.source_path.as_ref() {
                        if let Some((decoded, sr, bits)) = self.decode_audio_for_virtual(path) {
                            audio = Some(decoded);
                            sample_rate = sr;
                            bits_per_sample = bits;
                        }
                    }
                }
                let Some(audio) = audio else {
                    continue;
                };
                let name = self.unique_virtual_display_name(&item.display_name);
                let vitem = self.make_virtual_item(name, audio, sample_rate, bits_per_sample);
                added_paths.push(vitem.path.clone());
                self.add_virtual_item(vitem, Some(insert_idx));
                insert_idx = insert_idx.saturating_add(1);
                added_any = true;
            }
            if added_any {
                self.after_add_refresh();
                self.selected_multi.clear();
                for p in &added_paths {
                    if let Some(row) = self.row_for_path(p) {
                        self.selected_multi.insert(row);
                    }
                }
                self.selected = self.selected_multi.iter().next().copied();
                if self.debug.cfg.enabled {
                    self.debug.last_paste_at = Some(std::time::Instant::now());
                    self.debug.last_paste_count = added_paths.len();
                    self.debug.last_paste_source = Some("internal".to_string());
                    self.debug_trace_input(format!(
                        "paste_clipboard_to_list internal items={}",
                        added_paths.len()
                    ));
                }
            }
            return;
        }
        let files = self.get_clipboard_files();
        if !files.is_empty() {
            let added = self.add_files_merge(&files);
            if added > 0 {
                self.after_add_refresh();
                if self.debug.cfg.enabled {
                    self.debug.last_paste_at = Some(std::time::Instant::now());
                    self.debug.last_paste_count = added;
                    self.debug.last_paste_source = Some("os".to_string());
                    self.debug_trace_input(format!("paste_clipboard_to_list os files={added}"));
                }
            }
        }
    }

    fn spawn_export_gains(&mut self, _overwrite: bool) {
        use std::sync::mpsc;
        let mut targets: Vec<(PathBuf, f32)> = Vec::new();
        for item in &self.items {
            if item.pending_gain_db.abs() > 0.0001 {
                targets.push((item.path.clone(), item.pending_gain_db));
            }
        }
        if targets.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel::<ExportResult>();
        std::thread::spawn(move || {
            let mut ok = 0usize;
            let mut failed = 0usize;
            let mut success_paths = Vec::new();
            let mut failed_paths = Vec::new();
            for (src, db) in targets {
                let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("");
                let dst = if ext.is_empty() {
                    src.with_file_name(format!("{} (gain{:+.1}dB)", stem, db))
                } else {
                    src.with_file_name(format!("{} (gain{:+.1}dB).{}", stem, db, ext))
                };
                match crate::wave::export_gain_audio(&src, &dst, db) {
                    Ok(_) => {
                        ok += 1;
                        success_paths.push(dst);
                    }
                    Err(e) => {
                        eprintln!("export failed {}: {e:?}", src.display());
                        failed += 1;
                        failed_paths.push(src.clone());
                    }
                }
            }
            let _ = tx.send(ExportResult {
                ok,
                failed,
                success_paths,
                failed_paths,
            });
        });
        self.export_state = Some(ExportState {
            msg: "Exporting gains".into(),
            rx,
        });
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

    fn spawn_save_selected(&mut self, indices: std::collections::BTreeSet<usize>) {
        use std::sync::mpsc;
        if indices.is_empty() {
            return;
        }
        let mut items: Vec<(PathBuf, f32)> = Vec::new();
        let mut virtual_tasks: Vec<(
            PathBuf,
            PathBuf,
            std::sync::Arc<crate::audio::AudioBuffer>,
            f32,
            u32,
        )> = Vec::new();
        for i in indices {
            let Some(item) = self.item_for_row(i) else {
                continue;
            };
            let p = item.path.clone();
            let db = item.pending_gain_db;
            if item.source == MediaSource::Virtual {
                let audio = self
                    .edited_audio_for_path(&p)
                    .or_else(|| item.virtual_audio.clone());
                let Some(audio) = audio else {
                    continue;
                };
                let parent = self
                    .export_cfg
                    .dest_folder
                    .clone()
                    .or_else(|| self.root.clone())
                    .unwrap_or_else(|| PathBuf::from("."));
                let display_name = item.display_name.clone();
                let stem = std::path::Path::new(&display_name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("out");
                let mut name = self.export_cfg.name_template.clone();
                name = name.replace("{name}", stem);
                name = name.replace("{gain:+.1}", &format!("{:+.1}", db));
                name = name.replace("{gain:+0.0}", &format!("{:+.1}", db));
                name = name.replace("{gain}", &format!("{:+.1}", db));
                let name = crate::app::helpers::sanitize_filename_component(&name);
                let mut dst = parent.join(name);
                dst.set_extension("wav");
                if dst.exists() {
                    match self.export_cfg.conflict {
                        ConflictPolicy::Overwrite => {}
                        ConflictPolicy::Skip => continue,
                        ConflictPolicy::Rename => {
                            let orig = dst.clone();
                            let orig_ext =
                                orig.extension().and_then(|e| e.to_str()).unwrap_or("wav");
                            let mut idx = 1u32;
                            loop {
                                let stem2 =
                                    orig.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                                let n = crate::app::helpers::sanitize_filename_component(&format!(
                                    "{}_{:02}",
                                    stem2, idx
                                ));
                                dst = orig.with_file_name(n);
                                if !orig_ext.is_empty() {
                                    dst.set_extension(orig_ext);
                                }
                                if !dst.exists() {
                                    break;
                                }
                                idx += 1;
                                if idx > 999 {
                                    break;
                                }
                            }
                        }
                    }
                }
                let sr = item
                    .meta
                    .as_ref()
                    .map(|m| m.sample_rate)
                    .unwrap_or(self.audio.shared.out_sample_rate);
                virtual_tasks.push((p, dst, audio, db, sr));
            } else if db.abs() > 0.0001 {
                items.push((p, db));
            }
        }
        if items.is_empty() && virtual_tasks.is_empty() {
            return;
        }
        let cfg = self.export_cfg.clone();
        // remember sources for post-save cleanup + reload
        self.saving_sources = items.iter().map(|(p, _)| p.clone()).collect();
        self.saving_virtual = virtual_tasks
            .iter()
            .map(|(src, dst, _, _, _)| (src.clone(), dst.clone()))
            .collect();
        self.saving_mode = Some(if items.is_empty() {
            SaveMode::NewFile
        } else {
            cfg.save_mode
        });
        let virtual_jobs = virtual_tasks
            .iter()
            .map(|(src, dst, audio, db, sr)| (src.clone(), dst.clone(), audio.clone(), *db, *sr))
            .collect::<Vec<_>>();
        let (tx, rx) = mpsc::channel::<ExportResult>();
        std::thread::spawn(move || {
            let mut ok = 0usize;
            let mut failed = 0usize;
            let mut success_paths = Vec::new();
            let mut failed_paths = Vec::new();
            for (src, db) in items {
                match cfg.save_mode {
                    SaveMode::Overwrite => {
                        match crate::wave::overwrite_gain_audio(&src, db, cfg.backup_bak) {
                            Ok(()) => {
                                ok += 1;
                                success_paths.push(src.clone());
                            }
                            Err(_) => {
                                failed += 1;
                                failed_paths.push(src.clone());
                            }
                        }
                    }
                    SaveMode::NewFile => {
                        let parent = cfg.dest_folder.clone().unwrap_or_else(|| {
                            src.parent()
                                .unwrap_or_else(|| std::path::Path::new("."))
                                .to_path_buf()
                        });
                        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                        let mut name = cfg.name_template.clone();
                        name = name.replace("{name}", stem);
                        name = name.replace("{gain:+.1}", &format!("{:+.1}", db));
                        name = name.replace("{gain:+0.0}", &format!("{:+.1}", db));
                        name = name.replace("{gain}", &format!("{:+.1}", db));
                        let name = crate::app::helpers::sanitize_filename_component(&name);
                        let mut dst = parent.join(name);
                        let src_ext = src.extension().and_then(|e| e.to_str()).unwrap_or("wav");
                        let dst_ext = dst.extension().and_then(|e| e.to_str());
                        let use_dst_ext = dst_ext
                            .map(|e| crate::audio_io::is_supported_extension(e))
                            .unwrap_or(false);
                        if !use_dst_ext {
                            dst.set_extension(src_ext);
                        }
                        if dst.exists() {
                            match cfg.conflict {
                                ConflictPolicy::Overwrite => {}
                                ConflictPolicy::Skip => {
                                    failed += 1;
                                    failed_paths.push(src.clone());
                                    continue;
                                }
                                ConflictPolicy::Rename => {
                                    let orig = dst.clone();
                                    let orig_ext =
                                        orig.extension().and_then(|e| e.to_str()).unwrap_or("");
                                    let mut idx = 1u32;
                                    loop {
                                        let stem2 = orig
                                            .file_stem()
                                            .and_then(|s| s.to_str())
                                            .unwrap_or("out");
                                        let n = crate::app::helpers::sanitize_filename_component(
                                            &format!("{}_{:02}", stem2, idx),
                                        );
                                        dst = orig.with_file_name(n);
                                        if !orig_ext.is_empty() {
                                            dst.set_extension(orig_ext);
                                        }
                                        if !dst.exists() {
                                            break;
                                        }
                                        idx += 1;
                                        if idx > 999 {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        match crate::wave::export_gain_audio(&src, &dst, db) {
                            Ok(()) => {
                                ok += 1;
                                success_paths.push(dst.clone());
                            }
                            Err(_) => {
                                failed += 1;
                                failed_paths.push(src.clone());
                            }
                        }
                    }
                }
            }
            for (_src, dst, audio, db, sr) in virtual_jobs {
                let mut channels = audio.channels.clone();
                if db.abs() > 0.0001 {
                    let gain = 10.0f32.powf(db / 20.0);
                    for ch in channels.iter_mut() {
                        for v in ch.iter_mut() {
                            *v *= gain;
                        }
                    }
                }
                let res = crate::wave::export_selection_wav(
                    &channels,
                    sr.max(1),
                    (0, channels.get(0).map(|c| c.len()).unwrap_or(0)),
                    &dst,
                );
                match res {
                    Ok(()) => {
                        ok += 1;
                        success_paths.push(dst.clone());
                    }
                    Err(_) => {
                        failed += 1;
                        failed_paths.push(dst.clone());
                    }
                }
            }
            let _ = tx.send(ExportResult {
                ok,
                failed,
                success_paths,
                failed_paths,
            });
        });
        self.export_state = Some(ExportState {
            msg: "Saving...".into(),
            rx,
        });
    }

    // moved to logic.rs: update_selection_on_click

    // --- Gain helpers ---
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
    }

    fn schedule_lufs_for_path(&mut self, path: PathBuf) {
        use std::time::{Duration, Instant};
        if self.is_virtual_path(&path) {
            return;
        }
        let dl = Instant::now() + Duration::from_millis(400);
        self.lufs_recalc_deadline.insert(path, dl);
    }

    fn reset_meta_pool(&mut self) {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(6);
        let (pool, rx) = crate::app::meta::spawn_meta_pool(workers);
        self.meta_pool = Some(pool);
        self.meta_rx = Some(rx);
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
    }

    fn ensure_meta_pool(&mut self) {
        if self.meta_pool.is_none() {
            self.reset_meta_pool();
        }
    }

    fn queue_meta_for_path(&mut self, path: &PathBuf, priority: bool) {
        if self.is_virtual_path(path) {
            return;
        }
        if self.meta_for_path(path).is_some() {
            return;
        }
        self.ensure_meta_pool();
        if let Some(pool) = &self.meta_pool {
            if self.meta_inflight.contains(path) {
                if priority {
                    pool.promote_path(path);
                }
                return;
            }
            self.meta_inflight.insert(path.clone());
            if priority {
                pool.enqueue_front(meta::MetaTask::Header(path.clone()));
            } else {
                pool.enqueue(meta::MetaTask::Header(path.clone()));
            }
        }
    }

    fn queue_transcript_for_path(&mut self, path: &PathBuf, priority: bool) {
        if self.is_virtual_path(path) {
            return;
        }
        let Some(srt_path) = transcript::srt_path_for_audio(path) else {
            return;
        };
        if !srt_path.is_file() {
            self.clear_transcript_for_path(path);
            self.transcript_inflight.remove(path);
            return;
        }
        if self.transcript_for_path(path).is_some() {
            return;
        }
        self.ensure_meta_pool();
        if let Some(pool) = &self.meta_pool {
            if self.transcript_inflight.contains(path) {
                if priority {
                    pool.promote_path(path);
                }
                return;
            }
            self.transcript_inflight.insert(path.clone());
            if priority {
                pool.enqueue_front(meta::MetaTask::Transcript(path.clone()));
            } else {
                pool.enqueue(meta::MetaTask::Transcript(path.clone()));
            }
        }
    }

    fn start_scan_folder(&mut self, dir: PathBuf) {
        self.scan_rx = Some(self.spawn_scan_worker(dir, self.skip_dotfiles));
        self.scan_in_progress = true;
        self.scan_started_at = Some(std::time::Instant::now());
        self.scan_found_count = 0;
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.files.clear();
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
        self.selected = None;
        self.selected_multi.clear();
        self.select_anchor = None;
        self.reset_meta_pool();
    }

    fn append_scanned_paths(&mut self, batch: Vec<PathBuf>) {
        if batch.is_empty() {
            return;
        }
        let has_search = !self.search_query.trim().is_empty();
        let query = self.search_query.to_lowercase();
        self.items.reserve(batch.len());
        if !has_search {
            self.files.reserve(batch.len());
            self.original_files.reserve(batch.len());
        }
        let mut added = 0usize;
        for p in batch {
            if self.path_index.contains_key(&p) {
                continue;
            }
            let item = self.make_media_item(p.clone());
            let id = item.id;
            self.path_index.insert(p.clone(), id);
            self.item_index.insert(id, self.items.len());
            self.items.push(item);
            added += 1;
            if !has_search {
                self.files.push(id);
                self.original_files.push(id);
            } else {
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
                let matches = name.contains(&query) || parent.contains(&query);
                if matches {
                    self.files.push(id);
                    self.original_files.push(id);
                }
            }
        }
        if added > 0 {
            self.scan_found_count = self.scan_found_count.saturating_add(added);
        }
    }

    fn process_scan_messages(&mut self) {
        let Some(rx) = &self.scan_rx else {
            return;
        };
        let mut done = false;
        let mut batches: Vec<Vec<PathBuf>> = Vec::new();
        let start = std::time::Instant::now();
        let budget = Duration::from_millis(3);
        loop {
            if start.elapsed() >= budget {
                break;
            }
            match rx.try_recv() {
                Ok(ScanMessage::Batch(batch)) => batches.push(batch),
                Ok(ScanMessage::Done) => {
                    done = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }

        for batch in batches {
            self.append_scanned_paths(batch);
        }

        if done {
            self.scan_rx = None;
            self.scan_in_progress = false;
            self.scan_started_at = None;
            if self.external_source.is_some() {
                self.apply_external_mapping();
            }
            self.apply_filter_from_search();
            self.apply_sort();
        }
    }

    fn process_mcp_commands(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.mcp_cmd_rx else {
            return;
        };
        let Some(tx) = self.mcp_resp_tx.clone() else {
            return;
        };
        let mut cmds = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            cmds.push(cmd);
        }
        for cmd in cmds {
            let res = self.handle_mcp_command(cmd, ctx);
            let _ = tx.send(res);
        }
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

    fn mcp_list_files(
        &self,
        args: mcp::types::ListFilesArgs,
    ) -> std::result::Result<mcp::types::ListFilesResult, String> {
        use regex::RegexBuilder;
        let query = args.query.unwrap_or_default();
        let query = query.trim().to_string();
        let use_regex = args.regex.unwrap_or(false);
        let mut ids: Vec<MediaId> = self.files.clone();
        ids.retain(|id| {
            self.item_for_id(*id)
                .map(|item| item.source == MediaSource::File)
                .unwrap_or(false)
        });
        if !query.is_empty() {
            let re = if use_regex {
                RegexBuilder::new(&query)
                    .case_insensitive(true)
                    .build()
                    .ok()
            } else {
                RegexBuilder::new(&regex::escape(&query))
                    .case_insensitive(true)
                    .build()
                    .ok()
            };
            ids.retain(|id| {
                let Some(item) = self.item_for_id(*id) else {
                    return false;
                };
                let name = item.display_name.as_str();
                let parent = item.display_folder.as_str();
                let transcript = item
                    .transcript
                    .as_ref()
                    .map(|t| t.full_text.as_str())
                    .unwrap_or("");
                let external_hit = item.external.values().any(|v| {
                    if let Some(re) = re.as_ref() {
                        re.is_match(v)
                    } else {
                        false
                    }
                });
                if let Some(re) = re.as_ref() {
                    re.is_match(name)
                        || re.is_match(parent)
                        || re.is_match(transcript)
                        || external_hit
                } else {
                    false
                }
            });
        }
        let total = ids.len() as u32;
        let offset = args.offset.unwrap_or(0) as usize;
        let limit = args.limit.unwrap_or(u32::MAX) as usize;
        let include_meta = args.include_meta.unwrap_or(true);
        let mut items = Vec::new();
        for id in ids.into_iter().skip(offset).take(limit) {
            let Some(item) = self.item_for_id(id) else {
                continue;
            };
            let path = item.path.display().to_string();
            let name = item.display_name.clone();
            let folder = item.display_folder.clone();
            let meta = if include_meta {
                item.meta.as_ref()
            } else {
                None
            };
            let status = if !item.path.exists() {
                Some("missing".to_string())
            } else if let Some(m) = item.meta.as_ref() {
                if m.decode_error.is_some() {
                    Some("decode_failed".to_string())
                } else {
                    Some("ok".to_string())
                }
            } else {
                None
            };
            items.push(mcp::types::FileItem {
                path,
                name,
                folder,
                length_secs: meta.and_then(|m| m.duration_secs),
                sample_rate: meta.map(|m| m.sample_rate),
                channels: meta.map(|m| m.channels),
                bits: meta.map(|m| m.bits_per_sample),
                peak_db: meta.and_then(|m| m.peak_db),
                lufs_i: meta.and_then(|m| m.lufs_i),
                gain_db: Some(item.pending_gain_db),
                status,
            });
        }
        Ok(mcp::types::ListFilesResult { total, items })
    }

    fn request_transcript_seek(&mut self, path: &Path, start_ms: u64) {
        self.pending_transcript_seek = Some((path.to_path_buf(), start_ms));
        if self.playing_path.as_deref() == Some(path) {
            return;
        }
        if let Some(row) = self.row_for_path(path) {
            self.select_and_load(row, true);
            return;
        }
        if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
            self.active_tab = Some(idx);
            self.rebuild_current_buffer_with_mode();
        }
    }

    fn apply_pending_transcript_seek(&mut self) {
        let Some((path, start_ms)) = self.pending_transcript_seek.clone() else {
            return;
        };
        if self.playing_path.as_ref() != Some(&path) {
            return;
        }
        let sr = self.audio.shared.out_sample_rate.max(1) as u64;
        let mut samples = ((start_ms * sr) / 1000) as usize;
        if let Some(tab) = self.tabs.iter().find(|t| t.path == path) {
            samples = self.map_display_to_audio_sample(tab, samples);
        }
        self.audio.seek_to_sample(samples);
        self.pending_transcript_seek = None;
    }

    fn schedule_search_refresh(&mut self) {
        self.search_dirty = true;
        self.search_deadline = Some(std::time::Instant::now() + Duration::from_millis(300));
    }

    fn apply_search_if_due(&mut self) {
        let Some(deadline) = self.search_deadline else {
            return;
        };
        if !self.search_dirty {
            return;
        }
        if std::time::Instant::now() >= deadline {
            self.apply_filter_from_search();
            if self.sort_dir != SortDir::None {
                self.apply_sort();
            }
            self.search_dirty = false;
            self.search_deadline = None;
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
            let item = self.make_media_item(path.clone());
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
            external_source: None,
            external_headers: Vec::new(),
            external_rows: Vec::new(),
            external_key_index: None,
            external_key_rule: ExternalKeyRule::FileName,
            external_visible_columns: Vec::new(),
            external_lookup: HashMap::new(),
            external_match_count: 0,
            external_unmatched_count: 0,
            show_external_dialog: false,
            external_load_error: None,
            external_match_regex: String::new(),
            external_match_replace: String::new(),
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
            sort_key: SortKey::File,
            sort_dir: SortDir::None,
            scroll_to_selected: false,
            last_list_scroll_at: None,
            auto_play_list_nav: false,
            suppress_list_enter: false,
            original_files: Vec::new(),
            search_query: String::new(),
            search_use_regex: false,
            search_dirty: false,
            search_deadline: None,
            skip_dotfiles: true,
            mode: RateMode::Speed,
            processing: None,
            list_preview_rx: None,
            list_preview_job_id: 0,
            editor_apply_state: None,
            editor_decode_state: None,
            editor_decode_job_id: 0,
            edited_cache: HashMap::new(),
            export_state: None,
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
            saving_mode: None,

            lufs_override: HashMap::new(),
            lufs_recalc_deadline: HashMap::new(),
            lufs_rx2: None,
            lufs_worker_busy: false,
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

    fn push_undo_state(tab: &mut EditorTab, clear_redo: bool) {
        let state = Self::capture_undo_state(tab);
        Self::push_undo_state_from(tab, state, clear_redo);
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

    #[cfg(feature = "kittest")]
    pub fn test_playing_path(&self) -> Option<&PathBuf> {
        self.playing_path.as_ref()
    }

    #[cfg(feature = "kittest")]
    pub fn test_mode_name(&self) -> &'static str {
        match self.mode {
            RateMode::Speed => "Speed",
            RateMode::PitchShift => "PitchShift",
            RateMode::TimeStretch => "TimeStretch",
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_has_pending_gain(&self, path: &PathBuf) -> bool {
        self.pending_gain_db_for_path(path).abs() > 0.0001
    }

    #[cfg(feature = "kittest")]
    pub fn test_show_export_settings(&self) -> bool {
        self.show_export_settings
    }

    #[cfg(feature = "kittest")]
    pub fn test_pending_gain_count(&self) -> usize {
        self.pending_gain_count()
    }

    #[cfg(feature = "kittest")]
    pub fn test_sort_key_name(&self) -> &'static str {
        match self.sort_key {
            SortKey::File => "File",
            SortKey::Folder => "Folder",
            SortKey::Transcript => "Transcript",
            SortKey::Length => "Length",
            SortKey::Channels => "Channels",
            SortKey::SampleRate => "SampleRate",
            SortKey::Bits => "Bits",
            SortKey::Level => "Level",
            SortKey::Lufs => "Lufs",
            SortKey::External(_) => "External",
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_sort_dir_name(&self) -> &'static str {
        match self.sort_dir {
            SortDir::Asc => "Asc",
            SortDir::Desc => "Desc",
            SortDir::None => "None",
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_set_search_query(&mut self, query: &str) {
        self.search_query = query.to_string();
        self.apply_filter_from_search();
        self.apply_sort();
        self.search_dirty = false;
        self.search_deadline = None;
    }

    #[cfg(feature = "kittest")]
    pub fn test_replace_with_files(&mut self, paths: &[PathBuf]) {
        self.replace_with_files(paths);
        self.after_add_refresh();
    }

    #[cfg(feature = "kittest")]
    pub fn test_add_paths(&mut self, paths: &[PathBuf]) -> usize {
        let added = self.add_files_merge(paths);
        self.after_add_refresh();
        added
    }

}

impl eframe::App for WavesPreviewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.suppress_list_enter = false;
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
        // Undo/Redo (Ctrl+Z / Ctrl+Shift+Z) in editor
        if !ctx.wants_keyboard_input() {
            let (undo, redo) = ctx.input(|i| {
                let redo = i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(Key::Z);
                let undo = i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(Key::Z);
                (undo, redo)
            });
            if undo || redo {
                if let Some(tab_idx) = self.active_tab {
                    self.clear_preview_if_any(tab_idx);
                    self.editor_apply_state = None;
                    let changed = if redo {
                        self.redo_in_tab(tab_idx)
                    } else {
                        self.undo_in_tab(tab_idx)
                    };
                    if changed {
                        ctx.request_repaint();
                    }
                }
            }
        }
        // Drain heavy preview results
        if let Some(rx) = &self.heavy_preview_rx {
            if let Ok(mono) = rx.try_recv() {
                if let Some(idx) = self.active_tab {
                    if let Some(tool) = self.heavy_preview_tool {
                        self.set_preview_mono(idx, tool, mono);
                    }
                }
                self.heavy_preview_rx = None;
                self.heavy_preview_tool = None;
            }
        }
        // Drain list preview full-load results
        if let Some(rx) = &self.list_preview_rx {
            if let Ok(res) = rx.try_recv() {
                if res.job_id == self.list_preview_job_id {
                    if self.active_tab.is_none() && self.playing_path.as_ref() == Some(&res.path) {
                        let buf = crate::audio::AudioBuffer::from_channels(res.channels);
                        self.audio
                            .replace_samples_keep_pos(std::sync::Arc::new(buf));
                    }
                }
                self.list_preview_rx = None;
            }
        }
        self.drain_editor_decode();
        // Drain heavy per-channel overlay results
        if let Some(rx) = &self.heavy_overlay_rx {
            if let Ok((p, overlay, timeline_len, gen)) = rx.try_recv() {
                let expected_tool = self.overlay_expected_tool.take();
                if gen == self.overlay_expected_gen {
                    if let Some(idx) = self.tabs.iter().position(|t| t.path == p) {
                        if let Some(tab) = self.tabs.get_mut(idx) {
                            if let Some(tool) = expected_tool {
                                if tab.preview_audio_tool == Some(tool) || tab.active_tool == tool {
                                    tab.preview_overlay =
                                        Some(Self::preview_overlay_from_channels(
                                            overlay,
                                            tool,
                                            timeline_len,
                                        ));
                                }
                            } else {
                                tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                    overlay,
                                    tab.active_tool,
                                    timeline_len,
                                ));
                            }
                        }
                    }
                }
                self.heavy_overlay_rx = None;
            }
        }
        // Drain editor apply jobs (pitch/stretch)
        let mut apply_done: Option<(EditorApplyResult, Option<EditorUndoState>)> = None;
        if let Some(state) = &mut self.editor_apply_state {
            if let Ok(res) = state.rx.try_recv() {
                let undo = state.undo.take();
                apply_done = Some((res, undo));
            }
        }
        if let Some((res, undo)) = apply_done {
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
            self.editor_apply_state = None;
            ctx.request_repaint();
        }
        // Drain metadata updates
        if let Some(rx) = &self.meta_rx {
            let mut updates: Vec<meta::MetaUpdate> = Vec::new();
            while let Ok(update) = rx.try_recv() {
                updates.push(update);
            }
            if !updates.is_empty() {
                let mut resort = false;
                let mut refilter = false;
                for update in updates {
                    match update {
                        meta::MetaUpdate::Header(p, m) => {
                            if self.set_meta_for_path(&p, m) {
                                resort = true;
                            }
                        }
                        meta::MetaUpdate::Full(p, m) => {
                            self.meta_inflight.remove(&p);
                            if self.set_meta_for_path(&p, m) {
                                resort = true;
                            }
                        }
                        meta::MetaUpdate::Transcript(p, t) => {
                            self.transcript_inflight.remove(&p);
                            if self.set_transcript_for_path(&p, t)
                                && !self.search_query.trim().is_empty()
                            {
                                refilter = true;
                            }
                        }
                    }
                }
                if refilter {
                    self.apply_filter_from_search();
                    self.apply_sort();
                    ctx.request_repaint();
                } else if resort {
                    self.apply_sort();
                    ctx.request_repaint();
                }
            }
        }
        // Drain spectrogram jobs (tiled)
        self.drain_spectrogram_jobs(ctx);

        // Drain export results
        if let Some(state) = &self.export_state {
            if let Ok(res) = state.rx.try_recv() {
                eprintln!("save/export done: ok={}, failed={}", res.ok, res.failed);
                if state.msg.starts_with("Saving") {
                    let sources = self.saving_sources.clone();
                    for p in &sources {
                        self.set_pending_gain_db_for_path(p, 0.0);
                        self.lufs_override.remove(p);
                    }
                    let success_set: std::collections::HashSet<PathBuf> =
                        res.success_paths.iter().cloned().collect();
                    let mut virtual_success: Vec<(PathBuf, PathBuf)> = Vec::new();
                    for (src, dst) in &self.saving_virtual {
                        if success_set.contains(dst) {
                            virtual_success.push((src.clone(), dst.clone()));
                        }
                    }
                    for (src, dst) in &virtual_success {
                        self.set_pending_gain_db_for_path(src, 0.0);
                        self.lufs_override.remove(src);
                        self.replace_path_in_state(src, dst);
                    }
                    match self.saving_mode.unwrap_or(self.export_cfg.save_mode) {
                        SaveMode::Overwrite => {
                            if !self.saving_sources.is_empty() {
                                self.ensure_meta_pool();
                                let sources = self.saving_sources.clone();
                                for p in sources {
                                    self.clear_meta_for_path(&p);
                                    self.meta_inflight.remove(&p);
                                    self.queue_meta_for_path(&p, false);
                                }
                            }
                            if let Some(path) = self.saving_sources.get(0).cloned() {
                                if let Some(idx) = self.row_for_path(&path) {
                                    self.select_and_load(idx, true);
                                }
                            }
                        }
                        SaveMode::NewFile => {
                            let virtual_dests: std::collections::HashSet<PathBuf> = self
                                .saving_virtual
                                .iter()
                                .map(|(_, dst)| dst.clone())
                                .collect();
                            let mut added_any = false;
                            let mut first_added = None;
                            for p in &res.success_paths {
                                if virtual_dests.contains(p) {
                                    continue;
                                }
                                if self.add_files_merge(&[p.clone()]) > 0 {
                                    if first_added.is_none() {
                                        first_added = Some(p.clone());
                                    }
                                    added_any = true;
                                }
                            }
                            if added_any {
                                self.after_add_refresh();
                            }
                            if let Some(p) = first_added {
                                if let Some(idx) = self.row_for_path(&p) {
                                    self.select_and_load(idx, true);
                                }
                            }
                        }
                    }
                    self.saving_sources.clear();
                    self.saving_virtual.clear();
                    self.saving_mode = None;
                }
                self.export_state = None;
                ctx.request_repaint();
            }
        }

        // Drain LUFS (with gain) recompute results
        let mut got_any = false;
        if let Some(rx) = &self.lufs_rx2 {
            while let Ok((p, v)) = rx.try_recv() {
                self.lufs_override.insert(p, v);
                got_any = true;
            }
        }
        if got_any {
            self.lufs_worker_busy = false;
        }

        // Pump LUFS recompute worker (debounced)
        if !self.lufs_worker_busy {
            let now = std::time::Instant::now();
            if let Some(path) = self
                .lufs_recalc_deadline
                .iter()
                .find(|(_, dl)| **dl <= now)
                .map(|(p, _)| p.clone())
            {
                self.lufs_recalc_deadline.remove(&path);
                let g_db = self.pending_gain_db_for_path(&path);
                if g_db.abs() < 0.0001 {
                    self.lufs_override.remove(&path);
                } else {
                    use std::sync::mpsc;
                    let (tx, rx) = mpsc::channel();
                    self.lufs_rx2 = Some(rx);
                    self.lufs_worker_busy = true;
                    std::thread::spawn(move || {
                        let res = (|| -> anyhow::Result<f32> {
                            let (mut chans, sr) = crate::wave::decode_wav_multi(&path)?;
                            let gain = 10.0f32.powf(g_db / 20.0);
                            for ch in chans.iter_mut() {
                                for v in ch.iter_mut() {
                                    *v *= gain;
                                }
                            }
                            crate::wave::lufs_integrated_from_multi(&chans, sr)
                        })();
                        let val = match res {
                            Ok(v) => v,
                            Err(_) => f32::NEG_INFINITY,
                        };
                        let _ = tx.send((path, val));
                    });
                }
            }
        }

        // Drain heavy processing result
        let mut processing_done: Option<(ProcessingResult, bool)> = None;
        if let Some(state) = &mut self.processing {
            if let Ok(res) = state.rx.try_recv() {
                processing_done = Some((res, state.autoplay_when_ready));
            }
        }
        if let Some((res, autoplay_when_ready)) = processing_done {
            let ProcessingResult {
                path,
                samples,
                waveform,
                channels,
            } = res;
            // Apply new buffer and waveform
            if channels.is_empty() {
                self.audio.set_samples_mono(samples);
            } else {
                self.audio.set_samples_channels(channels);
            }
            self.audio.stop();
            if let Some(idx) = self.tabs.iter().position(|t| t.path == path) {
                if let Some(tab) = self.tabs.get_mut(idx) {
                    tab.waveform_minmax = waveform;
                }
            }
            // update current playing path (for effective volume using pending gains)
            self.playing_path = Some(path.clone());
            // full-buffer loop region if needed
            if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                self.audio.set_loop_region(0, buf.len());
            }
            self.processing = None;
            if autoplay_when_ready
                && self.auto_play_list_nav
                && self.selected_path_buf().as_ref() == Some(&path)
            {
                self.audio.play();
            }
            ctx.request_repaint();
        }

        // Shortcuts
        if ctx.input(|i| i.key_pressed(Key::Space)) {
            // Keep preview audio/overlay when toggling playback.
            self.audio.toggle_play();
        }
        // Tab switching: Ctrl+1 = List, Ctrl+2.. = editor tabs
        if !ctx.wants_keyboard_input() {
            let mods = ctx.input(|i| i.modifiers);
            if mods.ctrl {
                let mut target: Option<usize> = None;
                if ctx.input(|i| i.key_pressed(Key::Num1)) {
                    target = Some(0);
                } else if ctx.input(|i| i.key_pressed(Key::Num2)) {
                    target = Some(1);
                } else if ctx.input(|i| i.key_pressed(Key::Num3)) {
                    target = Some(2);
                } else if ctx.input(|i| i.key_pressed(Key::Num4)) {
                    target = Some(3);
                } else if ctx.input(|i| i.key_pressed(Key::Num5)) {
                    target = Some(4);
                } else if ctx.input(|i| i.key_pressed(Key::Num6)) {
                    target = Some(5);
                } else if ctx.input(|i| i.key_pressed(Key::Num7)) {
                    target = Some(6);
                } else if ctx.input(|i| i.key_pressed(Key::Num8)) {
                    target = Some(7);
                } else if ctx.input(|i| i.key_pressed(Key::Num9)) {
                    target = Some(8);
                }
                if let Some(idx) = target {
                    if idx == 0 {
                        if let Some(prev) = self.active_tab {
                            self.clear_preview_if_any(prev);
                        }
                        self.active_tab = None;
                        self.audio.stop();
                        self.audio.set_loop_enabled(false);
                    } else {
                        let tab_idx = idx - 1;
                        if tab_idx < self.tabs.len() {
                            if let Some(prev) = self.active_tab {
                                if prev != tab_idx {
                                    self.clear_preview_if_any(prev);
                                }
                            }
                            if let Some(tab) = self.tabs.get(tab_idx) {
                                self.active_tab = Some(tab_idx);
                                self.audio.stop();
                                self.pending_activate_path = Some(tab.path.clone());
                            }
                        }
                    }
                }
            }
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::S)) {
            self.trigger_save_selected();
        }
        // Editor-specific shortcuts: Loop region setters, Loop toggle (L), Zero-cross snap (S)
        if let Some(tab_idx) = self.active_tab {
            // Loop Start/End at playhead
            if ctx.input(|i| i.key_pressed(Key::K)) {
                // Set Loop Start
                let pos_audio = self
                    .audio
                    .shared
                    .play_pos
                    .load(std::sync::atomic::Ordering::Relaxed);
                let pos_now = self
                    .tabs
                    .get(tab_idx)
                    .map(|tab_ro| self.map_audio_to_display_sample(tab_ro, pos_audio))
                    .unwrap_or(0);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    let end = tab.loop_region.map(|(_, e)| e).unwrap_or(pos_now);
                    let s = pos_now.min(end);
                    let e = end.max(s);
                    tab.loop_region = Some((s, e));
                    Self::update_loop_markers_dirty(tab);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::P)) {
                // Set Loop End
                let pos_audio = self
                    .audio
                    .shared
                    .play_pos
                    .load(std::sync::atomic::Ordering::Relaxed);
                let pos_now = self
                    .tabs
                    .get(tab_idx)
                    .map(|tab_ro| self.map_audio_to_display_sample(tab_ro, pos_audio))
                    .unwrap_or(0);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    let start = tab.loop_region.map(|(s, _)| s).unwrap_or(pos_now);
                    let s = start.min(pos_now);
                    let e = pos_now.max(start);
                    tab.loop_region = Some((s, e));
                    Self::update_loop_markers_dirty(tab);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::L)) {
                // Toggle loop mode without holding a mutable borrow across &self call
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.loop_mode = match tab.loop_mode {
                        LoopMode::Off => LoopMode::OnWhole,
                        _ => LoopMode::Off,
                    };
                }
                if let Some(tab_ro) = self.tabs.get(tab_idx) {
                    self.apply_loop_mode_for_tab(tab_ro);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::S)) {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.snap_zero_cross = !tab.snap_zero_cross;
                }
            }
        }

        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::W)) {
            if let Some(active_idx) = self.active_tab {
                self.audio.stop();
                self.tabs.remove(active_idx);
                // NOTE: invalid-encoding comment removed
                if !self.tabs.is_empty() {
                    let new_active = if active_idx < self.tabs.len() {
                        active_idx
                    } else {
                        self.tabs.len() - 1
                    };
                    self.active_tab = Some(new_active);
                } else {
                    self.active_tab = None;
                }
            }
        }

        // Top controls (always visible)
        self.ui_top_bar(ctx);
        // Drag & Drop: merge dropped files/folders into the list (supported audio)
        {
            let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
            if !dropped.is_empty() {
                let mut project_path: Option<std::path::PathBuf> = None;
                let mut paths: Vec<std::path::PathBuf> = Vec::new();
                for f in dropped {
                    if let Some(p) = f.path {
                        let is_project = p
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s.eq_ignore_ascii_case("nwproj"))
                            .unwrap_or(false);
                        if is_project && project_path.is_none() {
                            project_path = Some(p);
                        } else if !is_project {
                            paths.push(p);
                        }
                    }
                }
                if let Some(project) = project_path {
                    self.queue_project_open(project);
                } else if !paths.is_empty() {
                    let added = self.add_files_merge(&paths);
                    if added > 0 {
                        self.after_add_refresh();
                    }
                }
            }
        }
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
                }
                let mut to_close: Option<usize> = None;
                let tabs_len = self.tabs.len();
                for i in 0..tabs_len {
                    // avoid holding immutable borrow over calls that mutate self inside closure
                    let active = self.active_tab == Some(i);
                    let tab = &self.tabs[i];
                    let mut display = tab.display_name.clone();
                    if tab.dirty || tab.loop_markers_dirty {
                        display.push_str(" *");
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
                    self.cache_dirty_tab_at(i);
                    self.tabs.remove(i);
                    match self.active_tab {
                        Some(ai) if ai == i => self.active_tab = None,
                        Some(ai) if ai > i => self.active_tab = Some(ai - 1),
                        _ => {}
                    }
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
                                                    duration_secs: info.duration_secs,
                                                    rms_db: None,
                                                    peak_db: None,
                                                    lufs_i: None,
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
        let block_busy = self.export_state.is_some() || self.editor_apply_state.is_some();
        if block_busy {
            use egui::{Id, LayerId, Order};
            let screen = ctx.viewport_rect();
            // block input
            egui::Area::new("busy_block_input".into())
                .order(Order::Foreground)
                .show(ctx, |ui| {
                    let _ = ui.allocate_rect(screen, Sense::click_and_drag());
                });
            // darken background
            let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("busy_layer")));
            painter.rect_filled(screen, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 180));
            // centered box with spinner and text
            egui::Area::new("busy_center".into())
                .order(Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    egui::Frame::window(ui.style()).show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Spinner::new());
                            let msg = if let Some(p) = &self.processing {
                                p.msg.as_str()
                            } else if let Some(st) = &self.editor_apply_state {
                                st.msg.as_str()
                            } else if let Some(st) = &self.export_state {
                                st.msg.as_str()
                            } else {
                                "Working..."
                            };
                            ui.label(RichText::new(msg).strong());
                            if self.editor_apply_state.is_some() {
                                if ui.button("Cancel").clicked() {
                                    self.cancel_editor_apply();
                                }
                            }
                        });
                    });
                });
        }
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
                                        self.cache_dirty_tab_at(i);
                                        self.tabs.remove(i);
                                        if let Some(ai) = self.active_tab {
                                            if ai == i {
                                                self.active_tab = None;
                                            } else if ai > i {
                                                self.active_tab = Some(ai - 1);
                                            }
                                        }
                                    }
                                    self.audio.stop();
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
            egui::Window::new("First Save Option")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label("Choose default save behavior for Ctrl+S:");
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
        // Debug window
        self.ui_debug_window(ctx);
    }
}
