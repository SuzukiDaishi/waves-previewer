use crate::app::render::waveform_pyramid::{Peak, WaveformPyramidSet};
use crate::audio::AudioBuffer;
use crate::markers::MarkerEntry;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleValueKind {
    Unknown,
    Int,
    Float,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VirtualSourceRef {
    FilePath(PathBuf),
    VirtualPath(PathBuf),
    Sidecar(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VirtualOp {
    Trim { start: usize, end: usize },
}

#[derive(Clone, Debug)]
pub struct VirtualState {
    pub source: VirtualSourceRef,
    pub op_chain: Vec<VirtualOp>,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
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
    pub transcript_language: Option<String>,
    pub external: HashMap<String, String>,
    pub virtual_audio: Option<Arc<AudioBuffer>>,
    pub virtual_state: Option<VirtualState>,
}

#[derive(Clone, Debug)]
pub struct ExternalSource {
    pub path: PathBuf,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub sheet_names: Vec<String>,
    pub sheet_name: Option<String>,
    pub has_header: bool,
    pub header_row: Option<usize>,
    pub data_row: Option<usize>,
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
    Type,
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
    EffectGraph,
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
    pub sample_rate_override: Option<u32>,
    pub bit_depth_override: Option<crate::wave::WavBitDepth>,
    pub format_override: Option<String>,
}

#[derive(Clone)]
pub enum ListUndoActionKind {
    Remove {
        items: Vec<ListUndoItem>,
    },
    Insert {
        items: Vec<ListUndoItem>,
    },
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
    pub cover_art: bool,
    pub type_badge: bool,
    pub file: bool,
    pub folder: bool,
    pub transcript: bool,
    pub transcript_language: bool,
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
            cover_art: false,
            type_badge: false,
            file: true,
            folder: true,
            transcript: false,
            transcript_language: false,
            external: true,
            length: true,
            channels: true,
            sample_rate: true,
            bits: true,
            bit_rate: false,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WorkspaceView {
    #[default]
    List,
    Editor,
    EffectGraph,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemBgMode {
    Standard,
    Dbfs,
    Lufs,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SrcQuality {
    Fast,
    Good,
    Best,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Waveform,
    Spectrogram,
    Log,
    Mel,
    Tempogram,
    Chromagram,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EditorPrimaryView {
    #[default]
    Wave,
    Spec,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EditorSpecSubView {
    #[default]
    Spec,
    Log,
    Mel,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EditorOtherSubView {
    F0,
    #[default]
    Tempogram,
    Chromagram,
}

impl EditorPrimaryView {
    pub fn from_mode(mode: ViewMode) -> Self {
        match mode {
            ViewMode::Waveform => Self::Wave,
            ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => Self::Spec,
            ViewMode::Tempogram | ViewMode::Chromagram => Self::Other,
        }
    }

    pub fn default_mode(self) -> ViewMode {
        match self {
            Self::Wave => ViewMode::Waveform,
            Self::Spec => ViewMode::Spectrogram,
            Self::Other => ViewMode::Tempogram,
        }
    }
}

impl EditorSpecSubView {
    pub fn from_mode(mode: ViewMode) -> Self {
        match mode {
            ViewMode::Log => Self::Log,
            ViewMode::Mel => Self::Mel,
            _ => Self::Spec,
        }
    }

    pub fn to_mode(self) -> ViewMode {
        match self {
            Self::Spec => ViewMode::Spectrogram,
            Self::Log => ViewMode::Log,
            Self::Mel => ViewMode::Mel,
        }
    }
}

impl EditorOtherSubView {
    pub fn from_mode(mode: ViewMode) -> Self {
        match mode {
            ViewMode::Chromagram => Self::Chromagram,
            ViewMode::Tempogram => Self::Tempogram,
            _ => Self::Tempogram,
        }
    }

    pub fn to_mode(self) -> ViewMode {
        match self {
            Self::F0 => ViewMode::Tempogram,
            Self::Tempogram => ViewMode::Tempogram,
            Self::Chromagram => ViewMode::Chromagram,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SpectrogramScale {
    Linear,
    Log,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectGraphSpectrumMode {
    Linear,
    Log,
    Mel,
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
    pub hop_size: usize, // hop size in samples
    pub overlap: f32,    // 0.0..0.95 (fraction)
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
            hop_size: 256,
            overlap: 0.875,
            max_frames: 4096,
            scale: SpectrogramScale::Linear,
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
    Loudness,
    Reverse,
    MusicAnalyze,
    PluginFx,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StemGainsDb {
    pub bass: f32,
    pub drums: f32,
    pub other: f32,
    pub vocals: f32,
}

#[derive(Clone, Debug, Default)]
pub struct MusicStemSet {
    pub sample_rate: u32,
    pub bass: Vec<Vec<f32>>,
    pub drums: Vec<Vec<f32>>,
    pub other: Vec<Vec<f32>>,
    pub vocals: Vec<Vec<f32>>,
}

impl MusicStemSet {
    pub fn len_samples(&self) -> usize {
        self.bass
            .first()
            .or_else(|| self.drums.first())
            .or_else(|| self.other.first())
            .or_else(|| self.vocals.first())
            .map(|c| c.len())
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MusicAnalysisResult {
    pub beats: Vec<usize>,
    pub downbeats: Vec<usize>,
    pub sections: Vec<(usize, String)>,
    pub estimated_bpm: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MusicAnalysisSourceKind {
    #[default]
    StemsDir,
    AutoDemucs,
}

#[derive(Clone, Debug)]
pub struct MusicAnalysisDraft {
    pub result: Option<MusicAnalysisResult>,
    pub show_beat: bool,
    pub show_downbeat: bool,
    pub show_section: bool,
    pub preview_click_beat: bool,
    pub preview_click_downbeat: bool,
    pub preview_cue_section: bool,
    pub preview_gains_db: StemGainsDb,
    pub preview_selection_only: bool,
    pub analysis_inflight: bool,
    pub stems_dir_override: Option<PathBuf>,
    pub last_error: Option<String>,
    pub preview_active: bool,
    pub stems_audio: Option<Arc<MusicStemSet>>,
    pub preview_inflight: bool,
    pub preview_generation: u64,
    pub preview_error: Option<String>,
    pub analysis_source_len: usize,
    pub analysis_source_kind: MusicAnalysisSourceKind,
    pub provisional_markers: Vec<MarkerEntry>,
    pub preview_peak_abs: f32,
    pub preview_clip_applied: bool,
    pub analysis_process_message: String,
}

impl Default for MusicAnalysisDraft {
    fn default() -> Self {
        Self {
            result: None,
            show_beat: true,
            show_downbeat: true,
            show_section: true,
            preview_click_beat: false,
            preview_click_downbeat: false,
            preview_cue_section: false,
            preview_gains_db: StemGainsDb::default(),
            preview_selection_only: false,
            analysis_inflight: false,
            stems_dir_override: None,
            last_error: None,
            preview_active: false,
            stems_audio: None,
            preview_inflight: false,
            preview_generation: 0,
            preview_error: None,
            analysis_source_len: 0,
            analysis_source_kind: MusicAnalysisSourceKind::StemsDir,
            provisional_markers: Vec::new(),
            preview_peak_abs: 0.0,
            preview_clip_applied: false,
            analysis_process_message: String::new(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct ToolState {
    pub fade_in_ms: f32,
    pub fade_out_ms: f32,
    pub gain_db: f32,
    pub normalize_target_db: f32,
    pub loudness_target_lufs: f32,
    pub pitch_semitones: f32,
    pub stretch_rate: f32,
    pub loop_repeat: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginParamUiState {
    pub id: String,
    pub name: String,
    pub normalized: f32,
    pub default_normalized: f32,
    pub min: f32,
    pub max: f32,
    pub unit: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct EffectGraphPluginParamState {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub normalized: f32,
    #[serde(default)]
    pub default_normalized: f32,
    #[serde(default)]
    pub min: f32,
    #[serde(default)]
    pub max: f32,
    #[serde(default)]
    pub unit: String,
}

impl EffectGraphPluginParamState {
    pub fn from_ui(param: &PluginParamUiState) -> Self {
        Self {
            id: param.id.clone(),
            name: param.name.clone(),
            normalized: param.normalized.clamp(0.0, 1.0),
            default_normalized: param.default_normalized.clamp(0.0, 1.0),
            min: param.min,
            max: param.max,
            unit: param.unit.clone(),
        }
    }

    pub fn to_worker_value(&self) -> crate::plugin::PluginParamValue {
        crate::plugin::PluginParamValue {
            id: self.id.clone(),
            normalized: self.normalized.clamp(0.0, 1.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectGraphPluginNodeConfig {
    #[serde(default)]
    pub plugin_key: Option<String>,
    #[serde(default)]
    pub plugin_name: String,
    #[serde(default = "default_effect_graph_plugin_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub bypass: bool,
    #[serde(default)]
    pub filter: String,
    #[serde(default)]
    pub params: Vec<EffectGraphPluginParamState>,
    #[serde(default)]
    pub state_blob_b64: Option<String>,
}

impl Default for EffectGraphPluginNodeConfig {
    fn default() -> Self {
        Self {
            plugin_key: None,
            plugin_name: String::new(),
            enabled: true,
            bypass: false,
            filter: String::new(),
            params: Vec::new(),
            state_blob_b64: None,
        }
    }
}

fn default_effect_graph_plugin_enabled() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginFxDraft {
    pub plugin_key: Option<String>,
    pub plugin_name: String,
    pub backend: Option<crate::plugin::PluginHostBackend>,
    pub gui_capabilities: crate::plugin::GuiCapabilities,
    pub gui_status: crate::plugin::GuiSessionStatus,
    pub enabled: bool,
    pub bypass: bool,
    pub filter: String,
    pub params: Vec<PluginParamUiState>,
    pub state_blob: Option<Vec<u8>>,
    pub last_error: Option<String>,
    pub last_backend_log: Option<String>,
}

#[derive(Clone, Debug)]
pub enum PluginGuiCommand {
    SyncNow,
    Close,
}

#[derive(Clone, Debug)]
pub enum PluginGuiEvent {
    Opened {
        session_id: u64,
        backend: crate::plugin::PluginHostBackend,
        capabilities: crate::plugin::GuiCapabilities,
        params: Vec<PluginParamUiState>,
        state_blob: Option<Vec<u8>>,
        backend_note: Option<String>,
    },
    Snapshot {
        session_id: u64,
        params: Vec<crate::plugin::PluginParamValue>,
        state_blob: Option<Vec<u8>>,
        backend: crate::plugin::PluginHostBackend,
        closed: bool,
        backend_note: Option<String>,
    },
    Closed {
        session_id: u64,
        state_blob: Option<Vec<u8>>,
        backend: crate::plugin::PluginHostBackend,
        backend_note: Option<String>,
    },
    Error {
        session_id: u64,
        message: String,
    },
}

pub struct PluginGuiSessionState {
    pub tab_path: PathBuf,
    pub session_id: u64,
    pub started_at: Instant,
    pub cmd_tx: std::sync::mpsc::Sender<PluginGuiCommand>,
    pub rx: std::sync::mpsc::Receiver<PluginGuiEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginCatalogEntry {
    pub key: String,
    pub name: String,
    pub path: PathBuf,
    pub format: crate::plugin::PluginFormat,
}

pub struct PluginScanResult {
    pub job_id: u64,
    pub plugins: Vec<PluginCatalogEntry>,
    pub error: Option<String>,
}

pub struct PluginProbeResult {
    pub job_id: u64,
    pub plugin_key: String,
    pub plugin_name: String,
    pub params: Vec<PluginParamUiState>,
    pub state_blob: Option<Vec<u8>>,
    pub backend: crate::plugin::PluginHostBackend,
    pub capabilities: crate::plugin::GuiCapabilities,
    pub backend_note: Option<String>,
    pub error: Option<String>,
}

pub struct PluginProcessResult {
    pub job_id: u64,
    pub tab_idx: usize,
    pub is_apply: bool,
    pub channels: Vec<Vec<f32>>,
    pub state_blob: Option<Vec<u8>>,
    pub backend: crate::plugin::PluginHostBackend,
    pub backend_note: Option<String>,
    pub error: Option<String>,
}

pub struct PluginScanState {
    pub job_id: u64,
    pub started_at: Instant,
    pub rx: std::sync::mpsc::Receiver<PluginScanResult>,
}

pub struct PluginProbeState {
    pub job_id: u64,
    pub tab_path: PathBuf,
    pub started_at: Instant,
    pub rx: std::sync::mpsc::Receiver<PluginProbeResult>,
}

pub struct PluginProcessState {
    pub job_id: u64,
    pub started_at: Instant,
    pub tab_idx: usize,
    pub is_apply: bool,
    pub rx: std::sync::mpsc::Receiver<PluginProcessResult>,
    pub undo: Option<EditorUndoState>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PreviewOverlayDetailKind {
    FullSample,
    OverviewOnly,
}

#[derive(Clone)]
pub struct PreviewOverlay {
    pub channels: Vec<Vec<f32>>,
    pub mixdown: Option<Vec<f32>>,
    pub overview_channels: Vec<Vec<(f32, f32)>>,
    pub overview_mixdown: Option<Vec<(f32, f32)>>,
    #[allow(dead_code)]
    pub source_tool: ToolKind,
    pub timeline_len: usize,
    pub detail_kind: PreviewOverlayDetailKind,
}

impl PreviewOverlay {
    pub fn is_full_sample(&self) -> bool {
        self.detail_kind == PreviewOverlayDetailKind::FullSample && !self.channels.is_empty()
    }

    pub fn is_overview_only(&self) -> bool {
        self.detail_kind == PreviewOverlayDetailKind::OverviewOnly
            && (!self.overview_channels.is_empty() || self.overview_mixdown.is_some())
    }
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
pub enum RightDragMode {
    Seek,
    SelectRange,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditorHorizontalZoomAnchorMode {
    Pointer,
    Playhead,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditorPauseResumeMode {
    ReturnToLastStart,
    ContinueFromPause,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoopXfadeShape {
    Linear,
    EqualPower,
    LinearDip,
    EqualPowerDip,
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
    pub waveform_pyramid: Option<Arc<WaveformPyramidSet>>,
    #[allow(dead_code)]
    pub loop_enabled: bool,
    pub loading: bool,
    pub ch_samples: Vec<Vec<f32>>, // per-channel samples (playback buffer SR)
    pub buffer_sample_rate: u32,   // current sample rate of ch_samples
    pub samples_len: usize,        // length in samples
    pub samples_len_visual: usize, // length used for viewport math while loading
    pub loading_waveform_minmax: Vec<(f32, f32)>, // coarse overview while full decode streams
    pub view_offset: usize,        // first visible sample index
    pub view_offset_exact: f64,    // authoritative horizontal view position
    pub samples_per_px: f32,       // time zoom: samples per pixel
    pub vertical_zoom: f32,        // waveform vertical zoom multiplier
    pub vertical_view_center: f32, // centered waveform viewport anchor in [-1, 1]
    pub last_wave_w: f32,          // last waveform width (for resize anchoring)
    pub last_amplitude_nav_rect: Option<egui::Rect>, // transient right rail rect for UI tests
    pub last_amplitude_viewport_rect: Option<egui::Rect>, // transient right rail viewport
    pub last_amplitude_nav_click_at: f64, // transient double-click timing for amplitude rail
    pub last_amplitude_nav_click_pos: Option<egui::Pos2>, // transient double-click location
    pub viewport_source_generation: u64, // transient source generation for viewport cache
    pub viewport_render_requested_generation: u64, // transient latest queued viewport request
    pub viewport_render_requested_key: Option<EditorViewportRenderKey>, // transient desired key
    pub viewport_render_pending_fine_at: Option<Instant>, // transient fine render debounce
    pub viewport_render_inflight_coarse_generation: Option<u64>, // transient coarse inflight
    pub viewport_render_inflight_fine_generation: Option<u64>, // transient fine inflight
    pub viewport_render_coarse: Option<EditorViewportRenderCache>, // transient coarse viewport
    pub viewport_render_fine: Option<EditorViewportRenderCache>, // transient fine viewport
    pub viewport_render_last: Option<EditorViewportRenderCache>, // transient stale fallback
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
    pub primary_view: EditorPrimaryView, // high-level editor view
    pub spec_sub_view: EditorSpecSubView, // Spec subtree selection
    pub other_sub_view: EditorOtherSubView, // Other subtree selection
    pub show_waveform_overlay: bool,     // draw waveform overlay in feature views
    pub channel_view: ChannelView,       // Mixdown / All / Custom
    pub bpm_enabled: bool,               // grid toggle in editor
    pub bpm_value: f32,                  // current BPM for grid
    pub bpm_user_set: bool,              // user-overridden BPM
    pub bpm_offset_sec: f32,             // grid offset in seconds
    pub seek_hold: Option<SeekHoldState>, // key repeat state for seek
    pub snap_zero_cross: bool,           // enable zero-cross snapping
    pub selection_anchor_sample: Option<usize>, // shared Shift/click/drag anchor
    pub right_drag_mode: Option<RightDragMode>, // transient mode while secondary drag
    pub active_tool: ToolKind,           // current editing tool
    pub tool_state: ToolState,           // simple per-tool parameters
    pub loop_mode: LoopMode,             // Off / On (whole) / Marker
    pub dragging_marker: Option<MarkerKind>, // transient while dragging A/B
    // Preview audio state (non-destructive): tool-driven preview, cleared on tool/tab/view changes
    pub preview_audio_tool: Option<ToolKind>,
    pub active_tool_last: Option<ToolKind>,
    pub preview_offset_samples: Option<usize>,
    // Per-channel non-destructive preview overlay (green waveform)
    pub preview_overlay: Option<PreviewOverlay>,
    pub music_analysis_draft: MusicAnalysisDraft,
    pub plugin_fx_draft: PluginFxDraft,
    pub pending_loop_unwrap: Option<u32>,
    pub undo_stack: Vec<EditorUndoState>,
    pub undo_bytes: usize,
    pub redo_stack: Vec<EditorUndoState>,
    pub redo_bytes: usize,
}

impl EditorTab {
    pub fn leaf_view_mode(&self) -> ViewMode {
        match self.primary_view {
            EditorPrimaryView::Wave => ViewMode::Waveform,
            EditorPrimaryView::Spec => self.spec_sub_view.to_mode(),
            EditorPrimaryView::Other => self.other_sub_view.to_mode(),
        }
    }

    pub fn set_leaf_view_mode(&mut self, mode: ViewMode) {
        self.primary_view = EditorPrimaryView::from_mode(mode);
        match self.primary_view {
            EditorPrimaryView::Wave => {}
            EditorPrimaryView::Spec => {
                self.spec_sub_view = EditorSpecSubView::from_mode(mode);
            }
            EditorPrimaryView::Other => {
                self.other_sub_view = EditorOtherSubView::from_mode(mode);
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileMeta {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub sample_value_kind: SampleValueKind,
    pub bit_rate_bps: Option<u32>,
    pub duration_secs: Option<f32>,
    pub total_frames: Option<u64>,
    #[allow(dead_code)]
    pub rms_db: Option<f32>,
    pub peak_db: Option<f32>,
    pub lufs_i: Option<f32>,
    pub bpm: Option<f32>,
    pub created_at: Option<SystemTime>,
    pub modified_at: Option<SystemTime>,
    pub cover_art: Option<Arc<egui::ColorImage>>,
    pub thumb: Vec<(f32, f32)>,
    pub marker_fracs: Vec<f32>,
    pub loop_frac: Option<(f32, f32)>,
    pub decode_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SpectrogramData {
    pub frames: usize,
    pub bins: usize,
    pub frame_step: usize,
    pub sample_rate: u32,
    pub values_db: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorViewportRenderQuality {
    Coarse,
    Fine,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorViewportPayloadKind {
    Waveform,
    Spectral,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorViewportRenderKey {
    pub kind: EditorViewportPayloadKind,
    pub view_mode: ViewMode,
    pub source_generation: u64,
    pub display_samples_len: usize,
    pub start: usize,
    pub end: usize,
    pub lane_count: usize,
    pub lane_height_px: usize,
    pub wave_width_px: usize,
    pub use_mixdown: bool,
    pub visible_channels: Vec<usize>,
    pub samples_per_px_bits: u32,
    pub vertical_zoom_bits: u32,
    pub vertical_view_center_bits: u32,
    pub scale_bits: u32,
    pub spectro_cfg_digest: u64,
}

#[derive(Clone, Debug)]
pub enum EditorViewportWaveLane {
    Peaks(Vec<Peak>),
    Samples(Vec<f32>),
}

#[derive(Clone, Debug)]
pub struct EditorViewportWavePayload {
    pub lanes: Vec<EditorViewportWaveLane>,
}

#[derive(Clone)]
pub enum EditorViewportRenderPayload {
    Waveform(EditorViewportWavePayload),
    Image(Arc<egui::ColorImage>),
}

#[derive(Clone)]
pub enum EditorViewportCachePayload {
    Waveform(EditorViewportWavePayload),
    Image {
        image: Arc<egui::ColorImage>,
        texture: Option<egui::TextureHandle>,
    },
}

#[derive(Clone)]
pub struct EditorViewportRenderCache {
    pub key: EditorViewportRenderKey,
    pub quality: EditorViewportRenderQuality,
    pub ready_at: Instant,
    pub payload: EditorViewportCachePayload,
}

pub enum EditorViewportJobMsg {
    Ready {
        tab_path: PathBuf,
        generation: u64,
        quality: EditorViewportRenderQuality,
        key: EditorViewportRenderKey,
        payload: EditorViewportRenderPayload,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditorAnalysisKind {
    Spectrogram,
    Tempogram,
    Chromagram,
}

#[derive(Clone, Debug)]
pub struct TempogramData {
    pub frames: usize,
    pub tempo_bins: usize,
    pub frame_step: usize,
    pub sample_rate: u32,
    pub bpm_values: Vec<f32>,
    pub values: Vec<f32>,
    pub estimated_bpm: Option<f32>,
    pub confidence: f32,
}

#[derive(Clone, Debug)]
pub struct ChromagramData {
    pub frames: usize,
    pub bins: usize,
    pub frame_step: usize,
    pub sample_rate: u32,
    pub values: Vec<f32>,
    pub estimated_key: Option<String>,
    pub estimated_mode: Option<String>,
    pub confidence: f32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EditorAnalysisKey {
    pub path: PathBuf,
    pub kind: EditorAnalysisKind,
}

#[derive(Clone, Debug)]
pub enum EditorFeatureAnalysisData {
    Tempogram(TempogramData),
    Chromagram(ChromagramData),
}

pub enum EditorFeatureAnalysisJobMsg {
    TempogramDone {
        path: PathBuf,
        generation: u64,
        data: TempogramData,
    },
    ChromagramDone {
        path: PathBuf,
        generation: u64,
        data: ChromagramData,
    },
}

pub struct AnalysisProgress {
    pub done_units: usize,
    pub total_units: usize,
    pub started_at: std::time::Instant,
}

pub struct SpectrogramTile {
    pub path: PathBuf,
    pub generation: u64,
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
    Done { path: PathBuf, generation: u64 },
}

pub struct SpectrogramProgress {
    pub done_tiles: usize,
    pub total_tiles: usize,
    pub started_at: std::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListLoadKind {
    Folder,
    Files,
}

#[derive(Clone, Debug)]
pub enum ScanRequestKind {
    Folder { root: PathBuf },
    Explicit { paths: Vec<PathBuf> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingListLoadTargetKind {
    Select,
    OpenEditor,
}

#[derive(Clone, Debug)]
pub struct PendingListLoadTarget {
    pub path: PathBuf,
    pub kind: PendingListLoadTargetKind,
    pub auto_scroll: bool,
}

pub enum ScanMessage {
    Batch(Vec<PathBuf>),
    Progress { visited: usize, matched: usize },
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessingTarget {
    EditorTab(PathBuf),
    ListPreview(PathBuf),
}

impl ProcessingTarget {
    pub fn path(&self) -> &Path {
        match self {
            Self::EditorTab(path) | Self::ListPreview(path) => path.as_path(),
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::EditorTab(_) => "editor",
            Self::ListPreview(_) => "list",
        }
    }
}

pub struct ProcessingState {
    pub msg: String,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub job_id: u64,
    pub mode: RateMode,
    pub target: ProcessingTarget,
    pub autoplay_when_ready: bool,
    pub source_time_sec: Option<f64>,
    pub started_at: std::time::Instant,
    pub rx: std::sync::mpsc::Receiver<ProcessingResult>,
}

pub struct ProcessingResult {
    pub path: PathBuf,
    pub job_id: u64,
    pub mode: RateMode,
    pub target: ProcessingTarget,
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
    pub lufs_override: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VirtualTrimPhase {
    Copying,
    Processing,
}

pub struct VirtualTrimState {
    pub source_path: PathBuf,
    pub source_name: String,
    pub range: (usize, usize),
    pub copied_frames: usize,
    pub total_frames: usize,
    pub channels: Vec<Vec<f32>>,
    pub out_sr: u32,
    pub source_sr: u32,
    pub bits_per_sample: u16,
    pub source_start: usize,
    pub source_end: usize,
    pub source_ref: VirtualSourceRef,
    pub insert_idx: Option<usize>,
    pub phase: VirtualTrimPhase,
    pub rx: Option<std::sync::mpsc::Receiver<VirtualTrimResult>>,
    pub started_at: Instant,
}

pub struct VirtualTrimResult {
    pub source_path: PathBuf,
    pub source_name: String,
    pub audio: Arc<AudioBuffer>,
    pub meta: FileMeta,
    pub source_sr: u32,
    pub bits_per_sample: u16,
    pub source_start: usize,
    pub source_end: usize,
    pub source_ref: VirtualSourceRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorDecodeStage {
    Preview,
    StreamingFull,
    FinalizingAudio,
    FinalizingWaveform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorDecodeStrategy {
    CompressedProgressiveFull,
    StreamingOverviewFinalAudio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorDecodeEvent {
    Progress,
    FinalReady,
    Failed,
}

pub struct EditorDecodeResult {
    pub path: PathBuf,
    pub event: EditorDecodeEvent,
    pub channels: Vec<Vec<f32>>,
    pub waveform_minmax: Vec<(f32, f32)>,
    pub waveform_pyramid: Option<Arc<WaveformPyramidSet>>,
    pub loading_waveform_minmax: Vec<(f32, f32)>,
    pub buffer_sample_rate: u32,
    pub job_id: u64,
    pub error: Option<String>,
    pub stage: EditorDecodeStage,
    pub decoded_frames: usize,
    pub decoded_source_frames: usize,
    pub total_source_frames: Option<usize>,
    pub visual_total_frames: Option<usize>,
    pub progress_emit_gap_ms: Option<f32>,
    pub finalize_audio_ms: Option<f32>,
    pub finalize_waveform_ms: Option<f32>,
}

pub struct EditorDecodeState {
    pub path: PathBuf,
    pub started_at: Instant,
    pub rx: std::sync::mpsc::Receiver<EditorDecodeResult>,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub job_id: u64,
    pub partial_ready: bool,
    pub stage: EditorDecodeStage,
    pub decoded_frames: usize,
    pub estimated_total_frames: Option<usize>,
    pub total_source_frames: Option<usize>,
    pub visual_total_frames: Option<usize>,
    pub decoded_source_frames: usize,
    pub loading_waveform_updates: u64,
    pub max_progress_gap_ms: f32,
}

pub struct EditorDecodeUiStatus {
    pub message: String,
    pub progress: f32,
    pub show_percentage: bool,
}

#[derive(Clone)]
pub struct EditorUndoState {
    pub ch_samples: Vec<Vec<f32>>,
    pub samples_len: usize,
    pub samples_len_visual: usize,
    pub buffer_sample_rate: u32,
    pub waveform_minmax: Vec<(f32, f32)>,
    pub waveform_pyramid: Option<Arc<WaveformPyramidSet>>,
    pub view_offset: usize,
    pub vertical_zoom: f32,
    pub vertical_view_center: f32,
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
    pub plugin_fx_draft: PluginFxDraft,
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
    pub buffer_sample_rate: u32,
    pub waveform_minmax: Vec<(f32, f32)>,
    pub waveform_pyramid: Option<Arc<WaveformPyramidSet>>,
    pub display_meta: Option<FileMeta>,
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
    pub bpm_offset_sec: f32,
    pub snap_zero_cross: bool,
    pub tool_state: ToolState,
    pub active_tool: ToolKind,
    pub plugin_fx_draft: PluginFxDraft,
    pub show_waveform_overlay: bool,
    pub applied_effect_graph: Option<AppliedEffectGraphStamp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedEffectGraphStamp {
    pub template_id: String,
    pub template_name: String,
    pub template_updated_at_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectGraphSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectGraphNodeKind {
    Input,
    Output,
    Gain,
    Loudness,
    MonoMix,
    PitchShift,
    TimeStretch,
    Speed,
    PluginFx,
    Duplicate,
    SplitChannels,
    CombineChannels,
    DebugWaveform,
    DebugSpectrum,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectGraphNodeData {
    Input,
    Output,
    Gain {
        gain_db: f32,
    },
    Loudness {
        target_lufs: f32,
    },
    MonoMix {
        ignored_channels: Vec<bool>,
    },
    PitchShift {
        semitones: f32,
    },
    TimeStretch {
        rate: f32,
    },
    Speed {
        rate: f32,
    },
    PluginFx {
        #[serde(default)]
        config: EffectGraphPluginNodeConfig,
    },
    Duplicate,
    SplitChannels,
    CombineChannels,
    DebugWaveform {
        zoom: f32,
    },
    DebugSpectrum {
        mode: EffectGraphSpectrumMode,
        zoom: f32,
    },
}

impl EffectGraphNodeData {
    pub fn kind(&self) -> EffectGraphNodeKind {
        match self {
            Self::Input => EffectGraphNodeKind::Input,
            Self::Output => EffectGraphNodeKind::Output,
            Self::Gain { .. } => EffectGraphNodeKind::Gain,
            Self::Loudness { .. } => EffectGraphNodeKind::Loudness,
            Self::MonoMix { .. } => EffectGraphNodeKind::MonoMix,
            Self::PitchShift { .. } => EffectGraphNodeKind::PitchShift,
            Self::TimeStretch { .. } => EffectGraphNodeKind::TimeStretch,
            Self::Speed { .. } => EffectGraphNodeKind::Speed,
            Self::PluginFx { .. } => EffectGraphNodeKind::PluginFx,
            Self::Duplicate => EffectGraphNodeKind::Duplicate,
            Self::SplitChannels => EffectGraphNodeKind::SplitChannels,
            Self::CombineChannels => EffectGraphNodeKind::CombineChannels,
            Self::DebugWaveform { .. } => EffectGraphNodeKind::DebugWaveform,
            Self::DebugSpectrum { .. } => EffectGraphNodeKind::DebugSpectrum,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self.kind() {
            EffectGraphNodeKind::Input => "Input",
            EffectGraphNodeKind::Output => "Output",
            EffectGraphNodeKind::Gain => "Gain",
            EffectGraphNodeKind::Loudness => "LoudNorm",
            EffectGraphNodeKind::MonoMix => "Mono Mix",
            EffectGraphNodeKind::PitchShift => "PitchShift",
            EffectGraphNodeKind::TimeStretch => "TimeStretch",
            EffectGraphNodeKind::Speed => "Speed",
            EffectGraphNodeKind::PluginFx => "Plugin FX",
            EffectGraphNodeKind::Duplicate => "Duplicate",
            EffectGraphNodeKind::SplitChannels => "Split Channels",
            EffectGraphNodeKind::CombineChannels => "Combine Channels",
            EffectGraphNodeKind::DebugWaveform => "Waveform",
            EffectGraphNodeKind::DebugSpectrum => "Spectrum",
        }
    }

    pub fn default_for_kind(kind: EffectGraphNodeKind) -> Self {
        match kind {
            EffectGraphNodeKind::Input => Self::Input,
            EffectGraphNodeKind::Output => Self::Output,
            EffectGraphNodeKind::Gain => Self::Gain { gain_db: 0.0 },
            EffectGraphNodeKind::Loudness => Self::Loudness { target_lufs: -14.0 },
            EffectGraphNodeKind::MonoMix => Self::MonoMix {
                ignored_channels: vec![false; 8],
            },
            EffectGraphNodeKind::PitchShift => Self::PitchShift { semitones: 0.0 },
            EffectGraphNodeKind::TimeStretch => Self::TimeStretch { rate: 1.0 },
            EffectGraphNodeKind::Speed => Self::Speed { rate: 1.0 },
            EffectGraphNodeKind::PluginFx => Self::PluginFx {
                config: EffectGraphPluginNodeConfig::default(),
            },
            EffectGraphNodeKind::Duplicate => Self::Duplicate,
            EffectGraphNodeKind::SplitChannels => Self::SplitChannels,
            EffectGraphNodeKind::CombineChannels => Self::CombineChannels,
            EffectGraphNodeKind::DebugWaveform => Self::DebugWaveform { zoom: 1.0 },
            EffectGraphNodeKind::DebugSpectrum => Self::DebugSpectrum {
                mode: EffectGraphSpectrumMode::Log,
                zoom: 1.0,
            },
        }
    }

    pub fn input_ports(&self) -> &'static [&'static str] {
        match self {
            Self::Input => &[],
            Self::Output => &["in"],
            Self::Gain { .. }
            | Self::Loudness { .. }
            | Self::MonoMix { .. }
            | Self::PitchShift { .. }
            | Self::TimeStretch { .. }
            | Self::Speed { .. }
            | Self::PluginFx { .. }
            | Self::Duplicate
            | Self::DebugWaveform { .. }
            | Self::DebugSpectrum { .. }
            | Self::SplitChannels => &["in"],
            Self::CombineChannels => &["in1", "in2", "in3", "in4", "in5", "in6", "in7", "in8"],
        }
    }

    pub fn output_ports(&self) -> &'static [&'static str] {
        match self {
            Self::Output => &[],
            Self::Input
            | Self::Gain { .. }
            | Self::Loudness { .. }
            | Self::MonoMix { .. }
            | Self::PitchShift { .. }
            | Self::TimeStretch { .. }
            | Self::Speed { .. }
            | Self::PluginFx { .. }
            | Self::DebugWaveform { .. }
            | Self::DebugSpectrum { .. }
            | Self::CombineChannels => &["out"],
            Self::Duplicate => &["out1", "out2"],
            Self::SplitChannels => &["ch1", "ch2", "ch3", "ch4", "ch5", "ch6", "ch7", "ch8"],
        }
    }

    pub fn has_input_port(&self, port_id: &str) -> bool {
        self.input_ports().iter().any(|port| *port == port_id)
    }

    pub fn has_output_port(&self, port_id: &str) -> bool {
        self.output_ports().iter().any(|port| *port == port_id)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectGraphNode {
    pub id: String,
    pub ui_pos: [f32; 2],
    pub ui_size: [f32; 2],
    #[serde(flatten)]
    pub data: EffectGraphNodeData,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectGraphEdge {
    pub id: String,
    pub from_node_id: String,
    #[serde(default = "default_effect_graph_out_port")]
    pub from_port_id: String,
    pub to_node_id: String,
    #[serde(default = "default_effect_graph_in_port")]
    pub to_port_id: String,
}

fn default_effect_graph_out_port() -> String {
    "out".to_string()
}

fn default_effect_graph_in_port() -> String {
    "in".to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectGraphCanvasPersistedState {
    pub zoom: f32,
    pub pan: [f32; 2],
}

impl Default for EffectGraphCanvasPersistedState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: [48.0, 48.0],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectGraphDocument {
    pub schema_version: u32,
    pub name: String,
    pub nodes: Vec<EffectGraphNode>,
    pub edges: Vec<EffectGraphEdge>,
    #[serde(default)]
    pub canvas: EffectGraphCanvasPersistedState,
}

impl Default for EffectGraphDocument {
    fn default() -> Self {
        Self {
            schema_version: 3,
            name: "New Effect Graph".to_string(),
            nodes: vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [60.0, 120.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [360.0, 120.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            edges: vec![EffectGraphEdge {
                id: "edge_input_output".to_string(),
                from_node_id: "input".to_string(),
                from_port_id: "out".to_string(),
                to_node_id: "output".to_string(),
                to_port_id: "in".to_string(),
            }],
            canvas: EffectGraphCanvasPersistedState::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectGraphTemplateFile {
    pub schema_version: u32,
    pub template_id: String,
    pub name: String,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub graph: EffectGraphDocument,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectGraphValidationIssue {
    pub severity: EffectGraphSeverity,
    pub code: String,
    pub message: String,
    pub node_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EffectGraphLibraryEntry {
    pub template_id: String,
    pub name: String,
    pub path: PathBuf,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub valid: bool,
}

#[derive(Clone, Debug, Default)]
pub struct EffectGraphLibraryState {
    pub entries: Vec<EffectGraphLibraryEntry>,
    pub new_template_name: String,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct EffectGraphCanvasState {
    pub zoom: f32,
    pub pan: [f32; 2],
    pub selected_nodes: HashSet<String>,
    pub selected_edge_id: Option<String>,
    pub connecting_from_port: Option<EffectGraphPortKey>,
    pub drag_palette_kind: Option<EffectGraphNodeKind>,
    pub last_canvas_pointer_world: Option<[f32; 2]>,
    pub focus_node_id: Option<String>,
    pub background_panning: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectGraphDebugViewState {
    pub scroll_x: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectGraphPluginNodeRuntimeState {
    pub backend: Option<crate::plugin::PluginHostBackend>,
    pub gui_capabilities: crate::plugin::GuiCapabilities,
    pub gui_status: crate::plugin::GuiSessionStatus,
    pub last_error: Option<String>,
    pub last_backend_log: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectGraphUndoState {
    pub active_template_id: Option<String>,
    pub draft: EffectGraphDocument,
    pub draft_dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectGraphNodeRunPhase {
    Idle,
    Running,
    Success,
    Failed,
}

impl Default for EffectGraphNodeRunPhase {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Clone, Debug, Default)]
pub struct EffectGraphNodeRunStatus {
    pub phase: EffectGraphNodeRunPhase,
    pub elapsed_ms: Option<f32>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum EffectGraphDebugPreview {
    Waveform { mono: Vec<f32>, sample_rate: u32 },
    Spectrum { spectrogram: SpectrogramData },
}

#[derive(Clone, Debug)]
pub struct EffectGraphConsoleLine {
    pub timestamp_unix_ms: u64,
    pub severity: EffectGraphSeverity,
    pub scope: String,
    pub message: String,
    pub node_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EffectGraphConsoleState {
    pub lines: VecDeque<EffectGraphConsoleLine>,
    pub max_lines: usize,
}

impl Default for EffectGraphConsoleState {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines: 500,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectGraphRunMode {
    TestPreview,
    ApplyToListSelection,
}

#[derive(Clone, Debug)]
pub enum EffectGraphWorkerEvent {
    RunStarted {
        mode: EffectGraphRunMode,
        total: usize,
    },
    PathStarted {
        path: PathBuf,
        index: usize,
        total: usize,
    },
    NodeStarted {
        node_id: String,
    },
    NodeFinished {
        node_id: String,
        elapsed_ms: f32,
    },
    NodeLog {
        node_id: String,
        severity: EffectGraphSeverity,
        message: String,
    },
    NodeDebugPreview {
        node_id: String,
        preview: EffectGraphDebugPreview,
    },
    PathFinished {
        path: PathBuf,
        output_bus: EffectGraphAudioBus,
        input_bus: Option<EffectGraphAudioBus>,
        input_monitor_audio: Option<Arc<AudioBuffer>>,
        monitor_audio: Vec<Vec<f32>>,
        rough_waveform: Vec<(f32, f32)>,
        total_elapsed_ms: f32,
    },
    Failed {
        path: Option<PathBuf>,
        node_id: Option<String>,
        message: String,
    },
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectGraphPlaybackTarget {
    Input,
    Output,
}

#[derive(Clone, Debug, Default)]
pub struct EffectGraphTesterState {
    pub target_path_input: String,
    pub target_path: Option<PathBuf>,
    pub last_input_audio: Option<Arc<AudioBuffer>>,
    pub last_input_bus: Option<EffectGraphAudioBus>,
    pub last_output_audio: Option<Arc<AudioBuffer>>,
    pub last_output_bus: Option<EffectGraphAudioBus>,
    pub last_run_ms: Option<f32>,
    pub last_output_summary: String,
    pub last_error: Option<String>,
    pub playback_target: Option<EffectGraphPlaybackTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EffectGraphPortKey {
    pub node_id: String,
    pub port_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectGraphChannelLayout {
    pub declared_width: usize,
    pub entries: Vec<EffectGraphChannelLayoutEntry>,
}

impl Default for EffectGraphChannelLayout {
    fn default() -> Self {
        Self {
            declared_width: 0,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectGraphChannelLayoutEntry {
    Dense,
    Slotted {
        slot_index: usize,
    },
    Vacant {
        requested_slot: usize,
    },
    AutoPlaced {
        origin_slot: Option<usize>,
        branch_group_id: String,
        branch_channel_index: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectGraphCombineMode {
    Concat,
    Restore,
    Adaptive,
    Mixed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectGraphChannelFlowHint {
    PlainDense,
    Slotted {
        declared_width_hint: Option<usize>,
        slot_indices: Vec<usize>,
    },
    AutoPlaced {
        declared_width_hint: Option<usize>,
        origin_slots: Vec<Option<usize>>,
        branch_group_count: usize,
        predicted_channels_hint: usize,
    },
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectGraphAudioBus {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
    pub channel_layout: EffectGraphChannelLayout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectGraphPredictedFormat {
    pub channel_count: usize,
    pub sample_rate: u32,
    pub combine_mode: Option<EffectGraphCombineMode>,
    pub summary: String,
}

#[derive(Debug)]
pub struct EffectGraphRunnerState {
    pub mode: Option<EffectGraphRunMode>,
    pub started_at: Option<Instant>,
    pub total: usize,
    pub done: usize,
    pub current_path: Option<PathBuf>,
    pub template_stamp: Option<AppliedEffectGraphStamp>,
    pub rx: Option<Receiver<EffectGraphWorkerEvent>>,
    pub cancel_requested: Option<Arc<AtomicBool>>,
    pub node_status: HashMap<String, EffectGraphNodeRunStatus>,
}

impl Default for EffectGraphRunnerState {
    fn default() -> Self {
        Self {
            mode: None,
            started_at: None,
            total: 0,
            done: 0,
            current_path: None,
            template_stamp: None,
            rx: None,
            cancel_requested: None,
            node_status: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum EffectGraphPendingAction {
    CloseWorkspace,
    SwitchTemplate(String),
    DeleteTemplate(String),
}

#[derive(Debug)]
pub struct EffectGraphPluginProbeState {
    pub job_id: u64,
    pub node_id: String,
    pub started_at: Instant,
    pub rx: std::sync::mpsc::Receiver<PluginProbeResult>,
}

#[derive(Debug)]
pub struct EffectGraphPluginGuiSessionState {
    pub node_id: String,
    pub session_id: u64,
    pub started_at: Instant,
    pub cmd_tx: std::sync::mpsc::Sender<PluginGuiCommand>,
    pub rx: std::sync::mpsc::Receiver<PluginGuiEvent>,
}

#[derive(Clone, Debug)]
pub struct EffectGraphPredictionCacheEntry {
    pub generation: u64,
    pub target_signature: String,
    pub result: Result<EffectGraphPredictedFormat, String>,
}

#[derive(Debug)]
pub struct EffectGraphInputPreviewResult {
    pub job_id: u64,
    pub target_path: PathBuf,
    pub input_bus: Option<EffectGraphAudioBus>,
    pub input_audio: Option<Arc<AudioBuffer>>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct EffectGraphInputPreviewState {
    pub active_job_id: u64,
    pub autoplay_requested: bool,
    pub rx: Option<Receiver<EffectGraphInputPreviewResult>>,
}

#[derive(Debug)]
pub struct EffectGraphApplyPostprocessJob {
    pub generation: u64,
    pub path: PathBuf,
    pub channels: Vec<Vec<f32>>,
    pub final_sample_rate: u32,
    pub bits_per_sample: u16,
}

#[derive(Debug)]
pub struct EffectGraphApplyPostprocessResult {
    pub generation: u64,
    pub path: PathBuf,
    pub waveform_minmax: Vec<(f32, f32)>,
    pub waveform_pyramid: Option<Arc<WaveformPyramidSet>>,
    pub display_meta: FileMeta,
}

#[derive(Debug)]
pub struct EffectGraphState {
    pub workspace_open: bool,
    pub active_template_id: Option<String>,
    pub draft: EffectGraphDocument,
    pub draft_dirty: bool,
    pub library: EffectGraphLibraryState,
    pub canvas: EffectGraphCanvasState,
    pub tester: EffectGraphTesterState,
    pub runner: EffectGraphRunnerState,
    pub debug_previews: HashMap<String, Arc<EffectGraphDebugPreview>>,
    pub debug_view_state: HashMap<String, EffectGraphDebugViewState>,
    pub plugin_runtime: HashMap<String, EffectGraphPluginNodeRuntimeState>,
    pub plugin_probe_state: Option<EffectGraphPluginProbeState>,
    pub plugin_gui_state: Option<EffectGraphPluginGuiSessionState>,
    pub run_generation: u64,
    pub prediction_generation: u64,
    pub cached_predicted_output_format: Option<EffectGraphPredictionCacheEntry>,
    pub input_preview_worker_state: EffectGraphInputPreviewState,
    pub postprocess_tx: Option<Sender<EffectGraphApplyPostprocessJob>>,
    pub postprocess_rx: Option<Receiver<EffectGraphApplyPostprocessResult>>,
    pub pending_effect_graph_commits: HashMap<PathBuf, u64>,
    pub undo_stack: Vec<EffectGraphUndoState>,
    pub redo_stack: Vec<EffectGraphUndoState>,
    pub console: EffectGraphConsoleState,
    pub validation: Vec<EffectGraphValidationIssue>,
    pub left_panel_width: f32,
    pub right_panel_width: f32,
    pub bottom_panel_height: f32,
    pub last_editor_tab: Option<usize>,
    pub clipboard_paste_serial: u64,
    pub pending_action: Option<EffectGraphPendingAction>,
    pub show_unsaved_prompt: bool,
}

impl Default for EffectGraphState {
    fn default() -> Self {
        let draft = EffectGraphDocument::default();
        Self {
            workspace_open: false,
            active_template_id: None,
            canvas: EffectGraphCanvasState {
                zoom: draft.canvas.zoom,
                pan: draft.canvas.pan,
                ..Default::default()
            },
            draft,
            draft_dirty: false,
            library: EffectGraphLibraryState::default(),
            tester: EffectGraphTesterState::default(),
            runner: EffectGraphRunnerState::default(),
            debug_previews: HashMap::new(),
            debug_view_state: HashMap::new(),
            plugin_runtime: HashMap::new(),
            plugin_probe_state: None,
            plugin_gui_state: None,
            run_generation: 0,
            prediction_generation: 0,
            cached_predicted_output_format: None,
            input_preview_worker_state: EffectGraphInputPreviewState::default(),
            postprocess_tx: None,
            postprocess_rx: None,
            pending_effect_graph_commits: HashMap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            console: EffectGraphConsoleState::default(),
            validation: Vec::new(),
            left_panel_width: 260.0,
            right_panel_width: 300.0,
            bottom_panel_height: 180.0,
            last_editor_tab: None,
            clipboard_paste_serial: 0,
            pending_action: None,
            show_unsaved_prompt: false,
        }
    }
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OfflineRenderSpec {
    pub mode: RateMode,
    pub speed_rate: f32,
    pub pitch_semitones: f32,
    pub stretch_rate: f32,
    pub master_gain_db: f32,
    pub file_gain_db: f32,
    pub out_sr: u32,
    pub target_sr: Option<u32>,
    pub bit_depth: Option<crate::wave::WavBitDepth>,
    pub quality: SrcQuality,
    pub source_variant: u64,
    pub loop_preview_enabled: bool,
    pub effect_state_version: u64,
}

pub struct ListPreviewResult {
    pub path: PathBuf,
    pub channels: Vec<Vec<f32>>,
    pub play_sr: u32,
    pub job_id: u64,
    pub is_final: bool,
    pub settings: ListPreviewSettings,
}

pub type ListPreviewSettings = OfflineRenderSpec;

#[derive(Clone)]
pub struct ListPreviewCacheEntry {
    pub audio: Arc<AudioBuffer>,
    pub play_sr: u32,
    pub truncated: bool,
    pub settings: ListPreviewSettings,
}

pub struct ListPreviewPrefetchResult {
    pub path: PathBuf,
    pub entry: Option<ListPreviewCacheEntry>,
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

pub struct BulkResampleState {
    pub targets: Vec<PathBuf>,
    pub target_sr: u32,
    pub index: usize,
    pub before: ListSelectionSnapshot,
    pub before_items: Vec<ListUndoItem>,
    pub started_at: Instant,
    pub chunk: usize,
    pub cancel_requested: bool,
    pub finalizing: bool,
    pub after_items: Vec<ListUndoItem>,
    pub after_index: usize,
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
    pub format_override: Option<String>,
    pub conflict: ConflictPolicy,
    pub backup_bak: bool,
    pub export_srt: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptAiConfig {
    pub language: String,
    pub task: String,
    pub max_new_tokens: usize,
    pub overwrite_existing_srt: bool,
    pub perf_mode: TranscriptPerfMode,
    pub model_variant: TranscriptModelVariant,
    pub omit_language_token: bool,
    pub omit_notimestamps_token: bool,
    pub vad_enabled: bool,
    pub vad_model_path: Option<PathBuf>,
    pub vad_threshold: f32,
    pub vad_min_speech_ms: usize,
    pub vad_min_silence_ms: usize,
    pub vad_speech_pad_ms: usize,
    pub max_window_ms: usize,
    pub no_speech_threshold: Option<f32>,
    pub logprob_threshold: Option<f32>,
    pub compute_target: TranscriptComputeTarget,
    pub dml_device_id: i32,
    pub cpu_intra_threads: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptComputeTarget {
    Auto,
    Cpu,
    Gpu,
    Npu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptPerfMode {
    Stable,
    Balanced,
    Boost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptModelVariant {
    Auto,
    Fp16,
    Quantized,
}

impl Default for TranscriptAiConfig {
    fn default() -> Self {
        Self {
            language: "auto".to_string(),
            task: "transcribe".to_string(),
            max_new_tokens: 128,
            overwrite_existing_srt: false,
            perf_mode: TranscriptPerfMode::Stable,
            model_variant: TranscriptModelVariant::Auto,
            omit_language_token: false,
            omit_notimestamps_token: false,
            vad_enabled: true,
            vad_model_path: None,
            vad_threshold: 0.5,
            vad_min_speech_ms: 250,
            vad_min_silence_ms: 100,
            vad_speech_pad_ms: 30,
            max_window_ms: 30_000,
            no_speech_threshold: None,
            logprob_threshold: None,
            compute_target: TranscriptComputeTarget::Auto,
            dml_device_id: 0,
            cpu_intra_threads: 0,
        }
    }
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
    pub external_dummy_merge: bool,
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
    pub no_ipc_forward: bool,
    pub ipc_rx: Option<Arc<Mutex<std::sync::mpsc::Receiver<crate::ipc::IpcRequest>>>>,
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
            external_dummy_merge: false,
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
            no_ipc_forward: false,
            ipc_rx: None,
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
    pub input_trace_to_console: bool,
    pub input_trace_enabled: bool,
    pub event_trace_enabled: bool,
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
            input_trace_to_console: false,
            input_trace_enabled: false,
            event_trace_enabled: false,
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
    pub last_clip_allow: bool,
    pub last_clip_wants_kb: bool,
    pub last_clip_ctrl: bool,
    pub last_clip_event_copy: bool,
    pub last_clip_event_paste: bool,
    pub last_clip_raw_key_c: bool,
    pub last_clip_raw_key_v: bool,
    pub last_clip_os_ctrl: bool,
    pub last_clip_os_key_c: bool,
    pub last_clip_os_key_v: bool,
    pub last_clip_consumed_copy: bool,
    pub last_clip_consumed_paste: bool,
    pub last_clip_copy_trigger: bool,
    pub last_clip_paste_trigger: bool,
    pub auto: Option<DebugAutomation>,
    pub check_counter: u32,
    pub overlay_trace: bool,
    pub dummy_list_count: u32,
    pub frame_last_ms: f32,
    pub frame_peak_ms: f32,
    pub frame_sum_ms: f64,
    pub frame_samples: u64,
    pub started_at: Instant,
    pub ui_input_started_at: Option<Instant>,
    pub ui_input_to_paint_ms: VecDeque<f32>,
    pub list_select_started_at: Option<Instant>,
    pub list_select_started_path: Option<PathBuf>,
    pub tab_switch_started_at: Option<Instant>,
    pub tab_switch_started_path: Option<PathBuf>,
    pub tab_switch_to_interactive_ms: VecDeque<f32>,
    pub editor_open_started_at: Option<Instant>,
    pub editor_open_started_path: Option<PathBuf>,
    pub editor_open_partial_logged: bool,
    pub editor_open_shell_paint_logged: bool,
    pub editor_open_first_paint_logged: bool,
    pub editor_open_to_shell_paint_ms: VecDeque<f32>,
    pub editor_open_to_partial_ms: VecDeque<f32>,
    pub editor_open_to_final_ms: VecDeque<f32>,
    pub editor_open_to_first_paint_ms: VecDeque<f32>,
    pub editor_stream_activation_ms: VecDeque<f32>,
    pub editor_mixdown_build_ms: VecDeque<f32>,
    pub editor_wave_render_ms: VecDeque<f32>,
    pub editor_decode_progress_emit_ms: VecDeque<f32>,
    pub editor_decode_finalize_audio_ms: VecDeque<f32>,
    pub editor_decode_finalize_waveform_ms: VecDeque<f32>,
    pub editor_loading_progress_max_gap_ms: VecDeque<f32>,
    pub editor_loading_waveform_updates: u64,
    pub waveform_render_ms: VecDeque<f32>,
    pub waveform_query_ms: VecDeque<f32>,
    pub waveform_draw_ms: VecDeque<f32>,
    pub waveform_lod_raw_count: u64,
    pub waveform_lod_visible_count: u64,
    pub waveform_lod_pyramid_count: u64,
    pub select_to_preview_ms: VecDeque<f32>,
    pub select_to_play_ms: VecDeque<f32>,
    pub metadata_probe_ms: VecDeque<f32>,
    pub bg_lufs_job_ms: VecDeque<f32>,
    pub bg_dbfs_job_ms: VecDeque<f32>,
    pub src_resample_ms: VecDeque<f32>,
    pub src_cache_hits: u64,
    pub src_cache_misses: u64,
    pub plugin_scan_ms: VecDeque<f32>,
    pub plugin_probe_ms: VecDeque<f32>,
    pub plugin_preview_ms: VecDeque<f32>,
    pub plugin_apply_ms: VecDeque<f32>,
    pub autoplay_pending_count: u64,
    pub stale_preview_cancel_count: u64,
    pub plugin_stale_drop_count: u64,
    pub plugin_worker_timeout_count: u64,
    pub plugin_native_fallback_count: u64,
}

impl DebugState {
    pub fn new(cfg: DebugConfig) -> Self {
        let show = cfg.enabled;
        let check_counter = cfg.check_interval_frames.max(1);
        let input_trace_enabled = cfg.input_trace_enabled;
        let event_trace_enabled = cfg.event_trace_enabled;
        Self {
            cfg,
            show_window: show,
            logs: VecDeque::new(),
            input_trace: VecDeque::new(),
            input_trace_enabled,
            input_trace_max: 200,
            event_trace: VecDeque::new(),
            event_trace_enabled,
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
            last_clip_allow: false,
            last_clip_wants_kb: false,
            last_clip_ctrl: false,
            last_clip_event_copy: false,
            last_clip_event_paste: false,
            last_clip_raw_key_c: false,
            last_clip_raw_key_v: false,
            last_clip_os_ctrl: false,
            last_clip_os_key_c: false,
            last_clip_os_key_v: false,
            last_clip_consumed_copy: false,
            last_clip_consumed_paste: false,
            last_clip_copy_trigger: false,
            last_clip_paste_trigger: false,
            auto: None,
            check_counter,
            overlay_trace: false,
            dummy_list_count: 300000,
            frame_last_ms: 0.0,
            frame_peak_ms: 0.0,
            frame_sum_ms: 0.0,
            frame_samples: 0,
            started_at: Instant::now(),
            ui_input_started_at: None,
            ui_input_to_paint_ms: VecDeque::new(),
            list_select_started_at: None,
            list_select_started_path: None,
            tab_switch_started_at: None,
            tab_switch_started_path: None,
            tab_switch_to_interactive_ms: VecDeque::new(),
            editor_open_started_at: None,
            editor_open_started_path: None,
            editor_open_partial_logged: false,
            editor_open_shell_paint_logged: false,
            editor_open_first_paint_logged: false,
            editor_open_to_shell_paint_ms: VecDeque::new(),
            editor_open_to_partial_ms: VecDeque::new(),
            editor_open_to_final_ms: VecDeque::new(),
            editor_open_to_first_paint_ms: VecDeque::new(),
            editor_stream_activation_ms: VecDeque::new(),
            editor_mixdown_build_ms: VecDeque::new(),
            editor_wave_render_ms: VecDeque::new(),
            editor_decode_progress_emit_ms: VecDeque::new(),
            editor_decode_finalize_audio_ms: VecDeque::new(),
            editor_decode_finalize_waveform_ms: VecDeque::new(),
            editor_loading_progress_max_gap_ms: VecDeque::new(),
            editor_loading_waveform_updates: 0,
            waveform_render_ms: VecDeque::new(),
            waveform_query_ms: VecDeque::new(),
            waveform_draw_ms: VecDeque::new(),
            waveform_lod_raw_count: 0,
            waveform_lod_visible_count: 0,
            waveform_lod_pyramid_count: 0,
            select_to_preview_ms: VecDeque::new(),
            select_to_play_ms: VecDeque::new(),
            metadata_probe_ms: VecDeque::new(),
            bg_lufs_job_ms: VecDeque::new(),
            bg_dbfs_job_ms: VecDeque::new(),
            src_resample_ms: VecDeque::new(),
            src_cache_hits: 0,
            src_cache_misses: 0,
            plugin_scan_ms: VecDeque::new(),
            plugin_probe_ms: VecDeque::new(),
            plugin_preview_ms: VecDeque::new(),
            plugin_apply_ms: VecDeque::new(),
            autoplay_pending_count: 0,
            stale_preview_cancel_count: 0,
            plugin_stale_drop_count: 0,
            plugin_worker_timeout_count: 0,
            plugin_native_fallback_count: 0,
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
    SetSelection {
        start_frac: f32,
        end_frac: f32,
    },
    SetTrimRange {
        start_frac: f32,
        end_frac: f32,
    },
    SetLoopRegion {
        start_frac: f32,
        end_frac: f32,
    },
    SetLoopMode(LoopMode),
    SetLoopXfade {
        ms: f32,
        shape: LoopXfadeShape,
    },
    AddMarker {
        frac: f32,
    },
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
    ApplyGain {
        db: f32,
    },
    ApplyNormalize {
        db: f32,
    },
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
