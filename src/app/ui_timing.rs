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

/// How long Multi Edits waits after the last edit before mixing the timeline
/// again: long enough that a drag re-mixes once when it stops rather than on
/// every frame, short enough that the change is heard at once.
pub const MULTI_EDIT_RENDER_DEBOUNCE: Duration = Duration::from_millis(100);

/// Held arrow-key seeking, in the editor and on the Multi Edits timeline:
/// the first step is immediate, the next waits `SEEK_REPEAT_DELAY` (so a tap
/// moves one step), then steps come every `SEEK_REPEAT_SLOW`, and every
/// `SEEK_REPEAT_FAST` once the key has been held for
/// `SEEK_REPEAT_ACCELERATE_AFTER`.
pub const SEEK_REPEAT_DELAY: Duration = Duration::from_millis(220);
pub const SEEK_REPEAT_SLOW: Duration = Duration::from_millis(70);
pub const SEEK_REPEAT_FAST: Duration = Duration::from_millis(35);
pub const SEEK_REPEAT_ACCELERATE_AFTER: Duration = Duration::from_millis(650);
