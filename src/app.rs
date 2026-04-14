use crate::audio::{AudioBuffer, AudioEngine};
use crate::ipc;
use crate::wave::build_minmax;
use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
// use walkdir::WalkDir; // unused here (used in logic.rs)

type HeavyPreviewMessage = (std::path::PathBuf, ToolKind, Vec<f32>, u64);
type HeavyOverlayMessage = (
    std::path::PathBuf,
    ToolKind,
    crate::app::types::PreviewOverlay,
    u64,
    bool,
);

mod app_init;
mod audio_ops;
mod capture;
mod clipboard_ops;
mod cli_workspace;
mod cli_ops;
mod debug_ops;
mod dialogs;
mod editor_decode_ops;
mod editor_features;
mod editor_ops;
mod editor_viewport;
mod effect_graph_ops;
mod export_ops;
mod external;
mod external_load_jobs;
mod external_load_ops;
mod external_ops;
mod frame_ops;
mod gain_ops;
mod helpers;
mod hf_cache;
mod input_ops;
#[cfg(feature = "kittest")]
mod kittest_ops;
mod list_ops;
mod list_preview_ops;
mod list_state_ops;
mod list_undo;
mod loading_ops;
mod logic;
mod loudnorm_ops;
mod meta;
mod meta_ops;
mod music_ai_ops;
mod music_onnx;
mod plugin_ops;
mod preview;
mod preview_ops;
mod project;
mod rename_ops;
mod render;
mod resample_ops;
mod scan_ops;
mod search_ops;
mod session_ops;
mod spectrogram;
mod spectrogram_jobs;
mod startup;
mod tab_ops;
mod temp_audio_ops;
mod theme_ops;
mod threading;
mod tool_ops;
mod tooling;
mod transcript;
mod transcript_ai_ops;
mod transcript_onnx;
mod transcript_ops;
mod types;
mod ui;
mod zoo_assets;
mod zoo_ops;
#[cfg(feature = "kittest")]
use self::dialogs::TestDialogQueue;
use self::render::waveform_pyramid::WaveformScratch;
use self::session_ops::ProjectOpenState;
use self::tooling::{ToolDef, ToolJob, ToolLogEntry, ToolRunResult};
use self::types::*;
pub use self::types::{
    ExternalKeyRule, ExternalRegexInput, FadeShape, LoopMode, LoopXfadeShape, RateMode,
    StartupConfig, ToolKind, TranscriptComputeTarget, TranscriptModelVariant,
    TranscriptPerfMode, ViewMode, WorkspaceView,
};
pub use self::cli_ops::run_cli;

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
const META_UPDATE_FRAME_BUDGET: usize = 256;
const META_SORT_MIN_INTERVAL_MS: u64 = 120;
const LIST_META_PREFETCH_BUDGET: usize = 64;
const LIST_PREVIEW_CACHE_MAX: usize = 48;
const LIST_PREVIEW_PREFETCH_INFLIGHT_MAX: usize = 2;
const LIST_BG_META_LARGE_THRESHOLD: usize = 8_000;
const LIST_BG_META_INFLIGHT_LIMIT: usize = 192;
const LIST_PLAY_EMIT_SECS: f32 = 0.75;
const EDITOR_MIN_VERTICAL_ZOOM: f32 = 0.25;
const EDITOR_MAX_VERTICAL_ZOOM: f32 = 32.0;
const EDITOR_MIN_SAMPLES_PER_PX: f32 = 0.0025;
const EDITOR_VIEWPORT_COARSE_MAX_COLUMNS: usize = 128;
const EDITOR_VIEWPORT_COARSE_FINE_DELAY_MS: u64 = 90;

// moved to types.rs

#[derive(Clone, Debug)]
struct ExternalLoadQueueItem {
    path: PathBuf,
    sheet_name: Option<String>,
    has_header: bool,
    header_row: Option<usize>,
    data_row: Option<usize>,
    target: external_ops::ExternalLoadTarget,
}

#[derive(Clone, Debug)]
struct PendingExternalRestore {
    active_source: Option<usize>,
    visible_columns: Vec<String>,
    key_column: Option<String>,
    show_unmatched: bool,
}

#[derive(Clone)]
struct ZooFrameImage {
    image: egui::ColorImage,
    delay_s: f32,
}

#[derive(Clone)]
struct ZooFrameTexture {
    texture: egui::TextureHandle,
    delay_s: f32,
}

struct TranscriptAiItemResult {
    path: PathBuf,
    srt_path: Option<PathBuf>,
    detected_language: Option<String>,
    error: Option<String>,
}

enum TranscriptAiRunResult {
    Started(PathBuf),
    Item(TranscriptAiItemResult),
    Finished,
}

struct TranscriptAiRunState {
    started_at: std::time::Instant,
    total: usize,
    process_total: usize,
    skipped_total: usize,
    done: usize,
    pending: HashSet<PathBuf>,
    cancel_requested: Arc<AtomicBool>,
    rx: std::sync::mpsc::Receiver<TranscriptAiRunResult>,
}

struct TranscriptModelDownloadResult {
    model_dir: Option<PathBuf>,
    error: Option<String>,
}

enum TranscriptModelDownloadEvent {
    Progress { done: usize, total: usize },
    Finished(TranscriptModelDownloadResult),
}

struct TranscriptModelDownloadState {
    _started_at: std::time::Instant,
    done: usize,
    total: usize,
    rx: std::sync::mpsc::Receiver<TranscriptModelDownloadEvent>,
}

struct MusicAnalyzeItemResult {
    path: PathBuf,
    result: Option<MusicAnalysisResult>,
    source_len_samples: usize,
    source_kind: MusicAnalysisSourceKind,
    stems: Option<MusicStemSet>,
    error: Option<String>,
}

enum MusicAnalyzeRunResult {
    Started(PathBuf),
    Progress { path: PathBuf, message: String },
    Item(MusicAnalyzeItemResult),
    Finished,
}

struct MusicAnalyzeRunState {
    started_at: std::time::Instant,
    total: usize,
    done: usize,
    pending: HashSet<PathBuf>,
    cancel_requested: Arc<AtomicBool>,
    current_step: String,
    rx: std::sync::mpsc::Receiver<MusicAnalyzeRunResult>,
}

struct MusicPreviewResult {
    tab_path: PathBuf,
    generation: u64,
    overlay: Option<Vec<Vec<f32>>>,
    mono: Option<Vec<f32>>,
    peak_abs: f32,
    clip_applied: bool,
    error: Option<String>,
}

struct MusicPreviewRunState {
    started_at: std::time::Instant,
    tab_path: PathBuf,
    generation: u64,
    cancel_requested: Arc<AtomicBool>,
    rx: std::sync::mpsc::Receiver<MusicPreviewResult>,
}

struct MusicModelDownloadResult {
    model_dir: Option<PathBuf>,
    error: Option<String>,
}

enum MusicModelDownloadEvent {
    Progress { done: usize, total: usize },
    Finished(MusicModelDownloadResult),
}

struct MusicModelDownloadState {
    _started_at: std::time::Instant,
    done: usize,
    total: usize,
    rx: std::sync::mpsc::Receiver<MusicModelDownloadEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PlaybackSourceKind {
    None,
    ListPreview(PathBuf),
    EditorTab(PathBuf),
    EffectGraph,
    ToolPreview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlaybackTransportKind {
    Buffer,
    ExactStreamWav,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingTabActivationKind {
    TabSwitch,
    InitialOpen,
}

#[derive(Clone, Debug)]
struct PlaybackSessionState {
    source: PlaybackSourceKind,
    transport: PlaybackTransportKind,
    user_speed: f32,
    transport_sr: u32,
    is_playing: bool,
    last_play_start_display_sample: Option<usize>,
    applied_mode: RateMode,
    applied_playback_rate: f32,
    dry_audio: Option<Arc<AudioBuffer>>,
    last_applied_master_gain_db: f32,
    last_applied_file_gain_db: f32,
}

impl Default for PlaybackSessionState {
    fn default() -> Self {
        Self {
            source: PlaybackSourceKind::None,
            transport: PlaybackTransportKind::Buffer,
            user_speed: 1.0,
            transport_sr: 48_000,
            is_playing: false,
            last_play_start_display_sample: None,
            applied_mode: RateMode::Speed,
            applied_playback_rate: 1.0,
            dry_audio: None,
            last_applied_master_gain_db: f32::NAN,
            last_applied_file_gain_db: f32::NAN,
        }
    }
}

struct PlaybackFxRenderState {
    source: PlaybackSourceKind,
    source_generation: u64,
    job_id: u64,
    mode: RateMode,
    playback_rate: f32,
    pitch_semitones: f32,
    autoplay_when_ready: bool,
    rx: std::sync::mpsc::Receiver<PlaybackFxResult>,
}

struct PlaybackFxResult {
    source: PlaybackSourceKind,
    source_generation: u64,
    job_id: u64,
    mode: RateMode,
    playback_rate: f32,
    pitch_semitones: f32,
    buffer_sr: u32,
    audio: Arc<AudioBuffer>,
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
    audio_output_device_name: Option<String>,
    audio_output_devices: Vec<String>,
    audio_output_error: Option<String>,
    pub playback_rate: f32,
    playback_session: PlaybackSessionState,
    playback_fx_state: Option<PlaybackFxRenderState>,
    playback_fx_job_id: u64,
    playback_source_generation: u64,
    playback_base_audio: Option<Arc<AudioBuffer>>,
    prepared_playback_fx_audio: Option<Arc<AudioBuffer>>,
    prepared_playback_fx_generation: u64,
    prepared_playback_fx_mode: Option<RateMode>,
    prepared_playback_fx_rate: f32,
    prepared_playback_fx_pitch: f32,
    // unified numeric control via DragValue; no string normalization
    pub pitch_semitones: f32,
    pub meter_db: f32,
    pub tabs: Vec<EditorTab>,
    waveform_scratch: WaveformScratch,
    pub active_tab: Option<usize>,
    workspace_view: WorkspaceView,
    effect_graph: EffectGraphState,
    pub meta_rx: Option<std::sync::mpsc::Receiver<meta::MetaUpdate>>,
    pub meta_pool: Option<meta::MetaPool>,
    pub meta_inflight: HashSet<PathBuf>,
    meta_sort_pending: bool,
    meta_sort_last_applied: Option<std::time::Instant>,
    list_meta_prefetch_cursor: usize,
    pub transcript_inflight: HashSet<PathBuf>,
    transcript_ai_inflight: HashSet<PathBuf>,
    pub show_transcript_window: bool,
    pub pending_transcript_seek: Option<(PathBuf, u64)>,
    transcript_ai_opt_in: bool,
    transcript_ai_model_dir: Option<PathBuf>,
    transcript_ai_available: bool,
    transcript_ai_state: Option<TranscriptAiRunState>,
    transcript_model_download_state: Option<TranscriptModelDownloadState>,
    transcript_ai_last_error: Option<String>,
    transcript_ai_cfg: TranscriptAiConfig,
    transcript_supported_languages: Vec<String>,
    transcript_supported_tasks: Vec<String>,
    music_ai_model_dir: Option<PathBuf>,
    music_ai_available: bool,
    music_ai_demucs_model_path: Option<PathBuf>,
    music_ai_state: Option<MusicAnalyzeRunState>,
    music_model_download_state: Option<MusicModelDownloadState>,
    music_ai_last_error: Option<String>,
    music_ai_inflight: HashSet<PathBuf>,
    music_preview_state: Option<MusicPreviewRunState>,
    music_preview_generation_counter: u64,
    music_preview_expected_generation: u64,
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
    external_load_queue: VecDeque<ExternalLoadQueueItem>,
    pending_external_restore: Option<PendingExternalRestore>,
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
    pub spectro_generation: HashMap<PathBuf, u64>,
    spectro_generation_counter: u64,
    pub spectro_cfg: SpectrogramConfig,
    pub spectro_tx: Option<std::sync::mpsc::Sender<SpectrogramJobMsg>>,
    pub spectro_rx: Option<std::sync::mpsc::Receiver<SpectrogramJobMsg>>,
    pub editor_viewport_tx: Option<std::sync::mpsc::Sender<EditorViewportJobMsg>>,
    pub editor_viewport_rx: Option<std::sync::mpsc::Receiver<EditorViewportJobMsg>>,
    editor_viewport_generation_counter: u64,
    pub editor_feature_cache: HashMap<EditorAnalysisKey, std::sync::Arc<EditorFeatureAnalysisData>>,
    pub editor_feature_inflight: HashSet<EditorAnalysisKey>,
    pub editor_feature_progress: HashMap<EditorAnalysisKey, AnalysisProgress>,
    pub editor_feature_cancel:
        HashMap<EditorAnalysisKey, std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub editor_feature_generation: HashMap<EditorAnalysisKey, u64>,
    editor_feature_generation_counter: u64,
    pub editor_feature_tx: Option<std::sync::mpsc::Sender<EditorFeatureAnalysisJobMsg>>,
    pub editor_feature_rx: Option<std::sync::mpsc::Receiver<EditorFeatureAnalysisJobMsg>>,
    pub scan_rx: Option<std::sync::mpsc::Receiver<ScanMessage>>,
    pub scan_in_progress: bool,
    pub scan_started_at: Option<std::time::Instant>,
    pub scan_found_count: usize,
    // dynamic row height for wave thumbnails (list view)
    pub wave_row_h: f32,
    pub list_columns: ListColumnConfig,
    list_art_textures: HashMap<PathBuf, egui::TextureHandle>,
    show_list_art_window: bool,
    list_art_window_path: Option<PathBuf>,
    list_art_window_texture: Option<egui::TextureHandle>,
    list_art_window_error: Option<String>,
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
    sort_loading_started_at: Option<std::time::Instant>,
    sort_loading_hold_until: Option<std::time::Instant>,
    sort_loading_last_ms: f32,
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
    invert_wave_zoom_wheel: bool,
    invert_shift_wheel_pan: bool,
    horizontal_zoom_anchor_mode: EditorHorizontalZoomAnchorMode,
    editor_pause_resume_mode: EditorPauseResumeMode,
    // processing mode
    mode: RateMode,
    // heavy processing state (overlay)
    processing: Option<ProcessingState>,
    processing_job_id: u64,
    // background full load for list preview
    list_preview_rx: Option<std::sync::mpsc::Receiver<ListPreviewResult>>,
    list_preview_job_id: u64,
    list_preview_job_epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
    list_preview_partial_ready: bool,
    list_preview_pending_path: Option<PathBuf>,
    list_play_pending: bool,
    list_preview_prefetch_tx: Option<std::sync::mpsc::Sender<ListPreviewPrefetchResult>>,
    list_preview_prefetch_rx: Option<std::sync::mpsc::Receiver<ListPreviewPrefetchResult>>,
    list_preview_prefetch_inflight: HashSet<PathBuf>,
    list_preview_cache: HashMap<PathBuf, ListPreviewCacheEntry>,
    list_preview_cache_order: VecDeque<PathBuf>,
    plugin_search_paths: Vec<PathBuf>,
    plugin_search_path_input: String,
    plugin_catalog: Vec<PluginCatalogEntry>,
    plugin_scan_state: Option<PluginScanState>,
    plugin_scan_error: Option<String>,
    plugin_probe_state: Option<PluginProbeState>,
    plugin_process_state: Option<PluginProcessState>,
    plugin_gui_state: Option<PluginGuiSessionState>,
    plugin_job_id: u64,
    plugin_temp_seq: u64,
    zoo_enabled: bool,
    zoo_walk_enabled: bool,
    zoo_voice_enabled: bool,
    zoo_use_bpm: bool,
    zoo_gif_path: Option<PathBuf>,
    zoo_voice_path: Option<PathBuf>,
    zoo_scale: f32,
    zoo_opacity: f32,
    zoo_speed: f32,
    zoo_flip_manual: bool,
    zoo_anim_clock: f32,
    zoo_pos_x: f32,
    zoo_dir: f32,
    zoo_last_tick: std::time::Instant,
    zoo_squish_until: Option<std::time::Instant>,
    zoo_last_error: Option<String>,
    zoo_texture_gen: u64,
    zoo_frames_raw: Vec<ZooFrameImage>,
    zoo_frames_tex: Vec<ZooFrameTexture>,
    zoo_voice_cache_path: Option<PathBuf>,
    zoo_voice_cache: Option<Arc<crate::audio::AudioBuffer>>,
    zoo_voice_audio: Option<AudioEngine>,
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
    show_transcription_settings: bool,
    show_first_save_prompt: bool,
    project_path: Option<PathBuf>,
    project_open_pending: Option<PathBuf>,
    project_open_state: Option<ProjectOpenState>,
    theme_mode: ThemeMode,
    item_bg_mode: ItemBgMode,
    show_rename_dialog: bool,
    rename_target: Option<PathBuf>,
    rename_input: String,
    rename_focus_next: bool,
    rename_error: Option<String>,
    show_batch_rename_dialog: bool,
    batch_rename_targets: Vec<PathBuf>,
    batch_rename_pattern: String,
    batch_rename_start: u32,
    batch_rename_pad: u32,
    batch_rename_error: Option<String>,
    saving_sources: Vec<PathBuf>,
    saving_virtual: Vec<(PathBuf, PathBuf)>,
    saving_format_targets: Vec<(PathBuf, PathBuf)>,
    saving_edit_sources: Vec<PathBuf>,
    saving_edit_annotations:
        HashMap<PathBuf, (Vec<crate::markers::MarkerEntry>, Option<(usize, usize)>)>,
    saving_mode: Option<SaveMode>,
    overwrite_undo_stack: Vec<Vec<(PathBuf, PathBuf)>>,

    // LUFS with Gain recompute support
    lufs_override: HashMap<PathBuf, f32>,
    lufs_recalc_deadline: HashMap<PathBuf, std::time::Instant>,
    lufs_rx2: Option<std::sync::mpsc::Receiver<(PathBuf, f32, f32)>>,
    lufs_worker_busy: bool,
    // Sample rate conversion (non-destructive)
    sample_rate_override: HashMap<PathBuf, u32>,
    sample_rate_probe_cache: HashMap<PathBuf, u32>,
    bit_depth_override: HashMap<PathBuf, crate::wave::WavBitDepth>,
    format_override: HashMap<PathBuf, String>,
    src_quality: SrcQuality,
    show_resample_dialog: bool,
    resample_targets: Vec<PathBuf>,
    resample_target_sr: u32,
    resample_error: Option<String>,
    bulk_resample_state: Option<BulkResampleState>,
    // leaving dirty editor confirmation
    leave_intent: Option<LeaveIntent>,
    show_leave_prompt: bool,
    pending_activate_path: Option<PathBuf>,
    pending_activate_kind: Option<PendingTabActivationKind>,
    pending_activate_ready: bool,
    // Heavy preview worker for Pitch/Stretch (mono) with path/generation guard
    heavy_preview_rx: Option<std::sync::mpsc::Receiver<HeavyPreviewMessage>>,
    heavy_preview_gen_counter: u64,
    heavy_preview_expected_gen: u64,
    heavy_preview_expected_path: Option<PathBuf>,
    heavy_preview_expected_tool: Option<ToolKind>,
    // Heavy overlay worker (per-channel preview for Pitch/Stretch) with generation guard
    heavy_overlay_rx: Option<std::sync::mpsc::Receiver<HeavyOverlayMessage>>,
    overlay_gen_counter: u64,
    overlay_expected_gen: u64,
    overlay_expected_path: Option<PathBuf>,
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
    #[cfg(feature = "kittest")]
    test_dialogs: TestDialogQueue,
}

impl WavesPreviewer {
    fn playback_is_playing_now(&self) -> bool {
        self.audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn playback_user_speed_for_mode(&self) -> f32 {
        if self.mode == RateMode::Speed {
            self.playback_rate
        } else {
            1.0
        }
    }

    fn playback_live_mapping_rate(&self) -> f32 {
        if self.mode == RateMode::Speed {
            self.playback_rate.max(0.25)
        } else {
            1.0
        }
    }

    fn playback_set_applied_mapping(&mut self, mode: RateMode, playback_rate: f32) {
        self.playback_session.applied_mode = mode;
        self.playback_session.applied_playback_rate = playback_rate.max(0.25);
    }

    fn next_playback_fx_job_id(&mut self) -> u64 {
        self.playback_fx_job_id = self.playback_fx_job_id.wrapping_add(1).max(1);
        self.playback_fx_job_id
    }

    fn playback_fx_required_for(mode: RateMode, playback_rate: f32, pitch_semitones: f32) -> bool {
        match mode {
            RateMode::Speed => false,
            RateMode::PitchShift => pitch_semitones.abs() > 0.0001,
            RateMode::TimeStretch => (playback_rate - 1.0).abs() > 0.0001,
        }
    }

    fn playback_mode_needs_fx_buffer(&self) -> bool {
        Self::playback_fx_required_for(self.mode, self.playback_rate, self.pitch_semitones)
    }

    fn clear_prepared_playback_fx(&mut self) {
        self.prepared_playback_fx_audio = None;
        self.prepared_playback_fx_generation = 0;
        self.prepared_playback_fx_mode = None;
        self.prepared_playback_fx_rate = 1.0;
        self.prepared_playback_fx_pitch = 0.0;
    }

    fn clear_pending_playback_fx_render(&mut self) {
        self.playback_fx_state = None;
    }

    fn clear_playback_fx_state(&mut self) {
        self.clear_pending_playback_fx_render();
        self.clear_prepared_playback_fx();
    }

    fn prepared_playback_fx_matches_current(&self) -> bool {
        self.prepared_playback_fx_audio.is_some()
            && self.prepared_playback_fx_generation == self.playback_source_generation
            && self.prepared_playback_fx_mode == Some(self.mode)
            && (self.prepared_playback_fx_rate - self.playback_rate).abs() <= 1.0e-6
            && (self.prepared_playback_fx_pitch - self.pitch_semitones).abs() <= 1.0e-6
    }

    fn pending_playback_fx_matches_current(&self) -> bool {
        self.playback_fx_state
            .as_ref()
            .map(|state| {
                state.source_generation == self.playback_source_generation
                    && state.source == self.playback_session.source
                    && state.mode == self.mode
                    && (state.playback_rate - self.playback_rate).abs() <= 1.0e-6
                    && (state.pitch_semitones - self.pitch_semitones).abs() <= 1.0e-6
            })
            .unwrap_or(false)
    }

    fn playback_rate_from_values(
        transport: PlaybackTransportKind,
        user_speed: f32,
        transport_sr: u32,
        out_sr: u32,
    ) -> f32 {
        match transport {
            PlaybackTransportKind::Buffer => user_speed.clamp(0.25, 4.0),
            PlaybackTransportKind::ExactStreamWav => {
                let src = transport_sr.max(1) as f32;
                let out = out_sr.max(1) as f32;
                let ratio = (src / out).clamp(0.25, 4.0);
                (user_speed.clamp(0.25, 4.0) * ratio).clamp(0.25, 4.0)
            }
        }
    }

    fn playback_source_time_for_output_pos(
        mode: RateMode,
        transport: PlaybackTransportKind,
        output_pos_f: f64,
        transport_sr: u32,
        out_sr: u32,
        playback_rate: f32,
    ) -> f64 {
        match transport {
            PlaybackTransportKind::ExactStreamWav => output_pos_f / transport_sr.max(1) as f64,
            PlaybackTransportKind::Buffer => {
                let out_sr = out_sr.max(1) as f64;
                let playback_rate = playback_rate.max(0.25) as f64;
                match mode {
                    RateMode::Speed | RateMode::TimeStretch => {
                        (output_pos_f / out_sr) * playback_rate
                    }
                    RateMode::PitchShift => output_pos_f / out_sr,
                }
            }
        }
    }

    fn playback_output_pos_for_source_time(
        mode: RateMode,
        transport: PlaybackTransportKind,
        source_time_sec: f64,
        transport_sr: u32,
        out_sr: u32,
        playback_rate: f32,
    ) -> usize {
        let frames = match transport {
            PlaybackTransportKind::ExactStreamWav => source_time_sec * transport_sr.max(1) as f64,
            PlaybackTransportKind::Buffer => {
                let out_sr = out_sr.max(1) as f64;
                let playback_rate = playback_rate.max(0.25) as f64;
                match mode {
                    RateMode::Speed | RateMode::TimeStretch => {
                        source_time_sec / playback_rate * out_sr
                    }
                    RateMode::PitchShift => source_time_sec * out_sr,
                }
            }
        };
        frames.max(0.0).round() as usize
    }

    fn playback_current_source_time_sec_with(
        &self,
        mode: RateMode,
        playback_rate: f32,
    ) -> Option<f64> {
        if matches!(self.playback_session.source, PlaybackSourceKind::None) {
            return None;
        }
        let pos_f = self
            .audio
            .shared
            .play_pos_f
            .load(std::sync::atomic::Ordering::Relaxed);
        Some(Self::playback_source_time_for_output_pos(
            mode,
            self.playback_session.transport,
            pos_f as f64,
            self.playback_session.transport_sr.max(1),
            self.audio.shared.out_sample_rate.max(1),
            playback_rate,
        ))
    }

    pub(super) fn playback_current_source_time_sec(&self) -> Option<f64> {
        self.playback_current_source_time_sec_with(
            self.playback_session.applied_mode,
            self.playback_session.applied_playback_rate,
        )
    }

    pub(super) fn playback_seek_to_source_time_with(
        &self,
        mode: RateMode,
        playback_rate: f32,
        source_time_sec: f64,
    ) {
        let pos = Self::playback_output_pos_for_source_time(
            mode,
            self.playback_session.transport,
            source_time_sec,
            self.playback_session.transport_sr.max(1),
            self.audio.shared.out_sample_rate.max(1),
            playback_rate,
        );
        self.audio.seek_to_sample(pos);
    }

    pub(super) fn playback_seek_to_source_time(&self, mode: RateMode, source_time_sec: f64) {
        self.playback_seek_to_source_time_with(mode, self.playback_rate, source_time_sec);
    }

    pub(super) fn playback_mark_source(
        &mut self,
        source: PlaybackSourceKind,
        transport: PlaybackTransportKind,
        transport_sr: u32,
    ) {
        let keep_last_start = matches!(
            (&self.playback_session.source, &source),
            (PlaybackSourceKind::EditorTab(prev), PlaybackSourceKind::EditorTab(next))
                if prev == next
        );
        if !keep_last_start {
            self.playback_session.last_play_start_display_sample = None;
        }
        self.playback_session.source = source;
        self.playback_session.transport = transport;
        self.playback_session.transport_sr = transport_sr.max(1);
        self.playback_session.user_speed = self.playback_user_speed_for_mode();
        self.playback_session.is_playing = self.playback_is_playing_now();
        self.playback_set_applied_mapping(self.mode, self.playback_live_mapping_rate());
        self.playback_session.dry_audio = match transport {
            PlaybackTransportKind::Buffer => self.audio.shared.samples.load_full(),
            PlaybackTransportKind::ExactStreamWav => None,
        };
        self.playback_base_audio = self.playback_session.dry_audio.clone();
        self.playback_source_generation = self.playback_source_generation.wrapping_add(1).max(1);
        self.clear_playback_fx_state();
        self.playback_session.last_applied_master_gain_db = f32::NAN;
        self.playback_session.last_applied_file_gain_db = f32::NAN;
        self.playback_refresh_rate_for_current_source();
    }

    pub(super) fn playback_mark_buffer_source(
        &mut self,
        source: PlaybackSourceKind,
        buffer_sr: u32,
    ) {
        self.playback_mark_source(source, PlaybackTransportKind::Buffer, buffer_sr);
    }

    pub(super) fn playback_mark_source_without_buffer(
        &mut self,
        source: PlaybackSourceKind,
        transport: PlaybackTransportKind,
        transport_sr: u32,
    ) {
        let keep_last_start = matches!(
            (&self.playback_session.source, &source),
            (PlaybackSourceKind::EditorTab(prev), PlaybackSourceKind::EditorTab(next))
                if prev == next
        );
        if !keep_last_start {
            self.playback_session.last_play_start_display_sample = None;
        }
        self.playback_session.source = source;
        self.playback_session.transport = transport;
        self.playback_session.transport_sr = transport_sr.max(1);
        self.playback_session.user_speed = self.playback_user_speed_for_mode();
        self.playback_session.is_playing = self.playback_is_playing_now();
        self.playback_set_applied_mapping(self.mode, self.playback_live_mapping_rate());
        self.playback_session.dry_audio = None;
        self.playback_base_audio = None;
        self.playback_source_generation = self.playback_source_generation.wrapping_add(1).max(1);
        self.clear_playback_fx_state();
        self.playback_session.last_applied_master_gain_db = f32::NAN;
        self.playback_session.last_applied_file_gain_db = f32::NAN;
        self.playback_refresh_rate_for_current_source();
    }

    pub(super) fn playback_refresh_rate_for_current_source(&mut self) {
        self.playback_session.user_speed = self.playback_user_speed_for_mode();
        self.playback_session.is_playing = self.playback_is_playing_now();
        let rate = Self::playback_rate_from_values(
            self.playback_session.transport,
            self.playback_session.user_speed,
            self.playback_session.transport_sr,
            self.audio.shared.out_sample_rate.max(1),
        );
        self.audio.set_rate(rate);
    }

    pub(super) fn playback_capture_editor_start_display_sample(&mut self) {
        let Some(tab_idx) = self
            .active_tab
            .filter(|_| self.is_editor_workspace_active())
        else {
            self.playback_session.last_play_start_display_sample = None;
            return;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            self.playback_session.last_play_start_display_sample = None;
            return;
        };
        let audio_len = self.audio.current_source_len();
        let mut audio_pos = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        if audio_len > 0 && audio_pos >= audio_len {
            audio_pos = 0;
        }
        self.playback_session.last_play_start_display_sample =
            Some(self.map_audio_to_display_sample(tab, audio_pos));
    }

    pub(super) fn playback_return_editor_to_last_start_if_needed(&mut self) {
        if self.editor_pause_resume_mode != EditorPauseResumeMode::ReturnToLastStart {
            return;
        }
        let Some(display_sample) = self.playback_session.last_play_start_display_sample else {
            return;
        };
        let tab_idx = match &self.playback_session.source {
            PlaybackSourceKind::EditorTab(path) => {
                self.tabs.iter().position(|tab| &tab.path == path)
            }
            _ => self
                .active_tab
                .filter(|_| self.is_editor_workspace_active()),
        };
        let Some(tab_idx) = tab_idx else {
            self.playback_session.last_play_start_display_sample = None;
            return;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            self.playback_session.last_play_start_display_sample = None;
            return;
        };
        let audio_sample = self.map_display_to_audio_sample(tab, display_sample);
        self.audio.seek_to_sample(audio_sample);
    }

    pub(super) fn playback_sync_state_snapshot(&mut self) {
        let was_playing = self.playback_session.is_playing;
        let is_playing = self.playback_is_playing_now();
        self.playback_session.is_playing = is_playing;
        if was_playing && !is_playing {
            let len = self.audio.current_source_len();
            let pos = self
                .audio
                .shared
                .play_pos
                .load(std::sync::atomic::Ordering::Relaxed);
            if len == 0 || pos >= len.saturating_sub(1) {
                self.playback_session.last_play_start_display_sample = None;
            }
        }
    }

    fn current_output_meter_db(&self) -> f32 {
        if !self.playback_is_playing_now() {
            return -80.0;
        }
        let callback_rms = self
            .audio
            .shared
            .meter_rms
            .load(std::sync::atomic::Ordering::Relaxed);
        let rms = if callback_rms > 0.0 {
            callback_rms
        } else {
            self.audio.current_source_meter_rms_fallback(1024)
        };
        if rms > 0.0 {
            (20.0 * rms.max(1.0e-8).log10()).clamp(-80.0, 6.0)
        } else {
            -80.0
        }
    }

    fn playback_stop_if_editor_source_invalidated(&mut self, path: &Path) {
        let should_stop = matches!(
            &self.playback_session.source,
            PlaybackSourceKind::EditorTab(src) if src.as_path() == path
        );
        if should_stop {
            self.audio.stop();
            self.playback_session.source = PlaybackSourceKind::None;
            self.playback_session.transport = PlaybackTransportKind::Buffer;
            self.playback_session.is_playing = false;
            self.playback_session.transport_sr = self.audio.shared.out_sample_rate.max(1);
            self.playback_set_applied_mapping(RateMode::Speed, 1.0);
            self.playback_session.dry_audio = None;
            self.playback_base_audio = None;
            self.clear_playback_fx_state();
            self.playback_session.last_applied_master_gain_db = f32::NAN;
            self.playback_session.last_applied_file_gain_db = f32::NAN;
            self.playback_session.last_play_start_display_sample = None;
        }
    }

    fn queue_tab_activation_with_kind(&mut self, path: PathBuf, kind: PendingTabActivationKind) {
        self.pending_activate_path = Some(path);
        self.pending_activate_kind = Some(kind);
        self.pending_activate_ready = false;
    }

    fn queue_tab_activation(&mut self, path: PathBuf) {
        self.queue_tab_activation_with_kind(path, PendingTabActivationKind::TabSwitch);
    }

    fn close_tab_at(&mut self, idx: usize, ctx: &egui::Context) {
        self.close_plugin_gui_for_tab(idx);
        let mut closing_path: Option<PathBuf> = None;
        if let Some(path) = self.tabs.get(idx).map(|t| t.path.clone()) {
            self.cancel_music_preview_if_path(path.as_path());
            closing_path = Some(path);
        }
        self.clear_preview_if_any(idx);
        if let Some(path) = closing_path.as_ref() {
            self.playback_stop_if_editor_source_invalidated(path.as_path());
        }
        self.cache_dirty_tab_at(idx);
        self.tabs.remove(idx);
        if !self.tabs.is_empty() {
            let new_active = if idx < self.tabs.len() {
                idx
            } else {
                self.tabs.len() - 1
            };
            self.active_tab = Some(new_active);
            if self.workspace_view == WorkspaceView::Editor {
                self.workspace_view = WorkspaceView::Editor;
            }
            if let Some(tab) = self.tabs.get(new_active) {
                let path = tab.path.clone();
                self.debug_mark_tab_switch_start(&path);
                self.queue_tab_activation(path);
            }
        } else {
            self.active_tab = None;
            if !self.effect_graph.workspace_open {
                self.workspace_view = WorkspaceView::List;
            }
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
            for (i, out_sample) in out.iter_mut().enumerate().take(len) {
                if let Some(&v) = ch.get(i) {
                    *out_sample += v;
                }
            }
        }
        for v in &mut out {
            *v /= chn;
        }
        out
    }

    pub(super) fn to_wave_resample_quality(quality: SrcQuality) -> crate::wave::ResampleQuality {
        match quality {
            SrcQuality::Fast => crate::wave::ResampleQuality::Fast,
            SrcQuality::Good => crate::wave::ResampleQuality::Good,
            SrcQuality::Best => crate::wave::ResampleQuality::Best,
        }
    }

    pub(super) fn resample_mono_with_quality(
        &self,
        mono: &[f32],
        in_sr: u32,
        out_sr: u32,
    ) -> Vec<f32> {
        crate::wave::resample_quality(
            mono,
            in_sr,
            out_sr,
            Self::to_wave_resample_quality(self.src_quality),
        )
    }
    fn editor_mixdown_mono(tab: &EditorTab) -> Vec<f32> {
        Self::mixdown_channels(&tab.ch_samples, tab.samples_len)
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
        requested.min(loop_len / 2)
    }

    fn loop_xfade_uses_through_zero(shape: LoopXfadeShape) -> bool {
        matches!(
            shape,
            LoopXfadeShape::LinearDip | LoopXfadeShape::EqualPowerDip
        )
    }

    fn loop_xfade_weights(shape: LoopXfadeShape, t: f32) -> (f32, f32) {
        let x = t.clamp(0.0, 1.0);
        match shape {
            LoopXfadeShape::Linear | LoopXfadeShape::LinearDip => (1.0 - x, x),
            LoopXfadeShape::EqualPower | LoopXfadeShape::EqualPowerDip => {
                let ang = core::f32::consts::FRAC_PI_2 * x;
                (ang.cos(), ang.sin())
            }
        }
    }

    fn loop_xfade_style_code(shape: LoopXfadeShape) -> u8 {
        match shape {
            LoopXfadeShape::Linear => 0,
            LoopXfadeShape::EqualPower => 1,
            LoopXfadeShape::LinearDip => 2,
            LoopXfadeShape::EqualPowerDip => 3,
        }
    }
    fn apply_loop_mode_for_tab(&self, tab: &EditorTab) {
        let audio_len = self.audio.current_source_len();
        let display_len = Self::editor_display_samples_len(tab);
        let map_display_count_to_audio = |display_count: usize| -> usize {
            if audio_len == 0 || display_len == 0 || audio_len == display_len {
                return display_count;
            }
            ((display_count as u128)
                .saturating_mul(audio_len as u128)
                .saturating_add((display_len / 2) as u128)
                / (display_len as u128)) as usize
        };
        match tab.loop_mode {
            LoopMode::Off => {
                self.audio.set_loop_enabled(false);
            }
            LoopMode::OnWhole => {
                self.audio.set_loop_enabled(true);
                if audio_len > 0 {
                    self.audio.set_loop_region(0, audio_len);
                    let requested = map_display_count_to_audio(tab.loop_xfade_samples);
                    let cf = Self::effective_loop_xfade_samples(0, audio_len, audio_len, requested);
                    self.audio
                        .set_loop_crossfade(cf, Self::loop_xfade_style_code(tab.loop_xfade_shape));
                }
            }
            LoopMode::Marker => {
                if let Some((a, b)) = tab.loop_region {
                    if a != b {
                        let (display_s, display_e) = if a <= b { (a, b) } else { (b, a) };
                        let mut audio_s = self.map_display_to_audio_sample(tab, display_s);
                        let mut audio_e = self.map_display_to_audio_sample(tab, display_e);
                        if audio_len > 0 {
                            audio_s = audio_s.min(audio_len.saturating_sub(1));
                            audio_e = audio_e.min(audio_len);
                            if audio_e <= audio_s {
                                audio_e = (audio_s + 1).min(audio_len);
                            }
                        }
                        if audio_len == 0 || audio_e <= audio_s {
                            self.audio.set_loop_enabled(false);
                            return;
                        }
                        self.audio.set_loop_enabled(true);
                        self.audio.set_loop_region(audio_s, audio_e);
                        let requested = map_display_count_to_audio(tab.loop_xfade_samples);
                        let cf = Self::effective_loop_xfade_samples(
                            audio_s, audio_e, audio_len, requested,
                        );
                        self.audio.set_loop_crossfade(
                            cf,
                            Self::loop_xfade_style_code(tab.loop_xfade_shape),
                        );
                        if self.debug.cfg.enabled {
                            eprintln!(
                                "loop_apply_map path={} display={}..{} audio={}..{} display_len={} audio_len={}",
                                tab.path.display(),
                                display_s,
                                display_e,
                                audio_s,
                                audio_e,
                                tab.samples_len,
                                audio_len
                            );
                        }
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

    fn update_markers_dirty(tab: &mut EditorTab) {
        tab.markers_dirty = tab.markers != tab.markers_saved;
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
            transcript_language: None,
            external: HashMap::new(),
            virtual_audio: None,
            virtual_state: None,
        };
        self.fill_external_for_item(&mut item);
        item
    }

    fn build_meta_from_audio(
        channels: &[Vec<f32>],
        sample_rate: u32,
        bits_per_sample: u16,
    ) -> FileMeta {
        let frames = channels.first().map(|c| c.len()).unwrap_or(0);
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
            sample_value_kind: if bits_per_sample == 32 {
                SampleValueKind::Float
            } else {
                SampleValueKind::Int
            },
            bit_rate_bps: None,
            duration_secs,
            total_frames: Some(frames as u64),
            rms_db: Some(rms_db),
            peak_db: Some(peak_db),
            lufs_i,
            bpm,
            created_at: None,
            modified_at: None,
            cover_art: None,
            thumb,
            marker_fracs: Vec::new(),
            loop_frac: None,
            decode_error: None,
        }
    }

    fn make_virtual_item(
        &mut self,
        display_name: String,
        audio: std::sync::Arc<crate::audio::AudioBuffer>,
        sample_rate: u32,
        bits_per_sample: u16,
        virtual_state: Option<VirtualState>,
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
            transcript_language: None,
            external: HashMap::new(),
            virtual_audio: Some(audio),
            virtual_state,
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

    fn editor_display_sample_rate(tab: &EditorTab, fallback_out_sr: u32) -> u32 {
        if tab.buffer_sample_rate > 0 {
            tab.buffer_sample_rate
        } else {
            fallback_out_sr.max(1)
        }
    }

    fn editor_uses_source_time_mapping(
        playback_source: &PlaybackSourceKind,
        tab: &EditorTab,
    ) -> bool {
        matches!(playback_source, PlaybackSourceKind::EditorTab(path) if path == &tab.path)
    }

    fn map_audio_to_display_sample_with(
        tab: &EditorTab,
        audio_pos: usize,
        audio_len: usize,
        playback_source: &PlaybackSourceKind,
        transport: PlaybackTransportKind,
        transport_sr: u32,
        out_sr: u32,
        mode: RateMode,
        playback_rate: f32,
    ) -> usize {
        let display_len = Self::editor_display_samples_len(tab);
        if Self::editor_uses_source_time_mapping(playback_source, tab) {
            let source_time_sec = Self::playback_source_time_for_output_pos(
                mode,
                transport,
                audio_pos as f64,
                transport_sr.max(1),
                out_sr.max(1),
                playback_rate,
            );
            let display_sr = Self::editor_display_sample_rate(tab, out_sr) as f64;
            let mapped = (source_time_sec * display_sr).round().max(0.0) as usize;
            return mapped.min(display_len);
        }
        if audio_len == 0 || display_len == 0 || audio_len == display_len {
            return audio_pos.min(display_len);
        }
        let mapped = ((audio_pos as u128)
            .saturating_mul(display_len as u128)
            .saturating_add((audio_len / 2) as u128)
            / (audio_len as u128)) as usize;
        mapped.min(display_len)
    }

    fn map_display_to_audio_sample_with(
        tab: &EditorTab,
        display_pos: usize,
        audio_len: usize,
        playback_source: &PlaybackSourceKind,
        transport: PlaybackTransportKind,
        transport_sr: u32,
        out_sr: u32,
        mode: RateMode,
        playback_rate: f32,
    ) -> usize {
        let display_len = Self::editor_display_samples_len(tab);
        if Self::editor_uses_source_time_mapping(playback_source, tab) {
            let display_sr = Self::editor_display_sample_rate(tab, out_sr).max(1) as f64;
            let source_time_sec = display_pos.min(display_len) as f64 / display_sr;
            let mapped = Self::playback_output_pos_for_source_time(
                mode,
                transport,
                source_time_sec,
                transport_sr.max(1),
                out_sr.max(1),
                playback_rate,
            );
            return mapped.min(audio_len.max(1));
        }
        if audio_len == 0 {
            return display_pos;
        }
        if display_len == 0 || audio_len == display_len {
            return display_pos.min(audio_len);
        }
        let mapped = ((display_pos as u128)
            .saturating_mul(audio_len as u128)
            .saturating_add((display_len / 2) as u128)
            / (display_len as u128)) as usize;
        mapped.min(audio_len)
    }

    fn map_audio_to_display_sample(&self, tab: &EditorTab, audio_pos: usize) -> usize {
        Self::map_audio_to_display_sample_with(
            tab,
            audio_pos,
            self.audio.current_source_len(),
            &self.playback_session.source,
            self.playback_session.transport,
            self.playback_session.transport_sr.max(1),
            self.audio.shared.out_sample_rate.max(1),
            self.playback_session.applied_mode,
            self.playback_session.applied_playback_rate,
        )
    }

    fn map_display_to_audio_sample(&self, tab: &EditorTab, display_pos: usize) -> usize {
        Self::map_display_to_audio_sample_with(
            tab,
            display_pos,
            self.audio.current_source_len(),
            &self.playback_session.source,
            self.playback_session.transport,
            self.playback_session.transport_sr.max(1),
            self.audio.shared.out_sample_rate.max(1),
            self.playback_session.applied_mode,
            self.playback_session.applied_playback_rate,
        )
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
                let edited = self.has_edits_for_paths(std::slice::from_ref(&item.path));
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
                let text = self
                    .effective_bits_for_path(&item.path)
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
        if let Err(err) =
            self.export_list_csv(&state.path, &state.ids, state.cols, &state.external_cols)
        {
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
        self.spectro_generation.clear();
        self.spectro_generation_counter = 0;
        self.editor_feature_cache.clear();
        self.editor_feature_inflight.clear();
        self.editor_feature_progress.clear();
        self.editor_feature_cancel.clear();
        self.editor_feature_generation.clear();
        self.editor_feature_generation_counter = 0;
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

    pub(super) fn open_new_window(&mut self) {
        // Spawn a fresh process of the current executable (no args) to open a new window.
        // This keeps state isolated and avoids blocking the current UI thread.
        let exe = match std::env::current_exe() {
            Ok(path) => path,
            Err(err) => {
                self.debug_log(format!("new window: current_exe failed: {err}"));
                return;
            }
        };
        let mut cmd = std::process::Command::new(exe);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Prevent a console flash when launching a GUI child.
            cmd.creation_flags(0x08000000);
        }
        if let Err(err) = cmd.spawn() {
            self.debug_log(format!("new window: spawn failed: {err}"));
        }
    }
}
// moved to types.rs

impl WavesPreviewer {
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
            samples_len_visual: tab.samples_len_visual,
            buffer_sample_rate: tab.buffer_sample_rate.max(1),
            waveform_minmax: tab.waveform_minmax.clone(),
            waveform_pyramid: tab.waveform_pyramid.clone(),
            view_offset: tab.view_offset,
            vertical_zoom: tab.vertical_zoom,
            vertical_view_center: tab.vertical_view_center,
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
            plugin_fx_draft: tab.plugin_fx_draft.clone(),
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

    fn push_editor_undo_state(&mut self, tab_idx: usize, state: EditorUndoState, clear_redo: bool) {
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
            tab.samples_len_visual = state.samples_len_visual;
            tab.loading = false;
            tab.loading_waveform_minmax.clear();
            tab.buffer_sample_rate = state.buffer_sample_rate.max(1);
            tab.waveform_minmax = state.waveform_minmax;
            tab.waveform_pyramid = state.waveform_pyramid;
            tab.view_offset = state.view_offset;
            tab.view_offset_exact = state.view_offset as f64;
            tab.vertical_zoom = state.vertical_zoom;
            tab.vertical_view_center = state.vertical_view_center;
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
            tab.plugin_fx_draft = state.plugin_fx_draft;
            tab.show_waveform_overlay = state.show_waveform_overlay;
            tab.markers = state.markers;
            tab.markers_committed = state.markers_committed;
            tab.markers_applied = state.markers_applied;
            tab.loop_region_applied = state.loop_region_applied;
            tab.loop_region_committed = state.loop_region_committed;
            tab.selection_anchor_sample = None;
            tab.dragging_marker = None;
            tab.preview_offset_samples = None;
            tab.last_amplitude_nav_rect = None;
            tab.last_amplitude_viewport_rect = None;
            tab.last_amplitude_nav_click_at = 0.0;
            tab.last_amplitude_nav_click_pos = None;
            tab.dirty = state.dirty;
            Self::editor_clamp_ranges(tab);
            Self::invalidate_editor_viewport_cache(tab);
            Self::update_markers_dirty(tab);
            Self::update_loop_markers_dirty(tab);
        }
        let Some((path, buffer_sample_rate, channels)) = self.tabs.get(tab_idx).map(|tab| {
            (
                tab.path.clone(),
                tab.buffer_sample_rate.max(1),
                tab.ch_samples.clone(),
            )
        }) else {
            return false;
        };
        self.audio.stop();
        self.audio.set_samples_channels(channels);
        self.playback_mark_buffer_source(PlaybackSourceKind::EditorTab(path), buffer_sample_rate);
        if let Some(tab) = self.tabs.get(tab_idx) {
            self.apply_loop_mode_for_tab(tab);
        }
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

    pub fn new_headless(startup: StartupConfig) -> Result<Self> {
        let audio = AudioEngine::new_for_test();
        Ok(Self::build_app(startup, audio))
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
        let frame_started = std::time::Instant::now();
        let had_ui_input = ctx.input(|i| {
            !i.events.is_empty()
                || i.pointer.any_pressed()
                || i.pointer.any_released()
                || i.pointer.delta() != egui::Vec2::ZERO
        });
        self.run_frame(ctx, frame_started, had_ui_input);
    }
}
