//! Turning a Multi Edits timeline into samples.
//!
//! Pure and thread-agnostic: every function here runs on a worker, is handed
//! everything it needs, and touches neither the app nor the disk.
//!
//! One track at a time, in the order the spec gives
//! (`docs/MULTI_EDITS_SPEC.md`): lay the clips down with their trims and
//! fades, then Pitch, then Gain with the fader, then Mute, and add the
//! result to the master through the track's pan. Pitch is the one stage that
//! needs the whole track at once (the shifter carries state across the
//! timeline), which is why a track is rendered whole rather than in blocks.
//!
//! The master has one channel per speaker of the timeline's output layout
//! (stereo unless changed). A stereo track renders two channels and lands on
//! the output's front pair; a mono track renders one and lands on its own
//! output channel alone. The pan balances a stereo track left against right
//! -- unless the output has three or more speakers to turn around
//! (`multi_edit::pan_rotates`), where it turns the track round the listener
//! instead, by VBAP (`crate::panning`, the rules every panner in the app
//! shares: 3-D over the speakers' triangles when the output has height
//! speakers). The LFE is never part of that turn.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::multi_edit::{
    pan_rotates, track_pans, AutomationLane, Clip, LaneParam, MultiEditDoc, Track, TrackOutput,
};
use crate::audio_channels::SpeakerPos;
use crate::panning::{dir_to_vec, pan_gains, Rotation, Speakers, PAN_TURN_DEG};

/// The channels a stereo track renders; a mono track renders one.
const STEREO: usize = 2;

/// How long a Mute step takes to fade, so switching a track off does not
/// click: short enough to read as a cut.
const MUTE_RAMP_SECS: f64 = 0.005;

/// A clip dropped wholly inside another on the same track fades in and out
/// over this at each of its ends while the outer clip dips out under it.
pub const CONTAINED_XFADE_SECS: f64 = 0.01;

/// One stretch of a clip's crossfade with its neighbours on the track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum XfadeShape {
    /// Rising along a quarter sine: 0 to 1.
    In,
    /// Falling along a quarter cosine: 1 to 0.
    Out,
    /// Silent: under a clip laid wholly over it.
    Zero,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XfadeSeg {
    pub start: f64,
    pub end: f64,
    pub shape: XfadeShape,
}

/// Where clips on one track overlap, each fades against the other at equal
/// power (sin / cos), so the sum keeps its loudness through the join.
/// Returned per clip, in the order given.
///
/// A clip overlapping the end of an earlier one crossfades over the overlap.
/// A clip lying wholly inside another fades in and out over
/// `CONTAINED_XFADE_SECS` at its ends, and the outer clip is silent between.
pub fn crossfade_segments(clips: &[Clip]) -> Vec<Vec<XfadeSeg>> {
    let mut out = vec![Vec::new(); clips.len()];
    // A clip still waiting for its length is not heard, and crosses nothing.
    let mut order: Vec<usize> = (0..clips.len()).filter(|&i| !clips[i].len_pending).collect();
    order.sort_by(|&a, &b| {
        clips[a]
            .start_secs
            .total_cmp(&clips[b].start_secs)
            .then(clips[a].end_secs().total_cmp(&clips[b].end_secs()))
    });
    for (pos, &ai) in order.iter().enumerate() {
        for &bi in &order[pos + 1..] {
            let (a, b) = (&clips[ai], &clips[bi]);
            if b.start_secs >= a.end_secs() {
                continue;
            }
            let seg = |start: f64, end: f64, shape| XfadeSeg { start, end, shape };
            if b.end_secs() >= a.end_secs() {
                let (s, e) = (b.start_secs, a.end_secs());
                out[ai].push(seg(s, e, XfadeShape::Out));
                out[bi].push(seg(s, e, XfadeShape::In));
            } else {
                let x = CONTAINED_XFADE_SECS.min(b.len_secs * 0.5);
                let (s, e) = (b.start_secs, b.end_secs());
                out[ai].push(seg(s, s + x, XfadeShape::Out));
                out[ai].push(seg(s + x, e - x, XfadeShape::Zero));
                out[ai].push(seg(e - x, e, XfadeShape::In));
                out[bi].push(seg(s, s + x, XfadeShape::In));
                out[bi].push(seg(e - x, e, XfadeShape::Out));
            }
        }
    }
    out
}

/// A clip's crossfade gain at timeline second `t`: 1 outside every segment.
pub fn xfade_gain(segs: &[XfadeSeg], t: f64) -> f32 {
    let mut gain = 1.0f64;
    for seg in segs {
        if t < seg.start || t >= seg.end {
            continue;
        }
        let x = if seg.end > seg.start {
            ((t - seg.start) / (seg.end - seg.start)).clamp(0.0, 1.0)
        } else {
            1.0
        };
        gain *= match seg.shape {
            XfadeShape::In => (x * std::f64::consts::FRAC_PI_2).sin(),
            XfadeShape::Out => (x * std::f64::consts::FRAC_PI_2).cos(),
            XfadeShape::Zero => 0.0,
        };
    }
    gain as f32
}

/// Per clip, in the order given: the spans where it lies over a clip drawn
/// before it. Clips are drawn in order of start (then end), so a clip that
/// starts later lies over the one it overlaps -- these are where it is drawn
/// see-through, and where it draws the crossfade. Merged and sorted.
pub fn covered_spans(clips: &[Clip]) -> Vec<Vec<(f64, f64)>> {
    let mut out = vec![Vec::new(); clips.len()];
    let order = draw_order(clips);
    for (pos, &bi) in order.iter().enumerate() {
        let b = &clips[bi];
        let mut spans: Vec<(f64, f64)> = order[..pos]
            .iter()
            .map(|&ai| &clips[ai])
            .filter_map(|a| {
                let (s, e) = (a.start_secs.max(b.start_secs), a.end_secs().min(b.end_secs()));
                (e > s).then_some((s, e))
            })
            .collect();
        spans.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (s, e) in spans {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        out[bi] = merged;
    }
    out
}

/// The order clips on a track are drawn in, back to front: by start, then
/// end, then as given. Clips waiting for a length are left out.
pub fn draw_order(clips: &[Clip]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..clips.len()).filter(|&i| !clips[i].len_pending).collect();
    order.sort_by(|&a, &b| {
        clips[a]
            .start_secs
            .total_cmp(&clips[b].start_secs)
            .then(clips[a].end_secs().total_cmp(&clips[b].end_secs()))
            .then(a.cmp(&b))
    });
    order
}

/// A clip's source, decoded, at the rate the render runs at.
#[derive(Debug)]
pub struct SourceAudio {
    pub channels: Arc<Vec<Vec<f32>>>,
    pub sample_rate: u32,
}

impl SourceAudio {
    pub fn frames(&self) -> usize {
        self.channels.first().map(Vec::len).unwrap_or(0)
    }
}

/// Sources by the list path a clip names.
pub type SourceMap = HashMap<PathBuf, Arc<SourceAudio>>;

fn secs_to_frames(secs: f64, sr: u32) -> usize {
    if !secs.is_finite() || secs <= 0.0 {
        return 0;
    }
    (secs * sr as f64).round() as usize
}

/// One frame of a source as stereo. Mono goes to both sides; more than two
/// channels fold as even-numbered to the left and odd to the right, each
/// side averaged so a folded surround file is not louder than its stems.
fn stereo_frame(channels: &[Vec<f32>], frame: usize) -> (f32, f32) {
    match channels.len() {
        0 => (0.0, 0.0),
        1 => {
            let v = channels[0].get(frame).copied().unwrap_or(0.0);
            (v, v)
        }
        2 => (
            channels[0].get(frame).copied().unwrap_or(0.0),
            channels[1].get(frame).copied().unwrap_or(0.0),
        ),
        n => {
            let (mut l, mut r, mut nl, mut nr) = (0.0f32, 0.0f32, 0u32, 0u32);
            for (idx, ch) in channels.iter().enumerate() {
                let v = ch.get(frame).copied().unwrap_or(0.0);
                if idx % 2 == 0 {
                    l += v;
                    nl += 1;
                } else {
                    r += v;
                    nr += 1;
                }
            }
            let _ = n;
            (l / nl.max(1) as f32, r / nr.max(1) as f32)
        }
    }
}

/// One frame of a clip's source the way its track takes it: the clip's own
/// channel when it has one (on both sides of a stereo track), every channel
/// averaged on a mono track, else the stereo fold.
fn clip_frame(channels: &[Vec<f32>], clip_channel: Option<u16>, frame: usize, mono: bool) -> (f32, f32) {
    let at = |ch: &Vec<f32>| ch.get(frame).copied().unwrap_or(0.0);
    if let Some(ch) = clip_channel {
        let v = channels.get(ch as usize).map_or(0.0, at);
        return (v, v);
    }
    if mono {
        let v = channels.iter().map(at).sum::<f32>() / channels.len().max(1) as f32;
        return (v, v);
    }
    stereo_frame(channels, frame)
}

/// Add one clip, trimmed, faded and crossfaded, into a track buffer of one
/// channel (a mono track) or two.
fn place_clip(
    out: &mut [Vec<f32>],
    clip: &Clip,
    xfades: &[XfadeSeg],
    source: &SourceAudio,
    sr: u32,
) {
    let start = secs_to_frames(clip.start_secs, sr);
    let src_in = secs_to_frames(clip.in_secs, sr);
    let src_frames = source.frames();
    if src_in >= src_frames || start >= out[0].len() {
        return;
    }
    let len = secs_to_frames(clip.len_secs, sr)
        .min(src_frames - src_in)
        .min(out[0].len() - start);
    let fade_in = secs_to_frames(clip.fade_in_secs, sr).min(len);
    let fade_out = secs_to_frames(clip.fade_out_secs, sr).min(len);
    let channels = source.channels.as_slice();
    let mono = out.len() == 1;
    let (left, right) = out.split_at_mut(1);
    let left = &mut left[0];
    let mut right = right.first_mut();
    for i in 0..len {
        let mut gain = 1.0f32;
        if i < fade_in {
            gain *= i as f32 / fade_in as f32;
        }
        let from_end = len - i;
        if from_end <= fade_out {
            gain *= (from_end - 1) as f32 / fade_out as f32;
        }
        if !xfades.is_empty() {
            gain *= xfade_gain(xfades, (start + i) as f64 / sr as f64);
        }
        let (l, r) = clip_frame(channels, clip.channel, src_in + i, mono);
        left[start + i] += l * gain;
        if let Some(right) = right.as_deref_mut() {
            right[start + i] += r * gain;
        }
    }
}

/// `AutomationLane::value_at` for times that only move forward, in constant
/// time per call rather than a walk from the first point: a lane is read once
/// per frame, and a long track has millions of them.
struct LaneCursor<'a> {
    lane: &'a AutomationLane,
    idx: usize,
}

impl<'a> LaneCursor<'a> {
    fn new(lane: &'a AutomationLane) -> Self {
        Self { lane, idx: 0 }
    }

    fn value(&mut self, secs: f64) -> f32 {
        let points = &self.lane.points;
        let Some(first) = points.first() else {
            return self.lane.param.neutral();
        };
        if secs <= first.secs {
            return first.value;
        }
        while self.idx + 1 < points.len() && points[self.idx + 1].secs <= secs {
            self.idx += 1;
        }
        let a = points[self.idx];
        let Some(b) = points.get(self.idx + 1) else {
            return a.value;
        };
        if self.lane.param.is_stepped() || b.secs <= a.secs {
            return a.value;
        }
        let t = ((secs - a.secs) / (b.secs - a.secs)) as f32;
        a.value + (b.value - a.value) * t
    }
}

/// A lane's points as (frame, value), for the sample-indexed DSP helpers.
fn lane_points_in_frames(lane: &AutomationLane, sr: u32) -> Vec<(usize, f32)> {
    lane.points
        .iter()
        .map(|point| (secs_to_frames(point.secs, sr), point.value))
        .collect()
}

/// Everything after the clips are laid down: Pitch, Gain with the fader,
/// Mute. The pan is applied where the track meets the master
/// (`mix_track_into`), because on a surround output it decides which
/// channels the track reaches at all.
fn apply_track_processing(buf: &mut Vec<Vec<f32>>, track: &Track, sr: u32) {
    if let Some(lane) = track.lane(LaneParam::Pitch).filter(|lane| !lane.is_neutral()) {
        let points = lane_points_in_frames(lane, sr);
        *buf = crate::wave::process_pitchshift_curve_multi(buf, sr, &points, 0.0, 0);
    }
    let fader = crate::app::helpers::db_to_amp(track.volume_db);
    let gain_lane = track.lane(LaneParam::Gain).filter(|lane| !lane.is_neutral());
    for channel in buf.iter_mut() {
        if let Some(lane) = gain_lane {
            crate::wave::apply_gain_envelope_in_place(
                channel,
                &lane_points_in_frames(lane, sr),
                0.0,
                false,
            );
        }
        if (fader - 1.0).abs() > f32::EPSILON {
            for v in channel.iter_mut() {
                *v *= fader;
            }
        }
    }
    let frames = buf.first().map(Vec::len).unwrap_or(0);
    if let Some(lane) = track.lane(LaneParam::Mute).filter(|lane| !lane.is_neutral()) {
        let step = 1.0 / (MUTE_RAMP_SECS * sr as f64).max(1.0) as f32;
        let mut gain = if lane.value_at(0.0) >= 0.5 { 0.0f32 } else { 1.0 };
        let mut cursor = LaneCursor::new(lane);
        for frame in 0..frames {
            let target = if cursor.value(frame as f64 / sr as f64) >= 0.5 {
                0.0
            } else {
                1.0
            };
            if gain < target {
                gain = (gain + step).min(target);
            } else if gain > target {
                gain = (gain - step).max(target);
            }
            for channel in buf.iter_mut() {
                channel[frame] *= gain;
            }
        }
    }
}

/// Render one track on its own: clips, then the track's processing. `None`
/// when cancelled. A track that is not heard renders as silence.
pub fn render_track(
    doc: &MultiEditDoc,
    track: &Track,
    sources: &SourceMap,
    sr: u32,
    frames: usize,
    cancel: Option<&AtomicBool>,
) -> Option<Vec<Vec<f32>>> {
    let width = if track.output.is_mono() { 1 } else { STEREO };
    let mut buf = vec![vec![0.0f32; frames]; width];
    if !doc.track_audible(track) {
        return Some(buf);
    }
    let xfades = crossfade_segments(&track.clips);
    for (clip, segs) in track.clips.iter().zip(xfades.iter()) {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return None;
        }
        if let Some(source) = sources.get(&clip.source.path) {
            place_clip(&mut buf, clip, segs, source, sr);
        }
    }
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return None;
    }
    apply_track_processing(&mut buf, track, sr);
    Some(buf)
}

/// Add `src` into `dst`, sample by sample, times `gain`.
fn add_into(dst: &mut [f32], src: &[f32], gain: f32) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d += *s * gain;
    }
}

/// How often a moving pan (a Pan lane) is evaluated; the gains in between
/// are interpolated. A millisecond is far finer than a pan is heard to step,
/// and keeps the VBAP search out of the per-sample loop.
const PAN_BLOCK_SECS: f64 = 0.001;

/// Add the gains of the speaker direction of `pos` on `layout`, turned by
/// `pan`, into `gains`.
fn add_turned(speakers: &Speakers, layout: &[Option<SpeakerPos>], pos: SpeakerPos, pan: f32, gains: &mut [f32]) {
    let (azimuth, elevation) = pos.direction(layout);
    let dir = Rotation::yaw(pan * PAN_TURN_DEG).apply(dir_to_vec(azimuth, elevation));
    speakers.add_gains(dir, 1.0, gains);
}

/// Each output channel's gain for channel `ch` of a track rendered for
/// `output`, at pan `pan` (the knob plus its lane), into `gains` (one per
/// output channel; cleared first). `turn` is the output's speakers when the
/// output turns (`multi_edit::pan_rotates`), `None` when it balances.
fn route_gains(
    layout: &[Option<SpeakerPos>],
    turn: Option<&Speakers>,
    output: TrackOutput,
    ch: usize,
    pan: f32,
    gains: &mut [f32],
) {
    gains.iter_mut().for_each(|g| *g = 0.0);
    match output {
        TrackOutput::Channel { index } => {
            let speaker = layout.get(index).copied().flatten();
            match (turn, speaker) {
                // Turned round from its own speaker.
                (Some(speakers), Some(pos)) if pan != 0.0 && !pos.is_lfe() => {
                    add_turned(speakers, layout, pos, pan, gains);
                }
                _ => {
                    if let Some(slot) = gains.get_mut(index) {
                        *slot = 1.0;
                    }
                }
            }
        }
        TrackOutput::Stereo => {
            if gains.len() == 1 {
                // Both sides averaged onto a mono output, after the balance.
                let (gl, gr) = pan_gains(pan);
                gains[0] = if ch == 0 { gl } else { gr } / STEREO as f32;
                return;
            }
            if let Some(speakers) = turn.filter(|_| pan != 0.0) {
                // The pair turned round the listener from where it stands.
                let side = if ch == 0 { SpeakerPos::Fl } else { SpeakerPos::Fr };
                add_turned(speakers, layout, side, pan, gains);
                return;
            }
            let find = |pos: SpeakerPos| layout.iter().position(|p| *p == Some(pos));
            let (left, right) = match (find(SpeakerPos::Fl), find(SpeakerPos::Fr)) {
                (Some(l), Some(r)) => (l, r),
                _ => (0, 1),
            };
            // A turning output turns; only a balancing one balances.
            let (gl, gr) = if turn.is_some() { (1.0, 1.0) } else { pan_gains(pan) };
            let (dst, gain) = if ch == 0 { (left, gl) } else { (right, gr) };
            if let Some(slot) = gains.get_mut(dst) {
                *slot = gain;
            }
        }
    }
}

/// Add one rendered track into the master through its pan: a stereo track
/// onto the output's front pair (its first two channels when it names
/// none; both sides averaged onto a mono output), a mono track onto its own
/// channel alone -- each then balanced, or turned round the listener when
/// the output turns. A Pan lane is evaluated every `PAN_BLOCK_SECS` and the
/// gains slide in between, so a moving pan does not click.
fn mix_track_into(
    master: &mut [Vec<f32>],
    layout: &[Option<SpeakerPos>],
    turn: Option<&Speakers>,
    track: &Track,
    buf: &[Vec<f32>],
    sr: u32,
) {
    let outs = master.len();
    let width = buf.len();
    let frames = buf.first().map(Vec::len).unwrap_or(0);
    let pans = track_pans(layout, track.output);
    let knob = if pans { track.pan } else { 0.0 };
    let lane = track
        .lane(LaneParam::Pan)
        .filter(|lane| pans && !lane.is_neutral());
    let Some(lane) = lane else {
        let mut gains = vec![0.0f32; outs];
        for (ch, src) in buf.iter().enumerate() {
            route_gains(layout, turn, track.output, ch, knob, &mut gains);
            for (dst, &gain) in master.iter_mut().zip(&gains) {
                if gain != 0.0 {
                    add_into(dst, src, gain);
                }
            }
        }
        return;
    };
    let block = secs_to_frames(PAN_BLOCK_SECS, sr).max(1);
    let mut cursor = LaneCursor::new(lane);
    let mut pan_at = |frame: usize| knob + cursor.value(frame as f64 / sr.max(1) as f64);
    let mut from = vec![vec![0.0f32; outs]; width];
    let mut to = vec![vec![0.0f32; outs]; width];
    let pan = pan_at(0);
    for (ch, gains) in from.iter_mut().enumerate() {
        route_gains(layout, turn, track.output, ch, pan, gains);
    }
    let mut start = 0;
    while start < frames {
        let end = (start + block).min(frames);
        let pan = pan_at(end);
        for (ch, gains) in to.iter_mut().enumerate() {
            route_gains(layout, turn, track.output, ch, pan, gains);
        }
        let len = (end - start) as f32;
        for (ch, src) in buf.iter().enumerate() {
            let src = &src[start..end];
            for (out, dst) in master.iter_mut().enumerate() {
                let (g0, g1) = (from[ch][out], to[ch][out]);
                if g0 == 0.0 && g1 == 0.0 {
                    continue;
                }
                let step = (g1 - g0) / len;
                for (i, (d, s)) in dst[start..end].iter_mut().zip(src).enumerate() {
                    *d += *s * (g0 + step * i as f32);
                }
            }
        }
        std::mem::swap(&mut from, &mut to);
        start = end;
    }
}

/// The whole timeline at `sr`, one channel per speaker of its output layout,
/// from the start to the end of the last clip. Every source must already be
/// at `sr`. `progress` is told the fraction done after each track. `None`
/// when cancelled.
pub fn render_timeline(
    doc: &MultiEditDoc,
    sources: &SourceMap,
    sr: u32,
    cancel: Option<&AtomicBool>,
    mut progress: impl FnMut(f32),
) -> Option<Vec<Vec<f32>>> {
    let sr = sr.max(1);
    let frames = secs_to_frames(doc.end_secs(), sr);
    let layout = doc.output_layout();
    let turn = pan_rotates(&layout).then(|| Speakers::of(&layout));
    let mut master = vec![vec![0.0f32; frames]; layout.len().max(1)];
    let tracks = doc.tracks.len().max(1);
    for (idx, track) in doc.tracks.iter().enumerate() {
        let buf = render_track(doc, track, sources, sr, frames, cancel)?;
        mix_track_into(&mut master, &layout, turn.as_ref(), track, &buf, sr);
        progress((idx + 1) as f32 / tracks as f32);
    }
    Some(master)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::multi_edit::{ClipSource, NewClip, TrackKind};

    const SR: u32 = 48_000;

    fn tone(freq: f32, secs: f32, amp: f32) -> Vec<f32> {
        let frames = (SR as f32 * secs) as usize;
        (0..frames)
            .map(|i| (i as f32 / SR as f32 * freq * std::f32::consts::TAU).sin() * amp)
            .collect()
    }

    fn source(channels: Vec<Vec<f32>>) -> Arc<SourceAudio> {
        Arc::new(SourceAudio {
            channels: Arc::new(channels),
            sample_rate: SR,
        })
    }

    fn doc_with(sources: &[(&str, f64)]) -> MultiEditDoc {
        let mut doc = MultiEditDoc::new("t");
        for (name, len) in sources {
            let track = doc.add_track(TrackKind::Audio);
            doc.insert_clips(
                track,
                0.0,
                vec![NewClip {
                    source: ClipSource {
                        path: PathBuf::from(name),
                        asset_id: None,
                    },
                    name: name.to_string(),
                    len_secs: Some(*len),
                    is_video: false,
                }],
            );
        }
        doc
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    fn zero_crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|w| (w[0] <= 0.0) != (w[1] <= 0.0))
            .count()
    }

    fn render(doc: &MultiEditDoc, sources: &SourceMap) -> Vec<Vec<f32>> {
        render_timeline(doc, sources, SR, None, |_| {}).expect("not cancelled")
    }

    #[test]
    fn overlapping_tracks_add_up() {
        let doc = doc_with(&[("a", 1.0), ("b", 1.0)]);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![0.25; SR as usize]]));
        sources.insert("b".into(), source(vec![vec![0.5; SR as usize]]));
        let mix = render(&doc, &sources);
        assert_eq!(mix.len(), 2);
        assert_eq!(mix[0].len(), SR as usize);
        assert!((mix[0][100] - 0.75).abs() < 1e-6);
        assert!((mix[1][100] - 0.75).abs() < 1e-6, "mono goes to both sides");
    }

    #[test]
    fn a_trimmed_clip_plays_the_source_from_its_offset() {
        let mut doc = doc_with(&[("ramp", 2.0)]);
        let id = doc.tracks[0].clips[0].id.clone();
        doc.trim_clip_start(&id, 0.5);
        let ramp: Vec<f32> = (0..2 * SR as usize).map(|i| i as f32 / 1e6).collect();
        let mut sources = SourceMap::new();
        sources.insert("ramp".into(), source(vec![ramp.clone()]));
        let mix = render(&doc, &sources);
        let at = SR as usize / 2;
        assert_eq!(mix[0][at - 1], 0.0, "silent before the clip");
        assert!((mix[0][at] - ramp[at]).abs() < 1e-9, "the audio stayed in place");
    }

    #[test]
    fn fades_start_and_end_at_silence() {
        let mut doc = doc_with(&[("a", 1.0)]);
        let id = doc.tracks[0].clips[0].id.clone();
        doc.set_fade_in(&id, 0.1);
        doc.set_fade_out(&id, 0.1);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![1.0; SR as usize]]));
        let mix = render(&doc, &sources);
        assert_eq!(mix[0][0], 0.0);
        assert_eq!(*mix[0].last().unwrap(), 0.0);
        assert!((mix[0][SR as usize / 2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_gain_point_at_minus_six_halves_the_level() {
        let mut doc = doc_with(&[("a", 1.0)]);
        doc.ensure_lane(0, LaneParam::Gain);
        doc.tracks[0].lanes[0].insert_point(0.0, -6.0206);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![0.8; SR as usize]]));
        let mix = render(&doc, &sources);
        assert!((mix[0][1000] - 0.4).abs() < 1e-3, "{}", mix[0][1000]);
    }

    #[test]
    fn hard_left_silences_the_right() {
        let mut doc = doc_with(&[("a", 1.0)]);
        doc.ensure_lane(0, LaneParam::Pan);
        doc.tracks[0].lanes[0].insert_point(0.0, -1.0);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![tone(440.0, 1.0, 0.5)]));
        let mix = render(&doc, &sources);
        assert!(peak(&mix[1]) < 1e-6);
        assert!((peak(&mix[0]) - 0.5).abs() < 1e-3, "the kept side is not boosted");
    }

    #[test]
    fn a_mute_stretch_is_silent() {
        let mut doc = doc_with(&[("a", 2.0)]);
        doc.ensure_lane(0, LaneParam::Mute);
        let lane = &mut doc.tracks[0].lanes[0];
        lane.insert_point(0.0, 0.0);
        lane.insert_point(0.5, 1.0);
        lane.insert_point(1.5, 0.0);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![0.5; 2 * SR as usize]]));
        let mix = render(&doc, &sources);
        let quarter = SR as usize / 4;
        assert!((mix[0][quarter] - 0.5).abs() < 1e-6);
        assert!(peak(&mix[0][SR as usize - quarter..SR as usize + quarter]) < 1e-6);
        assert!((mix[0][7 * quarter] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn plus_twelve_semitones_doubles_the_frequency() {
        let mut doc = doc_with(&[("a", 1.0)]);
        doc.ensure_lane(0, LaneParam::Pitch);
        doc.tracks[0].lanes[0].insert_point(0.0, 12.0);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![tone(220.0, 1.0, 0.5)]));
        let mix = render(&doc, &sources);
        // Skip the edges, where the shifter is settling.
        let middle = &mix[0][SR as usize / 4..3 * SR as usize / 4];
        let crossings = zero_crossings(middle) as f32;
        let expected = 2.0 * 440.0 * 0.5;
        assert!((crossings / expected - 1.0).abs() < 0.1, "{crossings} vs {expected}");
    }

    #[test]
    fn a_muted_track_and_a_missing_source_are_silent() {
        let mut doc = doc_with(&[("a", 1.0), ("gone", 1.0)]);
        doc.tracks[0].mute = true;
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![0.5; SR as usize]]));
        let mix = render(&doc, &sources);
        assert_eq!(peak(&mix[0]), 0.0);
    }

    #[test]
    fn many_channels_fold_to_stereo_by_parity() {
        let doc = doc_with(&[("surround", 0.1)]);
        let frames = SR as usize / 10;
        let mut sources = SourceMap::new();
        sources.insert(
            "surround".into(),
            source(vec![
                vec![0.2; frames],
                vec![0.4; frames],
                vec![0.6; frames],
                vec![0.0; frames],
            ]),
        );
        let mix = render(&doc, &sources);
        assert!((mix[0][10] - 0.4).abs() < 1e-6, "(0.2 + 0.6) / 2");
        assert!((mix[1][10] - 0.2).abs() < 1e-6, "(0.4 + 0.0) / 2");
    }

    fn layout(text: &str) -> crate::audio_channels::Layout {
        crate::app::channel_layout_ops::layout_from_string(text).expect("a layout")
    }

    #[test]
    fn a_split_surround_clip_mixes_back_into_its_own_channels() {
        // Six channels of noise, each its own.
        let frames = SR as usize / 2;
        let channels: Vec<Vec<f32>> = (0..6u32)
            .map(|ch| {
                let mut state = (ch + 1).wrapping_mul(2_654_435_761);
                (0..frames)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        (state as f32 / u32::MAX as f32) - 0.5
                    })
                    .collect()
            })
            .collect();
        let mut doc = doc_with(&[("surround", 0.5)]);
        let id = doc.tracks[0].clips[0].id.clone();
        assert_eq!(doc.split_clip_by_channel(&id, &layout("FL,FR,FC,LFE,BL,BR")).len(), 6);
        let mut sources = SourceMap::new();
        sources.insert("surround".into(), source(channels.clone()));
        let mix = render(&doc, &sources);
        assert_eq!(mix.len(), 6, "the timeline is 5.1 now");
        for (ch, (got, want)) in mix.iter().zip(&channels).enumerate() {
            assert_eq!(got.len(), frames);
            assert!(got.iter().zip(want).all(|(g, w)| g == w), "channel {ch} is the source's");
        }
    }

    #[test]
    fn a_mono_track_reaches_its_channel_alone() {
        let mut doc = doc_with(&[("stereo", 0.1)]);
        doc.set_output_layout(&layout("FL,FR,FC,LFE,BL,BR"));
        doc.tracks[0].output = TrackOutput::Channel { index: 2 };
        let frames = SR as usize / 10;
        let mut sources = SourceMap::new();
        sources.insert("stereo".into(), source(vec![vec![0.2; frames], vec![0.6; frames]]));
        let mix = render(&doc, &sources);
        assert_eq!(mix.len(), 6);
        assert!((mix[2][10] - 0.4).abs() < 1e-6, "both sides averaged into the centre");
        for ch in [0, 1, 3, 4, 5] {
            assert_eq!(peak(&mix[ch]), 0.0, "channel {ch}");
        }
    }

    #[test]
    fn a_stereo_track_feeds_the_front_pair_of_a_surround_output() {
        let mut doc = doc_with(&[("stereo", 0.1)]);
        // Film order: L C R Ls Rs LFE, so the front pair is 0 and 2.
        doc.set_output_layout(&layout("FL,FC,FR,BL,BR,LFE"));
        let frames = SR as usize / 10;
        let mut sources = SourceMap::new();
        sources.insert("stereo".into(), source(vec![vec![0.2; frames], vec![0.6; frames]]));
        let mix = render(&doc, &sources);
        assert!((mix[0][10] - 0.2).abs() < 1e-6);
        assert!((mix[2][10] - 0.6).abs() < 1e-6);
        assert_eq!(peak(&mix[1]), 0.0);
    }

    #[test]
    fn a_mono_track_on_a_stereo_output_ignores_pan_but_not_mute() {
        let mut doc = doc_with(&[("a", 1.0)]);
        doc.tracks[0].output = TrackOutput::Channel { index: 1 };
        doc.tracks[0].pan = -1.0;
        doc.ensure_lane(0, LaneParam::Pan);
        doc.tracks[0].lanes[0].insert_point(0.0, -1.0);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![0.5; SR as usize]]));
        let mix = render(&doc, &sources);
        assert!((mix[1][100] - 0.5).abs() < 1e-6, "hard left means nothing on one channel");
        assert_eq!(peak(&mix[0]), 0.0);
        let mute = doc.ensure_lane(0, LaneParam::Mute).expect("lane");
        doc.tracks[0].lanes[mute].insert_point(0.0, 1.0);
        let mix = render(&doc, &sources);
        assert_eq!(peak(&mix[1][1000..]), 0.0);
    }

    /// One mono track playing a steady 0.5 on `output` of `layout`, panned
    /// by the knob.
    fn panned(layout_text: &str, output: TrackOutput, pan: f32) -> Vec<Vec<f32>> {
        let mut doc = doc_with(&[("a", 0.1)]);
        doc.set_output_layout(&layout(layout_text));
        doc.tracks[0].output = output;
        doc.tracks[0].pan = pan;
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![0.5; SR as usize / 10]]));
        render(&doc, &sources)
    }

    const SEVEN_ONE: &str = "FL,FR,FC,LFE,BL,BR,SL,SR";

    #[test]
    fn the_pan_knob_balances_a_stereo_output() {
        let mix = panned("FL,FR", TrackOutput::Stereo, -1.0);
        assert!((mix[0][10] - 0.5).abs() < 1e-6);
        assert_eq!(peak(&mix[1]), 0.0);
        // 2.1 has two speakers to turn around: it balances too.
        let mix = panned("FL,FR,LFE", TrackOutput::Stereo, 1.0);
        assert_eq!(peak(&mix[0]), 0.0);
        assert!((mix[1][10] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_quarter_turn_takes_the_centre_to_the_right_side_speaker() {
        // 7.1: the sides are at +-90 degrees.
        let mix = panned(SEVEN_ONE, TrackOutput::Channel { index: 2 }, 0.5);
        for (ch, channel) in mix.iter().enumerate() {
            let want = if ch == 7 { 0.5 } else { 0.0 };
            assert!((channel[10] - want).abs() < 1e-5, "channel {ch}: {}", channel[10]);
        }
        let mix = panned(SEVEN_ONE, TrackOutput::Channel { index: 2 }, -0.5);
        assert!((mix[6][10] - 0.5).abs() < 1e-5, "a quarter turn left: SL");
    }

    #[test]
    fn a_turn_keeps_the_level_and_never_reaches_the_lfe() {
        let layout = layout(SEVEN_ONE);
        assert!(pan_rotates(&layout), "7.1 turns");
        let speakers = Speakers::of(&layout);
        let mut gains = vec![0.0; layout.len()];
        for step in -20..=20 {
            let pan = step as f32 / 20.0;
            for ch in 0..2 {
                route_gains(&layout, Some(&speakers), TrackOutput::Stereo, ch, pan, &mut gains);
                let power: f32 = gains.iter().map(|g| g * g).sum();
                assert!((power - 1.0).abs() < 1e-4, "pan {pan} side {ch}: {gains:?}");
                assert_eq!(gains[3], 0.0, "pan {pan}: the LFE");
            }
        }
        // A stereo pair turned half way round faces the back, sides swapped.
        let mix = panned(SEVEN_ONE, TrackOutput::Stereo, 1.0);
        assert_eq!(peak(&mix[0]) + peak(&mix[1]) + peak(&mix[2]) + peak(&mix[3]), 0.0);
        assert!(peak(&mix[4]) > 0.0 && peak(&mix[5]) > 0.0);
    }

    #[test]
    fn the_lfe_is_not_turned() {
        let mix = panned(SEVEN_ONE, TrackOutput::Channel { index: 3 }, 0.5);
        for (ch, channel) in mix.iter().enumerate() {
            let want = if ch == 3 { 0.5 } else { 0.0 };
            assert!((channel[10] - want).abs() < 1e-6, "channel {ch}");
        }
    }

    #[test]
    fn the_height_layer_turns_on_its_own_ring() {
        // 7.1.4: Ltf at -45 turned a quarter right is at +45, Rtf.
        let mix = panned(
            "FL,FR,FC,LFE,BL,BR,SL,SR,TFL,TFR,TBL,TBR",
            TrackOutput::Channel { index: 8 },
            0.5,
        );
        assert!((mix[9][10] - 0.5).abs() < 1e-5, "{}", mix[9][10]);
        assert_eq!(mix[..8].iter().map(|c| peak(c)).sum::<f32>(), 0.0, "nothing at ear level");
    }

    #[test]
    fn a_pan_lane_turns_smoothly_from_the_knob() {
        let mut doc = doc_with(&[("a", 1.0)]);
        doc.set_output_layout(&layout(SEVEN_ONE));
        doc.tracks[0].output = TrackOutput::Channel { index: 2 };
        doc.tracks[0].pan = 0.25;
        doc.ensure_lane(0, LaneParam::Pan);
        doc.tracks[0].lanes[0].insert_point(0.0, -0.25);
        doc.tracks[0].lanes[0].insert_point(1.0, 0.25);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![0.5; SR as usize]]));
        let mix = render(&doc, &sources);
        // The lane moves the knob: it starts dead centre, ends at the side.
        assert!((mix[2][10] - 0.5).abs() < 1e-4, "{}", mix[2][10]);
        let last = SR as usize - 1;
        assert!(mix[7][last] > 0.49, "{}", mix[7][last]);
        assert_eq!(peak(&mix[3]), 0.0, "the LFE");
        for (ch, channel) in mix.iter().enumerate() {
            let jump = channel.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
            assert!(jump < 1e-3, "channel {ch} steps by {jump}");
        }
    }

    /// Two clips on one track, overlapping.
    fn overlapping(a: (f64, f64), b: (f64, f64)) -> MultiEditDoc {
        let mut doc = MultiEditDoc::new("t");
        let track = doc.add_track(TrackKind::Audio);
        for (name, (start, len)) in [("a", a), ("b", b)] {
            doc.insert_clips(
                track,
                start,
                vec![NewClip {
                    source: ClipSource {
                        path: PathBuf::from(name),
                        asset_id: None,
                    },
                    name: name.to_string(),
                    len_secs: Some(len),
                    is_video: false,
                }],
            );
        }
        doc
    }

    #[test]
    fn an_overlap_crossfades_at_equal_power() {
        let doc = overlapping((0.0, 2.0), (1.0, 2.0));
        let segs = crossfade_segments(&doc.tracks[0].clips);
        let (a, b) = (xfade_gain(&segs[0], 1.5), xfade_gain(&segs[1], 1.5));
        assert!((a - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4, "{a}");
        assert!((b - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4, "{b}");
        assert!((a * a + b * b - 1.0).abs() < 1e-4, "power is kept");
        assert_eq!(xfade_gain(&segs[0], 0.5), 1.0, "untouched before the overlap");
        assert_eq!(xfade_gain(&segs[1], 2.5), 1.0, "and after it");

        // Two unrelated noises: the level through the join matches either side.
        let noise = |seed: u32| -> Vec<f32> {
            let mut x = seed;
            (0..2 * SR as usize)
                .map(|_| {
                    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (x >> 8) as f32 / (1u32 << 24) as f32 - 0.5
                })
                .collect()
        };
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![noise(1)]));
        sources.insert("b".into(), source(vec![noise(7)]));
        let mix = render(&doc, &sources);
        let rms = |from: f64, to: f64| {
            let s = &mix[0][(from * SR as f64) as usize..(to * SR as f64) as usize];
            (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt()
        };
        let (before, join) = (rms(0.1, 0.9), rms(1.35, 1.65));
        assert!((join / before - 1.0).abs() < 0.1, "{before} vs {join}");
    }

    #[test]
    fn a_clip_inside_another_replaces_it_there() {
        let doc = overlapping((0.0, 4.0), (1.0, 1.0));
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![1.0; 4 * SR as usize]]));
        sources.insert("b".into(), source(vec![vec![0.5; SR as usize]]));
        let mix = render(&doc, &sources);
        let at = |t: f64| mix[0][(t * SR as f64) as usize];
        assert!((at(0.5) - 1.0).abs() < 1e-6);
        assert!((at(1.5) - 0.5).abs() < 1e-6, "only the inner clip: {}", at(1.5));
        assert!((at(3.0) - 1.0).abs() < 1e-6, "the outer clip comes back");
    }

    /// Clips on one track at (start, length), placed in the order given.
    fn clips_at(spans: &[(f64, f64)]) -> Vec<Clip> {
        let mut doc = MultiEditDoc::new("t");
        let track = doc.add_track(TrackKind::Audio);
        for (i, &(start, len)) in spans.iter().enumerate() {
            doc.insert_clips(
                track,
                start,
                vec![NewClip {
                    source: ClipSource {
                        path: PathBuf::from(format!("c{i}")),
                        asset_id: None,
                    },
                    name: format!("c{i}"),
                    len_secs: Some(len),
                    is_video: false,
                }],
            );
        }
        doc.tracks.remove(track).clips
    }

    #[test]
    fn covered_spans_are_where_a_later_clip_lies_over_an_earlier_one() {
        // Given out of order: B 3-6, D 8-9, A 0-4, C 1-2 (inside A), E 3.5-5.
        let clips = clips_at(&[(3.0, 3.0), (8.0, 1.0), (0.0, 4.0), (1.0, 1.0), (3.5, 1.5)]);
        let covered = covered_spans(&clips);
        assert_eq!(covered[2], vec![], "A starts first: nothing under it");
        assert_eq!(covered[3], vec![(1.0, 2.0)], "C lies wholly over A");
        assert_eq!(covered[0], vec![(3.0, 4.0)], "B over A's end");
        assert_eq!(covered[4], vec![(3.5, 5.0)], "E over A and B, merged");
        assert_eq!(covered[1], vec![], "D overlaps nothing");
        assert_eq!(draw_order(&clips), vec![2, 3, 0, 4, 1], "back to front by start");
    }

    #[test]
    fn a_clip_without_a_length_is_not_heard_and_crosses_nothing() {
        let mut doc = MultiEditDoc::new("t");
        let track = doc.add_track(TrackKind::Audio);
        let new = |name: &str, len: Option<f64>| NewClip {
            source: ClipSource {
                path: PathBuf::from(name),
                asset_id: None,
            },
            name: name.to_string(),
            len_secs: len,
            is_video: false,
        };
        doc.insert_clips(track, 0.0, vec![new("a", Some(2.0))]);
        doc.insert_clips(track, 1.0, vec![new("b", None)]);
        let clips = &doc.tracks[track].clips;
        assert!(crossfade_segments(clips).iter().all(Vec::is_empty));
        assert!(covered_spans(clips).iter().all(Vec::is_empty));
        assert_eq!(draw_order(clips), vec![0], "drawn as its start, apart");
    }

    #[test]
    fn a_cancelled_render_stops() {
        let doc = doc_with(&[("a", 1.0)]);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![0.5; SR as usize]]));
        let cancel = AtomicBool::new(true);
        assert!(render_timeline(&doc, &sources, SR, Some(&cancel), |_| {}).is_none());
    }
}
