//! One time budget shared by every deferrable per-frame drain.
//!
//! Several subsystems already cap their own per-frame work (metadata
//! updates, spectrogram tiles, list sort/filter slices, bulk resample).
//! Those caps are independent and therefore additive: when a session
//! restore, a metadata backlog, a spectrogram render and a folder-watch
//! batch all land on the same frame, each spends its own budget and the
//! frame runs long enough for Windows to paint the ghost window.
//!
//! `DrainBudget` gives them a shared deadline. A drain that is not
//! latency-critical asks `should_continue()` first and, when the budget is
//! spent, leaves its work queued for the next frame and requests a repaint.
//! Latency-critical work (playback sync, audio device recovery, input,
//! IPC, editor decode/apply completion) never consults the budget.

use std::time::{Duration, Instant};

pub struct DrainBudget {
    started_at: Instant,
    budget: Duration,
    /// Set once the deadline is first observed to have passed, so the
    /// answer stays stable for the rest of the frame even if a later call
    /// happens to land inside the same microsecond.
    exhausted: bool,
    /// Number of drains that were skipped this frame. The frame loop uses
    /// this to decide whether another repaint is owed.
    deferred: u32,
    /// Most recently skipped stage, retained across frames for Debug.
    last_deferred_stage: Option<&'static str>,
}

impl Default for DrainBudget {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            budget: Duration::from_millis(8),
            exhausted: false,
            deferred: 0,
            last_deferred_stage: None,
        }
    }
}

impl DrainBudget {
    /// Start a new frame. `started_at` is the frame's own start instant, so
    /// time already spent before the drain phase counts against the budget.
    pub fn begin(&mut self, started_at: Instant, budget: Duration) {
        self.started_at = started_at;
        self.budget = budget;
        self.exhausted = false;
        self.deferred = 0;
    }

    /// True while deferrable work may still run this frame.
    pub fn should_continue(&mut self) -> bool {
        if self.exhausted {
            return false;
        }
        if self.started_at.elapsed() >= self.budget {
            self.exhausted = true;
            return false;
        }
        true
    }

    /// Record that a drain was skipped because the budget was spent.
    #[cfg(test)]
    pub fn note_deferred(&mut self) {
        self.note_deferred_at("(unspecified)");
    }

    pub fn note_deferred_at(&mut self, stage: &'static str) {
        self.deferred = self.deferred.saturating_add(1);
        self.last_deferred_stage = Some(stage);
    }

    /// True when anything was skipped and the frame loop owes a repaint.
    pub fn has_deferred_work(&self) -> bool {
        self.deferred > 0
    }

    pub fn deferred_count(&self) -> u32 {
        self.deferred
    }

    pub fn last_deferred_stage(&self) -> Option<&'static str> {
        self.last_deferred_stage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_budget_allows_work() {
        let mut budget = DrainBudget::default();
        budget.begin(Instant::now(), Duration::from_millis(50));
        assert!(budget.should_continue());
        assert!(!budget.has_deferred_work());
    }

    #[test]
    fn an_elapsed_budget_stops_work_and_stays_stopped() {
        let mut budget = DrainBudget::default();
        // A deadline already in the past: begin() from an old instant.
        budget.begin(
            Instant::now() - Duration::from_millis(100),
            Duration::from_millis(1),
        );
        assert!(!budget.should_continue());
        // Latching matters: a caller must not see the budget flip back open
        // partway through a frame.
        assert!(!budget.should_continue());
    }

    #[test]
    fn begin_resets_a_spent_budget() {
        let mut budget = DrainBudget::default();
        budget.begin(
            Instant::now() - Duration::from_millis(100),
            Duration::from_millis(1),
        );
        assert!(!budget.should_continue());
        budget.note_deferred();
        assert!(budget.has_deferred_work());

        budget.begin(Instant::now(), Duration::from_millis(50));
        assert!(budget.should_continue());
        assert!(!budget.has_deferred_work());
        assert_eq!(budget.deferred_count(), 0);
    }

    #[test]
    fn deferrals_are_counted() {
        let mut budget = DrainBudget::default();
        budget.begin(Instant::now(), Duration::from_millis(50));
        budget.note_deferred();
        budget.note_deferred();
        assert_eq!(budget.deferred_count(), 2);
        assert!(budget.has_deferred_work());
    }
}
