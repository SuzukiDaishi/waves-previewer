//! Turning a Multi Edits timeline into samples.
//!
//! Pure and thread-agnostic: every function here runs on a worker, is handed
//! everything it needs, and touches neither the app nor the disk.
//!
//! One track at a time, in the order the spec gives
//! (`docs/MULTI_EDITS_SPEC.md`): lay the clips down with their trims and
//! fades, then Pitch, then Gain with the fader, then Pan, then Mute, and add
//! the result to the master. Pitch is the one stage that needs the whole
//! track at once (the shifter carries state across the timeline), which is
//! why a track is rendered whole rather than in blocks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::multi_edit::{AutomationLane, Clip, LaneParam, MultiEditDoc, Track};

/// The mix is stereo whatever the sources are; see `fold_to_stereo`.
pub const MIX_CHANNELS: usize = 2;

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
    let mut order: Vec<usize> = (0..clips.len()).collect();
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

/// Add one clip, trimmed, faded and crossfaded, into a stereo track buffer.
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
    let (left, right) = out.split_at_mut(1);
    let (left, right) = (&mut left[0], &mut right[0]);
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
        let (l, r) = stereo_frame(channels, src_in + i);
        left[start + i] += l * gain;
        right[start + i] += r * gain;
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
/// Pan, Mute.
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
    if let Some(lane) = track.lane(LaneParam::Pan).filter(|lane| !lane.is_neutral()) {
        let mut cursor = LaneCursor::new(lane);
        for frame in 0..frames {
            let (gl, gr) = pan_gains(cursor.value(frame as f64 / sr as f64));
            buf[0][frame] *= gl;
            buf[1][frame] *= gr;
        }
    }
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
            buf[0][frame] *= gain;
            buf[1][frame] *= gain;
        }
    }
}

/// Balance: the centre leaves both sides as they are, and turning towards
/// one side lowers the other along a quarter cosine, so hard left is the left
/// channel alone and nothing is ever boosted.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    let fall = |amount: f32| (amount * std::f32::consts::FRAC_PI_2).cos();
    if pan > 0.0 {
        (fall(pan), 1.0)
    } else if pan < 0.0 {
        (1.0, fall(-pan))
    } else {
        (1.0, 1.0)
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
    let mut buf = vec![vec![0.0f32; frames]; MIX_CHANNELS];
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

/// The whole timeline as stereo at `sr`, from the start to the end of the
/// last clip. Every source must already be at `sr`. `progress` is told the
/// fraction done after each track. `None` when cancelled.
pub fn render_timeline(
    doc: &MultiEditDoc,
    sources: &SourceMap,
    sr: u32,
    cancel: Option<&AtomicBool>,
    mut progress: impl FnMut(f32),
) -> Option<Vec<Vec<f32>>> {
    let sr = sr.max(1);
    let frames = secs_to_frames(doc.end_secs(), sr);
    let mut master = vec![vec![0.0f32; frames]; MIX_CHANNELS];
    let tracks = doc.tracks.len().max(1);
    for (idx, track) in doc.tracks.iter().enumerate() {
        let buf = render_track(doc, track, sources, sr, frames, cancel)?;
        for (dst, src) in master.iter_mut().zip(buf.iter()) {
            for (d, s) in dst.iter_mut().zip(src.iter()) {
                *d += *s;
            }
        }
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
                    len_secs: *len,
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
                    len_secs: len,
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

    #[test]
    fn a_cancelled_render_stops() {
        let doc = doc_with(&[("a", 1.0)]);
        let mut sources = SourceMap::new();
        sources.insert("a".into(), source(vec![vec![0.5; SR as usize]]));
        let cancel = AtomicBool::new(true);
        assert!(render_timeline(&doc, &sources, SR, Some(&cancel), |_| {}).is_none());
    }
}
