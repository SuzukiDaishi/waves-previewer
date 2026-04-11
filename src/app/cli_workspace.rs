use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::types::{EditorTab, ToolKind};
use super::WavesPreviewer;

const CLI_DECODE_TIMEOUT: Duration = Duration::from_secs(30);
const CLI_APPLY_TIMEOUT: Duration = Duration::from_secs(60);

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
}

impl CliWorkspace {
    pub(super) fn load(session_path: &Path) -> Result<Self> {
        let session_path = absolute_existing_path(session_path)?;
        let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())
            .context("create headless workspace")?;
        app.open_project_file(session_path.clone())
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("open session: {}", session_path.display()))?;
        Ok(Self { session_path, app })
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
            return Ok(idx);
        }
        self.app.open_or_activate_tab(&target);
        let idx = self
            .find_tab_index(&target)
            .context("failed to open target tab")?;
        self.app.active_tab = Some(idx);
        self.wait_for_decode(idx, &target)?;
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

    pub(super) fn apply_loop_for_target(&mut self, requested: Option<&Path>) -> Result<()> {
        let tab_idx = self.ensure_target_tab_loaded(requested)?;
        let (apply_xfade, pending_repeat) = {
            let tab = self
                .app
                .tabs
                .get_mut(tab_idx)
                .context("missing target tab")?;
            tab.loop_region_committed = tab.loop_region;
            tab.loop_region_applied = tab.loop_region;
            let apply_xfade = tab.loop_xfade_samples > 0;
            let pending_repeat = if tab.pending_loop_unwrap.is_some() {
                tab.pending_loop_unwrap.take()
            } else {
                None
            };
            Self::update_loop_dirty(tab);
            (apply_xfade, pending_repeat)
        };
        if let Some(repeats) = pending_repeat {
            self.app.editor_apply_loop_unwrap(tab_idx, repeats);
        } else if apply_xfade {
            self.app.editor_apply_loop_xfade(tab_idx);
        }
        if let Some(tab) = self.app.tabs.get_mut(tab_idx) {
            tab.loop_xfade_samples = 0;
            tab.tool_state.loop_repeat = 2;
            Self::update_loop_dirty(tab);
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

    fn update_loop_dirty(tab: &mut EditorTab) {
        tab.loop_markers_dirty = tab.loop_region != tab.loop_markers_saved;
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
