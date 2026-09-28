//! Timing values the UI shares: how often to redraw while something moves,
//! and how long to wait before showing that something is busy.
//!
//! The frame loop sleeps when idle, so anything animating asks for its next
//! frame with `request_repaint_after(...)`. These are the cadences to ask
//! with -- pick by how the thing on screen moves, not by how busy the job is.

use std::time::Duration;

/// One 60 Hz frame: for things that move with playback (playheads, video).
pub const ANIMATION_FRAME: Duration = Duration::from_millis(16);

/// 30 Hz: continuous but not playback-locked -- meters, clocks, live
/// waveforms, running progress bars.
pub const SMOOTH_REFRESH: Duration = Duration::from_millis(33);

/// 10 Hz: a background job's progress text or percentage.
pub const PROGRESS_REFRESH: Duration = Duration::from_millis(100);

/// How long a job must run before its busy indicator appears. Anything
/// faster finishes before the eye registers it, and a spinner that flashes
/// for one frame reads as a glitch.
pub const BUSY_INDICATOR_DELAY: Duration = Duration::from_millis(120);

/// Worker-side counterpart: the least time between two progress messages,
/// so a fast loop does not flood the UI's channel.
pub const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(120);

/// Two clicks within this count as a double click.
pub const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

/// How long typing must pause before a text filter is re-applied.
pub const TYPING_DEBOUNCE: Duration = Duration::from_millis(300);
