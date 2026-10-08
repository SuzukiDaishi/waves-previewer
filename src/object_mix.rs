//! Object audio, played: every element's track panned onto the 7.1.4
//! virtual bed inside the output callback.
//!
//! Nothing is rendered ahead. The callback reads each track of the source
//! (a memory-mapped ADM master, or the decoded temp file of a TrueHD stream)
//! at the playhead, works out where each element is at that moment, and
//! mixes the tracks into the twelve bed channels. From there the bed goes
//! the way any 7.1.4 source goes: through the speaker matrix to the device,
//! or through the HRTF to headphones, and into the loudness tap. So an edit
//! is heard on the next block, memory does not grow with the length of the
//! programme, and a 128-track source needs no wider matrix -- tracks are
//! read by index, and only the bed's twelve channels reach the matrix.
//!
//! Gains are worked out every [`RETARGET_FRAMES`] at the actual playhead
//! (so a seek, a loop or a change of speed is followed), then ramped
//! linearly across the frames in between, so a moving object never steps.
//! DSP only: no UI, no disk. The app builds an [`ObjectMix`] from a scene
//! (`app::spatial_ops`); the callback owns an [`ObjectMixState`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::spatial::panner::{bed_channel_gains, pan_allocentric, Gains, BED_CHANNELS, GAIN_SLOTS};
use crate::spatial::scene::{
    to_cart, Coords, Element, ElementKind, Keyframe, ObjectScene, SceneEdits,
};

/// Frames between two evaluations of every element's position. 64 frames is
/// 1.3 ms at 48 kHz: far finer than any object moves, and it keeps the
/// position lookups (a binary search per element) off the per-frame path.
pub const RETARGET_FRAMES: usize = 64;

/// A keyframe in the form the callback reads: always Cartesian.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixKey {
    pub secs: f64,
    pub ramp_secs: f64,
    pub cart: [f32; 3],
    pub gain: f32,
}

/// How one element reaches the bed.
#[derive(Clone, Debug)]
pub enum MixRoute {
    /// A bed channel: fixed gains, worked out once.
    Fixed(Gains),
    /// An object: panned wherever its keyframes put it.
    Panned,
}

#[derive(Clone, Debug)]
pub struct MixElement {
    pub track: usize,
    pub route: MixRoute,
    pub keys: Arc<[MixKey]>,
    /// When it sounds, in seconds; `None` is the whole source.
    pub active: Option<(f64, f64)>,
    /// Linear gain of the element as a whole.
    pub gain: f32,
    /// False when muted, or when another element is soloed.
    pub audible: bool,
}

/// What a mix was built for. The callback plays it only against a source of
/// exactly this shape (and path, for a streamed file), so a mix left behind
/// by the previous file can never be applied to the next one.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectMixSource {
    /// The streamed file the tracks are read from; `None` for samples held
    /// in memory.
    pub stream_path: Option<PathBuf>,
    pub tracks: usize,
    pub frames: usize,
    pub file_sr: u32,
}

/// Everything the callback needs to play a scene.
#[derive(Clone, Debug)]
pub struct ObjectMix {
    pub source: ObjectMixSource,
    /// The scene is still being read: the callback holds the transport
    /// silent, without advancing, rather than play the raw tracks.
    pub pending: bool,
    pub elements: Vec<MixElement>,
}

impl ObjectMix {
    /// A mix that holds the transport until the scene is ready.
    pub fn pending(source: ObjectMixSource) -> Self {
        Self {
            source,
            pending: true,
            elements: Vec::new(),
        }
    }

    pub fn applies_to(&self, stream_path: Option<&Path>, tracks: usize, frames: usize) -> bool {
        self.source.tracks == tracks
            && self.source.frames == frames
            && self.source.stream_path.as_deref() == stream_path
    }
}

/// An element's keyframes as Cartesian [`MixKey`]s.
pub fn mix_keys(coords: Coords, keys: &[Keyframe]) -> Arc<[MixKey]> {
    keys.iter()
        .map(|key| MixKey {
            secs: key.secs,
            ramp_secs: key.ramp_secs,
            cart: to_cart(coords, key.pos),
            gain: key.gain,
        })
        .collect()
}

/// How `element` reaches the bed.
pub fn mix_route(element: &Element) -> MixRoute {
    match &element.kind {
        ElementKind::Bed { speaker, .. } => {
            let nominal = element
                .keyframes
                .first()
                .map(|key| to_cart(element.coords, key.pos))
                .unwrap_or([0.0, 1.0, 0.0]);
            MixRoute::Fixed(bed_channel_gains(*speaker, nominal))
        }
        ElementKind::Object => MixRoute::Panned,
    }
}

/// Each element's [`MixKey`]s, kept until its keyframes change. A drag
/// rebuilds the mix every frame; converting every element's keyframes each
/// time would cost the size of the whole scene, where only one moved. Holds
/// the keyframes it converted, so a match is by identity and an address
/// can never be reused under it.
#[derive(Default)]
pub struct MixKeyCache {
    entries: std::collections::HashMap<Arc<str>, (Arc<[Keyframe]>, Arc<[MixKey]>)>,
}

impl MixKeyCache {
    pub fn keys(&mut self, element: &Element, keyframes: &Arc<[Keyframe]>) -> Arc<[MixKey]> {
        if let Some((converted, keys)) = self.entries.get(&element.key) {
            if Arc::ptr_eq(converted, keyframes) {
                return Arc::clone(keys);
            }
        }
        let keys = mix_keys(element.coords, keyframes);
        self.entries.insert(
            Arc::clone(&element.key),
            (Arc::clone(keyframes), Arc::clone(&keys)),
        );
        keys
    }
}

impl ObjectMix {
    /// A mix of `scene`, with `edits` in place of the file's keyframes where
    /// there are any, and only the elements `audible` says are heard.
    pub fn from_scene(
        scene: &ObjectScene,
        edits: Option<&SceneEdits>,
        audible: impl Fn(&Element) -> bool,
        cache: &mut MixKeyCache,
        source: ObjectMixSource,
    ) -> Self {
        let edits = edits.filter(|edits| edits.fits(scene));
        let elements = scene
            .elements
            .iter()
            .map(|element| {
                let keyframes = edits
                    .map(|edits| edits.keyframes_for(element))
                    .unwrap_or(&element.keyframes);
                MixElement {
                    track: element.track as usize,
                    route: mix_route(element),
                    keys: cache.keys(element, keyframes),
                    active: element.active,
                    gain: element.gain,
                    audible: audible(element),
                }
            })
            .collect();
        Self {
            source,
            pending: false,
            elements,
        }
    }
}

/// Position and gain at `secs` (see `spatial::scene::sample_keyframes`,
/// which this mirrors in Cartesian without allocating).
fn sample(keys: &[MixKey], secs: f64) -> Option<([f32; 3], f32)> {
    let first = keys.first()?;
    let next_index = keys.partition_point(|k| k.secs <= secs);
    if next_index == 0 {
        return Some((first.cart, first.gain));
    }
    let prev = &keys[next_index - 1];
    let Some(next) = keys.get(next_index) else {
        return Some((prev.cart, prev.gain));
    };
    let ramp_start = (next.secs - next.ramp_secs).max(prev.secs);
    let ramp = next.secs - ramp_start;
    if secs <= ramp_start || ramp <= 0.0 {
        return Some((prev.cart, prev.gain));
    }
    let t = ((secs - ramp_start) / ramp).clamp(0.0, 1.0) as f32;
    let mut cart = [0.0; 3];
    for (i, value) in cart.iter_mut().enumerate() {
        *value = prev.cart[i] + (next.cart[i] - prev.cart[i]) * t;
    }
    Some((cart, prev.gain + (next.gain - prev.gain) * t))
}

/// Where `element` sends its track at `secs`.
pub fn element_gains(element: &MixElement, secs: f64) -> Gains {
    let silent = [0.0; GAIN_SLOTS];
    if !element.audible
        || element.gain == 0.0
        || element
            .active
            .is_some_and(|(start, end)| secs < start || secs >= end)
    {
        return silent;
    }
    let Some((cart, key_gain)) = sample(&element.keys, secs) else {
        return silent;
    };
    let scale = element.gain * key_gain;
    let mut gains = match &element.route {
        MixRoute::Fixed(gains) => *gains,
        MixRoute::Panned => pan_allocentric(cart),
    };
    for gain in &mut gains {
        *gain *= scale;
    }
    gains
}

/// The callback's side of a mix: the gains it is ramping between, and the
/// bed meters. Allocates only when the element count changes.
#[derive(Default)]
pub struct ObjectMixState {
    current: Vec<Gains>,
    step: Vec<Gains>,
    /// Whether the element is heard anywhere in the current stretch, so a
    /// silent one costs no track read.
    live: Vec<bool>,
    /// Frames left before the next evaluation.
    countdown: usize,
    /// The mix the gains belong to, by address, to notice a new one.
    mix_id: usize,
    primed: bool,
    pub meter_sum_sq: [f64; BED_CHANNELS],
    pub meter_peak: [f32; BED_CHANNELS],
    pub meter_frames: usize,
}

impl ObjectMixState {
    /// Forget the gains: the next frame starts from the positions at the
    /// playhead, without a ramp (a new clip, a stop).
    pub fn reset(&mut self) {
        self.primed = false;
        self.countdown = 0;
    }

    fn shape_for(&mut self, mix: &ObjectMix) {
        let count = mix.elements.len();
        if self.current.len() != count {
            self.current = vec![[0.0; GAIN_SLOTS]; count];
            self.step = vec![[0.0; GAIN_SLOTS]; count];
            self.live = vec![false; count];
            self.primed = false;
        }
        let id = mix as *const ObjectMix as usize;
        if id != self.mix_id {
            // A new mix (an edit, a mute): ramp to it from where the gains
            // are now, starting on this very frame.
            self.mix_id = id;
            self.countdown = 0;
        }
    }

    /// Work out where every element is heading over the next stretch.
    fn retarget(&mut self, mix: &ObjectMix, secs: f64) {
        let jump = !self.primed;
        let inv = 1.0 / RETARGET_FRAMES as f32;
        for (index, element) in mix.elements.iter().enumerate() {
            let target = element_gains(element, secs);
            let current = &mut self.current[index];
            let step = &mut self.step[index];
            let mut live = false;
            for slot in 0..GAIN_SLOTS {
                if jump {
                    current[slot] = target[slot];
                    step[slot] = 0.0;
                } else {
                    step[slot] = (target[slot] - current[slot]) * inv;
                }
                live |= current[slot] != 0.0 || target[slot] != 0.0;
            }
            self.live[index] = live;
        }
        self.primed = true;
        self.countdown = RETARGET_FRAMES;
    }

    /// One frame of the bed into `out` (at least [`BED_CHANNELS`] long).
    /// `pos_secs` is the playhead in the source, `fetch(track)` reads a
    /// track there.
    pub fn mix_frame(
        &mut self,
        mix: &ObjectMix,
        pos_secs: f64,
        fetch: &impl Fn(usize) -> f32,
        out: &mut [f32],
    ) {
        self.shape_for(mix);
        if self.countdown == 0 {
            self.retarget(mix, pos_secs);
        }
        self.countdown -= 1;
        let mut acc = [0.0f32; GAIN_SLOTS];
        for (index, element) in mix.elements.iter().enumerate() {
            if !self.live[index] {
                continue;
            }
            let gains = &mut self.current[index];
            let step = &self.step[index];
            let sample = fetch(element.track);
            for slot in 0..GAIN_SLOTS {
                acc[slot] += gains[slot] * sample;
                gains[slot] += step[slot];
            }
        }
        for (channel, value) in out.iter_mut().take(BED_CHANNELS).enumerate() {
            *value = acc[channel];
            self.meter_sum_sq[channel] += f64::from(acc[channel] * acc[channel]);
            self.meter_peak[channel] = self.meter_peak[channel].max(acc[channel].abs());
        }
        self.meter_frames += 1;
    }

    /// The bed meters since the last call, as (RMS, peak) per channel, and
    /// cleared for the next block.
    pub fn take_meters(&mut self) -> Option<[(f32, f32); BED_CHANNELS]> {
        if self.meter_frames == 0 {
            return None;
        }
        let mut out = [(0.0, 0.0); BED_CHANNELS];
        for (channel, slot) in out.iter_mut().enumerate() {
            let rms = (self.meter_sum_sq[channel] / self.meter_frames as f64).sqrt() as f32;
            *slot = (rms, self.meter_peak[channel]);
        }
        self.meter_sum_sq = [0.0; BED_CHANNELS];
        self.meter_peak = [0.0; BED_CHANNELS];
        self.meter_frames = 0;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(tracks: usize) -> ObjectMixSource {
        ObjectMixSource {
            stream_path: None,
            tracks,
            frames: 48_000,
            file_sr: 48_000,
        }
    }

    fn object(track: usize, keys: Vec<MixKey>) -> MixElement {
        MixElement {
            track,
            route: MixRoute::Panned,
            keys: keys.into(),
            active: None,
            gain: 1.0,
            audible: true,
        }
    }

    fn key(secs: f64, ramp: f64, cart: [f32; 3]) -> MixKey {
        MixKey {
            secs,
            ramp_secs: ramp,
            cart,
            gain: 1.0,
        }
    }

    fn run(
        state: &mut ObjectMixState,
        mix: &ObjectMix,
        frames: usize,
        start_secs: f64,
    ) -> Vec<[f32; 16]> {
        let sr = mix.source.file_sr as f64;
        (0..frames)
            .map(|n| {
                let mut out = [0.0; 16];
                state.mix_frame(mix, start_secs + n as f64 / sr, &|_| 1.0, &mut out);
                out
            })
            .collect()
    }

    #[test]
    fn an_object_at_a_corner_plays_from_that_speaker() {
        let mix = ObjectMix {
            source: source(1),
            pending: false,
            elements: vec![object(0, vec![key(0.0, 0.0, [-1.0, 1.0, 0.0])])],
        };
        let mut state = ObjectMixState::default();
        let out = run(&mut state, &mix, 200, 0.0);
        let last = out.last().unwrap();
        assert!((last[0] - 1.0).abs() < 1e-5, "FL: {last:?}");
        assert!(last[1..BED_CHANNELS].iter().all(|v| v.abs() < 1e-5));
        // The first frame already plays at the position: no fade-in.
        assert!((out[0][0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_moving_object_never_steps() {
        // Left to right across the front in 0.1 s.
        let mix = ObjectMix {
            source: source(1),
            pending: false,
            elements: vec![object(
                0,
                vec![
                    key(0.0, 0.0, [-1.0, 1.0, 0.0]),
                    key(0.1, 0.1, [1.0, 1.0, 0.0]),
                ],
            )],
        };
        let mut state = ObjectMixState::default();
        let out = run(&mut state, &mix, 4_800, 0.0);
        let largest = out
            .windows(2)
            .flat_map(|w| (0..BED_CHANNELS).map(move |c| (w[1][c] - w[0][c]).abs()))
            .fold(0.0f32, f32::max);
        assert!(largest < 0.01, "largest step {largest}");
        let end = out.last().unwrap();
        assert!((end[1] - 1.0).abs() < 1e-3, "arrives at FR: {end:?}");
    }

    #[test]
    fn a_new_mix_ramps_from_the_old_gains() {
        let at = |cart| ObjectMix {
            source: source(1),
            pending: false,
            elements: vec![object(0, vec![key(0.0, 0.0, cart)])],
        };
        let left = at([-1.0, 1.0, 0.0]);
        let right = at([1.0, 1.0, 0.0]);
        let mut state = ObjectMixState::default();
        run(&mut state, &left, 128, 0.0);
        let out = run(&mut state, &right, RETARGET_FRAMES, 0.0);
        assert!(out[0][0] > 0.9, "still mostly left on the first frame");
        assert!(
            out[RETARGET_FRAMES - 1][1] > 0.9,
            "right by the end of the ramp"
        );
    }

    #[test]
    fn muted_inactive_and_soloed_out_elements_are_silent() {
        let mut quiet = object(0, vec![key(0.0, 0.0, [0.0, 1.0, 0.0])]);
        quiet.audible = false;
        let mut later = object(1, vec![key(0.0, 0.0, [0.0, 1.0, 0.0])]);
        later.active = Some((1.0, 2.0));
        let mix = ObjectMix {
            source: source(2),
            pending: false,
            elements: vec![quiet, later],
        };
        let mut state = ObjectMixState::default();
        let out = run(&mut state, &mix, 128, 0.0);
        assert!(out.iter().flatten().all(|v| *v == 0.0));
        let out = run(&mut state, &mix, 256, 1.0);
        assert!(
            (out.last().unwrap()[2] - 1.0).abs() < 1e-5,
            "the later one, once active"
        );
    }

    #[test]
    fn a_bed_channel_keeps_its_speaker() {
        let mut lfe = object(0, vec![key(0.0, 0.0, [0.0, 0.0, 0.0])]);
        lfe.route = MixRoute::Fixed(bed_channel_gains(
            Some(crate::audio_channels::SpeakerPos::Lfe),
            [0.0; 3],
        ));
        let mix = ObjectMix {
            source: source(1),
            pending: false,
            elements: vec![lfe],
        };
        let mut state = ObjectMixState::default();
        let out = run(&mut state, &mix, 64, 0.0);
        assert_eq!(out[10][3], 1.0, "the LFE channel of 7.1.4");
        let meters = state.take_meters().unwrap();
        assert!((meters[3].0 - 1.0).abs() < 1e-6 && meters[0].0 == 0.0);
        assert!(state.take_meters().is_none(), "cleared");
    }

    #[test]
    fn a_mix_applies_only_to_its_own_source() {
        let mix = ObjectMix::pending(ObjectMixSource {
            stream_path: Some(PathBuf::from("a.wav")),
            tracks: 4,
            frames: 100,
            file_sr: 48_000,
        });
        assert!(mix.applies_to(Some(Path::new("a.wav")), 4, 100));
        assert!(!mix.applies_to(Some(Path::new("b.wav")), 4, 100));
        assert!(!mix.applies_to(Some(Path::new("a.wav")), 6, 100));
        assert!(!mix.applies_to(None, 4, 100));
    }
}
