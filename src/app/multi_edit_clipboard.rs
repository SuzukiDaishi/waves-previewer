//! Copying clips on a Multi Edits timeline, and pasting them back.
//!
//! The clipboard is the app's own and holds one clip: what was selected
//! (its source, in-point, length and fades) and the kind of track it sat on.
//! The OS clipboard is left alone -- a clip is a reference into a list row,
//! nothing another program could paste -- and a clip copied on one timeline
//! pastes onto another.
//!
//! Ctrl+V pastes at the playhead and, when stopped, moves the playhead to
//! the end of the pasted clip, so pressing it again lays the next copy right
//! after the last. While playing it only pastes: moving the playhead would
//! make the sound jump.

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

    /// Copy the selected clip. Returns whether there was one.
    pub(super) fn multi_edit_copy_selected(&mut self) -> bool {
        let Some(clip_id) = self.multi_edit.ui.selected_clip.clone() else {
            return false;
        };
        let Some(doc) = self.multi_edit_active_doc() else {
            return false;
        };
        let Some((ti, ci)) = doc.clip_location(&clip_id) else {
            return false;
        };
        let track = &doc.tracks[ti];
        self.multi_edit.ui.clip_clipboard = Some(CopiedClip {
            clip: track.clips[ci].clone(),
            kind: track.kind,
            track_id: track.id.clone(),
        });
        true
    }

    /// Copy the selected clip and take it off the timeline.
    pub(super) fn multi_edit_cut_selected(&mut self) -> bool {
        if !self.multi_edit_copy_selected() {
            return false;
        }
        let Some(clip_id) = self.multi_edit.ui.selected_clip.take() else {
            return false;
        };
        self.multi_edit_checkpoint();
        if let Some(doc) = self.multi_edit_active_doc_mut() {
            doc.remove_clip(&clip_id);
        }
        self.multi_edit_touched();
        true
    }

    /// Paste the copied clip at the playhead, on the track `paste_target`
    /// picks (made when there is none), and select it. One undo step.
    /// Returns the new clip's id.
    pub(super) fn multi_edit_paste(&mut self) -> Option<String> {
        let copied = self.multi_edit.ui.clip_clipboard.clone()?;
        let doc_id = self.multi_edit.active.clone()?;
        let at = self.multi_edit_playhead(&doc_id);
        let playing = self.multi_edit_is_playing(&doc_id);
        let selected_track = self.multi_edit.ui.selected_track.clone();
        let selected_clip = self.multi_edit.ui.selected_clip.clone();
        let source_sr = Some(self.resolve_file_sample_rate(&copied.clip.source.path))
            .filter(|rate| !rate.is_assumed())
            .map(|rate| rate.hz);
        self.multi_edit_checkpoint();
        let doc = self.multi_edit_active_doc_mut()?;
        let target = doc
            .paste_target(
                copied.kind,
                selected_track.as_deref(),
                selected_clip.as_deref(),
                Some(&copied.track_id),
            )
            .unwrap_or_else(|| doc.add_track(copied.kind));
        let id = doc.paste_clip(target, &copied.clip, at)?;
        // A first clip on an empty timeline sets its rate, as a drop does.
        if doc.timeline_sr == 0 {
            if let Some(sr) = source_sr {
                doc.timeline_sr = sr;
            }
        }
        self.multi_edit_select_clip(Some(id.clone()));
        self.multi_edit_touched();
        if !playing {
            self.multi_edit_seek(at + copied.clip.len_secs);
        }
        Some(id)
    }
}
