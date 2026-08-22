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
use std::time::Instant;

use super::{PlaybackSourceKind, PlaybackTransportKind, WavesPreviewer};

pub(crate) const LIST_TRANSPORT_FADE_IN_MS: f32 = 5.0;
const DECODED_GUARD_SECS: f64 = 0.02;
const MIN_RESUME_HEADROOM_SECS: f64 = crate::app::LIST_PLAY_EMIT_SECS as f64 * 2.0;
const REBUFFER_LOW_WATER_SECS: f64 = crate::app::LIST_PLAY_EMIT_SECS as f64 / 3.0;
const MIN_DECODE_SPEED_MARGIN: f64 = 1.25;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListSeekPhase {
    Begin,
    Update,
    Commit,
    Cancel,
}

/// A seek requested from a row's waveform, applied after the table's borrows
/// are released (`select_and_load` can remove a missing row, which would
/// mutate `items` while the table is iterating).
#[derive(Clone, Debug)]
pub(crate) struct ListSeekRequest {
    pub(crate) row: usize,
    pub(crate) frac: f32,
    pub(crate) phase: ListSeekPhase,
}

/// Pointer-down state for one waveform seek. The real transport remains at
/// `original_source_time_sec` until `Commit` arrives.
#[derive(Clone, Debug)]
pub(crate) struct ListSeekGesture {
    pub(crate) row: usize,
    pub(crate) path: PathBuf,
    pub(crate) frac: f32,
    pub(crate) resume_playing: bool,
    pub(crate) original_source_time_sec: Option<f64>,
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

/// Timing observed for the active progressive list decode. All durations are
/// transport-buffer seconds so comparison with the audio callback's playback
/// rate is direct even in Speed mode.
#[derive(Clone, Debug)]
pub(crate) struct ListDecodeProgress {
    pub(crate) job_id: u64,
    pub(crate) started_at: Instant,
    pub(crate) last_emit_at: Option<Instant>,
    pub(crate) decoded_transport_secs: f64,
    pub(crate) cumulative_speed: f64,
    pub(crate) recent_speed: Option<f64>,
    pub(crate) last_emit_gap_secs: f64,
    pub(crate) complete: bool,
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
    if duration_secs <= 0.0 || duration_secs.is_nan() || !source_time_sec.is_finite() {
        return 0.0;
    }
    ((source_time_sec / duration_secs) as f32).clamp(0.0, 1.0)
}

pub(crate) fn list_decoded_frac(decoded_source_secs: f64, duration_secs: f64) -> f32 {
    // An unknown duration must read as fully decoded, or the whole row would be
    // shaded as unseekable.
    if duration_secs <= 0.0 || duration_secs.is_nan() {
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

fn buffered_resume_ready(
    complete: bool,
    decoded_ahead_secs: f64,
    cumulative_speed: f64,
    recent_speed: Option<f64>,
    last_emit_gap_secs: f64,
    playback_rate: f64,
) -> bool {
    if complete {
        return true;
    }
    let playback_rate = playback_rate.clamp(0.25, 4.0);
    let recent = recent_speed
        .filter(|speed| speed.is_finite() && *speed > 0.0)
        .unwrap_or(cumulative_speed);
    let conservative_speed = cumulative_speed.min(recent);
    if !conservative_speed.is_finite()
        || conservative_speed < playback_rate * MIN_DECODE_SPEED_MARGIN
    {
        return false;
    }
    let adaptive_headroom =
        (last_emit_gap_secs.max(0.0) * 2.0 * playback_rate).max(MIN_RESUME_HEADROOM_SECS);
    decoded_ahead_secs.is_finite() && decoded_ahead_secs >= adaptive_headroom
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

    pub(crate) fn list_transport_is_whole_file(&self) -> bool {
        self.playback_session.transport == PlaybackTransportKind::ExactStreamWav
    }

    /// Source-time seconds the decoded transport currently reaches, mapped
    /// through the timeline so rate/pitch modes agree with the playhead.
    pub(crate) fn list_decoded_source_secs(&self) -> f64 {
        let len = self.audio.current_source_len();
        if len == 0 {
            return 0.0;
        }
        self.playback_session
            .timeline_map
            .source_time_for_transport_frame(len as f64)
    }

    pub(crate) fn list_sounding_path(&self) -> Option<&Path> {
        match &self.playback_session.source {
            PlaybackSourceKind::ListPreview(path) => Some(path.as_path()),
            _ => None,
        }
    }

    /// Playhead state for whichever row is sounding (or awaiting a parked
    /// seek), or `None` when the list is silent.
    pub(crate) fn resolve_list_playhead_frame(&self) -> Option<ListPlayheadFrame> {
        let path = self
            .list_seek_gesture
            .as_ref()
            .map(|gesture| gesture.path.clone())
            .or_else(|| self.list_seek_pending.as_ref().map(|p| p.path.clone()))
            .or_else(|| self.list_sounding_path().map(Path::to_path_buf))?;
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
                    .list_seek_gesture
                    .as_ref()
                    .filter(|gesture| gesture.path == path)
                    .map(|gesture| gesture.frac)
                    .or_else(|| {
                        self.list_seek_pending
                            .as_ref()
                            .filter(|pending| pending.path == path)
                            .map(|pending| pending.frac)
                    }),
                decoded_frac,
                playing: sounding && self.playback_is_playing_now(),
            },
            path,
        })
    }

    pub(crate) fn clear_list_seek_pending(&mut self) {
        self.list_seek_pending = None;
    }

    pub(crate) fn clear_list_seek_runtime(&mut self) {
        self.list_seek_pending = None;
        self.list_seek_gesture = None;
        self.list_decode_progress = None;
    }

    pub(crate) fn has_active_list_seek_gesture(&self) -> bool {
        self.list_seek_gesture.is_some()
    }

    pub(crate) fn finish_list_seek_gesture_from_pointer_state(&mut self, focused: bool) {
        let Some(gesture) = self.list_seek_gesture.as_ref() else {
            return;
        };
        let phase = if focused {
            ListSeekPhase::Commit
        } else {
            ListSeekPhase::Cancel
        };
        let req = ListSeekRequest {
            row: gesture.row,
            frac: gesture.frac,
            phase,
        };
        self.apply_list_seek_request(req);
    }

    pub(super) fn begin_list_decode_progress(&mut self, job_id: u64) {
        self.list_decode_progress = Some(ListDecodeProgress {
            job_id,
            started_at: Instant::now(),
            last_emit_at: None,
            decoded_transport_secs: 0.0,
            cumulative_speed: 0.0,
            recent_speed: None,
            last_emit_gap_secs: crate::app::LIST_PLAY_EMIT_SECS as f64,
            complete: false,
        });
    }

    pub(super) fn record_list_decode_progress(
        &mut self,
        job_id: u64,
        decoded_transport_secs: f64,
        is_final: bool,
    ) {
        let now = Instant::now();
        let whole_file_job = self.list_preview_job_max_secs <= 0.0;
        let Some(progress) = self
            .list_decode_progress
            .as_mut()
            .filter(|progress| progress.job_id == job_id)
        else {
            return;
        };
        let total_elapsed = now
            .saturating_duration_since(progress.started_at)
            .as_secs_f64()
            .max(1e-6);
        progress.cumulative_speed = decoded_transport_secs.max(0.0) / total_elapsed;
        if let Some(last_emit_at) = progress.last_emit_at {
            let gap = now
                .saturating_duration_since(last_emit_at)
                .as_secs_f64()
                .max(1e-6);
            let decoded_delta = (decoded_transport_secs - progress.decoded_transport_secs).max(0.0);
            progress.recent_speed = Some(decoded_delta / gap);
            progress.last_emit_gap_secs = gap;
        } else {
            progress.last_emit_gap_secs = total_elapsed;
        }
        progress.last_emit_at = Some(now);
        progress.decoded_transport_secs = decoded_transport_secs.max(0.0);
        progress.complete = is_final && whole_file_job;
    }

    /// True when a decode covering the whole file is already running.
    pub(crate) fn list_full_decode_in_flight(&self) -> bool {
        self.list_preview_rx.is_some() && self.list_preview_job_max_secs <= 0.0
    }

    fn ensure_list_full_decode(&mut self, path: PathBuf) {
        if !self.list_transport_is_whole_file() && !self.list_full_decode_in_flight() {
            self.spawn_list_preview_async(path, 0.0, crate::app::LIST_PLAY_EMIT_SECS);
        }
    }

    fn list_decoded_ahead_transport_secs(&self, source_time_sec: f64) -> f64 {
        let target_frame = self
            .playback_session
            .timeline_map
            .transport_frame_for_source_time(source_time_sec);
        let decoded_frames = self.audio.current_source_len();
        decoded_frames.saturating_sub(target_frame) as f64
            / self.playback_session.transport_sr.max(1) as f64
    }

    fn list_buffer_ready_to_resume(&self, source_time_sec: f64) -> bool {
        if self.list_transport_is_whole_file() {
            return true;
        }
        let Some(progress) = self.list_decode_progress.as_ref() else {
            // A resident/cached full buffer has no active decode telemetry.
            return self.list_preview_rx.is_none() && !self.list_preview_partial_ready;
        };
        buffered_resume_ready(
            progress.complete,
            self.list_decoded_ahead_transport_secs(source_time_sec),
            progress.cumulative_speed,
            progress.recent_speed,
            progress.last_emit_gap_secs,
            self.audio
                .shared
                .rate
                .load(std::sync::atomic::Ordering::Relaxed) as f64,
        )
    }

    fn park_list_seek(
        &mut self,
        path: PathBuf,
        source_time_sec: f64,
        frac: f32,
        resume_playing: bool,
    ) {
        self.audio.stop();
        self.list_seek_pending = Some(ListSeekPending {
            path: path.clone(),
            source_time_sec,
            frac: frac.clamp(0.0, 1.0),
            resume_playing,
        });
        self.ensure_list_full_decode(path);
    }

    fn begin_list_seek_gesture(&mut self, row: usize, path: PathBuf, frac: f32) {
        if self.list_row_duration_secs(&path).is_none() {
            return;
        }
        if self.list_seek_gesture.is_some() {
            self.finish_list_seek_gesture_from_pointer_state(false);
        }
        let sounding = self.list_sounding_path() == Some(path.as_path());
        let was_playing = self.playback_is_playing_now();
        let original_source_time_sec = sounding
            .then(|| self.playback_current_source_time_sec())
            .flatten();
        let resume_playing = self.auto_play_list_nav || was_playing;
        self.clear_list_seek_pending();

        if !sounding || self.selected_path_buf().as_deref() != Some(path.as_path()) {
            self.update_selection_on_click(row, egui::Modifiers::NONE);
            self.select_and_load_without_autoplay(row, false);
            // Exact WAV seeking should stay immediate even when Auto Play is
            // off. This activation loads the stream but never starts it.
            let _ = self.try_activate_list_stream_transport(&path);
        }
        self.audio.stop();
        self.list_seek_gesture = Some(ListSeekGesture {
            row,
            path: path.clone(),
            frac: frac.clamp(0.0, 1.0),
            resume_playing,
            original_source_time_sec,
        });
        self.ensure_list_full_decode(path);
    }

    fn commit_list_seek_gesture(&mut self, row: usize, frac: f32) {
        let Some(mut gesture) = self.list_seek_gesture.take() else {
            return;
        };
        if gesture.row != row {
            self.list_seek_gesture = Some(gesture);
            return;
        }
        gesture.frac = frac.clamp(0.0, 1.0);
        let duration = self.list_row_duration_secs(&gesture.path);
        let outcome = decide_list_seek(
            gesture.frac,
            duration,
            self.list_decoded_source_secs(),
            self.list_transport_is_whole_file(),
        );
        match outcome {
            ListSeekOutcome::SeekNow(target)
                if !gesture.resume_playing || self.list_buffer_ready_to_resume(target) =>
            {
                self.clear_list_seek_pending();
                self.playback_seek_to_source_time(self.mode, target);
                if gesture.resume_playing {
                    self.audio.play_declicked(LIST_TRANSPORT_FADE_IN_MS);
                }
            }
            ListSeekOutcome::SeekNow(target) | ListSeekOutcome::WaitForDecode(target) => {
                self.park_list_seek(gesture.path, target, gesture.frac, gesture.resume_playing);
            }
            ListSeekOutcome::Ignore => {}
        }
    }

    fn cancel_list_seek_gesture(&mut self) {
        let Some(gesture) = self.list_seek_gesture.take() else {
            return;
        };
        self.audio.stop();
        if self.list_sounding_path() == Some(gesture.path.as_path()) {
            if let Some(original_time) = gesture.original_source_time_sec {
                self.playback_seek_to_source_time(self.mode, original_time);
            }
        }
    }

    /// Apply a seek requested from a row's waveform.
    pub(crate) fn apply_list_seek_request(&mut self, req: ListSeekRequest) {
        let Some(path) = self.path_for_row(req.row).cloned() else {
            return;
        };
        match req.phase {
            ListSeekPhase::Begin => self.begin_list_seek_gesture(req.row, path, req.frac),
            ListSeekPhase::Update => {
                if let Some(gesture) = self
                    .list_seek_gesture
                    .as_mut()
                    .filter(|gesture| gesture.row == req.row && gesture.path == path)
                {
                    gesture.frac = req.frac.clamp(0.0, 1.0);
                }
            }
            ListSeekPhase::Commit => {
                // A very fast click can deliver press+release in one egui
                // frame. Build the gesture first so it still follows the same
                // no-audio-before-commit path.
                if self.list_seek_gesture.is_none() {
                    self.begin_list_seek_gesture(req.row, path, req.frac);
                }
                self.commit_list_seek_gesture(req.row, req.frac);
            }
            ListSeekPhase::Cancel => self.cancel_list_seek_gesture(),
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
        let covered = self.list_transport_is_whole_file()
            || pending.source_time_sec <= (decoded - DECODED_GUARD_SECS).max(0.0);
        let safe_to_apply = covered
            && (!pending.resume_playing
                || self.list_buffer_ready_to_resume(pending.source_time_sec));
        if safe_to_apply {
            self.clear_list_seek_pending();
            self.playback_seek_to_source_time(self.mode, pending.source_time_sec);
            if pending.resume_playing {
                self.audio.play_declicked(LIST_TRANSPORT_FADE_IN_MS);
            }
            return;
        }
        // The decode finished without reaching the target: a shorter file than
        // the metadata claimed, or a decode error. Land on what we have rather
        // than waiting forever. This is the termination condition - no timer.
        if self.list_preview_rx.is_none() {
            self.clear_list_seek_pending();
            self.playback_seek_to_source_time(self.mode, decoded.max(0.0));
            if pending.resume_playing {
                self.audio.play_declicked(LIST_TRANSPORT_FADE_IN_MS);
            }
        }
    }

    /// Keep a progressively decoded transport away from its moving buffer end.
    /// A slow decoder is allowed to pause/rebuffer, but never to hit the end and
    /// repeatedly restart with audible discontinuities.
    pub(crate) fn maintain_list_playback_buffer(&mut self) {
        if self.list_seek_pending.is_some()
            || self.list_seek_gesture.is_some()
            || !self.playback_is_playing_now()
            || self.list_transport_is_whole_file()
        {
            return;
        }
        let Some(path) = self.list_sounding_path().map(Path::to_path_buf) else {
            return;
        };
        let progressive = self.list_preview_rx.is_some()
            || self
                .list_decode_progress
                .as_ref()
                .is_some_and(|progress| !progress.complete);
        if !progressive {
            return;
        }
        let pos = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        let ahead = self.audio.current_source_len().saturating_sub(pos) as f64
            / self.playback_session.transport_sr.max(1) as f64;
        if ahead > REBUFFER_LOW_WATER_SECS {
            return;
        }
        let source_time = self.playback_current_source_time_sec().unwrap_or(0.0);
        let frac = self
            .list_row_duration_secs(&path)
            .map(|duration| list_play_frac_from_source_time(source_time, duration))
            .unwrap_or(0.0);
        self.park_list_seek(path, source_time, frac, true);
    }

    pub(crate) fn set_pending_list_resume_intent(&mut self, resume: bool) {
        if let Some(pending) = &mut self.list_seek_pending {
            pending.resume_playing = resume;
        }
    }

    /// Gate a manual Play on an already loaded progressive List buffer through
    /// the same headroom policy as a seek release.
    pub(crate) fn prepare_current_list_transport_for_resume(&mut self, path: &Path) -> bool {
        let source_time = self.playback_current_source_time_sec().unwrap_or(0.0);
        if self.list_buffer_ready_to_resume(source_time) {
            return true;
        }
        let frac = self
            .list_row_duration_secs(path)
            .map(|duration| list_play_frac_from_source_time(source_time, duration))
            .unwrap_or(0.0);
        self.park_list_seek(path.to_path_buf(), source_time, frac, true);
        false
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

    #[test]
    fn progressive_resume_requires_speed_margin_and_headroom() {
        assert!(!buffered_resume_ready(
            false,
            10.0,
            1.1,
            Some(1.1),
            0.5,
            1.0,
        ));
        assert!(!buffered_resume_ready(
            false,
            MIN_RESUME_HEADROOM_SECS - 0.01,
            3.0,
            Some(3.0),
            0.5,
            1.0,
        ));
        assert!(buffered_resume_ready(
            false,
            MIN_RESUME_HEADROOM_SECS,
            3.0,
            Some(3.0),
            0.5,
            1.0,
        ));
    }

    #[test]
    fn long_decode_emit_gap_increases_required_headroom() {
        assert!(!buffered_resume_ready(false, 1.9, 4.0, Some(4.0), 1.0, 1.0,));
        assert!(buffered_resume_ready(false, 2.0, 4.0, Some(4.0), 1.0, 1.0,));
        assert!(buffered_resume_ready(true, 0.0, 0.0, None, 10.0, 4.0,));
    }
}
