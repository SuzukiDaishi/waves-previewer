use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::types::{
    ChannelView, ChannelViewMode, EditorNote, EditorOtherSubView, EditorPrimaryView,
    EditorSpecSubView, FadeShape, FileMeta, LoopMode, LoopXfadeShape, MetadataSubView,
    MusicAnalysisDraft, MusicAnalysisResult, MusicAnalysisSourceKind, PluginFxChainDraft,
    PluginFxDraft, PluginFxSlot, PluginParamUiState, PluginPreviewEngine, SpectrogramConfig,
    SpectrogramScale, ToolKind, ToolState, TranscriptAiConfig, ViewMode,
};
use crate::markers::MarkerEntry;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectFile {
    pub version: u32,
    /// Identifies the document's lineage, so a conflict can distinguish "the
    /// same session, saved again by someone else" from "a different session
    /// now lives at this path". Issued on the first save; a Save As forks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Incremented on every successful save. Advisory only -- the authority
    /// on whether a document changed is a hash of its bytes (see
    /// `session_sync`), because an older build or an outside tool can write
    /// the file without touching this. It exists to *describe* a conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// RFC3339 UTC. UTC so two machines in different time zones compare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_at: Option<String>,
    /// Who saved it, for the conflict message. See `session_sync::local_display_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_by: Option<String>,
    #[serde(default)]
    pub assets: Vec<ProjectAsset>,
    #[serde(default)]
    pub transcripts: Vec<ProjectTranscriptDocument>,
    pub name: Option<String>,
    /// Source-path serialization policy for the whole session.
    ///
    /// Older sessions omit this field. They are inferred as relative only when
    /// every stored source path is relative; mixed/absolute legacy sessions
    /// are normalized to absolute paths on the next save.
    #[serde(default)]
    pub path_mode: Option<String>,
    pub base_dir: Option<String>,
    pub list: ProjectList,
    pub app: ProjectApp,
    pub spectrogram: ProjectSpectrogram,
    pub tabs: Vec<ProjectTab>,
    pub active_tab: Option<usize>,
    #[serde(default)]
    pub cached_edits: Vec<ProjectEdit>,
    /// The shared session's conversation. Held flat, with `parent` naming the
    /// reply target, because flat is the only shape that merges: two people
    /// appending to a nested tree fight over the same array positions, while
    /// two people appending to a flat list of uniquely-named entries do not.
    #[serde(default)]
    pub comments: Vec<ProjectComment>,
}

/// One message in a session's comment thread.
///
/// Unlike [`EditorNote`], which is a single author's private annotation in
/// their own editor buffer's sample space, a comment is written to be read by
/// somebody else on another machine. Everything here follows from that:
/// an author, a UTC timestamp, a reply target, and file/time references that
/// are stored inside `body` (see `crate::app::comments`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectComment {
    /// 128 random bits as hex. Never a counter: a shared session has more
    /// than one writer, and two people posting at once would claim the same
    /// number -- the failure the status/tag slugs and the content-addressed
    /// sidecars already exist to avoid.
    pub id: String,
    /// The comment this one replies to, or `None` for the root of a thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,

    /// The OS account name, trimmed and lowercased. The primary key for
    /// "is this mine".
    pub author_id: String,
    /// The machine name. A secondary key, shown only to tell two people who
    /// happen to share an `author_id` apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_host: Option<String>,
    /// The `display_name=` pref, when set. Display only -- identity is
    /// decided by `author_id`, so renaming yourself never disowns your posts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,

    /// RFC3339 UTC, so two machines in different time zones sort together.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<String>,
    /// Edit count. Merging picks the highest `(rev, edited_at)`, so this is
    /// what settles two edits to the same comment from two processes.
    #[serde(default)]
    pub rev: u32,

    /// Markdown subset. File and time references live in here as tokens
    /// rather than in a parallel array, so the text can never disagree with
    /// the references it shows.
    #[serde(default)]
    pub body: String,

    /// A tombstone. The row stays, because dropping it would let a colleague
    /// whose copy still has the comment merge it straight back in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,

    /// Roots only. A resolved thread collapses by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectAsset {
    pub id: String,
    pub revision: u64,
    pub item_path: String,
    pub backing: String,
    pub location: String,
    #[serde(default)]
    pub sample_rate: u32,
    #[serde(default)]
    pub channels: u16,
    #[serde(default)]
    pub bits_per_sample: u16,
    #[serde(default)]
    pub frame_count: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectTranscriptDocument {
    pub item_path: String,
    pub document: super::types::TranscriptDocument,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionPathMode {
    #[default]
    Absolute,
    Relative,
}

impl SessionPathMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absolute => "absolute",
            Self::Relative => "relative",
        }
    }

    pub fn from_project(project: &ProjectFile) -> Self {
        match project.path_mode.as_deref().map(str::trim) {
            Some(raw) if raw.eq_ignore_ascii_case("relative") => Self::Relative,
            Some(raw) if raw.eq_ignore_ascii_case("absolute") => Self::Absolute,
            _ => {
                if project_source_paths(project)
                    .into_iter()
                    .all(|raw| !Path::new(raw).is_absolute())
                {
                    Self::Relative
                } else {
                    Self::Absolute
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionPathRepair {
    /// Number of stored references that were redirected to a file/folder next
    /// to the session's new location. Duplicate logical references count
    /// separately because each serialized field is updated independently.
    pub relocated_references: usize,
    /// Absolute source references that were still missing after fallback.
    pub unresolved_references: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectList {
    pub root: Option<String>,
    pub files: Vec<String>,
    #[serde(default)]
    pub items: Vec<ProjectListItem>,
    #[serde(default)]
    pub sample_rate_overrides: Vec<ProjectSampleRateOverride>,
    #[serde(default)]
    pub bit_depth_overrides: Vec<ProjectBitDepthOverride>,
    #[serde(default)]
    pub format_overrides: Vec<ProjectFormatOverride>,
    #[serde(default)]
    pub virtual_items: Vec<ProjectVirtualItem>,
    #[serde(default)]
    pub transcript_languages: Vec<ProjectTranscriptLanguage>,
    /// The status palette this session was authored against. Carried in the
    /// document so a colleague opening a shared `.nwsess` sees the author's
    /// labels and colors rather than whatever their own prefs happen to hold.
    #[serde(default)]
    pub statuses: Vec<ProjectLabelDef>,
    /// Tag palette, same reason.
    #[serde(default)]
    pub tags: Vec<ProjectLabelDef>,
    /// Status stamped on rows newly added to this session, by id.
    #[serde(default)]
    pub default_status: Option<String>,
}

/// One status or tag definition as stored in a session.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectLabelDef {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub color: [u8; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectTranscriptLanguage {
    pub path: String,
    pub language: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectListItem {
    pub path: String,
    #[serde(default)]
    pub pending_gain_db: f32,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub editor_notes: Vec<EditorNote>,
    /// Workflow status id, or `None` for a row deliberately left unset.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectSampleRateOverride {
    pub path: String,
    pub sample_rate: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectBitDepthOverride {
    pub path: String,
    pub bit_depth: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectFormatOverride {
    pub path: String,
    pub format: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectVirtualItem {
    pub path: String,
    pub display_name: String,
    #[serde(default)]
    pub sample_rate: u32,
    #[serde(default)]
    pub channels: u16,
    #[serde(default)]
    pub bits_per_sample: u16,
    #[serde(default)]
    pub source: ProjectVirtualSource,
    #[serde(default)]
    pub op_chain: Vec<ProjectVirtualOp>,
    #[serde(default)]
    pub sidecar_audio: Option<String>,
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    pub asset_revision: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectVirtualSource {
    #[serde(default = "default_virtual_source_kind")]
    pub kind: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectVirtualOp {
    #[serde(default = "default_virtual_op_kind")]
    pub kind: String,
    #[serde(default)]
    pub start: Option<usize>,
    #[serde(default)]
    pub end: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectEdit {
    pub path: String,
    pub edited_audio: String,
    #[serde(default)]
    pub buffer_sample_rate: Option<u32>,
    pub dirty: bool,
    pub loop_region: Option<[usize; 2]>,
    pub loop_markers_saved: Option<[usize; 2]>,
    pub loop_markers_dirty: bool,
    pub markers: Vec<ProjectMarker>,
    #[serde(default)]
    pub regions: Vec<ProjectRegion>,
    pub markers_saved: Vec<ProjectMarker>,
    pub markers_dirty: bool,
    pub trim_range: Option<[usize; 2]>,
    pub loop_xfade_samples: usize,
    pub loop_xfade_shape: String,
    pub fade_in_range: Option<[usize; 2]>,
    pub fade_out_range: Option<[usize; 2]>,
    pub fade_in_shape: String,
    pub fade_out_shape: String,
    pub loop_mode: String,
    pub tool_state: ProjectToolState,
    pub active_tool: String,
    pub show_waveform_overlay: bool,
    #[serde(default)]
    pub bpm_enabled: bool,
    #[serde(default = "default_bpm_value")]
    pub bpm_value: f32,
    #[serde(default)]
    pub bpm_user_set: bool,
    #[serde(default)]
    pub bpm_offset_sec: f32,
    #[serde(default = "default_time_sig_numerator")]
    pub time_sig_numerator: u8,
    #[serde(default = "default_time_sig_denominator")]
    pub time_sig_denominator: u8,
    #[serde(default)]
    pub plugin_fx_draft: ProjectPluginFxDraft,
    #[serde(default)]
    pub plugin_fx_chain: ProjectPluginFxChainDraft,
    #[serde(default)]
    pub applied_effect_graph: Option<ProjectAppliedEffectGraph>,
    #[serde(default)]
    pub music_analysis: Option<ProjectMusicAnalysisDraft>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectApp {
    pub theme: String,
    pub sort_key: String,
    pub sort_dir: String,
    pub search_query: String,
    pub search_regex: bool,
    #[serde(default)]
    pub selected_path: Option<String>,
    pub list_columns: ProjectListColumns,
    /// Logical-point top-left position of the movable List Columns window.
    /// Optional so version-2 Sessions written by older builds remain valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_columns_window_pos: Option<[f32; 2]>,
    #[serde(default)]
    pub auto_play_list_nav: bool,
    #[serde(default)]
    pub export_policy: Option<ProjectExportPolicy>,
    #[serde(default)]
    pub external_state: Option<ProjectExternalState>,
    #[serde(default)]
    pub effect_graph_ui: Option<ProjectEffectGraphUi>,
    #[serde(default)]
    pub transcript_ai_config: Option<TranscriptAiConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectEffectGraphUi {
    #[serde(default)]
    pub tab_open: bool,
    #[serde(default)]
    pub active_template_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectAppliedEffectGraph {
    #[serde(default)]
    pub template_id: String,
    #[serde(default)]
    pub template_name: String,
    #[serde(default)]
    pub template_updated_at_unix_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectExportPolicy {
    #[serde(default = "default_export_save_mode")]
    pub save_mode: String,
    #[serde(default = "default_export_conflict")]
    pub conflict: String,
    #[serde(default = "default_export_backup_bak")]
    pub backup_bak: bool,
    #[serde(default = "default_export_srt")]
    pub export_srt: bool,
    #[serde(default = "default_export_name_template")]
    pub name_template: String,
    #[serde(default)]
    pub dest_folder: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectExternalState {
    #[serde(default)]
    pub sources: Vec<ProjectExternalSource>,
    #[serde(default)]
    pub active_source: Option<usize>,
    #[serde(default = "default_external_key_rule")]
    pub key_rule: String,
    #[serde(default = "default_external_match_input")]
    pub match_input: String,
    #[serde(default)]
    pub match_regex: String,
    #[serde(default)]
    pub match_replace: String,
    #[serde(default)]
    pub scope_regex: String,
    #[serde(default)]
    pub visible_columns: Vec<String>,
    #[serde(default)]
    pub show_unmatched: bool,
    #[serde(default)]
    pub key_column: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectExternalSource {
    pub path: String,
    #[serde(default)]
    pub sheet_name: Option<String>,
    #[serde(default = "default_external_has_header")]
    pub has_header: bool,
    #[serde(default)]
    pub header_row: Option<usize>,
    #[serde(default)]
    pub data_row: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectListColumns {
    #[serde(default)]
    pub edited: bool,
    #[serde(default)]
    pub cover_art: bool,
    #[serde(default)]
    pub type_badge: bool,
    pub file: bool,
    pub folder: bool,
    pub transcript: bool,
    #[serde(default)]
    pub transcript_language: bool,
    pub external: bool,
    #[serde(default = "default_status_column_visible")]
    pub status: bool,
    #[serde(default)]
    pub tags: bool,
    pub length: bool,
    pub ch: bool,
    pub sr: bool,
    pub bits: bool,
    #[serde(default)]
    pub bit_rate: bool,
    pub peak: bool,
    pub lufs: bool,
    #[serde(default)]
    pub dbtp: bool,
    #[serde(default)]
    pub lufs_s: bool,
    #[serde(default)]
    pub lufs_m: bool,
    #[serde(default)]
    pub bpm: bool,
    #[serde(default)]
    pub created_at: bool,
    #[serde(default)]
    pub modified_at: bool,
    pub gain: bool,
    pub wave: bool,
    /// The conversation column. Defaults visible like `note`: a session that
    /// predates it is still a session somebody may be discussing.
    #[serde(default = "default_comments_column_visible")]
    pub comments: bool,
    #[serde(default = "default_note_column_visible")]
    pub note: bool,
    #[serde(default)]
    pub silence_lead: bool,
    #[serde(default)]
    pub silence_tail: bool,
    #[serde(default)]
    pub edge_zero: bool,
    #[serde(default)]
    pub over_peak: bool,
    #[serde(default)]
    pub blank_pad: bool,
    /// Display order by column name; empty = default order (old files).
    #[serde(default)]
    pub order: Vec<String>,
    /// Per-project column widths by name; empty = keep the global widths.
    #[serde(default)]
    pub widths: Vec<(String, f32)>,
    /// Session-local visibility and labels for globally registered metadata
    /// columns. Their unified order is stored in `order` with the built-ins.
    #[serde(default)]
    pub metadata: Vec<ProjectMetadataColumn>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectMetadataColumn {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub visible: bool,
    #[serde(default = "default_metadata_column_width")]
    pub width: f32,
}

fn default_metadata_column_width() -> f32 {
    150.0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectSpectrogram {
    pub fft_size: usize,
    pub window: String,
    #[serde(default)]
    pub hop_size: Option<usize>,
    pub overlap: f32,
    pub max_frames: usize,
    pub scale: String,
    pub mel_scale: String,
    pub db_floor: f32,
    #[serde(default = "default_spectrogram_db_ceiling")]
    pub db_ceiling: f32,
    #[serde(default)]
    pub db_ref: Option<String>,
    pub max_freq_hz: f32,
    pub show_note_labels: bool,
}

fn default_spectrogram_db_ceiling() -> f32 {
    0.0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectTab {
    pub path: String,
    #[serde(default)]
    pub primary_view: Option<String>,
    #[serde(default)]
    pub spec_sub_view: Option<String>,
    #[serde(default)]
    pub other_sub_view: Option<String>,
    #[serde(default)]
    pub metadata_sub_view: Option<String>,
    pub view_mode: String,
    pub show_waveform_overlay: bool,
    pub channel_view: ProjectChannelView,
    pub active_tool: String,
    pub tool_state: ProjectToolState,
    #[serde(default)]
    pub bpm_enabled: bool,
    #[serde(default = "default_bpm_value")]
    pub bpm_value: f32,
    #[serde(default)]
    pub bpm_user_set: bool,
    #[serde(default)]
    pub bpm_offset_sec: f32,
    #[serde(default = "default_time_sig_numerator")]
    pub time_sig_numerator: u8,
    #[serde(default = "default_time_sig_denominator")]
    pub time_sig_denominator: u8,
    #[serde(default)]
    pub preview_tool: Option<String>,
    #[serde(default)]
    pub preview_audio: Option<String>,
    pub loop_mode: String,
    pub loop_region: Option<[usize; 2]>,
    pub loop_xfade_samples: usize,
    pub loop_xfade_shape: String,
    pub trim_range: Option<[usize; 2]>,
    pub selection: Option<[usize; 2]>,
    #[serde(default)]
    pub cursor_sample: Option<usize>,
    #[serde(default)]
    pub editor_note_position_mode: String,
    pub markers: Vec<ProjectMarker>,
    #[serde(default)]
    pub regions: Vec<ProjectRegion>,
    pub markers_dirty: bool,
    pub loop_markers_dirty: bool,
    pub fade_in_range: Option<[usize; 2]>,
    pub fade_out_range: Option<[usize; 2]>,
    pub fade_in_shape: String,
    pub fade_out_shape: String,
    pub view_offset: usize,
    pub samples_per_px: f32,
    #[serde(default = "default_vertical_zoom")]
    pub vertical_zoom: f32,
    #[serde(default = "default_vertical_view_center")]
    pub vertical_view_center: f32,
    pub dirty: bool,
    #[serde(default)]
    pub buffer_sample_rate: Option<u32>,
    pub edited_audio: Option<String>,
    #[serde(default)]
    pub plugin_fx_draft: ProjectPluginFxDraft,
    #[serde(default)]
    pub plugin_fx_chain: ProjectPluginFxChainDraft,
    #[serde(default)]
    pub music_analysis: Option<ProjectMusicAnalysisDraft>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectMusicAnalysisDraft {
    #[serde(default)]
    pub result: Option<MusicAnalysisResult>,
    #[serde(default = "default_music_analysis_visible")]
    pub show_beat: bool,
    #[serde(default = "default_music_analysis_visible")]
    pub show_downbeat: bool,
    #[serde(default = "default_music_analysis_visible")]
    pub show_section: bool,
    #[serde(default)]
    pub stems_dir_override: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub analysis_source_len: usize,
    #[serde(default)]
    pub analysis_source_kind: MusicAnalysisSourceKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectPluginFxDraft {
    #[serde(default)]
    pub plugin_key: Option<String>,
    #[serde(default)]
    pub plugin_name: String,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bypass: bool,
    #[serde(default)]
    pub filter: String,
    #[serde(default)]
    pub params: Vec<ProjectPluginParam>,
    #[serde(default)]
    pub state_blob_b64: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_backend_log: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectPluginFxChainDraft {
    #[serde(default)]
    pub slots: Vec<ProjectPluginFxSlot>,
    #[serde(default)]
    pub selected_slot_id: Option<u64>,
    #[serde(default)]
    pub bypass: bool,
    #[serde(default = "default_plugin_preview_engine")]
    pub preview_engine: String,
    #[serde(default = "default_render_ahead_ms")]
    pub render_ahead_ms: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectPluginFxSlot {
    pub id: u64,
    pub draft: ProjectPluginFxDraft,
    #[serde(default)]
    pub latency_samples: u32,
    #[serde(default)]
    pub failure_reason: Option<String>,
}

fn default_plugin_preview_engine() -> String {
    "render_ahead".to_string()
}

fn default_render_ahead_ms() -> u32 {
    150
}

fn default_vertical_zoom() -> f32 {
    1.0
}

fn default_vertical_view_center() -> f32 {
    0.0
}

fn default_music_analysis_visible() -> bool {
    true
}

fn default_note_column_visible() -> bool {
    true
}

fn default_comments_column_visible() -> bool {
    true
}

fn default_status_column_visible() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectPluginParam {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectToolState {
    pub fade_in_ms: f32,
    pub fade_out_ms: f32,
    pub gain_db: f32,
    pub normalize_target_db: f32,
    #[serde(default = "default_loudness_target_lufs")]
    pub loudness_target_lufs: f32,
    pub pitch_semitones: f32,
    pub stretch_rate: f32,
    #[serde(default = "default_speed_rate")]
    pub speed_rate: f32,
    #[serde(default = "default_warp_time_radius_ms")]
    pub warp_time_radius_ms: f32,
    #[serde(default = "default_warp_freq_radius_hz")]
    pub warp_freq_radius_hz: f32,
    #[serde(default = "default_loop_repeat")]
    pub loop_repeat: u32,
    #[serde(default = "default_noise_gate_threshold_db")]
    pub noise_gate_threshold_db: f32,
    #[serde(default = "default_noise_gate_attack_ms")]
    pub noise_gate_attack_ms: f32,
    #[serde(default = "default_noise_gate_release_ms")]
    pub noise_gate_release_ms: f32,
    #[serde(default = "default_eq_low_shelf_freq_hz")]
    pub eq_low_shelf_freq_hz: f32,
    #[serde(default)]
    pub eq_low_shelf_gain_db: f32,
    #[serde(default = "default_eq_mid_freq_hz")]
    pub eq_mid_freq_hz: f32,
    #[serde(default)]
    pub eq_mid_gain_db: f32,
    #[serde(default = "default_eq_mid_q")]
    pub eq_mid_q: f32,
    #[serde(default = "default_eq_high_shelf_freq_hz")]
    pub eq_high_shelf_freq_hz: f32,
    #[serde(default)]
    pub eq_high_shelf_gain_db: f32,
    #[serde(default = "default_compressor_threshold_db")]
    pub compressor_threshold_db: f32,
    #[serde(default = "default_compressor_ratio")]
    pub compressor_ratio: f32,
    #[serde(default = "default_compressor_attack_ms")]
    pub compressor_attack_ms: f32,
    #[serde(default = "default_compressor_release_ms")]
    pub compressor_release_ms: f32,
    #[serde(default)]
    pub compressor_makeup_db: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectChannelView {
    pub mode: String,
    pub selected: Vec<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectMarker {
    pub sample: usize,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectRegion {
    pub start: usize,
    pub end: usize,
    pub label: String,
}

pub fn project_region_to_entry(r: &ProjectRegion) -> crate::markers::RegionEntry {
    crate::markers::RegionEntry {
        start: r.start,
        end: r.end,
        label: r.label.clone(),
    }
}

pub fn region_entry_to_project(r: &crate::markers::RegionEntry) -> ProjectRegion {
    ProjectRegion {
        start: r.start,
        end: r.end,
        label: r.label.clone(),
    }
}

fn component_eq(a: std::path::Component<'_>, b: std::path::Component<'_>) -> bool {
    #[cfg(windows)]
    {
        use std::path::Component;
        match (a, b) {
            (Component::Normal(x), Component::Normal(y)) => x
                .to_string_lossy()
                .eq_ignore_ascii_case(&y.to_string_lossy()),
            _ => a == b,
        }
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn same_volume(path: &Path, base: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::Component;
        let p = path.components().find_map(|c| match c {
            Component::Prefix(prefix) => {
                Some(prefix.as_os_str().to_string_lossy().to_ascii_lowercase())
            }
            _ => None,
        });
        let b = base.components().find_map(|c| match c {
            Component::Prefix(prefix) => {
                Some(prefix.as_os_str().to_string_lossy().to_ascii_lowercase())
            }
            _ => None,
        });
        p == b
    }
    #[cfg(not(windows))]
    {
        let _ = (path, base);
        true
    }
}

fn diff_paths(path: &Path, base: &Path) -> Option<PathBuf> {
    if path.is_absolute() != base.is_absolute() {
        return None;
    }
    let path_components: Vec<_> = path.components().collect();
    let base_components: Vec<_> = base.components().collect();
    let mut common = 0usize;
    while common < path_components.len() && common < base_components.len() {
        if !component_eq(path_components[common], base_components[common]) {
            break;
        }
        common += 1;
    }
    let mut rel = PathBuf::new();
    for comp in &base_components[common..] {
        if matches!(comp, std::path::Component::Normal(_)) {
            rel.push("..");
        }
    }
    for comp in &path_components[common..] {
        match comp {
            std::path::Component::Normal(seg) => rel.push(seg),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => rel.push(".."),
            _ => {}
        }
    }
    if rel.as_os_str().is_empty() {
        Some(PathBuf::from("."))
    } else {
        Some(rel)
    }
}

/// Save path with session-file-relative preference:
/// - same volume => relative path (can include `..`)
/// - different volume/unresolvable => absolute path fallback
pub(super) fn rel_path(path: &Path, base: &Path) -> String {
    if !path.is_absolute() {
        return path.to_string_lossy().to_string();
    }
    let base_abs = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(base)
    };
    if !same_volume(path, &base_abs) {
        return path.to_string_lossy().to_string();
    }
    diff_paths(path, &base_abs)
        .unwrap_or_else(|| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

pub(super) fn can_store_relative(path: &Path, base: &Path) -> bool {
    if !path.is_absolute() {
        return true;
    }
    let base_abs = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(base)
    };
    same_volume(path, &base_abs) && diff_paths(path, &base_abs).is_some()
}

/// Serialize a user/source path according to the session-wide policy.
///
/// Session-owned sidecars intentionally continue to use [`rel_path`] directly:
/// they always live beside the `.nwsess` file and are not source-path policy
/// entries.
pub(super) fn session_path(path: &Path, base: &Path, mode: SessionPathMode) -> String {
    match mode {
        SessionPathMode::Relative => rel_path(path, base),
        SessionPathMode::Absolute => {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(path)
            };
            absolute.to_string_lossy().to_string()
        }
    }
}

/// Resolve saved path:
/// - absolute path => use as-is
/// - relative path => resolve from the session file's parent directory
pub(super) fn resolve_path(raw: &str, base: &Path) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

fn is_virtual_path(raw: &str) -> bool {
    raw.contains("://")
}

fn project_source_paths(project: &ProjectFile) -> Vec<&str> {
    let mut out = Vec::new();
    if let Some(raw) = project.list.root.as_deref() {
        out.push(raw);
    }
    out.extend(project.list.files.iter().map(String::as_str));
    out.extend(project.list.items.iter().map(|item| item.path.as_str()));
    out.extend(
        project
            .list
            .sample_rate_overrides
            .iter()
            .map(|item| item.path.as_str()),
    );
    out.extend(
        project
            .list
            .bit_depth_overrides
            .iter()
            .map(|item| item.path.as_str()),
    );
    out.extend(
        project
            .list
            .format_overrides
            .iter()
            .map(|item| item.path.as_str()),
    );
    out.extend(
        project
            .list
            .transcript_languages
            .iter()
            .map(|item| item.path.as_str()),
    );
    for item in &project.list.virtual_items {
        if !is_virtual_path(&item.path) {
            out.push(item.path.as_str());
        }
        if item.source.kind.eq_ignore_ascii_case("file") {
            if let Some(raw) = item.source.path.as_deref() {
                out.push(raw);
            }
        }
    }
    if let Some(raw) = project.app.selected_path.as_deref() {
        out.push(raw);
    }
    if let Some(policy) = project.app.export_policy.as_ref() {
        if let Some(raw) = policy.dest_folder.as_deref() {
            out.push(raw);
        }
    }
    if let Some(external) = project.app.external_state.as_ref() {
        out.extend(external.sources.iter().map(|source| source.path.as_str()));
    }
    for tab in &project.tabs {
        out.push(tab.path.as_str());
        if let Some(raw) = tab
            .music_analysis
            .as_ref()
            .and_then(|draft| draft.stems_dir_override.as_deref())
        {
            out.push(raw);
        }
    }
    for edit in &project.cached_edits {
        out.push(edit.path.as_str());
        if let Some(raw) = edit
            .music_analysis
            .as_ref()
            .and_then(|draft| draft.stems_dir_override.as_deref())
        {
            out.push(raw);
        }
    }
    out
}

fn saved_session_base(project: &ProjectFile, session_dir: &Path) -> PathBuf {
    project
        .base_dir
        .as_deref()
        .map(PathBuf::from)
        .map(|base| {
            if base.is_absolute() {
                base
            } else {
                session_dir.join(base)
            }
        })
        .unwrap_or_else(|| session_dir.to_path_buf())
}

/// The same location written the other way round: a mapped drive as its UNC
/// share, or a UNC share as a drive that is mapped to it here.
///
/// A session on a file server is opened by people whose machines mount the
/// same share differently -- `Z:\Proj` for one, `\\server\share\Proj` for
/// another. The absolute paths inside it then resolve for whoever saved it
/// and for nobody else. New sessions saved on a share default to relative
/// paths, which sidesteps this entirely; this is the repair for the ones
/// already written.
///
/// The rewriting is split from the drive lookup so the part that is easy to
/// get wrong -- the string surgery -- is testable on any platform, and only
/// the `WNetGetConnectionW` call is Windows-only.
/// `X:` plus the share it is mapped to, e.g. `("Z:", r"\\server\share")`.
type DriveMapping = (String, String);

/// A drive-letter path rewritten onto the share that drive is mapped to.
/// `Z:\Proj\a.wav` + (`Z:`, `\\server\share`) -> `\\server\share\Proj\a.wav`.
fn unc_form_of_mapped_drive(path: &str, mapping: &DriveMapping) -> Option<PathBuf> {
    let (drive, remote) = mapping;
    let drive = drive.trim_end_matches(['\\', '/']);
    if drive.len() < 2 {
        return None;
    }
    let head = path.get(..2)?;
    if !head.eq_ignore_ascii_case(drive) {
        return None;
    }
    let remote = remote.trim_end_matches(['\\', '/']);
    if remote.is_empty() {
        return None;
    }
    let suffix = path[2..].trim_start_matches(['\\', '/']);
    Some(PathBuf::from(if suffix.is_empty() {
        remote.to_string()
    } else {
        format!("{remote}\\{suffix}")
    }))
}

/// A UNC path rewritten onto a local drive letter mapped to that share.
/// `\\server\share\Proj\a.wav` + (`Z:`, `\\server\share`) -> `Z:\Proj\a.wav`.
///
/// Shares are case-insensitive and a mapping may be recorded in either case,
/// so the prefix match ignores case -- but the suffix is taken from the
/// original text, because the part below the share root may well be on a
/// case-sensitive server.
fn drive_form_of_unc(path: &str, mapping: &DriveMapping) -> Option<PathBuf> {
    let (drive, remote) = mapping;
    let drive = drive.trim_end_matches(['\\', '/']);
    let remote = remote.trim_end_matches(['\\', '/']);
    if drive.len() < 2 || remote.is_empty() {
        return None;
    }
    let lower_path = path.to_ascii_lowercase();
    let lower_remote = remote.to_ascii_lowercase();
    if !lower_path.starts_with(&lower_remote) {
        return None;
    }
    // The match must end at a path boundary: `\\srv\share` must not match
    // `\\srv\share2`.
    let rest = &path[remote.len()..];
    if !rest.is_empty() && !rest.starts_with('\\') && !rest.starts_with('/') {
        return None;
    }
    let suffix = rest.trim_start_matches(['\\', '/']);
    Some(PathBuf::from(if suffix.is_empty() {
        format!("{drive}\\")
    } else {
        format!("{drive}\\{suffix}")
    }))
}

/// Look up what each drive letter is mapped to. Windows only; elsewhere a
/// share is just a mount point and there is no second spelling to try.
#[cfg(windows)]
fn network_drive_mappings(only: Option<char>) -> Vec<DriveMapping> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;

    let letters: Vec<char> = match only {
        Some(letter) => vec![letter],
        None => (b'A'..=b'Z').map(|b| b as char).collect(),
    };
    let mut out = Vec::new();
    for letter in letters {
        let drive = format!("{letter}:");
        let wide: Vec<u16> = std::ffi::OsStr::new(&drive)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut buffer = vec![0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = unsafe { WNetGetConnectionW(wide.as_ptr(), buffer.as_mut_ptr(), &mut len) };
        if ok != 0 {
            continue;
        }
        let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let remote = String::from_utf16_lossy(&buffer[..end]);
        if remote.trim().is_empty() {
            continue;
        }
        out.push((drive, remote));
    }
    out
}

#[cfg(not(windows))]
fn network_drive_mappings(_only: Option<char>) -> Vec<DriveMapping> {
    Vec::new()
}

pub(crate) fn unc_drive_alternatives(path: &Path) -> Vec<PathBuf> {
    let text = path.to_string_lossy().to_string();
    let bytes = text.as_bytes();

    // `Z:\dir\file` -> the share `Z:` is mapped to.
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let letter = bytes[0] as char;
        return network_drive_mappings(Some(letter))
            .iter()
            .filter_map(|mapping| unc_form_of_mapped_drive(&text, mapping))
            .collect();
    }

    // `\\server\share\dir\file` -> whichever local drive is mapped to it.
    if text.starts_with("\\\\") && !text.starts_with("\\\\?\\") {
        return network_drive_mappings(None)
            .iter()
            .filter_map(|mapping| drive_form_of_unc(&text, mapping))
            .collect();
    }
    Vec::new()
}

fn repair_one_source_path(
    raw: &mut String,
    old_base: &Path,
    session_dir: &Path,
    report: &mut SessionPathRepair,
) {
    if raw.trim().is_empty() || is_virtual_path(raw) {
        return;
    }
    let stored = PathBuf::from(raw.as_str());
    if stored.is_absolute() {
        if stored.exists() {
            return;
        }
        if let Some(relative) = diff_paths(&stored, old_base) {
            let candidate = session_dir.join(relative);
            if candidate.exists() {
                *raw = session_path(&candidate, session_dir, SessionPathMode::Absolute);
                report.relocated_references = report.relocated_references.saturating_add(1);
                return;
            }
        }
        // Last resort: the same share, written the way this machine mounts
        // it. Whoever saved the session may have had it on a drive letter
        // this machine does not use, or vice versa.
        for candidate in unc_drive_alternatives(&stored) {
            if candidate.exists() {
                *raw = session_path(&candidate, session_dir, SessionPathMode::Absolute);
                report.relocated_references = report.relocated_references.saturating_add(1);
                return;
            }
        }
        report.unresolved_references = report.unresolved_references.saturating_add(1);
        return;
    }

    // Absolute-mode legacy sessions may still contain relative entries. Resolve
    // them as one policy, preferring the current session location and retaining
    // the original base only when the moved copy is absent.
    let current_candidate = session_dir.join(&stored);
    let original_candidate = old_base.join(&stored);
    let resolved = if current_candidate.exists() {
        current_candidate
    } else {
        original_candidate.clone()
    };
    if !resolved.exists() {
        report.unresolved_references = report.unresolved_references.saturating_add(1);
    } else if resolved != original_candidate || old_base != session_dir {
        report.relocated_references = report.relocated_references.saturating_add(1);
    }
    *raw = session_path(&resolved, session_dir, SessionPathMode::Absolute);
}

/// Repair source references in an absolute-path session after the `.nwsess`
/// file and its companion files have moved together.
///
/// Only source/user paths are touched. Session-owned sidecars remain relative
/// to the `.nwsess` file. Missing entries stay in the document so a handful of
/// unavailable files never prevents the remaining session from opening.
pub(super) fn repair_project_source_paths(
    project: &mut ProjectFile,
    session_path: &Path,
) -> SessionPathRepair {
    let mode = SessionPathMode::from_project(project);
    if mode != SessionPathMode::Absolute {
        return SessionPathRepair::default();
    }
    let session_dir = session_path.parent().unwrap_or_else(|| Path::new("."));
    let old_base = saved_session_base(project, session_dir);
    let mut report = SessionPathRepair::default();
    let mut repair = |raw: &mut String| {
        repair_one_source_path(raw, &old_base, session_dir, &mut report);
    };

    if let Some(raw) = project.list.root.as_mut() {
        repair(raw);
    }
    for raw in &mut project.list.files {
        repair(raw);
    }
    for item in &mut project.list.items {
        repair(&mut item.path);
    }
    for item in &mut project.list.sample_rate_overrides {
        repair(&mut item.path);
    }
    for item in &mut project.list.bit_depth_overrides {
        repair(&mut item.path);
    }
    for item in &mut project.list.format_overrides {
        repair(&mut item.path);
    }
    for item in &mut project.list.transcript_languages {
        repair(&mut item.path);
    }
    for item in &mut project.list.virtual_items {
        if !is_virtual_path(&item.path) {
            repair(&mut item.path);
        }
        if item.source.kind.eq_ignore_ascii_case("file") {
            if let Some(raw) = item.source.path.as_mut() {
                repair(raw);
            }
        }
    }
    if let Some(raw) = project.app.selected_path.as_mut() {
        repair(raw);
    }
    if let Some(policy) = project.app.export_policy.as_mut() {
        if let Some(raw) = policy.dest_folder.as_mut() {
            repair(raw);
        }
    }
    if let Some(external) = project.app.external_state.as_mut() {
        for source in &mut external.sources {
            repair(&mut source.path);
        }
    }
    for tab in &mut project.tabs {
        repair(&mut tab.path);
        if let Some(raw) = tab
            .music_analysis
            .as_mut()
            .and_then(|draft| draft.stems_dir_override.as_mut())
        {
            repair(raw);
        }
    }
    for edit in &mut project.cached_edits {
        repair(&mut edit.path);
        if let Some(raw) = edit
            .music_analysis
            .as_mut()
            .and_then(|draft| draft.stems_dir_override.as_mut())
        {
            repair(raw);
        }
    }

    project.path_mode = Some(mode.as_str().to_string());
    if report.relocated_references > 0 {
        project.base_dir = Some(session_dir.to_string_lossy().to_string());
    }
    report
}

fn project_sidecar_dir(path: &Path) -> PathBuf {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("nwsess");
    let ext = if ext.eq_ignore_ascii_case("nwproj") || ext.eq_ignore_ascii_case("nwsess") {
        ext
    } else {
        "nwsess"
    };
    path.with_extension(format!("{ext}.d"))
}

/// Where a session's own sidecar audio lives: `<name>.nwsess.d/data/`.
pub fn project_data_dir(path: &Path) -> PathBuf {
    project_sidecar_dir(path).join("data")
}

fn project_assets_dir(path: &Path) -> PathBuf {
    project_sidecar_dir(path).join("assets")
}

fn default_loop_repeat() -> u32 {
    2
}

fn default_speed_rate() -> f32 {
    1.0
}

fn default_warp_time_radius_ms() -> f32 {
    150.0
}

fn default_warp_freq_radius_hz() -> f32 {
    300.0
}

fn default_loudness_target_lufs() -> f32 {
    -14.0
}

fn default_noise_gate_threshold_db() -> f32 {
    -40.0
}

fn default_noise_gate_attack_ms() -> f32 {
    2.0
}

fn default_noise_gate_release_ms() -> f32 {
    100.0
}

fn default_eq_low_shelf_freq_hz() -> f32 {
    120.0
}

fn default_eq_mid_freq_hz() -> f32 {
    1000.0
}

fn default_eq_mid_q() -> f32 {
    1.0
}

fn default_eq_high_shelf_freq_hz() -> f32 {
    8000.0
}

fn default_compressor_threshold_db() -> f32 {
    -18.0
}

fn default_compressor_ratio() -> f32 {
    3.0
}

fn default_compressor_attack_ms() -> f32 {
    10.0
}

fn default_compressor_release_ms() -> f32 {
    150.0
}

fn default_bpm_value() -> f32 {
    0.0
}

fn default_time_sig_numerator() -> u8 {
    4
}

fn default_time_sig_denominator() -> u8 {
    4
}

fn default_external_key_rule() -> String {
    "file".to_string()
}

fn default_external_match_input() -> String {
    "file".to_string()
}

fn default_external_has_header() -> bool {
    true
}

fn default_virtual_source_kind() -> String {
    "file".to_string()
}

fn default_virtual_op_kind() -> String {
    "trim".to_string()
}

fn default_export_save_mode() -> String {
    "new_file".to_string()
}

fn default_export_conflict() -> String {
    "rename".to_string()
}

fn default_export_backup_bak() -> bool {
    true
}

fn default_export_srt() -> bool {
    false
}

fn default_export_name_template() -> String {
    "{name}{gain_suffix}".to_string()
}

pub fn serialize_project(project: &ProjectFile) -> Result<String> {
    toml::to_string_pretty(project).context("serialize session")
}

/// Hash the document with the conversation, and the save stamp, taken out.
///
/// This is what lets a reader tell "somebody commented" from "somebody
/// saved". Both change the file, but only the second is a reason to stand up
/// the reload warning -- and a colleague's comment arriving every few minutes
/// behind an amber "reload me" badge would make the feature unusable.
///
/// The stamp has to come out along with the comments: `revision`, `saved_at`
/// and `saved_by` move on *every* write, comment-only ones included, so
/// leaving them in would mean nothing ever compared equal.
///
/// Takes `&mut` and puts everything back rather than cloning, because a
/// session here can list a hundred thousand files and this runs on every
/// change the watch notices.
pub(crate) fn comment_free_fingerprint(
    project: &mut ProjectFile,
) -> Result<crate::app::session_sync::SessionFingerprint> {
    let comments = std::mem::take(&mut project.comments);
    let revision = project.revision.take();
    let saved_at = project.saved_at.take();
    let saved_by = project.saved_by.take();
    let text = serialize_project(project);
    project.comments = comments;
    project.revision = revision;
    project.saved_at = saved_at;
    project.saved_by = saved_by;
    Ok(crate::app::session_sync::SessionFingerprint::of_bytes(
        text?.as_bytes(),
    ))
}

pub fn deserialize_project(text: &str) -> Result<ProjectFile> {
    toml::from_str(text).context("parse session")
}

pub fn spectro_config_from_project(p: &ProjectSpectrogram) -> SpectrogramConfig {
    let window = match p.window.as_str() {
        "hann" => super::types::WindowFunction::Hann,
        _ => super::types::WindowFunction::BlackmanHarris,
    };
    let scale = match p.scale.as_str() {
        "linear" => SpectrogramScale::Linear,
        _ => SpectrogramScale::Log,
    };
    let mel_scale = match p.mel_scale.as_str() {
        "log" => SpectrogramScale::Log,
        _ => SpectrogramScale::Linear,
    };
    let fft = p.fft_size.max(2);
    let hop_size = p.hop_size.filter(|v| *v > 0).unwrap_or_else(|| {
        let overlap = if p.overlap.is_finite() {
            p.overlap.clamp(0.0, 0.95)
        } else {
            0.875
        };
        ((fft as f32) * (1.0 - overlap)).round().max(1.0) as usize
    });
    let overlap = (1.0 - (hop_size as f32 / fft as f32)).clamp(0.0, 0.95);
    SpectrogramConfig {
        fft_size: p.fft_size,
        window,
        hop_size,
        overlap,
        max_frames: p.max_frames,
        scale,
        mel_scale,
        db_floor: p.db_floor,
        db_ceiling: p.db_ceiling,
        db_ref: match p.db_ref.as_deref() {
            Some("max") => super::types::SpectrogramDbRef::MaxNormalized,
            _ => super::types::SpectrogramDbRef::Absolute,
        },
        max_freq_hz: p.max_freq_hz,
        show_note_labels: p.show_note_labels,
    }
}

pub fn project_spectrogram_from_cfg(cfg: &SpectrogramConfig) -> ProjectSpectrogram {
    let window = match cfg.window {
        super::types::WindowFunction::Hann => "hann",
        super::types::WindowFunction::BlackmanHarris => "blackman_harris",
    };
    let scale = match cfg.scale {
        SpectrogramScale::Linear => "linear",
        SpectrogramScale::Log => "log",
    };
    let mel_scale = match cfg.mel_scale {
        SpectrogramScale::Linear => "linear",
        SpectrogramScale::Log => "log",
    };
    ProjectSpectrogram {
        fft_size: cfg.fft_size,
        window: window.to_string(),
        hop_size: Some(cfg.hop_size.max(1)),
        overlap: cfg.overlap,
        max_frames: cfg.max_frames,
        scale: scale.to_string(),
        mel_scale: mel_scale.to_string(),
        db_floor: cfg.db_floor,
        db_ceiling: cfg.db_ceiling,
        db_ref: Some(
            match cfg.db_ref {
                super::types::SpectrogramDbRef::MaxNormalized => "max",
                super::types::SpectrogramDbRef::Absolute => "absolute",
            }
            .to_string(),
        ),
        max_freq_hz: cfg.max_freq_hz,
        show_note_labels: cfg.show_note_labels,
    }
}

pub fn project_tab_from_tab(
    tab: &super::types::EditorTab,
    base: &Path,
    path_mode: SessionPathMode,
    edited_audio: Option<PathBuf>,
    preview_audio: Option<PathBuf>,
    preview_tool: Option<String>,
) -> ProjectTab {
    ProjectTab {
        path: session_path(&tab.path, base, path_mode),
        primary_view: Some(project_primary_view_string(tab.primary_view)),
        spec_sub_view: Some(project_spec_sub_view_string(tab.spec_sub_view)),
        other_sub_view: Some(project_other_sub_view_string(tab.other_sub_view)),
        metadata_sub_view: Some(project_metadata_sub_view_string(tab.metadata_sub_view)),
        view_mode: format!("{:?}", tab.leaf_view_mode()),
        show_waveform_overlay: tab.show_waveform_overlay,
        channel_view: ProjectChannelView {
            mode: match tab.channel_view.mode {
                ChannelViewMode::Mixdown => "mixdown",
                ChannelViewMode::All => "all",
                ChannelViewMode::Custom => "custom",
            }
            .to_string(),
            selected: tab.channel_view.selected.clone(),
        },
        active_tool: format!("{:?}", tab.active_tool),
        tool_state: ProjectToolState {
            fade_in_ms: tab.tool_state.fade_in_ms,
            fade_out_ms: tab.tool_state.fade_out_ms,
            gain_db: tab.tool_state.gain_db,
            normalize_target_db: tab.tool_state.normalize_target_db,
            loudness_target_lufs: tab.tool_state.loudness_target_lufs,
            pitch_semitones: tab.tool_state.pitch_semitones,
            stretch_rate: tab.tool_state.stretch_rate,
            speed_rate: tab.tool_state.speed_rate,
            warp_time_radius_ms: tab.tool_state.warp_time_radius_ms,
            warp_freq_radius_hz: tab.tool_state.warp_freq_radius_hz,
            loop_repeat: tab.tool_state.loop_repeat,
            noise_gate_threshold_db: tab.tool_state.noise_gate_threshold_db,
            noise_gate_attack_ms: tab.tool_state.noise_gate_attack_ms,
            noise_gate_release_ms: tab.tool_state.noise_gate_release_ms,
            eq_low_shelf_freq_hz: tab.tool_state.eq_low_shelf_freq_hz,
            eq_low_shelf_gain_db: tab.tool_state.eq_low_shelf_gain_db,
            eq_mid_freq_hz: tab.tool_state.eq_mid_freq_hz,
            eq_mid_gain_db: tab.tool_state.eq_mid_gain_db,
            eq_mid_q: tab.tool_state.eq_mid_q,
            eq_high_shelf_freq_hz: tab.tool_state.eq_high_shelf_freq_hz,
            eq_high_shelf_gain_db: tab.tool_state.eq_high_shelf_gain_db,
            compressor_threshold_db: tab.tool_state.compressor_threshold_db,
            compressor_ratio: tab.tool_state.compressor_ratio,
            compressor_attack_ms: tab.tool_state.compressor_attack_ms,
            compressor_release_ms: tab.tool_state.compressor_release_ms,
            compressor_makeup_db: tab.tool_state.compressor_makeup_db,
        },
        bpm_enabled: tab.bpm_enabled,
        bpm_value: tab.bpm_value,
        bpm_user_set: tab.bpm_user_set,
        bpm_offset_sec: tab.bpm_offset_sec,
        time_sig_numerator: tab.time_sig_numerator,
        time_sig_denominator: tab.time_sig_denominator,
        preview_tool,
        preview_audio: preview_audio.map(|p| rel_path(&p, base)),
        loop_mode: format!("{:?}", tab.loop_mode),
        loop_region: tab.loop_region.map(|(a, b)| [a, b]),
        loop_xfade_samples: tab.loop_xfade_samples,
        loop_xfade_shape: match tab.loop_xfade_shape {
            LoopXfadeShape::Linear => "linear",
            LoopXfadeShape::EqualPower => "equal",
            LoopXfadeShape::LinearDip => "linear_dip",
            LoopXfadeShape::EqualPowerDip => "equal_dip",
        }
        .to_string(),
        trim_range: tab.trim_range.map(|(a, b)| [a, b]),
        selection: tab.selection.map(|(a, b)| [a, b]),
        cursor_sample: tab.preview_offset_samples,
        editor_note_position_mode: match tab.editor_note_position_mode {
            super::types::EditorNotePositionMode::Time => "time",
            super::types::EditorNotePositionMode::Beats => "beats",
        }
        .to_string(),
        markers: tab
            .markers
            .iter()
            .map(|m| ProjectMarker {
                sample: m.sample,
                label: m.label.clone(),
            })
            .collect(),
        regions: tab.regions.iter().map(region_entry_to_project).collect(),
        markers_dirty: tab.markers_dirty,
        loop_markers_dirty: tab.loop_markers_dirty,
        fade_in_range: tab.fade_in_range.map(|(a, b)| [a, b]),
        fade_out_range: tab.fade_out_range.map(|(a, b)| [a, b]),
        fade_in_shape: format!("{:?}", tab.fade_in_shape),
        fade_out_shape: format!("{:?}", tab.fade_out_shape),
        view_offset: tab.view_offset,
        samples_per_px: tab.samples_per_px,
        vertical_zoom: tab.vertical_zoom,
        vertical_view_center: tab.vertical_view_center,
        dirty: tab.dirty,
        buffer_sample_rate: Some(tab.buffer_sample_rate.max(1)),
        edited_audio: edited_audio.map(|p| rel_path(&p, base)),
        plugin_fx_draft: project_plugin_fx_draft_from_draft(&tab.plugin_fx_draft),
        plugin_fx_chain: project_plugin_fx_chain_from_draft(&tab.plugin_fx_chain),
        music_analysis: project_music_analysis_from_draft(
            &tab.music_analysis_draft,
            base,
            path_mode,
        ),
    }
}

pub fn project_music_analysis_from_draft(
    draft: &MusicAnalysisDraft,
    base: &Path,
    path_mode: SessionPathMode,
) -> Option<ProjectMusicAnalysisDraft> {
    if draft.result.is_none()
        && draft.stems_dir_override.is_none()
        && draft.last_error.is_none()
        && draft.show_beat
        && draft.show_downbeat
        && draft.show_section
    {
        return None;
    }
    Some(ProjectMusicAnalysisDraft {
        result: draft.result.clone(),
        show_beat: draft.show_beat,
        show_downbeat: draft.show_downbeat,
        show_section: draft.show_section,
        stems_dir_override: draft
            .stems_dir_override
            .as_ref()
            .map(|path| session_path(path, base, path_mode)),
        last_error: draft.last_error.clone(),
        analysis_source_len: draft.analysis_source_len,
        analysis_source_kind: draft.analysis_source_kind,
    })
}

pub fn project_music_analysis_to_draft(
    draft: &ProjectMusicAnalysisDraft,
    base: &Path,
) -> MusicAnalysisDraft {
    let mut out = MusicAnalysisDraft::default();
    out.result = draft.result.clone();
    out.show_beat = draft.show_beat;
    out.show_downbeat = draft.show_downbeat;
    out.show_section = draft.show_section;
    out.stems_dir_override = draft
        .stems_dir_override
        .as_deref()
        .map(|raw| resolve_path(raw, base));
    out.last_error = draft.last_error.clone();
    out.analysis_source_len = draft.analysis_source_len;
    out.analysis_source_kind = draft.analysis_source_kind;
    out
}

pub fn project_plugin_fx_draft_from_draft(draft: &PluginFxDraft) -> ProjectPluginFxDraft {
    ProjectPluginFxDraft {
        plugin_key: draft.plugin_key.clone(),
        plugin_name: draft.plugin_name.clone(),
        backend: draft.backend.map(|b| match b {
            crate::plugin::PluginHostBackend::Generic => "generic".to_string(),
            crate::plugin::PluginHostBackend::NativeVst3 => "native_vst3".to_string(),
            crate::plugin::PluginHostBackend::NativeClap => "native_clap".to_string(),
        }),
        enabled: draft.enabled,
        bypass: draft.bypass,
        filter: draft.filter.clone(),
        params: draft
            .params
            .iter()
            .map(|p| ProjectPluginParam {
                id: p.id.clone(),
                name: p.name.clone(),
                normalized: p.normalized,
                default_normalized: p.default_normalized,
                min: p.min,
                max: p.max,
                unit: p.unit.clone(),
            })
            .collect(),
        state_blob_b64: draft
            .state_blob
            .as_ref()
            .map(|bytes| base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes)),
        last_error: draft.last_error.clone(),
        last_backend_log: draft.last_backend_log.clone(),
    }
}

pub fn project_plugin_fx_draft_to_draft(draft: &ProjectPluginFxDraft) -> PluginFxDraft {
    PluginFxDraft {
        plugin_key: draft.plugin_key.clone(),
        plugin_name: draft.plugin_name.clone(),
        backend: draft.backend.as_deref().and_then(|raw| {
            match raw.trim().to_ascii_lowercase().as_str() {
                "generic" => Some(crate::plugin::PluginHostBackend::Generic),
                "native_vst3" => Some(crate::plugin::PluginHostBackend::NativeVst3),
                "native_clap" => Some(crate::plugin::PluginHostBackend::NativeClap),
                _ => None,
            }
        }),
        gui_capabilities: crate::plugin::GuiCapabilities::default(),
        gui_status: crate::plugin::GuiSessionStatus::Closed,
        enabled: draft.enabled,
        bypass: draft.bypass,
        filter: draft.filter.clone(),
        params: draft
            .params
            .iter()
            .map(|p| PluginParamUiState {
                id: p.id.clone(),
                name: p.name.clone(),
                normalized: p.normalized.clamp(0.0, 1.0),
                default_normalized: p.default_normalized.clamp(0.0, 1.0),
                min: p.min,
                max: p.max,
                unit: p.unit.clone(),
            })
            .collect(),
        state_blob: draft.state_blob_b64.as_ref().and_then(|raw| {
            base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(raw.as_bytes())
                .ok()
        }),
        last_error: draft.last_error.clone(),
        last_backend_note: None,
        last_backend_log: draft.last_backend_log.clone(),
        // A/B slots and auto-preview are session-transient.
        ab_alt: None,
        ab_active_b: false,
        auto_preview: false,
    }
}

pub fn project_plugin_fx_chain_from_draft(chain: &PluginFxChainDraft) -> ProjectPluginFxChainDraft {
    ProjectPluginFxChainDraft {
        slots: chain
            .slots
            .iter()
            .map(|slot| ProjectPluginFxSlot {
                id: slot.id,
                draft: project_plugin_fx_draft_from_draft(&slot.draft),
                latency_samples: slot.latency_samples,
                failure_reason: slot.failure_reason.clone(),
            })
            .collect(),
        selected_slot_id: chain.selected_slot_id,
        bypass: chain.bypass,
        preview_engine: match chain.preview_engine {
            PluginPreviewEngine::RenderAhead => "render_ahead",
            PluginPreviewEngine::Offline => "offline",
        }
        .to_string(),
        render_ahead_ms: chain.render_ahead_ms.clamp(100, 500),
    }
}

pub fn project_plugin_fx_chain_to_draft(
    stored: &ProjectPluginFxChainDraft,
    legacy: &ProjectPluginFxDraft,
) -> PluginFxChainDraft {
    if stored.slots.is_empty() {
        return PluginFxChainDraft::from_legacy(&project_plugin_fx_draft_to_draft(legacy));
    }
    let mut chain = PluginFxChainDraft {
        slots: stored
            .slots
            .iter()
            .map(|slot| PluginFxSlot {
                id: slot.id,
                draft: project_plugin_fx_draft_to_draft(&slot.draft),
                latency_samples: slot.latency_samples,
                failure_reason: slot.failure_reason.clone(),
            })
            .collect(),
        selected_slot_id: stored.selected_slot_id,
        bypass: stored.bypass,
        preview_engine: if stored.preview_engine.eq_ignore_ascii_case("offline") {
            PluginPreviewEngine::Offline
        } else {
            PluginPreviewEngine::RenderAhead
        },
        render_ahead_ms: stored.render_ahead_ms.clamp(100, 500),
        latency_samples: 0,
        underrun_count: 0,
    };
    chain.latency_samples = chain.total_latency_samples();
    chain
}

pub fn project_marker_to_entry(m: &ProjectMarker) -> MarkerEntry {
    MarkerEntry {
        sample: m.sample,
        label: m.label.clone(),
    }
}

pub fn marker_entry_to_project(m: &MarkerEntry) -> ProjectMarker {
    ProjectMarker {
        sample: m.sample,
        label: m.label.clone(),
    }
}

pub fn project_tool_state_to_tool_state(t: &ProjectToolState) -> ToolState {
    ToolState {
        fade_in_ms: t.fade_in_ms,
        fade_out_ms: t.fade_out_ms,
        gain_db: t.gain_db,
        normalize_target_db: t.normalize_target_db,
        loudness_target_lufs: t.loudness_target_lufs,
        pitch_semitones: t.pitch_semitones,
        stretch_rate: t.stretch_rate,
        speed_rate: if t.speed_rate > 0.0 {
            t.speed_rate
        } else {
            1.0
        },
        warp_time_radius_ms: if t.warp_time_radius_ms > 0.0 {
            t.warp_time_radius_ms
        } else {
            150.0
        },
        warp_freq_radius_hz: if t.warp_freq_radius_hz > 0.0 {
            t.warp_freq_radius_hz
        } else {
            300.0
        },
        // Brush/de-click params are session-transient; projects load defaults.
        brush_cut_db: 24.0,
        brush_time_radius_ms: 60.0,
        brush_freq_radius_hz: 200.0,
        declick_sensitivity: 0.5,
        denoise_reduction_db: 12.0,
        denoise_strength: 2.0,
        loop_repeat: t.loop_repeat.max(2),
        noise_gate_threshold_db: t.noise_gate_threshold_db,
        noise_gate_attack_ms: t.noise_gate_attack_ms,
        noise_gate_release_ms: t.noise_gate_release_ms,
        eq_low_shelf_freq_hz: t.eq_low_shelf_freq_hz,
        eq_low_shelf_gain_db: t.eq_low_shelf_gain_db,
        eq_mid_freq_hz: t.eq_mid_freq_hz,
        eq_mid_gain_db: t.eq_mid_gain_db,
        eq_mid_q: t.eq_mid_q,
        eq_high_shelf_freq_hz: t.eq_high_shelf_freq_hz,
        eq_high_shelf_gain_db: t.eq_high_shelf_gain_db,
        compressor_threshold_db: t.compressor_threshold_db,
        compressor_ratio: t.compressor_ratio,
        compressor_attack_ms: t.compressor_attack_ms,
        compressor_release_ms: t.compressor_release_ms,
        compressor_makeup_db: t.compressor_makeup_db,
        insert_silence_ms: 1000.0,
        invert_smooth_boundaries: false,
        declip_sensitivity: 0.5,
        dehum_hz: 50.0,
        dehum_harmonics: 8,
        dehum_q: 30.0,
        dehum_depth_db: 40.0,
    }
}

pub fn project_channel_view_to_channel_view(p: &ProjectChannelView) -> ChannelView {
    let mode = match p.mode.as_str() {
        "all" => ChannelViewMode::All,
        "custom" => ChannelViewMode::Custom,
        _ => ChannelViewMode::Mixdown,
    };
    ChannelView {
        mode,
        selected: p.selected.clone(),
    }
}

pub fn tool_kind_from_str(s: &str) -> ToolKind {
    match s {
        "Markers" => ToolKind::Markers,
        "EditorNote" => ToolKind::EditorNote,
        "Trim" => ToolKind::Trim,
        "Fade" => ToolKind::Fade,
        "PitchShift" => ToolKind::PitchShift,
        "TimeStretch" => ToolKind::TimeStretch,
        "Speed" => ToolKind::Speed,
        "SpectralWarp" => ToolKind::SpectralWarp,
        "Gain" => ToolKind::Gain,
        "Normalize" => ToolKind::Normalize,
        "Loudness" => ToolKind::Loudness,
        "Reverse" => ToolKind::Reverse,
        "InvertPolarity" => ToolKind::InvertPolarity,
        "DcOffset" => ToolKind::DcOffset,
        "InsertSilence" => ToolKind::InsertSilence,
        "Pencil" => ToolKind::Pencil,
        "DeClick" => ToolKind::DeClick,
        "DeClip" => ToolKind::DeClip,
        "DeHum" => ToolKind::DeHum,
        "DeNoise" => ToolKind::DeNoise,
        "SpectralBrush" => ToolKind::SpectralBrush,
        "PluginFx" => ToolKind::PluginFx,
        "ChannelRouting" => ToolKind::ChannelRouting,
        _ => ToolKind::LoopEdit,
    }
}

pub fn view_mode_from_str(s: &str) -> ViewMode {
    match s {
        "Log" => ViewMode::Log,
        "Spectrogram" => ViewMode::Spectrogram,
        "Mel" => ViewMode::Mel,
        "Tempogram" => ViewMode::Tempogram,
        "Chromagram" => ViewMode::Chromagram,
        _ => ViewMode::Waveform,
    }
}

pub fn project_primary_view_string(view: EditorPrimaryView) -> String {
    match view {
        EditorPrimaryView::Wave => "wave",
        EditorPrimaryView::Spec => "spec",
        EditorPrimaryView::Other => "other",
        EditorPrimaryView::Metadata => "metadata",
    }
    .to_string()
}

pub fn project_metadata_sub_view_string(view: MetadataSubView) -> String {
    match view {
        MetadataSubView::Structure => "structure",
        MetadataSubView::Hex => "hex",
    }
    .to_string()
}

pub fn metadata_sub_view_from_project(value: Option<&str>) -> MetadataSubView {
    match value.map(|raw| raw.trim().to_ascii_lowercase()) {
        Some(value) if value == "hex" => MetadataSubView::Hex,
        _ => MetadataSubView::Structure,
    }
}

pub fn project_spec_sub_view_string(view: EditorSpecSubView) -> String {
    match view {
        EditorSpecSubView::Spec => "spec",
        EditorSpecSubView::Log => "log",
        EditorSpecSubView::Mel => "mel",
    }
    .to_string()
}

pub fn project_other_sub_view_string(view: EditorOtherSubView) -> String {
    match view {
        EditorOtherSubView::World => "world",
        EditorOtherSubView::Tempogram => "tempogram",
        EditorOtherSubView::Chromagram => "chromagram",
    }
    .to_string()
}

pub fn primary_view_from_project(
    primary: Option<&str>,
    spec_sub_view: Option<&str>,
    other_sub_view: Option<&str>,
    legacy_view_mode: &str,
) -> (EditorPrimaryView, EditorSpecSubView, EditorOtherSubView) {
    let legacy_mode = view_mode_from_str(legacy_view_mode);
    let primary_view = match primary.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v == "spec" => EditorPrimaryView::Spec,
        Some(v) if v == "other" => EditorPrimaryView::Other,
        Some(v) if v == "wave" => EditorPrimaryView::Wave,
        Some(v) if v == "metadata" => EditorPrimaryView::Metadata,
        _ => EditorPrimaryView::from_mode(legacy_mode),
    };
    let spec_view = match spec_sub_view.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v == "log" => EditorSpecSubView::Log,
        Some(v) if v == "mel" => EditorSpecSubView::Mel,
        Some(v) if v == "spec" => EditorSpecSubView::Spec,
        _ => EditorSpecSubView::from_mode(legacy_mode),
    };
    let other_view = match other_sub_view.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v == "chromagram" => EditorOtherSubView::Chromagram,
        Some(v) if v == "world" || v == "f0" => EditorOtherSubView::World,
        Some(v) if v == "tempogram" => EditorOtherSubView::Tempogram,
        _ => EditorOtherSubView::from_mode(legacy_mode),
    };
    (primary_view, spec_view, other_view)
}

pub fn loop_mode_from_str(s: &str) -> LoopMode {
    match s {
        "OnWhole" => LoopMode::OnWhole,
        "Marker" => LoopMode::Marker,
        _ => LoopMode::Off,
    }
}

pub fn loop_shape_from_str(s: &str) -> LoopXfadeShape {
    match s {
        "linear_dip" => LoopXfadeShape::LinearDip,
        "equal_dip" => LoopXfadeShape::EqualPowerDip,
        "equal" => LoopXfadeShape::EqualPower,
        _ => LoopXfadeShape::Linear,
    }
}

pub fn fade_shape_from_str(s: &str) -> FadeShape {
    match s {
        "Linear" => FadeShape::Linear,
        "EqualPower" => FadeShape::EqualPower,
        "Cosine" => FadeShape::Cosine,
        "Quadratic" => FadeShape::Quadratic,
        "Cubic" => FadeShape::Cubic,
        _ => FadeShape::SCurve,
    }
}

pub fn missing_file_meta(path: &Path) -> FileMeta {
    FileMeta {
        audio_track_absent: false,
        audio_track_unsupported: false,
        channels: 0,
        sample_rate: 0,
        bits_per_sample: 0,
        sample_value_kind: super::types::SampleValueKind::Unknown,
        bit_rate_bps: None,
        duration_secs: None,
        total_frames: None,
        rms_db: None,
        peak_db: None,
        peak_db_estimate: false,
        lufs_i: None,
        lufs_m_max: None,
        lufs_s_max: None,
        true_peak_db: None,
        bpm: None,
        silence_lead_ms: None,
        silence_tail_ms: None,
        edge_abs: None,
        blank_pad: None,
        created_at: None,
        modified_at: None,
        cover_art: None,
        thumb: Vec::new(),
        marker_fracs: Vec::new(),
        loop_frac: None,
        decode_error: Some(format!("Missing: {}", path.display())),
    }
}

/// Destination path a sidecar WAV will be written to, without writing it.
/// Lets the session-save planner reference sidecars in the document while
/// deferring the actual encode to a worker thread.
/// Where a sidecar with this content hash lives.
///
/// Named after its contents rather than its index in the tab list. The index
/// scheme (`tab_0000.wav`) made every writer of a shared session target the
/// same handful of names, so two people saving the same session overwrote
/// each other's audio. Sessions written by older builds still reference the
/// old names and keep resolving -- nothing reads these paths except through
/// the string stored in the document -- and migrate on their next save.
pub fn sidecar_audio_dst(project_path: &Path, content_hash: &str) -> PathBuf {
    project_data_dir(project_path).join(format!("{content_hash}.wav"))
}

/// Where a managed asset revision lives, for a session written before
/// content hashes were part of the name. Reading still resolves these --
/// nothing looks a sidecar up except through the string in the document --
/// so this exists only for the tests and the migration path.
#[cfg(test)]
pub fn legacy_asset_audio_dst(
    project_path: &Path,
    id: crate::audio_asset::AudioAssetId,
    revision: crate::audio_asset::AssetRevision,
) -> PathBuf {
    project_assets_dir(project_path)
        .join(id.to_hex())
        .join(format!("{}.wav", revision.0.max(1)))
}

/// Where a managed asset revision lives.
///
/// `AssetRevision` is a plain counter, and the id it hangs off is stored in
/// the session -- so two people editing the same virtual item in a shared
/// session both produce revision 2 and, under the old name, wrote different
/// audio to the same file. The content hash makes their two revision 2s two
/// different files.
pub fn asset_audio_dst(
    project_path: &Path,
    id: crate::audio_asset::AudioAssetId,
    revision: crate::audio_asset::AssetRevision,
    content_hash: &str,
) -> PathBuf {
    project_assets_dir(project_path)
        .join(id.to_hex())
        .join(format!("{}-{content_hash}.wav", revision.0.max(1)))
}

pub fn load_sidecar_audio(
    project_path: &Path,
    raw_path: &str,
) -> Result<(Vec<Vec<f32>>, u32, PathBuf)> {
    let base = project_path.parent().unwrap_or_else(|| Path::new("."));
    let sidecar = resolve_path(raw_path, base);
    let (chans, sr) = crate::wave::decode_wav_multi(&sidecar)
        .with_context(|| format!("decode edited audio: {}", sidecar.display()))?;
    Ok((chans, sr, sidecar))
}

pub fn describe_missing(path: &Path) -> String {
    format!("Missing source: {}", path.display())
}

impl super::WavesPreviewer {
    pub(super) fn close_project(&mut self) {
        self.audio.stop();
        // Detach in-flight preview receivers before tab indices and paths are
        // reused by the next session. Worker threads may still finish, but
        // their generation-bound results can no longer be installed into the
        // newly opened project.
        self.clear_heavy_preview_state();
        self.clear_heavy_overlay_state();
        self.tabs.clear();
        self.active_tab = None;
        self.workspace_view = super::types::WorkspaceView::List;
        self.edited_cache.clear();
        self.effect_graph = super::types::EffectGraphState::default();
        self.pending_activate_path = None;
        self.pending_editor_autoplay_path = None;
        self.pending_editor_playback_handoff = None;
        self.pending_activate_kind = None;
        self.leave_intent = None;
        self.show_leave_prompt = false;
        self.replace_with_files(&[]);
        self.selected = None;
        self.selected_multi.clear();
        self.select_anchor = None;
        self.sample_rate_override.clear();
        self.bit_depth_override.clear();
        self.format_override.clear();
        self.list_undo_stack.clear();
        self.list_redo_stack.clear();
        self.overwrite_undo_stack.clear();
        self.project_path = None;
        self.session_path_mode = SessionPathMode::Absolute;
        self.session_disk_fingerprint = None;
        self.session_id = None;
        self.session_revision = None;
        self.session_paths_repaired = false;
        self.stop_session_watch();
        self.session_conflict = None;
        self.session_changed_on_disk = None;
        self.session_reload_prompt = false;
        self.cancel_baseline_scan();
        self.session_store_load = None;
        self.session_file_changes = None;
        self.comments.clear();
        self.mark_comment_index_dirty();
        self.session_comment_free_fingerprint = None;
        self.comment_outbox.clear();
        self.comment_write = None;
        self.comment_write_failures = 0;
        self.comment_retry_after = None;
        self.comment_pull = None;
        self.session_changed_pending = None;
        self.comment_draft.clear();
        self.comment_reply_to = None;
        self.comment_editing_id = None;
        self.comment_edit_draft.clear();
        self.comment_search.clear();
        self.comment_collapsed.clear();
        self.pending_comment_jump = None;
        self.comment_mention_open = false;
        self.comment_mention_index = 0;
        self.comment_ambiguous_authors.clear();
        self.comment_reads.clear();
        self.comment_reads_request = None;
        self.comment_unread_shown.clear();
        self.show_session_changes_window = false;
        self.show_session_history_window = false;
        self.session_history_entries.clear();
        self.session_history_request = None;
        self.session_history_pending = None;
        self.baseline_notes.clear();
        self.baseline_tracked.clear();
        self.list_columns_window_pos = self.list_columns_window_global_pos;
    }

    /// `exists` is parallel to `raw_paths` and comes from the parse worker
    /// (`ParsedSession::file_exists`). Statting here instead would put one
    /// blocking syscall per row on the UI thread; a session listing a few
    /// thousand files on a network share spent seconds in this loop alone.
    /// A short slice (older callers, or a truncated document) is treated as
    /// "unknown, assume present" -- the metadata worker flags a file it
    /// cannot decode anyway.
    pub(super) fn reset_list_from_project(
        &mut self,
        raw_paths: &[String],
        base_dir: &Path,
        exists: &[bool],
    ) {
        self.root = None;
        self.note_files_membership_changed();
        // Freeing a large previous list inline stalls the UI for hundreds of
        // ms (the same reason folder loads use this); hand it to a
        // low-priority thread instead.
        self.drop_list_contents_in_background();
        self.files.clear();
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.transcript_ai_inflight.clear();
        self.sample_rate_override.clear();
        self.bit_depth_override.clear();
        self.format_override.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
        self.reset_all_feature_analysis_state();
        self.clear_scan_state();
        self.list_max_duration_secs = 0.0;
        for (idx, raw) in raw_paths.iter().enumerate() {
            let p = resolve_path(raw, base_dir);
            let mut item = self.make_media_item(p.clone());
            if !exists.get(idx).copied().unwrap_or(true) {
                item.status = super::types::MediaStatus::DecodeFailed(describe_missing(&p));
                item.meta = Some(Box::new(missing_file_meta(&p)));
            }
            let id = item.id;
            self.path_index.insert(p, id);
            self.item_index.insert(id, self.items.len());
            self.items.push(item);
        }
        self.refresh_root_locality();
        self.ensure_meta_pool();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Mapped drive <-> UNC rewriting -----------------------------
    //
    // Runs on every platform: the lookup is Windows-only, but the string
    // surgery is where the mistakes live, and a session written by a
    // colleague on a mapped drive has to resolve here.

    fn mapping() -> (String, String) {
        ("Z:".to_string(), r"\\server\share".to_string())
    }

    #[test]
    fn a_mapped_drive_path_rewrites_onto_its_share() {
        assert_eq!(
            unc_form_of_mapped_drive(r"Z:\Proj\a.wav", &mapping()),
            Some(PathBuf::from(r"\\server\share\Proj\a.wav"))
        );
        // The drive letter's case is not meaningful on Windows.
        assert_eq!(
            unc_form_of_mapped_drive(r"z:\Proj\a.wav", &mapping()),
            Some(PathBuf::from(r"\\server\share\Proj\a.wav"))
        );
        // A different drive is not this mapping's business.
        assert_eq!(unc_form_of_mapped_drive(r"Y:\Proj\a.wav", &mapping()), None);
    }

    #[test]
    fn the_share_root_itself_rewrites_without_a_trailing_separator() {
        assert_eq!(
            unc_form_of_mapped_drive(r"Z:\", &mapping()),
            Some(PathBuf::from(r"\\server\share"))
        );
    }

    #[test]
    fn a_unc_path_rewrites_onto_the_drive_mapped_to_it() {
        assert_eq!(
            drive_form_of_unc(r"\\server\share\Proj\a.wav", &mapping()),
            Some(PathBuf::from(r"Z:\Proj\a.wav"))
        );
        // Shares are case-insensitive; the mapping may be recorded either way.
        assert_eq!(
            drive_form_of_unc(r"\\SERVER\Share\Proj\a.wav", &mapping()),
            Some(PathBuf::from(r"Z:\Proj\a.wav"))
        );
    }

    #[test]
    fn a_share_whose_name_merely_starts_the_same_does_not_match() {
        // `\\server\share2` must not be rewritten as if it were under
        // `\\server\share` -- that would silently redirect to another share.
        assert_eq!(
            drive_form_of_unc(r"\\server\share2\Proj\a.wav", &mapping()),
            None
        );
        assert_eq!(drive_form_of_unc(r"\\server\other\a.wav", &mapping()), None);
    }

    #[test]
    fn the_part_below_the_share_keeps_its_original_case() {
        // The server may well be case-sensitive below the share root.
        assert_eq!(
            drive_form_of_unc(r"\\SERVER\SHARE\MixedCase\A.WAV", &mapping()),
            Some(PathBuf::from(r"Z:\MixedCase\A.WAV"))
        );
    }

    #[test]
    fn an_empty_or_malformed_mapping_rewrites_nothing() {
        let empty = ("Z:".to_string(), String::new());
        assert_eq!(unc_form_of_mapped_drive(r"Z:\a.wav", &empty), None);
        assert_eq!(drive_form_of_unc(r"\\server\share\a.wav", &empty), None);
        let no_drive = (String::new(), r"\\server\share".to_string());
        assert_eq!(drive_form_of_unc(r"\\server\share\a.wav", &no_drive), None);
    }

    #[test]
    fn an_ordinary_local_path_has_no_alternative_spelling() {
        assert!(unc_drive_alternatives(Path::new(r"C:\Users\me\a.wav")).is_empty());
        assert!(unc_drive_alternatives(Path::new("/home/me/a.wav")).is_empty());
        // A verbatim path is left alone: rewriting one changes its meaning.
        assert!(unc_drive_alternatives(Path::new(r"\\?\C:\a.wav")).is_empty());
    }

    #[test]
    fn rel_path_prefers_relative_for_same_volume() {
        let base = std::env::temp_dir().join("nw_rel_base").join("a").join("b");
        let target = base
            .parent()
            .unwrap_or(base.as_path())
            .join("audio")
            .join("tone.wav");
        let rel = rel_path(&target, &base);
        assert!(
            !PathBuf::from(&rel).is_absolute(),
            "expected relative path, got: {rel}"
        );
    }

    #[test]
    fn resolve_path_keeps_absolute_and_joins_relative() {
        let base = std::env::temp_dir().join("nw_resolve_base");
        let abs = if cfg!(windows) {
            PathBuf::from(r"C:\tmp\nw_abs_test.wav")
        } else {
            PathBuf::from("/tmp/nw_abs_test.wav")
        };
        let rel = "foo/bar.wav";
        assert_eq!(resolve_path(abs.to_string_lossy().as_ref(), &base), abs);
        assert_eq!(resolve_path(rel, &base), base.join(rel));
    }

    #[test]
    fn project_list_deserialize_keeps_virtual_items_backward_compatible() {
        let raw = r#"
root = ""
files = []
"#;
        let list: ProjectList =
            toml::from_str(raw).expect("deserialize ProjectList without virtual_items");
        assert!(list.virtual_items.is_empty());
    }

    #[test]
    fn project_list_virtual_item_roundtrip() {
        let list = ProjectList {
            root: None,
            files: vec!["a.wav".to_string()],
            items: Vec::new(),
            sample_rate_overrides: Vec::new(),
            bit_depth_overrides: Vec::new(),
            format_overrides: Vec::new(),
            transcript_languages: Vec::new(),
            statuses: Vec::new(),
            tags: Vec::new(),
            default_status: None,
            virtual_items: vec![ProjectVirtualItem {
                path: "virtual://trim_0001".to_string(),
                display_name: "trim_0001".to_string(),
                sample_rate: 48_000,
                channels: 2,
                bits_per_sample: 24,
                source: ProjectVirtualSource {
                    kind: "file".to_string(),
                    path: Some("a.wav".to_string()),
                },
                op_chain: vec![ProjectVirtualOp {
                    kind: "trim".to_string(),
                    start: Some(100),
                    end: Some(1000),
                }],
                sidecar_audio: Some("data/virtual_0001.wav".to_string()),
                asset_id: None,
                asset_revision: None,
            }],
        };
        let text = toml::to_string(&list).expect("serialize ProjectList");
        let restored: ProjectList = toml::from_str(&text).expect("deserialize ProjectList");
        assert_eq!(restored.virtual_items.len(), 1);
        let v = &restored.virtual_items[0];
        assert_eq!(v.path, "virtual://trim_0001");
        assert_eq!(v.sample_rate, 48_000);
        assert_eq!(v.channels, 2);
        assert_eq!(v.bits_per_sample, 24);
        assert_eq!(v.source.kind, "file");
        assert_eq!(v.op_chain.len(), 1);
    }

    #[test]
    fn project_list_transcript_language_roundtrip() {
        let list = ProjectList {
            root: None,
            files: vec!["a.wav".to_string(), "b.wav".to_string()],
            items: Vec::new(),
            sample_rate_overrides: Vec::new(),
            bit_depth_overrides: Vec::new(),
            format_overrides: Vec::new(),
            statuses: Vec::new(),
            tags: Vec::new(),
            default_status: None,
            transcript_languages: vec![
                ProjectTranscriptLanguage {
                    path: "a.wav".to_string(),
                    language: "ja".to_string(),
                },
                ProjectTranscriptLanguage {
                    path: "b.wav".to_string(),
                    language: "en".to_string(),
                },
            ],
            virtual_items: Vec::new(),
        };
        let text = toml::to_string(&list).expect("serialize ProjectList");
        let restored: ProjectList = toml::from_str(&text).expect("deserialize ProjectList");
        assert_eq!(restored.transcript_languages.len(), 2);
        assert_eq!(restored.transcript_languages[0].path, "a.wav");
        assert_eq!(restored.transcript_languages[0].language, "ja");
        assert_eq!(restored.transcript_languages[1].path, "b.wav");
        assert_eq!(restored.transcript_languages[1].language, "en");
    }

    #[test]
    fn project_list_columns_optional_flags_default_false_when_missing() {
        let raw = r#"
edited = true
file = true
folder = true
transcript = false
external = true
length = true
ch = true
sr = true
bits = true
peak = true
lufs = true
gain = true
wave = true
"#;
        let cols: ProjectListColumns = toml::from_str(raw).expect("deserialize ProjectListColumns");
        assert!(!cols.cover_art);
        assert!(!cols.type_badge);
    }

    #[test]
    fn plugin_fx_draft_roundtrip() {
        let src = PluginFxDraft {
            plugin_key: Some("C:\\Plugins\\Demo.vst3".to_string()),
            plugin_name: "Demo".to_string(),
            backend: Some(crate::plugin::PluginHostBackend::NativeVst3),
            gui_capabilities: crate::plugin::GuiCapabilities::default(),
            gui_status: crate::plugin::GuiSessionStatus::Closed,
            enabled: true,
            bypass: false,
            filter: "gain".to_string(),
            params: vec![PluginParamUiState {
                id: "mix".to_string(),
                name: "Mix".to_string(),
                normalized: 0.75,
                default_normalized: 1.0,
                min: 0.0,
                max: 1.0,
                unit: String::new(),
            }],
            state_blob: Some(vec![1, 2, 3, 4, 5]),
            last_error: None,
            last_backend_log: Some("Probe: NativeVst3 params=1".to_string()),
            last_backend_note: None,
            ab_alt: None,
            ab_active_b: false,
            auto_preview: false,
        };
        let project = project_plugin_fx_draft_from_draft(&src);
        let restored = project_plugin_fx_draft_to_draft(&project);
        assert_eq!(src, restored);
    }

    #[test]
    fn plugin_fx_draft_supports_native_clap_backend() {
        let src = PluginFxDraft {
            plugin_key: Some("C:\\Plugins\\Demo.clap".to_string()),
            plugin_name: "Demo Clap".to_string(),
            backend: Some(crate::plugin::PluginHostBackend::NativeClap),
            gui_capabilities: crate::plugin::GuiCapabilities {
                supports_native_gui: false,
                supports_param_feedback: true,
                supports_state_sync: false,
            },
            gui_status: crate::plugin::GuiSessionStatus::Closed,
            enabled: true,
            bypass: false,
            filter: String::new(),
            params: Vec::new(),
            state_blob: None,
            last_error: None,
            last_backend_log: None,
            last_backend_note: None,
            ab_alt: None,
            ab_active_b: false,
            auto_preview: false,
        };
        let project = project_plugin_fx_draft_from_draft(&src);
        assert_eq!(project.backend.as_deref(), Some("native_clap"));
        let restored = project_plugin_fx_draft_to_draft(&project);
        assert_eq!(
            restored.backend,
            Some(crate::plugin::PluginHostBackend::NativeClap)
        );
    }

    #[test]
    fn tool_kind_parser_supports_pluginfx_and_loudness() {
        assert_eq!(tool_kind_from_str("PluginFx"), ToolKind::PluginFx);
        assert_eq!(tool_kind_from_str("Loudness"), ToolKind::Loudness);
    }

    #[test]
    fn spectrogram_project_hop_migration_keeps_legacy_overlap_compatible() {
        let legacy = ProjectSpectrogram {
            fft_size: 2048,
            window: "hann".to_string(),
            hop_size: None,
            overlap: 0.875,
            max_frames: 1024,
            scale: "log".to_string(),
            mel_scale: "linear".to_string(),
            db_floor: -120.0,
            db_ceiling: 0.0,
            db_ref: None,
            max_freq_hz: 0.0,
            show_note_labels: false,
        };
        let cfg_legacy = spectro_config_from_project(&legacy);
        assert_eq!(cfg_legacy.hop_size, 256);
        assert!((cfg_legacy.overlap - 0.875).abs() < 1e-4);

        let modern = ProjectSpectrogram {
            fft_size: 2048,
            window: "hann".to_string(),
            hop_size: Some(128),
            overlap: 0.0,
            max_frames: 1024,
            scale: "log".to_string(),
            mel_scale: "linear".to_string(),
            db_floor: -120.0,
            db_ceiling: 0.0,
            db_ref: None,
            max_freq_hz: 0.0,
            show_note_labels: false,
        };
        let cfg_modern = spectro_config_from_project(&modern);
        assert_eq!(cfg_modern.hop_size, 128);
        assert!((cfg_modern.overlap - 0.9375).abs() < 1e-4);
    }

    #[test]
    fn primary_view_from_project_migrates_legacy_leaf_modes() {
        assert_eq!(
            primary_view_from_project(None, None, None, "Waveform"),
            (
                EditorPrimaryView::Wave,
                EditorSpecSubView::Spec,
                EditorOtherSubView::Tempogram,
            )
        );
        assert_eq!(
            primary_view_from_project(None, None, None, "Log"),
            (
                EditorPrimaryView::Spec,
                EditorSpecSubView::Log,
                EditorOtherSubView::Tempogram,
            )
        );
        assert_eq!(
            primary_view_from_project(None, None, None, "Chromagram"),
            (
                EditorPrimaryView::Other,
                EditorSpecSubView::Spec,
                EditorOtherSubView::Chromagram,
            )
        );
    }

    #[test]
    fn primary_view_from_project_prefers_new_fields() {
        let restored =
            primary_view_from_project(Some("other"), Some("mel"), Some("chromagram"), "Waveform");
        assert_eq!(
            restored,
            (
                EditorPrimaryView::Other,
                EditorSpecSubView::Mel,
                EditorOtherSubView::Chromagram,
            )
        );
    }

    #[test]
    fn metadata_view_and_subview_restore_without_changing_legacy_view_mode() {
        let restored = primary_view_from_project(Some("metadata"), Some("spec"), None, "Waveform");
        assert_eq!(restored.0, EditorPrimaryView::Metadata);
        assert_eq!(
            metadata_sub_view_from_project(Some("hex")),
            MetadataSubView::Hex
        );
        assert_eq!(
            metadata_sub_view_from_project(None),
            MetadataSubView::Structure
        );
        assert_eq!(view_mode_from_str("Metadata"), ViewMode::Waveform);
    }

    // ── TOML serialization roundtrip (toml 1.1 migration) ────────────────────

    const MINIMAL_TOML: &str = r#"
version = 1
name = "test project"
base_dir = "/audio"
active_tab = 0
tabs = []

[list]
files = ["a.wav", "b.wav"]

[app]
theme = "dark"
sort_key = "name"
sort_dir = "asc"
search_query = ""
search_regex = false

[app.list_columns]
file = true
folder = false
transcript = false
external = false
length = true
ch = true
sr = true
bits = true
peak = false
lufs = false
gain = false
wave = true

[spectrogram]
fft_size = 2048
window = "hann"
overlap = 0.75
max_frames = 512
scale = "log"
mel_scale = "linear"
db_floor = -80.0
max_freq_hz = 20000.0
show_note_labels = false
"#;

    #[test]
    fn serialize_deserialize_minimal_roundtrip() {
        let p = deserialize_project(MINIMAL_TOML).unwrap();
        assert_eq!(p.spectrogram.db_ceiling, 0.0);
        assert_eq!(p.app.list_columns_window_pos, None);
        let s = serialize_project(&p).unwrap();
        assert!(s.contains("db_ceiling = 0.0"));
        let p2 = deserialize_project(&s).unwrap();
        assert_eq!(p.version, p2.version);
        assert_eq!(p.name, p2.name);
        assert_eq!(p.base_dir, p2.base_dir);
        assert_eq!(p.active_tab, p2.active_tab);
        assert_eq!(p.list.files, p2.list.files);
        assert_eq!(p.app.theme, p2.app.theme);
        assert_eq!(p.app.sort_key, p2.app.sort_key);
    }

    #[test]
    fn spectrogram_color_range_roundtrips_in_a_session() {
        let mut project = deserialize_project(MINIMAL_TOML).unwrap();
        project.spectrogram.db_floor = -98.0;
        project.spectrogram.db_ceiling = -12.0;
        let encoded = serialize_project(&project).expect("serialize color range");
        let restored = deserialize_project(&encoded).expect("deserialize color range");
        assert_eq!(restored.spectrogram.db_floor, -98.0);
        assert_eq!(restored.spectrogram.db_ceiling, -12.0);
    }

    #[test]
    fn legacy_snap_zero_cross_fields_are_ignored_and_not_written_again() {
        let legacy = include_str!("../../debug/cli-renders/phase1b_smoke.nwsess");
        assert!(legacy.contains("snap_zero_cross"));
        let project = deserialize_project(legacy).expect("load legacy session");
        let encoded = serialize_project(&project).expect("serialize migrated session");
        assert!(!encoded.contains("snap_zero_cross"));
    }

    #[test]
    fn list_columns_window_position_roundtrips_without_session_version_bump() {
        let mut project = deserialize_project(MINIMAL_TOML).unwrap();
        let version = project.version;
        project.app.list_columns_window_pos = Some([321.5, 147.25]);

        let encoded = serialize_project(&project).expect("serialize window position");
        let restored = deserialize_project(&encoded).expect("deserialize window position");

        assert_eq!(restored.version, version);
        assert_eq!(restored.app.list_columns_window_pos, Some([321.5, 147.25]));
    }

    #[test]
    fn dynamic_metadata_columns_roundtrip_with_stable_keys() {
        let mut project = deserialize_project(MINIMAL_TOML).unwrap();
        project
            .app
            .list_columns
            .metadata
            .push(ProjectMetadataColumn {
                key: "normalized:ucs.cat_id".to_string(),
                label: "CatID".to_string(),
                visible: true,
                width: 184.0,
            });
        let encoded = serialize_project(&project).unwrap();
        let restored = deserialize_project(&encoded).unwrap();
        let column = &restored.app.list_columns.metadata[0];
        assert_eq!(column.key, "normalized:ucs.cat_id");
        assert_eq!(column.label, "CatID");
        assert!(column.visible);
        assert_eq!(column.width, 184.0);
    }

    #[test]
    fn list_and_editor_notes_roundtrip_with_unified_column_order() {
        let mut project = deserialize_project(MINIMAL_TOML).unwrap();
        project.list.items.push(ProjectListItem {
            path: "a.wav".to_string(),
            pending_gain_db: 0.0,
            note: "List memo".to_string(),
            editor_notes: vec![
                EditorNote {
                    id: 1,
                    comment: "Cursor".to_string(),
                    start_sample: 480,
                    end_sample: None,
                    freq_range_hz: None,
                    view: None,
                },
                EditorNote {
                    id: 2,
                    comment: "Spectral range".to_string(),
                    start_sample: 960,
                    end_sample: Some(1_920),
                    freq_range_hz: Some((220.0, 880.0)),
                    view: Some("Spec".to_string()),
                },
            ],
            status: None,
            tags: Vec::new(),
        });
        project.app.list_columns.order = vec![
            "file".to_string(),
            "normalized:ucs.cat_id".to_string(),
            "note".to_string(),
        ];
        let encoded = serialize_project(&project).unwrap();
        let restored = deserialize_project(&encoded).unwrap();
        let item = &restored.list.items[0];
        assert_eq!(item.note, "List memo");
        assert_eq!(item.editor_notes.len(), 2);
        assert_eq!(item.editor_notes[0].start_sample, 480);
        assert_eq!(item.editor_notes[1].end_sample, Some(1_920));
        assert_eq!(item.editor_notes[1].freq_range_hz, Some((220.0, 880.0)));
        assert_eq!(item.editor_notes[1].view.as_deref(), Some("Spec"));
        assert_eq!(
            restored.app.list_columns.order,
            project.app.list_columns.order
        );
    }

    #[test]
    fn comments_roundtrip_without_a_session_version_bump() {
        let mut project = deserialize_project(MINIMAL_TOML).unwrap();
        let version_before = project.version;
        project.comments = vec![
            ProjectComment {
                id: "0123456789abcdef0123456789abcdef".to_string(),
                parent: None,
                author_id: "daishi".to_string(),
                author_host: Some("WS-01".to_string()),
                author_name: Some("\u{9234}\u{6728}".to_string()),
                created_at: "2026-09-01T00:00:00Z".to_string(),
                edited_at: None,
                rev: 0,
                body: "Reverb tail is long @[a.wav|1.25-2.5]".to_string(),
                deleted: false,
                resolved_by: None,
                resolved_at: None,
            },
            ProjectComment {
                id: "fedcba9876543210fedcba9876543210".to_string(),
                parent: Some("0123456789abcdef0123456789abcdef".to_string()),
                author_id: "tanaka".to_string(),
                author_host: None,
                author_name: None,
                created_at: "2026-09-01T00:01:00Z".to_string(),
                edited_at: Some("2026-09-01T00:02:00Z".to_string()),
                rev: 1,
                body: "Trimmed it".to_string(),
                deleted: false,
                resolved_by: Some("daishi".to_string()),
                resolved_at: Some("2026-09-01T00:03:00Z".to_string()),
            },
        ];
        let encoded = serialize_project(&project).unwrap();
        let restored = deserialize_project(&encoded).unwrap();
        assert_eq!(restored.comments, project.comments);
        // Comments are an additive `#[serde(default)]` field, so an older
        // build still opens a document that carries them.
        assert_eq!(restored.version, version_before);
    }

    #[test]
    fn a_session_written_before_comments_existed_loads_with_none() {
        let project = deserialize_project(MINIMAL_TOML).unwrap();
        assert!(project.comments.is_empty());
    }

    #[test]
    fn a_comment_omits_every_field_it_has_nothing_to_say_about() {
        let comment = ProjectComment {
            id: "a".to_string(),
            parent: None,
            author_id: "daishi".to_string(),
            author_host: None,
            author_name: None,
            created_at: "2026-09-01T00:00:00Z".to_string(),
            edited_at: None,
            rev: 0,
            body: "hi".to_string(),
            deleted: false,
            resolved_by: None,
            resolved_at: None,
        };
        let encoded = toml::to_string_pretty(&comment).unwrap();
        for absent in ["parent", "author_host", "author_name", "edited_at", "deleted", "resolved"] {
            assert!(!encoded.contains(absent), "{absent} should be omitted:\n{encoded}");
        }
    }

    #[test]
    fn legacy_list_item_defaults_notes_to_empty() {
        let raw = r#"
path = "a.wav"
pending_gain_db = 1.5
"#;
        let item: ProjectListItem = toml::from_str(raw).expect("legacy list item");
        assert!(item.note.is_empty());
        assert!(item.editor_notes.is_empty());
    }

    #[test]
    fn status_and_tag_assignments_and_palettes_roundtrip() {
        let mut project = deserialize_project(MINIMAL_TOML).unwrap();
        project.list.statuses = vec![
            ProjectLabelDef {
                id: "wip".to_string(),
                label: "WIP".to_string(),
                color: [212, 152, 56],
            },
            ProjectLabelDef {
                id: "ok".to_string(),
                label: "OK".to_string(),
                color: [76, 160, 96],
            },
        ];
        project.list.tags = vec![ProjectLabelDef {
            id: "foley".to_string(),
            label: "Foley".to_string(),
            color: [78, 132, 210],
        }];
        project.list.default_status = Some("wip".to_string());
        project.list.items.push(ProjectListItem {
            path: "a.wav".to_string(),
            pending_gain_db: 0.0,
            note: String::new(),
            editor_notes: Vec::new(),
            status: Some("ok".to_string()),
            tags: vec!["foley".to_string()],
        });

        let restored = deserialize_project(&serialize_project(&project).unwrap()).unwrap();

        assert_eq!(restored.list.default_status.as_deref(), Some("wip"));
        // Palette order is display order, so it has to survive the round trip.
        let ids: Vec<&str> = restored
            .list
            .statuses
            .iter()
            .map(|def| def.id.as_str())
            .collect();
        assert_eq!(ids, ["wip", "ok"]);
        assert_eq!(restored.list.statuses[0].label, "WIP");
        assert_eq!(restored.list.statuses[1].color, [76, 160, 96]);
        assert_eq!(restored.list.tags[0].id, "foley");
        let item = &restored.list.items[0];
        assert_eq!(item.status.as_deref(), Some("ok"));
        assert_eq!(item.tags, ["foley"]);
    }

    #[test]
    fn a_session_written_before_statuses_existed_still_loads() {
        let item: ProjectListItem = toml::from_str(
            r#"
path = "a.wav"
note = "memo"
"#,
        )
        .expect("legacy list item");
        assert!(item.status.is_none());
        assert!(item.tags.is_empty());

        // The whole list block predates the palette too.
        let list: ProjectList = toml::from_str("files = [\"a.wav\"]").expect("legacy list");
        assert!(list.statuses.is_empty());
        assert!(list.tags.is_empty());
        assert!(list.default_status.is_none());

        // A session saved before the column existed still shows it, the way
        // `note` was introduced.
        let columns = &deserialize_project(MINIMAL_TOML).unwrap().app.list_columns;
        assert!(columns.status);
        assert!(!columns.tags);
    }

    #[test]
    fn serialize_deserialize_name_none() {
        let toml = MINIMAL_TOML.replace(r#"name = "test project""#, "");
        let p = deserialize_project(&toml).unwrap();
        assert_eq!(p.name, None);
        let s = serialize_project(&p).unwrap();
        let p2 = deserialize_project(&s).unwrap();
        assert_eq!(p2.name, None);
    }

    #[test]
    fn serialize_deserialize_unicode_name() {
        let p = deserialize_project(MINIMAL_TOML).unwrap();
        let mut p2 = p.clone();
        p2.name = Some("日本語プロジェクト 🎵 été".to_string());
        let s = serialize_project(&p2).unwrap();
        let p3 = deserialize_project(&s).unwrap();
        assert_eq!(p3.name.as_deref(), Some("日本語プロジェクト 🎵 été"));
    }

    #[test]
    fn serialize_deserialize_unicode_file_paths() {
        let p = deserialize_project(MINIMAL_TOML).unwrap();
        let mut p2 = p.clone();
        p2.list.files = vec![
            "/audio/トラック01.wav".to_string(),
            "/audio/café_ambience.wav".to_string(),
        ];
        let s = serialize_project(&p2).unwrap();
        let p3 = deserialize_project(&s).unwrap();
        assert_eq!(p3.list.files, p2.list.files);
    }

    #[test]
    fn serialize_output_is_valid_toml() {
        let p = deserialize_project(MINIMAL_TOML).unwrap();
        let s = serialize_project(&p).unwrap();
        let parsed: Result<toml::Value, _> = toml::from_str(&s);
        assert!(
            parsed.is_ok(),
            "serialize output is not valid toml: {:?}",
            parsed.err()
        );
    }

    #[test]
    fn serialize_is_deterministic() {
        let p = deserialize_project(MINIMAL_TOML).unwrap();
        let s1 = serialize_project(&p).unwrap();
        let s2 = serialize_project(&p).unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn deserialize_version_values() {
        for v in [1u32, 2, 10, 999] {
            let toml = MINIMAL_TOML.replace("version = 1", &format!("version = {v}"));
            let p = deserialize_project(&toml).unwrap();
            let s = serialize_project(&p).unwrap();
            let p2 = deserialize_project(&s).unwrap();
            assert_eq!(p2.version, v);
        }
    }

    #[test]
    fn deserialize_default_fields_are_empty() {
        let p = deserialize_project(MINIMAL_TOML).unwrap();
        assert!(p.list.items.is_empty());
        assert!(p.list.sample_rate_overrides.is_empty());
        assert!(p.list.virtual_items.is_empty());
        assert!(p.cached_edits.is_empty());
        assert!(p.tabs.is_empty());
    }

    #[test]
    fn deserialize_list_columns_defaults_false() {
        let p = deserialize_project(MINIMAL_TOML).unwrap();
        assert!(!p.app.list_columns.edited);
        assert!(!p.app.list_columns.cover_art);
        assert!(!p.app.list_columns.bpm);
        assert!(!p.app.list_columns.created_at);
    }

    #[test]
    fn deserialize_spectrogram_floats_correct() {
        let p = deserialize_project(MINIMAL_TOML).unwrap();
        assert!((p.spectrogram.overlap - 0.75).abs() < 1e-5);
        assert!((p.spectrogram.db_floor - (-80.0)).abs() < 1e-3);
        assert!((p.spectrogram.max_freq_hz - 20000.0).abs() < 0.1);
        assert_eq!(p.spectrogram.fft_size, 2048);
        assert!(!p.spectrogram.show_note_labels);
    }

    #[test]
    fn serialize_large_file_list() {
        let p = deserialize_project(MINIMAL_TOML).unwrap();
        let mut p2 = p.clone();
        p2.list.files = (0..200)
            .map(|i| format!("/audio/track_{i:04}.wav"))
            .collect();
        let s = serialize_project(&p2).unwrap();
        let p3 = deserialize_project(&s).unwrap();
        assert_eq!(p3.list.files.len(), 200);
        assert_eq!(p3.list.files[0], "/audio/track_0000.wav");
        assert_eq!(p3.list.files[199], "/audio/track_0199.wav");
    }

    #[test]
    fn deserialize_invalid_toml_returns_error() {
        assert!(deserialize_project("not valid toml ][{").is_err());
    }

    #[test]
    fn deserialize_empty_string_returns_error() {
        assert!(deserialize_project("").is_err());
    }
}
