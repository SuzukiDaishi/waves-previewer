//! Lightweight, opt-in UI-thread frame profiler used by the Debug window.
//!
//! The profiler records only while its panel is visible. Samples are kept in
//! memory and never become part of prefs or a session.

use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

pub const FRAME_PHASE_COUNT: usize = 6;
const FRAME_HISTORY_LIMIT: usize = 600;
const STAGE_HISTORY_LIMIT: usize = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePhase {
    PreUi,
    Workspace,
    Activation,
    Overlays,
    Windows,
    Finish,
}

impl FramePhase {
    pub const ALL: [Self; FRAME_PHASE_COUNT] = [
        Self::PreUi,
        Self::Workspace,
        Self::Activation,
        Self::Overlays,
        Self::Windows,
        Self::Finish,
    ];

    pub const fn index(self) -> usize {
        match self {
            Self::PreUi => 0,
            Self::Workspace => 1,
            Self::Activation => 2,
            Self::Overlays => 3,
            Self::Windows => 4,
            Self::Finish => 5,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::PreUi => "Pre-UI jobs",
            Self::Workspace => "Workspace UI",
            Self::Activation => "Activation / transport",
            Self::Overlays => "Overlays",
            Self::Windows => "Windows / dialogs",
            Self::Finish => "Frame finish",
        }
    }
}

#[derive(Clone, Debug)]
pub struct FramePerfSample {
    /// Time between the start of this app frame and the previous one. This is
    /// the value used for the real cadence/FPS graph.
    pub interval_ms: f32,
    /// Wall time spent between entering the app update and completing its UI.
    pub app_ms: f32,
    /// CPU-side app work split into stable, user-readable phases.
    pub phases_ms: [f32; FRAME_PHASE_COUNT],
    /// Work not covered by a named phase (normally framework hand-off between
    /// `eframe::App::update` and `eframe::App::ui`).
    pub other_ms: f32,
    pub deferred_count: u32,
}

impl FramePerfSample {
    pub fn fps(&self) -> f32 {
        if self.interval_ms > 0.0 {
            1_000.0 / self.interval_ms
        } else {
            0.0
        }
    }

    pub fn accounted_ms(&self) -> f32 {
        self.phases_ms.iter().sum::<f32>() + self.other_ms
    }
}

#[derive(Clone, Debug)]
pub struct FrameStageSummary {
    pub name: &'static str,
    pub samples: usize,
    pub last_ms: f32,
    pub average_ms: f32,
    pub p95_ms: f32,
    pub max_ms: f32,
}

#[derive(Debug)]
struct PendingFrame {
    started_at: Instant,
    interval_ms: f32,
    phases_ms: [f32; FRAME_PHASE_COUNT],
    stages: Vec<(&'static str, f32)>,
}

#[derive(Debug)]
pub struct FrameProfiler {
    /// User-facing capture switch. Capture additionally requires the Debug
    /// window to be visible.
    pub enabled: bool,
    pub paused: bool,
    samples: VecDeque<FramePerfSample>,
    stage_history: BTreeMap<&'static str, VecDeque<f32>>,
    pending: Option<PendingFrame>,
    last_frame_started_at: Option<Instant>,
}

impl Default for FrameProfiler {
    fn default() -> Self {
        Self {
            enabled: true,
            paused: false,
            samples: VecDeque::with_capacity(FRAME_HISTORY_LIMIT),
            stage_history: BTreeMap::new(),
            pending: None,
            last_frame_started_at: None,
        }
    }
}

impl FrameProfiler {
    pub fn is_recording(&self, debug_window_visible: bool) -> bool {
        self.enabled && !self.paused && debug_window_visible
    }

    pub fn begin_frame(&mut self, started_at: Instant, recording: bool) {
        if !recording {
            self.pending = None;
            self.last_frame_started_at = None;
            return;
        }
        let interval_ms = self
            .last_frame_started_at
            .map(|previous| started_at.duration_since(previous).as_secs_f32() * 1_000.0)
            .unwrap_or(0.0);
        self.last_frame_started_at = Some(started_at);
        self.pending = Some(PendingFrame {
            started_at,
            interval_ms,
            phases_ms: [0.0; FRAME_PHASE_COUNT],
            stages: Vec::with_capacity(32),
        });
    }

    /// Some headless/test paths call `App::ui` without a preceding `update`.
    /// Start a valid sample in that case without resetting a normal frame.
    pub fn ensure_frame(&mut self, started_at: Instant, recording: bool) {
        if self.pending.is_none() {
            self.begin_frame(started_at, recording);
        }
    }

    pub fn note_phase(&mut self, phase: FramePhase, elapsed_ms: f32) {
        if let Some(pending) = self.pending.as_mut() {
            let elapsed_ms = finite_non_negative(elapsed_ms);
            pending.phases_ms[phase.index()] += elapsed_ms;
            Self::add_pending_stage(&mut pending.stages, phase.label(), elapsed_ms);
        }
    }

    pub fn note_stage(&mut self, name: &'static str, elapsed_ms: f32) {
        if let Some(pending) = self.pending.as_mut() {
            Self::add_pending_stage(&mut pending.stages, name, finite_non_negative(elapsed_ms));
        }
    }

    fn add_pending_stage(
        stages: &mut Vec<(&'static str, f32)>,
        name: &'static str,
        elapsed_ms: f32,
    ) {
        if let Some((_, accumulated)) = stages.iter_mut().find(|(stage, _)| *stage == name) {
            *accumulated += elapsed_ms;
        } else {
            stages.push((name, elapsed_ms));
        }
    }

    pub fn finish_frame(&mut self, deferred_count: u32) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let app_ms = finite_non_negative(pending.started_at.elapsed().as_secs_f32() * 1_000.0);
        let named_ms = pending.phases_ms.iter().sum::<f32>();
        let sample = FramePerfSample {
            interval_ms: finite_non_negative(pending.interval_ms),
            app_ms,
            phases_ms: pending.phases_ms,
            other_ms: (app_ms - named_ms).max(0.0),
            deferred_count,
        };
        push_capped(&mut self.samples, sample, FRAME_HISTORY_LIMIT);
        for (name, elapsed_ms) in pending.stages {
            let history = self.stage_history.entry(name).or_default();
            push_capped(history, elapsed_ms, STAGE_HISTORY_LIMIT);
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.stage_history.clear();
        self.pending = None;
        self.last_frame_started_at = None;
    }

    pub fn samples(&self) -> &VecDeque<FramePerfSample> {
        &self.samples
    }

    pub fn stage_summaries(&self) -> Vec<FrameStageSummary> {
        let mut summaries: Vec<_> = self
            .stage_history
            .iter()
            .filter_map(|(&name, samples)| summarize_stage(name, samples))
            .collect();
        summaries.sort_by(|a, b| {
            b.p95_ms
                .total_cmp(&a.p95_ms)
                .then_with(|| b.max_ms.total_cmp(&a.max_ms))
                .then_with(|| a.name.cmp(b.name))
        });
        summaries
    }
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn push_capped<T>(queue: &mut VecDeque<T>, value: T, limit: usize) {
    if queue.len() >= limit {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn summarize_stage(name: &'static str, samples: &VecDeque<f32>) -> Option<FrameStageSummary> {
    let last_ms = samples.back().copied()?;
    let mut sorted: Vec<f32> = samples.iter().copied().collect();
    sorted.sort_by(f32::total_cmp);
    let p95_idx = ((sorted.len().saturating_sub(1)) as f32 * 0.95).round() as usize;
    Some(FrameStageSummary {
        name,
        samples: sorted.len(),
        last_ms,
        average_ms: sorted.iter().sum::<f32>() / sorted.len() as f32,
        p95_ms: sorted[p95_idx.min(sorted.len() - 1)],
        max_ms: sorted.last().copied().unwrap_or(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn records_frame_breakdown_and_ranks_stages_by_p95() {
        let mut profiler = FrameProfiler::default();
        for index in 0..20 {
            let started = Instant::now() - Duration::from_millis(5);
            profiler.begin_frame(started, true);
            profiler.note_phase(FramePhase::PreUi, 1.0);
            profiler.note_phase(FramePhase::Workspace, 2.0);
            profiler.note_stage("video frames", if index == 19 { 12.0 } else { 8.0 });
            profiler.note_stage("metadata", 0.5);
            profiler.finish_frame(2);
        }

        let sample = profiler.samples().back().expect("frame sample");
        assert_eq!(sample.deferred_count, 2);
        assert_eq!(sample.phases_ms[FramePhase::Workspace.index()], 2.0);
        assert!(sample.app_ms >= sample.accounted_ms() - 0.01);

        let stages = profiler.stage_summaries();
        let video = stages
            .iter()
            .find(|stage| stage.name == "video frames")
            .expect("video stage");
        let metadata = stages
            .iter()
            .find(|stage| stage.name == "metadata")
            .expect("metadata stage");
        assert!(video.p95_ms > metadata.p95_ms);
        assert_eq!(video.max_ms, 12.0);
    }

    #[test]
    fn inactive_capture_resets_the_cadence_baseline() {
        let mut profiler = FrameProfiler::default();
        let old = Instant::now() - Duration::from_secs(30);
        profiler.begin_frame(old, false);
        profiler.begin_frame(Instant::now(), true);
        profiler.finish_frame(0);
        assert_eq!(profiler.samples().back().unwrap().interval_ms, 0.0);
    }
}
