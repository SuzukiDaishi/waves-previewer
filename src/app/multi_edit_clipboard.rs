//! Copying clips on a Multi Edits timeline, and pasting them back.
//!
//! The clipboard is the app's own and holds the selected clips: each one's
//! source, in-point, length and fades, how far below the topmost of them its
//! track was, and its kind. The OS clipboard is left alone -- a clip is a
//! reference into a list row, nothing another program could paste -- and
//! clips copied on one timeline paste onto another.
//!
//! Ctrl+V pastes the group with its earliest clip at the playhead and, when
//! stopped, moves the playhead to the group's end, so pressing it again lays
//! the next copy right after the last. While playing it only pastes: moving
//! the playhead would make the sound jump.

use super::multi_edit::{Clip, TrackKind};
use super::WavesPreviewer;

/// A copied clip, and where it came from.
#[derive(Clone, Debug)]
pub(crate) struct CopiedClip {
    pub clip: Clip,
    pub kind: TrackKind,
    /// The track it was copied from: where a paste goes when neither a
    /// track nor a clip is selected.
    pub track_id: String,
    /// How many tracks below the topmost copied clip's its track was.
    pub track_offset: usize,
}

impl WavesPreviewer {
    /// Ctrl+C / Ctrl+X / Ctrl+V while the timeline owns the keys.
    /// `os_paste` is the Ctrl+V Windows saw that egui did not report (see
    /// `os_paste_key`); any one of the three signals pastes once.
    pub(super) fn handle_multi_edit_clipboard_hotkeys(&mut self, ctx: &egui::Context, os_paste: bool) {
        let (mut copy, mut cut, mut paste) = (false, false, false);
        ctx.input_mut(|i| {
            i.events.retain(|event| match event {
                egui::Event::Copy => {
                    copy = true;
                    false
                }
                egui::Event::Cut => {
                    cut = true;
                    false
                }
                egui::Event::Paste(_) => {
                    paste = true;
                    false
                }
                _ => true,
            });
            copy |= i.consume_key(egui::Modifiers::COMMAND, egui::Key::C);
            cut |= i.consume_key(egui::Modifiers::COMMAND, egui::Key::X);
            paste |= i.consume_key(egui::Modifiers::COMMAND, egui::Key::V);
        });
        if cut {
            self.multi_edit_cut_selected();
        } else if copy {
            self.multi_edit_copy_selected();
        }
        if paste || os_paste {
            self.multi_edit_paste();
        }
    }

    /// Copy the selected clips. Returns whether there were any.
    pub(super) fn multi_edit_copy_selected(&mut self) -> bool {
        let ids = self.multi_edit_selected_ids();
        let Some(doc) = self.multi_edit_active_doc() else {
            return false;
        };
        let located: Vec<(usize, usize)> = ids.iter().filter_map(|id| doc.clip_location(id)).collect();
        let Some(top) = located.iter().map(|&(ti, _)| ti).min() else {
            return false;
        };
        let copied = located
            .into_iter()
            .map(|(ti, ci)| {
                let track = &doc.tracks[ti];
                CopiedClip {
                    clip: track.clips[ci].clone(),
                    kind: track.kind,
                    track_id: track.id.clone(),
                    track_offset: ti - top,
                }
            })
            .collect();
        self.multi_edit.ui.clip_clipboard = Some(copied);
        true
    }

    /// Copy the selected clips and take them off the timeline.
    pub(super) fn multi_edit_cut_selected(&mut self) -> bool {
        if !self.multi_edit_copy_selected() {
            return false;
        }
        let ids = self.multi_edit_selected_ids();
        self.multi_edit_checkpoint();
        if let Some(doc) = self.multi_edit_active_doc_mut() {
            doc.remove_clips(&ids);
        }
        self.multi_edit_select_clip(None);
        self.multi_edit_touched();
        true
    }

    /// Paste the copied clips with the earliest at the playhead, keeping
    /// their spacing and their tracks' order below the track `paste_target`
    /// picks; a clip whose track that would not be (none there, or the other
    /// kind) goes where `paste_target` puts its own kind. The pasted clips
    /// are selected. One undo step. Returns the first new clip's id.
    pub(super) fn multi_edit_paste(&mut self) -> Option<String> {
        let copied = self.multi_edit.ui.clip_clipboard.clone().filter(|c| !c.is_empty())?;
        let doc_id = self.multi_edit.active.clone()?;
        let at = self.multi_edit_playhead(&doc_id);
        let playing = self.multi_edit_is_playing(&doc_id);
        let selected_track = self.multi_edit.ui.selected_track.clone();
        let selected_clip = self.multi_edit.ui.selected_clip.clone();
        let first = copied.iter().map(|c| c.clip.start_secs).reduce(f64::min)?;
        let end = copied.iter().map(|c| c.clip.end_secs()).fold(first, f64::max);
        let top = copied.iter().min_by_key(|c| c.track_offset)?.clone();
        let source_sr = Some(self.resolve_file_sample_rate(&top.clip.source.path))
            .filter(|rate| !rate.is_assumed())
            .map(|rate| rate.hz);
        self.multi_edit_checkpoint();
        let doc = self.multi_edit_active_doc_mut()?;
        let base = doc
            .paste_target(
                top.kind,
                selected_track.as_deref(),
                selected_clip.as_deref(),
                Some(&top.track_id),
            )
            .unwrap_or_else(|| doc.add_track(top.kind));
        let mut ids = Vec::with_capacity(copied.len());
        for c in &copied {
            let below = base + c.track_offset;
            let target = if doc.tracks.get(below).is_some_and(|t| t.kind == c.kind) {
                below
            } else {
                doc.paste_target(c.kind, None, None, Some(&c.track_id))
                    .unwrap_or_else(|| doc.add_track(c.kind))
            };
            if let Some(id) = doc.paste_clip(target, &c.clip, at + (c.clip.start_secs - first)) {
                ids.push(id);
            }
        }
        // A first clip on an empty timeline sets its rate, as a drop does.
        if doc.timeline_sr == 0 {
            if let Some(sr) = source_sr {
                doc.timeline_sr = sr;
            }
        }
        let first_id = ids.first().cloned();
        self.multi_edit_select_clips(ids, first_id.clone());
        self.multi_edit_touched();
        if !playing {
            self.multi_edit_seek(at + (end - first));
        }
        first_id
    }
}
