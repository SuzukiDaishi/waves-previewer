//! Seeking the list preview from the row waveform.
//!
//! The trap this module exists for: for `PlaybackTransportKind::Buffer` the
//! list preview is usually a truncated *prefix* of the file
//! (`LIST_PLAY_PREFIX_SECS_BASE`, 0.6s), grown progressively by
//! `drain_list_preview_results`. So `play_pos / current_source_len()` is a
//! fraction of the decoded prefix, while the thumb, the markers and the loop
//! region the same cell draws are fractions of the whole file. Everything here
//! converts through source-time seconds using `FileMeta::duration_secs`, which
//! the header pass resolves before any decode - so the seek bar works on a row
//! whose waveform has not been drawn yet.
//!
//! `PlaybackTransportKind::ExactStreamWav` streams the whole file, so a seek
//! there is immediate and needs none of the pending machinery.

use std::path::{Path, PathBuf};

use super::{PlaybackSourceKind, PlaybackTransportKind, WavesPreviewer};

/// A seek requested from a row's waveform, applied after the table's borrows
/// are released (`select_and_load` can remove a missing row, which would
/// mutate `items` while the table is iterating).
#[derive(Clone, Debug)]
pub(crate) struct ListSeekRequest {
    pub(crate) row: usize,
    pub(crate) frac: f32,
    /// True while the pointer is still down. A scrub must not re-load the row
    /// or restart a decode on every pixel of the drag.
    pub(crate) scrubbing: bool,
}

/// A seek the decoded buffer does not reach yet, parked until it does.
#[derive(Clone, Debug)]
pub(crate) struct ListSeekPending {
    pub(crate) path: PathBuf,
    pub(crate) source_time_sec: f64,
    pub(crate) frac: f32,
    /// Play once the buffer reaches the position. Playback is held until then:
    /// starting from zero while waiting would audibly play the head of the
    /// file for as long as the decode takes.
    pub(crate) resume_playing: bool,
}

/// Playback position for one row's wave cell.
///
/// A sibling of `ListWaveOverlayInfo` rather than more fields on it: that
/// struct is marker/loop geometry with a `PartialEq` used for change
/// detection, and this changes every frame while playing.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ListWavePlayheadInfo {
    /// Whole-file fraction of the playhead, when this row is the sounding one.
    pub(crate) play_frac: Option<f32>,
    /// Whole-file fraction a parked seek is waiting on.
    pub(crate) pending_frac: Option<f32>,
    /// Whole-file fraction the decoded buffer reaches. 1.0 for a whole-file
    /// transport; less than that means seeking past it has to wait.
    pub(crate) decoded_frac: f32,
    pub(crate) playing: bool,
}

/// Resolved once per frame in `ui_list_view`; the row loop then costs one path
/// comparison per visible row instead of a transport query.
pub(crate) struct ListPlayheadFrame {
    pub(crate) path: PathBuf,
    pub(crate) info: ListWavePlayheadInfo,
}

/// Never seek into the last few milliseconds of what has been decoded: the
/// chunk boundary is not sample-exact and landing on it stalls playback.
const DECODED_GUARD_SECS: f64 = 0.02;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ListSeekOutcome {
    /// The transport already covers this position.
    SeekNow(f64),
    /// Park it and let the full decode catch up.
    WaitForDecode(f64),
    /// No duration known, so a fraction cannot become a time.
    Ignore,
}

pub(crate) fn list_seek_frac_to_source_time(frac: f32, duration_secs: f64) -> f64 {
    (frac.clamp(0.0, 1.0) as f64) * duration_secs.max(0.0)
}

pub(crate) fn list_play_frac_from_source_time(source_time_sec: f64, duration_secs: f64) -> f32 {
    if !(duration_secs > 0.0) || !source_time_sec.is_finite() {
        return 0.0;
    }
    ((source_time_sec / duration_secs) as f32).clamp(0.0, 1.0)
}

pub(crate) fn list_decoded_frac(decoded_source_secs: f64, duration_secs: f64) -> f32 {
    if !(duration_secs > 0.0) {
        return 1.0;
    }
    ((decoded_source_secs / duration_secs) as f32).clamp(0.0, 1.0)
}

/// The prefix trap, in one testable place.
pub(crate) fn decide_list_seek(
    frac: f32,
    duration_secs: Option<f64>,
    decoded_source_secs: f64,
    whole_file_transport: bool,
) -> ListSeekOutcome {
    let Some(duration) = duration_secs.filter(|d| d.is_finite() && *d > 0.0) else {
        return ListSeekOutcome::Ignore;
    };
    let target = list_seek_frac_to_source_time(frac, duration);
    if whole_file_transport || target <= (decoded_source_secs - DECODED_GUARD_SECS).max(0.0) {
        ListSeekOutcome::SeekNow(target)
    } else {
        ListSeekOutcome::WaitForDecode(target)
    }
}

impl WavesPreviewer {
    /// Whole-file duration for a row, from cached metadata only - never the
    /// filesystem.
    pub(crate) fn list_row_duration_secs(&self, path: &Path) -> Option<f64> {
        self.meta_for_path(path)
            .and_then(|m| m.duration_secs)
            .map(|d| d as f64)
            .filter(|d| d.is_finite() && *d > 0.0)
    }

    fn list_transport_is_whole_file(&self) -> bool {
        self.playback_session.transport == PlaybackTransportKind::ExactStreamWav
    }

    /// Source-time seconds the decoded transport currently reaches, mapped
    /// through the timeline so rate/pitch modes agree with the playhead.
    fn list_decoded_source_secs(&self) -> f64 {
        let len = self.audio.current_source_len();
        if len == 0 {
            return 0.0;
        }
        self.playback_session
            .timeline_map
            .source_time_for_transport_frame(len as f64)
    }

    fn list_sounding_path(&self) -> Option<&Path> {
        match &self.playback_session.source {
            PlaybackSourceKind::ListPreview(path) => Some(path.as_path()),
            _ => None,
        }
    }

    /// Playhead state for whichever row is sounding (or awaiting a parked
    /// seek), or `None` when the list is silent.
    pub(crate) fn resolve_list_playhead_frame(&self) -> Option<ListPlayheadFrame> {
        let path = self
            .list_sounding_path()
            .map(Path::to_path_buf)
            .or_else(|| self.list_seek_pending.as_ref().map(|p| p.path.clone()))?;
        let duration = self.list_row_duration_secs(&path);
        let sounding = self.list_sounding_path() == Some(path.as_path());
        let play_frac = duration.and_then(|d| {
            if !sounding {
                return None;
            }
            self.playback_current_source_time_sec()
                .map(|t| list_play_frac_from_source_time(t, d))
        });
        let decoded_frac = match duration {
            Some(d) if sounding && !self.list_transport_is_whole_file() => {
                list_decoded_frac(self.list_decoded_source_secs(), d)
            }
            _ => 1.0,
        };
        Some(ListPlayheadFrame {
            info: ListWavePlayheadInfo {
                play_frac,
                pending_frac: self
                    .list_seek_pending
                    .as_ref()
                    .filter(|p| p.path == path)
                    .map(|p| p.frac),
                decoded_frac,
                playing: sounding && self.playback_is_playing_now(),
            },
            path,
        })
    }

    pub(crate) fn clear_list_seek_pending(&mut self) {
        self.list_seek_pending = None;
    }

    /// True when a decode covering the whole file is already running.
    fn list_full_decode_in_flight(&self) -> bool {
        self.list_preview_rx.is_some() && self.list_preview_job_max_secs <= 0.0
    }

    /// Apply a seek requested from a row's waveform.
    pub(crate) fn apply_list_seek_request(&mut self, req: ListSeekRequest) {
        let Some(path) = self.path_for_row(req.row).cloned() else {
            return;
        };
        if self.list_row_duration_secs(&path).is_none() {
            return;
        }
        let sounding = self.list_sounding_path() == Some(path.as_path());
        let was_playing = self.playback_is_playing_now();
        if !sounding {
            // Mid-scrub the row is already loaded; re-loading it every pixel
            // would restart the decode and drop the transport.
            if req.scrubbing {
                return;
            }
            self.update_selection_on_click(req.row, egui::Modifiers::NONE);
            self.select_and_load(req.row, false);
        }
        // Honour the existing preferences: the click always parks the position,
        // but it only starts playback if the user's setup says it should.
        let resume_playing = self.auto_play_list_nav || was_playing;
        match decide_list_seek(
            req.frac,
            self.list_row_duration_secs(&path),
            self.list_decoded_source_secs(),
            self.list_transport_is_whole_file(),
        ) {
            ListSeekOutcome::SeekNow(target) => {
                self.clear_list_seek_pending();
                self.playback_seek_to_source_time(self.mode, target);
                if resume_playing {
                    self.audio.play();
                }
            }
            ListSeekOutcome::WaitForDecode(target) => {
                self.audio.stop();
                self.list_seek_pending = Some(ListSeekPending {
                    path: path.clone(),
                    source_time_sec: target,
                    frac: req.frac.clamp(0.0, 1.0),
                    resume_playing,
                });
                // Starting a decode per pixel of a scrub would thrash; wait
                // for the drag to end.
                if !req.scrubbing && !self.list_full_decode_in_flight() {
                    self.spawn_list_preview_async(path, 0.0, crate::app::LIST_PLAY_EMIT_SECS);
                }
            }
            ListSeekOutcome::Ignore => {}
        }
    }

    /// Retire a parked seek once the buffer covers it. Called every frame
    /// right after `drain_list_preview_results`, and returns immediately when
    /// nothing is parked.
    pub(crate) fn apply_pending_list_seek(&mut self) {
        let Some(pending) = self.list_seek_pending.clone() else {
            return;
        };
        // A different row took over the transport; the seek is stale.
        if self.list_sounding_path() != Some(pending.path.as_path()) {
            if self.playing_path.as_deref() != Some(pending.path.as_path()) {
                self.clear_list_seek_pending();
            }
            return;
        }
        let decoded = self.list_decoded_source_secs();
        let reached = self.list_transport_is_whole_file()
            || pending.source_time_sec <= (decoded - DECODED_GUARD_SECS).max(0.0);
        if reached {
            self.clear_list_seek_pending();
            self.playback_seek_to_source_time(self.mode, pending.source_time_sec);
            if pending.resume_playing {
                self.audio.play();
            }
            return;
        }
        // The decode finished without reaching the target: a shorter file than
        // the metadata claimed, or a decode error. Land on what we have rather
        // than waiting forever. This is the termination condition - no timer.
        if !self.list_preview_rx.is_some() {
            self.clear_list_seek_pending();
            self.playback_seek_to_source_time(self.mode, decoded.max(0.0));
            if pending.resume_playing {
                self.audio.play();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trap this module exists for. A 5-minute file whose list preview has
    /// decoded only its 0.6s prefix: a click at the middle is a real position
    /// in the file, but the buffer does not hold it yet, so it must be parked
    /// rather than clamped to the end of the prefix (which would silently play
    /// the wrong part of the file).
    #[test]
    fn a_seek_past_the_decoded_prefix_waits_for_the_decode() {
        let duration = Some(300.0);
        assert_eq!(
            decide_list_seek(0.5, duration, 0.6, false),
            ListSeekOutcome::WaitForDecode(150.0)
        );
    }

    /// The same click on a plain wav, which streams whole-file, is immediate.
    #[test]
    fn a_whole_file_transport_seeks_immediately() {
        assert_eq!(
            decide_list_seek(0.5, Some(300.0), 0.6, true),
            ListSeekOutcome::SeekNow(150.0)
        );
    }

    #[test]
    fn a_seek_inside_the_decoded_prefix_is_immediate() {
        // 0.1s into a 300s file is inside a 0.6s prefix.
        match decide_list_seek(0.000_333, Some(300.0), 0.6, false) {
            ListSeekOutcome::SeekNow(t) => assert!(t < 0.6, "{t}"),
            other => panic!("expected SeekNow, got {other:?}"),
        }
    }

    /// Landing exactly on the decoded boundary stalls playback, so the guard
    /// band must push it into the waiting branch.
    #[test]
    fn the_decoded_boundary_itself_is_treated_as_not_yet_reached() {
        assert!(matches!(
            decide_list_seek(1.0, Some(10.0), 10.0, false),
            ListSeekOutcome::WaitForDecode(_)
        ));
    }

    /// Without a duration a fraction cannot become a time. The cell suppresses
    /// seeking in that state, and this is the backstop.
    #[test]
    fn no_duration_means_no_seek() {
        for duration in [None, Some(0.0), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_eq!(
                decide_list_seek(0.5, duration, 10.0, false),
                ListSeekOutcome::Ignore,
                "duration={duration:?}"
            );
        }
    }

    #[test]
    fn fraction_and_time_round_trip() {
        let duration = 137.5;
        for frac in [0.0f32, 0.001, 0.25, 0.5, 0.75, 1.0] {
            let t = list_seek_frac_to_source_time(frac, duration);
            let back = list_play_frac_from_source_time(t, duration);
            assert!((back - frac).abs() < 1e-6, "frac={frac} back={back}");
        }
    }

    #[test]
    fn fractions_stay_in_range_for_hostile_inputs() {
        assert_eq!(list_seek_frac_to_source_time(-1.0, 10.0), 0.0);
        assert_eq!(list_seek_frac_to_source_time(2.0, 10.0), 10.0);
        assert_eq!(list_play_frac_from_source_time(-5.0, 10.0), 0.0);
        assert_eq!(list_play_frac_from_source_time(50.0, 10.0), 1.0);
        assert_eq!(list_play_frac_from_source_time(1.0, 0.0), 0.0);
        assert_eq!(list_play_frac_from_source_time(f64::NAN, 10.0), 0.0);
        // An unknown duration must not shade the whole row as undecoded.
        assert_eq!(list_decoded_frac(0.0, 0.0), 1.0);
        assert_eq!(list_decoded_frac(5.0, 10.0), 0.5);
        assert_eq!(list_decoded_frac(50.0, 10.0), 1.0);
    }
}
