//! Multi-variation audition: play the selected list rows one after
//! another (round-robin or random) to compare variations of a sound.
//! The audition ends on any explicit stop (Space, selecting another row,
//! Cancel) and advances automatically on each natural playback end.

use std::path::PathBuf;

use crate::app::types::{ToastSeverity, VariationAuditionMode, VariationAuditionState};

impl crate::app::WavesPreviewer {
    pub(super) fn start_variation_audition(&mut self, mode: VariationAuditionMode) {
        let paths = self.selected_paths();
        if paths.len() < 2 {
            self.push_toast(
                ToastSeverity::Warning,
                "Audition: select two or more files first",
            );
            return;
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            .max(1);
        let mut state = VariationAuditionState {
            paths,
            mode,
            cursor: 0,
            played: 1,
            item_started: false,
            rng: seed,
        };
        if mode == VariationAuditionMode::Random {
            state.cursor = (Self::variation_rng_next(&mut state.rng) as usize) % state.paths.len();
        }
        let first = state.paths[state.cursor].clone();
        self.variation_audition = Some(state);
        if !self.variation_play_path(&first) {
            self.variation_audition = None;
        }
    }

    pub(super) fn cancel_variation_audition(&mut self) {
        self.variation_audition = None;
    }

    fn variation_rng_next(rng: &mut u64) -> u64 {
        *rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *rng >> 33
    }

    /// Next cursor for the audition walk: round-robin cycles in order;
    /// random never repeats the current item (for `len >= 2`).
    pub(crate) fn variation_next_cursor(
        mode: VariationAuditionMode,
        cursor: usize,
        len: usize,
        rng: &mut u64,
    ) -> usize {
        if len == 0 {
            return 0;
        }
        match mode {
            VariationAuditionMode::RoundRobin => (cursor + 1) % len,
            VariationAuditionMode::Random => {
                if len <= 1 {
                    0
                } else {
                    let mut next = (Self::variation_rng_next(rng) as usize) % len;
                    if next == cursor {
                        next = (next + 1) % len;
                    }
                    next
                }
            }
        }
    }

    /// Select the row for `path` and start (or queue) its playback through
    /// the normal list-preview machinery.
    fn variation_play_path(&mut self, path: &PathBuf) -> bool {
        let Some(row) = self.row_for_path(path) else {
            return false;
        };
        self.variation_audition_advancing = true;
        self.select_and_load(row, true);
        if self.force_load_selected_list_preview_for_play() {
            if self.playback_mode_needs_fx_buffer() && !self.spawn_playback_fx_render(true) {
                self.list_play_pending = true;
            } else {
                self.audio.play();
            }
        }
        // Not immediately playable => list_play_pending autoplays on load.
        self.variation_audition_advancing = false;
        true
    }

    /// Per-frame driver: mark the current item as heard while it plays,
    /// and on stop decide between natural end (advance) and user stop
    /// (cancel) by whether the playhead reached the end of the buffer.
    pub(super) fn poll_variation_audition(&mut self, ctx: &egui::Context) {
        if self.variation_audition.is_none() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let playing = self
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        if playing {
            if let Some(state) = self.variation_audition.as_mut() {
                state.item_started = true;
            }
            return;
        }
        self.variation_audition_step();
    }

    /// One stopped-playback decision step (separated from the frame poll
    /// so tests can drive it deterministically).
    pub(super) fn variation_audition_step(&mut self) {
        let item_started = self
            .variation_audition
            .as_ref()
            .map(|s| s.item_started)
            .unwrap_or(false);
        if !item_started {
            // Still inside the async load window for the current item.
            return;
        }
        let len = self.audio.current_source_len();
        let pos = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        if len == 0 || pos + 2 < len {
            // Stopped mid-file: an explicit user stop ends the audition.
            self.variation_audition = None;
            return;
        }
        let next_path = {
            let Some(state) = self.variation_audition.as_mut() else {
                return;
            };
            let next =
                Self::variation_next_cursor(state.mode, state.cursor, state.paths.len(), &mut state.rng);
            state.cursor = next;
            state.played += 1;
            state.item_started = false;
            state.paths[next].clone()
        };
        if !self.variation_play_path(&next_path) {
            self.variation_audition = None;
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_set_list_multi_selection(&mut self, paths: &[PathBuf]) -> bool {
        let mut rows: Vec<usize> = paths
            .iter()
            .filter_map(|p| self.row_for_path(p))
            .collect();
        if rows.is_empty() {
            return false;
        }
        rows.sort_unstable();
        rows.dedup();
        self.selected_multi.clear();
        for row in &rows {
            self.selected_multi.insert(*row);
        }
        self.selected = Some(rows[0]);
        self.select_anchor = Some(rows[0]);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_start_variation_audition(&mut self, random: bool) -> bool {
        let mode = if random {
            VariationAuditionMode::Random
        } else {
            VariationAuditionMode::RoundRobin
        };
        self.start_variation_audition(mode);
        self.variation_audition.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_variation_audition_cursor(&self) -> Option<(usize, usize)> {
        self.variation_audition
            .as_ref()
            .map(|s| (s.cursor, s.played))
    }

    #[cfg(feature = "kittest")]
    pub fn test_variation_simulate_natural_end(&mut self) -> bool {
        if self.variation_audition.is_none() {
            return false;
        }
        if let Some(state) = self.variation_audition.as_mut() {
            state.item_started = true;
        }
        let len = self.audio.current_source_len();
        self.audio
            .shared
            .playing
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.audio
            .shared
            .play_pos
            .store(len, std::sync::atomic::Ordering::Relaxed);
        self.variation_audition_step();
        self.variation_audition.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_variation_simulate_user_stop(&mut self) -> bool {
        if self.variation_audition.is_none() {
            return false;
        }
        if let Some(state) = self.variation_audition.as_mut() {
            state.item_started = true;
        }
        self.audio
            .shared
            .playing
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.audio
            .shared
            .play_pos
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.variation_audition_step();
        self.variation_audition.is_none()
    }
}

#[cfg(test)]
mod tests {
    use crate::app::types::VariationAuditionMode;
    use crate::app::WavesPreviewer;

    #[test]
    fn round_robin_cycles_in_order() {
        let mut rng = 7u64;
        let mut cursor = 0usize;
        let seen: Vec<usize> = (0..6)
            .map(|_| {
                cursor = WavesPreviewer::variation_next_cursor(
                    VariationAuditionMode::RoundRobin,
                    cursor,
                    3,
                    &mut rng,
                );
                cursor
            })
            .collect();
        assert_eq!(seen, vec![1, 2, 0, 1, 2, 0]);
    }

    #[test]
    fn random_never_repeats_current() {
        let mut rng = 42u64;
        let mut cursor = 0usize;
        for _ in 0..200 {
            let next = WavesPreviewer::variation_next_cursor(
                VariationAuditionMode::Random,
                cursor,
                4,
                &mut rng,
            );
            assert_ne!(next, cursor, "random audition repeated the same item");
            assert!(next < 4);
            cursor = next;
        }
    }

    #[test]
    fn degenerate_lengths_are_safe() {
        let mut rng = 1u64;
        assert_eq!(
            WavesPreviewer::variation_next_cursor(VariationAuditionMode::Random, 0, 0, &mut rng),
            0
        );
        assert_eq!(
            WavesPreviewer::variation_next_cursor(VariationAuditionMode::Random, 0, 1, &mut rng),
            0
        );
        assert_eq!(
            WavesPreviewer::variation_next_cursor(
                VariationAuditionMode::RoundRobin,
                0,
                1,
                &mut rng
            ),
            0
        );
    }
}
