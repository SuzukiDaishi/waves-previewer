//! What the tool being adjusted on a tab is doing: its draft settings, the
//! draft's own undo history, and the preview of those settings.
//!
//! Rules only -- no UI, no audio engine. `tool_preview_ops.rs` renders and
//! installs previews from this; the Inspector reads it to draw its footer;
//! `input_ops::editor_undo_redo` steps the draft history before the tab's.
//!
//! The draft is not a copy of the settings: they stay where the canvas and
//! the panels already read and write them (`EditorTab::tool_state`, the gain
//! and pitch curves, the fade shapes, the warp points). A [`DraftSnapshot`]
//! captures and restores those fields, which is all the history needs.

use std::time::Instant;

use super::tool_render::{RenderTarget, ToolOp, ToolRenderOutput};
use super::types::{
    EditorTab, FadeShape, LoopXfadeShape, SpectralBrushStamp, SpectralWarpPoint, ToolKind,
    ToolState,
};

/// How many draft steps Ctrl+Z can walk back through.
const DRAFT_HISTORY_LIMIT: usize = 64;

/// Everything a tool's draft consists of, as the tab holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftSnapshot {
    pub tool_state: ToolState,
    pub gain_env_enabled: bool,
    pub gain_env_points: Vec<(usize, f32)>,
    pub pitch_env_enabled: bool,
    pub pitch_env_points: Vec<(usize, f32)>,
    pub fade_in_shape: FadeShape,
    pub fade_out_shape: FadeShape,
    pub loop_xfade_samples: usize,
    pub loop_xfade_shape: LoopXfadeShape,
    pub spectral_warp_points: Vec<SpectralWarpPoint>,
    pub spectral_brush_stamps: Vec<SpectralBrushStamp>,
}

impl DraftSnapshot {
    pub fn capture(tab: &EditorTab) -> Self {
        Self {
            tool_state: tab.tool_state,
            gain_env_enabled: tab.gain_env_enabled,
            gain_env_points: tab.gain_env_points.clone(),
            pitch_env_enabled: tab.pitch_env_enabled,
            pitch_env_points: tab.pitch_env_points.clone(),
            fade_in_shape: tab.fade_in_shape,
            fade_out_shape: tab.fade_out_shape,
            loop_xfade_samples: tab.loop_xfade_samples,
            loop_xfade_shape: tab.loop_xfade_shape,
            spectral_warp_points: tab.spectral_warp_points.clone(),
            spectral_brush_stamps: tab.spectral_brush_stamps.clone(),
        }
    }

    pub fn restore(&self, tab: &mut EditorTab) {
        tab.tool_state = self.tool_state;
        tab.gain_env_enabled = self.gain_env_enabled;
        tab.gain_env_points = self.gain_env_points.clone();
        tab.pitch_env_enabled = self.pitch_env_enabled;
        tab.pitch_env_points = self.pitch_env_points.clone();
        tab.fade_in_shape = self.fade_in_shape;
        tab.fade_out_shape = self.fade_out_shape;
        tab.loop_xfade_samples = self.loop_xfade_samples;
        tab.loop_xfade_shape = self.loop_xfade_shape;
        tab.spectral_warp_points = self.spectral_warp_points.clone();
        tab.spectral_brush_stamps = self.spectral_brush_stamps.clone();
        // Half-finished canvas gestures belong to the settings just replaced.
        tab.gain_env_drag = None;
        tab.pitch_env_drag = None;
        tab.pitch_drag_active = false;
        tab.spectral_warp_drag = None;
        tab.fade_drag_edge = None;
    }
}

/// Where the session is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SessionPhase {
    /// The tab plays its own audio. A tool that was just opened, applied or
    /// discarded is here: opening a tool never changes what is heard.
    #[default]
    Idle,
    /// The settings are heard: the preview follows every change.
    Previewing,
}

/// What uniquely identifies a rendered preview: the tab, the audio it was
/// rendered from, and everything about the op. A preview whose key differs
/// from the one the settings describe now is out of date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PreviewKey {
    pub tab_id: u64,
    pub audio_rev: u64,
    pub buffer_sr: u32,
    pub tool: ToolKind,
    pub fingerprint: u64,
    pub target: RenderTarget,
    pub mask_bits: u64,
}

/// A channel mask as bits (`u64::MAX`: every channel), for keys.
pub fn mask_bits(mask: Option<&[bool]>) -> u64 {
    mask.map_or(u64::MAX, |m| {
        m.iter().enumerate().fold(
            0u64,
            |bits, (i, on)| if *on { bits | (1 << (i % 64)) } else { bits },
        )
    })
}

impl PreviewKey {
    pub fn new(tab: &EditorTab, op: &ToolOp, target: RenderTarget, mask: Option<&[bool]>) -> Self {
        let mask_bits = mask_bits(mask);
        Self {
            tab_id: tab.tab_id,
            audio_rev: tab.audio_rev,
            buffer_sr: tab.buffer_sample_rate.max(1),
            tool: op.tool(),
            fingerprint: op.fingerprint(),
            target,
            mask_bits,
        }
    }
}

/// A finished preview render. The samples are shared with the playback
/// engine and the waveform overlay; `output.channels` is left empty.
#[derive(Clone, Debug)]
pub struct ToolPreviewResult {
    pub key: PreviewKey,
    pub output: ToolRenderOutput,
    pub channels: std::sync::Arc<Vec<Vec<f32>>>,
}

impl ToolPreviewResult {
    /// The render as Apply commits it.
    pub fn to_output(&self) -> ToolRenderOutput {
        ToolRenderOutput {
            channels: (*self.channels).clone(),
            ..self.output.clone()
        }
    }
}

/// Which picture of the source a tool draws behind its controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnalysisKind {
    /// The average spectrum (the EQ).
    Spectrum,
    /// The level over time (the dynamics tools).
    Levels,
}

impl AnalysisKind {
    pub fn for_tool(tool: ToolKind) -> Option<Self> {
        match tool {
            ToolKind::Eq => Some(AnalysisKind::Spectrum),
            ToolKind::Compressor | ToolKind::Limiter | ToolKind::NoiseGate => {
                Some(AnalysisKind::Levels)
            }
            _ => None,
        }
    }
}

/// What a source analysis was measured from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisKey {
    pub tab_id: u64,
    pub audio_rev: u64,
    pub kind: AnalysisKind,
    pub target: RenderTarget,
    pub mask_bits: u64,
}

/// The source measured over the tool's target before any edit: what the
/// EQ draws as "before", what the dynamics tools draw before a render.
#[derive(Clone, Debug)]
pub struct SourceAnalysis {
    pub key: AnalysisKey,
    pub spectrum: Option<Vec<f32>>,
    pub levels: Option<crate::dynamics::DynamicsTrace>,
}

/// What a tool panel shows while its tab plays. Display state only.
#[derive(Clone, Debug, Default)]
pub struct LiveMeters {
    pub playing: bool,
    /// The heard audio's spectrum, smoothed (the EQ's live analyzer).
    pub spectrum_db: Vec<f32>,
    pub spectrum_at: f64,
    /// Peak in (the source), out (what is heard) and the gain change at the
    /// playhead, in dB.
    pub in_db: f32,
    pub out_db: f32,
    pub gain_db: f32,
    pub at: f64,
    /// The playhead, in source samples.
    pub source_pos: Option<usize>,
}

/// The tool session of one tab.
#[derive(Clone, Debug, Default)]
pub struct ToolSession {
    pub phase: SessionPhase,
    /// The draft when the session started (tool opened, applied, discarded):
    /// what Esc goes back to.
    pub base: Option<DraftSnapshot>,
    pub draft_undo: Vec<DraftSnapshot>,
    pub draft_redo: Vec<DraftSnapshot>,
    /// The draft before the gesture in progress, if one is.
    pub gesture_open: Option<DraftSnapshot>,
    /// When a setting last changed: the preview waits for it to settle.
    pub last_change_at: Option<Instant>,
    pub preview: Option<ToolPreviewResult>,
    /// Hear the original while a preview exists (A/B).
    pub ab_original: bool,
    /// When the last apply landed, for the footer's "Applied" note.
    pub applied_at: Option<Instant>,
    /// `false`: changes mark the preview out of date instead of re-rendering
    /// it (heavy tools on long clips start this way).
    pub manual: bool,
    /// The last render failed; why, for the footer.
    pub error: Option<String>,
    /// The source over the target, measured when the tool opened.
    pub source_analysis: Option<std::sync::Arc<SourceAnalysis>>,
    pub live: LiveMeters,
}

impl ToolSession {
    /// A fresh session for a tool just opened, keeping nothing of the last.
    pub fn started(tab: &EditorTab, manual: bool) -> Self {
        Self {
            base: Some(DraftSnapshot::capture(tab)),
            manual,
            ..Self::default()
        }
    }

    pub fn has_draft_history(&self, redo: bool) -> bool {
        if redo {
            !self.draft_redo.is_empty()
        } else {
            !self.draft_undo.is_empty()
        }
    }

    /// A gesture on a setting began (a drag, a text field taking focus, a
    /// curve point grabbed). Nested begins keep the first snapshot.
    pub fn begin_gesture(&mut self, before: DraftSnapshot) {
        if self.gesture_open.is_none() {
            self.gesture_open = Some(before);
        }
    }

    /// The gesture ended with the draft as `now`. One gesture is one draft
    /// step; a gesture that changed nothing leaves no step. Returns whether
    /// the draft changed.
    pub fn end_gesture(&mut self, now: &DraftSnapshot, at: Instant) -> bool {
        let Some(before) = self.gesture_open.take() else {
            return false;
        };
        if before == *now {
            return false;
        }
        self.push_step(before, at);
        true
    }

    /// A setting changed in one go (a checkbox, a button, a menu pick).
    pub fn note_change(&mut self, before: DraftSnapshot, now: &DraftSnapshot, at: Instant) -> bool {
        if self.gesture_open.is_some() {
            // Inside a gesture the gesture's own snapshot wins.
            self.last_change_at = Some(at);
            return before != *now;
        }
        if before == *now {
            return false;
        }
        self.push_step(before, at);
        true
    }

    /// The draft is changing mid-gesture: keep the preview following it.
    pub fn note_live_change(&mut self, at: Instant) {
        self.last_change_at = Some(at);
        self.applied_at = None;
    }

    fn push_step(&mut self, before: DraftSnapshot, at: Instant) {
        self.draft_undo.push(before);
        if self.draft_undo.len() > DRAFT_HISTORY_LIMIT {
            self.draft_undo.remove(0);
        }
        self.draft_redo.clear();
        self.last_change_at = Some(at);
        self.applied_at = None;
        self.error = None;
        // A change is a request to hear it.
        self.phase = SessionPhase::Previewing;
    }

    /// Step the draft back; returns the snapshot to restore.
    pub fn undo(&mut self, current: DraftSnapshot, at: Instant) -> Option<DraftSnapshot> {
        let target = self.draft_undo.pop()?;
        self.draft_redo.push(current);
        self.gesture_open = None;
        self.last_change_at = Some(at);
        Some(target)
    }

    /// Step the draft forward again; returns the snapshot to restore.
    pub fn redo(&mut self, current: DraftSnapshot, at: Instant) -> Option<DraftSnapshot> {
        let target = self.draft_redo.pop()?;
        self.draft_undo.push(current);
        self.gesture_open = None;
        self.last_change_at = Some(at);
        Some(target)
    }

    /// Esc: back to the draft the session started with, nothing heard.
    /// Returns the snapshot to restore, if the draft moved.
    pub fn discard(&mut self) -> Option<DraftSnapshot> {
        let base = self.base.clone();
        let moved = !self.draft_undo.is_empty();
        self.phase = SessionPhase::Idle;
        self.draft_undo.clear();
        self.draft_redo.clear();
        self.gesture_open = None;
        self.preview = None;
        self.ab_original = false;
        self.error = None;
        if moved {
            base
        } else {
            None
        }
    }

    /// An apply landed: the settings stay (to apply them again elsewhere),
    /// but nothing is previewed until they change.
    pub fn applied(&mut self, tab: &EditorTab, at: Instant) {
        let manual = self.manual;
        *self = Self::started(tab, manual);
        self.applied_at = Some(at);
    }

    /// The preview the settings describe now, if it has been rendered.
    pub fn fresh_preview(&self, key: &PreviewKey) -> Option<&ToolPreviewResult> {
        self.preview.as_ref().filter(|p| p.key == *key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> EditorTab {
        EditorTab::new_base(std::path::PathBuf::from("/s.wav"), "s.wav".into())
    }

    #[test]
    fn opening_a_tool_previews_nothing() {
        let t = tab();
        let session = ToolSession::started(&t, false);
        assert_eq!(session.phase, SessionPhase::Idle);
        assert!(!session.has_draft_history(false));
    }

    #[test]
    fn one_gesture_is_one_step_and_starts_the_preview() {
        let mut t = tab();
        let mut session = ToolSession::started(&t, false);
        let now = Instant::now();
        session.begin_gesture(DraftSnapshot::capture(&t));
        for db in [1.0, 2.0, 3.0] {
            t.tool_state.gain_db = db;
            session.note_live_change(now);
        }
        assert!(session.end_gesture(&DraftSnapshot::capture(&t), now));
        assert_eq!(session.draft_undo.len(), 1);
        assert_eq!(session.phase, SessionPhase::Previewing);
        assert_eq!(session.draft_undo[0].tool_state.gain_db, 0.0);
    }

    #[test]
    fn a_gesture_that_changes_nothing_leaves_no_step() {
        let t = tab();
        let mut session = ToolSession::started(&t, false);
        session.begin_gesture(DraftSnapshot::capture(&t));
        assert!(!session.end_gesture(&DraftSnapshot::capture(&t), Instant::now()));
        assert!(session.draft_undo.is_empty());
        assert_eq!(session.phase, SessionPhase::Idle);
    }

    #[test]
    fn undo_and_redo_walk_the_draft() {
        let mut t = tab();
        let mut session = ToolSession::started(&t, false);
        let now = Instant::now();
        for db in [2.0, 4.0] {
            let before = DraftSnapshot::capture(&t);
            t.tool_state.gain_db = db;
            session.note_change(before, &DraftSnapshot::capture(&t), now);
        }
        let back = session.undo(DraftSnapshot::capture(&t), now).expect("undo");
        back.restore(&mut t);
        assert_eq!(t.tool_state.gain_db, 2.0);
        let fwd = session.redo(DraftSnapshot::capture(&t), now).expect("redo");
        fwd.restore(&mut t);
        assert_eq!(t.tool_state.gain_db, 4.0);
        // A new change after an undo drops what could have been redone.
        session
            .undo(DraftSnapshot::capture(&t), now)
            .unwrap()
            .restore(&mut t);
        let before = DraftSnapshot::capture(&t);
        t.tool_state.gain_db = -3.0;
        session.note_change(before, &DraftSnapshot::capture(&t), now);
        assert!(!session.has_draft_history(true));
    }

    #[test]
    fn discard_returns_to_the_start() {
        let mut t = tab();
        t.tool_state.gain_db = 1.5;
        let mut session = ToolSession::started(&t, false);
        let before = DraftSnapshot::capture(&t);
        t.tool_state.gain_db = 9.0;
        session.note_change(before, &DraftSnapshot::capture(&t), Instant::now());
        let base = session.discard().expect("moved");
        base.restore(&mut t);
        assert_eq!(t.tool_state.gain_db, 1.5);
        assert_eq!(session.phase, SessionPhase::Idle);
        assert!(session.discard().is_none(), "nothing left to discard");
    }

    #[test]
    fn after_an_apply_the_settings_stay_but_nothing_is_heard() {
        let mut t = tab();
        let mut session = ToolSession::started(&t, false);
        let before = DraftSnapshot::capture(&t);
        t.tool_state.gain_db = 6.0;
        session.note_change(before, &DraftSnapshot::capture(&t), Instant::now());
        session.applied(&t, Instant::now());
        assert_eq!(session.phase, SessionPhase::Idle);
        assert!(!session.has_draft_history(false));
        assert_eq!(session.base.as_ref().unwrap().tool_state.gain_db, 6.0);
        assert!(session.applied_at.is_some());
    }
}
