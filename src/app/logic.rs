use crate::audio_io;
use crate::loop_markers;
use regex::RegexBuilder;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use walkdir::WalkDir;

use super::types::{
    EditorDecodeStage, EditorDecodeStrategy, EditorDecodeUiStatus, EditorTab, OfflineRenderSpec,
    ProcessingResult, ProcessingState, ProcessingTarget, RateMode, ScanMessage, SortDir, SortKey,
};

const LIST_PREVIEW_PREFIX_SECS: f32 = 0.35;
const LIST_PLAY_PREFIX_SECS_BASE: f32 = 0.6;
const LIST_PLAY_PREFIX_SECS_COMPRESSED_BASE: f32 = 1.2;
const LIST_PLAY_PREFIX_SECS_MIN: f32 = 0.25;
impl super::WavesPreviewer {
    pub(super) fn build_editor_waveform_cache(
        channels: &[Vec<f32>],
        samples_len: usize,
    ) -> (
        Vec<(f32, f32)>,
        Option<std::sync::Arc<crate::app::render::waveform_pyramid::WaveformPyramidSet>>,
    ) {
        if channels.is_empty() || samples_len == 0 {
            return (Vec::new(), None);
        }
        let (waveform_minmax, waveform_pyramid) =
            crate::app::render::waveform_pyramid::build_editor_waveform_cache(
                channels,
                samples_len,
                2048,
                crate::app::render::waveform_pyramid::DEFAULT_BASE_BIN_SAMPLES,
            );
        (waveform_minmax, Some(waveform_pyramid))
    }

    fn option_num_order(a: Option<f32>, b: Option<f32>, dir: SortDir) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (a, b) {
            (Some(va), Some(vb)) => {
                let ord = va.partial_cmp(&vb).unwrap_or(Ordering::Equal);
                match dir {
                    SortDir::Asc => ord,
                    SortDir::Desc => ord.reverse(),
                    SortDir::None => Ordering::Equal,
                }
            }
            // Unknown values are always placed at the bottom in both directions.
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (None, None) => Ordering::Equal,
        }
    }

    fn option_num_order_f64(a: Option<f64>, b: Option<f64>, dir: SortDir) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (a, b) {
            (Some(va), Some(vb)) => {
                let ord = va.partial_cmp(&vb).unwrap_or(Ordering::Equal);
                match dir {
                    SortDir::Asc => ord,
                    SortDir::Desc => ord.reverse(),
                    SortDir::None => Ordering::Equal,
                }
            }
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (None, None) => Ordering::Equal,
        }
    }

    fn string_order(a: &str, b: &str, dir: SortDir) -> std::cmp::Ordering {
        let ord = a.cmp(b);
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
            SortDir::None => std::cmp::Ordering::Equal,
        }
    }

    pub(super) fn mark_list_preview_source(&mut self, path: &Path, play_sr: u32) {
        self.playback_mark_source(
            super::PlaybackSourceKind::ListPreview(path.to_path_buf()),
            super::PlaybackTransportKind::Buffer,
            play_sr.max(1),
        );
    }

    fn try_activate_list_stream_transport(&mut self, path: &Path) -> bool {
        if !self.exact_stream_path_eligible_cached(path) {
            return false;
        }
        self.audio.stop();
        match self.audio.set_streaming_wav_path(path) {
            Ok(()) => {
                let source_sr = self
                    .audio
                    .streaming_wav_sample_rate()
                    .or_else(|| self.cached_source_sample_rate_for_path(path))
                    .unwrap_or(self.audio.shared.out_sample_rate.max(1));
                self.playing_path = Some(path.to_path_buf());
                self.audio.set_loop_enabled(false);
                self.cancel_list_preview_job();
                self.list_preview_pending_path = None;
                self.playback_mark_source(
                    super::PlaybackSourceKind::ListPreview(path.to_path_buf()),
                    super::PlaybackTransportKind::ExactStreamWav,
                    source_sr,
                );
                self.apply_effective_volume();
                true
            }
            Err(err) => {
                self.debug_log(format!(
                    "list exact stream activation failed: {} ({err})",
                    path.display()
                ));
                false
            }
        }
    }

    pub(super) fn list_preview_cached_secs(&self, sample_len: usize, play_sr: u32) -> f32 {
        sample_len as f32 / play_sr.max(1) as f32
    }

    pub(super) fn list_play_prefix_secs(&self, path: &Path) -> f32 {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let base = match ext.as_str() {
            "mp3" | "m4a" | "ogg" => LIST_PLAY_PREFIX_SECS_COMPRESSED_BASE,
            _ => LIST_PLAY_PREFIX_SECS_BASE,
        };
        if let Some(dur) = self
            .meta_for_path(path)
            .and_then(|m| m.duration_secs)
            .filter(|v| v.is_finite() && *v > 0.0)
        {
            return dur.clamp(LIST_PLAY_PREFIX_SECS_MIN, base);
        }
        base
    }

    pub(super) fn active_editor_exact_audio_ready(&self) -> bool {
        if !self.is_editor_workspace_active() {
            return true;
        }
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        (self.editor_stream_transport_eligible(tab) && self.audio.is_streaming_wav_path(&tab.path))
            || (!tab.loading && !tab.ch_samples.is_empty())
    }

    pub(super) fn editor_display_samples_len(tab: &EditorTab) -> usize {
        if tab.loading && tab.samples_len_visual > 0 {
            tab.samples_len_visual
        } else {
            tab.samples_len
        }
    }

    fn cached_source_sample_rate_for_path(&self, path: &Path) -> Option<u32> {
        self.meta_for_path(path)
            .map(|meta| meta.sample_rate)
            .filter(|v| *v > 0)
    }

    fn exact_stream_path_eligible_cached(&self, path: &Path) -> bool {
        if self.playback_mode_needs_fx_buffer() {
            return false;
        }
        if !path.is_file() {
            return false;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if ext != "wav" {
            return false;
        }
        if self.is_virtual_path(path) || self.has_pending_gain(path) {
            return false;
        }
        if self.sample_rate_override.contains_key(path)
            || self.bit_depth_override.contains_key(path)
            || self.format_override.contains_key(path)
        {
            return false;
        }
        if self
            .edited_cache
            .get(path)
            .map(|cached| cached.dirty)
            .unwrap_or(false)
        {
            return false;
        }
        if self
            .tabs
            .iter()
            .any(|tab| tab.path.as_path() == path && tab.dirty)
        {
            return false;
        }
        if matches!(
            self.item_for_path(path).map(|item| item.source),
            Some(
                crate::app::types::MediaSource::Virtual | crate::app::types::MediaSource::External
            )
        ) {
            return false;
        }
        true
    }

    pub(super) fn editor_stream_transport_eligible(&self, tab: &EditorTab) -> bool {
        !tab.dirty
            && tab.preview_audio_tool.is_none()
            && tab.preview_overlay.is_none()
            && self.exact_stream_path_eligible_cached(&tab.path)
    }

    pub(super) fn try_activate_editor_stream_transport_for_tab(&mut self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        if !self.editor_stream_transport_eligible(tab) {
            return false;
        }
        let tab_path = tab.path.clone();
        let target = ProcessingTarget::EditorTab(tab_path.clone());
        if self.audio.is_streaming_wav_path(&tab_path) {
            let source_sr = self
                .audio
                .streaming_wav_sample_rate()
                .or_else(|| self.cached_source_sample_rate_for_path(&tab_path))
                .unwrap_or(self.audio.shared.out_sample_rate.max(1));
            self.invalidate_processing_for_target(&target, "editor exact stream retained");
            self.playback_mark_source(
                super::PlaybackSourceKind::EditorTab(tab_path),
                super::PlaybackTransportKind::ExactStreamWav,
                source_sr,
            );
            if let Some(tab) = self.tabs.get(tab_idx) {
                self.apply_loop_mode_for_tab(tab);
            }
            self.apply_effective_volume();
            return true;
        }
        match self.audio.set_streaming_wav_path(&tab_path) {
            Ok(()) => {
                let source_sr = self
                    .audio
                    .streaming_wav_sample_rate()
                    .or_else(|| self.cached_source_sample_rate_for_path(&tab_path))
                    .unwrap_or(self.audio.shared.out_sample_rate.max(1));
                self.invalidate_processing_for_target(&target, "editor exact stream activated");
                self.playback_mark_source(
                    super::PlaybackSourceKind::EditorTab(tab_path),
                    super::PlaybackTransportKind::ExactStreamWav,
                    source_sr,
                );
                if let Some(tab) = self.tabs.get(tab_idx) {
                    self.apply_loop_mode_for_tab(tab);
                }
                self.apply_effective_volume();
                true
            }
            Err(err) => {
                self.debug_log(format!(
                    "editor exact stream activation failed: {} ({err})",
                    tab_path.display()
                ));
                false
            }
        }
    }

    fn next_processing_job_id(&mut self) -> u64 {
        self.processing_job_id = self.processing_job_id.wrapping_add(1).max(1);
        self.processing_job_id
    }

    pub(super) fn format_processing_target(target: &ProcessingTarget) -> String {
        format!("{}:{}", target.kind_name(), target.path().display())
    }

    pub(super) fn invalidate_processing_for_target(
        &mut self,
        target: &ProcessingTarget,
        reason: &str,
    ) {
        let Some(state) = self.processing.as_ref() else {
            return;
        };
        if state.target != *target {
            return;
        }
        self.debug_log(format!(
            "processing invalidated: job={} mode={:?} target={} reason={reason}",
            state.job_id,
            state.mode,
            Self::format_processing_target(&state.target),
        ));
        self.processing = None;
    }

    pub(super) fn processing_discard_reason(
        &self,
        state: &ProcessingState,
        res: &ProcessingResult,
    ) -> Option<String> {
        if res.job_id != state.job_id {
            return Some(format!(
                "job_id mismatch state={} result={}",
                state.job_id, res.job_id
            ));
        }
        if res.mode != state.mode {
            return Some(format!(
                "result mode mismatch state={:?} result={:?}",
                state.mode, res.mode
            ));
        }
        if res.target != state.target {
            return Some(format!(
                "result target mismatch state={} result={}",
                Self::format_processing_target(&state.target),
                Self::format_processing_target(&res.target),
            ));
        }
        if !matches!(
            res.mode,
            RateMode::Speed | RateMode::PitchShift | RateMode::TimeStretch
        ) {
            return Some(format!("unsupported processing mode {:?}", res.mode));
        }
        if self.mode != res.mode {
            return Some(format!(
                "current mode mismatch current={:?} result={:?}",
                self.mode, res.mode
            ));
        }
        match &res.target {
            ProcessingTarget::EditorTab(path) => {
                if !self.is_editor_workspace_active() {
                    return Some("editor workspace inactive".to_string());
                }
                let Some(tab_idx) = self.active_tab else {
                    return Some("no active editor tab".to_string());
                };
                let Some(tab) = self.tabs.get(tab_idx) else {
                    return Some("active editor tab missing".to_string());
                };
                if tab.path != *path {
                    return Some(format!(
                        "active editor target mismatch active={} result={}",
                        tab.path.display(),
                        path.display()
                    ));
                }
                if self.mode == RateMode::Speed
                    && self.audio.is_streaming_wav_path(path)
                    && self.editor_stream_transport_eligible(tab)
                {
                    return Some("editor exact stream active".to_string());
                }
            }
            ProcessingTarget::ListPreview(path) => {
                if !self.is_list_workspace_active() {
                    return Some("list workspace inactive".to_string());
                }
                let selected_matches = self.selected_path_buf().as_ref() == Some(path);
                let playing_matches = self.playing_path.as_ref() == Some(path);
                if !selected_matches && !playing_matches {
                    return Some(format!(
                        "list target mismatch selected={:?} playing={:?} result={}",
                        self.selected_path_buf().map(|p| p.display().to_string()),
                        self.playing_path.as_ref().map(|p| p.display().to_string()),
                        path.display()
                    ));
                }
                if self.audio.is_streaming_wav_path(path)
                    && self.exact_stream_path_eligible_cached(path)
                {
                    return Some("list exact stream active".to_string());
                }
            }
        }
        None
    }

    pub(super) fn set_editor_buffer_transport_preserving_time(
        &self,
        path: &Path,
        channels: Vec<Vec<f32>>,
        new_buffer_sr: u32,
    ) {
        let previous_buffer_sr = self.playback_session.transport_sr.max(1);
        let new_buffer_sr = new_buffer_sr.max(1);
        let same_editor_source = matches!(
            &self.playback_session.source,
            super::PlaybackSourceKind::EditorTab(src) if src.as_path() == path
        );
        if same_editor_source {
            self.audio.set_samples_channels_keep_time_pos(
                channels,
                previous_buffer_sr,
                new_buffer_sr,
            );
        } else {
            self.audio.set_samples_channels(channels);
        }
    }

    pub(super) fn spawn_playback_fx_render(&mut self, autoplay_when_ready: bool) -> bool {
        if !self.playback_mode_needs_fx_buffer() {
            return true;
        }
        let source = self.playback_session.source.clone();
        if matches!(source, super::PlaybackSourceKind::None) {
            return false;
        }
        if self.prepared_playback_fx_matches_current() {
            return true;
        }
        if self.pending_playback_fx_matches_current() {
            if let Some(state) = &mut self.playback_fx_state {
                state.autoplay_when_ready |= autoplay_when_ready;
            }
            return false;
        }
        self.clear_pending_playback_fx_render();
        let source_generation = self.playback_source_generation;
        let mode = self.mode;
        let playback_rate = self.playback_rate;
        let pitch_semitones = self.pitch_semitones;
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let job_id = self.next_playback_fx_job_id();
        let path_spec = match &source {
            super::PlaybackSourceKind::EditorTab(path)
            | super::PlaybackSourceKind::ListPreview(path) => {
                Some((path.clone(), self.offline_render_spec_for_path(path)))
            }
            _ => None,
        };
        let base_audio = self
            .playback_base_audio
            .clone()
            .filter(|audio| audio.len() > 0)
            .or_else(|| {
                self.audio
                    .shared
                    .samples
                    .load_full()
                    .filter(|audio| audio.len() > 0)
            });
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<super::PlaybackFxResult>();
        let source_for_thread = source.clone();
        std::thread::spawn(move || {
            let mut channels = if let Some(audio) = base_audio {
                audio.channels.clone()
            } else if let Some((path, spec)) = path_spec {
                match crate::audio_io::decode_audio_multi(&path) {
                    Ok((channels, in_sr)) => {
                        super::WavesPreviewer::render_channels_offline_with_spec(
                            channels, in_sr, spec, false,
                        )
                    }
                    Err(_) => return,
                }
            } else {
                return;
            };
            for channel in &mut channels {
                let original = channel.clone();
                let processed = match mode {
                    RateMode::PitchShift => crate::wave::process_pitchshift_offline(
                        channel,
                        out_sr,
                        out_sr,
                        pitch_semitones,
                    ),
                    RateMode::TimeStretch => crate::wave::process_timestretch_offline(
                        channel,
                        out_sr,
                        out_sr,
                        playback_rate,
                    ),
                    RateMode::Speed => channel.clone(),
                };
                *channel = if processed.is_empty() {
                    original
                } else {
                    processed
                };
            }
            super::WavesPreviewer::sync_channel_lengths(&mut channels);
            let audio = Arc::new(crate::audio::AudioBuffer::from_channels(channels));
            let _ = tx.send(super::PlaybackFxResult {
                source: source_for_thread,
                source_generation,
                job_id,
                mode,
                playback_rate,
                pitch_semitones,
                buffer_sr: out_sr,
                audio,
            });
        });
        self.playback_fx_state = Some(super::PlaybackFxRenderState {
            source,
            source_generation,
            job_id,
            mode,
            playback_rate,
            pitch_semitones,
            autoplay_when_ready,
            rx,
        });
        false
    }

    pub(super) fn apply_ready_playback_fx_audio(
        &mut self,
        source: super::PlaybackSourceKind,
        audio: Arc<crate::audio::AudioBuffer>,
        buffer_sr: u32,
        mode: RateMode,
        playback_rate: f32,
        source_time_sec: Option<f64>,
        resume_after_apply: bool,
    ) {
        self.prepared_playback_fx_audio = Some(audio.clone());
        self.prepared_playback_fx_generation = self.playback_source_generation;
        self.prepared_playback_fx_mode = Some(mode);
        self.prepared_playback_fx_rate = playback_rate;
        self.prepared_playback_fx_pitch = self.pitch_semitones;
        self.audio.stop();
        self.audio.set_samples_buffer(audio);
        self.playback_session.source = source.clone();
        self.playback_session.transport = super::PlaybackTransportKind::Buffer;
        self.playback_session.transport_sr = buffer_sr.max(1);
        self.playback_set_applied_mapping(
            mode,
            match mode {
                RateMode::Speed => playback_rate.max(0.25),
                RateMode::PitchShift => 1.0,
                RateMode::TimeStretch => playback_rate.max(0.25),
            },
        );
        self.playback_refresh_rate_for_current_source();
        self.playback_session.last_applied_master_gain_db = f32::NAN;
        self.playback_session.last_applied_file_gain_db = f32::NAN;
        self.apply_effective_volume();
        if let Some(source_time_sec) = source_time_sec {
            self.playback_seek_to_source_time_with(mode, playback_rate, source_time_sec);
        }
        if let super::PlaybackSourceKind::EditorTab(path) = &source {
            if let Some(tab) = self.tabs.iter().find(|tab| &tab.path == path) {
                self.apply_loop_mode_for_tab(tab);
            }
        }
        if resume_after_apply {
            self.audio.play();
            if let super::PlaybackSourceKind::ListPreview(path) = &source {
                self.debug_mark_list_play_start(path);
            }
        }
    }

    fn restore_current_playback_source_after_fx(
        &mut self,
        source_time_sec: Option<f64>,
        resume_after_restore: bool,
    ) {
        self.clear_prepared_playback_fx();
        let source = self.playback_session.source.clone();
        match source {
            super::PlaybackSourceKind::EditorTab(path) => {
                if let Some(tab_idx) = self.tabs.iter().position(|tab| tab.path == path) {
                    self.preview_restore_audio_for_tab(tab_idx);
                    if let Some(tab) = self.tabs.get(tab_idx) {
                        self.apply_loop_mode_for_tab(tab);
                    }
                } else if let Some(base) = self.playback_base_audio.clone() {
                    self.audio.stop();
                    self.audio.set_samples_buffer(base);
                    self.playback_mark_buffer_source(
                        super::PlaybackSourceKind::EditorTab(path.clone()),
                        self.audio.shared.out_sample_rate.max(1),
                    );
                    if let Some(tab) = self.tabs.iter().find(|tab| tab.path == path) {
                        self.apply_loop_mode_for_tab(tab);
                    }
                }
            }
            super::PlaybackSourceKind::ListPreview(path) => {
                if self.try_activate_list_stream_transport(&path) {
                    // transport activation reapplies effective volume internally
                } else if let Some(base) = self.playback_base_audio.clone() {
                    self.audio.stop();
                    self.audio.set_samples_buffer(base);
                    self.mark_list_preview_source(&path, self.audio.shared.out_sample_rate.max(1));
                    self.audio.set_loop_enabled(false);
                    self.apply_effective_volume();
                } else if let Some(row_idx) = self.row_for_path(&path) {
                    self.select_and_load(row_idx, false);
                }
            }
            super::PlaybackSourceKind::EffectGraph | super::PlaybackSourceKind::ToolPreview => {
                if let Some(base) = self.playback_base_audio.clone() {
                    self.audio.stop();
                    self.audio.set_samples_buffer(base);
                    self.playback_mark_buffer_source(
                        source.clone(),
                        self.audio.shared.out_sample_rate.max(1),
                    );
                    self.apply_effective_volume();
                }
            }
            super::PlaybackSourceKind::None => {}
        }
        self.playback_set_applied_mapping(self.mode, self.playback_live_mapping_rate());
        self.playback_refresh_rate_for_current_source();
        if let Some(source_time_sec) = source_time_sec {
            self.playback_seek_to_source_time(self.mode, source_time_sec);
        }
        if resume_after_restore {
            self.audio.play();
            if let super::PlaybackSourceKind::ListPreview(path) =
                self.playback_session.source.clone()
            {
                self.debug_mark_list_play_start(&path);
            }
        }
    }

    pub(super) fn refresh_playback_mode_for_current_source(
        &mut self,
        prev_mode: RateMode,
        prev_playback_rate: f32,
    ) {
        let source_time_sec =
            self.playback_current_source_time_sec_with(prev_mode, prev_playback_rate);
        let was_playing = self.playback_is_playing_now();
        if self.playback_mode_needs_fx_buffer() {
            let _ = self.spawn_playback_fx_render(was_playing);
            self.playback_refresh_rate_for_current_source();
            return;
        }
        self.clear_pending_playback_fx_render();
        if self.prepared_playback_fx_audio.is_some() {
            self.restore_current_playback_source_after_fx(source_time_sec, was_playing);
        } else {
            self.playback_set_applied_mapping(self.mode, self.playback_live_mapping_rate());
            self.playback_refresh_rate_for_current_source();
        }
    }

    pub(super) fn request_workspace_play_toggle(&mut self) {
        if self.is_list_workspace_active() {
            let now_playing = self
                .audio
                .shared
                .playing
                .load(std::sync::atomic::Ordering::Relaxed);
            if now_playing {
                self.audio.stop();
                self.list_play_pending = false;
            } else if self.force_load_selected_list_preview_for_play() {
                if self.playback_mode_needs_fx_buffer() && !self.spawn_playback_fx_render(true) {
                    self.list_play_pending = true;
                    return;
                }
                self.audio.play();
                if let Some(path) = self.selected_path_buf() {
                    self.debug_mark_list_play_start(&path);
                }
            } else {
                if self.playback_mode_needs_fx_buffer() {
                    if let Some(path) = self.selected_path_buf() {
                        self.playing_path = Some(path.clone());
                        self.audio.set_loop_enabled(false);
                        self.audio.stop();
                        self.playback_mark_source_without_buffer(
                            super::PlaybackSourceKind::ListPreview(path.clone()),
                            super::PlaybackTransportKind::Buffer,
                            self.audio.shared.out_sample_rate.max(1),
                        );
                        self.list_play_pending = true;
                        if self.spawn_playback_fx_render(true) {
                            self.audio.play();
                            self.list_play_pending = false;
                            self.debug_mark_list_play_start(&path);
                        }
                        return;
                    }
                }
                self.list_play_pending = true;
            }
            return;
        }
        if self.is_editor_workspace_active() && !self.active_editor_exact_audio_ready() {
            self.audio.stop();
            if let Some(tab_idx) = self.active_tab {
                if let Some(tab) = self.tabs.get(tab_idx) {
                    self.debug_log(format!(
                        "editor play blocked until exact audio is ready: {}",
                        tab.path.display()
                    ));
                }
            }
            return;
        }
        if self.is_editor_workspace_active() {
            let now_playing = self
                .audio
                .shared
                .playing
                .load(std::sync::atomic::Ordering::Relaxed);
            if now_playing {
                self.audio.stop();
                self.playback_return_editor_to_last_start_if_needed();
            } else {
                self.playback_capture_editor_start_display_sample();
                if self.playback_mode_needs_fx_buffer() && !self.spawn_playback_fx_render(true) {
                    self.playback_sync_state_snapshot();
                    return;
                }
                self.audio.play();
            }
            self.playback_sync_state_snapshot();
            return;
        }
        let now_playing = self
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        if now_playing {
            self.audio.stop();
        } else {
            let needs_playback_fx = matches!(
                self.playback_session.source,
                super::PlaybackSourceKind::EffectGraph | super::PlaybackSourceKind::ToolPreview
            ) && self.playback_mode_needs_fx_buffer();
            if needs_playback_fx && !self.spawn_playback_fx_render(true) {
                self.playback_sync_state_snapshot();
                return;
            }
            self.audio.play();
        }
        self.playback_sync_state_snapshot();
    }

    pub(super) fn editor_decode_strategy(path: &Path) -> EditorDecodeStrategy {
        match path.extension().and_then(|s| s.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("mp3") || ext.eq_ignore_ascii_case("ogg") => {
                EditorDecodeStrategy::CompressedProgressiveFull
            }
            _ => EditorDecodeStrategy::StreamingOverviewFinalAudio,
        }
    }

    pub(super) fn convert_source_frames_to_output_frames(
        source_frames: usize,
        source_sr: u32,
        out_sr: u32,
    ) -> usize {
        if source_frames == 0 {
            return 0;
        }
        let source_sr = source_sr.max(1) as u128;
        let out_sr = out_sr.max(1) as u128;
        (((source_frames as u128)
            .saturating_mul(out_sr)
            .saturating_add(source_sr / 2))
            / source_sr) as usize
    }

    pub(super) fn estimate_editor_total_source_frames_cached(&self, path: &Path) -> Option<usize> {
        self.meta_for_path(path)
            .and_then(|meta| meta.total_frames)
            .map(|v| v as usize)
            .filter(|v| *v > 0)
    }

    pub(super) fn process_editor_decode_channels(
        mut chans: Vec<Vec<f32>>,
        in_sr: u32,
        out_sr: u32,
        target_sr: Option<u32>,
        bit_depth: Option<crate::wave::WavBitDepth>,
        resample_quality: crate::wave::ResampleQuality,
    ) -> Vec<Vec<f32>> {
        let target_sr = target_sr.filter(|v| *v > 0).map(|v| v.max(1));
        let needs_resample = match target_sr {
            Some(target_sr) => in_sr != target_sr || target_sr != out_sr,
            None => in_sr != out_sr,
        };
        let needs_quantize = bit_depth.is_some();
        if !needs_resample && !needs_quantize {
            return chans;
        }
        if let Some(target_sr) = target_sr {
            if in_sr != target_sr {
                chans = crate::wave::resample_channels_quality(
                    &chans,
                    in_sr,
                    target_sr,
                    resample_quality,
                );
            }
            if target_sr != out_sr {
                chans = crate::wave::resample_channels_quality(
                    &chans,
                    target_sr,
                    out_sr,
                    resample_quality,
                );
            }
        } else if in_sr != out_sr {
            chans = crate::wave::resample_channels_quality(&chans, in_sr, out_sr, resample_quality);
        }
        if let Some(depth) = bit_depth {
            crate::wave::quantize_channels_in_place(&mut chans, depth);
        }
        chans
    }

    pub(super) fn estimate_editor_total_frames_cached(
        &self,
        path: &Path,
        out_sr: u32,
    ) -> Option<usize> {
        if let Some(meta) = self.meta_for_path(path) {
            if let Some(source_frames) = meta.total_frames.filter(|v| *v > 0) {
                return Some(
                    Self::convert_source_frames_to_output_frames(
                        source_frames as usize,
                        meta.sample_rate.max(1),
                        out_sr,
                    )
                    .max(1),
                );
            }
            if let Some(secs) = meta.duration_secs.filter(|v| v.is_finite() && *v > 0.0) {
                return Some(((secs * out_sr.max(1) as f32).round() as usize).max(1));
            }
        }
        None
    }

    pub(super) fn initial_editor_loading_overview(&self, path: &Path) -> Vec<(f32, f32)> {
        if let Some(meta) = self.meta_for_path(path) {
            if !meta.thumb.is_empty() {
                return meta.thumb.clone();
            }
        }
        vec![(0.0, 0.0); 128]
    }

    pub(super) fn build_loading_overview_from_channels(
        channels: &[Vec<f32>],
    ) -> Vec<(f32, f32)> {
        crate::wave::build_waveform_minmax_from_channels(
            channels,
            channels.first().map(|ch| ch.len()).unwrap_or(0),
            crate::app::render::waveform_pyramid::DEFAULT_LOADING_OVERVIEW_BINS,
        )
    }

    pub(crate) fn editor_decode_ui_status(
        &self,
        path_filter: Option<&Path>,
    ) -> Option<EditorDecodeUiStatus> {
        let state = self.editor_decode_state.as_ref()?;
        if let Some(path) = path_filter {
            if state.path != path {
                return None;
            }
        }
        let message = match state.stage {
            EditorDecodeStage::Preview => "Loading display overview",
            EditorDecodeStage::StreamingFull => "Loading exact audio",
            EditorDecodeStage::FinalizingAudio => "Finalizing exact audio",
            EditorDecodeStage::FinalizingWaveform => "Finalizing waveform",
        };
        let progress = match state.stage {
            EditorDecodeStage::Preview => {
                if state.partial_ready {
                    0.15
                } else if state.loading_waveform_updates > 0 {
                    0.08
                } else if let Some(total) = state.estimated_total_frames.filter(|v| *v > 0) {
                    ((state.decoded_frames as f32 / total as f32) * 0.15).clamp(0.01, 0.15)
                } else {
                    0.03
                }
            }
            EditorDecodeStage::StreamingFull => {
                if let Some(total) = state.total_source_frames.filter(|v| *v > 0) {
                    0.15 + 0.77
                        * (state.decoded_source_frames as f32 / total as f32).clamp(0.0, 1.0)
                } else if let Some(total) = state.estimated_total_frames.filter(|v| *v > 0) {
                    0.15 + 0.77 * (state.decoded_frames as f32 / total as f32).clamp(0.0, 1.0)
                } else {
                    0.80
                }
            }
            EditorDecodeStage::FinalizingAudio => 0.95,
            EditorDecodeStage::FinalizingWaveform => 0.99,
        };
        Some(EditorDecodeUiStatus {
            message: message.to_string(),
            progress,
            show_percentage: true,
        })
    }

    fn mixdown_channels_mono(chs: &[Vec<f32>], len: usize) -> Vec<f32> {
        if len == 0 {
            return Vec::new();
        }
        if chs.is_empty() {
            return vec![0.0; len];
        }
        let chn = chs.len() as f32;
        let mut out = vec![0.0f32; len];
        for ch in chs {
            for i in 0..len {
                if let Some(&v) = ch.get(i) {
                    out[i] += v;
                }
            }
        }
        for v in &mut out {
            *v /= chn;
        }
        out
    }

    fn sync_channel_lengths(channels: &mut Vec<Vec<f32>>) {
        let max_len = channels
            .iter()
            .map(|channel| channel.len())
            .max()
            .unwrap_or(0);
        for channel in channels {
            if channel.len() < max_len {
                channel.resize(max_len, 0.0);
            }
        }
    }

    pub(super) fn apply_sample_rate_preview_for_path(
        &mut self,
        path: &Path,
        channels: &mut Vec<Vec<f32>>,
        in_sr: u32,
    ) {
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let target = self
            .sample_rate_override
            .get(path)
            .copied()
            .filter(|v| *v > 0);
        let resample_quality = if target.is_some() {
            crate::wave::ResampleQuality::Best
        } else {
            Self::to_wave_resample_quality(self.src_quality)
        };
        let mut did_resample = false;
        let resample_started = std::time::Instant::now();
        if in_sr != out_sr {
            *channels =
                crate::wave::resample_channels_quality(channels, in_sr, out_sr, resample_quality);
            did_resample = true;
        }
        if did_resample {
            let elapsed_ms = resample_started.elapsed().as_secs_f32() * 1000.0;
            self.debug_push_src_resample_sample(elapsed_ms);
        }
        if let Some(depth) = self.bit_depth_override.get(path).copied() {
            crate::wave::quantize_channels_in_place(channels, depth);
        }
    }

    pub(super) fn mode_requires_offline_processing(&self) -> bool {
        false
    }

    pub(super) fn offline_render_spec_for_path(&self, path: &Path) -> OfflineRenderSpec {
        OfflineRenderSpec {
            mode: RateMode::Speed,
            speed_rate: 1.0,
            pitch_semitones: 0.0,
            stretch_rate: 1.0,
            master_gain_db: 0.0,
            file_gain_db: self.pending_gain_db_for_path(path),
            out_sr: self.audio.shared.out_sample_rate.max(1),
            target_sr: self
                .sample_rate_override
                .get(path)
                .copied()
                .filter(|v| *v > 0),
            bit_depth: self.bit_depth_override.get(path).copied(),
            quality: self.src_quality,
            source_variant: self.list_preview_source_variant(path),
            loop_preview_enabled: false,
            effect_state_version: 0,
        }
    }

    pub(super) fn render_channels_offline_with_spec(
        mut channels: Vec<Vec<f32>>,
        in_sr: u32,
        spec: OfflineRenderSpec,
        apply_gain: bool,
    ) -> Vec<Vec<f32>> {
        let mut current_sr = in_sr.max(1);
        let quality = Self::to_wave_resample_quality(spec.quality);
        if let Some(target_sr) = spec.target_sr.filter(|v| *v > 0) {
            let target_sr = target_sr.max(1);
            if current_sr != target_sr {
                channels = crate::wave::resample_channels_quality(
                    &channels, current_sr, target_sr, quality,
                );
            }
            current_sr = target_sr;
        }
        if current_sr != spec.out_sr {
            channels =
                crate::wave::resample_channels_quality(&channels, current_sr, spec.out_sr, quality);
            current_sr = spec.out_sr;
        }
        match spec.mode {
            RateMode::Speed if (spec.speed_rate - 1.0).abs() > 0.0001 => {
                for channel in &mut channels {
                    *channel = crate::wave::process_speed_offline(channel, spec.speed_rate);
                }
            }
            RateMode::PitchShift if spec.pitch_semitones.abs() > 0.0001 => {
                for channel in &mut channels {
                    *channel = crate::wave::process_pitchshift_offline(
                        channel,
                        current_sr,
                        spec.out_sr,
                        spec.pitch_semitones,
                    );
                }
            }
            RateMode::TimeStretch if (spec.stretch_rate - 1.0).abs() > 0.0001 => {
                for channel in &mut channels {
                    *channel = crate::wave::process_timestretch_offline(
                        channel,
                        current_sr,
                        spec.out_sr,
                        spec.stretch_rate,
                    );
                }
            }
            _ => {}
        }
        if apply_gain {
            let gain = super::helpers::db_to_amp(spec.file_gain_db);
            if (gain - 1.0).abs() > 1.0e-6 {
                for channel in &mut channels {
                    for sample in channel {
                        *sample = (*sample * gain).clamp(-1.0, 1.0);
                    }
                }
            }
        }
        if let Some(depth) = spec.bit_depth {
            crate::wave::quantize_channels_in_place(&mut channels, depth);
        }
        channels
    }

    pub(super) fn should_skip_path(&self, path: &Path) -> bool {
        self.skip_dotfiles && Self::is_dotfile_path(path)
    }

    pub(super) fn cache_dirty_tab_at(&mut self, idx: usize) {
        let (path, cached) = {
            let Some(tab) = self.tabs.get(idx) else {
                return;
            };
            if !tab.dirty && !tab.loop_markers_dirty && !tab.markers_dirty {
                return;
            }
            let mut waveform = tab.waveform_minmax.clone();
            if waveform.is_empty() {
                let mono = Self::mixdown_channels_mono(&tab.ch_samples, tab.samples_len);
                crate::wave::build_minmax(&mut waveform, &mono, 2048);
            }
            (
                tab.path.clone(),
                crate::app::types::CachedEdit {
                    ch_samples: tab.ch_samples.clone(),
                    samples_len: tab.samples_len,
                    buffer_sample_rate: tab.buffer_sample_rate.max(1),
                    waveform_minmax: waveform,
                    waveform_pyramid: tab.waveform_pyramid.clone(),
                    display_meta: Some(Self::build_meta_from_audio(
                        &tab.ch_samples,
                        tab.buffer_sample_rate.max(1),
                        self.effective_bits_for_path(&tab.path).unwrap_or(32),
                    )),
                    dirty: tab.dirty,
                    loop_region: tab.loop_region,
                    loop_region_committed: tab.loop_region_committed,
                    loop_region_applied: tab.loop_region_applied,
                    loop_markers_saved: tab.loop_markers_saved,
                    loop_markers_dirty: tab.loop_markers_dirty,
                    markers: tab.markers.clone(),
                    markers_committed: tab.markers_committed.clone(),
                    markers_saved: tab.markers_saved.clone(),
                    markers_applied: tab.markers_applied.clone(),
                    markers_dirty: tab.markers_dirty,
                    trim_range: tab.trim_range,
                    loop_xfade_samples: tab.loop_xfade_samples,
                    loop_xfade_shape: tab.loop_xfade_shape,
                    fade_in_range: tab.fade_in_range,
                    fade_out_range: tab.fade_out_range,
                    fade_in_shape: tab.fade_in_shape,
                    fade_out_shape: tab.fade_out_shape,
                    loop_mode: tab.loop_mode,
                    bpm_enabled: tab.bpm_enabled,
                    bpm_value: tab.bpm_value,
                    bpm_user_set: tab.bpm_user_set,
                    bpm_offset_sec: tab.bpm_offset_sec,
                    snap_zero_cross: tab.snap_zero_cross,
                    tool_state: tab.tool_state,
                    active_tool: tab.active_tool,
                    plugin_fx_draft: tab.plugin_fx_draft.clone(),
                    show_waveform_overlay: tab.show_waveform_overlay,
                    applied_effect_graph: None,
                },
            )
        };
        self.edited_cache.insert(path, cached);
    }

    pub(super) fn apply_dirty_tab_audio_with_mode(&mut self, path: &Path) -> bool {
        let decode_failed = self.is_decode_failed_path(path);
        let mut render_spec = self.offline_render_spec_for_path(path);
        render_spec.master_gain_db = 0.0;
        render_spec.file_gain_db = 0.0;
        let source_time_sec = self.playback_current_source_time_sec();
        // Prefer a live dirty tab when open; otherwise fall back to cached edits.
        let idx = match self.tabs.iter().position(|t| {
            (t.dirty || t.loop_markers_dirty || t.markers_dirty) && t.path.as_path() == path
        }) {
            Some(i) => i,
            None => {
                let (channels, buffer_sr) = {
                    let cached = match self.edited_cache.get(path) {
                        Some(v) => v,
                        None => return false,
                    };
                    (cached.ch_samples.clone(), cached.buffer_sample_rate.max(1))
                };
                self.playing_path = Some(path.to_path_buf());
                if self.mode_requires_offline_processing() && !decode_failed {
                    self.audio.stop();
                    self.audio.set_samples_mono(Vec::new());
                    self.spawn_heavy_processing_from_channels(
                        path.to_path_buf(),
                        channels,
                        ProcessingTarget::EditorTab(path.to_path_buf()),
                    );
                } else {
                    let rendered = Self::render_channels_offline_with_spec(
                        channels,
                        buffer_sr,
                        render_spec,
                        false,
                    );
                    self.audio.set_samples_channels(rendered);
                    self.playback_mark_buffer_source(
                        super::PlaybackSourceKind::EditorTab(path.to_path_buf()),
                        self.audio.shared.out_sample_rate.max(1),
                    );
                    if let Some(source_time_sec) = source_time_sec {
                        self.playback_seek_to_source_time(self.mode, source_time_sec);
                    }
                }
                self.apply_effective_volume();
                return true;
            }
        };
        let (channels, tab_path, buffer_sr) = {
            let tab = &self.tabs[idx];
            (
                tab.ch_samples.clone(),
                tab.path.clone(),
                tab.buffer_sample_rate.max(1),
            )
        };
        self.playing_path = Some(tab_path.clone());
        if self.mode_requires_offline_processing() && !decode_failed {
            self.audio.stop();
            self.audio.set_samples_mono(Vec::new());
            self.spawn_heavy_processing_from_channels(
                tab_path.clone(),
                channels,
                ProcessingTarget::EditorTab(tab_path.clone()),
            );
        } else {
            let rendered =
                Self::render_channels_offline_with_spec(channels, buffer_sr, render_spec, false);
            self.audio.set_samples_channels(rendered);
            self.playback_mark_buffer_source(
                super::PlaybackSourceKind::EditorTab(tab_path.clone()),
                self.audio.shared.out_sample_rate.max(1),
            );
            if let Some(source_time_sec) = source_time_sec {
                self.playback_seek_to_source_time(self.mode, source_time_sec);
            }
        }
        if let Some(tab) = self.tabs.get(idx) {
            self.apply_loop_mode_for_tab(tab);
        }
        self.apply_effective_volume();
        true
    }

    fn reset_tab_from_virtual(&mut self, idx: usize, update_audio: bool) -> bool {
        let path = match self.tabs.get(idx) {
            Some(t) => t.path.clone(),
            None => return false,
        };
        let (display_name, audio) = {
            let Some(item) = self.item_for_path(&path) else {
                return false;
            };
            let Some(audio) = item.virtual_audio.clone() else {
                return false;
            };
            (item.display_name.clone(), audio)
        };
        let virtual_in_sr = self
            .item_for_path(&path)
            .and_then(|item| item.virtual_state.as_ref().map(|v| v.sample_rate))
            .or_else(|| self.meta_for_path(&path).map(|m| m.sample_rate))
            .filter(|v| *v > 0)
            .unwrap_or(self.audio.shared.out_sample_rate.max(1));
        let mut editor_channels = audio.channels.clone();
        self.apply_sample_rate_preview_for_path(&path, &mut editor_channels, virtual_in_sr);
        let samples_len = editor_channels.get(0).map(|c| c.len()).unwrap_or(0);
        let (waveform, waveform_pyramid) =
            Self::build_editor_waveform_cache(&editor_channels, samples_len);
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.display_name = display_name;
            tab.waveform_minmax = waveform;
            tab.waveform_pyramid = waveform_pyramid;
            tab.ch_samples = editor_channels.clone();
            tab.samples_len = samples_len;
            tab.buffer_sample_rate = self.audio.shared.out_sample_rate.max(1);
            Self::reset_tab_defaults(tab);
        }
        if update_audio {
            if self.mode_requires_offline_processing() {
                self.audio.stop();
                self.audio.set_samples_mono(Vec::new());
                self.spawn_heavy_processing_from_channels(
                    path.clone(),
                    editor_channels,
                    ProcessingTarget::EditorTab(path.clone()),
                );
            } else {
                self.audio.set_samples_channels(editor_channels);
                self.playback_mark_buffer_source(
                    super::PlaybackSourceKind::EditorTab(path.clone()),
                    self.audio.shared.out_sample_rate.max(1),
                );
            }
            self.apply_effective_volume();
        }
        true
    }

    fn apply_dirty_tab_preview_for_list(&mut self, path: &Path) -> bool {
        let mut render_spec = self.offline_render_spec_for_path(path);
        render_spec.master_gain_db = 0.0;
        render_spec.file_gain_db = 0.0;
        let source_time_sec = self.playback_current_source_time_sec();
        // List preview prioritizes dirty tab audio, then cached edits.
        let idx = match self.tabs.iter().position(|t| {
            (t.dirty || t.loop_markers_dirty || t.markers_dirty) && t.path.as_path() == path
        }) {
            Some(i) => i,
            None => {
                let (channels, buffer_sr) = {
                    let cached = match self.edited_cache.get(path) {
                        Some(v) => v,
                        None => return false,
                    };
                    (cached.ch_samples.clone(), cached.buffer_sample_rate.max(1))
                };
                self.playing_path = Some(path.to_path_buf());
                self.audio.set_loop_enabled(false);
                self.cancel_list_preview_job();
                self.list_play_pending = false;
                if self.mode_requires_offline_processing() {
                    self.audio.stop();
                    self.audio.set_samples_mono(Vec::new());
                    self.spawn_heavy_processing_from_channels(
                        path.to_path_buf(),
                        channels,
                        ProcessingTarget::ListPreview(path.to_path_buf()),
                    );
                } else {
                    let rendered = Self::render_channels_offline_with_spec(
                        channels,
                        buffer_sr,
                        render_spec,
                        false,
                    );
                    self.audio.set_samples_channels(rendered);
                    self.mark_list_preview_source(path, self.audio.shared.out_sample_rate.max(1));
                    self.audio.stop();
                    if let Some(source_time_sec) = source_time_sec {
                        self.playback_seek_to_source_time(self.mode, source_time_sec);
                    }
                }
                self.apply_effective_volume();
                return true;
            }
        };
        let (channels, buffer_sr) = {
            let tab = &self.tabs[idx];
            (tab.ch_samples.clone(), tab.buffer_sample_rate.max(1))
        };
        self.playing_path = Some(path.to_path_buf());
        self.audio.set_loop_enabled(false);
        self.cancel_list_preview_job();
        self.list_play_pending = false;
        if self.mode_requires_offline_processing() {
            self.audio.stop();
            self.audio.set_samples_mono(Vec::new());
            self.spawn_heavy_processing_from_channels(
                path.to_path_buf(),
                channels,
                ProcessingTarget::ListPreview(path.to_path_buf()),
            );
        } else {
            let rendered =
                Self::render_channels_offline_with_spec(channels, buffer_sr, render_spec, false);
            self.audio.set_samples_channels(rendered);
            self.mark_list_preview_source(path, self.audio.shared.out_sample_rate.max(1));
            self.audio.stop();
            if let Some(source_time_sec) = source_time_sec {
                self.playback_seek_to_source_time(self.mode, source_time_sec);
            }
        }
        self.apply_effective_volume();
        true
    }

    pub(super) fn spawn_heavy_processing_from_channels(
        &mut self,
        path: PathBuf,
        channels: Vec<Vec<f32>>,
        target: ProcessingTarget,
    ) {
        if !self.mode_requires_offline_processing() {
            self.debug_log(format!(
                "processing spawn skipped: mode={:?} target={}",
                self.mode,
                Self::format_processing_target(&target),
            ));
            return;
        }
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<ProcessingResult>();
        let job_id = self.next_processing_job_id();
        let mode = self.mode;
        let mut render_spec = self.offline_render_spec_for_path(&path);
        render_spec.master_gain_db = 0.0;
        render_spec.file_gain_db = 0.0;
        let source_time_sec = match &self.playback_session.source {
            super::PlaybackSourceKind::EditorTab(src)
            | super::PlaybackSourceKind::ListPreview(src)
                if src == &path =>
            {
                self.playback_current_source_time_sec()
            }
            _ => None,
        };
        let path_for_thread = path.clone();
        let target_for_thread = target.clone();
        std::thread::spawn(move || {
            let processed = Self::render_channels_offline_with_spec(
                channels,
                render_spec.out_sr,
                render_spec,
                false,
            );
            let len = processed.get(0).map(|c| c.len()).unwrap_or(0);
            let samples = Self::mixdown_channels_mono(&processed, len);
            let mut waveform = Vec::new();
            crate::wave::build_minmax(&mut waveform, &samples, 2048);
            let _ = tx.send(ProcessingResult {
                path: path_for_thread,
                job_id,
                mode,
                target: target_for_thread,
                samples,
                waveform,
                channels: processed,
            });
        });
        self.debug_log(format!(
            "processing spawn: job={} mode={:?} target={}",
            job_id,
            mode,
            Self::format_processing_target(&target),
        ));
        self.processing = Some(ProcessingState {
            msg: match mode {
                RateMode::PitchShift => "Pitch-shifting...".to_string(),
                RateMode::TimeStretch => "Time-stretching...".to_string(),
                RateMode::Speed => "Processing...".to_string(),
            },
            path,
            job_id,
            mode,
            target,
            autoplay_when_ready: false,
            source_time_sec,
            started_at: std::time::Instant::now(),
            rx,
        });
    }

    pub(super) fn has_edits_for_path(&self, path: &std::path::Path) -> bool {
        self.has_pending_gain(path)
            || self.sample_rate_override.contains_key(path)
            || self.bit_depth_override.contains_key(path)
            || self.format_override.contains_key(path)
            || self
                .edited_cache
                .get(path)
                .map(|c| c.dirty || c.loop_markers_dirty || c.markers_dirty)
                .unwrap_or(false)
            || self.tabs.iter().any(|t| {
                (t.dirty || t.loop_markers_dirty || t.markers_dirty) && t.path.as_path() == path
            })
    }

    pub(super) fn has_edits_for_paths(&self, paths: &[PathBuf]) -> bool {
        paths.iter().any(|p| self.has_edits_for_path(p))
    }

    pub(super) fn sort_key_uses_meta(&self) -> bool {
        self.sort_dir != SortDir::None
            && matches!(
                self.sort_key,
                SortKey::Length
                    | SortKey::Channels
                    | SortKey::SampleRate
                    | SortKey::Bits
                    | SortKey::BitRate
                    | SortKey::Level
                    | SortKey::Lufs
                    | SortKey::Bpm
                    | SortKey::CreatedAt
                    | SortKey::ModifiedAt
            )
    }

    pub(super) fn sort_key_uses_transcript(&self) -> bool {
        self.sort_dir != SortDir::None && matches!(self.sort_key, SortKey::Transcript)
    }

    fn reset_tab_defaults(tab: &mut EditorTab) {
        tab.view_offset = 0;
        tab.view_offset_exact = 0.0;
        tab.samples_per_px = 0.0;
        tab.vertical_zoom = 1.0;
        tab.vertical_view_center = 0.0;
        tab.last_wave_w = 0.0;
        tab.last_amplitude_nav_rect = None;
        tab.last_amplitude_viewport_rect = None;
        tab.last_amplitude_nav_click_at = 0.0;
        tab.last_amplitude_nav_click_pos = None;
        Self::invalidate_editor_viewport_cache(tab);
        tab.dirty = false;
        tab.ops.clear();
        tab.selection = None;
        tab.markers.clear();
        tab.markers_committed.clear();
        tab.markers_saved.clear();
        tab.markers_applied.clear();
        tab.markers_dirty = false;
        tab.ab_loop = None;
        tab.loop_region = None;
        tab.loop_region_committed = None;
        tab.loop_region_applied = None;
        tab.loop_markers_saved = None;
        tab.loop_markers_dirty = false;
        tab.trim_range = None;
        tab.loop_xfade_samples = 0;
        tab.loop_xfade_shape = crate::app::types::LoopXfadeShape::EqualPower;
        tab.fade_in_range = None;
        tab.fade_out_range = None;
        tab.fade_in_shape = crate::app::types::FadeShape::SCurve;
        tab.fade_out_shape = crate::app::types::FadeShape::SCurve;
        tab.set_leaf_view_mode(crate::app::types::ViewMode::Waveform);
        tab.snap_zero_cross = true;
        tab.selection_anchor_sample = None;
        tab.right_drag_mode = None;
        tab.active_tool = crate::app::types::ToolKind::LoopEdit;
        tab.tool_state = crate::app::types::ToolState {
            fade_in_ms: 0.0,
            fade_out_ms: 0.0,
            gain_db: 0.0,
            normalize_target_db: -6.0,
            loudness_target_lufs: -14.0,
            pitch_semitones: 0.0,
            stretch_rate: 1.0,
            loop_repeat: 2,
        };
        tab.loop_mode = crate::app::types::LoopMode::Off;
        tab.dragging_marker = None;
        tab.preview_audio_tool = None;
        tab.active_tool_last = None;
        tab.preview_offset_samples = None;
        tab.preview_overlay = None;
        tab.plugin_fx_draft = crate::app::types::PluginFxDraft::default();
        tab.pending_loop_unwrap = None;
        tab.undo_stack.clear();
        tab.undo_bytes = 0;
        tab.redo_stack.clear();
        tab.redo_bytes = 0;
    }

    fn reset_tab_from_disk(&mut self, idx: usize, update_audio: bool) -> bool {
        let path = match self.tabs.get(idx) {
            Some(t) => t.path.clone(),
            None => return false,
        };
        if !path.is_file() {
            self.remove_missing_path(&path);
            return false;
        }
        // Rebuild editor tab state from on-disk audio.
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string();
        let out_sr = self.audio.shared.out_sample_rate;
        let (mut chs, in_sr) = match crate::wave::decode_wav_multi(&path) {
            Ok(v) => v,
            Err(_) => (Vec::new(), out_sr),
        };
        if in_sr != out_sr {
            for c in chs.iter_mut() {
                *c = self.resample_mono_with_quality(c, in_sr, out_sr);
            }
        }
        let samples_len = chs.get(0).map(|c| c.len()).unwrap_or(0);
        let file_sr = self.sample_rate_for_path(&path, in_sr);
        let waveform_cache =
            if !self.mode_requires_offline_processing() && !chs.is_empty() && samples_len > 0 {
                Some(Self::build_editor_waveform_cache(&chs, samples_len))
            } else {
                None
            };
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.display_name = name;
            if let Some((waveform, waveform_pyramid)) = waveform_cache {
                tab.waveform_minmax = waveform;
                tab.waveform_pyramid = waveform_pyramid;
            } else {
                tab.waveform_minmax.clear();
                tab.waveform_pyramid = None;
            }
            tab.ch_samples = chs;
            tab.samples_len = samples_len;
            tab.buffer_sample_rate = out_sr.max(1);
            Self::reset_tab_defaults(tab);
            Self::set_loop_region_from_file_markers(tab, &path, in_sr, out_sr);
            Self::load_markers_for_tab(tab, &path, out_sr, file_sr);
        }
        if update_audio {
            self.playing_path = Some(path.clone());
            let source_time_sec = self.playback_current_source_time_sec();
            if self.try_activate_editor_stream_transport_for_tab(idx) {
                if let Some(source_time_sec) = source_time_sec {
                    self.playback_seek_to_source_time(self.mode, source_time_sec);
                }
                return true;
            }
            if self.mode_requires_offline_processing() {
                self.audio.stop();
                self.audio.set_samples_mono(Vec::new());
                self.spawn_heavy_processing(&path, ProcessingTarget::EditorTab(path.clone()));
            } else if let Some((channels, buffer_sr)) = self
                .tabs
                .get(idx)
                .map(|tab| (tab.ch_samples.clone(), tab.buffer_sample_rate.max(1)))
            {
                let mut render_spec = self.offline_render_spec_for_path(&path);
                render_spec.master_gain_db = 0.0;
                render_spec.file_gain_db = 0.0;
                let rendered = Self::render_channels_offline_with_spec(
                    channels,
                    buffer_sr,
                    render_spec,
                    false,
                );
                self.audio.set_samples_channels(rendered);
                self.playback_mark_buffer_source(
                    super::PlaybackSourceKind::EditorTab(path.clone()),
                    self.audio.shared.out_sample_rate.max(1),
                );
                if let Some(source_time_sec) = source_time_sec {
                    self.playback_seek_to_source_time(self.mode, source_time_sec);
                }
                if let Some(tab) = self.tabs.get(idx) {
                    self.apply_loop_mode_for_tab(tab);
                }
            }
            self.apply_effective_volume();
        }
        true
    }

    pub(super) fn clear_edits_for_paths(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let mut unique: HashSet<PathBuf> = HashSet::new();
        let mut unique_paths: Vec<PathBuf> = Vec::new();
        let mut reload_playing = false;
        let mut affect_playing = false;
        for p in paths {
            if !unique.insert(p.clone()) {
                continue;
            }
            unique_paths.push(p.clone());
        }
        unique_paths.sort();
        unique_paths.dedup();
        let before = self.capture_list_selection_snapshot();
        let before_items = self.capture_list_undo_items_by_paths(&unique_paths);
        for p in &unique_paths {
            self.set_pending_gain_db_for_path(p, 0.0);
            self.lufs_override.remove(p);
            self.lufs_recalc_deadline.remove(p);
            self.sample_rate_override.remove(p);
            self.sample_rate_probe_cache.remove(p);
            self.bit_depth_override.remove(p);
            self.format_override.remove(p);
            self.refresh_display_name_for_path(p);
            if self.playing_path.as_ref() == Some(p) {
                affect_playing = true;
            }
            self.edited_cache.remove(p);
            if let Some(idx) = self
                .tabs
                .iter()
                .position(|t| t.path.as_path() == p.as_path())
            {
                let update_audio = self.active_tab == Some(idx);
                if self.is_virtual_path(p) {
                    self.reset_tab_from_virtual(idx, update_audio);
                } else {
                    self.reset_tab_from_disk(idx, update_audio);
                }
            }
            if self.is_list_workspace_active() && self.playing_path.as_ref() == Some(p) {
                reload_playing = true;
            }
        }
        if reload_playing {
            if let Some(p) = self.playing_path.clone() {
                if let Some(row) = self.row_for_path(&p) {
                    self.select_and_load(row, false);
                }
            }
        }
        if affect_playing {
            self.apply_effective_volume();
        }
        self.record_list_update_from_paths(&unique_paths, before_items, before);
    }

    /// Helper: read loop markers and map to given output SR, set tab.loop_region if valid
    pub(super) fn set_loop_region_from_file_markers(
        tab: &mut EditorTab,
        path: &Path,
        in_sr: u32,
        out_sr: u32,
    ) {
        let mut saved = None;
        if let Some((ls, le)) = loop_markers::read_loop_markers(path) {
            let ls = (ls.min(u32::MAX as u64)) as u32;
            let le = (le.min(u32::MAX as u64)) as u32;
            if let Some((s, e)) =
                crate::wave::map_loop_markers_between_sr(ls, le, in_sr, out_sr, tab.samples_len)
            {
                tab.loop_region = Some((s, e));
                tab.loop_region_applied = Some((s, e));
                saved = Some((s, e));
            } else {
                tab.loop_region = None;
                tab.loop_region_applied = None;
            }
        } else {
            tab.loop_region = None;
            tab.loop_region_applied = None;
        }
        tab.loop_region_committed = tab.loop_region;
        tab.loop_markers_saved = saved;
        tab.loop_markers_dirty = false;
    }

    pub(super) fn sample_rate_for_path(&mut self, path: &Path, fallback: u32) -> u32 {
        if let Some(sr) = self
            .meta_for_path(path)
            .map(|m| m.sample_rate)
            .filter(|&sr| sr > 0)
        {
            self.sample_rate_probe_cache.insert(path.to_path_buf(), sr);
            return sr;
        }
        if let Some(sr) = self.sample_rate_probe_cache.get(path).copied() {
            return sr;
        }
        let probe_started = std::time::Instant::now();
        let sr = audio_io::read_audio_info(path)
            .ok()
            .map(|i| i.sample_rate)
            .filter(|v| *v > 0)
            .unwrap_or(fallback.max(1));
        let elapsed_ms = probe_started.elapsed().as_secs_f32() * 1000.0;
        self.debug_push_metadata_probe_sample(elapsed_ms);
        self.sample_rate_probe_cache.insert(path.to_path_buf(), sr);
        sr
    }

    pub(super) fn load_markers_for_tab(
        tab: &mut EditorTab,
        path: &Path,
        out_sr: u32,
        file_sr: u32,
    ) {
        let out_sr = out_sr.max(1);
        match crate::markers::read_markers(path, out_sr, file_sr) {
            Ok(mut markers) => {
                markers.retain(|m| m.sample <= tab.samples_len);
                tab.markers = markers.clone();
                tab.markers_committed = markers.clone();
                tab.markers_saved = markers;
                tab.markers_applied = tab.markers_committed.clone();
                tab.markers_dirty = false;
            }
            Err(err) => {
                eprintln!("read markers failed {}: {err:?}", path.display());
                tab.markers.clear();
                tab.markers_committed.clear();
                tab.markers_saved.clear();
                tab.markers_applied.clear();
                tab.markers_dirty = false;
            }
        }
    }

    pub(super) fn write_markers_for_tab(&mut self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let path = tab.path.clone();
        if !path.is_file() {
            self.remove_missing_path(&path);
            return false;
        }
        // Non-destructive: keep in memory and defer file writes until Save Selected.
        self.debug_log(format!(
            "markers queued for save (path: {})",
            path.display()
        ));
        true
    }

    pub(super) fn write_loop_markers_for_tab(&mut self, tab_idx: usize) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let path = tab.path.clone();
        if !path.is_file() {
            self.remove_missing_path(&path);
            return false;
        }
        // Non-destructive: keep in memory and defer file writes until Save Selected.
        self.debug_log(format!(
            "loop markers queued for save (path: {})",
            path.display()
        ));
        true
    }

    pub(super) fn mark_edit_saved_for_path(&mut self, path: &Path) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.path.as_path() == path) {
            tab.dirty = false;
            tab.markers_saved = tab.markers_committed.clone();
            tab.markers_applied = tab.markers_committed.clone();
            tab.markers_dirty = false;
            tab.loop_markers_saved = tab.loop_region_committed;
            tab.loop_region_applied = tab.loop_region_committed;
            tab.loop_markers_dirty = false;
        }
        self.edited_cache.remove(path);
        self.sample_rate_override.remove(path);
        self.sample_rate_probe_cache.remove(path);
        self.bit_depth_override.remove(path);
        self.format_override.remove(path);
        self.refresh_display_name_for_path(path);
    }
    // multi-select aware selection update for list clicks (moved from app.rs)
    pub(super) fn update_selection_on_click(&mut self, row_idx: usize, mods: egui::Modifiers) {
        let len = self.files.len();
        if row_idx >= len {
            return;
        }
        if mods.shift {
            let anchor = self.select_anchor.or(self.selected).unwrap_or(row_idx);
            let (a, b) = if anchor <= row_idx {
                (anchor, row_idx)
            } else {
                (row_idx, anchor)
            };
            self.selected_multi.clear();
            for i in a..=b {
                self.selected_multi.insert(i);
            }
            self.selected = Some(row_idx);
            self.select_anchor = Some(anchor);
        } else if mods.ctrl || mods.command {
            if self.selected_multi.contains(&row_idx) {
                self.selected_multi.remove(&row_idx);
            } else {
                self.selected_multi.insert(row_idx);
            }
            self.selected = Some(row_idx);
            if self.select_anchor.is_none() {
                self.select_anchor = Some(row_idx);
            }
        } else {
            self.selected_multi.clear();
            self.selected_multi.insert(row_idx);
            self.selected = Some(row_idx);
            self.select_anchor = Some(row_idx);
        }
    }
    /// Select a row and load audio buffer accordingly.
    /// Used when any cell in the row is clicked so Space can play immediately.
    pub(super) fn select_and_load(&mut self, row_idx: usize, auto_scroll: bool) {
        if row_idx >= self.files.len() {
            return;
        }
        self.list_play_pending = false;
        self.selected = Some(row_idx);
        self.scroll_to_selected = auto_scroll;
        let Some(item_snapshot) = self.item_for_row(row_idx).cloned() else {
            return;
        };
        let p_owned = item_snapshot.path.clone();
        if item_snapshot.source == crate::app::types::MediaSource::External {
            self.selected = Some(row_idx);
            self.scroll_to_selected = auto_scroll;
            return;
        }
        let is_virtual = item_snapshot.source == crate::app::types::MediaSource::Virtual;
        if !is_virtual && !p_owned.is_file() {
            self.remove_missing_path(&p_owned);
            return;
        }
        self.debug_mark_list_select_start(&p_owned);
        if self.apply_dirty_tab_preview_for_list(&p_owned) {
            return;
        }
        let need_heavy = self.mode_requires_offline_processing();
        let decode_failed = if is_virtual {
            false
        } else {
            self.is_decode_failed_path(&p_owned)
        };
        // record as current playing target
        self.playing_path = Some(p_owned.clone());
        // stop looping for list preview
        self.audio.set_loop_enabled(false);
        self.list_play_pending = false;
        if is_virtual {
            self.cancel_list_preview_job();
            self.list_preview_pending_path = None;
            let Some(audio) = item_snapshot.virtual_audio else {
                return;
            };
            let channels = audio.channels.clone();
            if need_heavy {
                self.audio.stop();
                self.audio.set_samples_mono(Vec::new());
                self.spawn_heavy_processing_from_channels(
                    p_owned.clone(),
                    channels,
                    ProcessingTarget::ListPreview(p_owned.clone()),
                );
                self.apply_effective_volume();
                return;
            }
            let virtual_in_sr = item_snapshot
                .virtual_state
                .as_ref()
                .map(|v| v.sample_rate)
                .or_else(|| item_snapshot.meta.as_ref().map(|m| m.sample_rate))
                .filter(|v| *v > 0)
                .unwrap_or(self.audio.shared.out_sample_rate.max(1));
            let mut render_spec = self.offline_render_spec_for_path(&p_owned);
            render_spec.master_gain_db = 0.0;
            render_spec.file_gain_db = 0.0;
            let rendered = Self::render_channels_offline_with_spec(
                channels,
                virtual_in_sr,
                render_spec,
                false,
            );
            self.audio.set_samples_channels(rendered);
            self.mark_list_preview_source(&p_owned, self.audio.shared.out_sample_rate.max(1));
            self.audio.stop();
            self.apply_effective_volume();
            self.debug_mark_list_preview_ready(&p_owned);
            return;
        }
        if self.auto_play_list_nav && self.try_activate_list_stream_transport(&p_owned) {
            self.audio.play();
            self.debug_mark_list_preview_ready(&p_owned);
            self.debug_mark_list_play_start(&p_owned);
            return;
        }
        if need_heavy && !decode_failed {
            self.cancel_list_preview_job();
            self.list_preview_pending_path = None;
            self.audio.stop();
            self.audio.set_samples_mono(Vec::new());
            self.spawn_heavy_processing(&p_owned, ProcessingTarget::ListPreview(p_owned.clone()));
            self.apply_effective_volume();
            return;
        }
        // AutoPlay uses a larger dynamic prefix so playback starts quickly but
        // still has enough headroom before full decode replaces the buffer.
        let decode_secs = if self.auto_play_list_nav {
            self.list_play_prefix_secs(&p_owned)
        } else {
            LIST_PREVIEW_PREFIX_SECS
        };
        if let Some((audio, truncated, play_sr)) = self.take_cached_list_preview(&p_owned) {
            let cached_secs = self.list_preview_cached_secs(audio.len(), play_sr);
            let min_secs = decode_secs * 0.85;
            let use_cached_now = !truncated || cached_secs >= min_secs;
            if use_cached_now {
                self.audio.set_samples_buffer(audio);
                self.mark_list_preview_source(&p_owned, play_sr);
                self.audio.stop();
                self.apply_effective_volume();
                self.debug_mark_list_preview_ready(&p_owned);
            } else {
                // Cached prefix (typically 0.35s prefetch) is too short for autoplay.
                self.evict_list_preview_cache_path(&p_owned);
                self.audio.stop();
                self.audio.set_samples_mono(Vec::new());
                self.apply_effective_volume();
            }
            if truncated {
                self.list_preview_pending_path = None;
                self.spawn_list_preview_async(
                    p_owned.clone(),
                    0.0,
                    crate::app::LIST_PLAY_EMIT_SECS,
                );
                if !use_cached_now {
                    return;
                }
            }
            return;
        }
        if self.list_preview_rx.is_some() {
            if self.list_preview_partial_ready || self.auto_play_list_nav {
                // Current async job is in full-decode phase; switch immediately.
                self.cancel_list_preview_job();
                self.debug.stale_preview_cancel_count =
                    self.debug.stale_preview_cancel_count.saturating_add(1);
                self.list_preview_pending_path = None;
                self.audio.stop();
                self.audio.set_samples_mono(Vec::new());
                let emit_secs = if self.auto_play_list_nav {
                    crate::app::LIST_PLAY_EMIT_SECS
                } else {
                    0.0
                };
                self.spawn_list_preview_async(p_owned.clone(), decode_secs, emit_secs);
                self.apply_effective_volume();
                return;
            } else {
                // Prefix is not ready yet; queue only the latest requested path.
                self.list_preview_pending_path = Some(p_owned.clone());
                self.audio.stop();
                self.audio.set_samples_mono(Vec::new());
                self.apply_effective_volume();
                return;
            }
        }
        self.list_preview_pending_path = None;
        // Do list decode asynchronously so row navigation never blocks UI.
        self.audio.stop();
        self.audio.set_samples_mono(Vec::new());
        let emit_secs = if self.auto_play_list_nav {
            crate::app::LIST_PLAY_EMIT_SECS
        } else {
            0.0
        };
        self.spawn_list_preview_async(p_owned.clone(), decode_secs, emit_secs);
        // apply effective volume including per-file gain
        self.apply_effective_volume();
    }

    pub(super) fn force_load_selected_list_preview_for_play(&mut self) -> bool {
        if !self.is_list_workspace_active() {
            return false;
        }
        let selected_row = self.selected;
        let Some(path) = self.selected_path_buf() else {
            return false;
        };
        let source = self.item_for_path(&path).map(|item| item.source);
        if matches!(source, Some(crate::app::types::MediaSource::External)) {
            return false;
        }
        // Keep Space/AutoPlay behavior consistent with row-click selection:
        // edited tab/cached dirty audio must win over file decode/cache.
        if self.apply_dirty_tab_preview_for_list(&path) {
            self.debug_mark_list_preview_ready(&path);
            self.list_play_pending = false;
            return true;
        }
        let need_heavy = self.mode_requires_offline_processing();
        if matches!(source, Some(crate::app::types::MediaSource::Virtual)) {
            if let Some(row) = selected_row {
                self.select_and_load(row, false);
                if need_heavy {
                    if let Some(state) = &mut self.processing {
                        if state.path == path {
                            state.autoplay_when_ready = true;
                        }
                    }
                    self.list_play_pending = true;
                    self.debug.autoplay_pending_count =
                        self.debug.autoplay_pending_count.saturating_add(1);
                    return false;
                }
                return true;
            }
            return false;
        }
        if !path.is_file() {
            return false;
        }
        if self.try_activate_list_stream_transport(&path) {
            self.debug_mark_list_preview_ready(&path);
            self.list_play_pending = false;
            return true;
        }
        let decode_failed = self.is_decode_failed_path(&path);
        if need_heavy && !decode_failed {
            if let Some(row) = selected_row {
                self.select_and_load(row, false);
            } else {
                self.spawn_heavy_processing(&path, ProcessingTarget::ListPreview(path.clone()));
            }
            if let Some(state) = &mut self.processing {
                if state.path == path {
                    state.autoplay_when_ready = true;
                }
            }
            self.list_play_pending = true;
            self.debug.autoplay_pending_count = self.debug.autoplay_pending_count.saturating_add(1);
            return false;
        }
        let has_active_sample = self
            .debug
            .list_select_started_path
            .as_deref()
            .map(|p| p == path.as_path())
            .unwrap_or(false)
            && self.debug.list_select_started_at.is_some();
        if !has_active_sample {
            self.debug_mark_list_select_start(&path);
        }
        self.playing_path = Some(path.clone());
        let play_prefix_secs = self.list_play_prefix_secs(&path);
        if let Some((audio, truncated, play_sr)) = self.take_cached_list_preview(&path) {
            let cached_secs = self.list_preview_cached_secs(audio.len(), play_sr);
            let min_secs = play_prefix_secs * 0.85;
            let use_cached_now = !truncated || cached_secs >= min_secs;
            if use_cached_now {
                self.audio.set_samples_buffer(audio);
                self.mark_list_preview_source(&path, play_sr);
                self.audio.stop();
                self.apply_effective_volume();
                self.debug_mark_list_preview_ready(&path);
                if !truncated {
                    self.list_play_pending = false;
                    return true;
                }
                // Cached prefix is long enough: start now and continue with full decode.
                self.list_play_pending = true;
                self.debug.autoplay_pending_count =
                    self.debug.autoplay_pending_count.saturating_add(1);
                if self.list_preview_rx.is_some() {
                    self.cancel_list_preview_job();
                    self.debug.stale_preview_cancel_count =
                        self.debug.stale_preview_cancel_count.saturating_add(1);
                }
                self.list_preview_pending_path = None;
                self.spawn_list_preview_async(path.clone(), 0.0, crate::app::LIST_PLAY_EMIT_SECS);
                return true;
            }
            // Too-short cached prefix causes audible gap; decode a longer prefix instead.
            self.evict_list_preview_cache_path(&path);
        }
        self.list_play_pending = true;
        self.debug.autoplay_pending_count = self.debug.autoplay_pending_count.saturating_add(1);
        if self.list_preview_rx.is_some() {
            self.cancel_list_preview_job();
            self.debug.stale_preview_cancel_count =
                self.debug.stale_preview_cancel_count.saturating_add(1);
            self.list_preview_pending_path = None;
            self.audio.stop();
            self.audio.set_samples_mono(Vec::new());
            self.spawn_list_preview_async(path, play_prefix_secs, crate::app::LIST_PLAY_EMIT_SECS);
            self.apply_effective_volume();
            return false;
        }
        self.list_preview_pending_path = None;
        self.audio.stop();
        self.audio.set_samples_mono(Vec::new());
        self.spawn_list_preview_async(path, play_prefix_secs, crate::app::LIST_PLAY_EMIT_SECS);
        self.apply_effective_volume();
        false
    }





    pub(super) fn nudge_list_selection(&mut self, delta: isize, auto_scroll: bool) -> bool {
        if !self.is_list_workspace_active() || self.files.is_empty() {
            return false;
        }
        let len = self.files.len();
        let cur = self.selected.unwrap_or(0).min(len.saturating_sub(1));
        let target = if delta >= 0 {
            (cur + (delta as usize)).min(len.saturating_sub(1))
        } else {
            cur.saturating_sub((-delta) as usize)
        };
        if target == cur {
            return false;
        }
        self.update_selection_on_click(target, egui::Modifiers::NONE);
        self.select_and_load(target, auto_scroll);
        if self.auto_play_list_nav {
            self.request_list_autoplay();
        }
        true
    }

    pub(super) fn remove_missing_path(&mut self, path: &Path) {
        if self.is_virtual_path(path) {
            return;
        }
        if path.exists() {
            return;
        }
        let Some(id) = self.path_index.get(path).copied() else {
            return;
        };
        let selected_path = self.selected_path_buf();
        let selected_row_before = self.selected;
        let selected_removed = selected_path
            .as_ref()
            .map(|p| p.as_path() == path)
            .unwrap_or(false);
        let selected_paths: Vec<PathBuf> = self
            .selected_multi
            .iter()
            .filter_map(|&row| self.path_for_row(row).cloned())
            .collect();
        let anchor_path = self
            .select_anchor
            .and_then(|row| self.path_for_row(row).cloned());
        let path_buf = path.to_path_buf();
        let was_playing = self.playing_path.as_ref() == Some(&path_buf);

        if let Some(idx) = self.item_index.remove(&id) {
            self.items.remove(idx);
            for i in idx..self.items.len() {
                let id = self.items[i].id;
                self.item_index.insert(id, i);
            }
        }
        self.path_index.remove(&path_buf);
        self.files.retain(|&fid| fid != id);
        self.original_files.retain(|&fid| fid != id);

        self.meta_inflight.remove(&path_buf);
        self.transcript_inflight.remove(&path_buf);
        self.transcript_ai_inflight.remove(&path_buf);
        self.purge_spectro_cache_entry(&path_buf);
        self.cancel_feature_analysis_for_path(&path_buf);
        self.edited_cache.remove(&path_buf);
        self.lufs_override.remove(&path_buf);
        self.lufs_recalc_deadline.remove(&path_buf);
        self.sample_rate_override.remove(&path_buf);
        self.sample_rate_probe_cache.remove(&path_buf);
        self.bit_depth_override.remove(&path_buf);
        self.format_override.remove(&path_buf);
        self.evict_list_preview_cache_path(&path_buf);
        if was_playing {
            self.playing_path = None;
            self.cancel_list_preview_job();
            self.list_play_pending = false;
            self.audio.stop();
        }
        if !self.external_sources.is_empty() {
            self.apply_external_mapping();
        }
        self.apply_filter_from_search();
        self.apply_sort();
        self.selected = selected_path.and_then(|p| self.row_for_path(&p));
        self.selected_multi.clear();
        for p in selected_paths {
            if let Some(row) = self.row_for_path(&p) {
                self.selected_multi.insert(row);
            }
        }
        if let Some(sel) = self.selected {
            if self.selected_multi.is_empty() {
                self.selected_multi.insert(sel);
            }
        }
        self.select_anchor = anchor_path.and_then(|p| self.row_for_path(&p));
        if self.files.is_empty() {
            self.selected = None;
            self.selected_multi.clear();
            self.select_anchor = None;
        } else if self.selected.is_none() && selected_removed {
            let len = self.files.len();
            let target = selected_row_before
                .unwrap_or(0)
                .saturating_sub(1)
                .min(len.saturating_sub(1));
            self.selected = Some(target);
            self.selected_multi.clear();
            self.selected_multi.insert(target);
            self.select_anchor = Some(target);
        }
    }

    pub(super) fn remove_paths_from_list(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let unique: HashSet<PathBuf> = paths.iter().cloned().collect();
        if unique.is_empty() {
            return;
        }
        let selected_path = self.selected_path_buf();
        let selected_row_before = self.selected;
        let selected_paths: Vec<PathBuf> = self
            .selected_multi
            .iter()
            .filter_map(|&row| self.path_for_row(row).cloned())
            .collect();
        let anchor_path = self
            .select_anchor
            .and_then(|row| self.path_for_row(row).cloned());
        let was_playing = self
            .playing_path
            .as_ref()
            .map(|p| unique.contains(p))
            .unwrap_or(false);
        let selected_removed = selected_path
            .as_ref()
            .map(|p| unique.contains(p))
            .unwrap_or(false);

        let mut removed_ids = HashSet::new();
        for path in unique.iter() {
            if let Some(id) = self.path_index.get(path).copied() {
                removed_ids.insert(id);
            }
        }
        if removed_ids.is_empty() {
            return;
        }
        self.items.retain(|item| !removed_ids.contains(&item.id));
        self.rebuild_item_indexes();
        self.files.retain(|id| !removed_ids.contains(id));
        self.original_files.retain(|id| !removed_ids.contains(id));

        for path in unique.iter() {
            self.meta_inflight.remove(path);
            self.transcript_inflight.remove(path);
            self.transcript_ai_inflight.remove(path);
            self.purge_spectro_cache_entry(path);
            self.cancel_feature_analysis_for_path(path);
            self.edited_cache.remove(path);
            self.lufs_override.remove(path);
            self.lufs_recalc_deadline.remove(path);
            self.sample_rate_override.remove(path);
            self.sample_rate_probe_cache.remove(path);
            self.bit_depth_override.remove(path);
            self.format_override.remove(path);
            self.evict_list_preview_cache_path(path);
        }
        if was_playing {
            self.playing_path = None;
            self.cancel_list_preview_job();
            self.list_play_pending = false;
            self.audio.stop();
        }
        if !self.external_sources.is_empty() {
            self.apply_external_mapping();
        }
        self.apply_filter_from_search();
        self.apply_sort();
        self.selected = selected_path.and_then(|p| self.row_for_path(&p));
        self.selected_multi.clear();
        for p in selected_paths {
            if let Some(row) = self.row_for_path(&p) {
                self.selected_multi.insert(row);
            }
        }
        if let Some(sel) = self.selected {
            if self.selected_multi.is_empty() {
                self.selected_multi.insert(sel);
            }
        }
        self.select_anchor = anchor_path.and_then(|p| self.row_for_path(&p));
        if self.files.is_empty() {
            self.selected = None;
            self.selected_multi.clear();
            self.select_anchor = None;
        } else if self.selected.is_none() && selected_removed {
            let len = self.files.len();
            let target = selected_row_before
                .unwrap_or(0)
                .saturating_sub(1)
                .min(len.saturating_sub(1));
            self.selected = Some(target);
            self.selected_multi.clear();
            self.selected_multi.insert(target);
            self.select_anchor = Some(target);
        }
    }
    pub fn rescan(&mut self) {
        self.files.clear();
        self.items.clear();
        self.item_index.clear();
        self.path_index.clear();
        self.original_files.clear();
        self.meta_inflight.clear();
        self.transcript_inflight.clear();
        self.transcript_ai_inflight.clear();
        self.spectro_cache.clear();
        self.spectro_inflight.clear();
        self.spectro_progress.clear();
        self.spectro_cancel.clear();
        self.spectro_cache_order.clear();
        self.spectro_cache_sizes.clear();
        self.spectro_cache_bytes = 0;
        self.reset_all_feature_analysis_state();
        self.scan_rx = None;
        self.scan_in_progress = false;
        self.sample_rate_override.clear();
        self.sample_rate_probe_cache.clear();
        self.bit_depth_override.clear();
        self.format_override.clear();
        if let Some(root) = &self.root {
            self.start_scan_folder(root.clone());
        } else {
            self.apply_filter_from_search();
            self.apply_sort();
        }
    }





    pub(super) fn apply_filter_from_search(&mut self) {
        // Preserve selection index if possible
        let selected_idx = self.selected.and_then(|i| self.files.get(i).copied());
        let query = self.search_query.trim().to_string();
        // Search spans display name, folder, transcript, meta summary, and external fields.
        if query.is_empty() {
            self.files = self.items.iter().map(|item| item.id).collect();
        } else if self.search_use_regex {
            let re = RegexBuilder::new(&query).case_insensitive(true).build();
            if let Ok(re) = re {
                self.files = self
                    .items
                    .iter()
                    .filter(|item| {
                        let name = item.display_name.as_str();
                        let parent = item.display_folder.as_str();
                        let transcript = item
                            .transcript
                            .as_ref()
                            .map(|t| t.full_text.as_str())
                            .unwrap_or("");
                        let meta_text = item
                            .meta
                            .as_ref()
                            .map(|m| {
                                format!(
                                    "sr:{} bits:{} br:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1} bpm:{:.1}",
                                    m.sample_rate,
                                    m.bits_per_sample,
                                    m.bit_rate_bps.unwrap_or(0),
                                    m.channels,
                                    m.duration_secs.unwrap_or(0.0),
                                    m.peak_db.unwrap_or(0.0),
                                    m.lufs_i.unwrap_or(0.0),
                                    m.bpm.unwrap_or(0.0)
                                )
                            })
                            .unwrap_or_default();
                        let external_hit = item.external.values().any(|v| re.is_match(v));
                        re.is_match(name)
                            || re.is_match(parent)
                            || re.is_match(transcript)
                            || re.is_match(&meta_text)
                            || external_hit
                    })
                    .map(|item| item.id)
                    .collect();
            } else {
                // Regex parse failed; fall back to case-insensitive substring matching.
                let q = query.to_lowercase();
                self.files = self
                    .items
                    .iter()
                    .filter(|item| {
                        let name = item.display_name.to_lowercase();
                        let parent = item.display_folder.to_lowercase();
                        let transcript = item
                            .transcript
                            .as_ref()
                            .map(|t| t.full_text.to_lowercase())
                            .unwrap_or_default();
                        let meta_text = item
                            .meta
                            .as_ref()
                            .map(|m| {
                                format!(
                                    "sr:{} bits:{} br:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1} bpm:{:.1}",
                                    m.sample_rate,
                                    m.bits_per_sample,
                                    m.bit_rate_bps.unwrap_or(0),
                                    m.channels,
                                    m.duration_secs.unwrap_or(0.0),
                                    m.peak_db.unwrap_or(0.0),
                                    m.lufs_i.unwrap_or(0.0),
                                    m.bpm.unwrap_or(0.0)
                                )
                            })
                            .unwrap_or_default();
                        let external_hit = item
                            .external
                            .values()
                            .any(|v| v.to_lowercase().contains(&q));
                        name.contains(&q)
                            || parent.contains(&q)
                            || transcript.contains(&q)
                            || meta_text.to_lowercase().contains(&q)
                            || external_hit
                    })
                    .map(|item| item.id)
                    .collect();
            }
        } else {
            let q = query.to_lowercase();
            self.files = self
                .items
                .iter()
                .filter(|item| {
                    let name = item.display_name.to_lowercase();
                    let parent = item.display_folder.to_lowercase();
                    let transcript = item
                        .transcript
                        .as_ref()
                        .map(|t| t.full_text.to_lowercase())
                        .unwrap_or_default();
                    let meta_text = item
                        .meta
                        .as_ref()
                        .map(|m| {
                            format!(
                                "sr:{} bits:{} br:{} ch:{} len:{:.2} peak:{:.1} lufs:{:.1} bpm:{:.1}",
                                m.sample_rate,
                                m.bits_per_sample,
                                m.bit_rate_bps.unwrap_or(0),
                                m.channels,
                                m.duration_secs.unwrap_or(0.0),
                                m.peak_db.unwrap_or(0.0),
                                m.lufs_i.unwrap_or(0.0),
                                m.bpm.unwrap_or(0.0)
                            )
                        })
                        .unwrap_or_default();
                    let external_hit = item
                        .external
                        .values()
                        .any(|v| v.to_lowercase().contains(&q));
                    name.contains(&q)
                        || parent.contains(&q)
                        || transcript.contains(&q)
                        || meta_text.to_lowercase().contains(&q)
                        || external_hit
                })
                .map(|item| item.id)
                .collect();
        }
        self.original_files = self.files.clone();
        // restore selected index
        self.selected = selected_idx.and_then(|idx| self.files.iter().position(|&x| x == idx));
        self.search_dirty = false;
        self.search_deadline = None;
    }

    pub(super) fn apply_sort(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let sort_started = std::time::Instant::now();
        self.sort_loading_started_at = Some(sort_started);
        // Keep selection stable while reordering the visible file list.
        let selected_idx = self.selected.and_then(|i| self.files.get(i).copied());
        let key = self.sort_key;
        let dir = self.sort_dir;
        if dir == SortDir::None {
            self.files = self.original_files.clone();
        } else {
            // Capture shared references to keep sort_by borrow-friendly.
            let items = &self.items;
            let item_index = &self.item_index;
            let lufs_override = &self.lufs_override;
            let external_cols = &self.external_visible_columns;
            let sample_rate_override = &self.sample_rate_override;
            let bit_depth_override = &self.bit_depth_override;
            self.files.sort_by(|a, b| {
                use std::cmp::Ordering;
                use std::time::UNIX_EPOCH;
                let pa_idx = match item_index.get(a) {
                    Some(idx) => *idx,
                    None => return Ordering::Equal,
                };
                let pb_idx = match item_index.get(b) {
                    Some(idx) => *idx,
                    None => return Ordering::Equal,
                };
                let pa_item = &items[pa_idx];
                let pb_item = &items[pb_idx];
                let ma = pa_item.meta.as_ref();
                let mb = pb_item.meta.as_ref();
                let ord = match key {
                    SortKey::File => {
                        Self::string_order(&pa_item.display_name, &pb_item.display_name, dir)
                    }
                    SortKey::Folder => {
                        Self::string_order(&pa_item.display_folder, &pb_item.display_folder, dir)
                    }
                    SortKey::Transcript => {
                        let sa = pa_item
                            .transcript
                            .as_ref()
                            .map(|t| t.full_text.as_str())
                            .unwrap_or("");
                        let sb = pb_item
                            .transcript
                            .as_ref()
                            .map(|t| t.full_text.as_str())
                            .unwrap_or("");
                        Self::string_order(sa, sb, dir)
                    }
                    SortKey::Type => {
                        let sa = Self::list_type_sort_key(pa_item);
                        let sb = Self::list_type_sort_key(pb_item);
                        Self::string_order(sa.as_ref(), sb.as_ref(), dir)
                    }
                    SortKey::Length => Self::option_num_order(
                        ma.and_then(|m| m.duration_secs).filter(|v| v.is_finite()),
                        mb.and_then(|m| m.duration_secs).filter(|v| v.is_finite()),
                        dir,
                    ),
                    SortKey::Channels => Self::option_num_order(
                        ma.map(|m| m.channels as f32).filter(|v| *v > 0.0),
                        mb.map(|m| m.channels as f32).filter(|v| *v > 0.0),
                        dir,
                    ),
                    SortKey::SampleRate => Self::option_num_order(
                        sample_rate_override
                            .get(&pa_item.path)
                            .copied()
                            .or_else(|| ma.map(|m| m.sample_rate))
                            .filter(|v| *v > 0)
                            .map(|v| v as f32),
                        sample_rate_override
                            .get(&pb_item.path)
                            .copied()
                            .or_else(|| mb.map(|m| m.sample_rate))
                            .filter(|v| *v > 0)
                            .map(|v| v as f32),
                        dir,
                    ),
                    SortKey::Bits => Self::option_num_order(
                        bit_depth_override
                            .get(&pa_item.path)
                            .copied()
                            .map(|v| v.bits_per_sample())
                            .or_else(|| ma.map(|m| m.bits_per_sample))
                            .filter(|v| *v > 0)
                            .map(|v| v as f32),
                        bit_depth_override
                            .get(&pb_item.path)
                            .copied()
                            .map(|v| v.bits_per_sample())
                            .or_else(|| mb.map(|m| m.bits_per_sample))
                            .filter(|v| *v > 0)
                            .map(|v| v as f32),
                        dir,
                    ),
                    SortKey::BitRate => Self::option_num_order(
                        ma.and_then(|m| m.bit_rate_bps)
                            .map(|v| v as f32)
                            .filter(|v| *v > 0.0),
                        mb.and_then(|m| m.bit_rate_bps)
                            .map(|v| v as f32)
                            .filter(|v| *v > 0.0),
                        dir,
                    ),
                    SortKey::Level => Self::option_num_order(
                        ma.and_then(|m| m.peak_db).filter(|v| v.is_finite()),
                        mb.and_then(|m| m.peak_db).filter(|v| v.is_finite()),
                        dir,
                    ),
                    // LUFS sorting uses effective value: override if present, else base + gain.
                    SortKey::Lufs => {
                        let ga = pa_item.pending_gain_db;
                        let gb = pb_item.pending_gain_db;
                        let va = if let Some(v) = lufs_override.get(&pa_item.path).copied() {
                            v
                        } else {
                            ma.and_then(|m| m.lufs_i.map(|x| x + ga))
                                .unwrap_or(f32::NAN)
                        };
                        let vb = if let Some(v) = lufs_override.get(&pb_item.path).copied() {
                            v
                        } else {
                            mb.and_then(|m| m.lufs_i.map(|x| x + gb))
                                .unwrap_or(f32::NAN)
                        };
                        Self::option_num_order(
                            if va.is_finite() { Some(va) } else { None },
                            if vb.is_finite() { Some(vb) } else { None },
                            dir,
                        )
                    }
                    SortKey::Bpm => Self::option_num_order(
                        ma.and_then(|m| m.bpm).filter(|v| v.is_finite() && *v > 0.0),
                        mb.and_then(|m| m.bpm).filter(|v| v.is_finite() && *v > 0.0),
                        dir,
                    ),
                    SortKey::CreatedAt => Self::option_num_order_f64(
                        ma.and_then(|m| m.created_at)
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64()),
                        mb.and_then(|m| m.created_at)
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64()),
                        dir,
                    ),
                    SortKey::ModifiedAt => Self::option_num_order_f64(
                        ma.and_then(|m| m.modified_at)
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64()),
                        mb.and_then(|m| m.modified_at)
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64()),
                        dir,
                    ),
                    SortKey::External(idx) => {
                        let Some(col) = external_cols.get(idx) else {
                            return Ordering::Equal;
                        };
                        let sa = pa_item.external.get(col).map(|v| v.as_str()).unwrap_or("");
                        let sb = pb_item.external.get(col).map(|v| v.as_str()).unwrap_or("");
                        Self::string_order(sa, sb, dir)
                    }
                };
                if ord == Ordering::Equal {
                    pa_item
                        .display_name
                        .cmp(&pb_item.display_name)
                        .then(pa_item.path.cmp(&pb_item.path))
                } else {
                    ord
                }
            });
        }

        // restore selection to the same path if possible
        self.selected = selected_idx.and_then(|idx| self.files.iter().position(|&x| x == idx));
        let elapsed = sort_started.elapsed();
        self.sort_loading_last_ms = elapsed.as_secs_f32() * 1000.0;
        let hold_ms = if elapsed >= std::time::Duration::from_millis(120) {
            900
        } else {
            500
        };
        self.sort_loading_hold_until =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(hold_ms));
        self.sort_loading_started_at = None;
    }

    pub(super) fn current_path_for_rebuild(&self) -> Option<PathBuf> {
        if let Some(i) = self.active_tab {
            return self.tabs.get(i).map(|t| t.path.clone());
        }
        if let Some(i) = self.selected {
            return self.path_for_row(i).cloned();
        }
        None
    }

    pub(super) fn rebuild_current_buffer_with_mode(&mut self) {
        if let Some(tab_idx) = self.active_tab {
            if let Some(tab) = self.tabs.get(tab_idx) {
                self.invalidate_processing_for_target(
                    &ProcessingTarget::EditorTab(tab.path.clone()),
                    "editor rebuild",
                );
            }
            if let Some(tab) = self.tabs.get(tab_idx) {
                if tab.dirty {
                    let path = tab.path.clone();
                    if self.apply_dirty_tab_audio_with_mode(&path) {
                        return;
                    }
                }
            }
            if let Some((tab_loading, channels, tab_path, buffer_sr)) =
                self.tabs.get(tab_idx).map(|tab| {
                    (
                        tab.loading,
                        tab.ch_samples.clone(),
                        tab.path.clone(),
                        tab.buffer_sample_rate.max(1),
                    )
                })
            {
                let source_time_sec = self.playback_current_source_time_sec();
                if self.try_activate_editor_stream_transport_for_tab(tab_idx) {
                    if let Some(source_time_sec) = source_time_sec {
                        self.playback_seek_to_source_time(self.mode, source_time_sec);
                    }
                    return;
                }
                if tab_loading {
                    return;
                }
                if !channels.is_empty() {
                    if self.mode_requires_offline_processing() {
                        self.audio.stop();
                        self.audio.set_samples_mono(Vec::new());
                        self.spawn_heavy_processing_from_channels(
                            tab_path.clone(),
                            channels,
                            ProcessingTarget::EditorTab(tab_path),
                        );
                    } else {
                        let mut render_spec = self.offline_render_spec_for_path(&tab_path);
                        render_spec.master_gain_db = 0.0;
                        render_spec.file_gain_db = 0.0;
                        let rendered = Self::render_channels_offline_with_spec(
                            channels,
                            buffer_sr,
                            render_spec,
                            false,
                        );
                        self.audio.set_samples_channels(rendered);
                        self.playback_mark_buffer_source(
                            super::PlaybackSourceKind::EditorTab(tab_path),
                            self.audio.shared.out_sample_rate.max(1),
                        );
                        if let Some(source_time_sec) = source_time_sec {
                            self.playback_seek_to_source_time(self.mode, source_time_sec);
                        }
                    }
                    self.apply_effective_volume();
                    return;
                }
            }
        } else if let Some(sel) = self.selected {
            if let Some(path) = self.path_for_row(sel).cloned() {
                if self.apply_dirty_tab_preview_for_list(&path) {
                    return;
                }
            }
        }
        if let Some(p) = self.current_path_for_rebuild() {
            if self.active_tab.is_none() {
                self.invalidate_processing_for_target(
                    &ProcessingTarget::ListPreview(p.clone()),
                    "list rebuild",
                );
                let source_time_sec = match &self.playback_session.source {
                    super::PlaybackSourceKind::ListPreview(src) if src == &p => {
                        self.playback_current_source_time_sec()
                    }
                    _ => None,
                };
                if self.try_activate_list_stream_transport(&p) {
                    if let Some(source_time_sec) = source_time_sec {
                        self.playback_seek_to_source_time(self.mode, source_time_sec);
                    }
                    return;
                }
            }
            if self.is_virtual_path(&p) {
                let Some(audio) = self.edited_audio_for_path(&p) else {
                    return;
                };
                let channels = audio.channels.clone();
                let buffer_sr = self
                    .effective_sample_rate_for_path(&p)
                    .unwrap_or(self.audio.shared.out_sample_rate.max(1));
                let source_time_sec = self.playback_current_source_time_sec();
                if self.mode_requires_offline_processing() {
                    self.audio.stop();
                    self.audio.set_samples_mono(Vec::new());
                    self.spawn_heavy_processing_from_channels(
                        p.clone(),
                        channels,
                        ProcessingTarget::EditorTab(p),
                    );
                } else {
                    let mut render_spec = self.offline_render_spec_for_path(&p);
                    render_spec.master_gain_db = 0.0;
                    render_spec.file_gain_db = 0.0;
                    let rendered = Self::render_channels_offline_with_spec(
                        channels,
                        buffer_sr,
                        render_spec,
                        false,
                    );
                    self.audio.set_samples_channels(rendered);
                    self.playback_mark_buffer_source(
                        super::PlaybackSourceKind::EditorTab(p),
                        self.audio.shared.out_sample_rate.max(1),
                    );
                    if let Some(source_time_sec) = source_time_sec {
                        self.playback_seek_to_source_time(self.mode, source_time_sec);
                    }
                }
                self.apply_effective_volume();
                return;
            }
            if !self.is_decode_failed_path(&p) && self.mode_requires_offline_processing() {
                let target = if self.active_tab.is_some() {
                    ProcessingTarget::EditorTab(p.clone())
                } else {
                    ProcessingTarget::ListPreview(p.clone())
                };
                self.audio.stop();
                self.audio.set_samples_mono(Vec::new());
                self.spawn_heavy_processing(&p, target);
            } else if let Some(row_idx) = self.row_for_path(&p) {
                self.select_and_load(row_idx, false);
            } else if let Some(tab_idx) = self.active_tab {
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    if tab.path == p && !tab.loading {
                        tab.loading = true;
                        self.spawn_editor_decode(p.clone());
                    }
                }
            }
        }
    }

    pub(super) fn spawn_heavy_processing(&mut self, path: &Path, target: ProcessingTarget) {
        if !self.mode_requires_offline_processing() {
            self.debug_log(format!(
                "processing spawn skipped: mode={:?} target={}",
                self.mode,
                Self::format_processing_target(&target),
            ));
            return;
        }
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<ProcessingResult>();
        let path_buf = path.to_path_buf();
        let job_id = self.next_processing_job_id();
        let mode = self.mode;
        let mut render_spec = self.offline_render_spec_for_path(path);
        render_spec.master_gain_db = 0.0;
        render_spec.file_gain_db = 0.0;
        let source_time_sec = match &self.playback_session.source {
            super::PlaybackSourceKind::EditorTab(src)
            | super::PlaybackSourceKind::ListPreview(src)
                if src == path =>
            {
                self.playback_current_source_time_sec()
            }
            _ => None,
        };
        let path_for_thread = path_buf.clone();
        let target_for_thread = target.clone();
        std::thread::spawn(move || {
            if let Ok((channels, in_sr)) = crate::wave::decode_wav_multi(&path_for_thread) {
                let channels =
                    Self::render_channels_offline_with_spec(channels, in_sr, render_spec, false);
                let len = channels.get(0).map(|channel| channel.len()).unwrap_or(0);
                let samples = Self::mixdown_channels_mono(&channels, len);
                let mut waveform = Vec::new();
                crate::wave::build_minmax(&mut waveform, &samples, 2048);
                let _ = tx.send(ProcessingResult {
                    path: path_for_thread.clone(),
                    job_id,
                    mode,
                    target: target_for_thread,
                    samples,
                    waveform,
                    channels,
                });
            }
        });
        self.debug_log(format!(
            "processing spawn: job={} mode={:?} target={}",
            job_id,
            mode,
            Self::format_processing_target(&target),
        ));
        self.processing = Some(ProcessingState {
            msg: match mode {
                RateMode::PitchShift => "Pitch-shifting...".to_string(),
                RateMode::TimeStretch => "Time-stretching...".to_string(),
                RateMode::Speed => "Processing...".to_string(),
            },
            path: path_buf,
            job_id,
            mode,
            target,
            autoplay_when_ready: false,
            source_time_sec,
            started_at: std::time::Instant::now(),
            rx,
        });
    }

    pub(super) fn spawn_scan_worker(
        &self,
        root: PathBuf,
        skip_dotfiles: bool,
    ) -> std::sync::mpsc::Receiver<ScanMessage> {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut batch: Vec<PathBuf> = Vec::with_capacity(512);
            for entry in WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| !skip_dotfiles || !Self::is_dotfile_path(e.path()))
            {
                if let Ok(e) = entry {
                    if e.file_type().is_file() {
                        if let Some(ext) = e.path().extension().and_then(|s| s.to_str()) {
                            if audio_io::is_supported_extension(ext) {
                                if skip_dotfiles && Self::is_dotfile_path(e.path()) {
                                    continue;
                                }
                                batch.push(e.into_path());
                                if batch.len() >= 512 {
                                    if tx
                                        .send(ScanMessage::Batch(std::mem::take(&mut batch)))
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !batch.is_empty() {
                let _ = tx.send(ScanMessage::Batch(batch));
            }
            let _ = tx.send(ScanMessage::Done);
        });
        rx
    }
}
