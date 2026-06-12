//! Frame-to-frame cache for editor preview-overlay min/max columns.
//!
//! Computing overlay bins scans every overlay sample visible in the viewport;
//! zoomed out that is the entire preview buffer (potentially tens of millions
//! of samples) per lane, per frame. The inputs only change when the user pans,
//! zooms, or a new preview buffer arrives, so caching the computed columns by
//! the exact view parameters makes the common (static view / playback) case
//! nearly free.

/// Identifies one overlay binning request. Two requests with equal keys are
/// guaranteed to produce identical columns: the buffer is identified by
/// pointer + length (preview buffers are immutable once published), and all
/// remaining fields are the binning parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OverlayBinsKey {
    /// `PreviewOverlay::revision` — guards against allocator address reuse.
    pub overlay_revision: u64,
    pub buf_ptr: usize,
    pub buf_len: usize,
    pub start: usize,
    pub visible_len: usize,
    pub startb: usize,
    pub over_vis: usize,
    pub bins: usize,
    /// Loop-unwrap mapping inputs (zeroed for the plain base-column mapping).
    pub unwrap_base_total: usize,
    pub unwrap_loop_start: usize,
    pub unwrap_overlay_total: usize,
}

struct OverlayBinsEntry {
    key: OverlayBinsKey,
    values: Vec<(f32, f32)>,
}

/// Small LRU-ish cache (one slot per lane is typical; capacity covers
/// mixdown + a few channel lanes without scanning a large map).
#[derive(Default)]
pub struct OverlayBinsCache {
    entries: Vec<OverlayBinsEntry>,
}

const MAX_ENTRIES: usize = 12;

impl OverlayBinsCache {
    /// Returns cached columns for `key`, computing them with `compute` on miss.
    pub fn get_or_compute(
        &mut self,
        key: OverlayBinsKey,
        compute: impl FnOnce() -> Vec<(f32, f32)>,
    ) -> &[(f32, f32)] {
        if let Some(idx) = self.entries.iter().position(|e| e.key == key) {
            // Move to the back so frequently used lanes stay resident.
            let entry = self.entries.remove(idx);
            self.entries.push(entry);
        } else {
            let values = compute();
            if self.entries.len() >= MAX_ENTRIES {
                self.entries.remove(0);
            }
            self.entries.push(OverlayBinsEntry { key, values });
        }
        &self
            .entries
            .last()
            .expect("entry pushed above")
            .values
    }
}
