//! Panning: where a channel's sound goes on a layout's speakers.
//!
//! One set of rules for every panner in the app -- the editor's Panner tool
//! (live in the output callback while it previews, offline when it applies)
//! and the Multi Edits track pan. DSP and rules only: no UI, no app state.
//!
//! Two modes (`PanMode`):
//!
//! - **Balance**: the side a channel stands on is turned down as the pan
//!   moves the other way, along a quarter cosine, so hard left silences the
//!   right and nothing is ever boosted. The default for stereo, 2.1 and a
//!   mono file made stereo.
//! - **VBAP**: every channel's direction is turned by a 3-D rotation
//!   (`Rotation`: yaw, pitch, roll) and laid back onto the speakers by
//!   vector-base amplitude panning (`Speakers`): over triangles of three
//!   speakers when the layout has height speakers, over neighbouring pairs
//!   on the horizontal ring when it has none. The default once a layout has
//!   three speakers to turn around.
//!
//! The LFE is never part of a pan: it has no direction to turn, and nothing
//! is ever panned into it. Neither is a channel no speaker is named for.
//!
//! Coordinates: x right, y ahead, z up; azimuth in degrees, positive to the
//! right (`SpeakerPos::direction`), elevation positive up.

use std::sync::Arc;

use crate::audio_channels::SpeakerPos;

/// How far a pan of +-1 turns a sound when the output turns: half a
/// circle, so both ends face the back and the knob covers every direction.
pub const PAN_TURN_DEG: f32 = 180.0;

/// How long the live panner takes to slide from one setting to the next.
/// Short enough to follow a knob without lag, long enough that a jump in
/// gain is not heard as a click.
pub const PAN_RAMP_SECS: f64 = 0.005;

/// A VBAP pair this wide or wider has no stable inverse (at 180 degrees the
/// two speakers face each other); across such a gap the sound is cross-faded
/// by angle instead, still at constant power.
const VBAP_MAX_PAIR_DEG: f32 = 179.0;

/// A speaker this far from the horizon (degrees) makes the layout 3-D.
const HEIGHT_MIN_DEG: f32 = 1.0;

/// Gains below this are silence: they keep a matrix sparse.
const GAIN_EPS: f32 = 1.0e-6;

/// How far a VBAP gain may dip below zero (rounding at a triangle's edge)
/// and still count the direction as inside that triangle.
const INSIDE_EPS: f32 = 1.0e-4;

/// A direction this close to a speaker's (dot product) is that speaker's.
const AT_SPEAKER_DOT: f32 = 1.0 - 1.0e-7;

/// Balance: the centre leaves both sides as they are, and turning towards
/// one side lowers the other along a quarter cosine, so hard left is the left
/// channel alone and nothing is ever boosted.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    // f32's cos(pi/2) is a hair below zero; hard left is silence on the right.
    let fall = |amount: f32| (amount * std::f32::consts::FRAC_PI_2).cos().max(0.0);
    if pan > 0.0 {
        (fall(pan), 1.0)
    } else if pan < 0.0 {
        (1.0, fall(-pan))
    } else {
        (1.0, 1.0)
    }
}

/// How a pan moves sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PanMode {
    /// Left against right.
    Balance,
    /// A turn of the whole sound field, laid back onto the speakers by VBAP.
    Vbap,
}

impl PanMode {
    /// What a pan on `layout` does unless told otherwise: it turns once
    /// there are three speakers to turn around -- named ones that are not
    /// the LFE -- and balances below that (stereo, 2.1, mono made stereo).
    pub fn default_for(layout: &[Option<SpeakerPos>]) -> Self {
        let speakers = layout.iter().flatten().filter(|pos| !pos.is_lfe()).count();
        if speakers >= 3 {
            PanMode::Vbap
        } else {
            PanMode::Balance
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PanMode::Balance => "Balance",
            PanMode::Vbap => "3D VBAP",
        }
    }
}

/// A unit vector for a direction (x right, y ahead, z up).
pub fn dir_to_vec(azimuth_deg: f32, elevation_deg: f32) -> [f32; 3] {
    let (az_sin, az_cos) = azimuth_deg.to_radians().sin_cos();
    let (el_sin, el_cos) = elevation_deg.to_radians().sin_cos();
    [az_sin * el_cos, az_cos * el_cos, el_sin]
}

/// The direction of a vector, as (azimuth in (-180, 180], elevation).
pub fn vec_to_dir(v: [f32; 3]) -> (f32, f32) {
    let horizontal = (v[0] * v[0] + v[1] * v[1]).sqrt();
    let azimuth = if horizontal < 1.0e-9 {
        0.0
    } else {
        v[0].atan2(v[1]).to_degrees()
    };
    let azimuth = if azimuth <= -180.0 {
        azimuth + 360.0
    } else {
        azimuth
    };
    (azimuth, v[2].atan2(horizontal).to_degrees())
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn normalized(v: [f32; 3]) -> Option<[f32; 3]> {
    let len = dot(v, v).sqrt();
    (len > 1.0e-9).then(|| [v[0] / len, v[1] / len, v[2] / len])
}

/// A turn of the whole sound field, in degrees, applied roll first, then
/// pitch, then yaw. Yaw turns to the right, pitch lifts the front, roll
/// lowers the right side (clockwise as seen from behind).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rotation {
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
}

impl Rotation {
    /// A turn about the vertical axis alone.
    pub fn yaw(deg: f32) -> Self {
        Self {
            yaw: deg,
            ..Self::default()
        }
    }

    pub fn is_identity(&self) -> bool {
        self.yaw == 0.0 && self.pitch == 0.0 && self.roll == 0.0
    }

    /// Without its pitch and roll: what a layout with no height speakers
    /// can show.
    pub fn level(self) -> Self {
        Self::yaw(self.yaw)
    }

    pub fn apply(&self, v: [f32; 3]) -> [f32; 3] {
        let [mut x, mut y, mut z] = v;
        if self.roll != 0.0 {
            let (s, c) = self.roll.to_radians().sin_cos();
            (x, z) = (x * c + z * s, -x * s + z * c);
        }
        if self.pitch != 0.0 {
            let (s, c) = self.pitch.to_radians().sin_cos();
            (y, z) = (y * c - z * s, y * s + z * c);
        }
        if self.yaw != 0.0 {
            let (s, c) = self.yaw.to_radians().sin_cos();
            (x, y) = (x * c + y * s, -x * s + y * c);
        }
        [x, y, z]
    }
}

/// One VBAP triangle: three output channels and the rows of the inverse of
/// the matrix whose columns are their unit vectors, so the gains of a
/// direction `p` are `inv[i] . p`.
#[derive(Clone, Debug)]
struct Face {
    channels: [usize; 3],
    inv: [[f32; 3]; 3],
}

impl Face {
    fn gains(&self, p: [f32; 3]) -> [f32; 3] {
        [
            dot(self.inv[0], p),
            dot(self.inv[1], p),
            dot(self.inv[2], p),
        ]
    }
}

/// A layout's speakers as VBAP sees them: 3-D triangles over the convex
/// hull when the layout has height speakers, otherwise the horizontal ring
/// (pairwise 2-D VBAP). The LFE and unnamed channels are in neither.
#[derive(Clone, Debug, Default)]
pub struct Speakers {
    /// (output channel, unit vector) of every speaker that can be panned to.
    points: Vec<(usize, [f32; 3])>,
    faces: Vec<Face>,
    /// The 2-D rings, as (azimuth wrapped to [0, 360), channel), sorted:
    /// ear level, and above. Used when there are no faces.
    ear: Vec<(f32, usize)>,
    height: Vec<(f32, usize)>,
    /// Some speaker is below the horizon; otherwise a direction turned below
    /// it is laid onto the horizon.
    below: bool,
}

impl Speakers {
    pub fn of(layout: &[Option<SpeakerPos>]) -> Self {
        let mut speakers = Speakers::default();
        let mut three_d = false;
        for (index, pos) in layout.iter().enumerate() {
            let Some(pos) = pos.filter(|pos| !pos.is_lfe()) else {
                continue;
            };
            let (azimuth, elevation) = pos.direction(layout);
            three_d |= elevation.abs() > HEIGHT_MIN_DEG;
            speakers.below |= elevation < -HEIGHT_MIN_DEG;
            speakers
                .points
                .push((index, dir_to_vec(azimuth, elevation)));
            let wrapped = azimuth.rem_euclid(360.0);
            if pos.is_height() || elevation > HEIGHT_MIN_DEG {
                speakers.height.push((wrapped, index));
            } else {
                speakers.ear.push((wrapped, index));
            }
        }
        speakers.ear.sort_by(|a, b| a.0.total_cmp(&b.0));
        speakers.height.sort_by(|a, b| a.0.total_cmp(&b.0));
        if three_d {
            speakers.faces = hull_faces(&speakers.points);
        }
        speakers
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Whether directions are laid over triangles (the layout has height
    /// speakers) rather than the horizontal ring.
    pub fn is_3d(&self) -> bool {
        !self.faces.is_empty()
    }

    /// The part of a turn these speakers can show: all of it in 3-D, the
    /// yaw alone on a flat layout (a pitch or roll there would only fold
    /// channels onto each other).
    pub fn effective_rotation(&self, rotation: Rotation) -> Rotation {
        if self.is_3d() {
            rotation
        } else {
            rotation.level()
        }
    }

    /// Where a sound from `dir` is put: on the horizon when it points below
    /// a layout with nothing below (azimuth kept), otherwise where it
    /// points. Unit length.
    pub fn place(&self, dir: [f32; 3]) -> [f32; 3] {
        let Some(p) = normalized(dir) else {
            return [0.0, 1.0, 0.0];
        };
        if !self.below && p[2] < 0.0 {
            normalized([p[0], p[1], 0.0]).unwrap_or([0.0, 1.0, 0.0])
        } else {
            p
        }
    }

    /// Add `weight` times the gains of a sound from direction `dir` (a
    /// vector; need not be unit) into `gains`, one per output channel.
    /// Power-normalised: the squares of what is added sum to `weight^2`.
    pub fn add_gains(&self, dir: [f32; 3], weight: f32, gains: &mut [f32]) {
        let Some(mut p) = normalized(dir) else {
            return;
        };
        if self.points.is_empty() {
            return;
        }
        // At a speaker: that speaker alone, exactly.
        if let Some(&(channel, _)) = self
            .points
            .iter()
            .find(|(_, v)| dot(*v, p) >= AT_SPEAKER_DOT)
        {
            if let Some(slot) = gains.get_mut(channel) {
                *slot += weight;
            }
            return;
        }
        if self.faces.is_empty() {
            let (azimuth, elevation) = vec_to_dir(p);
            let above = elevation > HEIGHT_MIN_DEG && !self.height.is_empty();
            let ring = if above || self.ear.is_empty() {
                &self.height
            } else {
                &self.ear
            };
            for (channel, gain) in ring_gains(ring, azimuth) {
                if let Some(slot) = gains.get_mut(channel) {
                    *slot += weight * gain;
                }
            }
            return;
        }
        // Nothing below the horizon to play it: lay it onto the horizon.
        p = self.place(p);
        let mut acc: Vec<(usize, f32)> = Vec::with_capacity(6);
        let mut inside = 0usize;
        let mut best: Option<(f32, &Face, [f32; 3])> = None;
        for face in &self.faces {
            let g = face.gains(p);
            let min = g[0].min(g[1]).min(g[2]);
            if min >= -INSIDE_EPS {
                inside += 1;
                add_normalized(&mut acc, face.channels, g);
            } else if best.as_ref().is_none_or(|(m, _, _)| min > *m) {
                best = Some((min, face, g));
            }
        }
        if inside == 0 {
            // Outside every triangle (a layout that does not surround the
            // listener): the nearest one, its negative gains dropped.
            if let Some((_, face, g)) = best {
                add_normalized(&mut acc, face.channels, g);
            }
        }
        // Several triangles hold it when it lies on a shared edge or in a
        // flat face of four or more speakers: the mean of their gains,
        // brought back to unit power, is symmetric where any one is not.
        let power: f32 = acc.iter().map(|(_, g)| g * g).sum();
        if power <= 0.0 {
            return;
        }
        let norm = weight / power.sqrt();
        for (channel, g) in acc {
            let g = g * norm;
            if g.abs() > GAIN_EPS {
                if let Some(slot) = gains.get_mut(channel) {
                    *slot += g;
                }
            }
        }
    }
}

/// Add one triangle's gains, negatives dropped and power-normalised, into
/// a (channel, gain) accumulator.
fn add_normalized(acc: &mut Vec<(usize, f32)>, channels: [usize; 3], g: [f32; 3]) {
    let g = g.map(|v| v.max(0.0));
    let power = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
    if power <= 0.0 {
        return;
    }
    for (channel, value) in channels.into_iter().zip(g) {
        let value = value / power;
        match acc.iter_mut().find(|(c, _)| *c == channel) {
            Some((_, slot)) => *slot += value,
            None => acc.push((channel, value)),
        }
    }
}

/// The faces of the convex hull of `points` that VBAP can use: every
/// triangle of three speakers with all the others on one side of it, except
/// those whose plane passes through the listener (the horizontal ring of a
/// layout with nothing below) -- they have no inverse. Brute force, which
/// for at most a few dozen speakers is a few thousand cheap tests, done
/// once per layout.
fn hull_faces(points: &[(usize, [f32; 3])]) -> Vec<Face> {
    const SIDE_EPS: f32 = 1.0e-5;
    const DET_EPS: f32 = 1.0e-4;
    let n = points.len();
    let mut faces = Vec::new();
    for i in 0..n {
        for j in i + 1..n {
            for k in j + 1..n {
                let (a, b, c) = (points[i].1, points[j].1, points[k].1);
                let normal = cross(sub(b, a), sub(c, a));
                if dot(normal, normal) < SIDE_EPS * SIDE_EPS {
                    continue;
                }
                let offset = dot(normal, a);
                let (mut above, mut beneath) = (false, false);
                for (m, (_, p)) in points.iter().enumerate() {
                    if m == i || m == j || m == k {
                        continue;
                    }
                    let side = dot(normal, *p) - offset;
                    above |= side > SIDE_EPS;
                    beneath |= side < -SIDE_EPS;
                }
                if above && beneath {
                    continue;
                }
                let det = dot(a, cross(b, c));
                if det.abs() < DET_EPS {
                    continue;
                }
                let bc = cross(b, c);
                let ca = cross(c, a);
                let ab = cross(a, b);
                let scale = 1.0 / det;
                faces.push(Face {
                    channels: [points[i].0, points[j].0, points[k].0],
                    inv: [bc, ca, ab].map(|r| r.map(|v| v * scale)),
                });
            }
        }
    }
    faces
}

/// Pairwise 2-D VBAP on one ring of (azimuth in [0, 360), channel), sorted:
/// the two neighbouring speakers that bracket `azimuth` share it,
/// power-normalised, so a sound at a speaker plays from that speaker alone
/// and its level never moves as it turns.
fn ring_gains(ring: &[(f32, usize)], azimuth: f32) -> [(usize, f32); 2] {
    let Some(&(_, first)) = ring.first() else {
        return [(usize::MAX, 0.0); 2];
    };
    let theta = azimuth.rem_euclid(360.0);
    let n = ring.len();
    for i in 0..n {
        let (a, lo) = ring[i];
        let (b, hi) = ring[(i + 1) % n];
        let span = if n == 1 {
            0.0
        } else {
            (b - a).rem_euclid(360.0)
        };
        let span = if span == 0.0 && n > 1 && i + 1 == n {
            360.0
        } else {
            span
        };
        if span <= 0.0 {
            continue;
        }
        let offset = (theta - a).rem_euclid(360.0);
        if offset > span + 1e-4 {
            continue;
        }
        let offset = offset.min(span);
        let (g_lo, g_hi) = if span < VBAP_MAX_PAIR_DEG {
            let (span_r, offset_r) = (span.to_radians(), offset.to_radians());
            let g_lo = (span_r - offset_r).sin() / span_r.sin();
            let g_hi = offset_r.sin() / span_r.sin();
            let norm = (g_lo * g_lo + g_hi * g_hi).sqrt().max(f32::EPSILON);
            (g_lo / norm, g_hi / norm)
        } else {
            let t = offset / span * std::f32::consts::FRAC_PI_2;
            (t.cos(), t.sin())
        };
        return [(lo, g_lo), (hi, g_hi)];
    }
    // One speaker, or every speaker at one angle.
    [(first, 1.0), (usize::MAX, 0.0)]
}

/// Which side of the listener a speaker stands on, for Balance: -1 left,
/// +1 right, 0 on the centre line (front or back).
fn side_of(azimuth: f32) -> f32 {
    let azimuth = vec_to_dir(dir_to_vec(azimuth, 0.0)).0;
    if azimuth.abs() < 1.0e-3 || (azimuth.abs() - 180.0).abs() < 1.0e-3 {
        0.0
    } else {
        azimuth.signum()
    }
}

/// What a panner does: its mode and both modes' settings (each mode reads
/// its own).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanParams {
    pub mode: PanMode,
    /// Balance, -1 (left) to +1 (right).
    pub balance: f32,
    pub rotation: Rotation,
}

impl PanParams {
    /// Whether the settings leave every channel where it is.
    pub fn is_neutral(&self) -> bool {
        match self.mode {
            PanMode::Balance => self.balance == 0.0,
            PanMode::Vbap => self.rotation.is_identity(),
        }
    }
}

/// Where every input channel goes: for each output channel, the inputs that
/// feed it and how much. Built once per setting on the UI thread; applied
/// per frame by the output callback (through `PanState`) and to whole
/// channels by `apply_offline` -- one matrix, so what is applied is what
/// was heard.
#[derive(Clone, Debug, PartialEq)]
pub struct PanMatrix {
    in_channels: usize,
    out_layout: Vec<Option<SpeakerPos>>,
    rows: Vec<Vec<(u8, f32)>>,
}

impl PanMatrix {
    /// Every channel to itself.
    pub fn identity(layout: &[Option<SpeakerPos>]) -> Self {
        Self {
            in_channels: layout.len(),
            out_layout: layout.to_vec(),
            rows: (0..layout.len()).map(|c| vec![(c as u8, 1.0)]).collect(),
        }
    }

    /// The panner on a file of `layout`: the same layout out, except that a
    /// mono file comes out stereo. `moves(channel)` says which input
    /// channels the pan moves (the editor's channel view); the others, the
    /// LFE and unnamed channels stay where they are.
    pub fn for_layout(
        layout: &[Option<SpeakerPos>],
        params: &PanParams,
        moves: impl Fn(usize) -> bool,
    ) -> Self {
        let n = layout.len().min(crate::audio_channels::MAX_SOURCE_CHANNELS);
        let layout = &layout[..n];
        if n == 1 {
            return Self::mono_to_stereo(params);
        }
        if params.is_neutral() || n == 0 {
            return Self::identity(layout);
        }
        let speakers = match params.mode {
            PanMode::Vbap => Speakers::of(layout),
            PanMode::Balance => Speakers::default(),
        };
        let rotation = speakers.effective_rotation(params.rotation);
        let mut rows: Vec<Vec<(u8, f32)>> = vec![Vec::new(); n];
        let mut gains = vec![0.0f32; n];
        for (input, pos) in layout.iter().enumerate() {
            let movable = pos.filter(|pos| !pos.is_lfe() && moves(input));
            let Some(pos) = movable else {
                rows[input].push((input as u8, 1.0));
                continue;
            };
            let (azimuth, elevation) = pos.direction(layout);
            match params.mode {
                PanMode::Balance => {
                    let (gl, gr) = pan_gains(params.balance);
                    let gain = match side_of(azimuth) {
                        s if s < 0.0 => gl,
                        s if s > 0.0 => gr,
                        _ => 1.0,
                    };
                    if gain > GAIN_EPS {
                        rows[input].push((input as u8, gain));
                    }
                }
                PanMode::Vbap => {
                    if speakers.is_empty() {
                        rows[input].push((input as u8, 1.0));
                        continue;
                    }
                    gains.iter_mut().for_each(|g| *g = 0.0);
                    let dir = rotation.apply(dir_to_vec(azimuth, elevation));
                    speakers.add_gains(dir, 1.0, &mut gains);
                    for (output, &gain) in gains.iter().enumerate() {
                        if gain > GAIN_EPS {
                            rows[output].push((input as u8, gain));
                        }
                    }
                }
            }
        }
        Self {
            in_channels: n,
            out_layout: layout.to_vec(),
            rows,
        }
    }

    /// A mono file panned onto a stereo pair. Balance sends it to both sides
    /// at full level and then balances, so in the centre it sounds as the
    /// mono file did on a stereo device; VBAP turns it from straight ahead
    /// over the pair at constant power (-3 dB each side in the centre).
    fn mono_to_stereo(params: &PanParams) -> Self {
        let stereo = vec![Some(SpeakerPos::Fl), Some(SpeakerPos::Fr)];
        let (gl, gr) = match params.mode {
            PanMode::Balance => pan_gains(params.balance),
            PanMode::Vbap => {
                let mut gains = [0.0f32; 2];
                let dir = Rotation::yaw(params.rotation.yaw).apply([0.0, 1.0, 0.0]);
                Speakers::of(&stereo).add_gains(dir, 1.0, &mut gains);
                (gains[0], gains[1])
            }
        };
        let row = |gain: f32| {
            if gain > GAIN_EPS {
                vec![(0u8, gain)]
            } else {
                Vec::new()
            }
        };
        Self {
            in_channels: 1,
            out_layout: stereo,
            rows: vec![row(gl), row(gr)],
        }
    }

    pub fn in_channels(&self) -> usize {
        self.in_channels
    }

    pub fn out_channels(&self) -> usize {
        self.rows.len()
    }

    /// The speakers of the output channels.
    pub fn out_layout(&self) -> &[Option<SpeakerPos>] {
        &self.out_layout
    }

    /// The inputs that feed output `output`, with their gains.
    pub fn row(&self, output: usize) -> &[(u8, f32)] {
        self.rows.get(output).map(Vec::as_slice).unwrap_or(&[])
    }

    /// How much of input `input` reaches output `output`.
    pub fn gain(&self, input: usize, output: usize) -> f32 {
        self.row(output)
            .iter()
            .filter(|(i, _)| *i as usize == input)
            .map(|(_, g)| *g)
            .sum()
    }

    /// Every channel to itself, unchanged.
    pub fn is_identity(&self) -> bool {
        self.in_channels == self.rows.len()
            && self
                .rows
                .iter()
                .enumerate()
                .all(|(output, row)| row.len() == 1 && row[0] == (output as u8, 1.0))
    }

    /// The matrix applied to whole channels: what Apply writes.
    pub fn apply_offline(&self, channels: &[Vec<f32>]) -> Vec<Vec<f32>> {
        if self.is_identity() {
            return channels.to_vec();
        }
        let frames = channels.iter().map(Vec::len).max().unwrap_or(0);
        self.rows
            .iter()
            .map(|row| {
                let mut out = vec![0.0f32; frames];
                for &(input, gain) in row {
                    if let Some(src) = channels.get(input as usize) {
                        for (d, s) in out.iter_mut().zip(src) {
                            *d += *s * gain;
                        }
                    }
                }
                out
            })
            .collect()
    }
}

/// The live panner's state in the output callback: the gains it is playing
/// now, and the slide towards a new matrix. A new matrix (by identity) is
/// reached over `ramp_frames` from wherever the gains are, so a knob dragged
/// while playing never steps, even when the next matrix arrives mid-slide.
/// When the matrix goes away the gains slide back to every channel on
/// itself, then the state lets go.
#[derive(Default)]
pub struct PanState {
    target: Option<Arc<PanMatrix>>,
    shape: (usize, usize),
    gains: Vec<f32>,
    goal: Vec<f32>,
    step: Vec<f32>,
    /// (input, output) pairs with a gain now or at the goal.
    active: Vec<(u16, u16)>,
    remaining: usize,
    /// Sliding back to the identity after the matrix was removed.
    releasing: bool,
}

impl PanState {
    pub fn reset(&mut self) {
        if self.target.is_none() && !self.releasing && self.active.is_empty() {
            return;
        }
        self.target = None;
        self.shape = (0, 0);
        self.gains.clear();
        self.goal.clear();
        self.step.clear();
        self.active.clear();
        self.remaining = 0;
        self.releasing = false;
    }

    /// Whether this frame goes through the panner: a matrix for this many
    /// input channels is installed, or one was and its gains can slide back
    /// to every channel on itself (it kept the channel count) or still are.
    /// A matrix that changed the count (mono made stereo) cannot slide back
    /// into fewer channels; it lets go at once.
    pub fn engaged(&self, matrix: Option<&Arc<PanMatrix>>, in_channels: usize) -> bool {
        let (ins, outs) = self.shape;
        let releasable = (self.releasing || self.target.is_some()) && ins == outs;
        matrix.is_some_and(|m| m.in_channels() == in_channels) || (releasable && ins == in_channels)
    }

    /// Pan one frame of `input` into `out` (sized to the matrix's output).
    /// `matrix` is what the engine holds now: `None` once removed.
    pub fn process(
        &mut self,
        matrix: Option<&Arc<PanMatrix>>,
        ramp_frames: usize,
        input: &[f32],
        out: &mut [f32],
    ) {
        match matrix {
            Some(matrix) => {
                if !self.target.as_ref().is_some_and(|t| Arc::ptr_eq(t, matrix)) {
                    self.retarget(matrix.clone(), ramp_frames);
                }
            }
            None => {
                if self.target.is_some() && !self.releasing {
                    self.release(ramp_frames);
                }
            }
        }
        if self.shape == (0, 0) {
            // Let go, possibly earlier in this very block: every channel on
            // itself.
            let n = input.len().min(out.len());
            out[..n].copy_from_slice(&input[..n]);
            return;
        }
        let (_, outs) = self.shape;
        for v in out.iter_mut().take(outs) {
            *v = 0.0;
        }
        for &(i, o) in &self.active {
            let (i, o) = (i as usize, o as usize);
            if let (Some(x), Some(y)) = (input.get(i), out.get_mut(o)) {
                *y += self.gains[i * outs + o] * *x;
            }
        }
        if self.remaining > 0 {
            for &(i, o) in &self.active {
                let k = i as usize * outs + o as usize;
                self.gains[k] += self.step[k];
            }
            self.remaining -= 1;
            if self.remaining == 0 {
                self.settle();
            }
        }
    }

    fn retarget(&mut self, matrix: Arc<PanMatrix>, ramp_frames: usize) {
        let shape = (matrix.in_channels(), matrix.out_channels());
        let jump = shape != self.shape || self.active.is_empty() && !self.releasing;
        self.shape = shape;
        self.releasing = false;
        let len = shape.0 * shape.1;
        self.goal.clear();
        self.goal.resize(len, 0.0);
        for (o, row) in matrix.rows.iter().enumerate() {
            for &(i, g) in row {
                self.goal[i as usize * shape.1 + o] += g;
            }
        }
        self.target = Some(matrix);
        if jump || ramp_frames == 0 {
            // First use, or a different shape: nothing to slide from.
            self.gains.clear();
            self.gains.extend_from_slice(&self.goal);
            self.settle();
            return;
        }
        self.slide(ramp_frames);
    }

    /// Slide back to every channel on itself; only possible when the
    /// matrix kept the channel count.
    fn release(&mut self, ramp_frames: usize) {
        let (ins, outs) = self.shape;
        if self.target.is_none() || ins != outs || ramp_frames == 0 {
            self.reset();
            return;
        }
        self.target = None;
        self.releasing = true;
        self.goal.iter_mut().for_each(|g| *g = 0.0);
        for c in 0..ins {
            self.goal[c * outs + c] = 1.0;
        }
        self.slide(ramp_frames);
    }

    fn slide(&mut self, ramp_frames: usize) {
        let inv = 1.0 / ramp_frames as f32;
        self.step.clear();
        self.step.extend(
            self.goal
                .iter()
                .zip(&self.gains)
                .map(|(g, c)| (g - c) * inv),
        );
        self.remaining = ramp_frames;
        self.collect_active(true);
    }

    /// Arrived: exactly the goal, and only its pairs.
    fn settle(&mut self) {
        self.gains.clear();
        self.gains.extend_from_slice(&self.goal);
        self.remaining = 0;
        if self.releasing {
            self.reset();
            return;
        }
        self.collect_active(false);
    }

    fn collect_active(&mut self, with_current: bool) {
        let (ins, outs) = self.shape;
        self.active.clear();
        for i in 0..ins {
            for o in 0..outs {
                let k = i * outs + o;
                let live = self.goal[k] != 0.0 || (with_current && self.gains[k] != 0.0);
                if live {
                    self.active.push((i as u16, o as u16));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_channels::standard_layout_vec;

    fn layout(channels: usize) -> Vec<Option<SpeakerPos>> {
        standard_layout_vec(channels).expect("a standard layout")
    }

    fn index(layout: &[Option<SpeakerPos>], pos: SpeakerPos) -> usize {
        layout
            .iter()
            .position(|p| *p == Some(pos))
            .expect("speaker")
    }

    fn gains_for(speakers: &Speakers, channels: usize, az: f32, el: f32) -> Vec<f32> {
        let mut gains = vec![0.0; channels];
        speakers.add_gains(dir_to_vec(az, el), 1.0, &mut gains);
        gains
    }

    fn power(gains: &[f32]) -> f32 {
        gains.iter().map(|g| g * g).sum()
    }

    fn vbap(rotation: Rotation) -> PanParams {
        PanParams {
            mode: PanMode::Vbap,
            balance: 0.0,
            rotation,
        }
    }

    #[test]
    fn the_balance_law_never_boosts_and_silences_the_far_side() {
        assert_eq!(pan_gains(0.0), (1.0, 1.0));
        assert_eq!(pan_gains(-1.0), (1.0, 0.0));
        assert_eq!(pan_gains(1.0), (0.0, 1.0));
        let (l, r) = pan_gains(0.5);
        assert!(l < 1.0 && l > 0.0 && r == 1.0);
    }

    #[test]
    fn the_default_mode_turns_once_three_speakers_can_be_turned_around() {
        assert_eq!(PanMode::default_for(&layout(1)), PanMode::Balance);
        assert_eq!(PanMode::default_for(&layout(2)), PanMode::Balance);
        let two_one = vec![
            Some(SpeakerPos::Fl),
            Some(SpeakerPos::Fr),
            Some(SpeakerPos::Lfe),
        ];
        assert_eq!(PanMode::default_for(&two_one), PanMode::Balance);
        assert_eq!(PanMode::default_for(&layout(6)), PanMode::Vbap);
    }

    #[test]
    fn directions_and_vectors_round_trip() {
        for (az, el) in [
            (0.0f32, 0.0f32),
            (30.0, 0.0),
            (-110.0, 0.0),
            (135.0, 45.0),
            (-45.0, 45.0),
        ] {
            let (a, e) = vec_to_dir(dir_to_vec(az, el));
            assert!(
                (a - az).abs() < 1e-3 && (e - el).abs() < 1e-3,
                "{az} {el} -> {a} {e}"
            );
        }
    }

    #[test]
    fn the_rotation_axes_turn_the_way_they_are_named() {
        let front = [0.0, 1.0, 0.0];
        let (az, _) = vec_to_dir(Rotation::yaw(90.0).apply(front));
        assert!((az - 90.0).abs() < 1e-3, "yaw right: {az}");
        let up = Rotation {
            pitch: 90.0,
            ..Rotation::default()
        }
        .apply(front);
        assert!((up[2] - 1.0).abs() < 1e-5, "pitch lifts the front: {up:?}");
        let right = Rotation {
            roll: 90.0,
            ..Rotation::default()
        }
        .apply([1.0, 0.0, 0.0]);
        assert!(
            (right[2] + 1.0).abs() < 1e-5,
            "roll lowers the right: {right:?}"
        );
    }

    #[test]
    fn every_speaker_of_every_layout_plays_alone_where_it_stands() {
        for channels in [2usize, 3, 4, 6, 8, 10, 12] {
            let layout = layout(channels);
            let speakers = Speakers::of(&layout);
            for (ch, pos) in layout.iter().enumerate() {
                let Some(pos) = pos.filter(|p| !p.is_lfe()) else {
                    continue;
                };
                let (az, el) = pos.direction(&layout);
                let gains = gains_for(&speakers, channels, az, el);
                assert_eq!(gains[ch], 1.0, "{channels} ch, {pos:?}: {gains:?}");
                assert!((power(&gains) - 1.0).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn seven_one_four_is_three_dimensional_and_keeps_the_level_everywhere() {
        let layout = layout(12);
        let speakers = Speakers::of(&layout);
        assert!(speakers.is_3d());
        assert!(!Speakers::of(&self::layout(8)).is_3d(), "7.1 is flat");
        let lfe = index(&layout, SpeakerPos::Lfe);
        for az in (-180..180).step_by(15) {
            for el in [-30.0f32, 0.0, 20.0, 45.0, 70.0, 90.0] {
                let gains = gains_for(&speakers, 12, az as f32, el);
                assert!((power(&gains) - 1.0).abs() < 1e-4, "{az} {el}: {gains:?}");
                assert_eq!(gains[lfe], 0.0);
                assert!(gains.iter().all(|g| *g >= 0.0));
            }
        }
    }

    #[test]
    fn the_zenith_splits_evenly_over_the_four_tops() {
        let layout = layout(12);
        let gains = gains_for(&Speakers::of(&layout), 12, 0.0, 90.0);
        for pos in [
            SpeakerPos::Tfl,
            SpeakerPos::Tfr,
            SpeakerPos::Tbl,
            SpeakerPos::Tbr,
        ] {
            assert!((gains[index(&layout, pos)] - 0.5).abs() < 1e-4, "{gains:?}");
        }
    }

    #[test]
    fn below_the_horizon_is_laid_onto_it() {
        let layout = layout(12);
        let speakers = Speakers::of(&layout);
        let low = gains_for(&speakers, 12, 60.0, -40.0);
        let level = gains_for(&speakers, 12, 60.0, 0.0);
        for (a, b) in low.iter().zip(&level) {
            assert!((a - b).abs() < 1e-4, "{low:?} vs {level:?}");
        }
    }

    #[test]
    fn a_neutral_panner_is_the_identity_and_mono_becomes_stereo() {
        let params = vbap(Rotation::default());
        assert!(PanMatrix::for_layout(&layout(12), &params, |_| true).is_identity());
        let balance = PanParams {
            mode: PanMode::Balance,
            ..params
        };
        let mono = PanMatrix::for_layout(&layout(1), &balance, |_| true);
        assert_eq!((mono.in_channels(), mono.out_channels()), (1, 2));
        assert_eq!(
            (mono.gain(0, 0), mono.gain(0, 1)),
            (1.0, 1.0),
            "the centre as before"
        );
        let vbap_mono = PanMatrix::for_layout(&layout(1), &params, |_| true);
        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (vbap_mono.gain(0, 0) - half).abs() < 1e-5,
            "constant power: -3 dB"
        );
        let hard_left = PanParams {
            balance: -1.0,
            ..balance
        };
        let left = PanMatrix::for_layout(&layout(1), &hard_left, |_| true);
        assert_eq!((left.gain(0, 0), left.gain(0, 1)), (1.0, 0.0));
    }

    #[test]
    fn a_quarter_yaw_takes_the_centre_to_the_right_side_and_leaves_the_lfe() {
        let layout = layout(12);
        let m = PanMatrix::for_layout(&layout, &vbap(Rotation::yaw(90.0)), |_| true);
        let (fc, sr, lfe) = (
            index(&layout, SpeakerPos::Fc),
            index(&layout, SpeakerPos::Sr),
            index(&layout, SpeakerPos::Lfe),
        );
        assert_eq!(m.gain(fc, sr), 1.0);
        assert_eq!(m.gain(lfe, lfe), 1.0);
        for input in 0..12 {
            if input != lfe {
                assert_eq!(m.gain(input, lfe), 0.0, "nothing is panned into the LFE");
            }
        }
    }

    #[test]
    fn pitching_up_takes_the_front_overhead_and_the_back_to_the_horizon() {
        let layout = layout(12);
        let pitch = Rotation {
            pitch: 90.0,
            ..Rotation::default()
        };
        let m = PanMatrix::for_layout(&layout, &vbap(pitch), |_| true);
        let fc = index(&layout, SpeakerPos::Fc);
        let tops: f32 = [
            SpeakerPos::Tfl,
            SpeakerPos::Tfr,
            SpeakerPos::Tbl,
            SpeakerPos::Tbr,
        ]
        .into_iter()
        .map(|p| m.gain(fc, index(&layout, p)))
        .sum();
        assert!(tops > 1.9, "the centre is overhead: {tops}");
        // The back channels turn below the floor: laid onto the horizon.
        let bl = index(&layout, SpeakerPos::Bl);
        let ear: f32 = (0..8).map(|o| m.gain(bl, o).powi(2)).sum();
        assert!((ear - 1.0).abs() < 1e-4, "{ear}");
    }

    #[test]
    fn a_flat_layout_ignores_pitch_and_roll() {
        let layout = layout(6);
        let tilted = Rotation {
            yaw: 30.0,
            pitch: 40.0,
            roll: 20.0,
        };
        let a = PanMatrix::for_layout(&layout, &vbap(tilted), |_| true);
        let b = PanMatrix::for_layout(&layout, &vbap(Rotation::yaw(30.0)), |_| true);
        assert_eq!(a, b);
    }

    #[test]
    fn balance_turns_down_the_far_side_of_a_surround_layout() {
        let layout = layout(6);
        let params = PanParams {
            mode: PanMode::Balance,
            balance: -1.0,
            rotation: Rotation::default(),
        };
        let m = PanMatrix::for_layout(&layout, &params, |_| true);
        for (pos, gain) in [
            (SpeakerPos::Fl, 1.0),
            (SpeakerPos::Fr, 0.0),
            (SpeakerPos::Fc, 1.0),
            (SpeakerPos::Bl, 1.0),
            (SpeakerPos::Br, 0.0),
        ] {
            let c = index(&layout, pos);
            assert_eq!(m.gain(c, c), gain, "{pos:?}");
        }
    }

    #[test]
    fn channels_out_of_view_stay_put() {
        let layout = layout(6);
        let fl = index(&layout, SpeakerPos::Fl);
        let m = PanMatrix::for_layout(&layout, &vbap(Rotation::yaw(90.0)), |c| c == fl);
        let fr = index(&layout, SpeakerPos::Fr);
        assert_eq!(m.gain(fr, fr), 1.0);
        assert_eq!(m.gain(fl, fl), 0.0, "the one in view moved");
    }

    #[test]
    fn offline_and_live_give_the_same_samples() {
        let layout = layout(6);
        let m = Arc::new(PanMatrix::for_layout(
            &layout,
            &vbap(Rotation::yaw(50.0)),
            |_| true,
        ));
        let channels: Vec<Vec<f32>> = (0..6).map(|c| vec![0.1 * (c + 1) as f32; 4]).collect();
        let offline = m.apply_offline(&channels);
        let mut state = PanState::default();
        let mut out = vec![0.0; 6];
        let input: Vec<f32> = channels.iter().map(|c| c[0]).collect();
        state.process(Some(&m), 64, &input, &mut out);
        for (o, channel) in offline.iter().enumerate() {
            assert!((channel[0] - out[o]).abs() < 1e-6, "channel {o}");
        }
    }

    #[test]
    fn the_live_panner_slides_without_steps_even_when_retargeted_mid_slide() {
        let layout = layout(2);
        let balance = |b: f32| {
            Arc::new(PanMatrix::for_layout(
                &layout,
                &PanParams {
                    mode: PanMode::Balance,
                    balance: b,
                    rotation: Rotation::default(),
                },
                |_| true,
            ))
        };
        let (a, b, c) = (balance(0.0), balance(-1.0), balance(1.0));
        let mut state = PanState::default();
        let mut out = [0.0f32; 2];
        let input = [1.0f32, 1.0];
        let mut right = Vec::new();
        state.process(Some(&a), 100, &input, &mut out);
        right.push(out[1]);
        for frame in 0..310 {
            let m = if frame < 50 {
                &b
            } else if frame < 200 {
                &c
            } else {
                &b
            };
            state.process(Some(m), 100, &input, &mut out);
            right.push(out[1]);
        }
        let jump = right
            .windows(2)
            .fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(jump <= 0.0101, "steps by {jump}");
        assert_eq!(*right.last().unwrap(), 0.0, "arrived at hard left");
        // Removed: back to every channel on itself, then let go (the caller
        // stops passing frames through once it is not engaged).
        let mut released = Vec::new();
        while state.engaged(None, 2) {
            state.process(None, 100, &input, &mut out);
            released.push(out[1]);
        }
        assert_eq!(released.len(), 100);
        let jump = released
            .windows(2)
            .fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(
            jump <= 0.0101 && released[99] > 0.98,
            "{jump} {}",
            released[99]
        );
    }
}
