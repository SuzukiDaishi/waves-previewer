//! Applying an editor tool: the op its settings describe, the span and
//! channels it acts on, the render (on the UI thread for light ops, on the
//! single apply slot otherwise), and the commit. One path for every tool, so
//! the undo entry, the annotation remap and the hand-over to the engine are
//! the same whichever tool made the edit. See `tool_render.rs` for the DSP.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::tool_render::{
    self, RenderError, RenderTarget, TimeMap, ToolOp, ToolRenderOutput, ToolRenderRequest,
};
use super::tool_automation;
use super::types::{
    EditorApplyResult, EditorApplyViewportRestore, EditorTab, EditorUndoState, ToolKind,
};
use super::WavesPreviewer;
use crate::markers::MarkerEntry;

/// The polarity flip's ramp at interior boundaries when "Smooth boundaries"
/// is on: long enough not to click, short enough not to be heard as a fade.
const INVERT_SMOOTH_SECS: f64 = 0.002;

impl WavesPreviewer {
    /// The op `tool`'s settings on `tab` describe, with times converted at
    /// the tab's own rate. `None` for a tool that is not rendered (Trim,
    /// Markers, Channel Routing, the Panner) or that lacks what it needs (a
    /// noise profile, warp points).
    pub(super) fn tool_op_for_tab(tab: &EditorTab, tool: ToolKind) -> Option<ToolOp> {
        let st = &tab.tool_state;
        let sr = tab.buffer_sample_rate.max(1);
        let ms = |v: f32| crate::sample_rate::frames_for_secs(f64::from(v.max(0.0)) / 1000.0, sr);
        Some(match tool {
            ToolKind::Gain => {
                if tab.gain_env_enabled && !tab.gain_env_points.is_empty() {
                    ToolOp::GainCurve {
                        points: tab.gain_env_points.clone(),
                    }
                } else {
                    ToolOp::Gain { db: st.gain_db }
                }
            }
            ToolKind::Normalize => ToolOp::Normalize {
                target_db: st.normalize_target_db,
            },
            ToolKind::Loudness => ToolOp::Loudness {
                target_lufs: st.loudness_target_lufs,
            },
            ToolKind::Reverse => ToolOp::Reverse,
            ToolKind::InvertPolarity => ToolOp::InvertPolarity {
                fade: if st.invert_smooth_boundaries {
                    crate::sample_rate::frames_for_secs(INVERT_SMOOTH_SECS, sr)
                } else {
                    0
                },
            },
            ToolKind::DcOffset => ToolOp::DcOffset,
            ToolKind::Fade => ToolOp::EdgeFade {
                in_len: ms(st.fade_in_ms),
                in_shape: tab.fade_in_shape,
                out_len: ms(st.fade_out_ms),
                out_shape: tab.fade_out_shape,
            },
            ToolKind::InsertSilence => ToolOp::InsertSilence {
                len: ms(st.insert_silence_ms),
            },
            ToolKind::PitchShift => ToolOp::PitchShift {
                semitones: if st.pitch_semitones.is_finite() {
                    st.pitch_semitones
                } else {
                    0.0
                },
                curve: if tab.pitch_env_enabled {
                    tab.pitch_env_points.clone()
                } else {
                    Vec::new()
                },
            },
            ToolKind::TimeStretch => ToolOp::TimeStretch {
                rate: st.stretch_rate,
            },
            ToolKind::Speed => ToolOp::Speed {
                rate: st.speed_rate,
            },
            ToolKind::NoiseGate => ToolOp::NoiseGate(st.gate),
            ToolKind::Eq => ToolOp::Eq(st.eq, tool_automation::eq_tracks(tab)),
            ToolKind::Compressor => {
                ToolOp::Compressor(st.compressor, tool_automation::comp_tracks(tab))
            }
            ToolKind::Limiter => ToolOp::Limiter(st.limiter, tool_automation::limiter_tracks(tab)),
            ToolKind::DeClick => ToolOp::DeClick {
                sensitivity: st.declick_sensitivity,
            },
            ToolKind::DeClip => ToolOp::DeClip {
                sensitivity: st.declip_sensitivity,
            },
            ToolKind::DeHum => ToolOp::DeHum(crate::app::dehum::DehumConfig {
                base_hz: st.dehum_hz.clamp(20.0, 400.0),
                harmonics: st.dehum_harmonics.clamp(1, 16),
                q: st.dehum_q.clamp(5.0, 100.0),
                depth_db: st.dehum_depth_db.clamp(3.0, 80.0),
            }),
            ToolKind::DeNoise => ToolOp::DeNoise {
                profile: Arc::new(tab.noise_profile.clone()?),
                reduction_db: st.denoise_reduction_db.max(0.0),
                strength: st.denoise_strength.clamp(1.0, 4.0),
            },
            ToolKind::LoopEdit => return Self::loop_apply_op(tab),
            ToolKind::SpectralWarp => {
                if tab.spectral_warp_points.is_empty() {
                    return None;
                }
                ToolOp::SpectralWarp {
                    points: tab.spectral_warp_points.clone(),
                    time_radius_ms: st.warp_time_radius_ms.max(1.0),
                    freq_radius_hz: st.warp_freq_radius_hz.max(1.0),
                }
            }
            ToolKind::SpectralBrush => {
                if tab.spectral_brush_stamps.is_empty() {
                    return None;
                }
                ToolOp::SpectralBrush {
                    stamps: tab.spectral_brush_stamps.clone(),
                }
            }
            ToolKind::SpectralMute => {
                Self::editor_selected_range(tab)?;
                ToolOp::SpectralMute {
                    band: tab.freq_selection,
                    time_fade: ms(st.spectral_time_fade_ms),
                    freq_fade_hz: st.spectral_freq_fade_hz.max(0.0),
                }
            }
            ToolKind::Heal => {
                Self::editor_selected_range(tab)?;
                ToolOp::Heal {
                    band: tab.freq_selection,
                    time_fade_ms: st.spectral_time_fade_ms.max(0.0),
                    freq_fade_hz: st.spectral_freq_fade_hz.max(0.0),
                }
            }
            _ => return None,
        })
    }

    /// The loop seam crossfade the Loop tool's settings describe.
    pub(super) fn loop_crossfade_op(tab: &EditorTab) -> Option<ToolOp> {
        let (start, end) = tab.loop_region.filter(|(a, b)| b > a)?;
        let half =
            Self::effective_loop_xfade_samples(start, end, tab.samples_len, tab.loop_xfade_samples);
        Some(ToolOp::LoopCrossfade {
            start,
            end,
            half,
            shape: tab.loop_xfade_shape,
        })
    }

    /// The loop written out `repeats` times.
    pub(super) fn loop_unroll_op(tab: &EditorTab, repeats: u32) -> Option<ToolOp> {
        let (start, end) = tab.loop_region.filter(|(a, b)| b > a)?;
        Some(ToolOp::LoopUnroll {
            start,
            end,
            repeats,
            markers: tab.markers.clone(),
        })
    }

    /// The span `tool` acts on in this tab, by its target rule.
    pub(super) fn tool_target_for_tab(&self, tab_idx: usize, tool: ToolKind) -> RenderTarget {
        use super::editor_tool::TargetRule;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return RenderTarget::Whole;
        };
        let selection =
            Self::editor_selected_range(tab).filter(|(s, e)| *e <= tab.samples_len && e > s);
        match tool.spec().target {
            TargetRule::SelectionOrWhole | TargetRule::SelectionOnly => selection
                .map(|(s, e)| RenderTarget::Range(s, e))
                .unwrap_or(RenderTarget::Whole),
            TargetRule::InsertPoint => RenderTarget::InsertAt(self.editor_insert_position(tab_idx)),
            TargetRule::WholeOnly | TargetRule::None => RenderTarget::Whole,
        }
    }

    /// The channels `tool` edits in this tab: the Custom channel view's when
    /// the tool honours it, every channel otherwise.
    pub(super) fn tool_channel_mask(tab: &EditorTab, tool: ToolKind) -> Option<Vec<bool>> {
        if tool.spec().honors_channel_view {
            Self::editor_channel_mask(tab)
        } else {
            None
        }
    }

    /// A render request for `op` on this tab over `target`, sharing the
    /// tab's buffer (no copy).
    pub(super) fn tool_request_for_tab(
        &self,
        tab_idx: usize,
        op: ToolOp,
        target: RenderTarget,
    ) -> Option<ToolRenderRequest> {
        let tab = self.tabs.get(tab_idx)?;
        let ch_mask = Self::tool_channel_mask(tab, op.tool());
        Some(ToolRenderRequest {
            op,
            channels: tab.ch_samples_arc.clone(),
            buffer_sr: tab.buffer_sample_rate.max(1),
            target,
            ch_mask,
        })
    }

    /// Render on the UI thread and commit. For ops that cost a pass over the
    /// samples; anything slower goes through [`Self::spawn_editor_apply_op`].
    /// Returns whether an edit was made.
    pub(super) fn editor_apply_op_now(&mut self, tab_idx: usize, req: ToolRenderRequest) -> bool {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        if tab.loading || tab.read_only {
            return false;
        }
        let label = req.op.label();
        let undo = Self::capture_undo_state_labeled(tab, label);
        // What was heard is what is written: a preview of exactly this op on
        // exactly this audio is committed as it is.
        if let Some(out) = self.tool_preview_for_request(tab_idx, &req) {
            self.editor_commit_render(tab_idx, out, undo);
            return true;
        }
        match tool_render::render(&req, &AtomicBool::new(false)) {
            Ok(out) => {
                self.editor_commit_render(tab_idx, out, undo);
                true
            }
            Err(RenderError::Invalid(msg)) => {
                self.push_toast(
                    crate::app::types::ToastSeverity::Warning,
                    format!("{label}: {msg}"),
                );
                false
            }
            Err(RenderError::NothingToDo | RenderError::Cancelled) => false,
        }
    }

    /// Commit a finished render to the tab as one undoable edit.
    pub(super) fn editor_commit_render(
        &mut self,
        tab_idx: usize,
        out: ToolRenderOutput,
        undo: EditorUndoState,
    ) {
        let ToolRenderOutput {
            channels,
            selection_after,
            time_map,
            markers_after,
            lufs_override,
            analysis: _,
        } = out;
        let Some((path, old_len, buffer_sr)) = self.tabs.get_mut(tab_idx).map(|tab| {
            let old_len = tab.samples_len;
            Self::editor_adopt_rendered_channels(
                tab,
                channels,
                &time_map,
                selection_after,
                markers_after,
            );
            (tab.path.clone(), old_len, tab.buffer_sample_rate.max(1))
        }) else {
            return;
        };
        self.cancel_tool_preview_for_tab(tab_idx);
        self.editor_finish_destructive_apply(tab_idx, undo, true);
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            let now = std::time::Instant::now();
            let mut session = std::mem::take(&mut tab.tool_session);
            session.applied(tab, now);
            tab.tool_session = session;
        }
        if let Some(v) = lufs_override {
            self.lufs_override.insert(path.clone(), v);
        }
        self.editor_remap_transcript(&path, &time_map, old_len, buffer_sr);
        self.sync_editor_notes_from_tab(tab_idx);
        self.notify_if_tab_over_fs(tab_idx);
    }

    /// Put rendered channels into the tab and move everything that points
    /// into the audio -- markers, regions, loop points, selections, fade
    /// ranges, notes -- through `time_map`. Shared by the synchronous commit
    /// and the apply-slot drain. Returns whether the channel count changed.
    pub(super) fn editor_adopt_rendered_channels(
        tab: &mut EditorTab,
        channels: Vec<Vec<f32>>,
        time_map: &TimeMap,
        selection_after: Option<(usize, usize)>,
        markers_after: Option<Vec<MarkerEntry>>,
    ) -> bool {
        let old_len = tab.samples_len;
        let rewired = channels.len() != tab.ch_samples.len();
        tab.ch_samples = channels;
        tab.samples_len = tab.ch_samples.first().map(Vec::len).unwrap_or(0);
        if rewired {
            // The Panner makes a mono file stereo.
            Self::editor_reset_per_channel_state(tab);
        }
        if *time_map != TimeMap::Identity {
            Self::editor_remap_annotations(tab, time_map, old_len);
            let new_len = tab.samples_len;
            tool_automation::remap(tab, time_map, new_len);
        }
        if let TimeMap::Unwrap { end, .. } = *time_map {
            // The loop has been written out: it no longer exists as a loop,
            // and ranges that pointed into it point at one pass of many.
            let shift = tab.samples_len.saturating_sub(old_len);
            if tab.view_offset >= end {
                tab.view_offset = tab.view_offset.saturating_add(shift);
            }
            Self::editor_sync_view_offset_exact(tab);
            tab.loop_region = None;
            tab.loop_region_committed = None;
            tab.loop_region_applied = None;
            tab.loop_mode = crate::app::types::LoopMode::Off;
            tab.loop_xfade_samples = 0;
            tab.selection = None;
            tab.ab_loop = None;
            tab.trim_range = None;
            tab.fade_in_range = None;
            tab.fade_out_range = None;
            tab.selection_anchor_sample = None;
            tab.right_drag_mode = None;
            Self::update_loop_markers_dirty(tab);
        }
        if let Some(markers) = markers_after {
            tab.markers_committed = markers.clone();
            tab.markers_applied = markers.clone();
            tab.markers = markers;
            Self::update_markers_dirty(tab);
        }
        if let Some(sel) = selection_after {
            tab.selection = Some(sel);
        }
        tab.mark_audio_changed();
        Self::editor_clamp_ranges(tab);
        rewired
    }

    /// Move every annotation of `tab` through `time_map`.
    fn editor_remap_annotations(tab: &mut EditorTab, time_map: &TimeMap, old_len: usize) {
        let map = |s: usize| time_map.map_sample(s);
        for layer in [
            &mut tab.markers,
            &mut tab.markers_committed,
            &mut tab.markers_applied,
        ] {
            for m in layer.iter_mut() {
                m.sample = map(m.sample);
            }
        }
        for r in tab.regions.iter_mut() {
            r.start = map(r.start);
            r.end = map(r.end);
        }
        let map_range = |range: &mut Option<(usize, usize)>| {
            if let Some((a, b)) = range.as_mut() {
                *a = map(*a);
                *b = map(*b);
            }
        };
        map_range(&mut tab.selection);
        map_range(&mut tab.ab_loop);
        map_range(&mut tab.loop_region);
        map_range(&mut tab.loop_region_applied);
        map_range(&mut tab.loop_region_committed);
        map_range(&mut tab.trim_range);
        map_range(&mut tab.fade_in_range);
        map_range(&mut tab.fade_out_range);
        for (a, b) in tab.extra_selections.iter_mut() {
            *a = map(*a);
            *b = map(*b);
        }
        match *time_map {
            TimeMap::Identity => {}
            TimeMap::Range {
                start,
                old_end,
                new_end,
            } => Self::remap_editor_notes_for_replacement(
                &mut tab.editor_notes,
                start,
                old_end,
                new_end.saturating_sub(start),
            ),
            TimeMap::Whole { new_len, .. } => {
                Self::remap_editor_notes_for_replacement(&mut tab.editor_notes, 0, old_len, new_len)
            }
            TimeMap::Unwrap {
                start,
                end,
                repeats,
            } => {
                let shift = end
                    .saturating_sub(start)
                    .saturating_mul(repeats.saturating_sub(1) as usize);
                Self::remap_editor_notes_for_replacement(&mut tab.editor_notes, end, end, shift)
            }
        }
    }

    /// Keep the transcript's timings on the same words after a length
    /// change.
    pub(super) fn editor_remap_transcript(
        &mut self,
        path: &std::path::Path,
        time_map: &TimeMap,
        old_len: usize,
        buffer_sr: u32,
    ) {
        let sr = u64::from(buffer_sr.max(1));
        let to_ms = |s: usize| (s as u64).saturating_mul(1000) / sr;
        match *time_map {
            TimeMap::Range {
                start,
                old_end,
                new_end,
            } if old_end > start => {
                let multiplier =
                    new_end.saturating_sub(start).max(1) as f64 / (old_end - start) as f64;
                let (start_ms, end_ms) = (to_ms(start), to_ms(old_end));
                self.mutate_transcript_document_for_path(path, |document| {
                    document.scale_range(start_ms, end_ms, multiplier)
                });
            }
            TimeMap::Whole { new_len, .. } if old_len > 0 => {
                let multiplier = new_len as f64 / old_len as f64;
                self.mutate_transcript_document_for_path(path, |document| {
                    document.scale_time(multiplier)
                });
            }
            _ => {}
        }
    }

    /// What the topbar says while `op` runs on the apply slot.
    fn apply_message(op: &ToolOp) -> String {
        match op {
            ToolOp::DeClick { .. } => "Removing clicks...".to_string(),
            ToolOp::DeClip { .. } => "Repairing clipping...".to_string(),
            ToolOp::DeHum(_) => "Removing hum...".to_string(),
            ToolOp::Heal { .. } => "Healing selection...".to_string(),
            ToolOp::Loudness { .. } => "Applying Loudness Normalize...".to_string(),
            ToolOp::PitchShift { .. } => "Applying PitchShift...".to_string(),
            ToolOp::TimeStretch { .. } => "Applying TimeStretch...".to_string(),
            ToolOp::Speed { .. } => "Applying Speed...".to_string(),
            other => format!("Applying {}...", other.label()),
        }
    }

    /// Render on the single apply slot and commit when it lands (see
    /// `drain_editor_apply_jobs`). Returns whether the job was started.
    pub(super) fn spawn_editor_apply_op(
        &mut self,
        tab_idx: usize,
        req: ToolRenderRequest,
        viewport_restore: Option<EditorApplyViewportRestore>,
    ) -> bool {
        use std::sync::mpsc;
        // Single apply slot: the UI disables further applies on the busy tab;
        // races (hotkeys, other tabs) refuse with a toast instead of
        // cancelling the running job.
        if self.editor_apply_slot_busy_toast() {
            return false;
        }
        // The preview of exactly this op is already rendered: commit it
        // rather than render it again.
        if self.tool_preview_for_request(tab_idx, &req).is_some() {
            return self.editor_apply_op_now(tab_idx, req);
        }
        let Some(tab) = self.tabs.get(tab_idx) else {
            return false;
        };
        let tool = req.op.tool();
        if tab.loading || tab.samples_len == 0 || tab.read_only {
            if viewport_restore.is_some() {
                self.push_toast(
                    crate::app::types::ToastSeverity::Info,
                    "Time Stretch is unavailable until the audio finishes loading",
                );
            }
            return false;
        }
        if matches!(
            tool,
            ToolKind::PitchShift | ToolKind::TimeStretch | ToolKind::Speed
        ) && self.is_decode_failed_path(&tab.path)
        {
            if viewport_restore.is_some() {
                self.push_toast(
                    crate::app::types::ToastSeverity::Error,
                    "Time Stretch is unavailable because audio decoding failed",
                );
            }
            return false;
        }
        let label = req.op.label();
        let msg = Self::apply_message(&req.op);
        let undo = Some(Self::capture_undo_state_labeled(tab, label));
        let tab_id = tab.tab_id;
        let source_len = tab.samples_len;
        let source_range = match req.target {
            RenderTarget::Range(s, e) => Some((s, e)),
            _ => None,
        };
        let buffer_sr = req.buffer_sr;
        // Stop playback only when this tab is the audible source; playback of
        // other tabs / list previews keeps running during the apply.
        if matches!(&self.playback_session.source,
            crate::app::PlaybackSourceKind::EditorTab(p) if *p == tab.path)
        {
            self.audio.stop();
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
        }
        let (tx, rx) = mpsc::channel::<Result<EditorApplyResult, Option<String>>>();
        std::thread::spawn(move || {
            let result = match tool_render::render(&req, &AtomicBool::new(false)) {
                Ok(out) => {
                    let len = out.channels.first().map(Vec::len).unwrap_or(0);
                    // Build the waveform cache + Arc mirror here so adopting
                    // the result on the UI thread is (nearly) copy-free.
                    let (waveform_minmax, waveform_pyramid) =
                        WavesPreviewer::build_editor_waveform_cache(&out.channels, len);
                    let channels_arc = Arc::new(out.channels.clone());
                    Ok(EditorApplyResult {
                        channels: out.channels,
                        channels_arc,
                        waveform_minmax,
                        waveform_pyramid,
                        lufs_override: out.lufs_override,
                        selection_after: out.selection_after,
                        time_map: out.time_map,
                        markers_after: out.markers_after,
                    })
                }
                Err(RenderError::Invalid(msg)) => Err(Some(msg)),
                Err(RenderError::NothingToDo | RenderError::Cancelled) => Err(None),
            };
            let _ = tx.send(result);
        });
        self.editor_apply_state = Some(crate::app::types::EditorApplyState {
            msg,
            rx,
            tab_id,
            undo,
            tool,
            source_range,
            source_len,
            source_sample_rate: buffer_sr,
            viewport_restore,
        });
        true
    }
}
