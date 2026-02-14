use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::types::{
    ChannelView, ChannelViewMode, FadeShape, FileMeta, LoopMode, LoopXfadeShape, PluginFxDraft,
    PluginParamUiState, SpectrogramConfig, SpectrogramScale, ToolKind, ToolState, ViewMode,
};
use crate::markers::MarkerEntry;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectFile {
    pub version: u32,
    pub name: Option<String>,
    pub base_dir: Option<String>,
    pub list: ProjectList,
    pub app: ProjectApp,
    pub spectrogram: ProjectSpectrogram,
    pub tabs: Vec<ProjectTab>,
    pub active_tab: Option<usize>,
    #[serde(default)]
    pub cached_edits: Vec<ProjectEdit>,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectListItem {
    pub path: String,
    pub pending_gain_db: f32,
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
    pub dirty: bool,
    pub loop_region: Option<[usize; 2]>,
    pub loop_markers_saved: Option<[usize; 2]>,
    pub loop_markers_dirty: bool,
    pub markers: Vec<ProjectMarker>,
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
    pub snap_zero_cross: bool,
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
    pub plugin_fx_draft: ProjectPluginFxDraft,
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
    #[serde(default)]
    pub auto_play_list_nav: bool,
    #[serde(default)]
    pub export_policy: Option<ProjectExportPolicy>,
    #[serde(default)]
    pub external_state: Option<ProjectExternalState>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProjectExportPolicy {
    #[serde(default = "default_export_save_mode")]
    pub save_mode: String,
    #[serde(default = "default_export_conflict")]
    pub conflict: String,
    #[serde(default = "default_export_backup_bak")]
    pub backup_bak: bool,
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
    pub file: bool,
    pub folder: bool,
    pub transcript: bool,
    pub external: bool,
    pub length: bool,
    pub ch: bool,
    pub sr: bool,
    pub bits: bool,
    #[serde(default)]
    pub bit_rate: bool,
    pub peak: bool,
    pub lufs: bool,
    #[serde(default)]
    pub bpm: bool,
    #[serde(default)]
    pub created_at: bool,
    #[serde(default)]
    pub modified_at: bool,
    pub gain: bool,
    pub wave: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectSpectrogram {
    pub fft_size: usize,
    pub window: String,
    pub overlap: f32,
    pub max_frames: usize,
    pub scale: String,
    pub mel_scale: String,
    pub db_floor: f32,
    pub max_freq_hz: f32,
    pub show_note_labels: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectTab {
    pub path: String,
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
    pub preview_tool: Option<String>,
    #[serde(default)]
    pub preview_audio: Option<String>,
    pub loop_mode: String,
    pub loop_region: Option<[usize; 2]>,
    pub loop_xfade_samples: usize,
    pub loop_xfade_shape: String,
    pub trim_range: Option<[usize; 2]>,
    pub selection: Option<[usize; 2]>,
    pub markers: Vec<ProjectMarker>,
    pub markers_dirty: bool,
    pub loop_markers_dirty: bool,
    pub fade_in_range: Option<[usize; 2]>,
    pub fade_out_range: Option<[usize; 2]>,
    pub fade_in_shape: String,
    pub fade_out_shape: String,
    pub snap_zero_cross: bool,
    pub view_offset: usize,
    pub samples_per_px: f32,
    pub dirty: bool,
    pub edited_audio: Option<String>,
    #[serde(default)]
    pub plugin_fx_draft: ProjectPluginFxDraft,
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
    #[serde(default = "default_loop_repeat")]
    pub loop_repeat: u32,
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

fn project_data_dir(path: &Path) -> PathBuf {
    project_sidecar_dir(path).join("data")
}

fn default_loop_repeat() -> u32 {
    2
}

fn default_loudness_target_lufs() -> f32 {
    -14.0
}

fn default_bpm_value() -> f32 {
    0.0
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

fn default_export_name_template() -> String {
    "{name} (gain{gain:+.1}dB)".to_string()
}

pub fn serialize_project(project: &ProjectFile) -> Result<String> {
    toml::to_string_pretty(project).context("serialize session")
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
    SpectrogramConfig {
        fft_size: p.fft_size,
        window,
        overlap: p.overlap,
        max_frames: p.max_frames,
        scale,
        mel_scale,
        db_floor: p.db_floor,
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
        overlap: cfg.overlap,
        max_frames: cfg.max_frames,
        scale: scale.to_string(),
        mel_scale: mel_scale.to_string(),
        db_floor: cfg.db_floor,
        max_freq_hz: cfg.max_freq_hz,
        show_note_labels: cfg.show_note_labels,
    }
}

pub fn project_tab_from_tab(
    tab: &super::types::EditorTab,
    base: &Path,
    edited_audio: Option<PathBuf>,
    preview_audio: Option<PathBuf>,
    preview_tool: Option<String>,
) -> ProjectTab {
    ProjectTab {
        path: rel_path(&tab.path, base),
        view_mode: format!("{:?}", tab.view_mode),
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
            loop_repeat: tab.tool_state.loop_repeat,
        },
        bpm_enabled: tab.bpm_enabled,
        bpm_value: tab.bpm_value,
        bpm_user_set: tab.bpm_user_set,
        preview_tool,
        preview_audio: preview_audio.map(|p| rel_path(&p, base)),
        loop_mode: format!("{:?}", tab.loop_mode),
        loop_region: tab.loop_region.map(|(a, b)| [a, b]),
        loop_xfade_samples: tab.loop_xfade_samples,
        loop_xfade_shape: match tab.loop_xfade_shape {
            LoopXfadeShape::Linear => "linear",
            LoopXfadeShape::EqualPower => "equal",
        }
        .to_string(),
        trim_range: tab.trim_range.map(|(a, b)| [a, b]),
        selection: tab.selection.map(|(a, b)| [a, b]),
        markers: tab
            .markers
            .iter()
            .map(|m| ProjectMarker {
                sample: m.sample,
                label: m.label.clone(),
            })
            .collect(),
        markers_dirty: tab.markers_dirty,
        loop_markers_dirty: tab.loop_markers_dirty,
        fade_in_range: tab.fade_in_range.map(|(a, b)| [a, b]),
        fade_out_range: tab.fade_out_range.map(|(a, b)| [a, b]),
        fade_in_shape: format!("{:?}", tab.fade_in_shape),
        fade_out_shape: format!("{:?}", tab.fade_out_shape),
        snap_zero_cross: tab.snap_zero_cross,
        view_offset: tab.view_offset,
        samples_per_px: tab.samples_per_px,
        dirty: tab.dirty,
        edited_audio: edited_audio.map(|p| rel_path(&p, base)),
        plugin_fx_draft: project_plugin_fx_draft_from_draft(&tab.plugin_fx_draft),
    }
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
        last_backend_log: draft.last_backend_log.clone(),
    }
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
        loop_repeat: t.loop_repeat.max(2),
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
        "Trim" => ToolKind::Trim,
        "Fade" => ToolKind::Fade,
        "PitchShift" => ToolKind::PitchShift,
        "TimeStretch" => ToolKind::TimeStretch,
        "Gain" => ToolKind::Gain,
        "Normalize" => ToolKind::Normalize,
        "Loudness" => ToolKind::Loudness,
        "Reverse" => ToolKind::Reverse,
        "PluginFx" => ToolKind::PluginFx,
        _ => ToolKind::LoopEdit,
    }
}

pub fn view_mode_from_str(s: &str) -> ViewMode {
    match s {
        "Spectrogram" => ViewMode::Spectrogram,
        "Mel" => ViewMode::Mel,
        _ => ViewMode::Waveform,
    }
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
        channels: 0,
        sample_rate: 0,
        bits_per_sample: 0,
        sample_value_kind: super::types::SampleValueKind::Unknown,
        bit_rate_bps: None,
        duration_secs: None,
        rms_db: None,
        peak_db: None,
        lufs_i: None,
        bpm: None,
        created_at: None,
        modified_at: None,
        thumb: Vec::new(),
        decode_error: Some(format!("Missing: {}", path.display())),
    }
}

pub fn save_sidecar_audio(
    project_path: &Path,
    tab_index: usize,
    channels: &[Vec<f32>],
    sample_rate: u32,
) -> Result<PathBuf> {
    let data_dir = project_data_dir(project_path);
    std::fs::create_dir_all(&data_dir).context("create session data dir")?;
    let filename = format!("tab_{:04}.wav", tab_index);
    let dst = data_dir.join(filename);
    let len = channels.get(0).map(|c| c.len()).unwrap_or(0);
    crate::wave::export_selection_wav(channels, sample_rate, (0, len), &dst)
        .context("export edited audio")?;
    Ok(dst)
}

pub fn save_sidecar_preview_audio(
    project_path: &Path,
    tab_index: usize,
    channels: &[Vec<f32>],
    sample_rate: u32,
) -> Result<PathBuf> {
    let data_dir = project_data_dir(project_path);
    std::fs::create_dir_all(&data_dir).context("create session data dir")?;
    let filename = format!("preview_{:04}.wav", tab_index);
    let dst = data_dir.join(filename);
    let len = channels.get(0).map(|c| c.len()).unwrap_or(0);
    crate::wave::export_selection_wav(channels, sample_rate, (0, len), &dst)
        .context("export preview audio")?;
    Ok(dst)
}

pub fn save_sidecar_cached_audio(
    project_path: &Path,
    edit_index: usize,
    channels: &[Vec<f32>],
    sample_rate: u32,
) -> Result<PathBuf> {
    let data_dir = project_data_dir(project_path);
    std::fs::create_dir_all(&data_dir).context("create session data dir")?;
    let filename = format!("cache_{:04}.wav", edit_index);
    let dst = data_dir.join(filename);
    let len = channels.get(0).map(|c| c.len()).unwrap_or(0);
    crate::wave::export_selection_wav(channels, sample_rate, (0, len), &dst)
        .context("export cached audio")?;
    Ok(dst)
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
        self.tabs.clear();
        self.active_tab = None;
        self.edited_cache.clear();
        self.pending_activate_path = None;
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
    }

    pub(super) fn reset_list_from_project(&mut self, raw_paths: &[String], base_dir: &Path) {
        self.root = None;
        self.files.clear();
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
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
        self.scan_rx = None;
        self.scan_in_progress = false;
        for raw in raw_paths {
            let p = resolve_path(raw, base_dir);
            let mut item = self.make_media_item(p.clone());
            if !p.is_file() {
                item.status = super::types::MediaStatus::DecodeFailed(describe_missing(&p));
                item.meta = Some(missing_file_meta(&p));
            }
            let id = item.id;
            self.path_index.insert(p, id);
            self.item_index.insert(id, self.items.len());
            self.items.push(item);
        }
        self.ensure_meta_pool();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
