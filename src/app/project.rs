use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::audio::AudioBuffer;
use crate::markers::MarkerEntry;
use super::types::{
    ChannelView, ChannelViewMode, FadeShape, FileMeta, LoopMode, LoopXfadeShape, MediaSource,
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

fn rel_path(path: &Path, base: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(base) {
        rel.to_string_lossy().to_string()
    } else {
        path.to_string_lossy().to_string()
    }
}

fn resolve_path(raw: &str, base: &Path) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

fn project_sidecar_dir(path: &Path) -> PathBuf {
    path.with_extension("nwproj.d")
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
    toml::to_string_pretty(project).context("serialize project")
}

pub fn deserialize_project(text: &str) -> Result<ProjectFile> {
    toml::from_str(text).context("parse project")
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
    std::fs::create_dir_all(&data_dir).context("create project data dir")?;
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
    std::fs::create_dir_all(&data_dir).context("create project data dir")?;
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
    std::fs::create_dir_all(&data_dir).context("create project data dir")?;
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
    pub(super) fn save_project(&mut self) -> Result<(), String> {
        let path = match self.project_path.clone() {
            Some(p) => p,
            None => {
                let Some(mut picked) = self.pick_project_save_dialog() else {
                    return Ok(());
                };
                let needs_ext = picked
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| !s.eq_ignore_ascii_case("nwproj"))
                    .unwrap_or(true);
                if needs_ext {
                    picked.set_extension("nwproj");
                }
                picked
            }
        };
        self.save_project_as(path)
    }

    pub(super) fn save_project_as(&mut self, path: PathBuf) -> Result<(), String> {
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let list_files: Vec<PathBuf> = self.items.iter().map(|i| i.path.clone()).collect();
        let mut list_items = Vec::new();
        for item in &self.items {
            if item.pending_gain_db.abs() > 0.0001 {
                list_items.push(ProjectListItem {
                    path: rel_path(&item.path, base_dir),
                    pending_gain_db: item.pending_gain_db,
                });
            }
        }
        let list = ProjectList {
            root: self.root.as_ref().map(|p| rel_path(p, base_dir)),
            files: list_files.iter().map(|p| rel_path(p, base_dir)).collect(),
            items: list_items,
        };
        let app = ProjectApp {
            theme: match self.theme_mode {
                super::types::ThemeMode::Light => "light".to_string(),
                _ => "dark".to_string(),
            },
            sort_key: match self.sort_key {
                super::types::SortKey::File => "File",
                super::types::SortKey::Folder => "Folder",
                super::types::SortKey::Transcript => "Transcript",
                super::types::SortKey::Length => "Length",
                super::types::SortKey::Channels => "Channels",
                super::types::SortKey::SampleRate => "SampleRate",
                super::types::SortKey::Bits => "Bits",
                super::types::SortKey::BitRate => "BitRate",
                super::types::SortKey::Level => "Level",
                super::types::SortKey::Lufs => "Lufs",
                super::types::SortKey::Bpm => "Bpm",
                super::types::SortKey::CreatedAt => "CreatedAt",
                super::types::SortKey::ModifiedAt => "ModifiedAt",
                super::types::SortKey::External(_) => "External",
            }
            .to_string(),
            sort_dir: match self.sort_dir {
                super::types::SortDir::Asc => "Asc",
                super::types::SortDir::Desc => "Desc",
                super::types::SortDir::None => "None",
            }
            .to_string(),
            search_query: self.search_query.clone(),
            search_regex: self.search_use_regex,
            list_columns: ProjectListColumns {
                edited: self.list_columns.edited,
                file: self.list_columns.file,
                folder: self.list_columns.folder,
                transcript: self.list_columns.transcript,
                external: self.list_columns.external,
                length: self.list_columns.length,
                ch: self.list_columns.channels,
                sr: self.list_columns.sample_rate,
                bits: self.list_columns.bits,
                bit_rate: self.list_columns.bit_rate,
                peak: self.list_columns.peak,
                lufs: self.list_columns.lufs,
                bpm: self.list_columns.bpm,
                created_at: self.list_columns.created_at,
                modified_at: self.list_columns.modified_at,
                gain: self.list_columns.gain,
                wave: self.list_columns.wave,
            },
        };
        let spectrogram = project_spectrogram_from_cfg(&self.spectro_cfg);

        let mut tabs = Vec::new();
        for (idx, tab) in self.tabs.iter().enumerate() {
            let mut edited_audio = None;
            let mut preview_audio = None;
            let mut preview_tool = None;
            if tab.dirty && !tab.ch_samples.is_empty() {
                match save_sidecar_audio(
                    &path,
                    idx,
                    &tab.ch_samples,
                    self.audio.shared.out_sample_rate,
                ) {
                    Ok(p) => {
                        edited_audio = Some(p);
                    }
                    Err(err) => {
                        return Err(format!("Failed to save edited audio: {err}"));
                    }
                }
            }
            if let Some(overlay) = tab.preview_overlay.as_ref() {
                if !overlay.channels.is_empty() {
                    match save_sidecar_preview_audio(
                        &path,
                        idx,
                        &overlay.channels,
                        self.audio.shared.out_sample_rate,
                    ) {
                        Ok(p) => {
                            preview_audio = Some(p);
                            preview_tool = Some(format!("{:?}", overlay.source_tool));
                        }
                        Err(err) => {
                            return Err(format!("Failed to save preview audio: {err}"));
                        }
                    }
                }
            } else if let Some(tool) = tab.preview_audio_tool {
                preview_tool = Some(format!("{:?}", tool));
            }
            let entry =
                project_tab_from_tab(tab, base_dir, edited_audio, preview_audio, preview_tool);
            tabs.push(entry);
        }

        let mut cached_edits = Vec::new();
        for (idx, (item_path, cached)) in self.edited_cache.iter().enumerate() {
            if cached.ch_samples.is_empty() {
                continue;
            }
            let edited_audio = match save_sidecar_cached_audio(
                &path,
                idx,
                &cached.ch_samples,
                self.audio.shared.out_sample_rate,
            ) {
                Ok(p) => p,
                Err(err) => {
                    return Err(format!("Failed to save cached audio: {err}"));
                }
            };
            cached_edits.push(ProjectEdit {
                path: rel_path(item_path, base_dir),
                edited_audio: rel_path(&edited_audio, base_dir),
                dirty: cached.dirty,
                loop_region: cached.loop_region.map(|v| [v.0, v.1]),
                loop_markers_saved: cached.loop_markers_saved.map(|v| [v.0, v.1]),
                loop_markers_dirty: cached.loop_markers_dirty,
                markers: cached.markers.iter().map(marker_entry_to_project).collect(),
                markers_saved: cached
                    .markers_saved
                    .iter()
                    .map(marker_entry_to_project)
                    .collect(),
                markers_dirty: cached.markers_dirty,
                trim_range: cached.trim_range.map(|v| [v.0, v.1]),
                loop_xfade_samples: cached.loop_xfade_samples,
                loop_xfade_shape: match cached.loop_xfade_shape {
                    LoopXfadeShape::Linear => "linear",
                    LoopXfadeShape::EqualPower => "equal",
                }
                .to_string(),
                fade_in_range: cached.fade_in_range.map(|v| [v.0, v.1]),
                fade_out_range: cached.fade_out_range.map(|v| [v.0, v.1]),
                fade_in_shape: format!("{:?}", cached.fade_in_shape),
                fade_out_shape: format!("{:?}", cached.fade_out_shape),
                loop_mode: format!("{:?}", cached.loop_mode),
                snap_zero_cross: cached.snap_zero_cross,
                tool_state: ProjectToolState {
                    fade_in_ms: cached.tool_state.fade_in_ms,
                    fade_out_ms: cached.tool_state.fade_out_ms,
                    gain_db: cached.tool_state.gain_db,
                    normalize_target_db: cached.tool_state.normalize_target_db,
                    loudness_target_lufs: cached.tool_state.loudness_target_lufs,
                    pitch_semitones: cached.tool_state.pitch_semitones,
                    stretch_rate: cached.tool_state.stretch_rate,
                    loop_repeat: cached.tool_state.loop_repeat,
                },
                active_tool: format!("{:?}", cached.active_tool),
                show_waveform_overlay: cached.show_waveform_overlay,
                bpm_enabled: cached.bpm_enabled,
                bpm_value: cached.bpm_value,
                bpm_user_set: cached.bpm_user_set,
            });
        }

        let project = ProjectFile {
            version: 1,
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string()),
            base_dir: Some(base_dir.to_string_lossy().to_string()),
            list,
            app,
            spectrogram,
            tabs,
            active_tab: self.active_tab,
            cached_edits,
        };
        let text = serialize_project(&project).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        self.project_path = Some(path);
        Ok(())
    }

    pub(super) fn open_project_file(&mut self, path: PathBuf) -> Result<(), String> {
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let project = deserialize_project(&text).map_err(|e| e.to_string())?;
        if project.version != 1 {
            return Err(format!("Unsupported project version: {}", project.version));
        }
        let base_dir = if let Some(base) = project.base_dir.as_ref() {
            let base_path = PathBuf::from(base);
            if base_path.is_absolute() {
                base_path
            } else {
                path.parent().unwrap_or_else(|| Path::new(".")).join(base_path)
            }
        } else {
            path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf()
        };

        let project_path = path.clone();
        self.close_project();
        self.project_path = Some(project_path.clone());

        self.search_query = project.app.search_query.clone();
        self.search_use_regex = project.app.search_regex;
        self.list_columns = super::types::ListColumnConfig {
            edited: project.app.list_columns.edited,
            file: project.app.list_columns.file,
            folder: project.app.list_columns.folder,
            transcript: project.app.list_columns.transcript,
            external: project.app.list_columns.external,
            length: project.app.list_columns.length,
            channels: project.app.list_columns.ch,
            sample_rate: project.app.list_columns.sr,
            bits: project.app.list_columns.bits,
            bit_rate: project.app.list_columns.bit_rate,
            peak: project.app.list_columns.peak,
            lufs: project.app.list_columns.lufs,
            bpm: project.app.list_columns.bpm,
            created_at: project.app.list_columns.created_at,
            modified_at: project.app.list_columns.modified_at,
            gain: project.app.list_columns.gain,
            wave: project.app.list_columns.wave,
        };
        self.sort_key = match project.app.sort_key.as_str() {
            "Folder" => super::types::SortKey::Folder,
            "Transcript" => super::types::SortKey::Transcript,
            "Length" => super::types::SortKey::Length,
            "Channels" => super::types::SortKey::Channels,
            "SampleRate" => super::types::SortKey::SampleRate,
            "Bits" => super::types::SortKey::Bits,
            "BitRate" => super::types::SortKey::BitRate,
            "Level" => super::types::SortKey::Level,
            "Lufs" => super::types::SortKey::Lufs,
            "Bpm" => super::types::SortKey::Bpm,
            "CreatedAt" => super::types::SortKey::CreatedAt,
            "ModifiedAt" => super::types::SortKey::ModifiedAt,
            _ => super::types::SortKey::File,
        };
        self.sort_dir = match project.app.sort_dir.as_str() {
            "Asc" => super::types::SortDir::Asc,
            "Desc" => super::types::SortDir::Desc,
            _ => super::types::SortDir::None,
        };
        match project.app.theme.as_str() {
            "light" => self.theme_mode = super::types::ThemeMode::Light,
            _ => self.theme_mode = super::types::ThemeMode::Dark,
        }
        self.apply_spectro_config(spectro_config_from_project(&project.spectrogram));

        if !project.list.files.is_empty() {
            self.reset_list_from_project(&project.list.files, &base_dir);
            self.after_add_refresh();
        } else if let Some(root) = project.list.root.as_ref() {
            let root_path = resolve_path(root, &base_dir);
            self.root = Some(root_path);
            self.rescan();
        }

        for item in project.list.items.iter() {
            let path = resolve_path(&item.path, &base_dir);
            if let Some(list_item) = self.item_for_path_mut(&path) {
                list_item.pending_gain_db = item.pending_gain_db;
            }
        }

        let out_sr = self.audio.shared.out_sample_rate;
        for edit in project.cached_edits.iter() {
            let path = resolve_path(&edit.path, &base_dir);
            let edited = load_sidecar_audio(&project_path, &edit.edited_audio).ok();
            let Some((mut chans, sr, _)) = edited else {
                continue;
            };
            if sr != out_sr {
                for ch in chans.iter_mut() {
                    *ch = crate::wave::resample_linear(ch, sr, out_sr);
                }
            }
            let samples_len = chans.get(0).map(|c| c.len()).unwrap_or(0);
            let mut waveform = Vec::new();
            let mono = super::WavesPreviewer::mixdown_channels(&chans, samples_len);
            crate::wave::build_minmax(&mut waveform, &mono, 2048);
            self.edited_cache.insert(
                path,
                super::types::CachedEdit {
                    ch_samples: chans,
                    samples_len,
                    waveform_minmax: waveform,
                    dirty: edit.dirty,
                    loop_region: edit.loop_region.map(|v| (v[0], v[1])),
                    loop_region_committed: edit.loop_region.map(|v| (v[0], v[1])),
                    loop_region_applied: edit.loop_region.map(|v| (v[0], v[1])),
                    loop_markers_saved: edit.loop_markers_saved.map(|v| (v[0], v[1])),
                    loop_markers_dirty: edit.loop_markers_dirty,
                    markers: edit.markers.iter().map(project_marker_to_entry).collect(),
                    markers_committed: edit.markers.iter().map(project_marker_to_entry).collect(),
                    markers_applied: edit.markers.iter().map(project_marker_to_entry).collect(),
                    markers_saved: edit
                        .markers_saved
                        .iter()
                        .map(project_marker_to_entry)
                        .collect(),
                    markers_dirty: edit.markers_dirty,
                    trim_range: edit.trim_range.map(|v| (v[0], v[1])),
                    loop_xfade_samples: edit.loop_xfade_samples,
                    loop_xfade_shape: loop_shape_from_str(&edit.loop_xfade_shape),
                    fade_in_range: edit.fade_in_range.map(|v| (v[0], v[1])),
                    fade_out_range: edit.fade_out_range.map(|v| (v[0], v[1])),
                    fade_in_shape: fade_shape_from_str(&edit.fade_in_shape),
                    fade_out_shape: fade_shape_from_str(&edit.fade_out_shape),
                    loop_mode: loop_mode_from_str(&edit.loop_mode),
                    snap_zero_cross: edit.snap_zero_cross,
                    tool_state: project_tool_state_to_tool_state(&edit.tool_state),
                    active_tool: tool_kind_from_str(&edit.active_tool),
                    show_waveform_overlay: edit.show_waveform_overlay,
                    bpm_enabled: edit.bpm_enabled,
                    bpm_value: edit.bpm_value,
                    bpm_user_set: edit.bpm_user_set,
                },
            );
        }

        for tab in project.tabs.iter() {
            let tab_path = resolve_path(&tab.path, &base_dir);
            let edited = if let Some(raw) = tab.edited_audio.as_ref() {
                load_sidecar_audio(&project_path, raw).ok()
            } else {
                None
            };
            if let Some((mut chans, sr, _)) = edited {
                if sr != out_sr {
                    for ch in chans.iter_mut() {
                        *ch = crate::wave::resample_linear(ch, sr, out_sr);
                    }
                }
                let mut waveform = Vec::new();
                let mono = super::WavesPreviewer::mixdown_channels(&chans, chans.get(0).map(|c| c.len()).unwrap_or(0));
                crate::wave::build_minmax(&mut waveform, &mono, 2048);
                self.edited_cache.insert(
                    tab_path.clone(),
                    super::types::CachedEdit {
                        ch_samples: chans,
                        samples_len: mono.len(),
                        waveform_minmax: waveform,
                        dirty: tab.dirty,
                        loop_region: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_region_committed: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_region_applied: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_markers_saved: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_markers_dirty: tab.loop_markers_dirty,
                        markers: tab
                            .markers
                            .iter()
                            .map(project_marker_to_entry)
                            .collect(),
                        markers_committed: tab
                            .markers
                            .iter()
                            .map(project_marker_to_entry)
                            .collect(),
                        markers_applied: tab
                            .markers
                            .iter()
                            .map(project_marker_to_entry)
                            .collect(),
                        markers_saved: tab
                            .markers
                            .iter()
                            .map(project_marker_to_entry)
                            .collect(),
                        markers_dirty: tab.markers_dirty,
                        trim_range: tab.trim_range.map(|v| (v[0], v[1])),
                        loop_xfade_samples: tab.loop_xfade_samples,
                        loop_xfade_shape: loop_shape_from_str(&tab.loop_xfade_shape),
                        fade_in_range: tab.fade_in_range.map(|v| (v[0], v[1])),
                        fade_out_range: tab.fade_out_range.map(|v| (v[0], v[1])),
                        fade_in_shape: fade_shape_from_str(&tab.fade_in_shape),
                        fade_out_shape: fade_shape_from_str(&tab.fade_out_shape),
                        loop_mode: loop_mode_from_str(&tab.loop_mode),
                        snap_zero_cross: tab.snap_zero_cross,
                        tool_state: project_tool_state_to_tool_state(&tab.tool_state),
                        active_tool: tool_kind_from_str(&tab.active_tool),
                        show_waveform_overlay: tab.show_waveform_overlay,
                        bpm_enabled: tab.bpm_enabled,
                        bpm_value: tab.bpm_value,
                        bpm_user_set: tab.bpm_user_set,
                    },
                );
            }
            if !tab_path.is_file() {
                if let Some(item) = self.item_for_path_mut(&tab_path) {
                    item.source = MediaSource::Virtual;
                    item.status =
                        super::types::MediaStatus::DecodeFailed(describe_missing(&tab_path));
                    item.meta = Some(missing_file_meta(&tab_path));
                    if item.virtual_audio.is_none() {
                        item.virtual_audio = Some(std::sync::Arc::new(AudioBuffer::from_channels(
                            vec![Vec::new()],
                        )));
                    }
                }
            }
        }

        for tab in project.tabs.iter() {
            let tab_path = resolve_path(&tab.path, &base_dir);
            self.open_or_activate_tab(&tab_path);
            if let Some(idx) = self.tabs.iter().position(|t| t.path == tab_path) {
                let mut preview_overlay = None;
                let mut preview_tool = None;
                if let Some(raw) = tab.preview_audio.as_ref() {
                    if let Ok((mut chans, sr, _)) = load_sidecar_audio(&project_path, raw) {
                        if sr != out_sr {
                            for ch in chans.iter_mut() {
                                *ch = crate::wave::resample_linear(ch, sr, out_sr);
                            }
                        }
                        let timeline_len =
                            chans.get(0).map(|c| c.len()).unwrap_or_default();
                        let tool = tab
                            .preview_tool
                            .as_deref()
                            .map(tool_kind_from_str)
                            .unwrap_or(super::types::ToolKind::LoopEdit);
                        preview_overlay = Some(super::WavesPreviewer::preview_overlay_from_channels(
                            chans,
                            tool,
                            timeline_len,
                        ));
                        preview_tool = Some(tool);
                    }
                }
                if let Some(t) = self.tabs.get_mut(idx) {
                    t.view_mode = view_mode_from_str(&tab.view_mode);
                    t.show_waveform_overlay = tab.show_waveform_overlay;
                    t.channel_view = project_channel_view_to_channel_view(&tab.channel_view);
                    t.active_tool = tool_kind_from_str(&tab.active_tool);
                    t.tool_state = project_tool_state_to_tool_state(&tab.tool_state);
                    t.loop_mode = loop_mode_from_str(&tab.loop_mode);
                    t.loop_region = tab.loop_region.map(|v| (v[0], v[1]));
                    t.loop_xfade_samples = tab.loop_xfade_samples;
                    t.loop_xfade_shape = loop_shape_from_str(&tab.loop_xfade_shape);
                    t.trim_range = tab.trim_range.map(|v| (v[0], v[1]));
                    t.selection = tab.selection.map(|v| (v[0], v[1]));
                    t.markers = tab.markers.iter().map(project_marker_to_entry).collect();
                    t.markers_saved = t.markers.clone();
                    t.markers_dirty = tab.markers_dirty;
                    t.loop_markers_saved = t.loop_region;
                    t.loop_markers_dirty = tab.loop_markers_dirty;
                    t.fade_in_range = tab.fade_in_range.map(|v| (v[0], v[1]));
                    t.fade_out_range = tab.fade_out_range.map(|v| (v[0], v[1]));
                    t.fade_in_shape = fade_shape_from_str(&tab.fade_in_shape);
                    t.fade_out_shape = fade_shape_from_str(&tab.fade_out_shape);
                    t.snap_zero_cross = tab.snap_zero_cross;
                    t.bpm_enabled = tab.bpm_enabled;
                    t.bpm_value = tab.bpm_value;
                    t.bpm_user_set = tab.bpm_user_set;
                    t.view_offset = tab.view_offset;
                    t.samples_per_px = tab.samples_per_px;
                    t.dirty = tab.dirty;
                    if let Some(overlay) = preview_overlay {
                        t.preview_overlay = Some(overlay);
                        t.preview_audio_tool = preview_tool;
                    }
                }
            }
        }

        if let Some(active) = project.active_tab {
            if active < self.tabs.len() {
                self.active_tab = Some(active);
            }
        }
        if let Some(active) = self.active_tab {
            let (tool, mono) = {
                let Some(tab) = self.tabs.get(active) else {
                    return Ok(());
                };
                let Some(tool) = tab.preview_audio_tool else {
                    return Ok(());
                };
                let Some(overlay) = tab.preview_overlay.as_ref() else {
                    return Ok(());
                };
                let mono = if let Some(m) = overlay.mixdown.as_ref() {
                    m.clone()
                } else {
                    overlay
                        .channels
                        .get(0)
                        .cloned()
                        .unwrap_or_default()
                };
                (tool, mono)
            };
            self.set_preview_mono(active, tool, mono);
        }
        Ok(())
    }

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

    fn reset_list_from_project(&mut self, raw_paths: &[String], base_dir: &Path) {
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
