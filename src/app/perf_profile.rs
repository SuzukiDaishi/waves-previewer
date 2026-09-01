//! Machine performance tier and the UI-thread budgets derived from it.
//!
//! Thresholds used to be hard-coded constants tuned on a developer machine
//! (`LIST_JOB_SYNC_THRESHOLD = 50_000`, fixed drain budgets, `cores / 2`
//! worker pools). On a two-core laptop those same numbers put seconds of
//! work inside a single frame, which Windows reports as "not responding".
//!
//! Everything that decides "how much may this frame do" and "how many
//! workers may compete with the UI thread" reads its number from here
//! instead, so a low-spec machine gets smaller slices without a separate
//! code path.
//!
//! The tier starts from the core count and can be demoted at runtime when
//! measured frame times stay bad. Demotion is one-way: promoting back on a
//! few fast frames would oscillate between policies while the user is
//! working. A restart (or the manual override) is the way back up.

use std::time::Duration;

/// How much machine we are running on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PerfTier {
    Low,
    Normal,
    High,
}

impl PerfTier {
    pub fn as_str(self) -> &'static str {
        match self {
            PerfTier::Low => "low",
            PerfTier::Normal => "normal",
            PerfTier::High => "high",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "low" => Some(PerfTier::Low),
            "normal" => Some(PerfTier::Normal),
            "high" => Some(PerfTier::High),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PerfTier::Low => "Low (small slices)",
            PerfTier::Normal => "Normal",
            PerfTier::High => "High (large slices)",
        }
    }

    fn demoted(self) -> Self {
        match self {
            PerfTier::High => PerfTier::Normal,
            PerfTier::Normal | PerfTier::Low => PerfTier::Low,
        }
    }
}

/// User-facing setting: follow the measurement, or pin a tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PerfTierPreference {
    #[default]
    Auto,
    Pinned(PerfTier),
}

impl PerfTierPreference {
    pub fn as_str(self) -> &'static str {
        match self {
            PerfTierPreference::Auto => "auto",
            PerfTierPreference::Pinned(tier) => tier.as_str(),
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => PerfTierPreference::Auto,
            other => PerfTier::parse(other)
                .map(PerfTierPreference::Pinned)
                .unwrap_or(PerfTierPreference::Auto),
        }
    }
}

/// A frame slower than this is evidence the current tier is too ambitious.
const SLOW_FRAME_MS: f32 = 120.0;
/// Consecutive slow frames before demoting. High enough that one expensive
/// one-off (a modal opening, a texture upload) cannot demote the machine.
const SLOW_FRAMES_BEFORE_DEMOTE: u32 = 12;

/// Budgets and thresholds for the current machine.
#[derive(Clone, Copy, Debug)]
pub struct PerfProfile {
    pub cores: usize,
    pub tier: PerfTier,
    /// Tier implied by the hardware alone, before any demotion. Kept so the
    /// Debug window can show "High, demoted to Normal".
    pub base_tier: PerfTier,
    pub preference: PerfTierPreference,
    /// True when the list's root lives on a network share. Background
    /// sweeps then take a fraction of the concurrency they would locally:
    /// the bottleneck is the link, and saturating it starves whatever the
    /// user is actually waiting on.
    pub remote_root: bool,
    slow_frame_streak: u32,
}

impl Default for PerfProfile {
    fn default() -> Self {
        Self::detect(PerfTierPreference::Auto)
    }
}

impl PerfProfile {
    pub fn detect(preference: PerfTierPreference) -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        Self::from_cores(cores, preference)
    }

    pub fn from_cores(cores: usize, preference: PerfTierPreference) -> Self {
        let cores = cores.max(1);
        let base_tier = if cores <= 2 {
            PerfTier::Low
        } else if cores >= 8 {
            PerfTier::High
        } else {
            PerfTier::Normal
        };
        let tier = match preference {
            PerfTierPreference::Auto => base_tier,
            PerfTierPreference::Pinned(pinned) => pinned,
        };
        Self {
            cores,
            tier,
            base_tier,
            preference,
            remote_root: false,
            slow_frame_streak: 0,
        }
    }

    /// Record whether the current list root is on a network share. Returns
    /// true when this changed, so the caller can rebuild pools sized from it.
    pub fn set_remote_root(&mut self, remote: bool) -> bool {
        let changed = self.remote_root != remote;
        self.remote_root = remote;
        changed
    }

    /// Re-apply a changed Settings choice without losing the detected cores.
    pub fn set_preference(&mut self, preference: PerfTierPreference) {
        let remote_root = self.remote_root;
        *self = Self::from_cores(self.cores, preference);
        self.remote_root = remote_root;
    }

    /// Feed each finished frame's wall time in. Returns true when the tier
    /// changed, so the caller can log it.
    ///
    /// Only `Auto` demotes: a pinned tier is the user's explicit instruction
    /// and must not drift underneath them.
    pub fn note_frame_ms(&mut self, frame_ms: f32) -> bool {
        if self.preference != PerfTierPreference::Auto || self.tier == PerfTier::Low {
            return false;
        }
        if frame_ms < SLOW_FRAME_MS {
            self.slow_frame_streak = 0;
            return false;
        }
        self.slow_frame_streak = self.slow_frame_streak.saturating_add(1);
        if self.slow_frame_streak < SLOW_FRAMES_BEFORE_DEMOTE {
            return false;
        }
        self.slow_frame_streak = 0;
        self.tier = self.tier.demoted();
        true
    }

    pub fn demoted_from_hardware(&self) -> bool {
        self.preference == PerfTierPreference::Auto && self.tier < self.base_tier
    }

    // ---- Derived budgets ---------------------------------------------------

    /// How long the whole pre-UI drain phase may spend before deferring the
    /// rest to the next frame. Deliberately well under one 60fps frame: the
    /// UI still has to lay out and paint after this.
    pub fn frame_budget(&self) -> Duration {
        Duration::from_micros(match self.tier {
            PerfTier::Low => 4_000,
            PerfTier::Normal => 8_000,
            PerfTier::High => 12_000,
        })
    }

    /// Lists at or below this size sort/filter synchronously in one frame.
    /// Above it the sliced + worker path runs instead.
    pub fn list_sync_threshold(&self) -> usize {
        match self.tier {
            PerfTier::Low => 2_000,
            PerfTier::Normal => 20_000,
            PerfTier::High => 50_000,
        }
    }

    /// Per-frame time budget for the sliced decorate / filter passes.
    pub fn list_job_frame_budget_ms(&self) -> f64 {
        match self.tier {
            PerfTier::Low => 1.0,
            PerfTier::Normal => 2.0,
            PerfTier::High => 3.0,
        }
    }

    /// Per-frame budget for turning scanned paths into list rows.
    ///
    /// This is the "how many files are loaded yet" path — the number the user
    /// watches during a folder load — so an idle frame gets the tier's full
    /// slice. While audio is running it is capped hard rather than scaled:
    /// playback is a latency constraint, not a throughput one, and a fast
    /// machine spending 4ms here still drops the frame the audio callback
    /// needs.
    pub fn list_append_budget(&self, playing: bool) -> Duration {
        if playing {
            return Duration::from_micros(match self.tier {
                PerfTier::Low => 200,
                PerfTier::Normal | PerfTier::High => 350,
            });
        }
        Duration::from_micros(match self.tier {
            PerfTier::Low => 1_500,
            PerfTier::Normal => 3_000,
            PerfTier::High => 5_000,
        })
    }

    /// Concurrent heavy restore/decode jobs during a session open.
    pub fn restore_concurrency(&self) -> usize {
        if self.remote_root {
            // Two streams keep a share busy without thrashing its queue;
            // more just makes every one of them finish later, and the
            // status line stall longer.
            return 2.min(self.cores.max(1));
        }
        match self.tier {
            PerfTier::Low => 1,
            PerfTier::Normal => self.cores.saturating_sub(1).clamp(1, 3),
            PerfTier::High => self.cores.saturating_sub(1).clamp(1, 4),
        }
    }

    /// How often to check whether somebody else saved the open session.
    ///
    /// This is a floor -- `watch::next_walk_delay` stretches it by whatever
    /// a pass actually costs. A share gets a long one because the answer
    /// changes on human timescales (a colleague pressing Ctrl+S), not
    /// machine ones, and because every probe competes with the audio the
    /// user is waiting on over the same link.
    pub fn session_watch_interval_ms(&self) -> u64 {
        if self.remote_root {
            20_000
        } else {
            5_000
        }
    }

    /// Worker count for background scan-style pools (inspection, duplicate
    /// fingerprinting). Always leaves the UI thread a core to itself.
    pub fn scan_pool_workers(&self, cap: usize) -> usize {
        let by_tier = match self.tier {
            PerfTier::Low => 1,
            PerfTier::Normal => self.cores.saturating_sub(1).clamp(1, 3),
            PerfTier::High => (self.cores / 2).max(1),
        };
        by_tier.min(cap.max(1))
    }

    /// Worker count for the list metadata pool.
    pub fn meta_pool_workers(&self) -> usize {
        if self.remote_root {
            // More readers do not make a share faster; they make every
            // read slower and crowd out the file the user opened.
            return 2.min(self.cores).max(1);
        }
        match self.tier {
            PerfTier::Low => 1,
            PerfTier::Normal => self.cores.saturating_sub(1).clamp(1, 4),
            PerfTier::High => self.cores.saturating_sub(1).clamp(1, 6),
        }
    }

    /// Scale a locally-tuned background sweep budget for the current root.
    /// Used for the list metadata prefetch queue and its in-flight cap.
    pub fn background_io_budget(&self, local_budget: usize) -> usize {
        if self.remote_root {
            (local_budget / 4).max(1)
        } else {
            local_budget.max(1)
        }
    }

    /// Cadence for the editor mini-meter FFT. Scope/levels remain visually
    /// responsive, but two FFTs per 60 Hz UI frame are wasted work—especially
    /// while decode and analysis workers are competing for the same cores.
    pub fn mini_meter_spectrum_interval(&self) -> Duration {
        Duration::from_millis(match self.tier {
            PerfTier::Low => 67,
            PerfTier::Normal => 42,
            PerfTier::High => 33,
        })
    }

    /// How many decoded video frames the editor's preview keeps ahead of the
    /// playhead.
    ///
    /// This is what makes the picture land on the sound rather than a frame or
    /// two behind it: the panel picks from frames already decoded instead of
    /// asking for one and waiting. Costs a few hundred KB per frame at panel
    /// resolution, so a slow machine keeps a shorter run.
    pub fn video_decode_ahead_frames(&self) -> usize {
        if self.remote_root {
            return 6;
        }
        match self.tier {
            PerfTier::Low => 8,
            PerfTier::Normal => 12,
            PerfTier::High => 18,
        }
    }

    /// Read-ahead measured in time, then converted through the source FPS.
    /// A fixed frame count gives a 60 fps movie half the safety margin of a
    /// 30 fps movie, which is exactly where a native decode hiccup becomes a
    /// visible underrun.
    pub fn video_decode_ahead_frames_for_fps(&self, fps: f32) -> usize {
        let fps = if fps.is_finite() && fps > 1.0 {
            fps
        } else {
            30.0
        };
        let seconds: f32 = if self.remote_root {
            0.25
        } else {
            match self.tier {
                PerfTier::Low => 0.25,
                PerfTier::Normal => 0.40,
                PerfTier::High => 0.60,
            }
        };
        let max_frames = match self.tier {
            PerfTier::Low => 15,
            PerfTier::Normal => 24,
            PerfTier::High => 36,
        };
        ((fps * seconds).ceil() as usize)
            .max(self.video_decode_ahead_frames().min(max_frames))
            .min(max_frames)
    }

    /// Hard memory ceiling for decoded RGBA video frames in one open tab.
    pub fn video_ring_memory_bytes(&self) -> usize {
        if self.remote_root {
            return 24 * 1024 * 1024;
        }
        match self.tier {
            PerfTier::Low => 24 * 1024 * 1024,
            PerfTier::Normal => 48 * 1024 * 1024,
            PerfTier::High => 96 * 1024 * 1024,
        }
    }

    /// How far ahead a seek may reach by decoding forward before it gives up
    /// and restarts from the previous keyframe.
    ///
    /// Walking forward is what keeps ordinary playback smooth; restarting is
    /// what makes a long scrub land quickly. A slow machine takes the restart
    /// sooner rather than spending a frame budget walking.
    pub fn video_forward_walk_frames(&self) -> usize {
        match self.tier {
            PerfTier::Low => 8,
            PerfTier::Normal => 24,
            PerfTier::High => 48,
        }
    }

    /// How many list thumbnails may be extracted from video files at once.
    ///
    /// Decoding a keyframe is far more expensive than reading an embedded
    /// cover image, so this stays deliberately small — a folder of video files
    /// must not turn the thumbnail pass into a decode farm.
    pub fn video_poster_concurrency(&self) -> usize {
        if self.remote_root {
            return 1;
        }
        match self.tier {
            PerfTier::Low => 0,
            PerfTier::Normal => 1,
            PerfTier::High => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_count_picks_the_tier() {
        assert_eq!(
            PerfProfile::from_cores(1, PerfTierPreference::Auto).tier,
            PerfTier::Low
        );
        assert_eq!(
            PerfProfile::from_cores(2, PerfTierPreference::Auto).tier,
            PerfTier::Low
        );
        assert_eq!(
            PerfProfile::from_cores(4, PerfTierPreference::Auto).tier,
            PerfTier::Normal
        );
        assert_eq!(
            PerfProfile::from_cores(16, PerfTierPreference::Auto).tier,
            PerfTier::High
        );
    }

    #[test]
    fn low_tier_keeps_the_ui_thread_a_core() {
        let low = PerfProfile::from_cores(2, PerfTierPreference::Auto);
        assert_eq!(low.restore_concurrency(), 1);
        assert_eq!(low.meta_pool_workers(), 1);
        assert_eq!(low.scan_pool_workers(8), 1);
        assert!(low.list_sync_threshold() < 50_000);
        assert!(
            low.frame_budget()
                < PerfProfile::from_cores(16, PerfTierPreference::Auto).frame_budget()
        );
        assert!(
            low.mini_meter_spectrum_interval()
                > PerfProfile::from_cores(16, PerfTierPreference::Auto)
                    .mini_meter_spectrum_interval()
        );
    }

    #[test]
    fn worker_counts_never_reach_the_core_count() {
        for cores in 1..=32usize {
            let profile = PerfProfile::from_cores(cores, PerfTierPreference::Auto);
            assert!(profile.meta_pool_workers() >= 1);
            assert!(profile.restore_concurrency() >= 1);
            assert!(profile.scan_pool_workers(64) >= 1);
            if cores > 1 {
                assert!(
                    profile.meta_pool_workers() < cores,
                    "cores={cores} left no room for the UI thread"
                );
                assert!(profile.restore_concurrency() < cores);
                assert!(profile.scan_pool_workers(64) < cores);
            }
        }
    }

    #[test]
    fn sustained_slow_frames_demote_one_step_at_a_time() {
        let mut profile = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        assert_eq!(profile.tier, PerfTier::High);
        for _ in 0..SLOW_FRAMES_BEFORE_DEMOTE {
            profile.note_frame_ms(SLOW_FRAME_MS + 1.0);
        }
        assert_eq!(profile.tier, PerfTier::Normal);
        assert!(profile.demoted_from_hardware());
        for _ in 0..SLOW_FRAMES_BEFORE_DEMOTE {
            profile.note_frame_ms(SLOW_FRAME_MS + 1.0);
        }
        assert_eq!(profile.tier, PerfTier::Low);
    }

    #[test]
    fn one_slow_frame_among_fast_ones_does_not_demote() {
        let mut profile = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        for _ in 0..200 {
            profile.note_frame_ms(SLOW_FRAME_MS + 50.0);
            profile.note_frame_ms(1.0);
        }
        assert_eq!(profile.tier, PerfTier::High);
    }

    #[test]
    fn demotion_never_promotes_back() {
        let mut profile = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        for _ in 0..SLOW_FRAMES_BEFORE_DEMOTE {
            profile.note_frame_ms(SLOW_FRAME_MS + 1.0);
        }
        assert_eq!(profile.tier, PerfTier::Normal);
        for _ in 0..10_000 {
            profile.note_frame_ms(0.5);
        }
        assert_eq!(profile.tier, PerfTier::Normal);
    }

    #[test]
    fn a_pinned_tier_is_never_demoted() {
        let mut profile = PerfProfile::from_cores(16, PerfTierPreference::Pinned(PerfTier::High));
        for _ in 0..1_000 {
            profile.note_frame_ms(SLOW_FRAME_MS * 10.0);
        }
        assert_eq!(profile.tier, PerfTier::High);
        assert!(!profile.demoted_from_hardware());
    }

    #[test]
    fn video_budgets_grow_with_the_tier_and_shrink_on_a_share() {
        let low = PerfProfile::from_cores(2, PerfTierPreference::Auto);
        let normal = PerfProfile::from_cores(6, PerfTierPreference::Auto);
        let high = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        assert!(low.video_decode_ahead_frames() < normal.video_decode_ahead_frames());
        assert!(normal.video_decode_ahead_frames() < high.video_decode_ahead_frames());
        assert!(low.video_forward_walk_frames() < normal.video_forward_walk_frames());
        assert!(normal.video_forward_walk_frames() < high.video_forward_walk_frames());
        // A two-core machine does not spend its cores extracting thumbnails.
        assert_eq!(low.video_poster_concurrency(), 0);
        assert!(high.video_poster_concurrency() >= normal.video_poster_concurrency());

        let mut remote = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        remote.set_remote_root(true);
        assert_eq!(remote.video_decode_ahead_frames(), 6);
        assert_eq!(remote.video_ring_memory_bytes(), 24 * 1024 * 1024);
        assert_eq!(remote.video_poster_concurrency(), 1);
    }

    #[test]
    fn video_read_ahead_is_time_based_and_memory_bounded() {
        let low = PerfProfile::from_cores(2, PerfTierPreference::Auto);
        let normal = PerfProfile::from_cores(6, PerfTierPreference::Auto);
        let high = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        assert!(
            normal.video_decode_ahead_frames_for_fps(60.0)
                > normal.video_decode_ahead_frames_for_fps(24.0)
        );
        assert_eq!(low.video_decode_ahead_frames_for_fps(60.0), 15);
        assert_eq!(normal.video_decode_ahead_frames_for_fps(60.0), 24);
        assert_eq!(high.video_decode_ahead_frames_for_fps(60.0), 36);
        assert_eq!(normal.video_ring_memory_bytes(), 48 * 1024 * 1024);
    }

    #[test]
    fn a_remote_root_polls_the_session_less_often() {
        let mut profile = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        let local = profile.session_watch_interval_ms();
        profile.set_remote_root(true);
        assert!(
            profile.session_watch_interval_ms() > local,
            "a share must not be probed as often as a local disk"
        );
    }

    #[test]
    fn a_remote_root_throttles_every_background_reader() {
        let mut local = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        let local_meta = local.meta_pool_workers();
        let local_restore = local.restore_concurrency();
        let local_budget = local.background_io_budget(64);

        assert!(
            local.set_remote_root(true),
            "first change should report true"
        );
        assert!(
            !local.set_remote_root(true),
            "setting the same value again is not a change"
        );

        // A share is not made faster by more readers; it is made slower,
        // and the file the user opened waits behind them.
        assert!(local.meta_pool_workers() < local_meta);
        assert!(local.restore_concurrency() < local_restore);
        assert!(local.background_io_budget(64) < local_budget);
        // ...but never to zero, or the list would never resolve at all.
        assert!(local.meta_pool_workers() >= 1);
        assert!(local.restore_concurrency() >= 1);
        assert!(local.background_io_budget(1) >= 1);
    }

    /// The append budget is what decides how fast filenames appear during a
    /// folder load, so it must grow with the machine -- but never at the
    /// expense of the audio callback.
    #[test]
    fn the_append_budget_grows_with_the_tier_but_yields_to_playback() {
        let low = PerfProfile::from_cores(2, PerfTierPreference::Auto);
        let normal = PerfProfile::from_cores(4, PerfTierPreference::Auto);
        let high = PerfProfile::from_cores(16, PerfTierPreference::Auto);

        assert!(low.list_append_budget(false) < normal.list_append_budget(false));
        assert!(normal.list_append_budget(false) < high.list_append_budget(false));

        for profile in [low, normal, high] {
            assert!(
                profile.list_append_budget(true) < profile.list_append_budget(false),
                "playback must shrink the slice, not keep it"
            );
            assert!(
                profile.list_append_budget(true) <= Duration::from_micros(350),
                "the playing slice must stay inside the old hard cap"
            );
            assert!(profile.list_append_budget(true) > Duration::ZERO);
        }
    }

    #[test]
    fn a_single_core_remote_machine_still_gets_one_reader() {
        let mut profile = PerfProfile::from_cores(1, PerfTierPreference::Auto);
        profile.set_remote_root(true);
        assert_eq!(profile.meta_pool_workers(), 1);
        assert_eq!(profile.restore_concurrency(), 1);
    }

    #[test]
    fn changing_the_tier_keeps_the_root_locality() {
        let mut profile = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        profile.set_remote_root(true);
        profile.set_preference(PerfTierPreference::Pinned(PerfTier::High));
        assert!(
            profile.remote_root,
            "a Settings change must not silently un-throttle a network share"
        );
    }

    #[test]
    fn preference_round_trips_through_prefs_text() {
        for pref in [
            PerfTierPreference::Auto,
            PerfTierPreference::Pinned(PerfTier::Low),
            PerfTierPreference::Pinned(PerfTier::Normal),
            PerfTierPreference::Pinned(PerfTier::High),
        ] {
            assert_eq!(PerfTierPreference::parse(pref.as_str()), pref);
        }
        // Unknown values fall back to Auto rather than refusing to load prefs.
        assert_eq!(
            PerfTierPreference::parse("banana"),
            PerfTierPreference::Auto
        );
    }

    #[test]
    fn pinning_keeps_the_detected_core_count() {
        let mut profile = PerfProfile::from_cores(12, PerfTierPreference::Auto);
        profile.set_preference(PerfTierPreference::Pinned(PerfTier::Low));
        assert_eq!(profile.cores, 12);
        assert_eq!(profile.tier, PerfTier::Low);
        assert_eq!(profile.base_tier, PerfTier::High);
    }
}
