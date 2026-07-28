//! Channel routing tool: rewire, duplicate and drop channels.
//!
//! The matrix is an NxM patchbay — each output channel names the input channels
//! mixed into it. Unlike every other editor tool this one can change the tab's
//! channel *count*, which is why the apply path has extra bookkeeping for the
//! per-channel state that is sized from that count.

use super::types::ChannelRoutingDraft;
use super::WavesPreviewer;

/// Render the routing matrix into a fresh channel set.
///
/// Outputs fed by several inputs are averaged rather than summed, so folding
/// L+R into mono can't push a legal file past 0 dBFS. An output with no cable
/// is silence of the same length.
pub fn route_channels(src: &[Vec<f32>], draft: &ChannelRoutingDraft) -> Vec<Vec<f32>> {
    let frames = src.iter().map(|c| c.len()).max().unwrap_or(0);
    let out_count = draft.out_count.max(1);
    let mut out = Vec::with_capacity(out_count);
    for dst in 0..out_count {
        let mut buf = vec![0.0f32; frames];
        // Only count sources that actually exist; a stale index must not drag
        // the average down toward silence.
        let sources: Vec<usize> = draft
            .sources
            .get(dst)
            .map(|s| s.iter().copied().filter(|i| *i < src.len()).collect())
            .unwrap_or_default();
        if sources.is_empty() {
            out.push(buf);
            continue;
        }
        let scale = 1.0 / sources.len() as f32;
        for &s in &sources {
            let ch = &src[s];
            for (i, v) in ch.iter().enumerate() {
                buf[i] += *v * scale;
            }
        }
        out.push(buf);
    }
    out
}

impl WavesPreviewer {
    /// Apply the tab's routing matrix as a destructive, undoable edit.
    pub(super) fn editor_apply_channel_routing(&mut self, tab_idx: usize) {
        let undo_state = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            let draft = tab.channel_routing_draft.clone();
            if draft.is_identity() || tab.ch_samples.is_empty() {
                return;
            }
            let undo_state = Self::capture_undo_state_labeled(tab, "Channel Routing");
            tab.ch_samples = route_channels(&tab.ch_samples, &draft);
            tab.dirty = true;
            Self::editor_reset_per_channel_state(tab);
            Self::editor_clamp_ranges(tab);
            undo_state
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
        // The matrix now describes the *old* layout; re-seed it for the new one.
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.channel_routing_draft = ChannelRoutingDraft::identity(tab.ch_samples.len());
        }
        self.notify_if_tab_over_fs(tab_idx);
    }

    /// Drop state whose length is tied to the channel count. Without this a
    /// 4->2 fold keeps mute/solo flags from the old layout, and a `Custom`
    /// channel view silently narrows the scope of every later edit.
    pub(super) fn editor_reset_per_channel_state(tab: &mut super::types::EditorTab) {
        tab.ch_muted.clear();
        tab.ch_solo.clear();
        tab.channel_view = super::types::ChannelView::mixdown();
        tab.mini_meter.peak_hold_db.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(in_count: usize, out: &[&[usize]]) -> ChannelRoutingDraft {
        ChannelRoutingDraft {
            in_count,
            out_count: out.len(),
            sources: out.iter().map(|s| s.to_vec()).collect(),
            connecting_from: None,
        }
    }

    #[test]
    fn identity_passes_audio_through_untouched() {
        let src = vec![vec![0.1, 0.2], vec![0.3, 0.4]];
        let d = ChannelRoutingDraft::identity(2);
        assert!(d.is_identity());
        assert_eq!(route_channels(&src, &d), src);
    }

    #[test]
    fn swap_exchanges_channels() {
        let src = vec![vec![0.1, 0.2], vec![0.3, 0.4]];
        let d = draft(2, &[&[1], &[0]]);
        assert!(!d.is_identity());
        assert_eq!(
            route_channels(&src, &d),
            vec![vec![0.3, 0.4], vec![0.1, 0.2]]
        );
    }

    #[test]
    fn mono_duplicates_into_stereo() {
        let src = vec![vec![0.5, -0.5]];
        let d = draft(1, &[&[0], &[0]]);
        let out = route_channels(&src, &d);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], vec![0.5, -0.5]);
        assert_eq!(out[1], vec![0.5, -0.5]);
    }

    #[test]
    fn multiple_sources_average_so_a_fold_cannot_clip() {
        // Both channels at full scale: summing would give 2.0.
        let src = vec![vec![1.0, -1.0], vec![1.0, -1.0]];
        let out = route_channels(&src, &draft(2, &[&[0, 1]]));
        assert_eq!(out, vec![vec![1.0, -1.0]]);

        let src = vec![vec![0.5], vec![0.1]];
        let out = route_channels(&src, &draft(2, &[&[0, 1]]));
        assert!((out[0][0] - 0.3).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn unconnected_output_is_silence_of_the_same_length() {
        let src = vec![vec![0.1, 0.2, 0.3]];
        let out = route_channels(&src, &draft(1, &[&[0], &[]]));
        assert_eq!(out[1], vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn ragged_channels_pad_to_the_longest() {
        let src = vec![vec![0.4, 0.4, 0.4], vec![0.2]];
        let out = route_channels(&src, &draft(2, &[&[1]]));
        assert_eq!(out[0], vec![0.2, 0.0, 0.0]);
    }

    #[test]
    fn out_of_range_sources_are_ignored_without_skewing_the_average() {
        let src = vec![vec![0.6]];
        // Index 5 no longer exists; the average must still be over 1 source.
        let out = route_channels(&src, &draft(1, &[&[0, 5]]));
        assert!((out[0][0] - 0.6).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn toggle_link_adds_then_removes() {
        let mut d = ChannelRoutingDraft::identity(2);
        assert!(d.toggle_link(1, 0));
        assert!(d.is_linked(1, 0));
        assert_eq!(d.sources[0], vec![0, 1]);
        assert!(!d.toggle_link(1, 0));
        assert!(!d.is_linked(1, 0));
        assert!(d.is_identity());
    }

    #[test]
    fn set_out_count_clamps_and_keeps_existing_rows() {
        let mut d = ChannelRoutingDraft::identity(2);
        d.set_out_count(4);
        assert_eq!(d.out_count, 4);
        assert_eq!(d.sources.len(), 4);
        assert_eq!(d.sources[0], vec![0]);
        assert!(d.sources[3].is_empty());
        d.set_out_count(99);
        assert_eq!(d.out_count, super::super::types::CHANNEL_ROUTING_MAX_OUT);
        d.set_out_count(0);
        assert_eq!(d.out_count, 1);
    }

    #[test]
    fn reseed_when_the_tab_channel_count_changed() {
        let mut d = ChannelRoutingDraft::identity(1);
        d.set_out_count(2);
        d.reseed_if_stale(1);
        assert_eq!(d.out_count, 2, "same channel count keeps the user's matrix");
        d.reseed_if_stale(2);
        assert!(d.is_identity());
        assert_eq!(d.in_count, 2);
    }
}
