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
//! The tier starts from CPU, memory and renderer ceilings. Runtime stalls
//! immediately shrink the adaptive share; recovery requires a long run of
//! stable frames and happens one step at a time.

use std::time::Duration;

const GIB: u64 = 1024 * 1024 * 1024;

/// Renderer characteristics matter independently from CPU throughput. A
/// machine with many cores can still be presenting through llvmpipe, WARP or
/// Microsoft's generic OpenGL driver, where progress animation and texture
/// churn are much more expensive than the background work itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RendererKind {
    #[default]
    Unknown,
    Hardware,
    Software,
}

impl RendererKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Hardware => "hardware",
            Self::Software => "software",
        }
    }

    pub fn from_description(raw: &str) -> Self {
        let lower = raw.to_ascii_lowercase();
        if [
            "llvmpipe",
            "softpipe",
            "swiftshader",
            "software rasterizer",
            "gdi generic",
            "microsoft basic render",
            "cpu",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            Self::Software
        } else if lower.trim().is_empty() {
            Self::Unknown
        } else {
            Self::Hardware
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MemoryPressure {
    #[default]
    Normal,
    High,
    Critical,
}

/// Injectable hardware facts. Tests use this instead of depending on the CI
/// host and the GUI fills in the renderer after eframe creates it.
#[derive(Clone, Copy, Debug, Default)]
pub struct HardwareSnapshot {
    pub cores: usize,
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    pub renderer: RendererKind,
}

impl HardwareSnapshot {
    pub fn detect() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        let (total_memory_bytes, available_memory_bytes) = detect_physical_memory();
        Self {
            cores,
            total_memory_bytes,
            available_memory_bytes,
            renderer: RendererKind::Unknown,
        }
    }
}

#[cfg(windows)]
fn detect_physical_memory() -> (Option<u64>, Option<u64>) {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        (None, None)
    } else {
        (Some(status.ullTotalPhys), Some(status.ullAvailPhys))
    }
}

#[cfg(target_os = "linux")]
fn detect_physical_memory() -> (Option<u64>, Option<u64>) {
    fn bytes(pages_name: libc::c_int) -> Option<u64> {
        let pages = unsafe { libc::sysconf(pages_name) };
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        (pages > 0 && page_size > 0).then(|| (pages as u64).saturating_mul(page_size as u64))
    }
    (bytes(libc::_SC_PHYS_PAGES), bytes(libc::_SC_AVPHYS_PAGES))
}

#[cfg(target_os = "macos")]
fn detect_physical_memory() -> (Option<u64>, Option<u64>) {
    let mut total = 0u64;
    let mut size = std::mem::size_of::<u64>();
    let name = b"hw.memsize\0";
    let ok = unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            (&mut total as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    (ok.then_some(total), None)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn detect_physical_memory() -> (Option<u64>, Option<u64>) {
    (None, None)
}

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

    fn promoted(self) -> Self {
        match self {
            PerfTier::Low => PerfTier::Normal,
            PerfTier::Normal | PerfTier::High => PerfTier::High,
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

/// Runtime feedback controller layered on top of the hardware ceilings.
/// It reacts immediately to input and long frames, but restores throughput
/// only after sustained stable frames so background work cannot oscillate.
#[derive(Clone, Copy, Debug)]
pub struct LoadGovernor {
    adaptive_percent: u8,
    interactive_frames_left: u16,
    stable_frame_streak: u16,
    slow_frame_streak: u32,
}

impl LoadGovernor {
    fn new(memory_pressure: MemoryPressure) -> Self {
        Self {
            adaptive_percent: if memory_pressure == MemoryPressure::Critical {
                25
            } else {
                100
            },
            interactive_frames_left: 0,
            stable_frame_streak: 0,
            slow_frame_streak: 0,
        }
    }
}

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
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    pub memory_pressure: MemoryPressure,
    pub renderer: RendererKind,
    /// Measured wall time for one storage operation. This is deliberately an
    /// EWMA rather than a disk-type guess: a busy SSD can be slower than an
    /// idle HDD, and a mapped drive can change while the app is open.
    pub io_latency_ewma_ms: f32,
    /// 25..=100. It is a temporary throughput scale, so a fast machine grows
    /// back to its hardware ceiling after a transient stall.
    governor: LoadGovernor,
    #[cfg_attr(test, allow(dead_code))]
    resource_refresh_frames: u16,
}

impl Default for PerfProfile {
    fn default() -> Self {
        Self::detect(PerfTierPreference::Auto)
    }
}

impl PerfProfile {
    pub fn detect(preference: PerfTierPreference) -> Self {
        Self::from_hardware(HardwareSnapshot::detect(), preference)
    }

    #[cfg(test)]
    pub fn from_cores(cores: usize, preference: PerfTierPreference) -> Self {
        Self::from_hardware(
            HardwareSnapshot {
                cores,
                ..Default::default()
            },
            preference,
        )
    }

    pub fn from_hardware(snapshot: HardwareSnapshot, preference: PerfTierPreference) -> Self {
        let cores = snapshot.cores.max(1);
        let cpu_tier = tier_for_cores(cores);
        let memory_ceiling = tier_for_memory(snapshot.total_memory_bytes);
        let renderer_ceiling = if snapshot.renderer == RendererKind::Software {
            PerfTier::Normal
        } else {
            PerfTier::High
        };
        let base_tier = cpu_tier.min(memory_ceiling).min(renderer_ceiling);
        let requested_tier = match preference {
            PerfTierPreference::Auto => base_tier,
            // Manual tiers are ceilings, not permission to ignore a 4 GiB
            // machine or software renderer.
            PerfTierPreference::Pinned(pinned) => pinned.min(base_tier),
        };
        let memory_pressure =
            memory_pressure(snapshot.total_memory_bytes, snapshot.available_memory_bytes);
        let tier = requested_tier.min(pressure_ceiling(memory_pressure));
        Self {
            cores,
            tier,
            base_tier,
            preference,
            remote_root: false,
            total_memory_bytes: snapshot.total_memory_bytes,
            available_memory_bytes: snapshot.available_memory_bytes,
            memory_pressure,
            renderer: snapshot.renderer,
            io_latency_ewma_ms: 0.0,
            governor: LoadGovernor::new(memory_pressure),
            resource_refresh_frames: 0,
        }
    }

    pub fn set_renderer_description(&mut self, description: &str) {
        self.renderer = RendererKind::from_description(description);
        let cpu_tier = tier_for_cores(self.cores);
        let memory_tier = tier_for_memory(self.total_memory_bytes);
        let renderer_tier = if self.software_renderer() {
            PerfTier::Normal
        } else {
            PerfTier::High
        };
        self.base_tier = cpu_tier.min(memory_tier).min(renderer_tier);
        self.tier = self.tier.min(self.configured_ceiling());
    }

    pub fn note_interaction(&mut self) {
        self.governor.interactive_frames_left = 30;
        self.governor.adaptive_percent = self.governor.adaptive_percent.min(25);
        self.governor.stable_frame_streak = 0;
    }

    pub fn note_io_latency(&mut self, elapsed: Duration) {
        let ms = elapsed.as_secs_f32() * 1_000.0;
        if !ms.is_finite() {
            return;
        }
        self.io_latency_ewma_ms = if self.io_latency_ewma_ms <= 0.0 {
            ms
        } else {
            self.io_latency_ewma_ms * 0.85 + ms * 0.15
        };
    }

    pub fn adaptive_percent(&self) -> u8 {
        self.governor.adaptive_percent
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
        let renderer = self.renderer;
        let io_latency = self.io_latency_ewma_ms;
        let snapshot = HardwareSnapshot {
            cores: self.cores,
            total_memory_bytes: self.total_memory_bytes,
            available_memory_bytes: self.available_memory_bytes,
            renderer,
        };
        *self = Self::from_hardware(snapshot, preference);
        self.remote_root = remote_root;
        self.io_latency_ewma_ms = io_latency;
    }

    /// Feed each finished frame's wall time in. Returns true when the tier
    /// changed, so the caller can log it.
    ///
    /// Only `Auto` demotes: a pinned tier is the user's explicit instruction
    /// and must not drift underneath them.
    pub fn note_frame_ms(&mut self, frame_ms: f32) -> bool {
        let mut changed = false;
        #[cfg(not(test))]
        {
            self.resource_refresh_frames = self.resource_refresh_frames.saturating_add(1);
            if self.resource_refresh_frames >= 120 {
                self.resource_refresh_frames = 0;
                let (total, available) = detect_physical_memory();
                if total.is_some() {
                    self.total_memory_bytes = total;
                }
                if available.is_some() {
                    self.available_memory_bytes = available;
                }
                let next_pressure =
                    memory_pressure(self.total_memory_bytes, self.available_memory_bytes);
                if next_pressure != self.memory_pressure {
                    self.memory_pressure = next_pressure;
                    // Callers also use this signal to resize/evict caches;
                    // pressure matters even when the tier was already Low.
                    changed = true;
                    let capped = self.tier.min(self.configured_ceiling());
                    self.tier = capped;
                    match next_pressure {
                        MemoryPressure::Normal => {}
                        MemoryPressure::High => {
                            self.governor.adaptive_percent = self.governor.adaptive_percent.min(50)
                        }
                        MemoryPressure::Critical => {
                            self.governor.adaptive_percent = self.governor.adaptive_percent.min(25)
                        }
                    }
                }
            }
        }
        if self.governor.interactive_frames_left > 0 {
            self.governor.interactive_frames_left -= 1;
        }
        if frame_ms >= 50.0 {
            self.governor.adaptive_percent = (self.governor.adaptive_percent / 2).max(25);
            self.governor.stable_frame_streak = 0;
        } else if frame_ms < 20.0 && self.governor.interactive_frames_left == 0 {
            self.governor.stable_frame_streak = self.governor.stable_frame_streak.saturating_add(1);
            if self.governor.stable_frame_streak >= 120 {
                if self.governor.adaptive_percent < 100 {
                    self.governor.adaptive_percent =
                        self.governor.adaptive_percent.saturating_add(25).min(100);
                } else {
                    let promoted = self.tier.promoted().min(self.configured_ceiling());
                    if promoted != self.tier {
                        self.tier = promoted;
                        self.governor.adaptive_percent = 75;
                        changed = true;
                    }
                }
                self.governor.stable_frame_streak = 0;
            }
        } else {
            self.governor.stable_frame_streak = 0;
        }

        if self.preference != PerfTierPreference::Auto || self.tier == PerfTier::Low {
            return changed;
        }
        if frame_ms < SLOW_FRAME_MS {
            self.governor.slow_frame_streak = 0;
            return changed;
        }
        self.governor.slow_frame_streak = self.governor.slow_frame_streak.saturating_add(1);
        if self.governor.slow_frame_streak < SLOW_FRAMES_BEFORE_DEMOTE {
            return changed;
        }
        self.governor.slow_frame_streak = 0;
        self.tier = self.tier.demoted();
        true
    }

    fn configured_ceiling(&self) -> PerfTier {
        let preference = match self.preference {
            PerfTierPreference::Auto => self.base_tier,
            PerfTierPreference::Pinned(tier) => tier.min(self.base_tier),
        };
        preference.min(pressure_ceiling(self.memory_pressure))
    }

    pub fn demoted_from_hardware(&self) -> bool {
        self.preference == PerfTierPreference::Auto && self.tier < self.base_tier
    }

    // ---- Derived budgets ---------------------------------------------------

    fn scale_duration(&self, duration: Duration, minimum_micros: u64) -> Duration {
        let micros = duration.as_micros() as u64;
        Duration::from_micros(
            micros
                .saturating_mul(self.governor.adaptive_percent as u64)
                .saturating_div(100)
                .max(minimum_micros),
        )
    }

    /// How long the whole pre-UI drain phase may spend before deferring the
    /// rest to the next frame. Deliberately well under one 60fps frame: the
    /// UI still has to lay out and paint after this.
    pub fn frame_budget(&self) -> Duration {
        self.scale_duration(
            Duration::from_micros(match self.tier {
                PerfTier::Low => 4_000,
                PerfTier::Normal => 8_000,
                PerfTier::High => 12_000,
            }),
            1_000,
        )
    }

    /// Lists at or below this size sort/filter synchronously in one frame.
    /// Above it the sliced + worker path runs instead.
    pub fn list_sync_threshold(&self) -> usize {
        match self.tier {
            PerfTier::Low => 256,
            PerfTier::Normal => 2_000,
            PerfTier::High => 8_000,
        }
    }

    /// Per-frame time budget for the sliced decorate / filter passes.
    pub fn list_job_frame_budget_ms(&self) -> f64 {
        let base: f64 = match self.tier {
            PerfTier::Low => 1.0,
            PerfTier::Normal => 2.0,
            PerfTier::High => 3.0,
        };
        (base * self.governor.adaptive_percent as f64 / 100.0).max(0.25)
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
        self.scale_duration(
            Duration::from_micros(match self.tier {
                PerfTier::Low => 1_500,
                PerfTier::Normal => 3_000,
                PerfTier::High => 5_000,
            }),
            250,
        )
    }

    /// Concurrent heavy restore/decode jobs during a session open.
    pub fn restore_concurrency(&self) -> usize {
        if self.remote_root
            || self.io_latency_ewma_ms >= 100.0
            || self.memory_pressure == MemoryPressure::Critical
        {
            return 1;
        }
        let base = match self.tier {
            PerfTier::Low => 1,
            PerfTier::Normal => self.cores.saturating_sub(1).clamp(1, 3),
            PerfTier::High => self.cores.saturating_sub(1).clamp(1, 8),
        };
        self.scale_workers(base)
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
        if self.remote_root
            || self.io_latency_ewma_ms >= 100.0
            || self.memory_pressure == MemoryPressure::Critical
        {
            return 1;
        }
        let by_tier = match self.tier {
            PerfTier::Low => 1,
            PerfTier::Normal => self.cores.saturating_sub(1).clamp(1, 3),
            PerfTier::High => (self.cores / 2).max(1),
        };
        self.scale_workers(by_tier.min(cap.max(1)))
    }

    /// Worker count for the list metadata pool.
    pub fn meta_pool_workers(&self) -> usize {
        if self.remote_root
            || self.io_latency_ewma_ms >= 100.0
            || self.memory_pressure == MemoryPressure::Critical
        {
            // More readers do not make a share faster; they make every
            // read slower and crowd out the file the user opened.
            return 1;
        }
        self.scale_workers(self.meta_pool_capacity())
    }

    pub fn meta_pool_capacity(&self) -> usize {
        if self.remote_root
            || self.io_latency_ewma_ms >= 100.0
            || self.memory_pressure == MemoryPressure::Critical
        {
            return 1;
        }
        match self.tier {
            PerfTier::Low => 1,
            PerfTier::Normal => self.cores.saturating_sub(1).clamp(1, 4),
            PerfTier::High => self.cores.saturating_sub(1).clamp(1, 8),
        }
    }

    fn scale_workers(&self, workers: usize) -> usize {
        workers
            .saturating_mul(self.governor.adaptive_percent as usize)
            .saturating_add(99)
            .saturating_div(100)
            .max(1)
    }

    pub fn scan_batch_size(&self) -> usize {
        match self.tier {
            PerfTier::Low => 32,
            PerfTier::Normal => 128,
            PerfTier::High => 512,
        }
    }

    pub fn scan_queue_batches(&self) -> usize {
        match self.tier {
            PerfTier::Low => 4,
            PerfTier::Normal => 8,
            PerfTier::High => 16,
        }
    }

    pub fn path_status_drain_limit(&self) -> usize {
        match self.tier {
            PerfTier::Low => 64,
            PerfTier::Normal => 256,
            PerfTier::High => 512,
        }
    }

    pub fn meta_update_drain_limit(&self) -> usize {
        let base = match self.tier {
            PerfTier::Low => 8,
            PerfTier::Normal => 32,
            PerfTier::High => 128,
        };
        self.scale_workers(base)
    }

    /// Capacity for streams whose messages hold per-file results. The
    /// producer blocks at this boundary, keeping a descheduled UI from
    /// turning completed work into an unbounded memory backlog.
    pub fn background_result_queue_capacity(&self) -> usize {
        match self.tier {
            PerfTier::Low => 8,
            PerfTier::Normal => 32,
            PerfTier::High => 128,
        }
    }

    pub fn background_result_drain_limit(&self) -> usize {
        self.scale_workers(match self.tier {
            PerfTier::Low => 8,
            PerfTier::Normal => 32,
            PerfTier::High => 96,
        })
    }

    pub fn folder_watch_batch_size(&self) -> usize {
        match self.tier {
            PerfTier::Low => 16,
            PerfTier::Normal => 32,
            PerfTier::High => 64,
        }
    }

    pub fn folder_watch_queue_batches(&self) -> usize {
        match self.tier {
            PerfTier::Low => 2,
            PerfTier::Normal => 4,
            PerfTier::High => 8,
        }
    }

    pub fn spectrogram_queue_tiles(&self) -> usize {
        match self.tier {
            PerfTier::Low => 2,
            PerfTier::Normal => 4,
            PerfTier::High => 8,
        }
    }

    /// Shared budget for caches that are cheap to regenerate. Keeping this a
    /// small fraction of RAM prevents the fixed 256MB+256MB+128MB ceilings
    /// from paging a 4GB laptop while still letting workstations cache more.
    pub fn optional_cache_budget_bytes(&self) -> usize {
        let detected = self
            .total_memory_bytes
            .map(|bytes| bytes / 32)
            .unwrap_or(match self.tier {
                PerfTier::Low => 128 * 1024 * 1024,
                PerfTier::Normal => 256 * 1024 * 1024,
                PerfTier::High => 512 * 1024 * 1024,
            } as u64);
        let mut budget = detected.clamp(128 * 1024 * 1024, 768 * 1024 * 1024) as usize;
        if self.memory_pressure == MemoryPressure::High {
            budget /= 2;
        } else if self.memory_pressure == MemoryPressure::Critical {
            budget /= 4;
        }
        budget.max(32 * 1024 * 1024)
    }

    pub fn spectro_cache_bytes(&self) -> usize {
        self.optional_cache_budget_bytes() * 17 / 100
    }

    pub fn analysis_cache_bytes(&self) -> usize {
        self.optional_cache_budget_bytes() * 8 / 100
    }

    pub fn metadata_memory_cache_bytes(&self) -> usize {
        self.optional_cache_budget_bytes() * 15 / 100
    }

    pub fn undo_cache_bytes(&self) -> usize {
        self.optional_cache_budget_bytes() * 35 / 100
    }

    pub fn visual_cache_bytes(&self) -> usize {
        self.optional_cache_budget_bytes() * 10 / 100
    }

    pub fn video_cache_bytes(&self) -> usize {
        self.optional_cache_budget_bytes() * 15 / 100
    }

    /// Maximum estimated PCM bytes that full-decode workers may hold while
    /// decoding concurrently. A single oversized/unknown clip may still use
    /// the whole allowance, but excludes every other full decode until it
    /// releases its permit.
    pub fn full_decode_budget_bytes(&self) -> usize {
        (self.optional_cache_budget_bytes() / 4).clamp(32 * 1024 * 1024, 192 * 1024 * 1024)
    }

    pub fn metadata_disk_cache_bytes(&self) -> u64 {
        match self.tier {
            PerfTier::Low => 128 * 1024 * 1024,
            PerfTier::Normal => 256 * 1024 * 1024,
            PerfTier::High => 512 * 1024 * 1024,
        }
    }

    pub fn software_renderer(&self) -> bool {
        self.renderer == RendererKind::Software
    }

    pub fn background_repaint_ms(&self) -> u64 {
        if self.software_renderer() {
            80
        } else {
            50
        }
    }

    pub fn texture_uploads_per_frame(&self) -> usize {
        if self.software_renderer() || self.tier == PerfTier::Low {
            1
        } else {
            4
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
        let tier_cap = if self.remote_root {
            24 * 1024 * 1024
        } else {
            match self.tier {
                PerfTier::Low => 24 * 1024 * 1024,
                PerfTier::Normal => 48 * 1024 * 1024,
                PerfTier::High => 96 * 1024 * 1024,
            }
        };
        tier_cap.min(self.video_cache_bytes()).max(2 * 1024 * 1024)
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
        if self.software_renderer() {
            return 0;
        }
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

fn tier_for_cores(cores: usize) -> PerfTier {
    if cores <= 2 {
        PerfTier::Low
    } else if cores >= 8 {
        PerfTier::High
    } else {
        PerfTier::Normal
    }
}

fn tier_for_memory(total: Option<u64>) -> PerfTier {
    match total {
        Some(bytes) if bytes <= 4 * GIB => PerfTier::Low,
        Some(bytes) if bytes <= 8 * GIB => PerfTier::Normal,
        _ => PerfTier::High,
    }
}

fn pressure_ceiling(pressure: MemoryPressure) -> PerfTier {
    match pressure {
        MemoryPressure::Normal => PerfTier::High,
        MemoryPressure::High => PerfTier::Normal,
        MemoryPressure::Critical => PerfTier::Low,
    }
}

fn memory_pressure(total: Option<u64>, available: Option<u64>) -> MemoryPressure {
    let (Some(total), Some(available)) = (total, available) else {
        return MemoryPressure::Normal;
    };
    let critical = (total / 20).max(256 * 1024 * 1024);
    let high = (total / 10).max(512 * 1024 * 1024);
    if available <= critical {
        MemoryPressure::Critical
    } else if available <= high {
        MemoryPressure::High
    } else {
        MemoryPressure::Normal
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
    fn demotion_recovers_only_after_sustained_stable_frames() {
        let mut profile = PerfProfile::from_cores(16, PerfTierPreference::Auto);
        for _ in 0..SLOW_FRAMES_BEFORE_DEMOTE {
            profile.note_frame_ms(SLOW_FRAME_MS + 1.0);
        }
        assert_eq!(profile.tier, PerfTier::Normal);
        for _ in 0..479 {
            profile.note_frame_ms(0.5);
        }
        assert_eq!(profile.tier, PerfTier::Normal);
        profile.note_frame_ms(0.5);
        assert_eq!(profile.tier, PerfTier::High);
    }

    #[test]
    fn a_pinned_tier_keeps_its_tier_but_still_throttles() {
        let mut profile = PerfProfile::from_cores(16, PerfTierPreference::Pinned(PerfTier::High));
        for _ in 0..1_000 {
            profile.note_frame_ms(SLOW_FRAME_MS * 10.0);
        }
        assert_eq!(profile.tier, PerfTier::High);
        assert_eq!(profile.adaptive_percent(), 25);
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
        assert!(remote.video_ring_memory_bytes() <= remote.video_cache_bytes());
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
        assert!(normal.video_ring_memory_bytes() <= normal.video_cache_bytes());
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

    fn hardware(
        cores: usize,
        total_gib: u64,
        available_gib: u64,
        renderer: RendererKind,
    ) -> HardwareSnapshot {
        HardwareSnapshot {
            cores,
            total_memory_bytes: Some(total_gib * GIB),
            available_memory_bytes: Some(available_gib * GIB),
            renderer,
        }
    }

    #[test]
    fn memory_and_software_rendering_cap_a_fast_cpu() {
        let low_memory = PerfProfile::from_hardware(
            hardware(32, 4, 3, RendererKind::Hardware),
            PerfTierPreference::Auto,
        );
        assert_eq!(low_memory.tier, PerfTier::Low);

        let software = PerfProfile::from_hardware(
            hardware(32, 32, 24, RendererKind::Software),
            PerfTierPreference::Auto,
        );
        assert_eq!(software.tier, PerfTier::Normal);
        assert_eq!(software.texture_uploads_per_frame(), 1);
        assert_eq!(software.video_poster_concurrency(), 0);
        assert!(software.background_repaint_ms() >= 80);
    }

    #[test]
    fn manual_high_is_a_ceiling_not_a_safety_bypass() {
        let profile = PerfProfile::from_hardware(
            hardware(32, 4, 3, RendererKind::Hardware),
            PerfTierPreference::Pinned(PerfTier::High),
        );
        assert_eq!(profile.base_tier, PerfTier::Low);
        assert_eq!(profile.tier, PerfTier::Low);
        assert!(profile.scan_queue_batches() * profile.scan_batch_size() <= 128);
    }

    #[test]
    fn memory_pressure_and_io_latency_force_one_reader() {
        let mut critical = PerfProfile::from_hardware(
            HardwareSnapshot {
                cores: 32,
                total_memory_bytes: Some(32 * GIB),
                available_memory_bytes: Some(128 * 1024 * 1024),
                renderer: RendererKind::Hardware,
            },
            PerfTierPreference::Pinned(PerfTier::High),
        );
        assert_eq!(critical.memory_pressure, MemoryPressure::Critical);
        assert_eq!(critical.tier, PerfTier::Low);
        assert_eq!(critical.restore_concurrency(), 1);
        assert_eq!(critical.meta_pool_workers(), 1);

        critical.available_memory_bytes = Some(24 * GIB);
        critical.memory_pressure = MemoryPressure::Normal;
        critical.note_io_latency(Duration::from_millis(150));
        assert_eq!(critical.restore_concurrency(), 1);
        assert_eq!(critical.meta_pool_workers(), 1);
        assert_eq!(critical.scan_pool_workers(64), 1);
    }

    #[test]
    fn interaction_shrinks_workers_immediately_and_stability_restores_them() {
        let mut profile = PerfProfile::from_hardware(
            hardware(16, 32, 24, RendererKind::Hardware),
            PerfTierPreference::Auto,
        );
        let full = profile.meta_pool_workers();
        assert!(full > 1);
        profile.note_interaction();
        assert_eq!(profile.adaptive_percent(), 25);
        assert!(profile.meta_pool_workers() < full);
        for _ in 0..390 {
            profile.note_frame_ms(1.0);
        }
        assert_eq!(profile.adaptive_percent(), 100);
        assert_eq!(profile.meta_pool_workers(), full);
    }

    #[test]
    fn regenerable_cache_budget_tracks_ram_with_hard_bounds() {
        let low = PerfProfile::from_hardware(
            hardware(4, 4, 3, RendererKind::Hardware),
            PerfTierPreference::Auto,
        );
        let workstation = PerfProfile::from_hardware(
            hardware(32, 64, 48, RendererKind::Hardware),
            PerfTierPreference::Auto,
        );
        assert_eq!(low.optional_cache_budget_bytes(), 128 * 1024 * 1024);
        assert_eq!(workstation.optional_cache_budget_bytes(), 768 * 1024 * 1024);
        assert!(workstation.meta_pool_workers() > low.meta_pool_workers());
        let partitioned = workstation.metadata_memory_cache_bytes()
            + workstation.spectro_cache_bytes()
            + workstation.analysis_cache_bytes()
            + workstation.undo_cache_bytes()
            + workstation.visual_cache_bytes()
            + workstation.video_cache_bytes();
        assert!(partitioned <= workstation.optional_cache_budget_bytes());
    }
}
