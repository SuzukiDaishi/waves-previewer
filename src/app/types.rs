use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey { File, Folder, Length, Channels, SampleRate, Bits, Level, Lufs }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortDir { Asc, Desc, None }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RateMode { Speed, PitchShift, TimeStretch }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Waveform,
    Spectrogram,
    Mel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolKind {
    SeekSelect,
    LoopEdit,
    Trim,
    Fade,
    Gain,
    Normalize,
}

#[derive(Clone, Copy)]
pub struct ToolState {
    pub fade_in_ms: f32,
    pub fade_out_ms: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoopMode { Off, OnWhole, Marker }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkerKind { A, B }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LeaveIntent {
    ToList,
    ToTab(usize),
    CloseTab(usize),
}

pub struct EditorTab {
    pub path: PathBuf,
    pub display_name: String,
    pub waveform_minmax: Vec<(f32, f32)>,
    pub loop_enabled: bool,
    pub ch_samples: Vec<Vec<f32>>, // per-channel samples (device SR)
    pub samples_len: usize,        // length in samples
    pub view_offset: usize,        // first visible sample index
    pub samples_per_px: f32,       // time zoom: samples per pixel
    pub dirty: bool,               // unsaved edits exist
    pub ops: Vec<EditOp>,          // non-destructive operations (skeleton)
    // --- Editing state (MVP) ---
    pub selection: Option<(usize, usize)>,     // [start,end) in samples
    pub ab_loop: Option<(usize, usize)>,       // A/B markers in samples
    pub view_mode: ViewMode,                   // which visualization panel
    pub snap_zero_cross: bool,                 // enable zero-cross snapping
    pub drag_select_anchor: Option<usize>,     // transient during drag
    pub active_tool: ToolKind,                 // current editing tool
    pub tool_state: ToolState,                 // simple per-tool parameters
    pub loop_mode: LoopMode,                   // Off / On (whole) / Marker
    pub dragging_marker: Option<MarkerKind>,   // transient while dragging A/B
}

#[derive(Clone)]
pub struct FileMeta {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub duration_secs: Option<f32>,
    pub rms_db: Option<f32>,
    pub peak_db: Option<f32>,
    pub lufs_i: Option<f32>,
    pub thumb: Vec<(f32, f32)>,
}

pub struct ProcessingState {
    pub msg: String,
    pub path: PathBuf,
    pub rx: std::sync::mpsc::Receiver<ProcessingResult>,
}

pub struct ProcessingResult {
    pub path: PathBuf,
    pub samples: Vec<f32>,
    pub waveform: Vec<(f32, f32)>,
}

// --- Editing skeleton ---

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
    pub failed_paths: Vec<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SaveMode { Overwrite, NewFile }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy { Rename, Overwrite, Skip }

#[derive(Clone)]
pub struct ExportConfig {
    pub first_prompt: bool,
    pub save_mode: SaveMode,
    pub dest_folder: Option<PathBuf>,
    pub name_template: String, // tokens: {name}, {gain:+0.0}
    pub conflict: ConflictPolicy,
    pub backup_bak: bool,
}
