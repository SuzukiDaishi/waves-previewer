//! Multi Edits: sources laid out on tracks, played and mixed down together.
//!
//! The document and its edits only -- no UI, no disk, no audio.
//! `multi_edit_render.rs` turns a document into samples and `multi_edit_ops.rs`
//! ties both to the app. See `docs/MULTI_EDITS_SPEC.md`.
//!
//! Every position is in seconds: a clip's place on the timeline, where in its
//! source it starts, how long it runs, and every automation point. Samples
//! would tie the document to one rate, and the same timeline is rendered at
//! `out_sr` to play and at `timeline_sr` to export.
//!
//! Every id is random, never a counter: a session has more than one writer,
//! and two people adding a track at once would otherwise claim the same
//! number (AGENTS.md).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Shortest clip an edit may leave behind. Anything shorter is a click, not
/// a clip, and a trim or split that would produce one is refused instead.
pub const MIN_CLIP_SECS: f64 = 0.01;

/// Zoom limits, in pixels per second of timeline: from about ten minutes on
/// a laptop screen down to single milliseconds.
pub const MIN_PX_PER_SEC: f32 = 2.0;
pub const MAX_PX_PER_SEC: f32 = 20_000.0;
/// Where a new timeline opens: about half a minute across a typical pane.
pub const DEFAULT_PX_PER_SEC: f32 = 40.0;

/// Fader and Gain lane range. The floor is where a track is effectively
/// gone; the ceiling leaves room to lift a quiet source without inviting
/// the kind of boost that clips the mix.
pub const TRACK_GAIN_MIN_DB: f32 = -60.0;
pub const TRACK_GAIN_MAX_DB: f32 = 12.0;
/// Pitch lane range: two octaves either way, the span the editor's pitch
/// tools accept.
pub const PITCH_LANE_RANGE_SEMITONES: f32 = 24.0;

/// Row heights, in points before the timeline's vertical zoom. A track row
/// holds its name, fader and clips; a lane row one parameter's curve.
pub const DEFAULT_TRACK_HEIGHT: f32 = 72.0;
pub const MIN_TRACK_HEIGHT: f32 = 36.0;
pub const MAX_TRACK_HEIGHT: f32 = 320.0;
pub const DEFAULT_LANE_HEIGHT: f32 = 56.0;
pub const MIN_LANE_HEIGHT: f32 = 28.0;
pub const MAX_LANE_HEIGHT: f32 = 240.0;
/// The timeline's vertical zoom: every row's height times this.
pub const MIN_TRACK_ZOOM: f32 = 0.5;
pub const MAX_TRACK_ZOOM: f32 = 3.0;
/// However short the timeline, zooming out stops at a view this long, and
/// past the end of the last clip at a quarter of its length again.
const MIN_FIT_SECS: f64 = 10.0;
const FIT_MARGIN: f64 = 1.25;

/// A fresh id: 128 random bits as hex.
pub fn new_id() -> String {
    crate::app::session_sync::new_session_id()
}

/// The candidate nearest `t`, when one is within `threshold`.
fn nearest_candidate(t: f64, candidates: &[f64], threshold: f64) -> Option<f64> {
    candidates
        .iter()
        .copied()
        .filter(|c| c.is_finite() && (c - t).abs() <= threshold)
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
}

/// `t` moved onto the nearest candidate within `threshold`, or left alone.
pub fn snap_secs(t: f64, candidates: &[f64], threshold: f64) -> f64 {
    nearest_candidate(t, candidates, threshold).unwrap_or(t)
}

/// Where a span `len` long goes when the pointer puts its start at `start`:
/// moved so that whichever end is nearer a candidate sits on it, or left
/// alone when neither end is near one.
///
/// Comparing the two snapped starts by how far each moved would not do: an
/// end that caught nothing "moves" by zero and would win every time, and
/// nothing would ever snap.
pub fn snap_span(start: f64, len: f64, candidates: &[f64], threshold: f64) -> f64 {
    let end = start + len;
    match (
        nearest_candidate(start, candidates, threshold),
        nearest_candidate(end, candidates, threshold),
    ) {
        (Some(s), Some(e)) if (e - end).abs() < (s - start).abs() => e - len,
        (Some(s), _) => s,
        (None, Some(e)) => e - len,
        (None, None) => start,
    }
}

/// The furthest the view may scroll: until the end of the last clip sits in
/// the middle of it. Scrolling on into empty time only loses the timeline.
pub fn clamp_scroll_secs(scroll: f64, end_secs: f64, visible_secs: f64) -> f64 {
    let limit = (end_secs - visible_secs * 0.5).max(0.0);
    scroll.clamp(0.0, limit)
}

/// The least pixels per second a view `width_px` wide may zoom out to: the
/// whole timeline and a margin, or `MIN_FIT_SECS` when it is shorter.
pub fn zoom_out_limit(end_secs: f64, width_px: f32) -> f32 {
    let span = (end_secs * FIT_MARGIN).max(MIN_FIT_SECS);
    ((width_px.max(1.0) as f64 / span) as f32).max(MIN_PX_PER_SEC)
}

/// The ruler's tick spacing at a zoom: the smallest of these that keeps two
/// ticks at least `min_px` apart. Grid snapping and arrow-key steps use it
/// too, so all three always agree.
pub fn grid_step_secs(px_per_sec: f32, min_px: f32) -> f64 {
    const STEPS: [f64; 14] = [
        0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0,
    ];
    STEPS
        .iter()
        .copied()
        .find(|step| (*step as f32) * px_per_sec >= min_px)
        .unwrap_or(3600.0)
}

/// The next grid line from `t` in direction `dir` (+1 or -1): a time already
/// on a line moves a whole step, one between lines moves onto the nearer
/// line in that direction. Never below zero.
pub fn step_to_grid(t: f64, dir: i32, step: f64) -> f64 {
    if !(step > 0.0) {
        return t;
    }
    let pos = t / step;
    let eps = 1e-6;
    let next = if dir > 0 {
        ((pos + eps).floor() + 1.0) * step
    } else {
        ((pos - eps).ceil() - 1.0) * step
    };
    next.max(0.0)
}

/// Where a step from `t` towards `target` stops: at the first of `stops`
/// (the markers) it would pass, or at `target` when it passes none. A stop
/// at `t` itself is the one being left, not one in the way.
pub fn stop_before(t: f64, target: f64, stops: impl IntoIterator<Item = f64>) -> f64 {
    const EPS: f64 = 1e-9;
    let mut stop = target;
    for s in stops.into_iter().filter(|s| s.is_finite()) {
        if target > t && s > t + EPS && s < stop {
            stop = s;
        } else if target < t && s < t - EPS && s > stop {
            stop = s;
        }
    }
    stop
}

/// Grid lines every `step` seconds from `start` to `end`, inclusive.
pub fn grid_points(start: f64, end: f64, step: f64) -> Vec<f64> {
    if !(step > 0.0) || end < start {
        return Vec::new();
    }
    let first = (start / step).ceil() as i64;
    let last = (end / step).floor() as i64;
    (first..=last).map(|n| n as f64 * step).collect()
}

fn default_track_height() -> f32 {
    DEFAULT_TRACK_HEIGHT
}

fn default_lane_height() -> f32 {
    DEFAULT_LANE_HEIGHT
}

fn default_true() -> bool {
    true
}

fn default_track_zoom() -> f32 {
    1.0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    #[default]
    Audio,
    /// Holds video files: their sound goes into the mix like any other clip,
    /// and the picture is shown in the preview. Nothing here writes video.
    Video,
}

impl TrackKind {
    pub fn default_name_prefix(self) -> &'static str {
        match self {
            TrackKind::Audio => "Track",
            TrackKind::Video => "Video",
        }
    }
}

/// What an automation lane moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneParam {
    /// dB on top of the fader, interpolated in dB.
    Gain,
    /// Semitones, interpolated in semitones (an even glissando).
    Pitch,
    /// -1 (hard left) .. 1 (hard right).
    Pan,
    /// 0 = sounding, 1 = muted. Steps rather than ramps.
    Mute,
}

impl LaneParam {
    pub const ALL: [LaneParam; 4] = [
        LaneParam::Gain,
        LaneParam::Pitch,
        LaneParam::Pan,
        LaneParam::Mute,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LaneParam::Gain => "Gain",
            LaneParam::Pitch => "Pitch",
            LaneParam::Pan => "Pan",
            LaneParam::Mute => "Mute",
        }
    }

    /// The lane's value range, bottom to top of the lane.
    pub fn range(self) -> (f32, f32) {
        match self {
            LaneParam::Gain => (TRACK_GAIN_MIN_DB, TRACK_GAIN_MAX_DB),
            LaneParam::Pitch => (-PITCH_LANE_RANGE_SEMITONES, PITCH_LANE_RANGE_SEMITONES),
            LaneParam::Pan => (-1.0, 1.0),
            LaneParam::Mute => (0.0, 1.0),
        }
    }

    /// The value a lane with no points holds, and what a new lane draws at.
    pub fn neutral(self) -> f32 {
        0.0
    }

    /// Mute is on or off; a point between the two means nothing.
    pub fn is_stepped(self) -> bool {
        matches!(self, LaneParam::Mute)
    }

    pub fn clamp(self, value: f32) -> f32 {
        let (lo, hi) = self.range();
        let value = if value.is_finite() { value } else { self.neutral() };
        let value = value.clamp(lo, hi);
        if self.is_stepped() {
            if value >= 0.5 {
                1.0
            } else {
                0.0
            }
        } else {
            value
        }
    }

    pub fn format_value(self, value: f32) -> String {
        match self {
            LaneParam::Gain => format!("{value:+.1} dB"),
            LaneParam::Pitch => format!("{value:+.1} st"),
            LaneParam::Pan => {
                let pct = (value * 100.0).round() as i32;
                if pct == 0 {
                    "C".to_string()
                } else if pct < 0 {
                    format!("L{}", -pct)
                } else {
                    format!("R{pct}")
                }
            }
            LaneParam::Mute => {
                if value >= 0.5 {
                    "Muted".to_string()
                } else {
                    "On".to_string()
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    /// Timeline seconds.
    pub secs: f64,
    pub value: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    pub id: String,
    pub param: LaneParam,
    /// Sorted by `secs`; every edit below keeps them so.
    #[serde(default)]
    pub points: Vec<AutomationPoint>,
    #[serde(default = "default_lane_height")]
    pub height: f32,
}

impl AutomationLane {
    pub fn new(param: LaneParam) -> Self {
        Self {
            id: new_id(),
            param,
            points: Vec::new(),
            height: DEFAULT_LANE_HEIGHT,
        }
    }

    /// Whether the lane changes anything at all.
    pub fn is_neutral(&self) -> bool {
        let neutral = self.param.neutral();
        self.points
            .iter()
            .all(|point| (point.value - neutral).abs() <= f32::EPSILON)
    }

    /// The lane's value at `secs`: the first point's value before it, the
    /// last point's after it, linear between (a step for Mute), and the
    /// neutral value when there are no points.
    pub fn value_at(&self, secs: f64) -> f32 {
        let Some(first) = self.points.first() else {
            return self.param.neutral();
        };
        if secs <= first.secs {
            return first.value;
        }
        for pair in self.points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if secs < b.secs {
                if self.param.is_stepped() || b.secs <= a.secs {
                    return a.value;
                }
                let t = ((secs - a.secs) / (b.secs - a.secs)) as f32;
                return a.value + (b.value - a.value) * t;
            }
        }
        self.points[self.points.len() - 1].value
    }

    /// Add a point, keeping the order. Returns its index.
    pub fn insert_point(&mut self, secs: f64, value: f32) -> usize {
        let point = AutomationPoint {
            secs: secs.max(0.0),
            value: self.param.clamp(value),
        };
        let idx = self.points.partition_point(|p| p.secs <= point.secs);
        self.points.insert(idx, point);
        idx
    }

    /// Move a point, never past its neighbours, so dragging one can never
    /// reorder the lane under the pointer.
    pub fn move_point(&mut self, idx: usize, secs: f64, value: f32) {
        if idx >= self.points.len() {
            return;
        }
        let lo = if idx > 0 {
            self.points[idx - 1].secs
        } else {
            0.0
        };
        let hi = self
            .points
            .get(idx + 1)
            .map(|p| p.secs)
            .unwrap_or(f64::INFINITY);
        self.points[idx] = AutomationPoint {
            secs: secs.clamp(lo, hi.max(lo)),
            value: self.param.clamp(value),
        };
    }

    pub fn remove_point(&mut self, idx: usize) {
        if idx < self.points.len() {
            self.points.remove(idx);
        }
    }
}

/// Where a clip's audio comes from: a list row.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClipSource {
    /// The row's path: a file, or a `(virtual)` row's label path.
    pub path: PathBuf,
    /// A virtual row's asset id. Its label path is issued per process, so a
    /// session finds the row again by this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    pub source: ClipSource,
    /// The row's name when the clip was made, for a clip whose row is gone.
    #[serde(default)]
    pub name: String,
    /// Where the clip starts on the timeline.
    pub start_secs: f64,
    /// Where in the source it starts.
    #[serde(default)]
    pub in_secs: f64,
    /// How long it runs.
    pub len_secs: f64,
    /// The whole source's length, which bounds a trim. 0 while unknown.
    #[serde(default)]
    pub source_len_secs: f64,
    #[serde(default)]
    pub fade_in_secs: f64,
    #[serde(default)]
    pub fade_out_secs: f64,
}

impl Clip {
    pub fn end_secs(&self) -> f64 {
        self.start_secs + self.len_secs
    }

    /// Keep both fades inside the clip, fade-in first.
    fn clamp_fades(&mut self) {
        self.fade_in_secs = self.fade_in_secs.clamp(0.0, self.len_secs);
        self.fade_out_secs = self
            .fade_out_secs
            .clamp(0.0, (self.len_secs - self.fade_in_secs).max(0.0));
    }

    /// How far the clip may run from `in_secs`: to the end of the source when
    /// its length is known, otherwise as far as it already does.
    fn max_len_secs(&self) -> f64 {
        if self.source_len_secs > 0.0 {
            (self.source_len_secs - self.in_secs).max(MIN_CLIP_SECS)
        } else {
            f64::INFINITY
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    #[serde(default)]
    pub kind: TrackKind,
    pub name: String,
    #[serde(default)]
    pub volume_db: f32,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub clips: Vec<Clip>,
    #[serde(default)]
    pub lanes: Vec<AutomationLane>,
    #[serde(default = "default_track_height")]
    pub height: f32,
    /// Lanes folded away; their curves are then drawn over the clips.
    #[serde(default)]
    pub lanes_collapsed: bool,
    /// A video track's picture window is open.
    #[serde(default = "default_true")]
    pub show_video: bool,
}

impl Track {
    pub fn lane(&self, param: LaneParam) -> Option<&AutomationLane> {
        self.lanes.iter().find(|lane| lane.param == param)
    }
}

/// Zoom and scroll, kept with the timeline so it reopens where it was left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiEditView {
    pub px_per_sec: f32,
    pub scroll_secs: f64,
    /// How far the track rows are scrolled down, in points.
    #[serde(default)]
    pub scroll_y: f32,
    /// Every row's height times this.
    #[serde(default = "default_track_zoom")]
    pub track_zoom: f32,
}

impl Default for MultiEditView {
    fn default() -> Self {
        Self {
            px_per_sec: DEFAULT_PX_PER_SEC,
            scroll_secs: 0.0,
            scroll_y: 0.0,
            track_zoom: 1.0,
        }
    }
}

/// A named point in time on the whole timeline (not on one track).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineMarker {
    pub id: String,
    pub secs: f64,
    #[serde(default)]
    pub label: String,
}

/// A clip about to be placed: a list row and what is known about it.
#[derive(Clone, Debug, PartialEq)]
pub struct NewClip {
    pub source: ClipSource,
    pub name: String,
    pub len_secs: f64,
    pub is_video: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiEditDoc {
    pub id: String,
    pub name: String,
    /// The rate a mixdown is written at. 0 until the first clip is placed,
    /// which then sets it from that clip's `file_sr`.
    #[serde(default)]
    pub timeline_sr: u32,
    #[serde(default)]
    pub tracks: Vec<Track>,
    /// Whether its tab is open. Closing a tab keeps the timeline.
    #[serde(default)]
    pub open: bool,
    #[serde(default)]
    pub view: MultiEditView,
    /// Sorted by time.
    #[serde(default)]
    pub markers: Vec<TimelineMarker>,
}

impl MultiEditDoc {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            name: name.into(),
            timeline_sr: 0,
            tracks: Vec::new(),
            open: true,
            view: MultiEditView::default(),
            markers: Vec::new(),
        }
    }

    /// Where the last clip ends.
    pub fn end_secs(&self) -> f64 {
        self.tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .map(Clip::end_secs)
            .fold(0.0, f64::max)
    }

    /// (track index, clip index) of a clip.
    pub fn clip_location(&self, clip_id: &str) -> Option<(usize, usize)> {
        self.tracks.iter().enumerate().find_map(|(ti, track)| {
            track
                .clips
                .iter()
                .position(|clip| clip.id == clip_id)
                .map(|ci| (ti, ci))
        })
    }

    pub fn clip(&self, clip_id: &str) -> Option<&Clip> {
        let (ti, ci) = self.clip_location(clip_id)?;
        self.tracks.get(ti)?.clips.get(ci)
    }

    fn clip_mut(&mut self, clip_id: &str) -> Option<&mut Clip> {
        let (ti, ci) = self.clip_location(clip_id)?;
        self.tracks.get_mut(ti)?.clips.get_mut(ci)
    }

    /// A name no other track of the kind has: "Track 03", "Video 01".
    fn next_track_name(&self, kind: TrackKind) -> String {
        let prefix = kind.default_name_prefix();
        (1..)
            .map(|n| format!("{prefix} {n:02}"))
            .find(|name| !self.tracks.iter().any(|track| &track.name == name))
            .unwrap_or_else(|| prefix.to_string())
    }

    /// Add an empty track at the bottom. Returns its index.
    pub fn add_track(&mut self, kind: TrackKind) -> usize {
        let name = self.next_track_name(kind);
        self.tracks.push(Track {
            id: new_id(),
            kind,
            name,
            volume_db: 0.0,
            mute: false,
            solo: false,
            clips: Vec::new(),
            lanes: Vec::new(),
            height: DEFAULT_TRACK_HEIGHT,
            lanes_collapsed: false,
            show_video: true,
        });
        self.tracks.len() - 1
    }

    pub fn remove_track(&mut self, track_id: &str) -> bool {
        let before = self.tracks.len();
        self.tracks.retain(|track| track.id != track_id);
        self.tracks.len() != before
    }

    /// Place clips back to back on a track, the first at `at_secs`. Returns
    /// their ids. Rows of unknown length are skipped: a clip needs a length.
    pub fn insert_clips(&mut self, track_idx: usize, at_secs: f64, clips: Vec<NewClip>) -> Vec<String> {
        let Some(track) = self.tracks.get_mut(track_idx) else {
            return Vec::new();
        };
        let mut cursor = at_secs.max(0.0);
        let mut ids = Vec::new();
        for new in clips {
            if !(new.len_secs.is_finite() && new.len_secs >= MIN_CLIP_SECS) {
                continue;
            }
            let clip = Clip {
                id: new_id(),
                source: new.source,
                name: new.name,
                start_secs: cursor,
                in_secs: 0.0,
                len_secs: new.len_secs,
                source_len_secs: new.len_secs,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
            };
            cursor += clip.len_secs;
            ids.push(clip.id.clone());
            track.clips.push(clip);
        }
        ids
    }

    /// Move a clip to `start_secs`, and onto another track when one is given
    /// and holds the same kind of media.
    pub fn move_clip(&mut self, clip_id: &str, start_secs: f64, to_track: Option<usize>) -> bool {
        let Some((ti, ci)) = self.clip_location(clip_id) else {
            return false;
        };
        let target = to_track.unwrap_or(ti);
        if target >= self.tracks.len() || self.tracks[target].kind != self.tracks[ti].kind {
            return false;
        }
        let mut clip = self.tracks[ti].clips.remove(ci);
        clip.start_secs = start_secs.max(0.0);
        self.tracks[target].clips.push(clip);
        true
    }

    /// Move a clip's left edge to `new_start_secs`, keeping the audio under
    /// it where it was: the source offset moves with the edge.
    pub fn trim_clip_start(&mut self, clip_id: &str, new_start_secs: f64) -> bool {
        let Some(clip) = self.clip_mut(clip_id) else {
            return false;
        };
        let earliest = (clip.start_secs - clip.in_secs).max(0.0);
        let latest = clip.end_secs() - MIN_CLIP_SECS;
        if latest < earliest {
            return false;
        }
        let new_start = new_start_secs.clamp(earliest, latest);
        let delta = new_start - clip.start_secs;
        clip.in_secs = (clip.in_secs + delta).max(0.0);
        clip.len_secs -= delta;
        clip.start_secs = new_start;
        clip.clamp_fades();
        true
    }

    /// Move a clip's right edge to `new_end_secs`, never past the end of its
    /// source.
    pub fn trim_clip_end(&mut self, clip_id: &str, new_end_secs: f64) -> bool {
        let Some(clip) = self.clip_mut(clip_id) else {
            return false;
        };
        let len = (new_end_secs - clip.start_secs).clamp(MIN_CLIP_SECS, clip.max_len_secs());
        clip.len_secs = len;
        clip.clamp_fades();
        true
    }

    pub fn set_fade_in(&mut self, clip_id: &str, secs: f64) -> bool {
        let Some(clip) = self.clip_mut(clip_id) else {
            return false;
        };
        clip.fade_in_secs = secs.clamp(0.0, (clip.len_secs - clip.fade_out_secs).max(0.0));
        true
    }

    pub fn set_fade_out(&mut self, clip_id: &str, secs: f64) -> bool {
        let Some(clip) = self.clip_mut(clip_id) else {
            return false;
        };
        clip.fade_out_secs = secs.clamp(0.0, (clip.len_secs - clip.fade_in_secs).max(0.0));
        true
    }

    /// Cut a clip in two at `at_secs`. The left half keeps the fade-in, the
    /// right half the fade-out. Returns the right half's id.
    pub fn split_clip(&mut self, clip_id: &str, at_secs: f64) -> Option<String> {
        let (ti, ci) = self.clip_location(clip_id)?;
        let clip = &self.tracks[ti].clips[ci];
        if at_secs < clip.start_secs + MIN_CLIP_SECS || at_secs > clip.end_secs() - MIN_CLIP_SECS {
            return None;
        }
        let left_len = at_secs - clip.start_secs;
        let mut right = clip.clone();
        right.id = new_id();
        right.start_secs = at_secs;
        right.in_secs = clip.in_secs + left_len;
        right.len_secs = clip.len_secs - left_len;
        right.fade_in_secs = 0.0;
        right.clamp_fades();
        let left = &mut self.tracks[ti].clips[ci];
        left.len_secs = left_len;
        left.fade_out_secs = 0.0;
        left.clamp_fades();
        let id = right.id.clone();
        self.tracks[ti].clips.insert(ci + 1, right);
        Some(id)
    }

    pub fn remove_clip(&mut self, clip_id: &str) -> bool {
        let Some((ti, ci)) = self.clip_location(clip_id) else {
            return false;
        };
        self.tracks[ti].clips.remove(ci);
        true
    }

    /// A copy of a clip placed right after it. Returns the copy's id.
    pub fn duplicate_clip(&mut self, clip_id: &str) -> Option<String> {
        let (ti, ci) = self.clip_location(clip_id)?;
        let mut copy = self.tracks[ti].clips[ci].clone();
        copy.id = new_id();
        copy.start_secs = self.tracks[ti].clips[ci].end_secs();
        let id = copy.id.clone();
        self.tracks[ti].clips.insert(ci + 1, copy);
        Some(id)
    }

    /// A copy of `clip` on track `track_idx` at `at_secs`, under a new id:
    /// the same source, in-point, length and fades. Returns the copy's id.
    pub fn paste_clip(&mut self, track_idx: usize, clip: &Clip, at_secs: f64) -> Option<String> {
        let track = self.tracks.get_mut(track_idx)?;
        let mut copy = clip.clone();
        copy.id = new_id();
        copy.start_secs = at_secs.max(0.0);
        let id = copy.id.clone();
        track.clips.push(copy);
        Some(id)
    }

    /// The track a pasted clip of `kind` goes on: the selected track, else
    /// the selected clip's, else the one it was copied from, else the first
    /// of its kind -- each only when it holds that kind. `None` when no
    /// track does, and the paste makes one.
    pub fn paste_target(
        &self,
        kind: TrackKind,
        selected_track: Option<&str>,
        selected_clip: Option<&str>,
        copied_from: Option<&str>,
    ) -> Option<usize> {
        let of_kind = |ti: &usize| self.tracks.get(*ti).is_some_and(|t| t.kind == kind);
        let by_id = |id: &str| self.tracks.iter().position(|t| t.id == id);
        selected_track
            .and_then(by_id)
            .filter(of_kind)
            .or_else(|| {
                selected_clip
                    .and_then(|id| self.clip_location(id))
                    .map(|(ti, _)| ti)
                    .filter(of_kind)
            })
            .or_else(|| copied_from.and_then(by_id).filter(of_kind))
            .or_else(|| self.tracks.iter().position(|t| t.kind == kind))
    }

    /// Clip edges a dragged clip may snap to, other than its own.
    pub fn snap_points(&self, except_clip: Option<&str>) -> Vec<f64> {
        let mut points = vec![0.0];
        for clip in self.tracks.iter().flat_map(|track| track.clips.iter()) {
            if Some(clip.id.as_str()) == except_clip {
                continue;
            }
            points.push(clip.start_secs);
            points.push(clip.end_secs());
        }
        points
    }

    /// Add a lane for `param` to a track, or return the one it has.
    pub fn ensure_lane(&mut self, track_idx: usize, param: LaneParam) -> Option<usize> {
        let track = self.tracks.get_mut(track_idx)?;
        if let Some(idx) = track.lanes.iter().position(|lane| lane.param == param) {
            return Some(idx);
        }
        track.lanes.push(AutomationLane::new(param));
        // Lanes keep the order the menu lists them in, whatever order they
        // were added: the eye finds Gain above Pitch every time.
        track
            .lanes
            .sort_by_key(|lane| LaneParam::ALL.iter().position(|p| *p == lane.param));
        track.lanes.iter().position(|lane| lane.param == param)
    }

    pub fn remove_lane(&mut self, track_idx: usize, lane_id: &str) -> bool {
        let Some(track) = self.tracks.get_mut(track_idx) else {
            return false;
        };
        let before = track.lanes.len();
        track.lanes.retain(|lane| lane.id != lane_id);
        track.lanes.len() != before
    }

    /// Whether any track is soloed; if so, only soloed tracks sound.
    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|track| track.solo)
    }

    /// Whether a track is heard in the mix.
    pub fn track_audible(&self, track: &Track) -> bool {
        if track.mute {
            return false;
        }
        !self.any_solo() || track.solo
    }

    /// A marker at `secs`, labelled with the next free "M01", "M02"...
    /// Returns its id.
    pub fn add_marker(&mut self, secs: f64) -> String {
        let label = (1..)
            .map(|n| format!("M{n:02}"))
            .find(|label| !self.markers.iter().any(|m| &m.label == label))
            .unwrap_or_else(|| "M".to_string());
        let marker = TimelineMarker {
            id: new_id(),
            secs: secs.max(0.0),
            label,
        };
        let id = marker.id.clone();
        self.markers.push(marker);
        self.sort_markers();
        id
    }

    pub fn move_marker(&mut self, id: &str, secs: f64) -> bool {
        let Some(marker) = self.markers.iter_mut().find(|m| m.id == id) else {
            return false;
        };
        marker.secs = secs.max(0.0);
        self.sort_markers();
        true
    }

    pub fn rename_marker(&mut self, id: &str, label: &str) -> bool {
        let Some(marker) = self.markers.iter_mut().find(|m| m.id == id) else {
            return false;
        };
        marker.label = label.trim().to_string();
        true
    }

    pub fn remove_marker(&mut self, id: &str) -> bool {
        let before = self.markers.len();
        self.markers.retain(|m| m.id != id);
        self.markers.len() != before
    }

    fn sort_markers(&mut self) {
        self.markers.sort_by(|a, b| a.secs.total_cmp(&b.secs));
    }

    /// Every source a clip refers to, once each, in timeline order. Asked
    /// every frame the timeline is drawn, so deduplicated by hash rather than
    /// by a scan per clip.
    pub fn sources(&self) -> Vec<ClipSource> {
        let mut seen = std::collections::HashSet::new();
        self.tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .filter(|clip| seen.insert(&clip.source))
            .map(|clip| clip.source.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(name: &str) -> ClipSource {
        ClipSource {
            path: PathBuf::from(name),
            asset_id: None,
        }
    }

    fn doc_with_clip(len: f64) -> (MultiEditDoc, String) {
        let mut doc = MultiEditDoc::new("t");
        let track = doc.add_track(TrackKind::Audio);
        let ids = doc.insert_clips(
            track,
            1.0,
            vec![NewClip {
                source: src("a.wav"),
                name: "a.wav".into(),
                len_secs: len,
                is_video: false,
            }],
        );
        (doc, ids[0].clone())
    }

    #[test]
    fn clips_dropped_together_sit_back_to_back() {
        let mut doc = MultiEditDoc::new("t");
        let track = doc.add_track(TrackKind::Audio);
        let ids = doc.insert_clips(
            track,
            2.0,
            vec![
                NewClip {
                    source: src("a.wav"),
                    name: "a".into(),
                    len_secs: 1.5,
                    is_video: false,
                },
                NewClip {
                    source: src("b.wav"),
                    name: "b".into(),
                    len_secs: 0.0,
                    is_video: false,
                },
                NewClip {
                    source: src("c.wav"),
                    name: "c".into(),
                    len_secs: 3.0,
                    is_video: false,
                },
            ],
        );
        assert_eq!(ids.len(), 2, "a row of unknown length is skipped");
        let starts: Vec<f64> = doc.tracks[0].clips.iter().map(|c| c.start_secs).collect();
        assert_eq!(starts, vec![2.0, 3.5]);
        assert_eq!(doc.end_secs(), 6.5);
    }

    #[test]
    fn trimming_the_left_edge_keeps_the_audio_in_place() {
        let (mut doc, id) = doc_with_clip(4.0);
        assert!(doc.trim_clip_start(&id, 2.0));
        let clip = doc.clip(&id).unwrap();
        assert_eq!((clip.start_secs, clip.in_secs, clip.len_secs), (2.0, 1.0, 3.0));
        // Cannot reach before the source's first sample.
        assert!(doc.trim_clip_start(&id, 0.0));
        let clip = doc.clip(&id).unwrap();
        assert_eq!((clip.start_secs, clip.in_secs, clip.len_secs), (1.0, 0.0, 4.0));
    }

    #[test]
    fn trimming_the_right_edge_stops_at_the_end_of_the_source() {
        let (mut doc, id) = doc_with_clip(4.0);
        assert!(doc.trim_clip_end(&id, 3.0));
        assert_eq!(doc.clip(&id).unwrap().len_secs, 2.0);
        assert!(doc.trim_clip_end(&id, 100.0));
        assert_eq!(doc.clip(&id).unwrap().len_secs, 4.0);
        assert!(doc.trim_clip_end(&id, -5.0));
        assert_eq!(doc.clip(&id).unwrap().len_secs, MIN_CLIP_SECS);
    }

    #[test]
    fn fades_never_overlap_or_leave_the_clip() {
        let (mut doc, id) = doc_with_clip(2.0);
        doc.set_fade_in(&id, 1.5);
        doc.set_fade_out(&id, 1.5);
        let clip = doc.clip(&id).unwrap();
        assert_eq!(clip.fade_in_secs, 1.5);
        assert!((clip.fade_out_secs - 0.5).abs() < 1e-9);
        doc.trim_clip_end(&id, 2.0);
        let clip = doc.clip(&id).unwrap();
        assert!(clip.fade_in_secs + clip.fade_out_secs <= clip.len_secs + 1e-9);
    }

    #[test]
    fn a_split_continues_the_source_where_the_left_half_stops() {
        let (mut doc, id) = doc_with_clip(4.0);
        doc.set_fade_in(&id, 0.5);
        doc.set_fade_out(&id, 0.5);
        let right = doc.split_clip(&id, 2.5).expect("split inside the clip");
        let left = doc.clip(&id).unwrap().clone();
        let right = doc.clip(&right).unwrap().clone();
        assert_eq!((left.start_secs, left.len_secs), (1.0, 1.5));
        assert_eq!((right.start_secs, right.in_secs, right.len_secs), (2.5, 1.5, 2.5));
        assert_eq!((left.fade_in_secs, left.fade_out_secs), (0.5, 0.0));
        assert_eq!((right.fade_in_secs, right.fade_out_secs), (0.0, 0.5));
        assert!(doc.split_clip(&id, 1.0).is_none(), "not at the very edge");
    }

    #[test]
    fn a_clip_moves_only_onto_a_track_of_its_kind() {
        let (mut doc, id) = doc_with_clip(1.0);
        let video = doc.add_track(TrackKind::Video);
        let audio = doc.add_track(TrackKind::Audio);
        assert!(!doc.move_clip(&id, 0.0, Some(video)));
        assert!(doc.move_clip(&id, 3.0, Some(audio)));
        assert_eq!(doc.clip_location(&id), Some((audio, 0)));
        assert_eq!(doc.clip(&id).unwrap().start_secs, 3.0);
    }

    #[test]
    fn track_names_count_per_kind_and_ids_are_random() {
        let mut doc = MultiEditDoc::new("t");
        doc.add_track(TrackKind::Audio);
        doc.add_track(TrackKind::Audio);
        doc.add_track(TrackKind::Video);
        let names: Vec<&str> = doc.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["Track 01", "Track 02", "Video 01"]);
        assert_ne!(doc.tracks[0].id, doc.tracks[1].id);
        assert_eq!(doc.tracks[0].id.len(), 32);
    }

    #[test]
    fn lanes_interpolate_and_mute_steps() {
        let mut lane = AutomationLane::new(LaneParam::Gain);
        assert_eq!(lane.value_at(3.0), 0.0, "no points: neutral");
        lane.insert_point(2.0, -6.0);
        lane.insert_point(1.0, 0.0);
        assert_eq!(lane.value_at(0.0), 0.0);
        assert!((lane.value_at(1.5) + 3.0).abs() < 1e-6);
        assert_eq!(lane.value_at(9.0), -6.0);
        let mut mute = AutomationLane::new(LaneParam::Mute);
        mute.insert_point(1.0, 0.0);
        mute.insert_point(2.0, 0.7);
        assert_eq!(mute.value_at(1.9), 0.0, "a step, not a ramp");
        assert_eq!(mute.value_at(2.0), 1.0);
    }

    #[test]
    fn a_dragged_point_stays_between_its_neighbours() {
        let mut lane = AutomationLane::new(LaneParam::Pan);
        lane.insert_point(1.0, 0.0);
        lane.insert_point(2.0, 0.0);
        lane.insert_point(3.0, 0.0);
        lane.move_point(1, 10.0, 5.0);
        assert_eq!(lane.points[1].secs, 3.0);
        assert_eq!(lane.points[1].value, 1.0, "clamped to the lane's range");
    }

    #[test]
    fn the_document_round_trips_through_toml() {
        let (mut doc, id) = doc_with_clip(2.0);
        doc.set_fade_in(&id, 0.25);
        doc.ensure_lane(0, LaneParam::Pitch);
        doc.tracks[0].lanes[0].insert_point(0.5, 12.0);
        doc.timeline_sr = 44_100;
        #[derive(Serialize, Deserialize)]
        struct Wrap {
            multi_edits: Vec<MultiEditDoc>,
        }
        let text = toml::to_string(&Wrap {
            multi_edits: vec![doc.clone()],
        })
        .expect("serialize");
        let back: Wrap = toml::from_str(&text).expect("parse");
        assert_eq!(back.multi_edits, vec![doc]);
    }

    #[test]
    fn snapping_takes_the_nearest_candidate_within_reach() {
        let candidates = [1.0, 2.0, 2.3];
        assert_eq!(snap_secs(2.2, &candidates, 0.25), 2.3);
        assert_eq!(snap_secs(1.9, &candidates, 0.25), 2.0);
        assert_eq!(snap_secs(1.5, &candidates, 0.25), 1.5, "nothing close enough");
        assert_eq!(snap_secs(1.5, &[], 10.0), 1.5);
    }

    #[test]
    fn a_step_stops_at_a_marker_it_would_pass() {
        let markers = [0.4, 1.0, 2.5];
        assert_eq!(stop_before(0.0, 1.0, markers), 0.4);
        assert_eq!(stop_before(0.4, 1.0, markers), 1.0, "the marker it is on is left behind");
        assert_eq!(stop_before(1.0, 2.0, markers), 2.0, "none in the way");
        assert_eq!(stop_before(3.0, 2.0, markers), 2.5, "leftwards too");
        assert_eq!(stop_before(2.5, 2.0, markers), 2.0);
        assert_eq!(stop_before(1.0, 0.0, markers), 0.4);
        assert_eq!(stop_before(1.0, 1.0, markers), 1.0, "no step, no stop");
        assert_eq!(stop_before(0.0, 1.0, []), 1.0);
    }

    #[test]
    fn a_pasted_clip_is_a_copy_under_a_new_id() {
        let (mut doc, id) = doc_with_clip(2.0);
        assert!(doc.set_fade_in(&id, 0.3));
        let original = doc.clip(&id).expect("the clip").clone();
        let pasted = doc.paste_clip(0, &original, 5.0).expect("pasted");
        assert_ne!(pasted, id);
        let copy = doc.clip(&pasted).expect("the copy");
        assert_eq!(copy.start_secs, 5.0);
        assert_eq!(
            (&copy.source, copy.in_secs, copy.len_secs, copy.fade_in_secs),
            (&original.source, original.in_secs, original.len_secs, original.fade_in_secs)
        );
        assert_eq!(doc.tracks[0].clips.len(), 2);
        assert_eq!(doc.clip(&id), Some(&original), "the original is untouched");
        assert!(doc.paste_clip(9, &original, 0.0).is_none(), "no such track");
    }

    #[test]
    fn a_paste_goes_to_a_track_of_its_kind() {
        let mut doc = MultiEditDoc::new("t");
        let a0 = doc.add_track(TrackKind::Audio);
        let v = doc.add_track(TrackKind::Video);
        let a1 = doc.add_track(TrackKind::Audio);
        let ids: Vec<String> = doc.tracks.iter().map(|t| t.id.clone()).collect();
        let (a0_id, v_id, a1_id) = (ids[a0].as_str(), ids[v].as_str(), ids[a1].as_str());
        let clip = doc.insert_clips(
            a1,
            0.0,
            vec![NewClip {
                source: src("a.wav"),
                name: "a.wav".into(),
                len_secs: 1.0,
                is_video: false,
            }],
        )[0]
        .clone();
        let audio = TrackKind::Audio;
        assert_eq!(doc.paste_target(audio, Some(a1_id), None, Some(a0_id)), Some(a1), "the selected track");
        assert_eq!(doc.paste_target(audio, None, Some(&clip), Some(a0_id)), Some(a1), "the selected clip's");
        assert_eq!(
            doc.paste_target(audio, Some(v_id), None, Some(a1_id)),
            Some(a1),
            "a selected track of the other kind is passed over"
        );
        assert_eq!(doc.paste_target(audio, None, None, None), Some(a0), "the first of its kind");
        assert_eq!(doc.paste_target(TrackKind::Video, Some(a0_id), None, Some(a0_id)), Some(v));
        let empty = MultiEditDoc::new("e");
        assert_eq!(empty.paste_target(audio, None, None, None), None);
    }

    #[test]
    fn a_span_snaps_by_whichever_end_is_caught() {
        let candidates = [0.0, 5.0];
        assert_eq!(snap_span(0.1, 2.0, &candidates, 0.25), 0.0, "the start caught");
        assert_eq!(snap_span(2.9, 2.0, &candidates, 0.25), 3.0, "the end caught");
        assert_eq!(snap_span(1.5, 2.0, &candidates, 0.25), 1.5, "neither end near one");
        assert_eq!(snap_span(0.2, 4.75, &candidates, 0.25), 0.25, "both: the nearer wins");
    }

    #[test]
    fn scrolling_stops_with_the_end_mid_view() {
        assert_eq!(clamp_scroll_secs(100.0, 30.0, 20.0), 20.0);
        assert_eq!(clamp_scroll_secs(-5.0, 30.0, 20.0), 0.0);
        assert_eq!(clamp_scroll_secs(5.0, 4.0, 20.0), 0.0, "a short timeline does not scroll");
    }

    #[test]
    fn zooming_out_stops_once_everything_fits() {
        assert_eq!(zoom_out_limit(80.0, 1000.0), 10.0);
        assert_eq!(zoom_out_limit(1.0, 1000.0), 100.0, "at least MIN_FIT_SECS wide");
        assert_eq!(zoom_out_limit(1.0e6, 1000.0), MIN_PX_PER_SEC);
    }

    #[test]
    fn grid_points_cover_the_range_inclusively() {
        assert_eq!(grid_points(0.5, 2.0, 0.5), vec![0.5, 1.0, 1.5, 2.0]);
        assert_eq!(grid_points(0.1, 0.9, 1.0), Vec::<f64>::new());
        assert!(grid_points(0.0, 1.0, 0.0).is_empty());
    }

    #[test]
    fn arrow_steps_land_on_grid_lines() {
        assert_eq!(step_to_grid(1.0, 1, 0.5), 1.5);
        assert_eq!(step_to_grid(1.2, 1, 0.5), 1.5);
        assert_eq!(step_to_grid(1.2, -1, 0.5), 1.0);
        assert_eq!(step_to_grid(1.0, -1, 0.5), 0.5);
        assert_eq!(step_to_grid(0.2, -1, 0.5), 0.0);
        assert_eq!(grid_step_secs(40.0, 70.0), 2.0);
        assert_eq!(grid_step_secs(1000.0, 70.0), 0.1);
    }

    #[test]
    fn markers_stay_ordered_and_take_the_next_free_label() {
        let mut doc = MultiEditDoc::new("t");
        let late = doc.add_marker(5.0);
        let early = doc.add_marker(1.0);
        assert_eq!(doc.markers[0].id, early);
        assert_eq!(
            doc.markers.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(),
            vec!["M02", "M01"]
        );
        doc.move_marker(&late, 0.5);
        assert_eq!(doc.markers[0].id, late);
        doc.rename_marker(&late, " intro ");
        assert_eq!(doc.markers[0].label, "intro");
        assert!(doc.remove_marker(&early));
        assert_eq!(doc.markers.len(), 1);
    }

    #[test]
    fn a_document_from_before_heights_and_markers_still_reads() {
        let text = r#"
[[multi_edits]]
id = "abc"
name = "Old"
open = true
[multi_edits.view]
px_per_sec = 40.0
scroll_secs = 0.0
[[multi_edits.tracks]]
id = "t1"
kind = "video"
name = "Video 01"
[[multi_edits.tracks.lanes]]
id = "l1"
param = "gain"
"#;
        #[derive(Deserialize)]
        struct Wrap {
            multi_edits: Vec<MultiEditDoc>,
        }
        let doc = toml::from_str::<Wrap>(text).expect("parse").multi_edits.remove(0);
        assert!(doc.markers.is_empty());
        assert_eq!(doc.view.track_zoom, 1.0);
        let track = &doc.tracks[0];
        assert_eq!(track.height, DEFAULT_TRACK_HEIGHT);
        assert!(track.show_video && !track.lanes_collapsed);
        assert_eq!(track.lanes[0].height, DEFAULT_LANE_HEIGHT);
    }

    #[test]
    fn solo_silences_the_others() {
        let mut doc = MultiEditDoc::new("t");
        doc.add_track(TrackKind::Audio);
        doc.add_track(TrackKind::Audio);
        assert!(doc.track_audible(&doc.tracks[0]));
        doc.tracks[1].solo = true;
        assert!(!doc.track_audible(&doc.tracks[0]));
        assert!(doc.track_audible(&doc.tracks[1]));
    }
}
