//! The editor's Panner tool in the app: a tab's settings as a
//! `panning::PanMatrix`, the live preview -- that matrix handed to the
//! output callback while the tab plays, so a knob turned while playing is
//! heard at once -- the green waveform, and Apply, which writes the same
//! matrix into the samples. The rules are `crate::panning` (shared with
//! Multi Edits); the knobs are `ui/pan_controls.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use super::types::{EditorApplyResult, ToolKind};
use super::{WavesPreviewer, LIVE_PREVIEW_SAMPLE_LIMIT};
use crate::audio_channels::{Layout, SpeakerPos};
use crate::panning::{dir_to_vec, PanMatrix, PanMode, Speakers};

/// What the live panner in the engine was built from; it is rebuilt only
/// when one of these moves.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AppliedPan {
    engine: usize,
    path: PathBuf,
    layout: Layout,
    /// The mode and the four settings, as bits.
    params: [u32; 5],
    channel_mask: Option<Vec<bool>>,
}

/// What the Panner's panel draws, worked out once per frame before the tab
/// is borrowed.
#[derive(Clone, Debug, Default)]
pub(crate) struct PannerView {
    /// The file's speakers; empty until its channel count is known.
    pub layout: Layout,
    /// The layout has height speakers: pitch and roll mean something.
    pub three_d: bool,
    /// Where the speakers the pan lays sound onto stand (x right, y ahead,
    /// z up).
    pub speakers: Vec<[f32; 3]>,
    /// Each channel that moves, by name, and where the settings send it.
    pub sources: Vec<(String, [f32; 3])>,
}

impl PannerView {
    pub fn mono(&self) -> bool {
        self.layout.len() == 1
    }
}

impl WavesPreviewer {
    /// How many channels the Panner works on for a tab: its samples', else
    /// (a tab that holds only an overview) what the engine plays of it.
    fn panner_channels(&self, tab_idx: usize) -> usize {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return 0;
        };
        if !tab.ch_samples.is_empty() {
            return tab.ch_samples.len();
        }
        let playing_it = matches!(
            &self.playback_session.source,
            super::PlaybackSourceKind::EditorTab(path) if *path == tab.path
        );
        if playing_it {
            self.audio.source_channels().unwrap_or(0)
        } else {
            0
        }
    }

    /// The tab's speakers as the Panner sees them.
    pub(super) fn panner_layout(&self, tab_idx: usize) -> Option<Layout> {
        let channels = self.panner_channels(tab_idx);
        let tab = self.tabs.get(tab_idx)?;
        (channels > 0)
            .then(|| self.channel_layout_for(&tab.path, channels))
            .flatten()
            .or_else(|| (channels > 0).then(|| vec![None; channels]))
    }

    /// The Panner's matrix for a tab: what the live preview plays and what
    /// Apply writes. The editor's channel view decides which channels move.
    pub(super) fn panner_matrix(&self, tab_idx: usize) -> Option<PanMatrix> {
        let layout = self.panner_layout(tab_idx)?;
        let tab = self.tabs.get(tab_idx)?;
        let params = tab.tool_state.pan_params(&layout);
        let mask = Self::editor_channel_mask(tab);
        Some(PanMatrix::for_layout(&layout, &params, |channel| {
            mask.as_ref()
                .is_none_or(|mask| mask.get(channel).copied().unwrap_or(false))
        }))
    }

    /// What the Panner's panel shows for a tab: the speakers, and where the
    /// settings send each channel -- by the same rules the pan uses, so a
    /// channel turned below a floor with no speakers is drawn on the floor.
    pub(super) fn panner_view(&self, tab_idx: usize) -> PannerView {
        let Some(layout) = self.panner_layout(tab_idx) else {
            return PannerView::default();
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return PannerView::default();
        };
        let mono = layout.len() == 1;
        let stereo = [Some(SpeakerPos::Fl), Some(SpeakerPos::Fr)];
        let speaker_layout: &[Option<SpeakerPos>] = if mono { &stereo } else { &layout };
        let speakers = Speakers::of(speaker_layout);
        let params = tab.tool_state.pan_params(&layout);
        let rotation = speakers.effective_rotation(params.rotation);
        let mut view = PannerView {
            three_d: speakers.is_3d(),
            speakers: speaker_layout
                .iter()
                .flatten()
                .filter(|pos| !pos.is_lfe())
                .map(|pos| {
                    let (az, el) = pos.direction(speaker_layout);
                    dir_to_vec(az, el)
                })
                .collect(),
            ..PannerView::default()
        };
        let turn = params.mode == PanMode::Vbap;
        if mono {
            let front = [0.0, 1.0, 0.0];
            let placed = if turn {
                speakers.place(crate::panning::Rotation::yaw(params.rotation.yaw).apply(front))
            } else {
                front
            };
            view.sources.push(("M".to_string(), placed));
        } else {
            let mask = Self::editor_channel_mask(tab);
            for (channel, pos) in layout.iter().enumerate() {
                let Some(pos) = pos.filter(|pos| !pos.is_lfe()) else {
                    continue;
                };
                let (az, el) = pos.direction(&layout);
                let home = dir_to_vec(az, el);
                let moves = mask
                    .as_ref()
                    .is_none_or(|mask| mask.get(channel).copied().unwrap_or(false));
                let placed = if turn && moves {
                    speakers.place(rotation.apply(home))
                } else {
                    home
                };
                view.sources.push((pos.label(&layout).to_string(), placed));
            }
        }
        view.layout = layout;
        view
    }

    /// The live preview's target: the active editor tab, on the Panner with
    /// its preview on, while the engine plays that tab.
    fn live_pan_target(&self) -> Option<(usize, AppliedPan)> {
        if !self.is_editor_workspace_active() {
            return None;
        }
        let tab_idx = self.active_tab?;
        let tab = self.tabs.get(tab_idx)?;
        let previewing = tab.active_tool == ToolKind::Panner
            && tab.preview_audio_tool == Some(ToolKind::Panner)
            && !tab.read_only;
        let playing_it = matches!(
            &self.playback_session.source,
            super::PlaybackSourceKind::EditorTab(path) if *path == tab.path
        );
        if !previewing || !playing_it {
            return None;
        }
        let layout = self.panner_layout(tab_idx)?;
        let t = &tab.tool_state;
        let mode = match t.pan_mode {
            None => 0,
            Some(PanMode::Balance) => 1,
            Some(PanMode::Vbap) => 2,
        };
        let applied = AppliedPan {
            engine: Arc::as_ptr(&self.audio.shared) as usize,
            path: tab.path.clone(),
            layout,
            params: [
                mode,
                t.pan_balance.to_bits(),
                t.pan_yaw_deg.to_bits(),
                t.pan_pitch_deg.to_bits(),
                t.pan_roll_deg.to_bits(),
            ],
            channel_mask: Self::editor_channel_mask(tab),
        };
        Some((tab_idx, applied))
    }

    /// Keep the engine's live panner in step with the Panner tool: install
    /// its matrix while the tab plays with the preview on (even at the
    /// centre, so the first turn of a knob slides rather than jumps), take
    /// it away otherwise. Re-sent only when something it was built from
    /// moved. Every frame, before the playing layout is synced: a mono file
    /// panned plays as stereo, and the layout sync asks the engine what it
    /// renders.
    pub(super) fn sync_playback_pan(&mut self) {
        match self.live_pan_target() {
            Some((tab_idx, applied)) => {
                if self.panner_live.as_ref() == Some(&applied) && self.audio.pan_matrix().is_some()
                {
                    return;
                }
                let Some(matrix) = self.panner_matrix(tab_idx) else {
                    return;
                };
                self.audio.set_pan_matrix(Some(Arc::new(matrix)));
                self.panner_live = Some(applied);
            }
            None => {
                if self.panner_live.take().is_some() || self.audio.pan_matrix().is_some() {
                    self.audio.set_pan_matrix(None);
                }
            }
        }
    }

    /// The green waveform of the Panner's preview: the samples panned,
    /// drawn over the originals. Only for a clip short enough to pan on the
    /// spot and only while the channel count stays (a mono file made stereo
    /// has no lanes to draw it on); the sound needs none of this.
    pub(super) fn panner_refresh_overlay(&mut self, tab_idx: usize) {
        let matrix = self.panner_matrix(tab_idx);
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        if tab.preview_audio_tool != Some(ToolKind::Panner) {
            return;
        }
        let drawable = matrix.filter(|matrix| {
            !matrix.is_identity()
                && matrix.out_channels() == tab.ch_samples.len()
                && tab.samples_len > 0
                && tab.samples_len <= LIVE_PREVIEW_SAMPLE_LIMIT
        });
        tab.preview_overlay = drawable.map(|matrix| {
            let channels = matrix.apply_offline(&tab.ch_samples);
            Self::preview_overlay_from_channels(channels, ToolKind::Panner, tab.samples_len)
        });
    }

    /// Write the Panner into the tab's samples: the matrix the preview
    /// played, over the whole file. A mono file becomes stereo. A long clip
    /// is panned on a worker; either way the knobs go back to the centre.
    pub(super) fn editor_apply_panner(&mut self, tab_idx: usize) {
        let Some(matrix) = self.panner_matrix(tab_idx) else {
            return;
        };
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.ch_samples.is_empty() || tab.loading || tab.read_only || matrix.is_identity() {
            return;
        }
        // A matrix covers at most `MAX_SOURCE_CHANNELS` inputs; written into
        // a wider file it would drop every channel past them.
        if matrix.in_channels() != tab.ch_samples.len() {
            self.push_toast(
                super::types::ToastSeverity::Info,
                format!(
                    "The Panner handles up to {} channels",
                    crate::audio_channels::MAX_SOURCE_CHANNELS
                ),
            );
            return;
        }
        if tab.samples_len > LIVE_PREVIEW_SAMPLE_LIMIT {
            self.spawn_panner_apply(tab_idx, matrix);
        } else {
            let channels = matrix.apply_offline(&tab.ch_samples);
            let reshaped = channels.len() != tab.ch_samples.len();
            self.editor_replace_channels(tab_idx, ToolKind::Panner.label(), channels, reshaped);
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.tool_state = tab.tool_state.without_pan();
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
        }
    }

    fn spawn_panner_apply(&mut self, tab_idx: usize, matrix: PanMatrix) {
        if self.editor_apply_slot_busy_toast() {
            return;
        }
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        let undo = Some(Self::capture_undo_state_labeled(
            tab,
            ToolKind::Panner.label(),
        ));
        let tab_id = tab.tab_id;
        let channels = tab.ch_samples_arc.clone();
        if matches!(&self.playback_session.source,
            super::PlaybackSourceKind::EditorTab(path) if *path == tab.path)
        {
            self.audio.stop();
        }
        let (tx, rx) = std::sync::mpsc::channel::<EditorApplyResult>();
        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            let out = matrix.apply_offline(&channels);
            let len = out.first().map(Vec::len).unwrap_or(0);
            let (waveform_minmax, waveform_pyramid) =
                WavesPreviewer::build_editor_waveform_cache(&out, len);
            let channels_arc = Arc::new(out.clone());
            let _ = tx.send(EditorApplyResult {
                channels: out,
                channels_arc,
                waveform_minmax,
                waveform_pyramid,
                lufs_override: None,
                selection_after: None,
            });
            crate::ui_wake::wake_ui();
        });
        self.editor_apply_state = Some(crate::app::types::EditorApplyState {
            msg: "Panning...".to_string(),
            rx,
            tab_id,
            undo,
            tool: ToolKind::Panner,
            source_range: None,
            source_len: 0,
            source_sample_rate: 1,
            viewport_restore: None,
        });
    }
}
