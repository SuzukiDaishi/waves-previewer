use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::markers::MarkerEntry;
use super::types::{
    ChannelView, ChannelViewMode, FadeShape, FileMeta, LoopMode, LoopXfadeShape,
    SpectrogramConfig, SpectrogramScale, ToolKind, ToolState, ViewMode,
};

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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectListItem {
    pub path: String,
    pub pending_gain_db: f32,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectApp {
    pub theme: String,
    pub sort_key: String,
    pub sort_dir: String,
    pub search_query: String,
    pub search_regex: bool,
    pub list_columns: ProjectListColumns,
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

pub(super) fn rel_path(path: &Path, base: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(base) {
        rel.to_string_lossy().to_string()
    } else {
        path.to_string_lossy().to_string()
    }
}

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
        "Reverse" => ToolKind::Reverse,
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
    let base = project_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
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
