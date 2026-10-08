//! The scene: which track is heard from where, and when. Rules only -- no
//! UI, no disk, no audio. Times are seconds from the start of the file, never
//! sample indices, so one scene serves the file's own rate, the output
//! device's and an export alike (as in `app::multi_edit`).

use std::sync::Arc;

use crate::audio_channels::SpeakerPos;

use super::coords;
use super::ObjectFormat;

/// How an element's positions were written, and so how its keyframes hold
/// them and how they are edited and exported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coords {
    /// ADM Cartesian `[X, Y, Z]`.
    Cartesian,
    /// ADM polar `[azimuth (left-positive), elevation, distance]`.
    Polar,
}

/// One point an element moves to.
///
/// The value holds until `secs - ramp_secs`, then moves in a straight line
/// to `pos` / `gain`, arriving at `secs`. That is exactly an ADM
/// `audioBlockFormat` (a ramp of the block's duration, or of its
/// `interpolationLength` when `jumpPosition` is set) and a TrueHD object
/// update (a ramp over `ramp_duration` samples), so neither loses anything.
#[derive(Clone, Debug, PartialEq)]
pub struct Keyframe {
    pub secs: f64,
    pub ramp_secs: f64,
    /// In the element's own [`Coords`].
    pub pos: [f32; 3],
    /// Linear.
    pub gain: f32,
    /// The ADM block's other children (width, diffuse, channelLock, ...),
    /// verbatim, so an export of an edited element keeps them.
    pub extras: Option<Arc<str>>,
}

impl Keyframe {
    pub fn at(secs: f64, pos: [f32; 3]) -> Self {
        Self {
            secs,
            ramp_secs: 0.0,
            pos,
            gain: 1.0,
            extras: None,
        }
    }

    fn same_value(&self, other: &Keyframe) -> bool {
        self.pos == other.pos && self.gain == other.gain && self.extras == other.extras
    }
}

/// What an element is.
#[derive(Clone, Debug, PartialEq)]
pub enum ElementKind {
    /// A channel that feeds a fixed speaker. `speaker` is `None` when the
    /// app has no `SpeakerPos` for it (a bottom-layer speaker, say); it is
    /// then panned at its nominal position.
    Bed {
        speaker: Option<SpeakerPos>,
        label: Arc<str>,
    },
    /// A sound that can move.
    Object,
}

/// One track's part in the scene.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    /// Stable for a given file: built from the file's own identifiers
    /// (`adm:AO_1001:AC_00031001`, `thd:obj3`), never from a counter, so a
    /// session written by one person still finds the element for another.
    pub key: Arc<str>,
    pub name: Arc<str>,
    /// Which group the element is listed under (an ADM content or object).
    pub group: Arc<str>,
    /// 0-based PCM channel of the stream that carries it.
    pub track: u32,
    pub kind: ElementKind,
    pub coords: Coords,
    /// Sorted by `secs`. Never empty for a playable element.
    pub keyframes: Arc<[Keyframe]>,
    /// When the element sounds, in seconds; `None` is the whole file.
    pub active: Option<(f64, f64)>,
    /// Linear gain of the element as a whole (an ADM object's `gain`, 0 for
    /// a muted one).
    pub gain: f32,
}

impl Element {
    pub fn is_object(&self) -> bool {
        matches!(self.kind, ElementKind::Object)
    }

    pub fn is_lfe(&self) -> bool {
        matches!(
            self.kind,
            ElementKind::Bed {
                speaker: Some(SpeakerPos::Lfe),
                ..
            }
        )
    }

    /// Native position and gain at `secs`.
    pub fn sample_at(&self, secs: f64) -> Option<([f32; 3], f32)> {
        sample_keyframes(&self.keyframes, self.coords, secs)
    }

    /// Cartesian position at `secs`, whatever the element's own coordinates.
    pub fn cart_at(&self, secs: f64) -> Option<[f32; 3]> {
        let (pos, _) = self.sample_at(secs)?;
        Some(to_cart(self.coords, pos))
    }

    pub fn is_active_at(&self, secs: f64) -> bool {
        self.active
            .map(|(start, end)| secs >= start && secs < end)
            .unwrap_or(true)
    }
}

/// A position in `coords` as ADM Cartesian.
pub fn to_cart(coords: Coords, pos: [f32; 3]) -> [f32; 3] {
    match coords {
        Coords::Cartesian => pos,
        Coords::Polar => coords::polar_to_cart(pos[0], pos[1], pos[2]),
    }
}

/// An ADM Cartesian position in `coords`.
pub fn from_cart(coords: Coords, cart: [f32; 3]) -> [f32; 3] {
    match coords {
        Coords::Cartesian => cart,
        Coords::Polar => coords::cart_to_polar(cart[0], cart[1], cart[2]),
    }
}

fn lerp_pos(coords: Coords, a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let mut out = [0.0; 3];
    for i in 0..3 {
        let mut to = b[i];
        // An azimuth takes the short way round.
        if coords == Coords::Polar && i == 0 {
            to = a[0] + coords::wrap_azimuth(b[0] - a[0]);
        }
        out[i] = a[i] + (to - a[i]) * t;
    }
    if coords == Coords::Polar {
        out[0] = coords::wrap_azimuth(out[0]);
    }
    out
}

/// Position and gain of `keys` at `secs`: the first keyframe's value before
/// it, the last one's after it, a straight line inside a ramp.
pub fn sample_keyframes(keys: &[Keyframe], coords: Coords, secs: f64) -> Option<([f32; 3], f32)> {
    let first = keys.first()?;
    let next_index = keys.partition_point(|k| k.secs <= secs);
    if next_index == 0 {
        return Some((first.pos, first.gain));
    }
    let prev = &keys[next_index - 1];
    let Some(next) = keys.get(next_index) else {
        return Some((prev.pos, prev.gain));
    };
    let ramp_start = (next.secs - next.ramp_secs).max(prev.secs);
    let ramp = next.secs - ramp_start;
    if secs <= ramp_start || ramp <= 0.0 {
        return Some((prev.pos, prev.gain));
    }
    let t = ((secs - ramp_start) / ramp).clamp(0.0, 1.0) as f32;
    Some((
        lerp_pos(coords, prev.pos, next.pos, t),
        prev.gain + (next.gain - prev.gain) * t,
    ))
}

/// Drop keyframes that repeat the value before them. They cannot change
/// what is heard -- the value holds, ramp or not -- and an ADM master
/// written block-per-frame repeats most of its blocks.
pub fn dedup_keyframes(mut keys: Vec<Keyframe>) -> Vec<Keyframe> {
    // A ramp never starts before the keyframe ahead of it (see
    // `sample_keyframes`). Make that explicit first, or dropping a repeat
    // would let the next ramp start earlier than it did.
    for i in 1..keys.len() {
        let gap = (keys[i].secs - keys[i - 1].secs).max(0.0);
        keys[i].ramp_secs = keys[i].ramp_secs.min(gap);
    }
    let mut out: Vec<Keyframe> = Vec::with_capacity(keys.len());
    for key in keys {
        if out.last().is_some_and(|prev| prev.same_value(&key)) {
            continue;
        }
        out.push(key);
    }
    out
}

/// The shape of the PCM a scene's tracks index into. Saved with session
/// edits so a file that changed underneath them is noticed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceShape {
    pub tracks: u32,
    pub frames: u64,
    pub file_sr: u32,
}

impl SourceShape {
    pub fn duration_secs(&self) -> f64 {
        if self.file_sr == 0 {
            0.0
        } else {
            self.frames as f64 / self.file_sr as f64
        }
    }
}

/// A whole file's scene.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectScene {
    pub format: ObjectFormat,
    pub shape: SourceShape,
    /// The programme the elements come from (ADM files may hold several;
    /// the first is played).
    pub programme: Option<Arc<str>>,
    /// Beds first, then objects, each by track.
    pub elements: Vec<Element>,
    /// What could not be read or will not be rendered, for the Debug window
    /// and `--cli adm inspect`.
    pub diagnostics: Vec<String>,
}

impl ObjectScene {
    pub fn element(&self, key: &str) -> Option<&Element> {
        self.elements.iter().find(|element| &*element.key == key)
    }

    pub fn object_count(&self) -> usize {
        self.elements.iter().filter(|e| e.is_object()).count()
    }

    pub fn bed_count(&self) -> usize {
        self.elements.len() - self.object_count()
    }

    pub fn sort_elements(&mut self) {
        self.elements.sort_by(|a, b| {
            a.is_object()
                .cmp(&b.is_object())
                .then(a.track.cmp(&b.track))
                .then(
                    a.active
                        .map(|w| w.0)
                        .unwrap_or(0.0)
                        .total_cmp(&b.active.map(|w| w.0).unwrap_or(0.0)),
                )
        });
    }
}

/// Two keyframes closer than this are the same point in time: a drag that
/// lands on an existing keyframe updates it instead of stacking another.
pub const KEYFRAME_SNAP_SECS: f64 = 0.001;

/// `keys` with a keyframe at `secs` holding `pos`: the one already there
/// (within [`KEYFRAME_SNAP_SECS`]) moved, or a new one that glides in over
/// `ramp_secs` (never from before the keyframe ahead of it) and inherits the
/// gain heard there and the extras of the keyframe ahead. Returns the index
/// of the keyframe written.
pub fn put_keyframe(
    keys: &[Keyframe],
    coords: Coords,
    secs: f64,
    pos: [f32; 3],
    ramp_secs: f64,
) -> (Vec<Keyframe>, usize) {
    let mut out = keys.to_vec();
    if let Some(index) = out
        .iter()
        .position(|key| (key.secs - secs).abs() <= KEYFRAME_SNAP_SECS)
    {
        out[index].pos = pos;
        return (out, index);
    }
    let index = out.partition_point(|key| key.secs < secs);
    let gain = sample_keyframes(keys, coords, secs)
        .map(|(_, gain)| gain)
        .unwrap_or(1.0);
    let prev = index.checked_sub(1).map(|i| &out[i]);
    let ramp = prev
        .map(|prev| ramp_secs.min(secs - prev.secs))
        .unwrap_or(0.0)
        .max(0.0);
    let extras = prev.and_then(|prev| prev.extras.clone());
    out.insert(
        index,
        Keyframe {
            secs,
            ramp_secs: ramp,
            pos,
            gain,
            extras,
        },
    );
    if let Some(next) = out.get_mut(index + 1) {
        next.ramp_secs = next.ramp_secs.min(next.secs - secs).max(0.0);
    }
    (out, index)
}

/// `keys` with keyframe `index` moved to `secs` / `pos`. It stays between
/// its neighbours, so dragging can never reorder the curve.
pub fn move_keyframe(keys: &[Keyframe], index: usize, secs: f64, pos: [f32; 3]) -> Vec<Keyframe> {
    let mut out = keys.to_vec();
    let Some(_) = out.get(index) else {
        return out;
    };
    let lo = index
        .checked_sub(1)
        .map(|i| out[i].secs + KEYFRAME_SNAP_SECS)
        .unwrap_or(0.0);
    let hi = out
        .get(index + 1)
        .map(|next| next.secs - KEYFRAME_SNAP_SECS)
        .unwrap_or(f64::INFINITY);
    let secs = if lo <= hi {
        secs.clamp(lo, hi)
    } else {
        out[index].secs
    };
    let key = &mut out[index];
    key.secs = secs;
    key.pos = pos;
    if index > 0 {
        let gap = secs - out[index - 1].secs;
        out[index].ramp_secs = out[index].ramp_secs.min(gap).max(0.0);
    } else {
        out[index].ramp_secs = 0.0;
    }
    if let Some(next) = out.get_mut(index + 1) {
        next.ramp_secs = next.ramp_secs.min(next.secs - secs).max(0.0);
    }
    out
}

/// `keys` without keyframe `index`. The last keyframe is kept: an element
/// with none would have no position at all.
pub fn remove_keyframe(keys: &[Keyframe], index: usize) -> Vec<Keyframe> {
    let mut out = keys.to_vec();
    if out.len() > 1 && index < out.len() {
        out.remove(index);
        if index == 0 {
            out[0].ramp_secs = 0.0;
        }
    }
    out
}

/// One person's changes to a file's scene: for each element they touched,
/// the keyframes that replace the file's. Kept in the session, never in the
/// file (see `docs/SPATIAL_AUDIO_SPEC.md`); an untouched element has no
/// entry, so a session stays small however large the master is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneEdits {
    /// The shape of the PCM the edits were made against. A file that no
    /// longer has it keeps its edits, marked stale, rather than having them
    /// applied to tracks they were not made for.
    pub shape: Option<SourceShape>,
    pub elements: std::collections::BTreeMap<Arc<str>, Arc<[Keyframe]>>,
}

impl SceneEdits {
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Whether these edits were made against `scene`'s PCM.
    pub fn fits(&self, scene: &ObjectScene) -> bool {
        self.shape.is_none_or(|shape| shape == scene.shape)
    }

    /// The keyframes `element` plays: the edited ones, or the file's.
    pub fn keyframes_for<'a>(&'a self, element: &'a Element) -> &'a Arc<[Keyframe]> {
        self.elements
            .get(&element.key)
            .unwrap_or(&element.keyframes)
    }

    pub fn is_edited(&self, key: &str) -> bool {
        self.elements.contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(secs: f64, ramp: f64, x: f32) -> Keyframe {
        Keyframe {
            secs,
            ramp_secs: ramp,
            pos: [x, 0.0, 0.0],
            gain: 1.0,
            extras: None,
        }
    }

    #[test]
    fn a_value_holds_then_ramps_into_the_next_keyframe() {
        let keys = [key(0.0, 0.0, -1.0), key(2.0, 1.0, 1.0)];
        let at = |t| sample_keyframes(&keys, Coords::Cartesian, t).unwrap().0[0];
        assert_eq!(at(-1.0), -1.0, "before the first keyframe: its value");
        assert_eq!(at(0.5), -1.0, "held until the ramp starts at 1.0");
        assert!((at(1.5) - 0.0).abs() < 1e-6, "halfway through the ramp");
        assert_eq!(at(2.0), 1.0);
        assert_eq!(at(9.0), 1.0, "after the last keyframe: its value");
    }

    #[test]
    fn a_ramp_never_starts_before_the_previous_arrival() {
        // A 5 s ramp into a keyframe 1 s after the previous one starts at it.
        let keys = [key(1.0, 0.0, 0.0), key(2.0, 5.0, 1.0)];
        let (pos, _) = sample_keyframes(&keys, Coords::Cartesian, 1.5).unwrap();
        assert!((pos[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn polar_azimuths_take_the_short_way_round() {
        let keys = [
            Keyframe::at(0.0, [170.0, 0.0, 1.0]),
            Keyframe {
                ramp_secs: 1.0,
                ..Keyframe::at(1.0, [-170.0, 0.0, 1.0])
            },
        ];
        let (pos, _) = sample_keyframes(&keys, Coords::Polar, 0.5).unwrap();
        assert!(
            (pos[0].abs() - 180.0).abs() < 1e-3,
            "through the back: {}",
            pos[0]
        );
    }

    #[test]
    fn a_keyframe_is_put_between_its_neighbours_or_updates_one_there() {
        let keys = vec![key(0.0, 0.0, -1.0), key(2.0, 2.0, 1.0)];
        let (out, index) = put_keyframe(&keys, Coords::Cartesian, 1.0, [0.5, 0.0, 0.0], 5.0);
        assert_eq!(index, 1);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[1].ramp_secs, 1.0,
            "never ramps from before the keyframe ahead"
        );
        assert_eq!(
            out[2].ramp_secs, 1.0,
            "the next one now ramps from the new one"
        );
        let (again, index) = put_keyframe(&out, Coords::Cartesian, 1.0005, [0.0; 3], 0.0);
        assert_eq!(
            (again.len(), index),
            (3, 1),
            "same instant: updated, not stacked"
        );
        assert_eq!(again[1].pos, [0.0; 3]);
    }

    #[test]
    fn a_dragged_keyframe_stays_between_its_neighbours() {
        let keys = vec![key(0.0, 0.0, 0.0), key(1.0, 0.0, 0.0), key(2.0, 0.5, 0.0)];
        let moved = move_keyframe(&keys, 1, 5.0, [1.0, 0.0, 0.0]);
        assert!(moved[1].secs < 2.0 && moved[1].secs > 1.9);
        assert!(moved[2].ramp_secs <= moved[2].secs - moved[1].secs);
        let first = move_keyframe(&keys, 0, -3.0, [0.0; 3]);
        assert_eq!(first[0].secs, 0.0);
        assert_eq!(remove_keyframe(&keys, 1).len(), 2);
        assert_eq!(
            remove_keyframe(&[key(0.0, 0.0, 0.0)], 0).len(),
            1,
            "the last one stays"
        );
    }

    #[test]
    fn repeated_values_are_dropped_and_changes_kept() {
        let keys = vec![
            key(0.0, 0.0, 0.0),
            key(1.0, 0.0, 0.0),
            key(2.0, 0.5, 1.0),
            key(3.0, 0.0, 1.0),
            // A ramp longer than the gap to a repeat that is dropped.
            key(4.0, 0.0, 0.5),
            key(5.0, 0.0, 0.5),
            key(5.5, 2.0, -1.0),
        ];
        let kept = dedup_keyframes(keys.clone());
        assert_eq!(kept.len(), 4);
        for t in [0.0, 1.0, 1.6, 1.75, 2.0, 3.5, 4.2, 5.0, 5.2, 5.5, 6.0] {
            assert_eq!(
                sample_keyframes(&keys, Coords::Cartesian, t),
                sample_keyframes(&kept, Coords::Cartesian, t),
                "at {t}"
            );
        }
    }
}
