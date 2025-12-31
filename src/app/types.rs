use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    File,
    Folder,
    Length,
    Channels,
    SampleRate,
    Bits,
    Level,
    Lufs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RateMode {
    Speed,
    PitchShift,
    TimeStretch,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Waveform,
    Spectrogram,
    Mel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolKind {
    LoopEdit,
    Trim,
    Fade,
    PitchShift,
    TimeStretch,
    Gain,
    Normalize,
    Reverse,
}

#[derive(Clone, Copy)]
pub struct ToolState {
    pub fade_in_ms: f32,
    pub fade_out_ms: f32,
    pub gain_db: f32,
    pub normalize_target_db: f32,
    pub pitch_semitones: f32,
    pub stretch_rate: f32,
}

#[derive(Clone)]
pub struct PreviewOverlay {
    pub channels: Vec<Vec<f32>>,
    #[allow(dead_code)]
    pub source_tool: ToolKind,
    pub timeline_len: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoopMode {
    Off,
    OnWhole,
    Marker,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum MarkerKind {
    A,
    B,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoopXfadeShape {
    Linear,
    EqualPower,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum FadeShape {
    Linear,
    EqualPower,
    Cosine,
    SCurve,
    Quadratic,
    Cubic,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LeaveIntent {
    ToList,
    ToTab(usize),
    CloseTab(usize),
}

pub struct EditorTab {
    pub path: PathBuf,
    pub display_name: String,
    pub waveform_minmax: Vec<(f32, f32)>,
    #[allow(dead_code)]
    pub loop_enabled: bool,
    pub ch_samples: Vec<Vec<f32>>, // per-channel samples (device SR)
    pub samples_len: usize,        // length in samples
    pub view_offset: usize,        // first visible sample index
    pub samples_per_px: f32,       // time zoom: samples per pixel
    pub dirty: bool,               // unsaved edits exist
    #[allow(dead_code)]
    pub ops: Vec<EditOp>,          // non-destructive operations (skeleton)
    // --- Editing state (MVP) ---
    pub selection: Option<(usize, usize)>, // [start,end) in samples
    // Deprecated: ab_loop (A/B) is no longer used as loop region; kept for transition
    pub ab_loop: Option<(usize, usize)>,
    // Playback loop region (independent from editing selection)
    pub loop_region: Option<(usize, usize)>,
    // Trim-specific A/B range (independent from loop)
    pub trim_range: Option<(usize, usize)>,
    pub loop_xfade_samples: usize, // crossfade length in samples (device SR)
    pub loop_xfade_shape: LoopXfadeShape, // blend shape
    // Fade tool ranges and shapes
    pub fade_in_range: Option<(usize, usize)>,
    pub fade_out_range: Option<(usize, usize)>,
    pub fade_in_shape: FadeShape,
    pub fade_out_shape: FadeShape,
    pub view_mode: ViewMode,                 // which visualization panel
    pub snap_zero_cross: bool,               // enable zero-cross snapping
    pub drag_select_anchor: Option<usize>,   // transient during drag
    pub active_tool: ToolKind,               // current editing tool
    pub tool_state: ToolState,               // simple per-tool parameters
    pub loop_mode: LoopMode,                 // Off / On (whole) / Marker
    pub dragging_marker: Option<MarkerKind>, // transient while dragging A/B
    // Preview audio state (non-destructive): which tool is driving runtime preview
    pub preview_audio_tool: Option<ToolKind>,
    pub active_tool_last: Option<ToolKind>,
    pub preview_offset_samples: Option<usize>,
    // Per-channel non-destructive preview overlay (green waveform)
    pub preview_overlay: Option<PreviewOverlay>,
}

#[derive(Clone)]
pub struct FileMeta {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub duration_secs: Option<f32>,
    #[allow(dead_code)]
    pub rms_db: Option<f32>,
    pub peak_db: Option<f32>,
    pub lufs_i: Option<f32>,
    pub thumb: Vec<(f32, f32)>,
}

pub struct SpectrogramData {
    pub frames: usize,
    pub bins: usize,
    pub frame_step: usize,
    pub sample_rate: u32,
    pub values_db: Vec<f32>,
}


pub enum ScanMessage {
    Batch(Vec<PathBuf>),
    Done,
}

pub struct ProcessingState {
    pub msg: String,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub rx: std::sync::mpsc::Receiver<ProcessingResult>,
}

pub struct ProcessingResult {
    pub path: PathBuf,
    pub samples: Vec<f32>,
    pub waveform: Vec<(f32, f32)>,
    #[allow(dead_code)]
    pub channels: Vec<Vec<f32>>,
}

pub struct EditorApplyState {
    pub msg: String,
    pub rx: std::sync::mpsc::Receiver<EditorApplyResult>,
    #[allow(dead_code)]
    pub tab_idx: usize,
}

pub struct EditorApplyResult {
    pub tab_idx: usize,
    pub samples: Vec<f32>,
    pub channels: Vec<Vec<f32>>,
}

pub struct ListPreviewResult {
    pub path: PathBuf,
    pub samples: Vec<f32>,
    pub job_id: u64,
}

// --- Editing skeleton ---

#[allow(dead_code)]
pub enum EditOp {
    GainDb(f32),
    Trim { start: usize, end: usize },
    FadeIn { samples: usize },
    FadeOut { samples: usize },
}

pub struct ExportState {
    pub msg: String,
    pub rx: std::sync::mpsc::Receiver<ExportResult>,
}

pub struct ExportResult {
    pub ok: usize,
    pub failed: usize,
    pub success_paths: Vec<PathBuf>,
    #[allow(dead_code)]
    pub failed_paths: Vec<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    Overwrite,
    NewFile,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Rename,
    Overwrite,
    Skip,
}

#[derive(Clone)]
pub struct ExportConfig {
    pub first_prompt: bool,
    pub save_mode: SaveMode,
    pub dest_folder: Option<PathBuf>,
    pub name_template: String, // tokens: {name}, {gain:+0.0}
    pub conflict: ConflictPolicy,
    pub backup_bak: bool,
}

#[derive(Clone)]
pub struct StartupConfig {
    pub open_folder: Option<PathBuf>,
    pub open_files: Vec<PathBuf>,
    pub open_first: bool,
    pub screenshot_path: Option<PathBuf>,
    pub screenshot_delay_frames: u32,
    pub exit_after_screenshot: bool,
    pub dummy_list_count: Option<usize>,
    pub debug: DebugConfig,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            open_folder: None,
            open_files: Vec::new(),
            open_first: false,
            screenshot_path: None,
            screenshot_delay_frames: 5,
            exit_after_screenshot: false,
            dummy_list_count: None,
            debug: DebugConfig::default(),
        }
    }
}

#[derive(Clone)]
pub struct StartupState {
    pub cfg: StartupConfig,
    pub open_first_pending: bool,
    pub screenshot_pending: bool,
    pub screenshot_frames_left: u32,
}

impl StartupState {
    pub fn new(cfg: StartupConfig) -> Self {
        let screenshot_pending = cfg.screenshot_path.is_some();
        let screenshot_frames_left = cfg.screenshot_delay_frames;
        Self {
            open_first_pending: cfg.open_first,
            screenshot_pending,
            screenshot_frames_left,
            cfg,
        }
    }
}
#[derive(Clone)]
pub struct DebugConfig {
    pub enabled: bool,
    pub log_path: Option<PathBuf>,
    pub auto_run: bool,
    pub auto_run_delay_frames: u32,
    pub auto_run_exit: bool,
    pub check_interval_frames: u32,
}

impl Default for DebugConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            log_path: None,
            auto_run: false,
            auto_run_delay_frames: 8,
            auto_run_exit: true,
            check_interval_frames: 30,
        }
    }
}

pub struct DebugState {
    pub cfg: DebugConfig,
    pub show_window: bool,
    pub logs: VecDeque<String>,
    pub auto: Option<DebugAutomation>,
    pub check_counter: u32,
    pub overlay_trace: bool,
    pub dummy_list_count: u32,
}

impl DebugState {
    pub fn new(cfg: DebugConfig) -> Self {
        let show = cfg.enabled;
        let check_counter = cfg.check_interval_frames.max(1);
        Self {
            cfg,
            show_window: show,
            logs: VecDeque::new(),
            auto: None,
            check_counter,
            overlay_trace: false,
            dummy_list_count: 300000,
        }
    }
}

pub struct DebugAutomation {
    pub steps: VecDeque<DebugStep>,
}

pub struct DebugStep {
    pub wait_frames: u32,
    pub action: DebugAction,
}

#[allow(dead_code)]
pub enum DebugAction {
    OpenFirst,
    ScreenshotAuto,
    ScreenshotPath(PathBuf),
    ToggleMode,
    PlayPause,
    SelectNext,
    DumpSummaryAuto,
    Exit,
}
