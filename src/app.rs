use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use egui::{Align, Color32, FontData, FontDefinitions, FontFamily, FontId, Key, RichText, Sense, TextStyle, Visuals};
use egui_extras::TableBuilder;
use crate::mcp;
use crate::audio::AudioEngine;
use crate::wave::{build_minmax, prepare_for_speed};
#[cfg(feature = "kittest")]
use std::collections::VecDeque;
// use walkdir::WalkDir; // unused here (used in logic.rs)

mod types;
mod helpers;
mod meta;
mod transcript;
mod external;
mod tooling;
mod logic;
mod editor_ops;
mod ui;
mod render;
mod capture;
use self::{types::*, helpers::*};
use self::tooling::{ToolDef, ToolJob, ToolLogEntry, ToolRunResult};
use self::capture::save_color_image_png;
pub use self::types::StartupConfig;

const LIVE_PREVIEW_SAMPLE_LIMIT: usize = 2_000_000;
const UNDO_STACK_LIMIT: usize = 20;
const UNDO_STACK_MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_EDITOR_TABS: usize = 12;

// moved to types.rs

#[cfg(feature = "kittest")]
#[derive(Default)]
struct TestDialogQueue {
    folder: VecDeque<Option<PathBuf>>,
    files: VecDeque<Option<Vec<PathBuf>>>,
}

#[cfg(feature = "kittest")]
impl TestDialogQueue {
    fn next_folder(&mut self) -> Option<PathBuf> {
        self.folder.pop_front().unwrap_or(None)
    }

    fn next_files(&mut self) -> Option<Vec<PathBuf>> {
        self.files.pop_front().unwrap_or(None)
    }

    fn push_folder(&mut self, path: Option<PathBuf>) {
        self.folder.push_back(path);
    }

    fn push_files(&mut self, files: Option<Vec<PathBuf>>) {
        self.files.push_back(files);
    }
}

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
    pub spectro_cache: HashMap<PathBuf, std::sync::Arc<SpectrogramData>>,
    pub spectro_inflight: HashSet<PathBuf>,
    pub spectro_tx: Option<std::sync::mpsc::Sender<(PathBuf, SpectrogramData)>>,
    pub spectro_rx: Option<std::sync::mpsc::Receiver<(PathBuf, SpectrogramData)>>,
    pub scan_rx: Option<std::sync::mpsc::Receiver<ScanMessage>>,
    pub scan_in_progress: bool,
    // dynamic row height for wave thumbnails (list view)
    pub wave_row_h: f32,
    pub list_columns: ListColumnConfig,
    // multi-selection (list view)
    pub selected_multi: std::collections::BTreeSet<usize>,
    pub select_anchor: Option<usize>,
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
        heavy_overlay_rx: Option<std::sync::mpsc::Receiver<(std::path::PathBuf, Vec<Vec<f32>>, usize, u64)>>,
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
        style.text_styles.insert(TextStyle::Body, FontId::proportional(16.0));
        style.text_styles.insert(TextStyle::Monospace, FontId::monospace(14.0));
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
        let base = std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        let mut path = PathBuf::from(base);
        path.push("NeoWaves");
        let _ = std::fs::create_dir_all(&path);
        path.push("prefs.txt");
        Some(path)
    }

    fn tools_config_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        let mut path = PathBuf::from(base);
        path.push("NeoWaves");
        let _ = std::fs::create_dir_all(&path);
        path.push("tools.toml");
        Some(path)
    }

    fn tools_log_path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")
            .or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        let mut path = PathBuf::from(base);
        path.push("NeoWaves");
        let _ = std::fs::create_dir_all(&path);
        path.push("tool_log.txt");
        Some(path)
    }

    fn write_sample_tools_config(&self) -> std::result::Result<PathBuf, String> {
        let path = Self::tools_config_path()
            .ok_or_else(|| "Could not resolve tools.toml path.".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let sample = r#"# NeoWaves tools config
# Use {path}, {dir}, {stem}, {ext}, {outdir}, {basename}, {cwd}, {args}

[[tool]]
name = "Download Whisper Models"
group = "Setup"
description = "Download whisper.cpp models into the HF cache"
command = "powershell -ExecutionPolicy Bypass -File \"{cwd}\\commands\\download_whisper.ps1\" {args}"
per_file = false
args = ""

[[tool]]
name = "Generate SRT (Root)"
group = "Transcription"
description = "Generate .srt files under a root folder"
command = "powershell -ExecutionPolicy Bypass -File \"{cwd}\\commands\\generate_srt.ps1\" -Root \"{cwd}\" {args}"
per_file = false
args = ""

[[tool]]
name = "Generate SRT (Selection Folder)"
group = "Transcription"
description = "Generate .srt files under the selected file's folder"
command = "powershell -ExecutionPolicy Bypass -File \"{cwd}\\commands\\generate_srt.ps1\" -Root \"{dir}\" {args}"
per_file = false
args = ""
"#;
        std::fs::write(&path, sample).map_err(|e| e.to_string())?;
        Ok(path)
    }

    fn load_prefs(&mut self) {
        let Some(path) = Self::prefs_path() else { return; };
        let Ok(text) = std::fs::read_to_string(path) else { return; };
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
            }
        }
    }

    fn save_prefs(&self) {
        let Some(path) = Self::prefs_path() else { return; };
        let theme = match self.theme_mode {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        };
        let skip = if self.skip_dotfiles { "1" } else { "0" };
        let _ = std::fs::write(path, format!("theme={}\nskip_dotfiles={}\n", theme, skip));
    }

    fn load_tools_config(&mut self) {
        let Some(path) = Self::tools_config_path() else {
            self.tool_defs.clear();
            return;
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => {
                self.tool_defs.clear();
                return;
            }
        };
        let parsed: Result<tooling::ToolsConfig, _> = toml::from_str(&text);
        match parsed {
            Ok(cfg) => {
                self.tool_defs = cfg.tool.unwrap_or_default();
            }
            Err(err) => {
                self.tool_defs.clear();
                self.debug_log(format!("tools.toml parse error: {err}"));
            }
        }
    }

    fn expand_tool_command(template: &str, path: Option<&Path>, args: &str) -> String {
        let empty = std::borrow::Cow::from("");
        let (path_s, dir, stem, ext, basename) = if let Some(path) = path {
            let dir = path.parent().map(|p| p.to_string_lossy()).unwrap_or_default();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            let basename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            (
                path.to_string_lossy(),
                dir,
                std::borrow::Cow::from(stem),
                std::borrow::Cow::from(ext),
                std::borrow::Cow::from(basename),
            )
        } else {
            (empty.clone(), empty.clone(), empty.clone(), empty.clone(), empty.clone())
        };
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        template
            .replace("{path}", &path_s)
            .replace("{dir}", dir.as_ref())
            .replace("{stem}", stem.as_ref())
            .replace("{ext}", ext.as_ref())
            .replace("{outdir}", dir.as_ref())
            .replace("{basename}", basename.as_ref())
            .replace("{cwd}", &cwd)
            .replace("{args}", args)
    }

    fn is_dangerous_command(cmd: &str) -> bool {
        let s = cmd.to_ascii_lowercase();
        let tokens = [" rm ", " del ", " erase ", " rmdir ", " rd ", " mv ", " move "];
        if s.contains('>') {
            return true;
        }
        tokens.iter().any(|t| s.contains(t))
    }

    fn enqueue_tool_runs(&mut self, tool: &ToolDef, paths: &[PathBuf], args: &str) {
        let per_file = tool.per_file.unwrap_or(true);
        if per_file {
            for path in paths {
                let command = Self::expand_tool_command(&tool.command, Some(path), args);
                let job = ToolJob {
                    tool: tool.clone(),
                    path: Some(path.clone()),
                    command,
                };
                self.tool_queue.push_back(job);
            }
        } else {
            let path = paths.get(0).map(|p| p.as_path());
            let command = Self::expand_tool_command(&tool.command, path, args);
            let job = ToolJob {
                tool: tool.clone(),
                path: paths.get(0).cloned(),
                command,
            };
            self.tool_queue.push_back(job);
        }
    }

    fn start_tool_job(&mut self, job: ToolJob) {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<ToolRunResult>();
        self.tool_run_rx = Some(rx);
        self.tool_worker_busy = true;
        std::thread::spawn(move || {
            let result = tooling::run_tool_command(job);
            let _ = tx.send(result);
        });
    }

    fn append_tool_log(&mut self, entry: ToolLogEntry) {
        let log_entry = entry.clone();
        self.tool_log.push_front(entry);
        while self.tool_log.len() > self.tool_log_max {
            self.tool_log.pop_back();
        }
        if let Some(path) = Self::tools_log_path() {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| {
                    let status = if log_entry.ok { "OK" } else { "FAIL" };
                    writeln!(
                        f,
                        "[{:?}] {} {} {}\n{}",
                        log_entry.timestamp,
                        status,
                        log_entry.tool_name,
                        log_entry
                            .path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "(none)".to_string()),
                        log_entry.command
                    )
                });
        }
    }

    fn process_tool_queue(&mut self) {
        if self.tool_worker_busy || self.pending_tool_confirm.is_some() {
            return;
        }
        let Some(job) = self.tool_queue.pop_front() else {
            return;
        };
        let confirm = job.tool.confirm.unwrap_or(false) || Self::is_dangerous_command(&job.command);
        if confirm {
            self.pending_tool_confirm = Some(job);
        } else {
            self.start_tool_job(job);
        }
    }

    fn process_tool_results(&mut self) {
        let Some(rx) = &self.tool_run_rx else { return; };
        if let Ok(result) = rx.try_recv() {
            self.tool_run_rx = None;
            self.tool_worker_busy = false;
            let entry = ToolLogEntry {
                timestamp: std::time::SystemTime::now(),
                tool_name: result.job.tool.name.clone(),
                path: result.job.path.clone(),
                command: result.job.command.clone(),
                ok: result.ok,
                status_code: result.status_code,
                stdout: result.stdout,
                stderr: result.stderr,
                duration: result.duration,
            };
            self.append_tool_log(entry);
        }
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

    fn preview_restore_audio_for_tab(&self, tab_idx: usize) {
        if let Some(tab) = self.tabs.get(tab_idx) {
            // Rebuild mono from current destructive state
            let mono = Self::editor_mixdown_mono(tab);
            self.audio.stop();
            self.audio.set_samples(std::sync::Arc::new(mono));
            // Reapply loop mode
            self.apply_loop_mode_for_tab(tab);
        }
    }
    fn set_preview_mono(&mut self, tab_idx: usize, tool: ToolKind, mono: Vec<f32>) {
        self.audio.stop();
        self.audio.set_samples(std::sync::Arc::new(mono));
        if let Some(tab) = self.tabs.get_mut(tab_idx) { tab.preview_audio_tool = Some(tool); }
        if let Some(tab) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab); }
    }
    fn clear_preview_if_any(&mut self, tab_idx: usize) {
        let had_preview_audio = self
            .tabs
            .get(tab_idx)
            .and_then(|tab| tab.preview_audio_tool)
            .is_some();
        if had_preview_audio {
            self.audio.stop();
            self.preview_restore_audio_for_tab(tab_idx);
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
        }
        // also discard any in-flight preview/overlay job
        self.heavy_preview_rx = None;
        self.heavy_preview_tool = None;
        self.heavy_overlay_rx = None;
        self.overlay_expected_tool = None;
    }
    fn spawn_heavy_preview_owned(&mut self, mono: Vec<f32>, tool: ToolKind, param: f32) {
        use std::sync::mpsc;
        let sr = self.audio.shared.out_sample_rate;        // cancel previous job by dropping receiver
        self.heavy_preview_rx = None; self.heavy_preview_tool = None;
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        std::thread::spawn(move || {
            let out = match tool {
                ToolKind::PitchShift => crate::wave::process_pitchshift_offline(&mono, sr, sr, param),
                ToolKind::TimeStretch => crate::wave::process_timestretch_offline(&mono, sr, sr, param),
                _ => mono,
            };
            let _ = tx.send(out);
        });
        self.heavy_preview_rx = Some(rx);
        self.heavy_preview_tool = Some(tool);
    }

    // Spawn per-channel overlay generator (Pitch/Stretch) in a worker thread.
    // Note: Call this ONLY after UI borrows end (see E0499 note) to avoid nested &mut self borrows.
    fn spawn_heavy_overlay_for_tab(&mut self, tab_idx: usize, tool: ToolKind, param: f32) {
        use std::sync::mpsc;
        // Cancel previous overlay job by dropping receiver
        self.heavy_overlay_rx = None;
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_overlay = None;
            let path = tab.path.clone();
            let ch = tab.ch_samples.clone();
            let sr = self.audio.shared.out_sample_rate;
            let target_len = tab.samples_len;
            // generation guard
            self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
            let gen = self.overlay_gen_counter;
            self.overlay_expected_gen = gen;
            self.overlay_expected_tool = Some(tool);
            let (tx, rx) = mpsc::channel::<(std::path::PathBuf, Vec<Vec<f32>>, usize, u64)>();
            std::thread::spawn(move || {
                let mut out: Vec<Vec<f32>> = Vec::with_capacity(ch.len());
                let mut result_len = target_len;
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
                    result_len = processed.len();
                    out.push(processed);
                }
                let timeline_len = out.get(0).map(|c| c.len()).unwrap_or(result_len).max(1);
                let _ = tx.send((path, out, timeline_len, gen));
            });
            self.heavy_overlay_rx = Some(rx);
        }
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
        self.editor_apply_state = Some(EditorApplyState { msg, rx, tab_idx, undo });
    }
    fn editor_mixdown_mono(tab: &EditorTab) -> Vec<f32> {
        let n = tab.samples_len;
        if n == 0 { return Vec::new(); }
        if tab.ch_samples.is_empty() { return vec![0.0; n]; }
        let chn = tab.ch_samples.len() as f32;
        let mut out = vec![0.0f32; n];
        for ch in &tab.ch_samples { for i in 0..n { if let Some(&v)=ch.get(i) { out[i]+=v; } } }
        for v in &mut out { *v /= chn; }
        out
    }
    fn draw_spectrogram(
        painter: &egui::Painter,
        rect: egui::Rect,
        wave_left: f32,
        wave_w: f32,
        tab: &EditorTab,
        spec: &SpectrogramData,
        view_mode: ViewMode,
    ) {
        if spec.frames == 0 || spec.bins == 0 {
            return;
        }
        let area = egui::Rect::from_min_max(
            egui::pos2(wave_left, rect.top()),
            egui::pos2(rect.right(), rect.bottom()),
        );
        let width_px = area.width().max(1.0);
        let height_px = area.height().max(1.0);
        let spp = tab.samples_per_px.max(0.0001);
        let vis = (wave_w * spp).ceil() as usize;
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
        let mel_max = 2595.0 * (1.0 + (sr * 0.5) / 700.0).log10();
        for x in 0..target_w {
            let frame_idx = f0 + ((x * frame_count) / target_w).min(frame_count - 1);
            let base = frame_idx * spec.bins;
            for y in 0..target_h {
                let frac = 1.0 - (y as f32 / (target_h.saturating_sub(1)) as f32);
                let bin = match view_mode {
                    ViewMode::Spectrogram | ViewMode::Waveform => {
                        (frac * max_bin as f32).round() as usize
                    }
                    ViewMode::Mel => {
                        let mel = frac * mel_max;
                        let freq = 700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0);
                        let pos = (freq / (sr * 0.5)).clamp(0.0, 1.0);
                        (pos * max_bin as f32).round() as usize
                    }
                };
                let idx = base + bin.min(max_bin);
                let db = spec.values_db.get(idx).copied().unwrap_or(-120.0).clamp(-80.0, 0.0);
                let col = db_to_color(db);
                let x0 = area.left() + x as f32 * cell_w;
                let y0 = area.bottom() - (y as f32 + 1.0) * cell_h;
                let r = egui::Rect::from_min_size(egui::pos2(x0, y0), egui::vec2(cell_w + 0.5, cell_h + 0.5));
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
            LoopMode::Off => { self.audio.set_loop_enabled(false); }
            LoopMode::OnWhole => {
                self.audio.set_loop_enabled(true);
                if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                    let len = buf.len();
                    self.audio.set_loop_region(0, len);
                    let cf = Self::effective_loop_xfade_samples(0, len, len, tab.loop_xfade_samples);
                    self.audio.set_loop_crossfade(cf, match tab.loop_xfade_shape { crate::app::types::LoopXfadeShape::Linear => 0, crate::app::types::LoopXfadeShape::EqualPower => 1 });
                }
            }
            LoopMode::Marker => {
                if let Some((a,b)) = tab.loop_region { if a!=b { let (s,e) = if a<=b {(a,b)} else {(b,a)}; self.audio.set_loop_enabled(true); self.audio.set_loop_region(s,e); let cf = Self::effective_loop_xfade_samples(s, e, tab.samples_len, tab.loop_xfade_samples); self.audio.set_loop_crossfade(cf, match tab.loop_xfade_shape { crate::app::types::LoopXfadeShape::Linear => 0, crate::app::types::LoopXfadeShape::EqualPower => 1 }); return; } }
                self.audio.set_loop_enabled(false);
            }
        }
    }
    #[allow(dead_code)]
    fn set_marker_sample(tab: &mut EditorTab, idx: usize) {
        match tab.loop_region {
            None => tab.loop_region = Some((idx, idx)),
            Some((a,b)) => {
                if a==b { tab.loop_region = Some((a.min(idx), a.max(idx))); }
                else { let da = a.abs_diff(idx); let db = b.abs_diff(idx); if da <= db { tab.loop_region = Some((idx, b)); } else { tab.loop_region = Some((a, idx)); } }
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
        self.item_index.get(&id).and_then(|&idx| self.items.get(idx))
    }

    fn item_for_id_mut(&mut self, id: MediaId) -> Option<&mut MediaItem> {
        let idx = *self.item_index.get(&id)?;
        self.items.get_mut(idx)
    }

    fn item_for_path(&self, path: &Path) -> Option<&MediaItem> {
        let id = *self.path_index.get(path)?;
        self.item_for_id(id)
    }

    fn item_for_path_mut(&mut self, path: &Path) -> Option<&mut MediaItem> {
        let id = *self.path_index.get(path)?;
        self.item_for_id_mut(id)
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
        self.item_for_path(path).and_then(|item| item.transcript.as_ref())
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

    fn make_media_item(&mut self, path: PathBuf) -> MediaItem {
        let id = self.next_media_id;
        self.next_media_id = self.next_media_id.wrapping_add(1);
        let mut item = MediaItem {
            id,
            path,
            meta: None,
            pending_gain_db: 0.0,
            status: MediaStatus::Ok,
            transcript: None,
            external: HashMap::new(),
        };
        self.fill_external_for_item(&mut item);
        item
    }

    fn fill_external_for_item(&self, item: &mut MediaItem) {
        item.external = self.external_row_for_path(&item.path).unwrap_or_default();
    }

    fn external_row_for_path(&self, path: &Path) -> Option<HashMap<String, String>> {
        if self.external_lookup.is_empty() {
            return None;
        }
        for key in self.external_keys_for_path(path) {
            if let Some(row) = self.external_lookup.get(&key) {
                return Some(row.clone());
            }
        }
        None
    }

    fn external_keys_for_path(&self, path: &Path) -> Vec<String> {
        let pat = self.external_match_regex.trim();
        let re = if pat.is_empty() {
            None
        } else {
            regex::Regex::new(pat).ok()
        };
        Self::external_keys_for_path_with_rule(
            path,
            self.external_key_rule,
            re.as_ref(),
            &self.external_match_replace,
        )
    }

    fn external_keys_for_path_with_rule(
        path: &Path,
        rule: ExternalKeyRule,
        re: Option<&regex::Regex>,
        replace: &str,
    ) -> Vec<String> {
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match rule {
            ExternalKeyRule::FileName => {
                if file_name.is_empty() { Vec::new() } else { vec![file_name] }
            }
            ExternalKeyRule::Stem => {
                if stem.is_empty() { Vec::new() } else { vec![stem] }
            }
            ExternalKeyRule::Regex => {
                if let Some(re) = re {
                    let replaced = re
                        .replace_all(&stem, replace)
                        .to_string()
                        .to_ascii_lowercase();
                    if replaced.is_empty() { Vec::new() } else { vec![replaced] }
                } else if stem.is_empty() {
                    Vec::new()
                } else {
                    vec![stem]
                }
            }
        }
    }

    fn rebuild_external_lookup(&mut self) {
        self.external_lookup.clear();
        let Some(key_idx) = self.external_key_index else {
            return;
        };
        for row in &self.external_rows {
            let Some(key_raw) = row.get(key_idx) else {
                continue;
            };
            let key = key_raw.trim().to_ascii_lowercase();
            if key.is_empty() {
                continue;
            }
            let mut map = HashMap::new();
            for (idx, header) in self.external_headers.iter().enumerate() {
                if let Some(val) = row.get(idx) {
                    let trimmed = val.trim();
                    if !trimmed.is_empty() {
                        map.insert(header.clone(), trimmed.to_string());
                    }
                }
            }
            self.external_lookup.insert(key, map);
        }
    }

    fn apply_external_mapping(&mut self) {
        self.external_match_count = 0;
        self.external_unmatched_count = 0;
        if self.external_source.is_none() {
            for item in &mut self.items {
                item.external.clear();
            }
            return;
        }
        if self.external_lookup.is_empty() {
            for item in &mut self.items {
                item.external.clear();
            }
            self.external_unmatched_count = self.items.len();
            return;
        }
        let lookup = self.external_lookup.clone();
        let rule = self.external_key_rule;
        let pat = self.external_match_regex.trim().to_string();
        let replace = self.external_match_replace.clone();
        let re = if pat.is_empty() {
            None
        } else {
            regex::Regex::new(&pat).ok()
        };
        for item in &mut self.items {
            let mut matched = false;
            let mut row = None;
            for key in Self::external_keys_for_path_with_rule(
                &item.path,
                rule,
                re.as_ref(),
                &replace,
            ) {
                if let Some(found) = lookup.get(&key) {
                    row = Some(found.clone());
                    break;
                }
            }
            if let Some(found) = row {
                item.external = found;
                matched = true;
            } else {
                item.external.clear();
            }
            if matched {
                self.external_match_count += 1;
            } else {
                self.external_unmatched_count += 1;
            }
        }
    }

    fn default_external_columns(headers: &[String], key_idx: usize) -> Vec<String> {
        headers
            .iter()
            .enumerate()
            .filter(|(idx, _)| *idx != key_idx)
            .take(3)
            .map(|(_, h)| h.clone())
            .collect()
    }

    fn load_external_source(&mut self, path: PathBuf) -> std::result::Result<(), String> {
        let Some(table) = external::load_table(&path) else {
            return Err("Unsupported or empty data source.".to_string());
        };
        if table.headers.is_empty() {
            return Err("No headers found in data source.".to_string());
        }
        self.external_source = Some(path);
        self.external_headers = table.headers;
        self.external_rows = table.rows;
        let key_idx = self
            .external_key_index
            .filter(|&idx| idx < self.external_headers.len())
            .unwrap_or(0);
        self.external_key_index = Some(key_idx);
        self.external_visible_columns = Self::default_external_columns(&self.external_headers, key_idx);
        self.rebuild_external_lookup();
        self.apply_external_mapping();
        self.apply_filter_from_search();
        self.apply_sort();
        Ok(())
    }

    fn clear_external_data(&mut self) {
        self.external_source = None;
        self.external_headers.clear();
        self.external_rows.clear();
        self.external_key_index = None;
        self.external_visible_columns.clear();
        self.external_lookup.clear();
        self.external_match_count = 0;
        self.external_unmatched_count = 0;
        self.external_load_error = None;
        for item in &mut self.items {
            item.external.clear();
        }
        self.apply_filter_from_search();
        self.apply_sort();
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
        self.selected
            .and_then(|i| self.path_for_row(i).cloned())
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
        rows
            .into_iter()
            .filter_map(|row| self.path_for_row(row).cloned())
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
        if let Some(i) = self.active_tab { return self.tabs.get(i).map(|t| &t.path); }
        if let Some(i) = self.selected { return self.path_for_row(i); }
        None
    }
    pub(super) fn apply_effective_volume(&self) {
        // Global output volume (0..1)
        let base = db_to_amp(self.volume_db);
        self.audio.set_volume(base);
        // Per-file gain (can be >1)
        let path_opt = self.playing_path.as_ref().or_else(|| self.current_active_path());
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
            item.transcript = None;
            item.external = external.unwrap_or_default();
        }
        self.path_index.remove(from);
        self.path_index.insert(new_path.clone(), id);
        if let Some(v) = self.spectro_cache.remove(from) {
            self.spectro_cache.insert(new_path.clone(), v);
        }
        if let Some(v) = self.edited_cache.remove(from) {
            self.edited_cache.insert(new_path.clone(), v);
        }
        self.meta_inflight.remove(from);
        self.transcript_inflight.remove(from);
        self.spectro_inflight.remove(from);
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

    fn unique_copy_target(dest_dir: &std::path::Path, src: &PathBuf) -> PathBuf {
        let name = src
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("copy");
        let base = src
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("copy");
        let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("");
        let mut candidate = dest_dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
        for i in 1.. {
            let suffix = if i == 1 {
                " (copy)".to_string()
            } else {
                format!(" (copy {i})")
            };
            let file = if ext.is_empty() {
                format!("{base}{suffix}")
            } else {
                format!("{base}{suffix}.{ext}")
            };
            candidate = dest_dir.join(file);
            if !candidate.exists() {
                return candidate;
            }
        }
        candidate
    }

    fn copy_paths_to_folder(&mut self, paths: &[PathBuf], dest_dir: &PathBuf) {
        if paths.is_empty() {
            return;
        }
        let mut added: Vec<PathBuf> = Vec::new();
        for src in paths {
            if !src.is_file() {
                continue;
            }
            let dst = Self::unique_copy_target(dest_dir, src);
            match std::fs::copy(src, &dst) {
                Ok(_) => added.push(dst),
                Err(e) => eprintln!("copy failed: {} -> {} ({})", src.display(), dst.display(), e),
            }
        }
        if !added.is_empty() {
            if self.add_files_merge(&added) > 0 {
                self.after_add_refresh();
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
        if targets.is_empty() { return; }
        let (tx, rx) = mpsc::channel::<ExportResult>();
        std::thread::spawn(move || {
            let mut ok = 0usize; let mut failed = 0usize; let mut success_paths = Vec::new(); let mut failed_paths = Vec::new();
            for (src, db) in targets {
                let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("");
                let dst = if ext.is_empty() {
                    src.with_file_name(format!("{} (gain{:+.1}dB)", stem, db))
                } else {
                    src.with_file_name(format!("{} (gain{:+.1}dB).{}", stem, db, ext))
                };
                match crate::wave::export_gain_audio(&src, &dst, db) { Ok(_) => { ok += 1; success_paths.push(dst); }, Err(e) => { eprintln!("export failed {}: {e:?}", src.display()); failed += 1; failed_paths.push(src.clone()); } }
            }
            let _ = tx.send(ExportResult{ ok, failed, success_paths, failed_paths });
        });
        self.export_state = Some(ExportState{ msg: "Exporting gains".into(), rx });
    }


    fn trigger_save_selected(&mut self) {
        if self.export_cfg.first_prompt { self.show_first_save_prompt = true; return; }
        let mut set = self.selected_multi.clone();
        if set.is_empty() { if let Some(i) = self.selected { set.insert(i); } }
        self.spawn_save_selected(set);
    }

    fn spawn_save_selected(&mut self, indices: std::collections::BTreeSet<usize>) {
        use std::sync::mpsc;
        if indices.is_empty() { return; }
        let mut items: Vec<(PathBuf, f32)> = Vec::new();
        for i in indices {
            if let Some(p) = self.path_for_row(i) {
                let db = self.pending_gain_db_for_path(p);
                if db.abs() > 0.0001 {
                    items.push((p.clone(), db));
                }
            }
        }
        if items.is_empty() { return; }
        let cfg = self.export_cfg.clone();
        // remember sources for post-save cleanup + reload
        self.saving_sources = items.iter().map(|(p,_)| p.clone()).collect();
        self.saving_mode = Some(cfg.save_mode);
        let (tx, rx) = mpsc::channel::<ExportResult>();
        std::thread::spawn(move || {
            let mut ok=0usize; let mut failed=0usize; let mut success_paths=Vec::new(); let mut failed_paths=Vec::new();
            for (src, db) in items {
                match cfg.save_mode {
                    SaveMode::Overwrite => {
                        match crate::wave::overwrite_gain_audio(&src, db, cfg.backup_bak) {
                            Ok(()) => { ok+=1; success_paths.push(src.clone()); },
                            Err(_)  => { failed+=1; failed_paths.push(src.clone()); }
                        }
                    }
                    SaveMode::NewFile => {
                        let parent = cfg.dest_folder.clone().unwrap_or_else(|| src.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf());
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
                        let use_dst_ext = dst_ext.map(|e| crate::audio_io::is_supported_extension(e)).unwrap_or(false);
                        if !use_dst_ext {
                            dst.set_extension(src_ext);
                        }
                        if dst.exists() {
                            match cfg.conflict {
                                ConflictPolicy::Overwrite => {}
                                ConflictPolicy::Skip => { failed+=1; failed_paths.push(src.clone()); continue; }
                                ConflictPolicy::Rename => {
                                    let orig = dst.clone();
                                    let orig_ext = orig.extension().and_then(|e| e.to_str()).unwrap_or("");
                                    let mut idx=1u32; loop {
                                        let stem2 = orig.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                                        let n = crate::app::helpers::sanitize_filename_component(&format!("{}_{:02}", stem2, idx));
                                        dst = orig.with_file_name(n);
                                        if !orig_ext.is_empty() {
                                            dst.set_extension(orig_ext);
                                        }
                                        if !dst.exists() { break; }
                                        idx+=1; if idx>999 { break; }
                                    }
                                }
                            }
                        }
                        match crate::wave::export_gain_audio(&src, &dst, db) {
                            Ok(()) => { ok+=1; success_paths.push(dst.clone()); },
                            Err(_)  => { failed+=1; failed_paths.push(src.clone()); }
                        }
                    }
                }
            }
            let _=tx.send(ExportResult{ ok, failed, success_paths, failed_paths });
        });
        self.export_state = Some(ExportState{ msg: "Saving...".into(), rx });
    }

    // moved to logic.rs: update_selection_on_click

    // --- Gain helpers ---
    fn clamp_gain_db(val: f32) -> f32 {
        let mut g = val.clamp(-24.0, 24.0);
        if g.abs() < 0.001 { g = 0.0; }
        g
    }

    fn adjust_gain_for_indices(&mut self, indices: &std::collections::BTreeSet<usize>, delta_db: f32) {
        if indices.is_empty() { return; }
        let mut affect_playing = false;
        for &i in indices {
            if let Some(p) = self.path_for_row(i).cloned() {
                let cur = self.pending_gain_db_for_path(&p);
                let new = Self::clamp_gain_db(cur + delta_db);
                self.set_pending_gain_db_for_path(&p, new);
                if self.playing_path.as_ref() == Some(&p) { affect_playing = true; }
                // schedule LUFS recompute for each affected path
                self.schedule_lufs_for_path(p.clone());
            }
        }
        if affect_playing { self.apply_effective_volume(); }
    }

    fn schedule_lufs_for_path(&mut self, path: PathBuf) {
        use std::time::{Duration, Instant};
        let dl = Instant::now() + Duration::from_millis(400);
        self.lufs_recalc_deadline.insert(path, dl);
    }

    fn reset_meta_pool(&mut self) {
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(6);
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

    fn ensure_spectro_channel(&mut self) {
        if self.spectro_tx.is_none() || self.spectro_rx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<(PathBuf, SpectrogramData)>();
            self.spectro_tx = Some(tx);
            self.spectro_rx = Some(rx);
        }
    }

    fn spawn_spectrogram_job(&mut self, path: PathBuf, mono: Vec<f32>, sample_rate: u32) {
        self.ensure_spectro_channel();
        let Some(tx) = self.spectro_tx.as_ref().cloned() else { return; };
        std::thread::spawn(move || {
            let data = crate::app::render::spectrogram::compute_spectrogram(&mono, sample_rate);
            let _ = tx.send((path, data));
        });
    }

    fn queue_spectrogram_for_tab(&mut self, tab_idx: usize) {
        let Some(tab) = self.tabs.get(tab_idx) else { return; };
        if tab.view_mode == ViewMode::Waveform {
            return;
        }
        if tab.samples_len > LIVE_PREVIEW_SAMPLE_LIMIT {
            return;
        }
        if self.spectro_cache.contains_key(&tab.path) || self.spectro_inflight.contains(&tab.path) {
            return;
        }
        let mono = Self::editor_mixdown_mono(tab);
        let sr = self.audio.shared.out_sample_rate;
        self.spectro_inflight.insert(tab.path.clone());
        self.spawn_spectrogram_job(tab.path.clone(), mono, sr);
    }

    fn start_scan_folder(&mut self, dir: PathBuf) {
        self.scan_rx = Some(self.spawn_scan_worker(dir, self.skip_dotfiles));
        self.scan_in_progress = true;
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.files.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
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
        for p in batch {
            if self.path_index.contains_key(&p) {
                continue;
            }
            let item = self.make_media_item(p.clone());
            let id = item.id;
            self.path_index.insert(p.clone(), id);
            self.item_index.insert(id, self.items.len());
            self.items.push(item);
            if !has_search {
                self.files.push(id);
                self.original_files.push(id);
            } else {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
                let parent = p.parent().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
                let matches = name.contains(&query) || parent.contains(&query);
                if matches {
                    self.files.push(id);
                    self.original_files.push(id);
                }
            }
        }
    }

    fn process_scan_messages(&mut self) {
        let Some(rx) = &self.scan_rx else { return; };
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
                Ok(ScanMessage::Done) => { done = true; break; }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => { done = true; break; }
            }
        }

        for batch in batches {
            self.append_scanned_paths(batch);
        }

        if done {
            self.scan_rx = None;
            self.scan_in_progress = false;
            if self.external_source.is_some() {
                self.apply_external_mapping();
            }
            self.apply_filter_from_search();
            self.apply_sort();
        }
    }

    fn process_mcp_commands(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.mcp_cmd_rx else { return; };
        let Some(tx) = self.mcp_resp_tx.clone() else { return; };
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
                let selected_paths: Vec<String> =
                    self.selected_paths().iter().map(|p| p.display().to_string()).collect();
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
                let selected_paths: Vec<String> =
                    self.selected_paths().iter().map(|p| p.display().to_string()).collect();
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
        if !query.is_empty() {
            let re = if use_regex {
                RegexBuilder::new(&query).case_insensitive(true).build().ok()
            } else {
                RegexBuilder::new(&regex::escape(&query))
                    .case_insensitive(true)
                    .build()
                    .ok()
            };
            ids.retain(|id| {
                let Some(item) = self.item_for_id(*id) else { return false; };
                let name = item
                    .path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let parent = item
                    .path
                    .parent()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
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
            let Some(item) = self.item_for_id(id) else { continue; };
            let path = item.path.display().to_string();
            let name = item
                .path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let folder = item
                .path
                .parent()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
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
        let samples = ((start_ms * sr) / 1000) as usize;
        self.audio.seek_to_sample(samples);
        self.pending_transcript_seek = None;
    }

    fn schedule_search_refresh(&mut self) {
        self.search_dirty = true;
        self.search_deadline = Some(std::time::Instant::now() + Duration::from_millis(300));
    }

    fn apply_search_if_due(&mut self) {
        let Some(deadline) = self.search_deadline else { return; };
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

    fn apply_startup_paths(&mut self) {
        let cfg = self.startup.cfg.clone();
        if let Some(count) = cfg.dummy_list_count {
            self.populate_dummy_list(count);
            self.startup.open_first_pending = false;
            return;
        }
        if !cfg.open_files.is_empty() {
            self.replace_with_files(&cfg.open_files);
            self.after_add_refresh();
            return;
        }
        if let Some(dir) = cfg.open_folder {
            self.root = Some(dir);
            self.rescan();
        }
    }

    fn open_first_in_list(&mut self) {
        if let Some(id) = self.files.first().copied() {
            let Some(item) = self.item_for_id(id) else {
                return;
            };
            let path = item.path.clone();
            self.selected = Some(0);
            self.selected_multi.clear();
            self.selected_multi.insert(0);
            self.select_anchor = Some(0);
            self.open_or_activate_tab(&path);
        }
    }

    fn run_startup_actions(&mut self, ctx: &egui::Context) {
        if self.startup.open_first_pending && !self.files.is_empty() {
            self.open_first_in_list();
            self.startup.open_first_pending = false;
        }

        if self.startup.screenshot_pending {
            let wait_for_tab = self.startup.cfg.open_first;
            let ready = if wait_for_tab {
                self.active_tab.is_some()
            } else {
                true
            };
            if ready {
                if self.startup.screenshot_frames_left > 0 {
                    self.startup.screenshot_frames_left = self.startup.screenshot_frames_left.saturating_sub(1);
                } else if let Some(path) = self.startup.cfg.screenshot_path.clone() {
                    self.request_screenshot(ctx, path, self.startup.cfg.exit_after_screenshot);
                    self.startup.screenshot_pending = false;
                }
            }
        }
    }

    fn default_screenshot_path(&mut self) -> PathBuf {
        let tag = if self.active_tab.is_some() { "editor" } else { "list" };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let base = Self::default_screenshot_dir();
        let mut name = format!("shot_{}_{}", stamp, tag);
        if self.screenshot_seq > 0 {
            name.push_str(&format!("_{:02}", self.screenshot_seq));
        }
        self.screenshot_seq = self.screenshot_seq.wrapping_add(1);
        base.join(format!("{name}.png"))
    }

    fn default_screenshot_dir() -> PathBuf {
        #[cfg(target_os = "windows")]
        {
            if let Some(home) = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            {
                return home.join("Pictures").join("Screenshots");
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                return home.join("Desktop");
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                return home.join("Pictures").join("Screenshots");
            }
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    fn request_screenshot(&mut self, ctx: &egui::Context, path: PathBuf, exit_after: bool) {
        if self.pending_screenshot.is_some() {
            return;
        }
        self.pending_screenshot = Some(path);
        self.exit_after_screenshot = exit_after;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }

    fn handle_screenshot_events(&mut self, ctx: &egui::Context) {
        let mut shot: Option<std::sync::Arc<egui::ColorImage>> = None;
        ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Screenshot { image, .. } = ev {
                    shot = Some(image.clone());
                }
            }
        });
        if let Some(image) = shot {
            if let Some(path) = self.pending_screenshot.take() {
                if let Err(err) = save_color_image_png(&path, &image) {
                    eprintln!("screenshot failed: {err:?}");
                    self.debug_log(format!("screenshot failed: {err}"));
                } else {
                    eprintln!("screenshot saved: {}", path.display());
                    self.debug_log(format!("screenshot saved: {}", path.display()));
                }
                if self.exit_after_screenshot {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    self.exit_after_screenshot = false;
                }
            }
        }
    }

    fn debug_log(&mut self, msg: impl Into<String>) {
        if !self.debug.cfg.enabled {
            return;
        }
        let msg = msg.into();
        self.debug.logs.push_back(msg.clone());
        while self.debug.logs.len() > 200 {
            self.debug.logs.pop_front();
        }
        if let Some(path) = self.debug.cfg.log_path.as_ref() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{msg}");
            }
        }
    }

    fn debug_summary(&self) -> String {
        let selected = self.selected_path_buf().map(|p| p.display().to_string()).unwrap_or_else(|| "(none)".to_string());
        let active_tab = self.active_tab.and_then(|i| self.tabs.get(i)).map(|t| t.display_name.clone()).unwrap_or_else(|| "(none)".to_string());
        let playing = self.audio.shared.playing.load(std::sync::atomic::Ordering::Relaxed);
        let loop_enabled = self.audio.shared.loop_enabled.load(std::sync::atomic::Ordering::Relaxed);
        let processing = self.processing.is_some();
        let export = self.export_state.is_some();
        let meta_pending = self.scan_in_progress || !self.meta_inflight.is_empty();
        let gain_dirty = self.pending_gain_count();
        let mut lines = Vec::new();
        lines.push(format!("files: {}/{}", self.files.len(), self.items.len()));
        lines.push(format!("selected: {selected}"));
        lines.push(format!("tabs: {} (active: {active_tab})", self.tabs.len()));
        lines.push(format!("mode: {:?} rate {:.2} pitch {:.2}", self.mode, self.playback_rate, self.pitch_semitones));
        lines.push(format!("playing: {} loop: {} meter_db: {:.1}", playing, loop_enabled, self.meter_db));
        lines.push(format!("pending_gains: {}", gain_dirty));
        lines.push(format!("processing: {} export: {}", processing, export));
        lines.push(format!("meta_pending: {}", meta_pending));
        lines.join("\n")
    }

    fn default_debug_summary_path(&mut self) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut name = format!("summary_{stamp}");
        if self.debug_summary_seq > 0 {
            name.push_str(&format!("_{:02}", self.debug_summary_seq));
        }
        self.debug_summary_seq = self.debug_summary_seq.wrapping_add(1);
        base.join("debug").join(format!("{name}.txt"))
    }

    fn save_debug_summary(&mut self, path: PathBuf) {
        let summary = self.debug_summary();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&path, summary) {
            Ok(()) => self.debug_log(format!("debug summary saved: {}", path.display())),
            Err(err) => self.debug_log(format!("debug summary failed: {err}")),
        }
    }

    fn debug_check_invariants(&mut self) {
        let mut issues = Vec::new();
        if let Some(i) = self.selected {
            if i >= self.files.len() {
                issues.push(format!("selected out of range: {i}"));
            }
        }
        if let Some(i) = self.active_tab {
            if i >= self.tabs.len() {
                issues.push(format!("active_tab out of range: {i}"));
            }
        }
        if let Some(i) = self.active_tab {
            if let Some(tab) = self.tabs.get(i) {
                if tab.view_offset > tab.samples_len {
                    issues.push(format!("view_offset > samples_len: {} > {}", tab.view_offset, tab.samples_len));
                }
                if tab.samples_per_px <= 0.0 {
                    issues.push(format!("samples_per_px <= 0: {}", tab.samples_per_px));
                }
                for (ci, ch) in tab.ch_samples.iter().enumerate() {
                    if ch.len() != tab.samples_len {
                        issues.push(format!("channel {ci} len {} != {}", ch.len(), tab.samples_len));
                        break;
                    }
                }
                if let Some((a, b)) = tab.loop_region {
                    if a > tab.samples_len || b > tab.samples_len {
                        issues.push(format!("loop_region out of range: {a}..{b} len {}", tab.samples_len));
                    }
                }
            }
        }
        if issues.is_empty() {
            self.debug_log("invariant check ok");
        } else {
            for issue in issues {
                self.debug_log(format!("invariant: {issue}"));
            }
        }
    }

    fn setup_debug_automation(&mut self) {
        if !self.debug.cfg.auto_run {
            return;
        }
        let delay = self.debug.cfg.auto_run_delay_frames.max(1);
        let mut steps = std::collections::VecDeque::new();
        let pitch = self.debug.cfg.auto_run_pitch_shift_semitones;
        let stretch = self.debug.cfg.auto_run_time_stretch_rate;
        if pitch.is_some() || stretch.is_some() {
            steps.push_back(DebugStep {
                wait_frames: delay,
                action: DebugAction::OpenFirst,
            });
            steps.push_back(DebugStep {
                wait_frames: delay,
                action: DebugAction::ScreenshotAuto,
            });
            if let Some(semi) = pitch {
                steps.push_back(DebugStep {
                    wait_frames: delay,
                    action: DebugAction::PreviewPitchShift(semi),
                });
                steps.push_back(DebugStep {
                    wait_frames: delay,
                    action: DebugAction::ScreenshotAuto,
                });
            }
            if let Some(rate) = stretch {
                steps.push_back(DebugStep {
                    wait_frames: delay,
                    action: DebugAction::PreviewTimeStretch(rate),
                });
                steps.push_back(DebugStep {
                    wait_frames: delay,
                    action: DebugAction::ScreenshotAuto,
                });
            }
            steps.push_back(DebugStep {
                wait_frames: delay,
                action: DebugAction::DumpSummaryAuto,
            });
            if self.debug.cfg.auto_run_exit {
                steps.push_back(DebugStep {
                    wait_frames: 1,
                    action: DebugAction::Exit,
                });
            }
            self.debug.auto = Some(DebugAutomation { steps });
            return;
        }
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::OpenFirst,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::ScreenshotAuto,
        });
        steps.push_back(DebugStep {
            wait_frames: delay,
            action: DebugAction::DumpSummaryAuto,
        });
        if self.debug.cfg.auto_run_exit {
            steps.push_back(DebugStep {
                wait_frames: 1,
                action: DebugAction::Exit,
            });
        }
        self.debug.auto = Some(DebugAutomation { steps });
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

    fn debug_tick(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.key_pressed(Key::F12)) {
            self.debug.cfg.enabled = true;
            self.debug.show_window = !self.debug.show_window;
        }

        if !self.debug.cfg.enabled {
            return;
        }

        if self.debug.check_counter > 0 {
            self.debug.check_counter = self.debug.check_counter.saturating_sub(1);
        } else {
            self.debug_check_invariants();
            self.debug.check_counter = self.debug.cfg.check_interval_frames.max(1);
        }

        self.debug_run_automation(ctx);
    }

    fn debug_run_automation(&mut self, ctx: &egui::Context) {
        if self.files.is_empty() {
            return;
        }

        let action = {
            let Some(auto) = self.debug.auto.as_mut() else { return; };
            if let Some(step) = auto.steps.front_mut() {
                if step.wait_frames > 0 {
                    step.wait_frames = step.wait_frames.saturating_sub(1);
                    return;
                }
            }
            auto.steps.pop_front().map(|step| step.action)
        };

        if let Some(action) = action {
            self.execute_debug_action(ctx, action);
        }

        let completed = self
            .debug
            .auto
            .as_ref()
            .map(|auto| auto.steps.is_empty())
            .unwrap_or(false);

        if completed {
            self.debug.auto = None;
            self.debug_log("auto-run complete");
        }
    }

    fn execute_debug_action(&mut self, ctx: &egui::Context, action: DebugAction) {
        match action {
            DebugAction::OpenFirst => {
                if self.active_tab.is_none() {
                    self.open_first_in_list();
                    self.debug_log("auto: open first");
                }
            }
            DebugAction::ScreenshotAuto => {
                let path = self.default_screenshot_path();
                self.request_screenshot(ctx, path, false);
                self.debug_log("auto: screenshot");
            }
            DebugAction::ScreenshotPath(path) => {
                self.request_screenshot(ctx, path, false);
                self.debug_log("auto: screenshot path");
            }
            DebugAction::ToggleMode => {
                self.mode = match self.mode {
                    RateMode::Speed => RateMode::PitchShift,
                    RateMode::PitchShift => RateMode::TimeStretch,
                    RateMode::TimeStretch => RateMode::Speed,
                };
                self.rebuild_current_buffer_with_mode();
                self.debug_log("auto: toggle mode");
            }
            DebugAction::PlayPause => {
                self.audio.toggle_play();
                self.debug_log("auto: play/pause");
            }
            DebugAction::SelectNext => {
                if let Some(cur) = self.selected {
                    let next = (cur + 1).min(self.files.len().saturating_sub(1));
                    self.select_and_load(next, true);
                    self.debug_log(format!("auto: select {next}"));
                }
            }
            DebugAction::PreviewTimeStretch(rate) => {
                if let Some(tab_idx) = self.active_tab {
                    let mono = {
                        let tab = &mut self.tabs[tab_idx];
                        tab.active_tool = ToolKind::TimeStretch;
                        tab.tool_state.stretch_rate = rate;
                        tab.preview_audio_tool = Some(ToolKind::TimeStretch);
                        tab.preview_overlay = None;
                        Self::editor_mixdown_mono(tab)
                    };
                    self.spawn_heavy_preview_owned(mono, ToolKind::TimeStretch, rate);
                    self.spawn_heavy_overlay_for_tab(tab_idx, ToolKind::TimeStretch, rate);
                    self.debug_log("auto: preview time stretch");
                } else {
                    self.debug_log("auto: preview time stretch skipped (no tab)");
                }
            }
            DebugAction::PreviewPitchShift(semi) => {
                if let Some(tab_idx) = self.active_tab {
                    let mono = {
                        let tab = &mut self.tabs[tab_idx];
                        tab.active_tool = ToolKind::PitchShift;
                        tab.tool_state.pitch_semitones = semi;
                        tab.preview_audio_tool = Some(ToolKind::PitchShift);
                        tab.preview_overlay = None;
                        Self::editor_mixdown_mono(tab)
                    };
                    self.spawn_heavy_preview_owned(mono, ToolKind::PitchShift, semi);
                    self.spawn_heavy_overlay_for_tab(tab_idx, ToolKind::PitchShift, semi);
                    self.debug_log("auto: preview pitch shift");
                } else {
                    self.debug_log("auto: preview pitch shift skipped (no tab)");
                }
            }
            DebugAction::DumpSummaryAuto => {
                let path = self.default_debug_summary_path();
                self.save_debug_summary(path);
            }
            DebugAction::Exit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
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
            spectro_tx: None,
            spectro_rx: None,
            scan_rx: None,
            scan_in_progress: false,
            wave_row_h: 26.0,
            list_columns: ListColumnConfig::default(),
            selected_multi: std::collections::BTreeSet::new(),
            select_anchor: None,
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

    fn pick_folder_dialog(&mut self) -> Option<PathBuf> {
        #[cfg(feature = "kittest")]
        {
            return self.test_dialogs.next_folder();
        }
        #[cfg(not(feature = "kittest"))]
        {
            rfd::FileDialog::new().pick_folder()
        }
    }

    fn estimate_state_bytes(tab: &EditorTab) -> usize {
        let sample_bytes = tab
            .ch_samples
            .iter()
            .map(|c| c.len())
            .sum::<usize>()
            * std::mem::size_of::<f32>();
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
            tab.drag_select_anchor = None;
            tab.dragging_marker = None;
            tab.preview_offset_samples = None;
            tab.dirty = state.dirty;
            Self::update_loop_markers_dirty(tab);
        }
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let mono = Self::editor_mixdown_mono(tab);
        self.audio.stop();
        self.audio.set_samples(std::sync::Arc::new(mono));
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

    fn pick_files_dialog(&mut self) -> Option<Vec<PathBuf>> {
        #[cfg(feature = "kittest")]
        {
            return self.test_dialogs.next_files();
        }
        #[cfg(not(feature = "kittest"))]
        {
            rfd::FileDialog::new()
                .add_filter("Audio", crate::audio_io::SUPPORTED_EXTS)
                .pick_files()
        }
    }

    fn pick_external_file_dialog(&mut self) -> Option<PathBuf> {
        #[cfg(feature = "kittest")]
        {
            return None;
        }
        #[cfg(not(feature = "kittest"))]
        {
            rfd::FileDialog::new()
                .add_filter("CSV", &["csv"])
                .pick_file()
        }
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

    #[cfg(feature = "kittest")]
    pub fn test_queue_folder_dialog(&mut self, path: Option<PathBuf>) {
        self.test_dialogs.push_folder(path);
    }

    #[cfg(feature = "kittest")]
    pub fn test_queue_files_dialog(&mut self, files: Option<Vec<PathBuf>>) {
        self.test_dialogs.push_files(files);
    }

    #[cfg(feature = "kittest")]
    pub fn test_simulate_drop_paths(&mut self, paths: &[PathBuf]) -> usize {
        let added = self.add_files_merge(paths);
        if added > 0 {
            self.after_add_refresh();
        }
        added
    }

}

impl eframe::App for WavesPreviewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.suppress_list_enter = false;
        self.ensure_theme_visuals(ctx);
        // Update meter from audio RMS (approximate dBFS)
        {
            let rms = self.audio.shared.meter_rms.load(std::sync::atomic::Ordering::Relaxed);
            let db = if rms > 0.0 { 20.0 * rms.max(1e-8).log10() } else { -80.0 };
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
                if let Some(idx) = self.active_tab { if let Some(tool) = self.heavy_preview_tool { self.set_preview_mono(idx, tool, mono); } }
                self.heavy_preview_rx = None; self.heavy_preview_tool = None;
            }
        }
        // Drain list preview full-load results
        if let Some(rx) = &self.list_preview_rx {
            if let Ok(res) = rx.try_recv() {
                if res.job_id == self.list_preview_job_id {
                    if self.active_tab.is_none() && self.playing_path.as_ref() == Some(&res.path) {
                        self.audio.replace_samples_keep_pos(std::sync::Arc::new(res.samples));
                    }
                }
                self.list_preview_rx = None;
            }
        }
        // Drain heavy per-channel overlay results
        if let Some(rx) = &self.heavy_overlay_rx {
            if let Ok((p, overlay, timeline_len, gen)) = rx.try_recv() {
                let expected_tool = self.overlay_expected_tool.take();
                if gen == self.overlay_expected_gen {
                    if let Some(idx) = self.tabs.iter().position(|t| t.path == p) {
                        if let Some(tab) = self.tabs.get_mut(idx) {
                            if let Some(tool) = expected_tool {
                                if tab.preview_audio_tool == Some(tool) || tab.active_tool == tool {
                                    tab.preview_overlay = Some(PreviewOverlay { channels: overlay, source_tool: tool, timeline_len });
                                }
                            } else {
                                tab.preview_overlay = Some(PreviewOverlay { channels: overlay, source_tool: tab.active_tool, timeline_len });
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
                if let Some(tab) = self.tabs.get_mut(res.tab_idx) {
                    let old_len = tab.samples_len.max(1);
                    let old_view = tab.view_offset;
                    let old_spp = tab.samples_per_px;
                    if let Some(undo_state) = undo {
                        Self::push_undo_state_from(tab, undo_state, true);
                    }
                    tab.preview_audio_tool = None;
                    tab.preview_overlay = None;
                    tab.ch_samples = res.channels;
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
                self.audio.set_samples(std::sync::Arc::new(res.samples));
                if let Some(tab) = self.tabs.get(res.tab_idx) {
                    self.apply_loop_mode_for_tab(tab);
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
        // Drain spectrogram jobs
        if let Some(rx) = &self.spectro_rx {
            while let Ok((p, data)) = rx.try_recv() {
                self.spectro_inflight.remove(&p);
                self.spectro_cache.insert(p, std::sync::Arc::new(data));
                ctx.request_repaint();
            }
        }

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
                                if let Some(idx) = self.row_for_path(&path) { self.select_and_load(idx, true); }
                            }
                        }
                        SaveMode::NewFile => {
                            let mut added_any=false; let mut first_added=None;
                            for p in &res.success_paths { if self.add_files_merge(&[p.clone()])>0 { if first_added.is_none(){ first_added=Some(p.clone()); } added_any=true; } }
                            if added_any { self.after_add_refresh(); }
                            if let Some(p) = first_added { if let Some(idx) = self.row_for_path(&p) { self.select_and_load(idx, true); } }
                        }
                    }
                    self.saving_sources.clear(); self.saving_mode=None;
                }
                self.export_state = None;
                ctx.request_repaint();
            }
        }

        // Drain LUFS (with gain) recompute results
        let mut got_any = false;
        if let Some(rx) = &self.lufs_rx2 {
            while let Ok((p, v)) = rx.try_recv() { self.lufs_override.insert(p, v); got_any = true; }
        }
        if got_any { self.lufs_worker_busy = false; }

        // Pump LUFS recompute worker (debounced)
        if !self.lufs_worker_busy {
            let now = std::time::Instant::now();
            if let Some(path) = self.lufs_recalc_deadline.iter().find(|(_, dl)| **dl <= now).map(|(p, _)| p.clone()) {
                self.lufs_recalc_deadline.remove(&path);
                let g_db = self.pending_gain_db_for_path(&path);
                if g_db.abs() < 0.0001 { self.lufs_override.remove(&path); }
                else {
                    use std::sync::mpsc; let (tx, rx) = mpsc::channel();
                    self.lufs_rx2 = Some(rx);
                    self.lufs_worker_busy = true;
                    std::thread::spawn(move || {
                        let res = (|| -> anyhow::Result<f32> {
                            let (mut chans, sr) = crate::wave::decode_wav_multi(&path)?;
                            let gain = 10.0f32.powf(g_db/20.0);
                            for ch in chans.iter_mut() { for v in ch.iter_mut() { *v *= gain; } }
                            crate::wave::lufs_integrated_from_multi(&chans, sr)
                        })();
                        let val = match res { Ok(v) => v, Err(_) => f32::NEG_INFINITY };
                        let _=tx.send((path, val));
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
            // Apply new buffer and waveform
            self.audio.set_samples(std::sync::Arc::new(res.samples));
            self.audio.stop();
            if let Some(idx) = self.tabs.iter().position(|t| t.path == res.path) {
                if let Some(tab) = self.tabs.get_mut(idx) { tab.waveform_minmax = res.waveform; }
            }
            // update current playing path (for effective volume using pending gains)
            self.playing_path = Some(res.path.clone());
            // full-buffer loop region if needed
            if let Some(buf) = self.audio.shared.samples.load().as_ref() { self.audio.set_loop_region(0, buf.len()); }
            self.processing = None;
            if autoplay_when_ready
                && self.auto_play_list_nav
                && self.selected_path_buf().as_ref() == Some(&res.path)
            {
                self.audio.play();
            }
            ctx.request_repaint();
        }

        // Shortcuts
        if ctx.input(|i| i.key_pressed(Key::Space)) {
            if let Some(tab_idx) = self.active_tab {
                if self
                    .tabs
                    .get(tab_idx)
                    .and_then(|t| t.preview_audio_tool)
                    .is_some()
                {
                    self.clear_preview_if_any(tab_idx);
                }
            }
            self.audio.toggle_play();
        }
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::S)) { self.trigger_save_selected(); }
        // Editor-specific shortcuts: Loop region setters, Loop toggle (L), Zero-cross snap (S)
        if let Some(tab_idx) = self.active_tab {
            // Loop Start/End at playhead
            if ctx.input(|i| i.key_pressed(Key::K)) { // Set Loop Start
                let pos_now = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    let end = tab.loop_region.map(|(_,e)| e).unwrap_or(pos_now);
                    let s = pos_now.min(end);
                    let e = end.max(s);
                    tab.loop_region = Some((s,e));
                    Self::update_loop_markers_dirty(tab);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::P)) { // Set Loop End
                let pos_now = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed);
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    let start = tab.loop_region.map(|(s,_)| s).unwrap_or(pos_now);
                    let s = start.min(pos_now);
                    let e = pos_now.max(start);
                    tab.loop_region = Some((s,e));
                    Self::update_loop_markers_dirty(tab);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::L)) {
                // Toggle loop mode without holding a mutable borrow across &self call
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    tab.loop_mode = match tab.loop_mode { LoopMode::Off => LoopMode::OnWhole, _ => LoopMode::Off };
                }
                if let Some(tab_ro) = self.tabs.get(tab_idx) {
                    self.apply_loop_mode_for_tab(tab_ro);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::S)) {
                if let Some(tab) = self.tabs.get_mut(tab_idx) { tab.snap_zero_cross = !tab.snap_zero_cross; }
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
                let mut paths: Vec<std::path::PathBuf> = Vec::new();
                for f in dropped {
                    if let Some(p) = f.path { paths.push(p); }
                }
                if !paths.is_empty() {
                    let added = self.add_files_merge(&paths);
                    if added > 0 { self.after_add_refresh(); }
                }
            }
        }
        let mut activate_path: Option<PathBuf> = None;
        egui::CentralPanel::default().show(ctx, |ui| {            // Tabs
            ui.horizontal_wrapped(|ui| {
                let is_list = self.active_tab.is_none();
                let list_label = if is_list { RichText::new("[List]").strong() } else { RichText::new("List") };
                if ui.selectable_label(is_list, list_label).clicked() {
                    if let Some(idx) = self.active_tab { self.clear_preview_if_any(idx); }
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
                            if let Some(prev) = self.active_tab { if prev != i { self.clear_preview_if_any(prev); } }
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
        let mut apply_pending_loop = false;
        if let Some(tab_idx) = self.active_tab {
    // Pre-read audio values to avoid borrowing self while editing tab
    let sr_ctx = self.audio.shared.out_sample_rate.max(1) as f32;
    let pos_ctx_now = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed);
    let mut request_seek: Option<usize> = None;
    let spec_path = self.tabs[tab_idx].path.clone();
    let spec_cache = self.spectro_cache.get(&spec_path).cloned();
    let spec_loading = self.spectro_inflight.contains(&spec_path);
    ui.horizontal(|ui| {
        let tab = &mut self.tabs[tab_idx];
        let dirty_mark = if tab.dirty || tab.loop_markers_dirty || tab.markers_dirty { " *" } else { "" };
        let path_text = format!("{}{}", tab.path.display(), dirty_mark);
        ui.add(
            egui::Label::new(RichText::new(path_text).monospace())
                .truncate()
                .show_tooltip_when_elided(true),
        );
    });
    ui.horizontal_wrapped(|ui| {
        let tab = &mut self.tabs[tab_idx];
        // Loop mode toggles (kept): Off / OnWhole / Marker
        ui.label("Loop:");
        for (m,label) in [ (LoopMode::Off, "Off"), (LoopMode::OnWhole, "On"), (LoopMode::Marker, "Marker") ] {
            if ui.selectable_label(tab.loop_mode == m, label).clicked() {
                tab.loop_mode = m;
                apply_pending_loop = true;
            }
        }
        ui.separator();
        // View mode toggles
        for (vm, label) in [ (ViewMode::Waveform, "Wave"), (ViewMode::Spectrogram, "Spec"), (ViewMode::Mel, "Mel") ] {
            if ui.selectable_label(tab.view_mode == vm, label).clicked() { tab.view_mode = vm; }
        }
        ui.separator();
        // Time HUD: play position (editable) / total length
        let sr = sr_ctx; // restore local sample-rate alias after removing top-level Loop block
        let mut pos_sec = pos_ctx_now as f32 / sr as f32;
        let len_sec = (tab.samples_len as f32 / sr as f32).max(0.0);
        ui.label("Pos:");
        let pos_resp = ui.add(
            egui::DragValue::new(&mut pos_sec)
                .range(0.0..=len_sec)
                .speed(0.05)
                .fixed_decimals(2)
        );
        if pos_resp.changed() { let samp = (pos_sec.max(0.0) * sr) as usize; request_seek = Some(samp.min(tab.samples_len)); }
        ui.label(RichText::new(format!(" / {}", crate::app::helpers::format_time_s(len_sec))).monospace());
    });
    ui.separator();

    let avail = ui.available_size();
                // pending actions to perform after UI borrows end
                let mut do_set_loop_from: Option<(usize,usize)> = None;
                let mut do_trim: Option<(usize,usize)> = None; // keep-only (optional)
                let do_fade: Option<((usize,usize), f32, f32)> = None; // legacy whole-file fade
                let mut do_gain: Option<((usize,usize), f32)> = None;
                let mut do_normalize: Option<((usize,usize), f32)> = None;
                let mut do_reverse: Option<(usize,usize)> = None;
                // let mut do_silence: Option<(usize,usize)> = None; // removed
                let mut do_cutjoin: Option<(usize,usize)> = None;
                let mut do_apply_xfade: bool = false;
                let mut do_write_loop_markers: bool = false;
                let mut do_write_markers: bool = false;
                let mut do_fade_in: Option<((usize,usize), crate::app::types::FadeShape)> = None;
                let mut do_fade_out: Option<((usize,usize), crate::app::types::FadeShape)> = None;
                let mut stop_playback = false;
                // Snapshot busy state and prepare deferred overlay job.
                // IMPORTANT: Do NOT call `self.*` (which takes &mut self) while holding `let tab = &mut self.tabs[...]`.
                // That pattern triggers borrow checker error E0499. Defer such calls to after the UI closures.
                let overlay_busy = self.heavy_overlay_rx.is_some();
                let apply_busy = self.editor_apply_state.is_some();
                let mut pending_overlay_job: Option<(ToolKind, f32)> = None;
                let mut request_undo = false;
                let mut request_redo = false;
                let gain_db = self
                    .tabs
                    .get(tab_idx)
                    .map(|tab| self.pending_gain_db_for_path(&tab.path))
                    .unwrap_or(0.0);
                // Split canvas and inspector; keep inspector visible on narrow widths.
                let min_canvas_w = 160.0f32;
                let min_inspector_w = 220.0f32;
                let max_inspector_w = 360.0f32;
                let inspector_w = if avail.x <= min_inspector_w {
                    avail.x
                } else {
                    let available = (avail.x - min_canvas_w).max(min_inspector_w);
                    available.min(max_inspector_w).min(avail.x)
                };
                let canvas_w = (avail.x - inspector_w).max(0.0);
                ui.horizontal(|ui| {
                    let tab = &mut self.tabs[tab_idx];
                    let preview_ok = tab.samples_len <= LIVE_PREVIEW_SAMPLE_LIMIT;
                    // Canvas area
                    let mut need_restore_preview = false;
                    // Accumulate non-destructive preview audio to audition.
                    // Carry the tool kind to keep preview state consistent.
                    let mut pending_preview: Option<(ToolKind, Vec<f32>)> = None;
                    let mut pending_heavy_preview: Option<(ToolKind, Vec<f32>, f32)> = None;
                    let mut pending_pitch_apply: Option<f32> = None;
                    let mut pending_stretch_apply: Option<f32> = None;
                    ui.vertical(|ui| {
                        let canvas_h = (canvas_w * 0.35).clamp(180.0, avail.y);
                        let (resp, painter) = ui.allocate_painter(egui::vec2(canvas_w, canvas_h), Sense::click_and_drag());
                        let rect = resp.rect;
                        let w = rect.width().max(1.0); let h = rect.height().max(1.0);
                        let mut hover_cursor: Option<egui::CursorIcon> = None;
                        painter.rect_filled(rect, 0.0, Color32::from_rgb(16,16,18));
                        // Layout parameters
                        let gutter_w = 44.0;
                        let wave_left = rect.left() + gutter_w;
                        let wave_w = (w - gutter_w).max(1.0);
                        let ch_n = tab.ch_samples.len().max(1);
                        let lane_h = h / ch_n as f32;

                        // Visual amplitude scale: assume Volume=0 dB for display; apply per-file Gain only
                        let scale = db_to_amp(gain_db);

                        // Initialize zoom to fit if unset (show whole file)
                        if tab.samples_len > 0 && tab.samples_per_px <= 0.0 {
                            let fit_spp = (tab.samples_len as f32 / wave_w.max(1.0)).max(0.01);
                            tab.samples_per_px = fit_spp;
                            tab.view_offset = 0;
                        }
                        // Keep the same center sample anchored when the window width changes.
                        if tab.samples_len > 0 {
                            let spp = tab.samples_per_px.max(0.0001);
                            let old_wave_w = tab.last_wave_w;
                            if old_wave_w > 0.0 && (old_wave_w - wave_w).abs() > 0.5 {
                                let old_vis = (old_wave_w * spp).ceil() as usize;
                                let new_vis = (wave_w * spp).ceil() as usize;
                                if old_vis > 0 && new_vis > 0 {
                                    let anchor = tab.view_offset.saturating_add(old_vis / 2);
                                    let max_left = tab.samples_len.saturating_sub(new_vis);
                                    let new_view = anchor.saturating_sub(new_vis / 2).min(max_left);
                                    tab.view_offset = new_view;
                                }
                            }
                            tab.last_wave_w = wave_w;
                        } else {
                            tab.last_wave_w = wave_w;
                        }

                // Time ruler (ticks + labels) across all lanes
                {
                    let spp = tab.samples_per_px.max(0.0001);
                    let vis = (wave_w * spp).ceil() as usize;
                    let start = tab.view_offset.min(tab.samples_len);
                    let end = (start + vis).min(tab.samples_len);
                    if end > start {
                        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                        let t0 = start as f32 / sr;
                        let t1 = end as f32 / sr;
                        let px_per_sec = (1.0 / spp) * sr;
                        let min_px = 90.0;
                        let steps: [f32; 15] = [0.01,0.02,0.05,0.1,0.2,0.5,1.0,2.0,5.0,10.0,15.0,30.0,60.0,120.0,300.0];
                        let mut step = steps[steps.len()-1];
                        for s in steps { if px_per_sec * s >= min_px { step = s; break; } }
                        let start_tick = (t0 / step).floor() * step;
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        let grid_col = Color32::from_rgb(38,38,44);
                        let label_col = Color32::GRAY;
                        let mut t = start_tick;
                        while t <= t1 + step*0.5 {
                            let s_idx = (t * sr).round() as isize;
                            let rel = (s_idx.max(start as isize) - start as isize) as f32;
                            let x = wave_left + (rel / spp).clamp(0.0, wave_w);
                            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0, grid_col));
                            // Label near top; avoid overcrowding by skipping when too dense
                            if px_per_sec * step >= 70.0 {
                                let label = crate::app::helpers::format_time_s(t);
                                painter.text(egui::pos2(x + 2.0, rect.top() + 2.0), egui::Align2::LEFT_TOP, label, fid.clone(), label_col);
                            }
                            t += step;
                        }
                    }
                }

                if tab.view_mode != ViewMode::Waveform {
                    if !preview_ok {
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        painter.text(
                            egui::pos2(wave_left + 6.0, rect.top() + 6.0),
                            egui::Align2::LEFT_TOP,
                            "Spectrogram disabled for large clips",
                            fid,
                            Color32::GRAY,
                        );
                    } else if let Some(spec) = spec_cache.as_ref() {
                        Self::draw_spectrogram(&painter, rect, wave_left, wave_w, tab, spec, tab.view_mode);
                    } else {
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        let msg = if spec_loading { "Building spectrogram..." } else { "Spectrogram not ready" };
                        painter.text(
                            egui::pos2(wave_left + 6.0, rect.top() + 6.0),
                            egui::Align2::LEFT_TOP,
                            msg,
                            fid,
                            Color32::GRAY,
                        );
                    }
                }

                // Handle interactions (seek, zoom, pan, selection)
                // Detect hover using pointer position against our canvas rect (robust across senses)
                let pointer_over_canvas = ui.input(|i| i.pointer.hover_pos()).map_or(false, |p| rect.contains(p));
                if pointer_over_canvas {
                    // Zoom with Ctrl + wheel (use hovered pos over this widget)
                    // Combine raw wheel delta with low-level events as a fallback (covers trackpads/pinch, some platforms).
                    let wheel_raw = ui.input(|i| i.raw_scroll_delta);
                    let mut wheel = wheel_raw;
                    let mut pinch_zoom_factor: f32 = 1.0;
                    let events = ctx.input(|i| i.events.clone());
                    for ev in events {
                        match ev {
                            egui::Event::MouseWheel { delta, .. } => {
                                wheel += delta;
                            }
                            egui::Event::Zoom(z) => {
                                pinch_zoom_factor *= z;
                            }
                            _ => {}
                        }
                    }
                    let scroll_y = wheel.y;
                    let modifiers = ui.input(|i| i.modifiers);
                    let pointer_pos = resp.hover_pos();
                    // Debug trace (dev builds): log incoming deltas and modifiers when over canvas
                    #[cfg(debug_assertions)]
                    if wheel_raw != egui::Vec2::ZERO || pinch_zoom_factor != 1.0 {
                        eprintln!(
                            "wheel_raw=({:.2},{:.2}) wheel_total=({:.2},{:.2}) ctrl={} shift={} pinch={:.3}",
                            wheel_raw.x, wheel_raw.y, wheel.x, wheel.y, modifiers.ctrl, modifiers.shift, pinch_zoom_factor
                        );
                    }
                    // Zoom: plain wheel (unless Shift is held for pan) or pinch zoom
                    if (((scroll_y.abs() > 0.0) && !modifiers.shift) || (pinch_zoom_factor != 1.0)) && tab.samples_len > 0 {
                        // Wheel up = zoom in
                        let factor = if pinch_zoom_factor != 1.0 { pinch_zoom_factor } else if scroll_y < 0.0 { 0.9 } else { 1.1 };
                        let factor = factor.clamp(0.2, 5.0);
                        let old_spp = tab.samples_per_px.max(0.0001);
                        let cursor_x = pointer_pos.map(|p| p.x).unwrap_or(wave_left + wave_w * 0.5).clamp(wave_left, wave_left + wave_w);
                        let t = ((cursor_x - wave_left) / wave_w).clamp(0.0, 1.0);
                        let vis = (wave_w * old_spp).ceil() as usize;
                        let anchor = tab.view_offset.saturating_add((t * vis as f32) as usize).min(tab.samples_len);
                        // Dynamic clamp: allow full zoom-out to "fit whole"
                        let min_spp = 0.01; // 100 px per sample
                        let max_spp_fit = (tab.samples_len as f32 / wave_w.max(1.0)).max(min_spp);
                        let new_spp = (old_spp * factor).clamp(min_spp, max_spp_fit);
                        tab.samples_per_px = new_spp;
                        let vis2 = (wave_w * tab.samples_per_px).ceil() as usize;
                        let left = anchor.saturating_sub((t * vis2 as f32) as usize);
                        let max_left = tab.samples_len.saturating_sub(vis2);
                        let new_view = left.min(max_left);
                        #[cfg(debug_assertions)]
                        {
                            let mode = if tab.samples_per_px >= 1.0 { "agg" } else { "line" };
                            let fit_whole = (new_spp - max_spp_fit).abs() < 1e-6;
                            eprintln!(
                                "ZOOM change: spp {:.5} -> {:.5} ({mode}) factor {:.3} vis={} -> {} anchor={} new_view={} wave_w={:.1} fit_whole={}",
                                old_spp, new_spp, factor, vis, vis2, anchor, new_view, wave_w, fit_whole
                            );
                        }
                        tab.view_offset = new_view;
                    }
                    // Pan with Shift + wheel (prefer horizontal wheel if available)
                    let scroll_for_pan = if wheel.x.abs() > 0.0 { wheel.x } else { wheel.y };
                    if modifiers.shift && scroll_for_pan.abs() > 0.0 && tab.samples_len > 0 {
                        let delta_px = -scroll_for_pan.signum() * 60.0; // a page step
                        let delta = (delta_px * tab.samples_per_px) as isize;
                        let mut off = tab.view_offset as isize + delta;
                        let vis = (wave_w * tab.samples_per_px).ceil() as usize;
                        let max_left = tab.samples_len.saturating_sub(vis);
                        if off < 0 { off = 0; }
                        if off as usize > max_left { off = max_left as isize; }
                        tab.view_offset = off as usize;
                    }
                    // Pan with Middle / Right drag, or Alt + Left drag (DAW-like)
                    let (left_down, mid_down, right_down, alt_mod) = ui.input(|i| (
                        i.pointer.button_down(egui::PointerButton::Primary),
                        i.pointer.button_down(egui::PointerButton::Middle),
                        i.pointer.button_down(egui::PointerButton::Secondary),
                        i.modifiers.alt,
                    ));
                    let alt_left_pan = alt_mod && left_down;
                    if (mid_down || right_down || alt_left_pan) && tab.samples_len > 0 {
                        let dx = ui.input(|i| i.pointer.delta().x);
                        if dx.abs() > 0.0 {
                            let delta = (-dx * tab.samples_per_px) as isize;
                            let mut off = tab.view_offset as isize + delta;
                            let vis = (wave_w * tab.samples_per_px).ceil() as usize;
                            let max_left = tab.samples_len.saturating_sub(vis);
                            if off < 0 { off = 0; }
                            if off as usize > max_left { off = max_left as isize; }
                            tab.view_offset = off as usize;
                        }
                    }
                }
                // Drag markers for LoopEdit / Trim (primary button only)
                let mut suppress_seek = false;
                if pointer_over_canvas
                    && matches!(tab.active_tool, ToolKind::LoopEdit | ToolKind::Trim)
                    && tab.samples_len > 0
                {
                    let pointer_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
                    let pointer_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
                    if pointer_released || !pointer_down {
                        tab.dragging_marker = None;
                    }
                    if pointer_down {
                        let spp = tab.samples_per_px.max(0.0001);
                        let vis = (wave_w * spp).ceil() as usize;
                        let to_sample = |x: f32| {
                            let x = x.clamp(wave_left, wave_left + wave_w);
                            let pos = (((x - wave_left) / wave_w) * vis as f32) as usize;
                            tab.view_offset.saturating_add(pos).min(tab.samples_len)
                        };
                        let to_x = |samp: usize| {
                            wave_left
                                + (((samp.saturating_sub(tab.view_offset)) as f32 / spp)
                                    .clamp(0.0, wave_w))
                        };
                        let hit_radius = 7.0;
                        if tab.dragging_marker.is_none() {
                            if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                                let x = pos.x;
                                match tab.active_tool {
                                    ToolKind::LoopEdit => {
                                        if let Some((a0, b0)) = tab.loop_region {
                                            let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                            let ax = to_x(a);
                                            let bx = to_x(b);
                                            if (x - ax).abs() <= hit_radius {
                                                tab.dragging_marker = Some(MarkerKind::A);
                                            } else if (x - bx).abs() <= hit_radius {
                                                tab.dragging_marker = Some(MarkerKind::B);
                                            }
                                        }
                                    }
                                                                        ToolKind::Trim => {
                                        if let Some((a0, b0)) = tab.trim_range {
                                            let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                            let ax = to_x(a);
                                            let bx = to_x(b);
                                            if (x - ax).abs() <= hit_radius {
                                                tab.dragging_marker = Some(MarkerKind::A);
                                            } else if (x - bx).abs() <= hit_radius {
                                                tab.dragging_marker = Some(MarkerKind::B);
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        if let Some(marker) = tab.dragging_marker {
                            if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                                let samp = to_sample(pos.x);
                                match tab.active_tool {
                                    ToolKind::LoopEdit => {
                                        if let Some((a0, b0)) = tab.loop_region {
                                            let (mut a, mut b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                            match marker {
                                                MarkerKind::A => a = samp.min(b),
                                                MarkerKind::B => b = samp.max(a),
                                            }
                                            tab.loop_region = Some((a, b));
                                            Self::update_loop_markers_dirty(tab);
                                            apply_pending_loop = true;
                                        }
                                    }
                                    ToolKind::Trim => {
                                        if let Some((a0, b0)) = tab.trim_range {
                                            let (mut a, mut b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                            match marker {
                                                MarkerKind::A => a = samp.min(b),
                                                MarkerKind::B => b = samp.max(a),
                                            }
                                            tab.trim_range = Some((a, b));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            suppress_seek = true;
                        }
                    }
                }
                // Drag to select a range (independent of tool), unless we are dragging markers
                let alt_now = ui.input(|i| i.modifiers.alt);
                if pointer_over_canvas
                    && !alt_now
                    && tab.samples_len > 0
                    && tab.dragging_marker.is_none()
                {
                    let drag_started = resp.drag_started_by(egui::PointerButton::Primary);
                    let dragging = resp.dragged_by(egui::PointerButton::Primary);
                    let drag_released = resp.drag_stopped_by(egui::PointerButton::Primary);
                    if drag_started {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let spp = tab.samples_per_px.max(0.0001);
                            let vis = (wave_w * spp).ceil() as usize;
                            let x = pos.x.clamp(wave_left, wave_left + wave_w);
                            let samp = tab
                                .view_offset
                                .saturating_add((((x - wave_left) / wave_w) * vis as f32) as usize)
                                .min(tab.samples_len);
                            tab.drag_select_anchor = Some(samp);
                        }
                    }
                    if dragging {
                        let anchor = tab.drag_select_anchor.or_else(|| {
                            resp.interact_pointer_pos().map(|pos| {
                                let spp = tab.samples_per_px.max(0.0001);
                                let vis = (wave_w * spp).ceil() as usize;
                                let x = pos.x.clamp(wave_left, wave_left + wave_w);
                                tab.view_offset
                                    .saturating_add(
                                        (((x - wave_left) / wave_w) * vis as f32) as usize,
                                    )
                                    .min(tab.samples_len)
                            })
                        });
                        if tab.drag_select_anchor.is_none() {
                            tab.drag_select_anchor = anchor;
                        }
                        if let (Some(anchor), Some(pos)) = (anchor, resp.interact_pointer_pos()) {
                            let spp = tab.samples_per_px.max(0.0001);
                            let vis = (wave_w * spp).ceil() as usize;
                            let x = pos.x.clamp(wave_left, wave_left + wave_w);
                            let samp = tab
                                .view_offset
                                .saturating_add((((x - wave_left) / wave_w) * vis as f32) as usize)
                                .min(tab.samples_len);
                            let (s, e) = if samp >= anchor {
                                (anchor, samp)
                            } else {
                                (samp, anchor)
                            };
                            tab.selection = Some((s, e));
                            if tab.active_tool == ToolKind::Trim {
                                tab.trim_range = Some((s, e));
                            }
                            suppress_seek = true;
                        }
                    }
                    if drag_released {
                        tab.drag_select_anchor = None;
                    }
                }
                // Selection vs Seek with primary button (Alt+LeftDrag = pan handled above)
                if !alt_now && !suppress_seek {
                    // Primary interactions: click to seek (no range selection)
                    if resp.clicked_by(egui::PointerButton::Primary) {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            let x = pos.x.clamp(wave_left, wave_left + wave_w);
                            let spp = tab.samples_per_px.max(0.0001);
                            let vis = (wave_w * spp).ceil() as usize;
                            let pos_samp = tab
                                .view_offset
                                .saturating_add(
                                    (((x - wave_left) / wave_w) * vis as f32) as usize,
                                )
                                .min(tab.samples_len);
                            if let Some((a0, b0)) = tab.selection {
                                let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                if pos_samp < a || pos_samp > b {
                                    tab.selection = None;
                                    if tab.active_tool == ToolKind::Trim {
                                        tab.trim_range = None;
                                    }
                                }
                            }
                            request_seek = Some(pos_samp);
                        }
                    }
                }

                let spp = tab.samples_per_px.max(0.0001);
                let vis = (wave_w * spp).ceil() as usize;
                let start = tab.view_offset.min(tab.samples_len);
                let end = (start + vis).min(tab.samples_len);
                let visible_len = end.saturating_sub(start);

                // Draw per-channel lanes with dB grid and playhead
                for (ci, ch) in tab.ch_samples.iter().enumerate() {
                    let lane_top = rect.top() + lane_h * ci as f32;
                    let lane_rect = egui::Rect::from_min_size(egui::pos2(wave_left, lane_top), egui::vec2(wave_w, lane_h));
                    // dB lines: -6, -12 dBFS and center line (0 amp)
                    let dbs = [-6.0f32, -12.0f32];
                    // center
                    painter.line_segment([egui::pos2(lane_rect.left(), lane_rect.center().y), egui::pos2(lane_rect.right(), lane_rect.center().y)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50)));
                    for &db in &dbs {
                        let a = db_to_amp(db).clamp(0.0, 1.0);
                        let y0 = lane_rect.center().y - a * (lane_rect.height()*0.48);
                        let y1 = lane_rect.center().y + a * (lane_rect.height()*0.48);
                        painter.line_segment([egui::pos2(lane_rect.left(), y0), egui::pos2(lane_rect.right(), y0)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50)));
                        painter.line_segment([egui::pos2(lane_rect.left(), y1), egui::pos2(lane_rect.right(), y1)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50)));
                        // labels on the left gutter
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        painter.text(egui::pos2(rect.left() + 2.0, y0), egui::Align2::LEFT_CENTER, format!("{db:.0} dB"), fid, Color32::GRAY);
                    }

                    if visible_len > 0 {
                        // Two rendering paths depending on zoom level:
                        // - Aggregated min/max bins for spp >= 1.0 (>= 1 sample per pixel)
                        // - Direct per-sample polyline/stem for spp < 1.0 (< 1 sample per pixel)
                        if spp >= 1.0 {
                            let bins = wave_w as usize; // one bin per pixel
                            if bins > 0 {
                                let mut tmp = Vec::new();
                                build_minmax(&mut tmp, &ch[start..end], bins);
                                let n = tmp.len().max(1) as f32;
                                for (idx, &(mn, mx)) in tmp.iter().enumerate() {
                                    let mn = (mn * scale).clamp(-1.0, 1.0);
                                    let mx = (mx * scale).clamp(-1.0, 1.0);
                                    let x = lane_rect.left() + (idx as f32 / n) * wave_w;
                                    let y0 = lane_rect.center().y - mx * (lane_rect.height()*0.48);
                                    let y1 = lane_rect.center().y - mn * (lane_rect.height()*0.48);
                                    let amp = (mn.abs().max(mx.abs())).clamp(0.0, 1.0);
                                    let col = amp_to_color(amp);
                                    painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.0, col));
                                }
                            }
                            // Aggregated mode: also draw overlay here so it shows at widest zoom
                            if tab.active_tool != ToolKind::Trim && tab.preview_overlay.is_some() {
                                if let Some(overlay) = &tab.preview_overlay {
                                    let och: Option<&[f32]> = overlay.channels.get(ci).map(|v| v.as_slice()).or_else(|| overlay.channels.get(0).map(|v| v.as_slice()));
                                    if let Some(buf) = och {
                                        use crate::app::render::overlay as ov;
                                        use crate::app::render::colors::{OVERLAY_COLOR, OVERLAY_STROKE_BASE, OVERLAY_STROKE_EMPH};
                                        let base_total = tab.samples_len.max(1);
                                        let overlay_total = overlay.timeline_len.max(1);
                                        let is_time_stretch = matches!(overlay.source_tool, ToolKind::TimeStretch);
                                        let ratio = if is_time_stretch {
                                            1.0
                                        } else if base_total == 0 {
                                            1.0
                                        } else {
                                            overlay_total as f32 / base_total as f32
                                        };
                                        let start_scaled = ((start as f32) * ratio).round() as usize;
                                        let mut vis_scaled = ((visible_len as f32) * ratio).ceil() as usize;
                                        if vis_scaled == 0 { vis_scaled = 1; }
                                        let (startb, _endb, over_vis) = ov::map_visible_overlay(start_scaled, vis_scaled, overlay_total, buf.len());
                                        if over_vis > 0 {
                                            let bins = wave_w as usize;
                                            let bins_values = ov::compute_overlay_bins_for_base_columns(start, visible_len, startb, over_vis, buf, bins);
                                            // Draw full overlay
                                            ov::draw_bins_locked(&painter, lane_rect, wave_w, &bins_values, scale, OVERLAY_COLOR, OVERLAY_STROKE_BASE);
                                            // Emphasize LoopEdit boundary segments if applicable
                                            if tab.active_tool == ToolKind::LoopEdit {
                                                if let Some((a, b)) = tab.loop_region {
                                                    let cf = Self::effective_loop_xfade_samples(
                                                        a,
                                                        b,
                                                        tab.samples_len,
                                                        tab.loop_xfade_samples,
                                                    );
                                                    if cf > 0 {
                                                        // Map required pre/post segments into overlay domain using ratio
                                                        let ratio = if base_total > 0 { (overlay_total as f32) / (base_total as f32) } else { 1.0 };
                                                        let pre0 = a.saturating_sub(cf);
                                                        let pre1 = (a + cf).min(tab.samples_len);
                                                        let post0 = b.saturating_sub(cf);
                                                        let post1 = (b + cf).min(tab.samples_len);
                                                        let a0 = (((pre0 as f32) * ratio).round() as usize).min(buf.len());
                                                        let a1 = (((pre1 as f32) * ratio).round() as usize).min(buf.len());
                                                        let b0 = (((post0 as f32) * ratio).round() as usize).min(buf.len());
                                                        let b1 = (((post1 as f32) * ratio).round() as usize).min(buf.len());
                                                        let segs = [(a0, a1), (b0, b1)];
                                                        for (s, e) in segs {
                                                            if let Some((p0, p1)) = ov::overlay_px_range_for_segment(startb, over_vis, bins, s, e) {
                                                                if p1 > p0 && p1 <= bins {
                                                                    let span_left = lane_rect.left() + (p0 as f32 / bins as f32) * wave_w;
                                                                    let span_w = ((p1 - p0) as f32 / bins as f32) * wave_w;
                                                                    let span_rect = egui::Rect::from_min_size(egui::pos2(span_left, lane_rect.top()), egui::vec2(span_w, lane_rect.height()));
                                                                    let sub = &bins_values[p0..p1];
                                                                    ov::draw_bins_in_rect(&painter, span_rect, sub, scale, OVERLAY_COLOR, OVERLAY_STROKE_EMPH);
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                    // Fine zoom: draw per-sample. When there are fewer samples than pixels,
                    // distribute samples evenly across the available width and connect them.
                    let scale_y = lane_rect.height() * 0.48;
                    if visible_len == 1 {
                        let sx = lane_rect.left() + wave_w * 0.5;
                        let v = (ch[start] * scale).clamp(-1.0, 1.0);
                        let sy = lane_rect.center().y - v * scale_y;
                        let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                        painter.circle_filled(egui::pos2(sx, sy), 2.0, col);
                    } else {
                        let denom = (visible_len - 1) as f32;
                        let mut last: Option<(f32, f32, egui::Color32)> = None;
                        for (i, &v0) in ch[start..end].iter().enumerate() {
                            let v = (v0 * scale).clamp(-1.0, 1.0);
                            let t = (i as f32) / denom;
                            let sx = lane_rect.left() + t * wave_w;
                            let sy = lane_rect.center().y - v * scale_y;
                            let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                            if let Some((px, py, pc)) = last {
                                // Use previous color to avoid color flicker between segments
                                painter.line_segment([egui::pos2(px, py), egui::pos2(sx, sy)], egui::Stroke::new(1.0, pc));
                            }
                            last = Some((sx, sy, col));
                        }
                        // Optionally draw stems for clarity when pixels-per-sample is large
                        let pps = 1.0 / spp; // pixels per sample
                        if pps >= 6.0 {
                            for (i, &v0) in ch[start..end].iter().enumerate() {
                                let v = (v0 * scale).clamp(-1.0, 1.0);
                                let t = (i as f32) / denom;
                                let sx = lane_rect.left() + t * wave_w;
                                let sy = lane_rect.center().y - v * scale_y;
                                let base = lane_rect.center().y;
                                let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                                painter.line_segment([egui::pos2(sx, base), egui::pos2(sx, sy)], egui::Stroke::new(1.0, col));
                            }
                        }
                    }

                    // Overlay preview aligned to this lane (if any), per-channel.
                    // Skip Trim tool (Trim does not show green overlay by spec).
                    // Draw whenever overlay data is present to avoid relying on preview_audio_tool state.
                    #[cfg(debug_assertions)]
                    if self.debug.cfg.enabled && self.debug.overlay_trace {
                        let mode = if spp >= 1.0 { "agg" } else { "line" };
                        let has_ov = tab.preview_overlay.is_some();
                        eprintln!(
                            "OVERLAY gate: mode={} has_overlay={} active={:?} spp={:.5} vis_len={} start={} end={} view_off={} len={}",
                            mode, has_ov, tab.active_tool, spp, visible_len, start, end, tab.view_offset, tab.samples_len
                        );
                    }
                    if tab.active_tool != ToolKind::Trim && tab.preview_overlay.is_some() {
                        if let Some(overlay) = &tab.preview_overlay {
                            // try channel match, fallback to first channel if overlay is mono
                            let och: Option<&[f32]> = overlay.channels.get(ci).map(|v| v.as_slice()).or_else(|| overlay.channels.get(0).map(|v| v.as_slice()));
                            if let Some(buf) = och {
                                // Map original-visible [start,end) to overlay domain using length ratio.
                                // This keeps overlays visible at any zoom, even when length differs (e.g. TimeStretch).
                                let lenb = buf.len();
                                        let base_total = tab.samples_len.max(1);
                                        let overlay_total = overlay.timeline_len.max(1);
                                        let is_time_stretch = matches!(overlay.source_tool, ToolKind::TimeStretch);
                                        let ratio = if is_time_stretch {
                                            1.0
                                        } else if base_total > 0 {
                                            (overlay_total as f32) / (base_total as f32)
                                        } else {
                                            1.0
                                        };
                                let orig_vis = visible_len.max(1);
                                // Map visible window [start .. start+orig_vis) into overlay domain using total-length ratio
                                // Align overlay start to original start using nearest sample to minimize off-by-one drift
                                let startb = (((start as f32) * ratio).round() as usize).min(lenb);
                                let mut endb = startb + (((orig_vis as f32) * ratio).ceil() as usize);
                                if endb > lenb { endb = lenb; }
                                if startb >= endb { endb = (startb + 1).min(lenb); }
                                let over_vis = (endb.saturating_sub(startb)).max(1);
                                let r_w = if orig_vis > 0 { (over_vis as f32) / (orig_vis as f32) } else { 1.0 };
                                let ov_w = (wave_w * r_w).max(1.0);
                                #[cfg(debug_assertions)]
                                if self.debug.cfg.enabled && self.debug.overlay_trace {
                                    let mode = if spp >= 1.0 { "agg" } else { "line" };
                                    eprintln!(
                                        "OVERLAY map: mode={} lenb={} startb={} endb={} over_vis={} ov_w_px={:.1}",
                                        mode, lenb, startb, endb, over_vis, ov_w
                                    );
                                }
                                if startb < endb {
                                    // Pre-compute LoopEdit highlight segments (mapped to overlay domain)
                                    let (seg1_opt, seg2_opt) = if tab.active_tool == ToolKind::LoopEdit {
                                        if let Some((a, b)) = tab.loop_region {
                                            let cf = Self::effective_loop_xfade_samples(
                                                a,
                                                b,
                                                tab.samples_len,
                                                tab.loop_xfade_samples,
                                            );
                                            if cf > 0 {
                                                let pre0 = a.saturating_sub(cf);
                                                let pre1 = (a + cf).min(tab.samples_len);
                                                let post0 = b.saturating_sub(cf);
                                                let post1 = (b + cf).min(tab.samples_len);
                                                let a0 = (((pre0 as f32) * ratio).round() as usize).min(lenb);
                                                let a1 = (((pre1 as f32) * ratio).round() as usize).min(lenb);
                                                let b0 = (((post0 as f32) * ratio).round() as usize).min(lenb);
                                                let b1 = (((post1 as f32) * ratio).round() as usize).min(lenb);
                                                let s1 = a0.max(startb); let e1 = a1.min(endb);
                                                let s2 = b0.max(startb); let e2 = b1.min(endb);
                                                (if s1 < e1 { Some((s1,e1)) } else { None }, if s2 < e2 { Some((s2,e2)) } else { None })
                                            } else { (None, None) }
                                        } else { (None, None) }
                                    } else { (None, None) };

                                    // helper: draw polyline for [p0,p1) within [startb,endb) mapped into [0..ov_w]
                                    let _draw_segment_poly = |p0: usize, p1: usize| {
                                        let seg_len = p1.saturating_sub(p0);
                                        if seg_len == 0 { return; }
                                        let seg_ratio = (seg_len as f32) / (over_vis as f32);
                                        let seg_w = (ov_w * seg_ratio).max(1.0);
                                        let seg_x0 = lane_rect.left() + ((p0 - startb) as f32 / over_vis as f32) * ov_w;
                                        let count = seg_w.max(1.0) as usize; // ~1 point per px
                                        let denom = (count.saturating_sub(1)).max(1) as f32;
                                        let scale_y = lane_rect.height() * 0.48;
                                        #[cfg(debug_assertions)]
                                        if self.debug.cfg.enabled && self.debug.overlay_trace {
                                            let band = egui::Rect::from_min_max(egui::pos2(seg_x0, lane_rect.top()), egui::pos2(seg_x0 + seg_w, lane_rect.bottom()));
                                            painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(110, 255, 200, 20));
                                            eprintln!(
                                                "OVERLAY seg: p0={} p1={} seg_len={} seg_w_px={:.1} count={}",
                                                p0, p1, seg_len, seg_w, count
                                            );
                                        }
                                        // Widest zoom: a very short segment can quantize to <=1px. Ensure something is drawn.
                                        if count <= 2 {
                                            let idx = p0; // head of segment as representative
                                            let v = (buf[idx] * scale).clamp(-1.0, 1.0);
                                            let sx = seg_x0 + (seg_w * 0.5);
                                            let sy = lane_rect.center().y - v * scale_y;
                                            // Draw a short tick so it remains visible
                                            let tick_h = (lane_rect.height() * 0.10).max(2.0);
                                            painter.line_segment(
                                                [egui::pos2(sx, sy - tick_h*0.5), egui::pos2(sx, sy + tick_h*0.5)],
                                                egui::Stroke::new(1.8, Color32::from_rgb(80, 240, 160))
                                            );
                                        #[cfg(debug_assertions)]
                                        if self.debug.cfg.enabled && self.debug.overlay_trace {
                                            eprintln!("OVERLAY seg: fallback_tick used at x={:.1}", sx);
                                        }
                                            return;
                                        }
                                        let mut last: Option<egui::Pos2> = None;
                                        for i in 0..count {
                                            let t = (i as f32) / denom;
                                            let idx = p0 + ((t * (seg_len as f32 - 1.0)).round() as usize).min(seg_len - 1);
                                            let v = (buf[idx] * scale).clamp(-1.0, 1.0);
                                            let sx = seg_x0 + t * seg_w;
                                            let sy = lane_rect.center().y - v * scale_y;
                                            let p = egui::pos2(sx, sy);
                                            if let Some(lp) = last { painter.line_segment([lp, p], egui::Stroke::new(1.8, Color32::from_rgb(80, 240, 160))); }
                                            last = Some(p);
                                        }
                                    };

                                    if spp >= 1.0 {
                                        // Aggregated: compute bins via helper and draw pixel-locked bars
                                        let bins = wave_w as usize;
                                        if bins > 0 {
                                            let ratio_approx_1 = (over_vis as i64 - orig_vis as i64).abs() <= 1;
                                                let values = if ratio_approx_1 {
                                                    let mut tmp = Vec::new();
                                                    let s = start.min(lenb);
                                                    let e = end.min(lenb);
                                                    if s < e {
                                                        build_minmax(&mut tmp, &buf[s..e], bins);
                                                    }
                                                    tmp
                                                } else {
                                                    crate::app::render::overlay::compute_overlay_bins_for_base_columns(
                                                        start, orig_vis, startb, over_vis, buf, bins
                                                    )
                                            };
                                            crate::app::render::overlay::draw_bins_locked(
                                                &painter, lane_rect, wave_w, &values, scale, egui::Color32::from_rgb(80, 240, 160), 1.3
                                            );
                                        }
                                        // Emphasize LoopEdit boundary subranges if present (thicker over the same px columns)
                                        if let Some((s1,e1)) = seg1_opt {
                                            let bins = wave_w as usize;
                                            if bins > 0 {
                                                let step_b = (orig_vis as f32) / (bins as f32);
                                                let mut pos_b = 0.0f32;
                                                let px_end = ((over_vis as f32 / orig_vis as f32) * bins as f32).round().clamp(1.0, bins as f32) as usize;
                                                for px in 0..px_end {
                                                    let i0 = start + pos_b.floor() as usize;
                                                    pos_b += step_b;
                                                    let mut i1 = start + pos_b.floor() as usize;
                                                    if i1 <= i0 { i1 = i0 + 1; }
                                                    let mut o0 = startb + (((i0 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                    let mut o1 = startb + (((i1 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                    if o1 <= o0 { o1 = o0 + 1; }
                                                    o0 = o0.max(s1); o1 = o1.min(e1);
                                                    if o1 <= o0 { continue; }
                                                    let mut mn = f32::INFINITY; let mut mx = f32::NEG_INFINITY;
                                                    for &v in &buf[o0..o1] { if v < mn { mn = v; } if v > mx { mx = v; } }
                                                    if !mn.is_finite() || !mx.is_finite() { continue; }
                                                    let mn = (mn * scale).clamp(-1.0, 1.0);
                                                    let mx = (mx * scale).clamp(-1.0, 1.0);
                                                    let x = lane_rect.left() + (px as f32 / bins as f32) * wave_w;
                                                    let y0 = lane_rect.center().y - mx * (lane_rect.height()*0.48);
                                                    let y1 = lane_rect.center().y - mn * (lane_rect.height()*0.48);
                                                    painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.6, Color32::from_rgb(80, 240, 160)));
                                                }
                                            }
                                        }
                                        if let Some((s2,e2)) = seg2_opt {
                                            let bins = wave_w as usize;
                                            if bins > 0 {
                                                let step_b = (orig_vis as f32) / (bins as f32);
                                                let mut pos_b = 0.0f32;
                                                let px_end = ((over_vis as f32 / orig_vis as f32) * bins as f32).round().clamp(1.0, bins as f32) as usize;
                                                for px in 0..px_end {
                                                    let i0 = start + pos_b.floor() as usize;
                                                    pos_b += step_b;
                                                    let mut i1 = start + pos_b.floor() as usize;
                                                    if i1 <= i0 { i1 = i0 + 1; }
                                                    let mut o0 = startb + (((i0 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                    let mut o1 = startb + (((i1 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                    if o1 <= o0 { o1 = o0 + 1; }
                                                    o0 = o0.max(s2); o1 = o1.min(e2);
                                                    if o1 <= o0 { continue; }
                                                    let mut mn = f32::INFINITY; let mut mx = f32::NEG_INFINITY;
                                                    for &v in &buf[o0..o1] { if v < mn { mn = v; } if v > mx { mx = v; } }
                                                    if !mn.is_finite() || !mx.is_finite() { continue; }
                                                    let mn = (mn * scale).clamp(-1.0, 1.0);
                                                    let mx = (mx * scale).clamp(-1.0, 1.0);
                                                    let x = lane_rect.left() + (px as f32 / bins as f32) * wave_w;
                                                    let y0 = lane_rect.center().y - mx * (lane_rect.height()*0.48);
                                                    let y1 = lane_rect.center().y - mn * (lane_rect.height()*0.48);
                                                    painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.6, Color32::from_rgb(80, 240, 160)));
                                                }
                                            }
                                        }
                                    } else {
                                        let denom = (endb - startb - 1).max(1) as f32;
                                        let scale_y = lane_rect.height() * 0.48;
                                        #[cfg(debug_assertions)]
                                        {
                                            let x0 = lane_rect.left();
                                            let x1 = x0 + ov_w;
                                            let band = egui::Rect::from_min_max(egui::pos2(x0, lane_rect.top()), egui::pos2(x1, lane_rect.bottom()));
                                            painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(80, 240, 160, 20));
                                        }
                                        let mut last: Option<egui::Pos2> = None;
                                        for i in startb..endb {
                                            let v = (buf[i] * scale).clamp(-1.0, 1.0);
                                            let t = (i - startb) as f32 / denom;
                                            let sx = lane_rect.left() + t * ov_w;
                                            let sy = lane_rect.center().y - v * scale_y;
                                            let p = egui::pos2(sx, sy);
                                            if let Some(lp) = last { painter.line_segment([lp, p], egui::Stroke::new(1.5, Color32::from_rgb(80, 240, 160))); }
                                            last = Some(p);
                                        }
                                        // Add stems like the base waveform when zoomed in enough
                                        let pps = 1.0 / spp; // pixels per sample
                                        if pps >= 6.0 {
                                            for i in startb..endb {
                                                let v = (buf[i] * scale).clamp(-1.0, 1.0);
                                                let t = (i - startb) as f32 / denom;
                                                let sx = lane_rect.left() + t * ov_w;
                                                let sy = lane_rect.center().y - v * scale_y;
                                                let base = lane_rect.center().y;
                                                painter.line_segment([egui::pos2(sx, base), egui::pos2(sx, sy)], egui::Stroke::new(1.0, Color32::from_rgb(80, 240, 160)));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                    }
                }

                // (Removed) global mono overlay to avoid double/triple drawing.

                // Overlay regions (loop/trim/fade) on top of waveform
                if tab.samples_len > 0 {
                    let to_x = |samp: usize| {
                        wave_left
                            + (((samp.saturating_sub(tab.view_offset)) as f32 / spp)
                                .clamp(0.0, wave_w))
                    };
                    let draw_handle = |x: f32, col: Color32| {
                        let handle_w = 6.0;
                        let handle_h = 16.0;
                        let r = egui::Rect::from_min_max(
                            egui::pos2(x - handle_w * 0.5, rect.top()),
                            egui::pos2(x + handle_w * 0.5, rect.top() + handle_h),
                        );
                        painter.rect_filled(r, 2.0, col);
                    };
                    let sr = self.audio.shared.out_sample_rate.max(1) as f32;

                    let mut fade_in_handle: Option<f32> = None;
                    let mut fade_out_handle: Option<f32> = None;

                    // Selection overlay (Trim tool only)
                    if tab.active_tool == ToolKind::Trim {
                        if let Some((a0, b0)) = tab.selection {
                            let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                            if b >= tab.view_offset {
                                let vis = (wave_w * spp).ceil() as usize;
                                let end = tab.view_offset.saturating_add(vis).min(tab.samples_len);
                                if a <= end {
                                    let ax = to_x(a);
                                    let bx = to_x(b);
                                    let sel_rect = egui::Rect::from_min_max(
                                        egui::pos2(ax, rect.top()),
                                        egui::pos2(bx, rect.bottom()),
                                    );
                                    let fill = Color32::from_rgba_unmultiplied(70, 140, 255, 28);
                                    let stroke = Color32::from_rgba_unmultiplied(70, 140, 255, 140);
                                    painter.rect_filled(sel_rect, 0.0, fill);
                                    painter.rect_stroke(
                                        sel_rect,
                                        0.0,
                                        egui::Stroke::new(1.0, stroke),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                            }
                        }
                    }

                    // Marker overlay
                    if !tab.markers.is_empty() {
                        let vis = (wave_w * spp).ceil() as usize;
                        let start = tab.view_offset.min(tab.samples_len);
                        let end = (start + vis).min(tab.samples_len);
                        let col = Color32::from_rgb(255, 200, 80);
                        for m in tab.markers.iter() {
                            if m.sample < start || m.sample > end {
                                continue;
                            }
                            let x = to_x(m.sample);
                            painter.line_segment(
                                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                                egui::Stroke::new(1.0, col),
                            );
                        }
                    }

                    // Loop overlay
                    if let Some((a0, b0)) = tab.loop_region {
                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                        let active = tab.active_tool == ToolKind::LoopEdit;
                        let line_alpha = if active { 220 } else { 160 };
                        let line = Color32::from_rgba_unmultiplied(60, 160, 255, line_alpha);
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        let ax = to_x(a);
                        if b == a {
                            painter.line_segment(
                                [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                                egui::Stroke::new(2.0, line),
                            );
                            draw_handle(ax, line);
                            painter.text(
                                egui::pos2(ax + 6.0, rect.top() + 2.0),
                                egui::Align2::LEFT_TOP,
                                "S",
                                fid,
                                Color32::from_rgb(170, 200, 255),
                            );
                        } else {
                            let bx = to_x(b);
                            let shade_alpha = if active { 40 } else { 22 };
                            let shade = Color32::from_rgba_unmultiplied(60, 160, 255, shade_alpha);
                            let r = egui::Rect::from_min_max(
                                egui::pos2(ax, rect.top()),
                                egui::pos2(bx, rect.bottom()),
                            );
                            painter.rect_filled(r, 0.0, shade);
                            painter.line_segment(
                                [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                                egui::Stroke::new(2.0, line),
                            );
                            painter.line_segment(
                                [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
                                egui::Stroke::new(2.0, line),
                            );
                            draw_handle(ax, line);
                            draw_handle(bx, line);
                            painter.text(
                                egui::pos2(ax + 6.0, rect.top() + 2.0),
                                egui::Align2::LEFT_TOP,
                                "S",
                                fid.clone(),
                                Color32::from_rgb(170, 200, 255),
                            );
                            painter.text(
                                egui::pos2(bx + 6.0, rect.top() + 2.0),
                                egui::Align2::LEFT_TOP,
                                "E",
                                fid.clone(),
                                Color32::from_rgb(170, 200, 255),
                            );
                            let dur = (b.saturating_sub(a)) as f32 / sr;
                            let label = crate::app::helpers::format_time_s(dur);
                            painter.text(
                                egui::pos2(ax + 6.0, rect.top() + 18.0),
                                egui::Align2::LEFT_TOP,
                                format!("Loop {label}"),
                                fid,
                                Color32::from_rgb(160, 190, 230),
                            );

                            // Crossfade bands and shape
                            let cf = Self::effective_loop_xfade_samples(
                                a,
                                b,
                                tab.samples_len,
                                tab.loop_xfade_samples,
                            );
                            if cf > 0 {
                                let pre0 = a.saturating_sub(cf);
                                let pre1 = (a + cf).min(tab.samples_len);
                                let post0 = b.saturating_sub(cf);
                                let post1 = (b + cf).min(tab.samples_len);
                                let xs0 = to_x(pre0);
                                let xs1 = to_x(pre1);
                                let xe0 = to_x(post0);
                                let xe1 = to_x(post1);
                                let band_alpha = if active { 50 } else { 28 };
                                let col_in = Color32::from_rgba_unmultiplied(255, 180, 60, band_alpha);
                                let col_out = Color32::from_rgba_unmultiplied(60, 180, 255, band_alpha);
                                let r_in = egui::Rect::from_min_max(
                                    egui::pos2(xs0, rect.top()),
                                    egui::pos2(xs1, rect.bottom()),
                                );
                                let r_out = egui::Rect::from_min_max(
                                    egui::pos2(xe0, rect.top()),
                                    egui::pos2(xe1, rect.bottom()),
                                );
                                painter.rect_filled(r_in, 0.0, col_in);
                                painter.rect_filled(r_out, 0.0, col_out);

                                let curve_alpha = if active { 220 } else { 140 };
                                let curve_col = Color32::from_rgba_unmultiplied(255, 170, 60, curve_alpha);
                                let steps = 36;
                                let mut last_in_up: Option<egui::Pos2> = None;
                                let mut last_in_down: Option<egui::Pos2> = None;
                                let mut last_out_up: Option<egui::Pos2> = None;
                                let mut last_out_down: Option<egui::Pos2> = None;
                                let h = rect.height();
                                let y_of = |w: f32| rect.bottom() - w * h;
                                for i in 0..=steps {
                                    let t = (i as f32) / (steps as f32);
                                    let (w_out, w_in) = match tab.loop_xfade_shape {
                                        crate::app::types::LoopXfadeShape::EqualPower => {
                                            let a = core::f32::consts::FRAC_PI_2 * t;
                                            (a.cos(), a.sin())
                                        }
                                        crate::app::types::LoopXfadeShape::Linear => (1.0 - t, t),
                                    };
                                    let x_in = egui::lerp(xs0..=xs1, t);
                                    let p_in_up = egui::pos2(x_in, y_of(w_in));
                                    let p_in_down = egui::pos2(x_in, y_of(w_out));
                                    if let Some(lp) = last_in_up {
                                        painter.line_segment(
                                            [lp, p_in_up],
                                            egui::Stroke::new(2.0, curve_col),
                                        );
                                    }
                                    if let Some(lp) = last_in_down {
                                        painter.line_segment(
                                            [lp, p_in_down],
                                            egui::Stroke::new(2.0, curve_col),
                                        );
                                    }
                                    last_in_up = Some(p_in_up);
                                    last_in_down = Some(p_in_down);

                                    let x_out = egui::lerp(xe0..=xe1, t);
                                    let p_out_up = egui::pos2(x_out, y_of(w_in));
                                    let p_out_down = egui::pos2(x_out, y_of(w_out));
                                    if let Some(lp) = last_out_up {
                                        painter.line_segment(
                                            [lp, p_out_up],
                                            egui::Stroke::new(2.0, curve_col),
                                        );
                                    }
                                    if let Some(lp) = last_out_down {
                                        painter.line_segment(
                                            [lp, p_out_down],
                                            egui::Stroke::new(2.0, curve_col),
                                        );
                                    }
                                    last_out_up = Some(p_out_up);
                                    last_out_down = Some(p_out_down);
                                }
                            }
                        }
                    }

                    // Trim overlay
                    if tab.active_tool == ToolKind::Trim {
                        if let Some((a0, b0)) = tab.trim_range {
                            let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                            let line = Color32::from_rgba_unmultiplied(255, 140, 0, 230);
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let ax = to_x(a);
                            if b == a {
                                painter.line_segment(
                                    [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                                    egui::Stroke::new(2.0, line),
                                );
                                draw_handle(ax, line);
                                painter.text(
                                    egui::pos2(ax + 6.0, rect.top() + 2.0),
                                    egui::Align2::LEFT_TOP,
                                    "A",
                                    fid,
                                    Color32::from_rgb(255, 200, 150),
                                );
                            } else {
                                let bx = to_x(b);
                                let dim = Color32::from_rgba_unmultiplied(0, 0, 0, 80);
                                let keep = Color32::from_rgba_unmultiplied(255, 160, 60, 36);
                                let left = egui::Rect::from_min_max(
                                    egui::pos2(rect.left(), rect.top()),
                                    egui::pos2(ax, rect.bottom()),
                                );
                                let right = egui::Rect::from_min_max(
                                    egui::pos2(bx, rect.top()),
                                    egui::pos2(rect.right(), rect.bottom()),
                                );
                                painter.rect_filled(left, 0.0, dim);
                                painter.rect_filled(right, 0.0, dim);
                                let keep_r = egui::Rect::from_min_max(
                                    egui::pos2(ax, rect.top()),
                                    egui::pos2(bx, rect.bottom()),
                                );
                                painter.rect_filled(keep_r, 0.0, keep);
                                painter.line_segment(
                                    [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                                    egui::Stroke::new(2.0, line),
                                );
                                painter.line_segment(
                                    [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
                                    egui::Stroke::new(2.0, line),
                                );
                                draw_handle(ax, line);
                                draw_handle(bx, line);
                                painter.text(
                                    egui::pos2(ax + 6.0, rect.top() + 2.0),
                                    egui::Align2::LEFT_TOP,
                                    "A",
                                    fid.clone(),
                                    Color32::from_rgb(255, 200, 150),
                                );
                                painter.text(
                                    egui::pos2(bx + 6.0, rect.top() + 2.0),
                                    egui::Align2::LEFT_TOP,
                                    "B",
                                    fid,
                                    Color32::from_rgb(255, 200, 150),
                                );
                            }
                        }
                    }

                    // Fade overlays
                    let draw_fade = |x0: f32, x1: f32, shape: crate::app::types::FadeShape, is_in: bool, base_col: Color32| {
                        let steps = 28;
                        let max_alpha = 80.0;
                        for i in 0..steps {
                            let t0 = i as f32 / steps as f32;
                            let t1 = (i + 1) as f32 / steps as f32;
                            let w0 = if is_in { Self::fade_weight(shape, t0) } else { Self::fade_weight_out(shape, t0) };
                            let w1 = if is_in { Self::fade_weight(shape, t1) } else { Self::fade_weight_out(shape, t1) };
                            let vol0 = w0;
                            let vol1 = w1;
                            let vol = (vol0 + vol1) * 0.5;
                            let alpha = ((1.0 - vol) * max_alpha).clamp(0.0, 255.0) as u8;
                            if alpha == 0 { continue; }
                            let rx0 = egui::lerp(x0..=x1, t0);
                            let rx1 = egui::lerp(x0..=x1, t1);
                            let r = egui::Rect::from_min_max(
                                egui::pos2(rx0, rect.top()),
                                egui::pos2(rx1, rect.bottom()),
                            );
                            painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(base_col.r(), base_col.g(), base_col.b(), alpha));
                        }
                        let curve_col = Color32::from_rgba_unmultiplied(base_col.r(), base_col.g(), base_col.b(), 200);
                        let mut last: Option<egui::Pos2> = None;
                        for i in 0..=steps {
                            let t = i as f32 / steps as f32;
                            let w = if is_in { Self::fade_weight(shape, t) } else { Self::fade_weight_out(shape, t) };
                            let vol = w;
                            let x = egui::lerp(x0..=x1, t);
                            let y = rect.bottom() - vol * rect.height();
                            let p = egui::pos2(x, y);
                            if let Some(lp) = last {
                                painter.line_segment([lp, p], egui::Stroke::new(2.0, curve_col));
                            }
                            last = Some(p);
                        }
                    };
                    if tab.active_tool == ToolKind::Fade {
                        let n_in = ((tab.tool_state.fade_in_ms / 1000.0) * sr).round() as usize;
                        if n_in > 0 {
                            let end = n_in.min(tab.samples_len);
                            let x0 = to_x(0);
                            let x1 = to_x(end);
                            if x1 > x0 + 1.0 {
                                draw_fade(
                                    x0,
                                    x1,
                                    tab.fade_in_shape,
                                    true,
                                    Color32::from_rgb(80, 180, 255),
                                );
                                fade_in_handle = Some(x1);
                                let fid = TextStyle::Monospace.resolve(ui.style());
                                let secs = (end as f32) / sr;
                                painter.text(
                                    egui::pos2(x0 + 6.0, rect.bottom() - 18.0),
                                    egui::Align2::LEFT_BOTTOM,
                                    format!(
                                        "Fade In {}",
                                        crate::app::helpers::format_time_s(secs)
                                    ),
                                    fid,
                                    Color32::from_rgb(150, 190, 230),
                                );
                            }
                        }
                        let n_out = ((tab.tool_state.fade_out_ms / 1000.0) * sr).round() as usize;
                        if n_out > 0 {
                            let start_out = tab.samples_len.saturating_sub(n_out);
                            let x0 = to_x(start_out);
                            let x1 = to_x(tab.samples_len);
                            if x1 > x0 + 1.0 {
                                draw_fade(
                                    x0,
                                    x1,
                                    tab.fade_out_shape,
                                    false,
                                    Color32::from_rgb(255, 160, 90),
                                );
                                fade_out_handle = Some(x0);
                                let fid = TextStyle::Monospace.resolve(ui.style());
                                let secs = (n_out as f32) / sr;
                                painter.text(
                                    egui::pos2(x0 + 6.0, rect.bottom() - 18.0),
                                    egui::Align2::LEFT_BOTTOM,
                                    format!(
                                        "Fade Out {}",
                                        crate::app::helpers::format_time_s(secs)
                                    ),
                                    fid,
                                    Color32::from_rgb(230, 190, 150),
                                );
                            }
                        }
                    }

                    // Cursor feedback for editor handles
                    if pointer_over_canvas {
                        let handle_radius = 7.0;
                        if tab.dragging_marker.is_some() {
                            hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                        } else if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            let x = pos.x;
                            let near = |hx: f32| (x - hx).abs() <= handle_radius;
                            match tab.active_tool {
                                ToolKind::LoopEdit => {
                                    if let Some((a0, b0)) = tab.loop_region {
                                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                        let ax = to_x(a);
                                        let bx = to_x(b);
                                        if near(ax) || near(bx) {
                                            hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                        }
                                    }
                                }
                                ToolKind::Trim => {
                                    if let Some((a0, b0)) = tab.trim_range {
                                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                        let ax = to_x(a);
                                        let bx = to_x(b);
                                        if near(ax) || near(bx) {
                                            hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                        }
                                    }
                                }
                                ToolKind::Fade => {
                                    if let Some(xh) = fade_in_handle {
                                        if near(xh) {
                                            hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                        }
                                    }
                                    if let Some(xh) = fade_out_handle {
                                        if near(xh) {
                                            hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    if let Some(icon) = hover_cursor {
                        ui.output_mut(|o| o.cursor_icon = icon);
                    }
                }

                // Shared playhead across lanes
                if tab.samples_len > 0 {
                    if let Some(buf) = self.audio.shared.samples.load().as_ref() {
                        let len = buf.len().max(1);
                        let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(len);
                        let spp = tab.samples_per_px.max(0.0001);
                        let x = wave_left + ((pos.saturating_sub(tab.view_offset)) as f32 / spp).clamp(0.0, wave_w);
                        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(2.0, Color32::from_rgb(70,140,255)));
                        // Playhead time label
                        let sr_f = self.audio.shared.out_sample_rate.max(1) as f32;
                        let pos_time = (pos as f32) / sr_f;
                        let label = crate::app::helpers::format_time_s(pos_time);
                        let fid = TextStyle::Monospace.resolve(ui.style());
                        let text_pos = egui::pos2(x + 6.0, rect.top() + 2.0);
                        painter.text(text_pos, egui::Align2::LEFT_TOP, label, fid, Color32::from_rgb(180, 200, 220));
                    }
                }

                // Horizontal scrollbar when zoomed in
                if tab.samples_len > 0 {
                    let spp = tab.samples_per_px.max(0.0001);
                    let vis = (wave_w * spp).ceil() as usize;
                    let max_left = tab.samples_len.saturating_sub(vis);
                    if tab.view_offset > max_left {
                        tab.view_offset = max_left;
                    }
                    if max_left > 0 {
                        let mut off = tab.view_offset as f32;
                        let resp = ui.add(
                            egui::Slider::new(&mut off, 0.0..=max_left as f32)
                                .show_value(false)
                                .clamping(egui::SliderClamping::Always),
                        );
                        if resp.changed() {
                            tab.view_offset = off.round().clamp(0.0, max_left as f32) as usize;
                        }
                    }
                }
                    }); // end canvas UI

                    // Inspector area (right)
                    ui.vertical(|ui| {
                        ui.set_width(inspector_w);
                        ui.heading("Inspector");
                        ui.separator();
                        let can_undo = !tab.undo_stack.is_empty();
                        let can_redo = !tab.redo_stack.is_empty();
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(can_undo, egui::Button::new("Undo"))
                                .clicked()
                            {
                                request_undo = true;
                            }
                            if ui
                                .add_enabled(can_redo, egui::Button::new("Redo"))
                                .clicked()
                            {
                                request_redo = true;
                            }
                        });
                        ui.separator();
                        match tab.view_mode {
                            ViewMode::Waveform => {
                                // Tool selector
                                let mut tool = tab.active_tool;
                                egui::ComboBox::new("tool_selector", "Tool")
                                    .selected_text(format!("{:?}", tool))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut tool, ToolKind::LoopEdit, "Loop Edit");
                                        ui.selectable_value(&mut tool, ToolKind::Markers, "Markers");
                                        ui.selectable_value(&mut tool, ToolKind::Trim, "Trim");
                                        ui.selectable_value(&mut tool, ToolKind::Fade, "Fade");
                                        ui.selectable_value(&mut tool, ToolKind::PitchShift, "PitchShift");
                                        ui.selectable_value(&mut tool, ToolKind::TimeStretch, "TimeStretch");
                                        ui.selectable_value(&mut tool, ToolKind::Gain, "Gain");
                                        ui.selectable_value(&mut tool, ToolKind::Normalize, "Normalize");
                                        ui.selectable_value(&mut tool, ToolKind::Reverse, "Reverse");
                                    });
                                if tool != tab.active_tool {
                                    tab.active_tool_last = Some(tab.active_tool);
                                    // Leaving a tool: if we had a runtime preview, restore original audio
                                    if tab.preview_audio_tool.is_some() { need_restore_preview = true; }
                                    stop_playback = true;
                                    tab.active_tool = tool;
                                }
                                ui.separator();
                                ui.label(RichText::new(format!("Tool: {:?}", tab.active_tool)).strong());
                                match tab.active_tool {
                                    // Seek/Select removed: seeking is always available on the canvas
                                    ToolKind::LoopEdit => {
                                        // compact spacing for inspector controls
                                        ui.scope(|ui| {
                                            let s = ui.style_mut();
                                            s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                            s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                            let (s0,e0) = tab.loop_region.unwrap_or((0,0));
                                            ui.label("Loop (samples)");
                                            let mut s_i = s0 as i64;
                                            let mut e_i = e0 as i64;
                                            let max_i = tab.samples_len as i64;
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label("Start:");
                                                let chs = ui.add(egui::DragValue::new(&mut s_i).range(0..=max_i).speed(64.0)).changed();
                                                ui.label("End:");
                                                let che = ui.add(egui::DragValue::new(&mut e_i).range(0..=max_i).speed(64.0)).changed();
                                                if chs || che {
                                                    let mut s = s_i.clamp(0, max_i) as usize;
                                                    let mut e = e_i.clamp(0, max_i) as usize;
                                                    if e < s { std::mem::swap(&mut s, &mut e); }
                                                    tab.loop_region = Some((s,e));
                                                    Self::update_loop_markers_dirty(tab);
                                                    apply_pending_loop = true;
                                                }
                                            });
                                            // Crossfade controls (duration in ms + shape)
                                            let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                            let mut x_ms = (tab.loop_xfade_samples as f32 / sr) * 1000.0;
                                            ui.horizontal_wrapped(|ui| {
                                                ui.label("Xfade (ms):");
                                                if ui.add(egui::DragValue::new(&mut x_ms).range(0.0..=5000.0).speed(5.0).fixed_decimals(1)).changed() {
                                                    let samp = ((x_ms / 1000.0) * sr).round().clamp(0.0, tab.samples_len as f32) as usize;
                                                    tab.loop_xfade_samples = samp;
                                                    apply_pending_loop = true;
                                                }
                                                ui.label("Shape:");
                                                let mut shp = tab.loop_xfade_shape;
                                                egui::ComboBox::from_id_salt("xfade_shape").selected_text(match shp { crate::app::types::LoopXfadeShape::Linear => "Linear", crate::app::types::LoopXfadeShape::EqualPower => "Equal" }).show_ui(ui, |ui| {
                                                    ui.selectable_value(&mut shp, crate::app::types::LoopXfadeShape::Linear, "Linear");
                                                    ui.selectable_value(&mut shp, crate::app::types::LoopXfadeShape::EqualPower, "Equal");
                                                });
                                                if shp != tab.loop_xfade_shape { tab.loop_xfade_shape = shp; apply_pending_loop = true; }
                                            });
                                            ui.horizontal_wrapped(|ui| {
                                                if ui.button("Set Start").on_hover_text("Set Start at playhead").clicked() {
                                                    let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(tab.samples_len);
                                                    let end = tab.loop_region.map(|(_,e)| e).unwrap_or(pos);
                                                    let (mut s, mut e) = (pos, end);
                                                    if e < s { std::mem::swap(&mut s, &mut e); }
                                                    tab.loop_region = Some((s,e));
                                                    Self::update_loop_markers_dirty(tab);
                                                    apply_pending_loop = true;
                                                }
                                                if ui.button("Set End").on_hover_text("Set End at playhead").clicked() {
                                                    let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(tab.samples_len);
                                                    let start = tab.loop_region.map(|(s,_)| s).unwrap_or(pos);
                                                    let (mut s, mut e) = (start, pos);
                                                    if e < s { std::mem::swap(&mut s, &mut e); }
                                                    tab.loop_region = Some((s,e));
                                                    Self::update_loop_markers_dirty(tab);
                                                    apply_pending_loop = true;
                                                }
                                                if ui.button("Clear").clicked() { do_set_loop_from = Some((0,0)); }
                                            });

                                            // Crossfade controls already above; add Apply button to destructively bake Xfade
                                            ui.horizontal_wrapped(|ui| {
                                                let effective_cf = tab
                                                    .loop_region
                                                    .map(|(a, b)| {
                                                        Self::effective_loop_xfade_samples(
                                                            a,
                                                            b,
                                                            tab.samples_len,
                                                            tab.loop_xfade_samples,
                                                        )
                                                    })
                                                    .unwrap_or(0);
                                                if ui
                                                    .add_enabled(
                                                        effective_cf > 0,
                                                        egui::Button::new("Apply Xfade"),
                                                    )
                                                    .on_hover_text(
                                                        "Bake crossfade into data at loop boundary",
                                                    )
                                                    .clicked()
                                                {
                                                    do_apply_xfade = true;
                                                }
                                            });
                                            ui.horizontal_wrapped(|ui| {
                                                let label = if tab.loop_region.is_some() {
                                                    "Write Markers to File"
                                                } else {
                                                    "Clear Markers in File"
                                                };
                                                if ui
                                                    .add_enabled(
                                                        tab.loop_markers_dirty,
                                                        egui::Button::new(label),
                                                    )
                                                    .on_hover_text(
                                                        "Write loop markers into file metadata",
                                                    )
                                                    .clicked()
                                                {
                                                    do_write_loop_markers = true;
                                                }
                                            });

                                            // Dynamic preview overlay for LoopEdit (non-destructive):
                                            // Build a mono preview applying the current loop crossfade to the mixdown.
                                            if !preview_ok {
                                                ui.label(RichText::new("Preview disabled for large clips").weak());
                                            } else if let Some((a,b)) = tab.loop_region {
                                                let cf = Self::effective_loop_xfade_samples(
                                                    a,
                                                    b,
                                                    tab.samples_len,
                                                    tab.loop_xfade_samples,
                                                );
                                                if cf > 0 {
                                                    // Build per-channel overlay applying crossfade across centered windows
                                                    let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                    let win_len = cf.saturating_mul(2);
                                                    let denom = (win_len.saturating_sub(1)).max(1) as f32;
                                                    let s_start = a.saturating_sub(cf);
                                                    let e_start = b.saturating_sub(cf);
                                                    for ch in overlay.iter_mut() {
                                                        for i in 0..win_len {
                                                            let s_idx = s_start.saturating_add(i);
                                                            let e_idx = e_start.saturating_add(i);
                                                            if s_idx >= ch.len() || e_idx >= ch.len() {
                                                                break;
                                                            }
                                                            let t = (i as f32) / denom;
                                                            let (w_out, w_in) = match tab.loop_xfade_shape {
                                                                crate::app::types::LoopXfadeShape::EqualPower => {
                                                                    let ang = core::f32::consts::FRAC_PI_2 * t; (ang.cos(), ang.sin())
                                                                }
                                                                crate::app::types::LoopXfadeShape::Linear => (1.0 - t, t),
                                                            };
                                                            let s = ch[s_idx];
                                                            let e = ch[e_idx];
                                                            let mixed = e * w_out + s * w_in;
                                                            ch[s_idx] = mixed;
                                                            ch[e_idx] = mixed;
                                                        }
                                                    }
                                                    let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                    tab.preview_overlay = Some(PreviewOverlay { channels: overlay, source_tool: ToolKind::LoopEdit, timeline_len });
                                                    tab.preview_audio_tool = Some(ToolKind::LoopEdit);
                                                }
                                            }
                                        });
                                    }
                                    
                                                                        ToolKind::Markers => {
                                        ui.scope(|ui| {
                                            let s = ui.style_mut();
                                            s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                            s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                            let out_sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                            ui.horizontal_wrapped(|ui| {
                                                if ui.button("Add at Playhead").clicked() {
                                                    let pos = self
                                                        .audio
                                                        .shared
                                                        .play_pos
                                                        .load(std::sync::atomic::Ordering::Relaxed)
                                                        .min(tab.samples_len);
                                                    let label = Self::next_marker_label(&tab.markers);
                                                    let entry = crate::markers::MarkerEntry {
                                                        sample: pos,
                                                        label,
                                                    };
                                                    match tab.markers.binary_search_by_key(&pos, |m| m.sample) {
                                                        Ok(idx) => {
                                                            tab.markers[idx] = entry;
                                                        }
                                                        Err(idx) => {
                                                            tab.markers.insert(idx, entry);
                                                        }
                                                    }
                                                    tab.markers_dirty = true;
                                                }
                                                if ui
                                                    .add_enabled(
                                                        !tab.markers.is_empty(),
                                                        egui::Button::new("Clear"),
                                                    )
                                                    .clicked()
                                                {
                                                    tab.markers.clear();
                                                    tab.markers_dirty = true;
                                                }
                                            });
                                            ui.horizontal_wrapped(|ui| {
                                                let label = if tab.markers.is_empty() {
                                                    "Clear Markers File"
                                                } else {
                                                    "Write Markers to File"
                                                };
                                                if ui
                                                    .add_enabled(
                                                        tab.markers_dirty,
                                                        egui::Button::new(label),
                                                    )
                                                    .clicked()
                                                {
                                                    do_write_markers = true;
                                                }
                                            });
                                            ui.label(format!("Count: {}", tab.markers.len()));
                                            if !tab.markers.is_empty() {
                                                let samples_len = tab.samples_len;
                                                let len_sec = (samples_len as f32 / out_sr).max(0.0);
                                                let mut remove_idx: Option<usize> = None;
                                                let mut resort = false;
                                                let mut dirty = false;
                                                egui::ScrollArea::vertical()
                                                    .max_height(160.0)
                                                    .show(ui, |ui| {
                                                        for (idx, m) in tab.markers.iter_mut().enumerate() {
                                                            let mut secs = (m.sample as f32) / out_sr;
                                                            ui.horizontal(|ui| {
                                                                let resp = ui.add(
                                                                    egui::TextEdit::singleline(&mut m.label)
                                                                        .desired_width(80.0),
                                                                );
                                                                if resp.changed() {
                                                                    dirty = true;
                                                                }
                                                                let time_changed = ui
                                                                    .add(
                                                                        egui::DragValue::new(&mut secs)
                                                                            .range(0.0..=len_sec)
                                                                            .speed(0.01)
                                                                            .fixed_decimals(3),
                                                                    )
                                                                    .changed();
                                                                if time_changed {
                                                                    let sample = ((secs.max(0.0)) * out_sr)
                                                                        .round() as usize;
                                                                    m.sample = sample.min(samples_len);
                                                                    dirty = true;
                                                                    resort = true;
                                                                }
                                                                ui.label(crate::app::helpers::format_time_s(secs));
                                                                if ui.button("Delete").clicked() {
                                                                    remove_idx = Some(idx);
                                                                }
                                                            });
                                                        }
                                                    });
                                                if let Some(idx) = remove_idx {
                                                    tab.markers.remove(idx);
                                                    dirty = true;
                                                }
                                                if resort {
                                                    tab.markers.sort_by_key(|m| m.sample);
                                                }
                                                if dirty {
                                                    tab.markers_dirty = true;
                                                }
                                            }
                                        });
                                    }
ToolKind::Trim => {
                                        ui.scope(|ui| {
                                            let s = ui.style_mut();
                                            s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                            s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                            if !preview_ok {
                                                ui.label(RichText::new("Preview disabled for large clips").weak());
                                            }
                                            // Trim has its own A/B range (independent from loop)
                                            let mut range_opt = tab.trim_range;
                                            let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                            let len_sec = (tab.samples_len as f32 / sr).max(0.0);
                                            if let Some((smp,emp)) = range_opt { ui.label(format!("Trim A?B: {}..{} samp", smp, emp)); } else { ui.label("Trim A?B: (set below)"); }
                                            ui.horizontal_wrapped(|ui| {
                                                let mut s_sec = range_opt.map(|(s, _)| s as f32 / sr).unwrap_or(0.0);
                                                let mut e_sec = range_opt.map(|(_, e)| e as f32 / sr).unwrap_or(0.0);
                                                ui.label("Start (s):");
                                                let chs = ui.add(egui::DragValue::new(&mut s_sec).range(0.0..=len_sec).speed(0.05).fixed_decimals(3)).changed();
                                                ui.label("End (s):");
                                                let che = ui.add(egui::DragValue::new(&mut e_sec).range(0.0..=len_sec).speed(0.05).fixed_decimals(3)).changed();
                                                if chs || che {
                                                    let mut s = ((s_sec.max(0.0)) * sr).round() as usize;
                                                    let mut e = ((e_sec.max(0.0)) * sr).round() as usize;
                                                    if e < s { std::mem::swap(&mut s, &mut e); }
                                                    s = s.min(tab.samples_len);
                                                    e = e.min(tab.samples_len);
                                                    tab.trim_range = Some((s, e));
                                                    tab.selection = Some((s, e));
                                                    range_opt = tab.trim_range;
                                                    if preview_ok && e > s {
                                                        let mut mono = Self::editor_mixdown_mono(tab);
                                                        mono = mono[s..e].to_vec();
                                                        pending_preview = Some((ToolKind::Trim, mono));
                                                        stop_playback = true;
                                                        tab.preview_audio_tool = Some(ToolKind::Trim);
                                                    } else {
                                                        tab.preview_audio_tool = None;
                                                        tab.preview_overlay = None;
                                                    }
                                                }
                                            });
                                            // A/B setters from playhead
                                            ui.horizontal_wrapped(|ui| {
                                                if ui.button("Set A").on_hover_text("Set A at playhead").clicked() {
                                                    let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(tab.samples_len);
                                                let new_r = match tab.trim_range { None => Some((pos, pos)), Some((_a,b)) => Some((pos.min(b), pos.max(b))) };
                                                    tab.trim_range = new_r;
                                                    if let Some((a,b)) = tab.trim_range { if b>a {
                                                        if preview_ok {
                                                            // live preview: keep-only A?B
                                                            let mut mono = Self::editor_mixdown_mono(tab);
                                                            mono = mono[a..b].to_vec();
                                                            pending_preview = Some((ToolKind::Trim, mono));
                                                            stop_playback = true;
                                                            tab.preview_audio_tool = Some(ToolKind::Trim);
                                                        } else {
                                                            tab.preview_audio_tool = None;
                                                            tab.preview_overlay = None;
                                                        }
                                                    } }
                                                }
                                                if ui.button("Set B").on_hover_text("Set B at playhead").clicked() {
                                                    let pos = self.audio.shared.play_pos.load(std::sync::atomic::Ordering::Relaxed).min(tab.samples_len);
                                                let new_r = match tab.trim_range { None => Some((pos, pos)), Some((a,_b)) => Some((a.min(pos), a.max(pos))) };
                                                    tab.trim_range = new_r;
                                                    if let Some((a,b)) = tab.trim_range { if b>a {
                                                        if preview_ok {
                                                            let mut mono = Self::editor_mixdown_mono(tab);
                                                            mono = mono[a..b].to_vec();
                                                            pending_preview = Some((ToolKind::Trim, mono));
                                                            stop_playback = true;
                                                            tab.preview_audio_tool = Some(ToolKind::Trim);
                                                        } else {
                                                            tab.preview_audio_tool = None;
                                                            tab.preview_overlay = None;
                                                        }
                                                    } }
                                                }
                                                if ui.button("Clear").clicked() { tab.trim_range = None; need_restore_preview = true; }
                                            });
                                            range_opt = tab.trim_range;
                                            // Actions
                                            ui.horizontal_wrapped(|ui| {
                                            let dis = !range_opt.map(|(s,e)| e> s).unwrap_or(false);
                                            let range = range_opt.unwrap_or((0,0));
                                            if ui.add_enabled(!dis, egui::Button::new("Cut+Join")).clicked() { do_cutjoin = Some(range); }
                                            if ui.add_enabled(!dis, egui::Button::new("Apply Keep A?B")).clicked() { do_trim = Some(range); tab.preview_audio_tool=None; }
                                        });
                                        });
                                    }
                                    ToolKind::Fade => {
                                        // Simplified: duration (seconds) from start/end + Apply
                                        ui.scope(|ui| {
                                            let s = ui.style_mut();
                                            s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                            s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                            if !preview_ok {
                                                ui.label(RichText::new("Preview disabled for large clips").weak());
                                            }
                                            let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                            let shape_label = |shape: crate::app::types::FadeShape| match shape {
                                                crate::app::types::FadeShape::Linear => "Linear",
                                                crate::app::types::FadeShape::EqualPower => "Equal",
                                                crate::app::types::FadeShape::Cosine => "Cosine",
                                                crate::app::types::FadeShape::SCurve => "S-Curve",
                                                crate::app::types::FadeShape::Quadratic => "Quadratic",
                                                crate::app::types::FadeShape::Cubic => "Cubic",
                                            };
                                            // Fade In
                                            ui.label("Fade In");
                                            ui.horizontal_wrapped(|ui| {
                                                let mut secs = tab.tool_state.fade_in_ms / 1000.0;
                                                ui.label("duration (s)");
                                                let mut changed = ui
                                                    .add(
                                                        egui::DragValue::new(&mut secs)
                                                            .range(0.0..=600.0)
                                                            .speed(0.05)
                                                            .fixed_decimals(2),
                                                    )
                                                    .changed();
                                                ui.label("shape");
                                                let mut shape = tab.fade_in_shape;
                                                egui::ComboBox::from_id_salt("fade_in_shape")
                                                    .selected_text(shape_label(shape))
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::Linear,
                                                            "Linear",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::EqualPower,
                                                            "Equal",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::Cosine,
                                                            "Cosine",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::SCurve,
                                                            "S-Curve",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::Quadratic,
                                                            "Quadratic",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::Cubic,
                                                            "Cubic",
                                                        );
                                                    });
                                                if shape != tab.fade_in_shape {
                                                    tab.fade_in_shape = shape;
                                                    changed = true;
                                                }
                                                if changed {
                                                    tab.tool_state = ToolState{ fade_in_ms: (secs*1000.0).max(0.0), ..tab.tool_state };
                                                    if preview_ok {
                                                        // Live preview (per-channel overlay) + mono audition
                                                        let n = ((secs) * sr).round() as usize;
                                                        // Build overlay per channel
                                                        let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                        for ch in overlay.iter_mut() {
                                                            let nn = n.min(ch.len());
                                                            for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight(tab.fade_in_shape, t); ch[i] *= w; }
                                                        }
                                                        let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                        tab.preview_overlay = Some(PreviewOverlay { channels: overlay.clone(), source_tool: ToolKind::Fade, timeline_len });
                                                        // Mono audition
                                                        let mut mono = Self::editor_mixdown_mono(tab);
                                                        let nn = n.min(mono.len());
                                                        for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight(tab.fade_in_shape, t); mono[i] *= w; }
                                                        pending_preview = Some((ToolKind::Fade, mono));
                                                        stop_playback = true;
                                                        tab.preview_audio_tool = Some(ToolKind::Fade);
                                                    } else {
                                                        tab.preview_audio_tool = None;
                                                        tab.preview_overlay = None;
                                                    }
                                                }
                                                if ui.add_enabled(secs>0.0, egui::Button::new("Apply")).clicked() {
                                                    let n = ((secs) * sr).round() as usize;
                                                    do_fade_in = Some(((0, n.min(tab.samples_len)), tab.fade_in_shape));
                                                    tab.preview_audio_tool = None; // will be rebuilt from destructive result below
                                                    tab.preview_overlay = None;
                                                }
                                            });
                                            ui.separator();
                                            // Fade Out
                                            ui.label("Fade Out");
                                            ui.horizontal_wrapped(|ui| {
                                                let mut secs = tab.tool_state.fade_out_ms / 1000.0;
                                                ui.label("duration (s)");
                                                let mut changed = ui
                                                    .add(
                                                        egui::DragValue::new(&mut secs)
                                                            .range(0.0..=600.0)
                                                            .speed(0.05)
                                                            .fixed_decimals(2),
                                                    )
                                                    .changed();
                                                ui.label("shape");
                                                let mut shape = tab.fade_out_shape;
                                                egui::ComboBox::from_id_salt("fade_out_shape")
                                                    .selected_text(shape_label(shape))
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::Linear,
                                                            "Linear",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::EqualPower,
                                                            "Equal",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::Cosine,
                                                            "Cosine",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::SCurve,
                                                            "S-Curve",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::Quadratic,
                                                            "Quadratic",
                                                        );
                                                        ui.selectable_value(
                                                            &mut shape,
                                                            crate::app::types::FadeShape::Cubic,
                                                            "Cubic",
                                                        );
                                                    });
                                                if shape != tab.fade_out_shape {
                                                    tab.fade_out_shape = shape;
                                                    changed = true;
                                                }
                                                if changed {
                                                    tab.tool_state = ToolState{ fade_out_ms: (secs*1000.0).max(0.0), ..tab.tool_state };
                                                    if preview_ok {
                                                        let n = ((secs) * sr).round() as usize;
                                                        // per-channel overlay
                                                        let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                        for ch in overlay.iter_mut() {
                                                            let len = ch.len(); let nn = n.min(len);
                                                            for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight_out(tab.fade_out_shape, t); let idx = len - nn + i; ch[idx] *= w; }
                                                        }
                                                        let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                        tab.preview_overlay = Some(PreviewOverlay { channels: overlay.clone(), source_tool: ToolKind::Fade, timeline_len });
                                                        // mono audition
                                                        let mut mono = Self::editor_mixdown_mono(tab);
                                                        let len = mono.len(); let nn = n.min(len);
                                                        for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight_out(tab.fade_out_shape, t); let idx = len - nn + i; mono[idx] *= w; }
                                                        pending_preview = Some((ToolKind::Fade, mono));
                                                        stop_playback = true;
                                                        tab.preview_audio_tool = Some(ToolKind::Fade);
                                                    } else {
                                                        tab.preview_audio_tool = None;
                                                        tab.preview_overlay = None;
                                                    }
                                                }
                                                if ui.add_enabled(secs>0.0, egui::Button::new("Apply")).clicked() {
                                                    let n = ((secs) * sr).round() as usize;
                                                    do_fade_out = Some(((0, n.min(tab.samples_len)), tab.fade_out_shape));
                                                    tab.preview_audio_tool = None;
                                                    tab.preview_overlay = None;
                                                }
                                            });
                                        });
                                    }
                                    ToolKind::PitchShift => {
                                        ui.scope(|ui| {
                                            let s = ui.style_mut(); s.spacing.item_spacing = egui::vec2(6.0,6.0); s.spacing.button_padding = egui::vec2(6.0,3.0);
                                            if !preview_ok {
                                                ui.label(RichText::new("Preview disabled for large clips").weak());
                                            }
                                            let mut semi = tab.tool_state.pitch_semitones;
                                            ui.label("Semitones");
                                            let changed = ui.add(egui::DragValue::new(&mut semi).range(-12.0..=12.0).speed(0.1).fixed_decimals(2)).changed();
                                            if changed {
                                                tab.tool_state = ToolState{ pitch_semitones: semi, ..tab.tool_state };
                                                if preview_ok {
                                                    let mono = Self::editor_mixdown_mono(tab);
                                                    pending_heavy_preview = Some((ToolKind::PitchShift, mono, semi));
                                                    stop_playback = true;
                                                    // Defer overlay spawn to avoid nested &mut borrow
                                                    pending_overlay_job = Some((ToolKind::PitchShift, semi));
                                                    tab.preview_audio_tool = Some(ToolKind::PitchShift);
                                                } else {
                                                    tab.preview_audio_tool = None;
                                                    tab.preview_overlay = None;
                                                }
                                            }
                                            if overlay_busy || apply_busy { ui.add(egui::Spinner::new()); }
                                            if ui.add_enabled(!apply_busy, egui::Button::new("Apply")).clicked() {
                                                pending_pitch_apply = Some(tab.tool_state.pitch_semitones);
                                            }
                                        });
                                    }
                                    ToolKind::TimeStretch => {
                                        ui.scope(|ui| {
                                            let s = ui.style_mut(); s.spacing.item_spacing = egui::vec2(6.0,6.0); s.spacing.button_padding = egui::vec2(6.0,3.0);
                                            if !preview_ok {
                                                ui.label(RichText::new("Preview disabled for large clips").weak());
                                            }
                                            let mut rate = tab.tool_state.stretch_rate;
                                            ui.label("Rate");
                                            let changed = ui.add(egui::DragValue::new(&mut rate).range(0.25..=4.0).speed(0.02).fixed_decimals(2)).changed();
                                            if changed {
                                                tab.tool_state = ToolState{ stretch_rate: rate, ..tab.tool_state };
                                                if preview_ok {
                                                    let mono = Self::editor_mixdown_mono(tab);
                                                    pending_heavy_preview = Some((ToolKind::TimeStretch, mono, rate));
                                                    stop_playback = true;
                                                    // Defer overlay spawn to avoid nested &mut borrow
                                                    pending_overlay_job = Some((ToolKind::TimeStretch, rate));
                                                    tab.preview_audio_tool = Some(ToolKind::TimeStretch);
                                                } else {
                                                    tab.preview_audio_tool = None;
                                                    tab.preview_overlay = None;
                                                }
                                            }
                                            if overlay_busy || apply_busy { ui.add(egui::Spinner::new()); }
                                            if ui.add_enabled(!apply_busy, egui::Button::new("Apply")).clicked() {
                                                pending_stretch_apply = Some(tab.tool_state.stretch_rate);
                                            }
                                        });
                                    }
                                    ToolKind::Gain => {
                                        if !preview_ok {
                                            ui.label(RichText::new("Preview disabled for large clips").weak());
                                        }
                                        let st = tab.tool_state;
                                        let mut gain_db = st.gain_db;
                                        ui.label("Gain (dB)"); ui.add(egui::DragValue::new(&mut gain_db).range(-24.0..=24.0).speed(0.1));
                                        tab.tool_state = ToolState{ gain_db, ..tab.tool_state };
                                        // live preview on change
                                        if (gain_db - st.gain_db).abs() > 1e-6 {
                                            if preview_ok {
                                                let g = db_to_amp(gain_db);
                                                // per-channel overlay
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                for ch in overlay.iter_mut() { for v in ch.iter_mut() { *v *= g; } }
                                                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                tab.preview_overlay = Some(PreviewOverlay { channels: overlay, source_tool: ToolKind::Gain, timeline_len });
                                                // mono audition
                                                let mut mono = Self::editor_mixdown_mono(tab);
                                                for v in &mut mono { *v *= g; }
                                                pending_preview = Some((ToolKind::Gain, mono));
                                                stop_playback = true;
                                                tab.preview_audio_tool = Some(ToolKind::Gain);
                                            } else {
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                            }
                                        }
                                        if ui.button("Apply").clicked() { do_gain = Some(((0, tab.samples_len), gain_db)); tab.preview_audio_tool=None; tab.preview_overlay=None; }
                                    }
                                    ToolKind::Normalize => {
                                        if !preview_ok {
                                            ui.label(RichText::new("Preview disabled for large clips").weak());
                                        }
                                        let st = tab.tool_state;
                                        let mut target_db = st.normalize_target_db;
                                        ui.label("Target dBFS"); ui.add(egui::DragValue::new(&mut target_db).range(-24.0..=0.0).speed(0.1));
                                        tab.tool_state = ToolState{ normalize_target_db: target_db, ..tab.tool_state };
                                        if preview_ok {
                                            let changed = (target_db - st.normalize_target_db).abs() > 1e-6;
                                            if changed {
                                                // live preview: compute gain to reach target (based on current peak)
                                                let mut mono = Self::editor_mixdown_mono(tab);
                                                if !mono.is_empty() {
                                                    let mut peak = 0.0f32; for &v in &mono { peak = peak.max(v.abs()); }
                                                    if peak > 0.0 {
                                                        let g = db_to_amp(target_db) / peak.max(1e-12);
                                                        // per-channel overlay
                                                        let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                        for ch in overlay.iter_mut() { for v in ch.iter_mut() { *v *= g; } }
                                                        let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                        tab.preview_overlay = Some(PreviewOverlay { channels: overlay, source_tool: ToolKind::Normalize, timeline_len });
                                                        // mono audition
                                                        for v in &mut mono { *v *= g; }
                                                        pending_preview = Some((ToolKind::Normalize, mono));
                                                        stop_playback = true;
                                                        tab.preview_audio_tool = Some(ToolKind::Normalize);
                                                    }
                                                }
                                            }
                                        } else {
                                            tab.preview_audio_tool = None;
                                            tab.preview_overlay = None;
                                        }
                                        if ui.button("Apply").clicked() { do_normalize = Some(((0, tab.samples_len), target_db)); tab.preview_audio_tool=None; tab.preview_overlay=None; }
                                    }
                                    ToolKind::Reverse => {
                                        if !preview_ok {
                                            ui.label(RichText::new("Preview disabled for large clips").weak());
                                        }
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.add_enabled(preview_ok, egui::Button::new("Preview")).clicked() {
                                                // per-channel overlay
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                for ch in overlay.iter_mut() { ch.reverse(); }
                                                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                tab.preview_overlay = Some(PreviewOverlay { channels: overlay, source_tool: ToolKind::Reverse, timeline_len });
                                                // mono audition
                                                let mut mono = Self::editor_mixdown_mono(tab);
                                                mono.reverse();
                                                pending_preview = Some((ToolKind::Reverse, mono));
                                                stop_playback = true;
                                                tab.preview_audio_tool = Some(ToolKind::Reverse);
                                            }
                                            if ui.button("Apply").clicked() { do_reverse = Some((0, tab.samples_len)); tab.preview_audio_tool=None; tab.preview_overlay=None; }
                                            if ui.button("Cancel").clicked() { need_restore_preview = true; }
                                        });
                                    }
                                }
                            }
                            _ => { ui.label("Tools for this view will appear here."); }
                        }
                    }); // end inspector
                    if let Some((tool, mono, p)) = pending_heavy_preview {
                        if self.is_decode_failed_path(&self.tabs[tab_idx].path) {
                            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                                tab.preview_audio_tool = None;
                                tab.preview_overlay = None;
                            }
                        } else {
                            self.spawn_heavy_preview_owned(mono, tool, p);
                        }
                    }
                    if let Some(semi) = pending_pitch_apply {
                        self.spawn_editor_apply_for_tab(tab_idx, ToolKind::PitchShift, semi);
                    }
                    if let Some(rate) = pending_stretch_apply {
                        self.spawn_editor_apply_for_tab(tab_idx, ToolKind::TimeStretch, rate);
                    }
                    if stop_playback { self.audio.stop(); }
                    if need_restore_preview { self.clear_preview_if_any(tab_idx); }
                    if let Some(s) = request_seek { self.audio.seek_to_sample(s); }
                    if let Some((tool_kind, mono)) = pending_preview { self.set_preview_mono(tab_idx, tool_kind, mono); }
                }); // end horizontal split

                // perform pending actions after borrows end
                // Defer starting heavy overlay until after UI to avoid nested &mut self borrow (E0499)
                if let Some((tool, p)) = pending_overlay_job {
                    if !self.is_decode_failed_path(&self.tabs[tab_idx].path) {
                        self.spawn_heavy_overlay_for_tab(tab_idx, tool, p);
                    }
                }
                if request_undo {
                    self.clear_preview_if_any(tab_idx);
                    self.editor_apply_state = None;
                    self.undo_in_tab(tab_idx);
                }
                if request_redo {
                    self.clear_preview_if_any(tab_idx);
                    self.editor_apply_state = None;
                    self.redo_in_tab(tab_idx);
                }
                if let Some((s,e)) = do_set_loop_from {
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        if s==0 && e==0 {
                            tab.loop_region=None;
                        } else {
                            tab.loop_region=Some((s,e));
                            if tab.loop_mode==LoopMode::Marker { self.audio.set_loop_enabled(true); self.audio.set_loop_region(s,e); }
                        }
                        Self::update_loop_markers_dirty(tab);
                    }
                }
                if let Some((s,e)) = do_trim { self.editor_apply_trim_range(tab_idx, (s,e)); }
                if let Some(((s,e), in_ms, out_ms)) = do_fade { self.editor_apply_fade_range(tab_idx, (s,e), in_ms, out_ms); }
                if let Some(((s,e), shp)) = do_fade_in { self.editor_apply_fade_in_explicit(tab_idx, (s,e), shp); }
                if let Some(((mut s,mut e), shp)) = do_fade_out {
                    // If range provided is (0, n) as length, anchor to end
                    if let Some(tab) = self.tabs.get(tab_idx) {
                        let len = tab.samples_len;
                        if s == 0 { s = len.saturating_sub(e); e = len; }
                    }
                    self.editor_apply_fade_out_explicit(tab_idx, (s,e), shp);
                }
                if let Some(((s,e), gdb)) = do_gain { self.editor_apply_gain_range(tab_idx, (s,e), gdb); }
                if let Some(((s,e), tdb)) = do_normalize { self.editor_apply_normalize_range(tab_idx, (s,e), tdb); }
                if let Some((s,e)) = do_reverse { self.editor_apply_reverse_range(tab_idx, (s,e)); }
                if let Some((_,_)) = do_cutjoin { if let Some(tab) = self.tabs.get_mut(tab_idx) { tab.trim_range = None; } }
                if let Some((s,e)) = do_cutjoin { self.editor_delete_range_and_join(tab_idx, (s,e)); }
                if do_apply_xfade { self.editor_apply_loop_xfade(tab_idx); }
                if do_write_loop_markers { self.write_loop_markers_for_tab(tab_idx); }
                if do_write_markers { self.write_markers_for_tab(tab_idx); }
                if apply_pending_loop { if let Some(tab_ro) = self.tabs.get(tab_idx) { self.apply_loop_mode_for_tab(tab_ro); } }
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
                let header_h = text_height * 1.6; let row_h = self.wave_row_h.max(text_height * 1.3);
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
                    .column(egui_extras::Column::initial(200.0).resizable(true))     // File (resizable)
                    .column(egui_extras::Column::initial(250.0).resizable(true))     // Folder (resizable)
                    .column(egui_extras::Column::initial(60.0).resizable(true))      // Length (resizable)
                    .column(egui_extras::Column::initial(40.0).resizable(true))      // Ch (resizable)
                    .column(egui_extras::Column::initial(70.0).resizable(true))      // SampleRate (resizable)
                    .column(egui_extras::Column::initial(50.0).resizable(true))      // Bits (resizable)
                    .column(egui_extras::Column::initial(90.0).resizable(true))      // Level (original)
                    .column(egui_extras::Column::initial(90.0).resizable(true))      // LUFS (Integrated)
                    .column(egui_extras::Column::initial(80.0).resizable(true))      // Gain (editable)
                    .column(egui_extras::Column::initial(150.0).resizable(true))     // Wave (resizable)
                    .column(egui_extras::Column::remainder())                        // Spacer (fills remainder)
                    .min_scrolled_height((avail_h - header_h).max(0.0));

                table.header(header_h, |mut header| {
                    header.col(|ui| { sort_changed |= sortable_header(ui, "File", &mut self.sort_key, &mut self.sort_dir, SortKey::File, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "Folder", &mut self.sort_key, &mut self.sort_dir, SortKey::Folder, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "Length", &mut self.sort_key, &mut self.sort_dir, SortKey::Length, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "Ch", &mut self.sort_key, &mut self.sort_dir, SortKey::Channels, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "SR", &mut self.sort_key, &mut self.sort_dir, SortKey::SampleRate, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "Bits", &mut self.sort_key, &mut self.sort_dir, SortKey::Bits, true); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "dBFS (Peak)", &mut self.sort_key, &mut self.sort_dir, SortKey::Level, false); });
                    header.col(|ui| { sort_changed |= sortable_header(ui, "LUFS (I)", &mut self.sort_key, &mut self.sort_dir, SortKey::Lufs, false); });
                    header.col(|ui| { ui.label(RichText::new("Gain (dB)").strong()); });
                    header.col(|ui| { ui.label(RichText::new("Wave").strong()); });
                    header.col(|_ui| { /* spacer */ });
                }).body(|body| {
                    let data_len = self.files.len();
                    // Ensure the table body fills the remaining height
                    let min_rows_for_height = ((avail_h - header_h).max(0.0) / row_h).ceil() as usize;
                    let total_rows = data_len.max(min_rows_for_height);

                    // Use virtualized rows for performance with large lists
                    body.rows(row_h, total_rows, |mut row| {
                        let row_idx = row.index();
                        let is_data = row_idx < data_len;
                        let is_selected = self.selected_multi.contains(&row_idx);
                        row.set_selected(is_selected);

                        if is_data {
                            let Some(path_owned) = self.path_for_row(row_idx).cloned() else { return; };
                            let name = path_owned.file_name().and_then(|s| s.to_str()).unwrap_or("(invalid)");
                            let parent = path_owned.parent().and_then(|p| p.to_str()).unwrap_or("");
                            let mut clicked_to_load = false;
                            let mut clicked_to_select = false;
                            // Ensure quick header meta is present when row is shown
                            if self.meta_for_path(&path_owned).is_none() {
                                if let Ok(info) = crate::audio_io::read_audio_info(&path_owned) {
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
                                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    let mark = if self.has_pending_gain(&path_owned) { " ?" } else { "" };
                                    let resp = ui.add(
                                        egui::Label::new(RichText::new(format!("{}{}", name, mark)).size(text_height * 1.05))
                                            .sense(Sense::click())
                                            .truncate()
                                            .show_tooltip_when_elided(false)
                                    ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                    
                                    // NOTE: invalid-encoding comment removed
                                    
                                    // NOTE: invalid-encoding comment removed
                                    if resp.double_clicked() { clicked_to_select = true; to_open = Some(path_owned.clone()); }
                                    
                                    if resp.hovered() {
                                        resp.on_hover_text(name);
                                    }
                                });
                            });
                            // col 1: Folder (clickable label with clipping)
                            row.col(|ui| {
                                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    let resp = ui.add(
                                        egui::Label::new(RichText::new(parent).monospace().size(text_height * 1.0))
                                            .sense(Sense::click())
                                            .truncate()
                                            .show_tooltip_when_elided(false)
                                    ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                    
                                    // NOTE: invalid-encoding comment removed
                                    
                                    // NOTE: invalid-encoding comment removed
                                    
                                    if resp.hovered() {
                                        resp.on_hover_text(parent);
                                    }
                                });
                            });
                            // col 2: Length (mm:ss) - clickable
                            row.col(|ui| {
                                let secs = meta.as_ref().and_then(|m| m.duration_secs).unwrap_or(f32::NAN);
                                let text = if secs.is_finite() { format_duration(secs) } else { "...".into() };
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(text).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() { clicked_to_load = true; }
                            });
                            // col 3: Channels - clickable
                            row.col(|ui| {
                                let ch = meta.as_ref().map(|m| m.channels).unwrap_or(0);
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(format!("{}", ch)).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() { clicked_to_load = true; }
                            });
                            // col 4: Sample rate - clickable
                            row.col(|ui| {
                                let sr = meta.as_ref().map(|m| m.sample_rate).unwrap_or(0);
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(format!("{}", sr)).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() { clicked_to_load = true; }
                            });
                            // col 5: Bits per sample - clickable
                            row.col(|ui| {
                                let bits = meta.as_ref().map(|m| m.bits_per_sample).unwrap_or(0);
                                let resp = ui.add(
                                    egui::Label::new(RichText::new(format!("{}", bits)).monospace())
                                        .sense(Sense::click())
                                ).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if resp.clicked() { clicked_to_load = true; }
                            });
                            // NOTE: invalid-encoding comment removed
                            row.col(|ui| {
                                let (rect2, resp2) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::click());
                                let gain_db = self.pending_gain_db_for_path(&path_owned);
                                let orig = meta.as_ref().and_then(|m| m.peak_db);
                                let adj = orig.map(|db| db + gain_db);
                                if let Some(db) = adj { ui.painter().rect_filled(rect2, 4.0, db_to_color(db)); }
                                let text = adj.map(|db| format!("{:.1}", db)).unwrap_or_else(|| "...".into());
                                let fid = TextStyle::Monospace.resolve(ui.style());
                                ui.painter().text(rect2.center(), egui::Align2::CENTER_CENTER, text, fid, Color32::WHITE);
                                if resp2.clicked() { clicked_to_load = true; }
                                // (optional tooltip removed to avoid borrow and unused warnings)
                            });
                            // col 7: LUFS (Integrated) with background color (same palette as dBFS)
                            row.col(|ui| {
                                let base = meta.as_ref().and_then(|m| m.lufs_i);
                                let gain_db = self.pending_gain_db_for_path(&path_owned);
                                let eff = if let Some(v) = self.lufs_override.get(&path_owned) { Some(*v) } else { base.map(|v| v + gain_db) };
                                let (rect2, resp2) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::click());
                                if let Some(db) = eff { ui.painter().rect_filled(rect2, 4.0, db_to_color(db)); }
                                let text = eff.map(|v| format!("{:.1}", v)).unwrap_or_else(|| "...".into());
                                let fid = TextStyle::Monospace.resolve(ui.style());
                                ui.painter().text(rect2.center(), egui::Align2::CENTER_CENTER, text, fid, Color32::WHITE);
                                if resp2.clicked() { clicked_to_load = true; }
                            });
                            // col 8: Gain (dB) editable
                            row.col(|ui| {
                                let old = self.pending_gain_db_for_path(&path_owned);
                                let mut g = old;
                                let resp = ui.add(
                                    egui::DragValue::new(&mut g)
                                        .range(-24.0..=24.0)
                                        .speed(0.1)
                                        .fixed_decimals(1)
                                        .suffix(" dB")
                                );
                                if resp.changed() {
                                    let new = Self::clamp_gain_db(g);
                                    let delta = new - old;
                                    if self.selected_multi.len() > 1 && self.selected_multi.contains(&row_idx) {
                                        let indices = self.selected_multi.clone();
                                        self.adjust_gain_for_indices(&indices, delta);
                                    } else {
                                        self.set_pending_gain_db_for_path(&path_owned, new);
                                        if self.playing_path.as_ref() == Some(&path_owned) { self.apply_effective_volume(); }
                                    }
                                    // schedule LUFS recompute (debounced)
                                    self.schedule_lufs_for_path(path_owned.clone());
                                }
                            });
                            // col 9: Wave thumbnail - clickable
                            row.col(|ui| {
                                let desired_w = ui.available_width().max(80.0);
                                let thumb_h = (desired_w * 0.22).clamp(text_height * 1.2, text_height * 4.0);
                                let (rect, painter) = ui.allocate_painter(egui::vec2(desired_w, thumb_h), Sense::click());
                                if row_idx == 0 { self.wave_row_h = thumb_h; }
                                if let Some(m) = meta.as_ref() { let w = rect.rect.width(); let h = rect.rect.height(); let n = m.thumb.len().max(1) as f32; 
                                        let gain_db = self.pending_gain_db_for_path(&path_owned);
                                        let scale = db_to_amp(gain_db);
                                        for (idx, &(mn0, mx0)) in m.thumb.iter().enumerate() {
                                        let mn = (mn0 * scale).clamp(-1.0, 1.0);
                                        let mx = (mx0 * scale).clamp(-1.0, 1.0);
                                        let x = rect.rect.left() + (idx as f32 / n) * w; let y0 = rect.rect.center().y - mx * (h*0.45); let y1 = rect.rect.center().y - mn * (h*0.45);
                                        let a = (mn.abs().max(mx.abs())).clamp(0.0,1.0);
                                        let col = amp_to_color(a);
                                        painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.0, col)); } }
                                if rect.clicked() { clicked_to_load = true; }
                            });
                            // col 10: Spacer (fills remainder so scrollbar stays at right edge)
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); });

                            // Row-level click handling (background/any non-interactive area)
                            let resp = row.response();
                            if resp.clicked() { clicked_to_load = true; }
                            if is_selected && self.scroll_to_selected { resp.scroll_to_me(Some(Align::Center)); }
                            if clicked_to_load {
                                // multi-select aware selection update (read modifiers from ctx to avoid UI borrow conflict)
                                let mods = ctx.input(|i| i.modifiers);
                                self.update_selection_on_click(row_idx, mods);
                                // load clicked row regardless of modifiers
                                self.select_and_load(row_idx, true);
                            } else if clicked_to_select { self.selected = Some(row_idx); self.scroll_to_selected = false; self.selected_multi.clear(); self.selected_multi.insert(row_idx); self.select_anchor = Some(row_idx); }
                        } else {
                            // filler row to extend frame
                            row.col(|_ui| {});
                            row.col(|_ui| {});
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Length
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Ch
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // SR
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Bits
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Level
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // LUFS
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Gain
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Wave
                            row.col(|ui| { let _ = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h*0.9), Sense::hover()); }); // Spacer
                        }
                    });
                });
                if sort_changed { self.apply_sort(); }
                if let Some(p) = to_open.as_ref() { self.open_or_activate_tab(p); }
                // moved to ui_list_view; do not draw here to avoid stray text
                // if self.files.is_empty() { ui.label("Select a folder to show list"); }
                }
            }
        });
        // When switching tabs, ensure the active tab's audio is loaded and loop state applied.
        if activate_path.is_none() {
            if let Some(pending) = self.pending_activate_path.take() { activate_path = Some(pending); }
        }
        if let Some(p) = activate_path {
            if !self.apply_dirty_tab_audio_with_mode(&p) {
                // Reload audio for the activated tab only; do not touch stored waveform
                match self.mode {
                    RateMode::Speed => {
                        let _ = prepare_for_speed(
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
                if let Some(idx) = self.active_tab {
                    if let Some(tab) = self.tabs.get(idx) {
                        self.apply_loop_mode_for_tab(tab);
                    }
                }
                // Update effective volume to include per-file gain for the activated tab
                self.apply_effective_volume();
            }
        }
        // List auto-scroll flag is cleared by list view when consumed.

        if let Some(tab_idx) = self.active_tab {
            self.queue_spectrogram_for_tab(tab_idx);
        }

        // Busy overlay (blocks input and shows loader)
        if self.processing.is_some()
            || self.export_state.is_some()
            || self.heavy_preview_rx.is_some()
            || self.editor_apply_state.is_some()
        {
            use egui::{Id, LayerId, Order};
            let screen = ctx.viewport_rect();
            // block input
            egui::Area::new("busy_block_input".into()).order(Order::Foreground).show(ctx, |ui| {
                let _ = ui.allocate_rect(screen, Sense::click_and_drag());
            });
            // darken background
            let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("busy_layer")));
            painter.rect_filled(screen, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 180));
            // centered box with spinner and text
            egui::Area::new("busy_center".into()).order(Order::Foreground).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0)).show(ctx, |ui| {
                egui::Frame::window(ui.style()).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Spinner::new());
                        let msg = if let Some(p) = &self.processing {
                            p.msg.as_str()
                        } else if let Some(st) = &self.editor_apply_state {
                            st.msg.as_str()
                        } else if let Some(st) = &self.export_state {
                            st.msg.as_str()
                        } else if let Some(t) = &self.heavy_preview_tool {
                            match t {
                                ToolKind::PitchShift => "Previewing PitchShift...",
                                ToolKind::TimeStretch => "Previewing TimeStretch...",
                                _ => "Previewing...",
                            }
                        } else {
                            "Working..."
                        };
                        ui.label(RichText::new(msg).strong());
                    });
                });
            });
        }
        ctx.request_repaint_after(Duration::from_millis(16));
        
        // Leave dirty editor confirmation
        if self.show_leave_prompt {
            egui::Window::new("Leave Editor?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0,0.0)).show(ctx, |ui| {
                ui.label("The waveform has been modified in memory. Leave this editor?");
                ui.horizontal(|ui| {
                    if ui.button("Leave").clicked() {
                        match self.leave_intent.take() {
                            Some(LeaveIntent::CloseTab(i)) => {
                                if i < self.tabs.len() {
                                    self.cache_dirty_tab_at(i);
                                    self.tabs.remove(i);
                                    if let Some(ai)=self.active_tab { if ai==i { self.active_tab=None; } else if ai>i { self.active_tab=Some(ai-1); } }
                                }
                                self.audio.stop();
                            }
                            Some(LeaveIntent::ToTab(i)) => {
                                if let Some(t) = self.tabs.get(i) { self.active_tab = Some(i); self.audio.stop(); self.pending_activate_path = Some(t.path.clone()); } self.rebuild_current_buffer_with_mode();
                            }
                            Some(LeaveIntent::ToList) => { self.active_tab=None; self.audio.stop(); self.audio.set_loop_enabled(false); }
                            None => {}
                        }
                        self.show_leave_prompt = false;
                    }
                    if ui.button("Cancel").clicked() { self.leave_intent=None; self.show_leave_prompt=false; }
                });
            });
        }
        
        // First save prompt window
        if self.show_first_save_prompt {
            egui::Window::new("First Save Option").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0,0.0)).show(ctx, |ui| {
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
                    if ui.button("Cancel").clicked() { self.show_first_save_prompt = false; }
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
                        ui.add(egui::DragValue::new(&mut self.batch_rename_start).range(0..=1_000_000));
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
                    for (i, src) in self.batch_rename_targets.iter().take(preview_count).enumerate() {
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























