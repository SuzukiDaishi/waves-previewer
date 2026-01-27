use crate::audio::AudioBuffer;
use crate::markers::MarkerEntry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

pub type MediaId = u64;

#[derive(Clone, Debug)]
pub enum MediaStatus {
    Ok,
    DecodeFailed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaSource {
    File,
    Virtual,
    External,
}

#[derive(Clone, Debug)]
pub struct MediaItem {
    pub id: MediaId,
    pub path: PathBuf,
    pub display_name: String,
    pub display_folder: String,
    pub source: MediaSource,
    pub meta: Option<FileMeta>,
    pub pending_gain_db: f32,
    pub status: MediaStatus,
    pub transcript: Option<Transcript>,
    pub external: HashMap<String, String>,
    pub virtual_audio: Option<Arc<AudioBuffer>>,
}

#[derive(Clone, Debug)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Transcript {
    pub segments: Vec<TranscriptSegment>,
    pub full_text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    File,
    Folder,
    Transcript,
    Length,
    Channels,
    SampleRate,
    Bits,
    BitRate,
    Level,
    Lufs,
    Bpm,
    CreatedAt,
    ModifiedAt,
    External(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoScope {
    Editor,
    List,
}

#[derive(Clone, Debug)]
pub struct ListSelectionSnapshot {
    pub selected_path: Option<PathBuf>,
    pub selected_paths: Vec<PathBuf>,
    pub anchor_path: Option<PathBuf>,
    pub playing_path: Option<PathBuf>,
}

#[derive(Clone)]
pub struct ListUndoItem {
    pub item: MediaItem,
    pub item_index: usize,
    pub edited_cache: Option<CachedEdit>,
    pub lufs_override: Option<f32>,
    pub lufs_deadline: Option<Instant>,
}

#[derive(Clone)]
pub enum ListUndoActionKind {
    Remove { items: Vec<ListUndoItem> },
    Insert { items: Vec<ListUndoItem> },
    Update {
        before: Vec<ListUndoItem>,
        after: Vec<ListUndoItem>,
    },
}

#[derive(Clone)]
pub struct ListUndoAction {
    pub kind: ListUndoActionKind,
    pub before: ListSelectionSnapshot,
    pub after: ListSelectionSnapshot,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ListColumnConfig {
    pub edited: bool,
    pub file: bool,
    pub folder: bool,
    pub transcript: bool,
    pub external: bool,
    pub length: bool,
    pub channels: bool,
    pub sample_rate: bool,
    pub bits: bool,
    pub bit_rate: bool,
    pub peak: bool,
    pub lufs: bool,
    pub bpm: bool,
    pub created_at: bool,
    pub modified_at: bool,
    pub gain: bool,
    pub wave: bool,
}

impl Default for ListColumnConfig {
    fn default() -> Self {
        Self {
            edited: true,
            file: true,
            folder: true,
            transcript: false,
            external: true,
            length: true,
            channels: true,
            sample_rate: true,
            bits: true,
            bit_rate: true,
            peak: true,
            lufs: true,
            bpm: false,
            created_at: false,
            modified_at: false,
            gain: true,
            wave: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExternalKeyRule {
    FileName,
    Stem,
    Regex,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExternalRegexInput {
    FileName,
    Stem,
    Path,
    Dir,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RateMode {
    Speed,
    PitchShift,
    TimeStretch,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Waveform,
    Spectrogram,
    Mel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpectrogramScale {
    Linear,
    Log,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowFunction {
    Hann,
    BlackmanHarris,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpectrogramConfig {
    pub fft_size: usize,
    pub window: WindowFunction,
    pub overlap: f32,     // 0.0..0.95 (fraction)
    pub max_frames: usize,
    pub scale: SpectrogramScale,
    pub mel_scale: SpectrogramScale,
    pub db_floor: f32,    // negative dBFS
    pub max_freq_hz: f32, // 0 = Nyquist
    pub show_note_labels: bool,
}

impl Default for SpectrogramConfig {
    fn default() -> Self {
        Self {
            fft_size: 2048,
            window: WindowFunction::BlackmanHarris,
            overlap: 0.875,
            max_frames: 4096,
            scale: SpectrogramScale::Log,
            mel_scale: SpectrogramScale::Linear,
            db_floor: -120.0,
            max_freq_hz: 0.0,
            show_note_labels: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolKind {
    LoopEdit,
    Markers,
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
    pub loop_repeat: u32,
}

#[derive(Clone)]
pub struct PreviewOverlay {
    pub channels: Vec<Vec<f32>>,
    pub mixdown: Option<Vec<f32>>,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChannelViewMode {
    Mixdown,
    All,
    Custom,
}

#[derive(Clone, Debug)]
pub struct ChannelView {
    pub mode: ChannelViewMode,
    pub selected: Vec<usize>,
}

impl ChannelView {
    pub fn mixdown() -> Self {
        Self {
            mode: ChannelViewMode::Mixdown,
            selected: Vec::new(),
        }
    }

    pub fn visible_indices(&self, total: usize) -> Vec<usize> {
        match self.mode {
            ChannelViewMode::Mixdown => Vec::new(),
            ChannelViewMode::All => (0..total).collect(),
            ChannelViewMode::Custom => {
                let mut out: Vec<usize> = self
                    .selected
                    .iter()
                    .copied()
                    .filter(|&i| i < total)
                    .collect();
                out.sort_unstable();
                out.dedup();
                out
            }
        }
    }
}

pub struct EditorTab {
    pub path: PathBuf,
    pub display_name: String,
    pub waveform_minmax: Vec<(f32, f32)>,
    #[allow(dead_code)]
    pub loop_enabled: bool,
    pub loading: bool,
    pub ch_samples: Vec<Vec<f32>>, // per-channel samples (device SR)
    pub samples_len: usize,        // length in samples
    pub view_offset: usize,        // first visible sample index
    pub samples_per_px: f32,       // time zoom: samples per pixel
    pub last_wave_w: f32,          // last waveform width (for resize anchoring)
    pub dirty: bool,               // unsaved edits exist
    #[allow(dead_code)]
    pub ops: Vec<EditOp>, // non-destructive operations (skeleton)
    // --- Editing state (MVP) ---
    pub selection: Option<(usize, usize)>, // [start,end) in samples
    pub markers: Vec<MarkerEntry>,         // marker positions in samples (device SR)
    pub markers_saved: Vec<MarkerEntry>,   // last saved markers
    pub markers_committed: Vec<MarkerEntry>, // New field
    pub markers_applied: Vec<MarkerEntry>, // last applied markers
    pub markers_dirty: bool,
    // Deprecated: ab_loop (A/B) is no longer used as loop region; kept for transition
    pub ab_loop: Option<(usize, usize)>,
    // Playback loop region (independent from editing selection)
    pub loop_region: Option<(usize, usize)>,
    pub loop_region_applied: Option<(usize, usize)>,
    pub loop_region_committed: Option<(usize, usize)>,
    // Loop markers baseline (device SR) for dirty tracking
    pub loop_markers_saved: Option<(usize, usize)>,
    pub loop_markers_dirty: bool,
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
    pub show_waveform_overlay: bool,         // draw waveform overlay in Spec/Mel views
    pub channel_view: ChannelView,           // Mixdown / All / Custom
    pub bpm_enabled: bool,                   // grid toggle in editor
    pub bpm_value: f32,                      // current BPM for grid
    pub bpm_user_set: bool,                  // user-overridden BPM
    pub seek_hold: Option<SeekHoldState>,    // key repeat state for seek
    pub snap_zero_cross: bool,               // enable zero-cross snapping
    pub drag_select_anchor: Option<usize>,   // transient during drag
    pub active_tool: ToolKind,               // current editing tool
    pub tool_state: ToolState,               // simple per-tool parameters
    pub loop_mode: LoopMode,                 // Off / On (whole) / Marker
    pub dragging_marker: Option<MarkerKind>, // transient while dragging A/B
    // Preview audio state (non-destructive): tool-driven preview, cleared on tool/tab/view changes
    pub preview_audio_tool: Option<ToolKind>,
    pub active_tool_last: Option<ToolKind>,
    pub preview_offset_samples: Option<usize>,
    // Per-channel non-destructive preview overlay (green waveform)
    pub preview_overlay: Option<PreviewOverlay>,
    pub pending_loop_unwrap: Option<u32>,
    pub undo_stack: Vec<EditorUndoState>,
    pub undo_bytes: usize,
    pub redo_stack: Vec<EditorUndoState>,
    pub redo_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct FileMeta {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub bit_rate_bps: Option<u32>,
    pub duration_secs: Option<f32>,
    #[allow(dead_code)]
    pub rms_db: Option<f32>,
    pub peak_db: Option<f32>,
    pub lufs_i: Option<f32>,
    pub bpm: Option<f32>,
    pub created_at: Option<SystemTime>,
    pub modified_at: Option<SystemTime>,
    pub thumb: Vec<(f32, f32)>,
    pub decode_error: Option<String>,
}

#[derive(Clone)]
pub struct SpectrogramData {
    pub frames: usize,
    pub bins: usize,
    pub frame_step: usize,
    pub sample_rate: u32,
    pub values_db: Vec<f32>,
}

pub struct SpectrogramTile {
    pub path: PathBuf,
    pub channel_index: usize,
    pub channel_count: usize,
    pub frames: usize,
    pub bins: usize,
    pub frame_step: usize,
    pub sample_rate: u32,
    pub start_frame: usize,
    pub values_db: Vec<f32>,
}

pub enum SpectrogramJobMsg {
    Tile(SpectrogramTile),
    Done(PathBuf),
}

pub struct SpectrogramProgress {
    pub done_tiles: usize,
    pub total_tiles: usize,
    pub started_at: std::time::Instant,
}

pub enum ScanMessage {
    Batch(Vec<PathBuf>),
    Done,
}

pub struct ProcessingState {
    pub msg: String,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub autoplay_when_ready: bool,
    pub started_at: std::time::Instant,
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
    pub undo: Option<EditorUndoState>,
}

pub struct EditorApplyResult {
    pub tab_idx: usize,
    pub samples: Vec<f32>,
    pub channels: Vec<Vec<f32>>,
}

pub struct EditorDecodeResult {
    pub path: PathBuf,
    pub channels: Vec<Vec<f32>>,
    pub is_final: bool,
    pub job_id: u64,
    pub error: Option<String>,
}

pub struct EditorDecodeState {
    pub path: PathBuf,
    pub started_at: Instant,
    pub rx: std::sync::mpsc::Receiver<EditorDecodeResult>,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub job_id: u64,
    pub partial_ready: bool,
}

#[derive(Clone)]
pub struct EditorUndoState {
    pub ch_samples: Vec<Vec<f32>>,
    pub samples_len: usize,
    pub view_offset: usize,
    pub samples_per_px: f32,
    pub selection: Option<(usize, usize)>,
    pub ab_loop: Option<(usize, usize)>,
    pub loop_region: Option<(usize, usize)>,
    pub loop_region_committed: Option<(usize, usize)>,
    pub trim_range: Option<(usize, usize)>,
    pub loop_xfade_samples: usize,
    pub loop_xfade_shape: LoopXfadeShape,
    pub fade_in_range: Option<(usize, usize)>,
    pub fade_out_range: Option<(usize, usize)>,
    pub fade_in_shape: FadeShape,
    pub fade_out_shape: FadeShape,
    pub loop_mode: LoopMode,
    pub snap_zero_cross: bool,
    pub tool_state: ToolState,
    pub active_tool: ToolKind,
    pub show_waveform_overlay: bool,
    pub dirty: bool,
    pub approx_bytes: usize,
    pub markers: Vec<MarkerEntry>,
    pub markers_committed: Vec<MarkerEntry>,
    pub markers_applied: Vec<MarkerEntry>,
    pub loop_region_applied: Option<(usize, usize)>,
}

#[derive(Clone)]
pub struct CachedEdit {
    pub ch_samples: Vec<Vec<f32>>,
    pub samples_len: usize,
    pub waveform_minmax: Vec<(f32, f32)>,
    pub dirty: bool,
    pub loop_region: Option<(usize, usize)>,
    pub loop_region_committed: Option<(usize, usize)>,
    pub loop_region_applied: Option<(usize, usize)>,
    pub loop_markers_saved: Option<(usize, usize)>,
    pub loop_markers_dirty: bool,
    pub markers: Vec<MarkerEntry>,
    pub markers_saved: Vec<MarkerEntry>,
    pub markers_committed: Vec<MarkerEntry>,
    pub markers_applied: Vec<MarkerEntry>,
    pub markers_dirty: bool,
    pub trim_range: Option<(usize, usize)>,
    pub loop_xfade_samples: usize,
    pub loop_xfade_shape: LoopXfadeShape,
    pub fade_in_range: Option<(usize, usize)>,
    pub fade_out_range: Option<(usize, usize)>,
    pub fade_in_shape: FadeShape,
    pub fade_out_shape: FadeShape,
    pub loop_mode: LoopMode,
    pub bpm_enabled: bool,
    pub bpm_value: f32,
    pub bpm_user_set: bool,
    pub snap_zero_cross: bool,
    pub tool_state: ToolState,
    pub active_tool: ToolKind,
    pub show_waveform_overlay: bool,
}

pub struct SeekHoldState {
    pub dir: i32,
    pub started_at: Instant,
    pub last_step_at: Instant,
}

#[derive(Clone)]
pub struct ClipboardItem {
    pub display_name: String,
    pub source_path: Option<PathBuf>,
    pub audio: Option<Arc<AudioBuffer>>,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
}

#[derive(Clone)]
pub struct ClipboardPayload {
    pub items: Vec<ClipboardItem>,
    pub created_at: Instant,
}

pub struct ListPreviewResult {
    pub path: PathBuf,
    pub channels: Vec<Vec<f32>>,
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

pub struct CsvExportState {
    pub path: PathBuf,
    pub ids: Vec<MediaId>,
    pub cols: ListColumnConfig,
    pub external_cols: Vec<String>,
    pub total: usize,
    pub done: usize,
    pub pending: HashSet<PathBuf>,
    pub needs_peak: bool,
    pub needs_lufs: bool,
    pub started_at: Instant,
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
    pub open_project: Option<PathBuf>,
    pub open_folder: Option<PathBuf>,
    pub open_files: Vec<PathBuf>,
    pub open_first: bool,
    pub open_view_mode: Option<ViewMode>,
    pub open_waveform_overlay: Option<bool>,
    pub screenshot_path: Option<PathBuf>,
    pub screenshot_delay_frames: u32,
    pub exit_after_screenshot: bool,
    pub dummy_list_count: Option<usize>,
    pub external_path: Option<PathBuf>,
    pub external_dummy_rows: Option<usize>,
    pub external_dummy_cols: usize,
    pub external_dummy_path: Option<PathBuf>,
    pub external_sheet: Option<String>,
    pub external_has_header: Option<bool>,
    pub external_header_row: Option<usize>,
    pub external_data_row: Option<usize>,
    pub external_key_rule: Option<ExternalKeyRule>,
    pub external_key_input: Option<ExternalRegexInput>,
    pub external_key_regex: Option<String>,
    pub external_key_replace: Option<String>,
    pub external_scope_regex: Option<String>,
    pub external_show_unmatched: bool,
    pub external_show_dialog: bool,
    pub debug_summary_path: Option<PathBuf>,
    pub debug_summary_delay_frames: u32,
    pub debug: DebugConfig,
    pub mcp_stdio: bool,
    pub mcp_allow_paths: Vec<PathBuf>,
    pub mcp_allow_write: bool,
    pub mcp_allow_export: bool,
    pub mcp_read_only: bool,
    pub mcp_http_addr: Option<String>,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            open_project: None,
            open_folder: None,
            open_files: Vec::new(),
            open_first: false,
            open_view_mode: None,
            open_waveform_overlay: None,
            screenshot_path: None,
            screenshot_delay_frames: 5,
            exit_after_screenshot: false,
            dummy_list_count: None,
            external_path: None,
            external_dummy_rows: None,
            external_dummy_cols: 6,
            external_dummy_path: None,
            external_sheet: None,
            external_has_header: None,
            external_header_row: None,
            external_data_row: None,
            external_key_rule: None,
            external_key_input: None,
            external_key_regex: None,
            external_key_replace: None,
            external_scope_regex: None,
            external_show_unmatched: false,
            external_show_dialog: false,
            debug_summary_path: None,
            debug_summary_delay_frames: 10,
            debug: DebugConfig::default(),
            mcp_stdio: false,
            mcp_allow_paths: Vec::new(),
            mcp_allow_write: false,
            mcp_allow_export: false,
            mcp_read_only: true,
            mcp_http_addr: None,
        }
    }
}

#[derive(Clone)]
pub struct StartupState {
    pub cfg: StartupConfig,
    pub open_first_pending: bool,
    pub screenshot_pending: bool,
    pub screenshot_frames_left: u32,
    pub debug_summary_pending: bool,
    pub debug_summary_frames_left: u32,
    pub view_mode_applied: bool,
    pub waveform_overlay_applied: bool,
}

impl StartupState {
    pub fn new(cfg: StartupConfig) -> Self {
        let screenshot_pending = cfg.screenshot_path.is_some();
        let screenshot_frames_left = cfg.screenshot_delay_frames;
        let debug_summary_pending = cfg.debug_summary_path.is_some();
        let debug_summary_frames_left = cfg.debug_summary_delay_frames;
        Self {
            open_first_pending: cfg.open_first,
            screenshot_pending,
            screenshot_frames_left,
            debug_summary_pending,
            debug_summary_frames_left,
            view_mode_applied: false,
            waveform_overlay_applied: false,
            cfg,
        }
    }
}
#[derive(Clone)]
pub struct DebugConfig {
    pub enabled: bool,
    pub log_path: Option<PathBuf>,
    pub auto_run: bool,
    pub auto_run_editor: bool,
    pub auto_run_pitch_shift_semitones: Option<f32>,
    pub auto_run_time_stretch_rate: Option<f32>,
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
            auto_run_editor: false,
            auto_run_pitch_shift_semitones: None,
            auto_run_time_stretch_rate: None,
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
    pub input_trace: VecDeque<String>,
    pub input_trace_enabled: bool,
    pub input_trace_max: usize,
    pub event_trace: VecDeque<String>,
    pub event_trace_enabled: bool,
    pub event_trace_max: usize,
    pub last_copy_at: Option<Instant>,
    pub last_copy_count: usize,
    pub last_paste_at: Option<Instant>,
    pub last_paste_count: usize,
    pub last_paste_source: Option<String>,
    pub last_hotkey: Option<String>,
    pub last_hotkey_at: Option<Instant>,
    pub last_pointer_over_list: bool,
    pub last_raw_focused: bool,
    pub last_events_len: usize,
    pub last_ctrl_down: bool,
    pub last_key_c_pressed: bool,
    pub last_key_v_pressed: bool,
    pub last_key_c_down: bool,
    pub last_key_v_down: bool,
    pub auto: Option<DebugAutomation>,
    pub check_counter: u32,
    pub overlay_trace: bool,
    pub dummy_list_count: u32,
    pub started_at: Instant,
}

impl DebugState {
    pub fn new(cfg: DebugConfig) -> Self {
        let show = cfg.enabled;
        let check_counter = cfg.check_interval_frames.max(1);
        Self {
            cfg,
            show_window: show,
            logs: VecDeque::new(),
            input_trace: VecDeque::new(),
            input_trace_enabled: false,
            input_trace_max: 200,
            event_trace: VecDeque::new(),
            event_trace_enabled: false,
            event_trace_max: 200,
            last_copy_at: None,
            last_copy_count: 0,
            last_paste_at: None,
            last_paste_count: 0,
            last_paste_source: None,
            last_hotkey: None,
            last_hotkey_at: None,
            last_pointer_over_list: false,
            last_raw_focused: true,
            last_events_len: 0,
            last_ctrl_down: false,
            last_key_c_pressed: false,
            last_key_v_pressed: false,
            last_key_c_down: false,
            last_key_v_down: false,
            auto: None,
            check_counter,
            overlay_trace: false,
            dummy_list_count: 300000,
            started_at: Instant::now(),
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
    SetActiveTool(ToolKind),
    SetSelection { start_frac: f32, end_frac: f32 },
    SetTrimRange { start_frac: f32, end_frac: f32 },
    SetLoopRegion { start_frac: f32, end_frac: f32 },
    SetLoopMode(LoopMode),
    SetLoopXfade {
        ms: f32,
        shape: LoopXfadeShape,
    },
    AddMarker { frac: f32 },
    ClearMarkers,
    WriteMarkers,
    WriteLoopMarkers,
    ApplyTrim,
    ApplyLoopXfade,
    ApplyFadeIn {
        ms: f32,
        shape: FadeShape,
    },
    ApplyFadeOut {
        ms: f32,
        shape: FadeShape,
    },
    ApplyFadeRange {
        in_ms: f32,
        out_ms: f32,
        shape: FadeShape,
    },
    ApplyGain { db: f32 },
    ApplyNormalize { db: f32 },
    ApplyReverse,
    ApplyPitchShift(f32),
    ApplyTimeStretch(f32),
    SetViewMode(ViewMode),
    SetWaveformOverlay(bool),
    ToggleMode,
    PlayPause,
    SelectNext,
    PreviewPitchShift(f32),
    PreviewTimeStretch(f32),
    DumpSummaryAuto,
    Exit,
}
