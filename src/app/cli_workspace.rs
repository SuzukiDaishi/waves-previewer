use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use serde_json::{Map, Value};

use anyhow::{bail, Context, Result};

use super::types::{
    FadeShape, LoopMode, LoopXfadeShape, ToolKind, ToolState,
};
use super::WavesPreviewer;
use crate::markers::MarkerEntry;

const CLI_DECODE_TIMEOUT: Duration = Duration::from_secs(30);
const CLI_APPLY_TIMEOUT: Duration = Duration::from_secs(60);
const CLI_JOB_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlaybackRangeSource {
    Selection,
    Loop,
    Explicit,
    Whole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlaybackRangeSpec {
    pub range: (usize, usize),
    pub source: PlaybackRangeSource,
}

pub(super) struct CliWorkspace {
    pub(super) session_path: PathBuf,
    pub(super) app: WavesPreviewer,
    session_tab_snapshots: std::collections::HashMap<String, SessionTabSnapshot>,
}

#[derive(Clone)]
struct SessionTabSnapshot {
    show_waveform_overlay: bool,
    snap_zero_cross: bool,
    active_tool: ToolKind,
    tool_state: ToolState,
    loop_mode: LoopMode,
    preview_offset_samples: Option<usize>,
    selection: Option<(usize, usize)>,
    loop_region: Option<(usize, usize)>,
    loop_region_committed: Option<(usize, usize)>,
    loop_region_applied: Option<(usize, usize)>,
    loop_markers_saved: Option<(usize, usize)>,
    trim_range: Option<(usize, usize)>,
    loop_xfade_samples: usize,
    loop_xfade_shape: LoopXfadeShape,
    fade_in_range: Option<(usize, usize)>,
    fade_out_range: Option<(usize, usize)>,
    fade_in_shape: FadeShape,
    fade_out_shape: FadeShape,
    dirty: bool,
    markers: Vec<MarkerEntry>,
    markers_committed: Vec<MarkerEntry>,
    markers_applied: Vec<MarkerEntry>,
    markers_dirty: bool,
    loop_markers_dirty: bool,
    view_offset: usize,
    samples_per_px: f32,
    vertical_zoom: f32,
    vertical_view_center: f32,
}

impl CliWorkspace {
    pub(super) fn load(session_path: &Path) -> Result<Self> {
        let session_path = absolute_existing_path(session_path)?;
        let session_text = std::fs::read_to_string(&session_path)
            .with_context(|| format!("read session file: {}", session_path.display()))?;
        let session_project = super::project::deserialize_project(&session_text)
            .with_context(|| format!("parse session file: {}", session_path.display()))?;
        let session_base = session_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())
            .context("create headless workspace")?;
        app.open_project_file(session_path.clone())
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("open session: {}", session_path.display()))?;
        let session_tab_snapshots = session_project
            .tabs
            .iter()
            .map(|tab| {
                (
                    workspace_path_key(&super::project::resolve_path(&tab.path, &session_base)),
                    SessionTabSnapshot::from_project_tab(tab),
                )
            })
            .collect();
        Ok(Self {
            session_path,
            app,
            session_tab_snapshots,
        })
    }

    pub(super) fn save(&mut self) -> Result<()> {
        self.app
            .save_project_as(self.session_path.clone())
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("save session: {}", self.session_path.display()))
    }

    pub(super) fn resolve_target_path(&self, requested: Option<&Path>) -> Result<PathBuf> {
        if let Some(path) = requested {
            return absolute_output_path(path);
        }
        if let Some(idx) = self.app.active_tab {
            if let Some(tab) = self.app.tabs.get(idx) {
                return Ok(tab.path.clone());
            }
        }
        if let Some(tab) = self.app.tabs.first() {
            return Ok(tab.path.clone());
        }
        if let Some(item) = self.app.items.first() {
            return Ok(item.path.clone());
        }
        bail!("session does not contain any target audio")
    }

    pub(super) fn ensure_target_tab_loaded(&mut self, requested: Option<&Path>) -> Result<usize> {
        let target = self.resolve_target_path(requested)?;
        if let Some(idx) = self.find_tab_index(&target) {
            self.app.active_tab = Some(idx);
            self.wait_for_decode(idx, &target)?;
            self.restore_session_snapshot(idx, &target);
            return Ok(idx);
        }
        self.app.open_or_activate_tab(&target);
        let idx = self
            .find_tab_index(&target)
            .context("failed to open target tab")?;
        self.app.active_tab = Some(idx);
        self.wait_for_decode(idx, &target)?;
        self.restore_session_snapshot(idx, &target);
        Ok(idx)
    }

    pub(super) fn wait_for_apply(&mut self) -> Result<()> {
        let ctx = egui::Context::default();
        let started = Instant::now();
        while self.app.editor_apply_state.is_some() {
            self.app.drain_editor_apply_jobs(&ctx);
            if self.app.editor_apply_state.is_none() {
                break;
            }
            if started.elapsed() > CLI_APPLY_TIMEOUT {
                bail!("editor apply timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    pub(super) fn wait_for_external_loads(&mut self) -> Result<()> {
        let ctx = egui::Context::default();
        let started = Instant::now();
        loop {
            self.app.drain_external_load_results(&ctx);
            if !self.app.external_load_inflight
                && self.app.external_load_rx.is_none()
                && self.app.external_load_queue.is_empty()
            {
                return Ok(());
            }
            if started.elapsed() > CLI_JOB_TIMEOUT {
                bail!("external load timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[allow(dead_code)]
    pub(super) fn wait_for_transcript_model_download(&mut self) -> Result<()> {
        let ctx = egui::Context::default();
        let started = Instant::now();
        while self.app.transcript_model_download_state.is_some() {
            self.app.drain_transcript_model_download_results(&ctx);
            if started.elapsed() > CLI_JOB_TIMEOUT {
                bail!("transcript model download timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    pub(super) fn wait_for_transcript_ai(&mut self) -> Result<()> {
        let ctx = egui::Context::default();
        let started = Instant::now();
        while self.app.transcript_ai_state.is_some() {
            self.app.drain_transcript_ai_results(&ctx);
            if started.elapsed() > CLI_JOB_TIMEOUT {
                bail!("transcript generate timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(super) fn wait_for_music_model_download(&mut self) -> Result<()> {
        let ctx = egui::Context::default();
        let started = Instant::now();
        while self.app.music_model_download_state.is_some() {
            self.app.drain_music_model_download_results(&ctx);
            if started.elapsed() > CLI_JOB_TIMEOUT {
                bail!("music model download timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    pub(super) fn wait_for_music_analysis(&mut self) -> Result<()> {
        let ctx = egui::Context::default();
        let started = Instant::now();
        while self.app.music_ai_state.is_some() {
            self.app.drain_music_ai_results(&ctx);
            if started.elapsed() > CLI_JOB_TIMEOUT {
                bail!("music analysis timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    pub(super) fn wait_for_plugin_scan(&mut self) -> Result<()> {
        let ctx = egui::Context::default();
        let started = Instant::now();
        while self.app.plugin_scan_state.is_some() {
            self.app.drain_plugin_jobs(&ctx);
            if started.elapsed() > CLI_JOB_TIMEOUT {
                bail!("plugin scan timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(super) fn wait_for_plugin_probe(&mut self) -> Result<()> {
        let ctx = egui::Context::default();
        let started = Instant::now();
        while self.app.plugin_probe_state.is_some() {
            self.app.drain_plugin_jobs(&ctx);
            if started.elapsed() > CLI_JOB_TIMEOUT {
                bail!("plugin probe timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    pub(super) fn wait_for_plugin_process(&mut self) -> Result<()> {
        let ctx = egui::Context::default();
        let started = Instant::now();
        while self.app.plugin_process_state.is_some() {
            self.app.drain_plugin_jobs(&ctx);
            if started.elapsed() > CLI_JOB_TIMEOUT {
                bail!("plugin processing timed out");
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    pub(super) fn apply_tool_for_target(&mut self, requested: Option<&Path>) -> Result<()> {
        let tab_idx = self.ensure_target_tab_loaded(requested)?;
        let active_tool = self
            .app
            .tabs
            .get(tab_idx)
            .map(|tab| tab.active_tool)
            .context("missing target tab")?;
        match active_tool {
            ToolKind::Trim => self.apply_trim(tab_idx)?,
            ToolKind::Fade => self.apply_fade(tab_idx)?,
            ToolKind::PitchShift => {
                let semitones = self
                    .app
                    .tabs
                    .get(tab_idx)
                    .map(|tab| tab.tool_state.pitch_semitones)
                    .unwrap_or(0.0);
                self.app
                    .spawn_editor_apply_for_tab(tab_idx, ToolKind::PitchShift, semitones);
                self.wait_for_apply()?;
            }
            ToolKind::TimeStretch => {
                let rate = self
                    .app
                    .tabs
                    .get(tab_idx)
                    .map(|tab| tab.tool_state.stretch_rate)
                    .unwrap_or(1.0);
                self.app
                    .spawn_editor_apply_for_tab(tab_idx, ToolKind::TimeStretch, rate);
                self.wait_for_apply()?;
            }
            ToolKind::Gain => {
                let db = self
                    .app
                    .tabs
                    .get(tab_idx)
                    .map(|tab| tab.tool_state.gain_db)
                    .unwrap_or(0.0);
                let len = self.tab_len(tab_idx)?;
                self.app.editor_apply_gain_range(tab_idx, (0, len), db);
            }
            ToolKind::Normalize => {
                let db = self
                    .app
                    .tabs
                    .get(tab_idx)
                    .map(|tab| tab.tool_state.normalize_target_db)
                    .unwrap_or(-6.0);
                let len = self.tab_len(tab_idx)?;
                self.app.editor_apply_normalize_range(tab_idx, (0, len), db);
            }
            ToolKind::Loudness => {
                let target = self
                    .app
                    .tabs
                    .get(tab_idx)
                    .map(|tab| tab.tool_state.loudness_target_lufs)
                    .unwrap_or(-14.0);
                self.app
                    .spawn_editor_apply_for_tab(tab_idx, ToolKind::Loudness, target);
                self.wait_for_apply()?;
            }
            ToolKind::Reverse => {
                let len = self.tab_len(tab_idx)?;
                self.app.editor_apply_reverse_range(tab_idx, (0, len));
            }
            ToolKind::LoopEdit | ToolKind::Markers | ToolKind::PluginFx | ToolKind::MusicAnalyze => {
                bail!("tool apply is not supported for {:?}", active_tool)
            }
        }
        Ok(())
    }

    pub(super) fn export_target(
        &mut self,
        requested: Option<&Path>,
        output: Option<&Path>,
        overwrite: bool,
        gain_db: Option<f32>,
        marker_override: Option<Vec<crate::markers::MarkerEntry>>,
        loop_override: Option<(usize, usize)>,
    ) -> Result<(PathBuf, PathBuf, Vec<crate::markers::MarkerEntry>, Option<(usize, usize)>)> {
        let tab_idx = self.ensure_target_tab_loaded(requested)?;
        let (src, mut channels, buffer_sr, bit_depth, current_markers, current_loop) = {
            let tab = self.app.tabs.get(tab_idx).context("missing target tab")?;
            (
                tab.path.clone(),
                tab.ch_samples.clone(),
                tab.buffer_sample_rate.max(1),
                self.app.bit_depth_override.get(&tab.path).copied(),
                marker_override.unwrap_or_else(|| tab.markers.clone()),
                loop_override.or(tab.loop_region),
            )
        };
        if let Some(db) = gain_db.filter(|db| db.abs() > 0.0001) {
            let gain = 10.0f32.powf(db / 20.0);
            for channel in &mut channels {
                for sample in channel {
                    *sample *= gain;
                }
            }
        }
        let dst = if overwrite {
            if output.is_some() {
                bail!("--overwrite cannot be combined with --output");
            }
            src.clone()
        } else {
            let output = output.context("session export requires --output or --overwrite")?;
            absolute_output_path(output)?
        };
        if !overwrite {
            ensure_parent_dir(&dst)?;
            crate::wave::export_channels_audio_with_depth(&channels, buffer_sr, &dst, bit_depth)
                .with_context(|| format!("export audio: {} -> {}", src.display(), dst.display()))?;
            crate::wave::copy_audio_metadata_from_source(&src, &dst)
                .with_context(|| format!("copy metadata: {} -> {}", src.display(), dst.display()))?;
        } else {
            crate::wave::overwrite_audio_from_channels_with_depth(
                &channels,
                buffer_sr,
                &dst,
                self.app.export_cfg.backup_bak,
                bit_depth,
            )
            .with_context(|| format!("overwrite audio: {}", dst.display()))?;
        }
        let dst_info = crate::audio_io::read_audio_info(&dst)
            .with_context(|| format!("read output audio info: {}", dst.display()))?;
        crate::markers::write_markers(
            &dst,
            buffer_sr,
            dst_info.sample_rate.max(1),
            &current_markers,
        )
        .with_context(|| format!("write markers: {}", dst.display()))?;
        let loop_file = current_loop.and_then(|(start, end)| {
            crate::wave::map_loop_markers_to_file_sr(
                start,
                end,
                buffer_sr,
                dst_info.sample_rate.max(1),
            )
            .map(|(s, e)| (s as u64, e as u64))
        });
        crate::loop_markers::write_loop_markers(&dst, loop_file)
            .with_context(|| format!("write loop markers: {}", dst.display()))?;
        if overwrite {
            self.app
                .mark_edit_saved_for_path(&src, &current_markers, current_loop);
            self.save()?;
        }
        Ok((src, dst, current_markers, current_loop))
    }

    pub(super) fn find_tab_index(&self, path: &Path) -> Option<usize> {
        self.app.tabs.iter().position(|tab| tab.path.as_path() == path)
    }

    pub(super) fn set_pending_gain_db_for_path(&mut self, path: &Path, db: f32) {
        self.app.set_pending_gain_db_for_path(path, db);
    }

    pub(super) fn pending_gain_map(&self) -> Value {
        let mut map = Map::new();
        for item in &self.app.items {
            map.insert(item.path.display().to_string(), Value::from(item.pending_gain_db));
        }
        Value::Object(map)
    }

    fn wait_for_decode(&mut self, tab_idx: usize, target: &Path) -> Result<()> {
        let started = Instant::now();
        loop {
            self.app.drain_editor_decode();
            if let Some(tab) = self.app.tabs.get(tab_idx) {
                if !tab.loading {
                    return Ok(());
                }
            }
            if self.app.is_decode_failed_path(target) {
                bail!("decode failed: {}", target.display());
            }
            if started.elapsed() > CLI_DECODE_TIMEOUT {
                bail!("editor decode timed out: {}", target.display());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn tab_len(&self, tab_idx: usize) -> Result<usize> {
        self.app
            .tabs
            .get(tab_idx)
            .map(|tab| tab.samples_len)
            .context("missing target tab")
    }

    fn apply_trim(&mut self, tab_idx: usize) -> Result<()> {
        let range = {
            let tab = self.app.tabs.get(tab_idx).context("missing target tab")?;
            tab.trim_range.or(tab.selection)
        }
        .context("trim apply requires selection or trim range")?;
        self.app.editor_apply_trim_range(tab_idx, range);
        Ok(())
    }

    fn apply_fade(&mut self, tab_idx: usize) -> Result<()> {
        let (fade_in_ms, fade_out_ms, fade_in_shape, fade_out_shape, sample_rate, samples_len) = {
            let tab = self.app.tabs.get(tab_idx).context("missing target tab")?;
            (
                tab.tool_state.fade_in_ms.max(0.0),
                tab.tool_state.fade_out_ms.max(0.0),
                tab.fade_in_shape,
                tab.fade_out_shape,
                tab.buffer_sample_rate.max(1) as f32,
                tab.samples_len,
            )
        };
        if fade_in_ms <= 0.0 && fade_out_ms <= 0.0 {
            bail!("fade apply requires fade-in or fade-out to be greater than zero");
        }
        if fade_in_ms > 0.0 {
            let frames = ((fade_in_ms / 1000.0) * sample_rate).round() as usize;
            self.app
                .editor_apply_fade_in_explicit(tab_idx, (0, frames.min(samples_len)), fade_in_shape);
        }
        if fade_out_ms > 0.0 {
            let frames = ((fade_out_ms / 1000.0) * sample_rate).round() as usize;
            self.app
                .editor_apply_fade_out_explicit(tab_idx, (0, frames.min(samples_len)), fade_out_shape);
        }
        if let Some(tab) = self.app.tabs.get_mut(tab_idx) {
            tab.tool_state.fade_in_ms = 0.0;
            tab.tool_state.fade_out_ms = 0.0;
        }
        Ok(())
    }

    fn restore_session_snapshot(&mut self, tab_idx: usize, target: &Path) {
        let key = workspace_path_key(target);
        let Some(snapshot) = self.session_tab_snapshots.get(&key).cloned() else {
            return;
        };
        let Some(tab) = self.app.tabs.get_mut(tab_idx) else {
            return;
        };
        tab.show_waveform_overlay = snapshot.show_waveform_overlay;
        tab.snap_zero_cross = snapshot.snap_zero_cross;
        tab.active_tool = snapshot.active_tool;
        tab.tool_state = snapshot.tool_state;
        tab.loop_mode = snapshot.loop_mode;
        tab.preview_offset_samples = snapshot.preview_offset_samples;
        tab.selection = snapshot.selection;
        tab.loop_region = snapshot.loop_region;
        tab.loop_region_committed = snapshot.loop_region_committed;
        tab.loop_region_applied = snapshot.loop_region_applied;
        tab.loop_markers_saved = snapshot.loop_markers_saved;
        tab.trim_range = snapshot.trim_range;
        tab.loop_xfade_samples = snapshot.loop_xfade_samples;
        tab.loop_xfade_shape = snapshot.loop_xfade_shape;
        tab.fade_in_range = snapshot.fade_in_range;
        tab.fade_out_range = snapshot.fade_out_range;
        tab.fade_in_shape = snapshot.fade_in_shape;
        tab.fade_out_shape = snapshot.fade_out_shape;
        tab.dirty = snapshot.dirty;
        tab.markers = snapshot.markers;
        tab.markers_committed = snapshot.markers_committed;
        tab.markers_applied = snapshot.markers_applied;
        tab.markers_dirty = snapshot.markers_dirty;
        tab.loop_markers_dirty = snapshot.loop_markers_dirty;
        tab.view_offset = snapshot.view_offset;
        tab.samples_per_px = snapshot.samples_per_px;
        tab.vertical_zoom = snapshot.vertical_zoom;
        tab.vertical_view_center = snapshot.vertical_view_center;
    }
}

impl SessionTabSnapshot {
    fn from_project_tab(tab: &super::project::ProjectTab) -> Self {
        Self {
            show_waveform_overlay: tab.show_waveform_overlay,
            snap_zero_cross: tab.snap_zero_cross,
            active_tool: super::project::tool_kind_from_str(&tab.active_tool),
            tool_state: super::project::project_tool_state_to_tool_state(&tab.tool_state),
            loop_mode: super::project::loop_mode_from_str(&tab.loop_mode),
            preview_offset_samples: tab.cursor_sample,
            selection: tab.selection.map(|range| (range[0], range[1])),
            loop_region: tab.loop_region.map(|range| (range[0], range[1])),
            loop_region_committed: tab.loop_region.map(|range| (range[0], range[1])),
            loop_region_applied: tab.loop_region.map(|range| (range[0], range[1])),
            loop_markers_saved: tab.loop_region.map(|range| (range[0], range[1])),
            trim_range: tab.trim_range.map(|range| (range[0], range[1])),
            loop_xfade_samples: tab.loop_xfade_samples,
            loop_xfade_shape: super::project::loop_shape_from_str(&tab.loop_xfade_shape),
            fade_in_range: tab.fade_in_range.map(|range| (range[0], range[1])),
            fade_out_range: tab.fade_out_range.map(|range| (range[0], range[1])),
            fade_in_shape: super::project::fade_shape_from_str(&tab.fade_in_shape),
            fade_out_shape: super::project::fade_shape_from_str(&tab.fade_out_shape),
            dirty: tab.dirty,
            markers: tab
                .markers
                .iter()
                .map(|marker| MarkerEntry {
                    sample: marker.sample,
                    label: marker.label.clone(),
                })
                .collect(),
            markers_committed: tab
                .markers
                .iter()
                .map(|marker| MarkerEntry {
                    sample: marker.sample,
                    label: marker.label.clone(),
                })
                .collect(),
            markers_applied: tab
                .markers
                .iter()
                .map(|marker| MarkerEntry {
                    sample: marker.sample,
                    label: marker.label.clone(),
                })
                .collect(),
            markers_dirty: tab.markers_dirty,
            loop_markers_dirty: tab.loop_markers_dirty,
            view_offset: tab.view_offset,
            samples_per_px: tab.samples_per_px,
            vertical_zoom: tab.vertical_zoom,
            vertical_view_center: tab.vertical_view_center,
        }
    }
}

pub(super) fn resolve_playback_range(
    total_samples: usize,
    selection_requested: bool,
    selection: Option<(usize, usize)>,
    loop_requested: bool,
    loop_region: Option<(usize, usize)>,
    explicit_range: Option<(usize, usize)>,
) -> PlaybackRangeSpec {
    let whole = (0, total_samples);
    if selection_requested {
        if let Some(range) = normalize_range(selection, total_samples) {
            return PlaybackRangeSpec {
                range,
                source: PlaybackRangeSource::Selection,
            };
        }
    }
    if loop_requested {
        if let Some(range) = normalize_range(loop_region, total_samples) {
            return PlaybackRangeSpec {
                range,
                source: PlaybackRangeSource::Loop,
            };
        }
    }
    if let Some(range) = normalize_range(explicit_range, total_samples) {
        return PlaybackRangeSpec {
            range,
            source: PlaybackRangeSource::Explicit,
        };
    }
    PlaybackRangeSpec {
        range: whole,
        source: PlaybackRangeSource::Whole,
    }
}

fn workspace_path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn normalize_range(range: Option<(usize, usize)>, total_samples: usize) -> Option<(usize, usize)> {
    let (start, end) = range?;
    let total = total_samples.max(1);
    let start = start.min(total);
    let end = end.min(total);
    (end > start).then_some((start, end))
}

fn absolute_output_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn absolute_existing_path(path: &Path) -> Result<PathBuf> {
    let path = absolute_output_path(path)?;
    if !path.exists() {
        bail!("path does not exist: {}", path.display());
    }
    Ok(path)
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{resolve_playback_range, PlaybackRangeSource};

    #[test]
    fn playback_range_priority_is_selection_then_loop_then_explicit_then_whole() {
        let spec = resolve_playback_range(
            1_000,
            true,
            Some((100, 200)),
            true,
            Some((300, 400)),
            Some((500, 600)),
        );
        assert_eq!(spec.source, PlaybackRangeSource::Selection);
        assert_eq!(spec.range, (100, 200));

        let spec = resolve_playback_range(
            1_000,
            true,
            None,
            true,
            Some((300, 400)),
            Some((500, 600)),
        );
        assert_eq!(spec.source, PlaybackRangeSource::Loop);
        assert_eq!(spec.range, (300, 400));

        let spec = resolve_playback_range(1_000, false, None, false, None, Some((500, 600)));
        assert_eq!(spec.source, PlaybackRangeSource::Explicit);
        assert_eq!(spec.range, (500, 600));

        let spec = resolve_playback_range(1_000, false, None, false, None, None);
        assert_eq!(spec.source, PlaybackRangeSource::Whole);
        assert_eq!(spec.range, (0, 1_000));
    }
}
