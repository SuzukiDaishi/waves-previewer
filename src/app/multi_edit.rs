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

use crate::audio_channels::{standard_layout_vec, Layout, SpeakerPos};

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

/// Where a track's sound goes in the timeline's output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackOutput {
    /// Its clips as two channels, panned, into the output's front pair.
    #[default]
    Stereo,
    /// Its clips as one channel into output channel `index` alone. No pan:
    /// there is nowhere to pan it to.
    Channel { index: usize },
}

impl TrackOutput {
    pub fn is_mono(self) -> bool {
        matches!(self, TrackOutput::Channel { .. })
    }
}

/// The output of a timeline nobody has changed: a stereo pair.
pub fn stereo_layout() -> Layout {
    standard_layout_vec(2).expect("stereo has a standard layout")
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
    /// Its row's length was not known when it was placed: `len_secs` is 0
    /// and the clip is its start alone until `resolve_len` gives it one.
    #[serde(default, skip_serializing_if = "is_false")]
    pub len_pending: bool,
    /// Placed in one drop right after this clip (by id), while a clip up
    /// that chain was still without a length. When that length arrives the
    /// clip moves along with it, so the drop stays back to back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<String>,
    /// The one source channel it plays (0-based), when it is a channel of a
    /// split clip; `None` plays them all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u16>,
}

fn is_false(value: &bool) -> bool {
    !*value
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
    /// The pan knob, -1 (left) to +1 (right). On an output with three or
    /// more speakers to turn around it is a turn instead, of up to half a
    /// circle each way (`pan_rotates`). A Pan lane moves it from here, as
    /// the Gain lane moves the fader.
    #[serde(default)]
    pub pan: f32,
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
    #[serde(default)]
    pub output: TrackOutput,
}

impl Track {
    pub fn lane(&self, param: LaneParam) -> Option<&AutomationLane> {
        self.lanes.iter().find(|lane| lane.param == param)
    }
}

/// Whether a pan on an output of `layout` turns the sound around the
/// listener (VBAP) rather than balancing left against right -- the shared
/// rule, `panning::PanMode::default_for`. 2.1 therefore balances, like stereo.
pub fn pan_rotates(layout: &[Option<SpeakerPos>]) -> bool {
    crate::panning::PanMode::default_for(layout) == crate::panning::PanMode::Vbap
}

/// Whether a track sent to `output` on `layout` is heard to pan. A stereo
/// track always is. A mono track only when the pan turns -- it then moves
/// round from its own speaker -- and never from the LFE or from a channel
/// no speaker is named for.
pub fn track_pans(layout: &[Option<SpeakerPos>], output: TrackOutput) -> bool {
    match output {
        TrackOutput::Stereo => true,
        TrackOutput::Channel { index } => {
            pan_rotates(layout) && matches!(layout.get(index), Some(Some(pos)) if !pos.is_lfe())
        }
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
    /// `None` while the row's length is not known yet.
    pub len_secs: Option<f64>,
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
    /// The output's speakers, one per mixed channel, as
    /// `channel_layout_ops::layout_to_string` writes them. Empty: stereo.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub layout: String,
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
            layout: String::new(),
        }
    }

    /// The output's speakers, one per channel the mix has.
    pub fn output_layout(&self) -> Layout {
        crate::app::channel_layout_ops::layout_from_string(&self.layout)
            .filter(|layout| !layout.is_empty())
            .unwrap_or_else(stereo_layout)
    }

    /// Change the output's speakers. A mono track follows its speaker to
    /// wherever the new layout has it; one whose speaker it lacks goes back
    /// to stereo. Returns how many did.
    pub fn set_output_layout(&mut self, layout: &[Option<SpeakerPos>]) -> usize {
        let old = self.output_layout();
        let new: Layout = if layout.is_empty() {
            stereo_layout()
        } else {
            layout.to_vec()
        };
        let mut reverted = 0;
        for track in &mut self.tracks {
            let TrackOutput::Channel { index } = track.output else {
                continue;
            };
            let target = match old.get(index).copied().flatten() {
                Some(pos) => new.iter().position(|p| *p == Some(pos)),
                // A channel with no speaker is known by its number alone.
                None => (new.get(index) == Some(&None)).then_some(index),
            };
            match target {
                Some(index) => track.output = TrackOutput::Channel { index },
                None => {
                    track.output = TrackOutput::Stereo;
                    reverted += 1;
                }
            }
        }
        self.layout = if new == stereo_layout() {
            String::new()
        } else {
            crate::app::channel_layout_ops::layout_to_string(&new)
        };
        reverted
    }

    /// Why `split_clip_by_channel` would not split this clip of a source
    /// with `source_channels` channels (0 while it is being read).
    pub fn split_by_channel_refusal(&self, clip_id: &str, source_channels: usize) -> Option<&'static str> {
        let Some((ti, ci)) = self.clip_location(clip_id) else {
            return Some("the clip is gone");
        };
        let clip = &self.tracks[ti].clips[ci];
        if self.tracks[ti].kind == TrackKind::Video {
            return Some("a video track's clip: its picture would be repeated on every new track");
        }
        if clip.channel.is_some() {
            return Some("it already plays one channel");
        }
        if clip.len_pending {
            return Some("its length is not known yet");
        }
        match source_channels {
            0 => Some("its source is still being read"),
            1 => Some("its source is mono"),
            _ => None,
        }
    }

    /// Split a clip into one clip per source channel, each on a track of its
    /// own right below the clip's, sent to the output channel of the same
    /// speaker. `source_layout` names the source's speakers, one per
    /// channel. A stereo output takes the source's layout first: it could
    /// not keep a surround source's channels apart. A channel the output has
    /// no place for plays as a stereo track. The new tracks copy the
    /// original's fader, mute, solo and lanes, and its pan (knob and lane)
    /// where the new track is heard to pan (`track_pans`). Returns the new
    /// clips' ids, in channel order.
    pub fn split_clip_by_channel(&mut self, clip_id: &str, source_layout: &[Option<SpeakerPos>]) -> Vec<String> {
        let channels = source_layout.len();
        if self.split_by_channel_refusal(clip_id, channels).is_some() {
            return Vec::new();
        }
        if self.output_layout().len() <= 2 && channels > 2 {
            self.set_output_layout(source_layout);
        }
        let out = self.output_layout();
        let Some((ti, ci)) = self.clip_location(clip_id) else {
            return Vec::new();
        };
        let original = self.tracks[ti].clips.remove(ci);
        let parent = &self.tracks[ti];
        let mut ids = Vec::with_capacity(channels);
        let mut new_tracks = Vec::with_capacity(channels);
        for (ch, pos) in source_layout.iter().enumerate() {
            let label = match pos {
                Some(pos) => pos.label(source_layout).to_string(),
                None => format!("Ch {}", ch + 1),
            };
            let index = match pos {
                Some(pos) => out.iter().position(|p| *p == Some(*pos)),
                None => (out.get(ch) == Some(&None)).then_some(ch),
            };
            let mut clip = original.clone();
            clip.id = new_id();
            clip.follows = None;
            clip.channel = Some(ch as u16);
            ids.push(clip.id.clone());
            let output = index.map_or(TrackOutput::Stereo, |index| TrackOutput::Channel { index });
            let pans = track_pans(&out, output);
            new_tracks.push(Track {
                id: new_id(),
                kind: TrackKind::Audio,
                name: format!("{} \u{b7} {label}", parent.name),
                volume_db: parent.volume_db,
                pan: if pans { parent.pan } else { 0.0 },
                mute: parent.mute,
                solo: parent.solo,
                clips: vec![clip],
                lanes: parent
                    .lanes
                    .iter()
                    .filter(|lane| pans || lane.param != LaneParam::Pan)
                    .map(|lane| AutomationLane {
                        id: new_id(),
                        ..lane.clone()
                    })
                    .collect(),
                height: parent.height,
                lanes_collapsed: parent.lanes_collapsed,
                show_video: true,
                output,
            });
        }
        // Nothing may wait on a clip that is gone.
        for clip in self.tracks.iter_mut().flat_map(|track| track.clips.iter_mut()) {
            if clip.follows.as_deref() == Some(original.id.as_str()) {
                clip.follows = None;
            }
        }
        self.tracks.splice(ti + 1..ti + 1, new_tracks);
        ids
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
            pan: 0.0,
            mute: false,
            solo: false,
            clips: Vec::new(),
            lanes: Vec::new(),
            height: DEFAULT_TRACK_HEIGHT,
            lanes_collapsed: false,
            show_video: true,
            output: TrackOutput::Stereo,
        });
        self.tracks.len() - 1
    }

    pub fn remove_track(&mut self, track_id: &str) -> bool {
        let before = self.tracks.len();
        self.tracks.retain(|track| track.id != track_id);
        self.tracks.len() != before
    }

    /// Place clips back to back on a track, the first at `at_secs`. Returns
    /// their ids.
    ///
    /// A row whose length is not known yet is placed as its start alone
    /// (`len_pending`), taking no room; the rows after it follow it
    /// (`follows`) and move along when `resolve_len` gives it a length. A
    /// row known to be too short to be a clip is skipped.
    pub fn insert_clips(&mut self, track_idx: usize, at_secs: f64, clips: Vec<NewClip>) -> Vec<String> {
        let Some(track) = self.tracks.get_mut(track_idx) else {
            return Vec::new();
        };
        let mut cursor = at_secs.max(0.0);
        let mut ids: Vec<String> = Vec::new();
        let mut pending_before = false;
        for new in clips {
            let len = match new.len_secs {
                Some(len) if len.is_finite() && len >= MIN_CLIP_SECS => len,
                Some(_) => continue,
                None => 0.0,
            };
            let clip = Clip {
                id: new_id(),
                source: new.source,
                name: new.name,
                start_secs: cursor,
                in_secs: 0.0,
                len_secs: len,
                source_len_secs: len,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
                len_pending: new.len_secs.is_none(),
                follows: if pending_before { ids.last().cloned() } else { None },
                channel: None,
            };
            pending_before |= clip.len_pending;
            cursor += clip.len_secs;
            ids.push(clip.id.clone());
            track.clips.push(clip);
        }
        ids
    }

    /// Whether any clip is still waiting for its length.
    pub fn has_pending(&self) -> bool {
        self.tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .any(|clip| clip.len_pending)
    }

    /// Give a clip placed without a length its length, and move the clips
    /// placed right after it in the same drop -- those that still sit there
    /// -- along by as much, so the drop stays back to back. A clip the user
    /// has moved since no longer sits at the old end and stays put. Returns
    /// whether the clip was waiting for a length.
    pub fn resolve_len(&mut self, clip_id: &str, len_secs: f64) -> bool {
        if !(len_secs.is_finite() && len_secs > 0.0) {
            return false;
        }
        let len = len_secs.max(MIN_CLIP_SECS);
        let Some(clip) = self.clip_mut(clip_id).filter(|clip| clip.len_pending) else {
            return false;
        };
        let old_end = clip.end_secs();
        clip.len_pending = false;
        clip.len_secs = len;
        clip.source_len_secs = len;
        let delta = len - (old_end - clip.start_secs);
        // Followers, and theirs: each one that still starts where its
        // leader used to end moves by the same amount.
        let mut leaders = vec![(clip_id.to_string(), old_end)];
        while let Some((leader, end)) = leaders.pop() {
            for clip in self.tracks.iter_mut().flat_map(|track| track.clips.iter_mut()) {
                if clip.follows.as_deref() == Some(leader.as_str())
                    && (clip.start_secs - end).abs() < 1e-6
                {
                    let old_end = clip.end_secs();
                    clip.start_secs += delta;
                    leaders.push((clip.id.clone(), old_end));
                }
            }
        }
        self.prune_follows();
        true
    }

    /// Drop each `follows` link with no clip waiting for a length up its
    /// chain: nothing is left to move it.
    fn prune_follows(&mut self) {
        let links: std::collections::HashMap<String, (bool, Option<String>)> = self
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .map(|clip| (clip.id.clone(), (clip.len_pending, clip.follows.clone())))
            .collect();
        let pending_upstream = |start: &str| {
            let mut at = Some(start.to_string());
            // Bounded: a cycle (from a hand-edited session) cannot loop.
            for _ in 0..=links.len() {
                let Some(id) = at else {
                    return false;
                };
                match links.get(&id) {
                    Some((true, _)) => return true,
                    Some((false, next)) => at = next.clone(),
                    None => return false,
                }
            }
            false
        };
        for clip in self.tracks.iter_mut().flat_map(|track| track.clips.iter_mut()) {
            if clip.follows.as_deref().is_some_and(|leader| !pending_upstream(leader)) {
                clip.follows = None;
            }
        }
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
        let Some(clip) = self.clip_mut(clip_id).filter(|clip| !clip.len_pending) else {
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
        let Some(clip) = self.clip_mut(clip_id).filter(|clip| !clip.len_pending) else {
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

    /// The clips on `tracks` that meet the time span `t0`..`t1` (either way
    /// round) -- one still waiting for a length by its start. Track by
    /// track, in each track's order.
    pub fn clips_in_range(&self, tracks: &[usize], t0: f64, t1: f64) -> Vec<String> {
        let (t0, t1) = (t0.min(t1), t0.max(t1));
        tracks
            .iter()
            .filter_map(|&ti| self.tracks.get(ti))
            .flat_map(|track| track.clips.iter())
            .filter(|clip| {
                if clip.len_pending {
                    clip.start_secs >= t0 && clip.start_secs <= t1
                } else {
                    clip.start_secs < t1 && clip.end_secs() > t0
                }
            })
            .map(|clip| clip.id.clone())
            .collect()
    }

    /// Move clips together from where they were -- `origins` holds each
    /// one's id, start and track then -- by `delta_secs` and `delta_tracks`
    /// rows. The group stops at 0: no clip goes before it. It changes tracks
    /// only when every clip lands on a track of its own kind, and otherwise
    /// moves in time alone. Measured from the origins, so a drag can call it
    /// every frame and land where the pointer says.
    pub fn move_group(&mut self, origins: &[(String, f64, usize)], delta_secs: f64, delta_tracks: i32) {
        let Some(earliest) = origins.iter().map(|o| o.1).reduce(f64::min) else {
            return;
        };
        let delta = delta_secs.max(-earliest);
        let lands = |ti: usize| {
            let to = ti as i64 + delta_tracks as i64;
            to >= 0
                && (to as usize) < self.tracks.len()
                && self.tracks.get(ti).is_some_and(|from| from.kind == self.tracks[to as usize].kind)
        };
        let across = delta_tracks != 0 && origins.iter().all(|(_, _, ti)| lands(*ti));
        for (id, start, ti) in origins {
            let Some((now, ci)) = self.clip_location(id) else {
                continue;
            };
            let to = if across {
                (*ti as i64 + delta_tracks as i64) as usize
            } else if *ti < self.tracks.len() {
                *ti
            } else {
                now
            };
            let mut clip = self.tracks[now].clips.remove(ci);
            clip.start_secs = (start + delta).max(0.0);
            self.tracks[to].clips.push(clip);
        }
    }

    /// Remove these clips. Returns how many went.
    pub fn remove_clips(&mut self, ids: &[String]) -> usize {
        ids.iter().filter(|id| self.remove_clip(id)).count()
    }

    /// Copies of these clips laid right after the group -- moved by its whole
    /// span, each on its own track. Returns the copies' ids.
    pub fn duplicate_clips(&mut self, ids: &[String]) -> Vec<String> {
        let group: Vec<(usize, Clip)> = ids
            .iter()
            .filter_map(|id| {
                let (ti, ci) = self.clip_location(id)?;
                Some((ti, self.tracks[ti].clips[ci].clone()))
            })
            .collect();
        let Some(first) = group.iter().map(|(_, clip)| clip.start_secs).reduce(f64::min) else {
            return Vec::new();
        };
        let span = group.iter().map(|(_, clip)| clip.end_secs()).fold(first, f64::max) - first;
        group
            .into_iter()
            .filter_map(|(ti, clip)| self.paste_clip(ti, &clip, clip.start_secs + span))
            .collect()
    }

    /// Split each of these clips that `at_secs` falls inside. Returns the
    /// right halves' ids.
    pub fn split_clips_at(&mut self, ids: &[String], at_secs: f64) -> Vec<String> {
        let cut: Vec<String> = ids.iter().filter(|id| self.can_split(id, at_secs)).cloned().collect();
        cut.iter().filter_map(|id| self.split_clip(id, at_secs)).collect()
    }

    /// Whether `split_clip` would cut this clip at `at_secs`: far enough
    /// from both ends, and the clip has a length.
    pub fn can_split(&self, clip_id: &str, at_secs: f64) -> bool {
        self.clip(clip_id).is_some_and(|clip| {
            !clip.len_pending
                && at_secs >= clip.start_secs + MIN_CLIP_SECS
                && at_secs <= clip.end_secs() - MIN_CLIP_SECS
        })
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

    /// A copy of `clip` on track `track_idx` at `at_secs`, under a new id:
    /// the same source, in-point, length and fades. Returns the copy's id.
    pub fn paste_clip(&mut self, track_idx: usize, clip: &Clip, at_secs: f64) -> Option<String> {
        let track = self.tracks.get_mut(track_idx)?;
        let mut copy = clip.clone();
        copy.id = new_id();
        copy.follows = None;
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
                len_secs: Some(len),
                is_video: false,
            }],
        );
        (doc, ids[0].clone())
    }

    fn layout(text: &str) -> Layout {
        crate::app::channel_layout_ops::layout_from_string(text).expect("a layout")
    }

    #[test]
    fn a_surround_clip_splits_onto_one_track_per_speaker() {
        let (mut doc, id) = doc_with_clip(4.0);
        doc.tracks[0].name = "Music".into();
        doc.tracks[0].volume_db = -3.0;
        doc.tracks[0].pan = 0.25;
        doc.ensure_lane(0, LaneParam::Gain);
        doc.ensure_lane(0, LaneParam::Pan);
        doc.set_fade_in(&id, 0.5);
        let five_one = layout("FL,FR,FC,LFE,BL,BR");
        let ids = doc.split_clip_by_channel(&id, &five_one);
        assert_eq!(ids.len(), 6);
        // A stereo timeline cannot hold them apart: it takes 5.1.
        assert_eq!(doc.output_layout(), five_one);
        assert_eq!(doc.tracks.len(), 7);
        assert!(doc.tracks[0].clips.is_empty(), "the original clip is replaced");
        for (ch, track) in doc.tracks[1..].iter().enumerate() {
            assert_eq!(track.output, TrackOutput::Channel { index: ch });
            assert_eq!(track.volume_db, -3.0);
            // 5.1 turns a pan: every speaker keeps it but the LFE.
            let lfe = ch == 3;
            assert_eq!(
                track.lanes.iter().map(|lane| lane.param).collect::<Vec<_>>(),
                if lfe {
                    vec![LaneParam::Gain]
                } else {
                    vec![LaneParam::Gain, LaneParam::Pan]
                },
                "channel {ch}"
            );
            assert_eq!(track.pan, if lfe { 0.0 } else { 0.25 }, "channel {ch}");
            assert_ne!(track.lanes[0].id, doc.tracks[0].lanes[0].id);
            let clip = &track.clips[0];
            assert_eq!(clip.channel, Some(ch as u16));
            assert_eq!((clip.start_secs, clip.len_secs, clip.fade_in_secs), (1.0, 4.0, 0.5));
        }
        assert_eq!(doc.tracks[1].name, "Music \u{b7} L");
        assert_eq!(doc.tracks[4].name, "Music \u{b7} LFE");
        // A split clip is one channel already.
        assert!(doc.split_clip_by_channel(&ids[0], &five_one).is_empty());
    }

    #[test]
    fn a_split_lands_on_the_speakers_the_output_has() {
        let (mut doc, id) = doc_with_clip(2.0);
        doc.set_output_layout(&layout("FL,FR,FC,LFE,BL,BR,SL,SR"));
        // Film order onto a WAV-order 7.1: by speaker, not by number.
        doc.split_clip_by_channel(&id, &layout("FL,FC,FR,BL,BR,LFE"));
        let outputs: Vec<TrackOutput> = doc.tracks[1..].iter().map(|t| t.output).collect();
        assert_eq!(
            outputs,
            [0, 2, 1, 4, 5, 3].map(|index| TrackOutput::Channel { index }).to_vec()
        );
        // A speaker the output lacks plays as stereo.
        let (mut doc, id) = doc_with_clip(2.0);
        doc.set_output_layout(&layout("FL,FR,FC,LFE,BL,BR"));
        doc.split_clip_by_channel(&id, &layout("FL,FR,FC,LFE,BL,BR,SL,SR"));
        assert_eq!(doc.tracks[7].output, TrackOutput::Stereo);
    }

    #[test]
    fn some_clips_are_not_split() {
        let (doc, id) = doc_with_clip(2.0);
        assert_eq!(doc.split_by_channel_refusal(&id, 1), Some("its source is mono"));
        assert_eq!(doc.split_by_channel_refusal(&id, 0), Some("its source is still being read"));
        assert_eq!(doc.split_by_channel_refusal(&id, 2), None);
        let mut doc = MultiEditDoc::new("t");
        let video = doc.add_track(TrackKind::Video);
        let ids = doc.insert_clips(
            video,
            0.0,
            vec![NewClip {
                source: src("v.mp4"),
                name: "v".into(),
                len_secs: Some(2.0),
                is_video: true,
            }],
        );
        assert!(doc.split_by_channel_refusal(&ids[0], 6).is_some());
        assert!(doc.split_clip_by_channel(&ids[0], &layout("FL,FR,FC,LFE,BL,BR")).is_empty());
        assert_eq!(doc.tracks.len(), 1);
    }

    #[test]
    fn a_mono_track_follows_its_speaker_when_the_output_changes() {
        let mut doc = MultiEditDoc::new("t");
        doc.set_output_layout(&layout("FL,FR,FC,LFE,BL,BR"));
        let c = doc.add_track(TrackKind::Audio);
        doc.tracks[c].output = TrackOutput::Channel { index: 2 };
        let lfe = doc.add_track(TrackKind::Audio);
        doc.tracks[lfe].output = TrackOutput::Channel { index: 3 };
        // Film order moves the centre to 1; 5.0 has no LFE.
        let reverted = doc.set_output_layout(&layout("FL,FC,FR,BL,BR"));
        assert_eq!(reverted, 1);
        assert_eq!(doc.tracks[c].output, TrackOutput::Channel { index: 1 });
        assert_eq!(doc.tracks[lfe].output, TrackOutput::Stereo);
        // Back to stereo stores nothing.
        doc.set_output_layout(&stereo_layout());
        assert!(doc.layout.is_empty());
        assert_eq!(doc.output_layout().len(), 2);
    }

    #[test]
    fn a_timeline_from_before_outputs_reads_as_stereo_and_round_trips() {
        let (mut doc, id) = doc_with_clip(2.0);
        let mut json: serde_json::Value = serde_json::to_value(&doc).expect("serialize");
        // What an older session holds: no layout, no output, no channel.
        assert!(json.get("layout").is_none());
        assert!(json["tracks"][0]["clips"][0].get("channel").is_none());
        json["tracks"][0].as_object_mut().unwrap().remove("output");
        let old: MultiEditDoc = serde_json::from_value(json).expect("deserialize");
        assert_eq!(old.output_layout(), stereo_layout());
        assert_eq!(old.tracks[0].output, TrackOutput::Stereo);
        doc.split_clip_by_channel(&id, &layout("FL,FR,FC,LFE,BL,BR"));
        let text = serde_json::to_string(&doc).expect("serialize");
        let back: MultiEditDoc = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, doc);
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
                    len_secs: Some(1.5),
                    is_video: false,
                },
                NewClip {
                    source: src("b.wav"),
                    name: "b".into(),
                    len_secs: Some(0.0),
                    is_video: false,
                },
                NewClip {
                    source: src("c.wav"),
                    name: "c".into(),
                    len_secs: Some(3.0),
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
                len_secs: Some(1.0),
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

    fn row(name: &str, len: Option<f64>) -> NewClip {
        NewClip {
            source: src(name),
            name: name.into(),
            len_secs: len,
            is_video: false,
        }
    }

    #[test]
    fn a_row_without_a_length_is_placed_as_its_start_and_its_drop_closes_up_when_it_arrives() {
        let mut doc = MultiEditDoc::new("t");
        let t = doc.add_track(TrackKind::Audio);
        let ids = doc.insert_clips(
            t,
            1.0,
            vec![
                row("a", Some(2.0)),
                row("b", None),
                row("c", Some(1.0)),
                row("d", None),
                row("e", Some(0.5)),
            ],
        );
        assert_eq!(ids.len(), 5, "every row is placed");
        let start = |doc: &MultiEditDoc, i: usize| doc.clip(&ids[i]).expect("clip").start_secs;
        let pending = |doc: &MultiEditDoc, i: usize| doc.clip(&ids[i]).expect("clip").len_pending;
        // a 1-3, b at 3 taking no room, c 3-4, d at 4, e 4-4.5.
        assert_eq!((0..5).map(|i| start(&doc, i)).collect::<Vec<_>>(), vec![1.0, 3.0, 3.0, 4.0, 4.0]);
        assert!(pending(&doc, 1) && pending(&doc, 3) && !pending(&doc, 0));
        assert!(doc.has_pending());

        // b is 1.5 s: c, d and e move along.
        assert!(doc.resolve_len(&ids[1], 1.5));
        assert_eq!(doc.clip(&ids[1]).expect("b").len_secs, 1.5);
        assert_eq!((start(&doc, 2), start(&doc, 3), start(&doc, 4)), (4.5, 5.5, 5.5));
        // d is 2 s: e moves along.
        assert!(doc.resolve_len(&ids[3], 2.0));
        assert_eq!(start(&doc, 4), 7.5);
        assert!(!doc.has_pending());
        assert!(
            doc.tracks[t].clips.iter().all(|clip| clip.follows.is_none()),
            "nothing is left to follow"
        );
        assert!(!doc.resolve_len(&ids[3], 9.0), "a length arrives once");
    }

    #[test]
    fn a_clip_moved_before_the_length_arrives_stays_where_it_was_put() {
        let mut doc = MultiEditDoc::new("t");
        let t = doc.add_track(TrackKind::Audio);
        let ids = doc.insert_clips(t, 0.0, vec![row("a", None), row("b", Some(1.0))]);
        assert!(doc.move_clip(&ids[1], 5.0, None));
        assert!(doc.resolve_len(&ids[0], 2.0));
        assert_eq!(doc.clip(&ids[1]).expect("b").start_secs, 5.0);
        assert!(!doc.trim_clip_end(&ids[0], 9.0) || doc.clip(&ids[0]).expect("a").len_secs > 0.0);
    }

    #[test]
    fn a_clip_without_a_length_cannot_be_trimmed_or_split() {
        let mut doc = MultiEditDoc::new("t");
        let t = doc.add_track(TrackKind::Audio);
        let id = doc.insert_clips(t, 1.0, vec![row("a", None)])[0].clone();
        assert!(!doc.trim_clip_end(&id, 3.0));
        assert!(!doc.trim_clip_start(&id, 0.5));
        assert!(!doc.can_split(&id, 1.0));
        assert!(doc.split_clip(&id, 1.0).is_none());
        // A row known to be too short is still skipped.
        assert!(doc.insert_clips(t, 0.0, vec![row("z", Some(0.0))]).is_empty());
    }

    /// A timeline of audio tracks with clips at (track, start, length), and
    /// the clips' ids in the order given.
    fn timeline(tracks: usize, clips: &[(usize, f64, f64)]) -> (MultiEditDoc, Vec<String>) {
        let mut doc = MultiEditDoc::new("t");
        for _ in 0..tracks {
            doc.add_track(TrackKind::Audio);
        }
        let ids = clips
            .iter()
            .map(|&(ti, start, len)| doc.insert_clips(ti, start, vec![row("c", Some(len))])[0].clone())
            .collect();
        (doc, ids)
    }

    #[test]
    fn clips_in_a_range_are_those_that_meet_it_on_its_tracks() {
        let (mut doc, ids) = timeline(3, &[(0, 0.0, 1.0), (0, 2.0, 1.0), (1, 1.5, 1.0), (2, 1.0, 1.0)]);
        let pending = doc.insert_clips(1, 2.8, vec![row("p", None)])[0].clone();
        let got = doc.clips_in_range(&[0, 1], 2.9, 0.5);
        assert_eq!(
            got,
            vec![ids[0].clone(), ids[1].clone(), ids[2].clone(), pending.clone()],
            "either way round"
        );
        assert!(!got.contains(&ids[3]), "a track outside the range");
        assert_eq!(doc.clips_in_range(&[1], 2.4, 2.9), vec![ids[2].clone(), pending.clone()]);
        assert_eq!(doc.clips_in_range(&[1], 2.7, 2.9), vec![pending], "a start alone counts");
        assert!(doc.clips_in_range(&[0], 1.0, 2.0).is_empty(), "touching edges do not meet");
        doc.tracks.clear();
        assert!(doc.clips_in_range(&[0, 5], 0.0, 9.0).is_empty());
    }

    #[test]
    fn a_group_moves_together_from_where_it_started() {
        let (mut doc, ids) = timeline(3, &[(0, 1.0, 1.0), (1, 3.0, 1.0)]);
        let origins: Vec<(String, f64, usize)> = vec![(ids[0].clone(), 1.0, 0), (ids[1].clone(), 3.0, 1)];
        let at = |doc: &MultiEditDoc, i: usize| {
            let (ti, ci) = doc.clip_location(&ids[i]).expect("clip");
            (ti, doc.tracks[ti].clips[ci].start_secs)
        };
        doc.move_group(&origins, 0.5, 1);
        assert_eq!((at(&doc, 0), at(&doc, 1)), ((1, 1.5), (2, 3.5)));
        // Called again from the same origins: from there, not from here.
        doc.move_group(&origins, 2.0, 0);
        assert_eq!((at(&doc, 0), at(&doc, 1)), ((0, 3.0), (1, 5.0)));
        // Not before 0: the whole group stops where its first clip meets it.
        doc.move_group(&origins, -5.0, 0);
        assert_eq!((at(&doc, 0), at(&doc, 1)), ((0, 0.0), (1, 2.0)));
        // Down two rows would take the second clip off the last track.
        doc.move_group(&origins, 0.0, 2);
        assert_eq!((at(&doc, 0), at(&doc, 1)), ((0, 1.0), (1, 3.0)), "time only");
    }

    #[test]
    fn a_group_does_not_cross_onto_a_track_of_the_other_kind() {
        let (mut doc, ids) = timeline(1, &[(0, 1.0, 1.0)]);
        doc.add_track(TrackKind::Video);
        doc.move_group(&[(ids[0].clone(), 1.0, 0)], 0.0, 1);
        assert_eq!(doc.clip_location(&ids[0]).map(|(ti, _)| ti), Some(0));
    }

    #[test]
    fn a_group_duplicates_after_itself_and_splits_where_the_time_falls() {
        let (mut doc, ids) = timeline(2, &[(0, 1.0, 1.0), (1, 2.5, 1.0), (0, 6.0, 1.0)]);
        let group = vec![ids[0].clone(), ids[1].clone()];
        let copies = doc.duplicate_clips(&group);
        assert_eq!(copies.len(), 2);
        let start_of = |doc: &MultiEditDoc, id: &str| doc.clip(id).expect("clip").start_secs;
        // The group spans 1 - 3.5: its copies come 2.5 s later.
        assert_eq!((start_of(&doc, &copies[0]), start_of(&doc, &copies[1])), (3.5, 5.0));
        assert_eq!(doc.clip_location(&copies[1]).map(|(ti, _)| ti), Some(1));

        let halves = doc.split_clips_at(&[ids[0].clone(), ids[1].clone(), ids[2].clone()], 1.5);
        assert_eq!(halves.len(), 1, "only the clip 1.5 s falls inside");
        assert_eq!(doc.clip(&ids[0]).expect("left half").len_secs, 0.5);
        assert_eq!(doc.remove_clips(&[ids[1].clone(), copies[0].clone(), "gone".into()]), 2);
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
