//! TrueHD's object metadata as a scene. Rules only: the decoder
//! (`crate::audio_truehd`, behind the `truehd` feature) turns each object
//! audio metadata payload into an [`OamdUpdate`] and this module keeps the
//! keyframes, so the conversion is the same -- and tested -- whether or not
//! the decoder is built.
//!
//! Timing follows the truehdd project's DAMF writer (Apache-2.0): an update
//! applies at `samples written so far + the payload's sample offset + its
//! evolution sample offset`, and glides in over the first block's ramp.
//! Positions arrive already in ADM Cartesian (the crate's `get_damf_pos`
//! maps OAMD's 0..1 room coordinates onto -1..1 with the front wall at
//! Y = +1).

use std::sync::Arc;

use crate::audio_channels::SpeakerPos;

use super::scene::{
    dedup_keyframes, Coords, Element, ElementKind, Keyframe, ObjectScene, SourceShape,
};
use super::ObjectFormat;

/// The 42/62 of a room's depth where TrueHD's wide speakers stand.
const WIDE_Y: f32 = 42.0 / 62.0;

/// TrueHD's bed speakers, by index (the crate's `SpeakerLabels`): label,
/// the app's speaker, and the position TrueHD gives it.
pub const BED_SPEAKERS: [(&str, Option<SpeakerPos>, [f32; 3]); 17] = [
    ("L", Some(SpeakerPos::Fl), [-1.0, 1.0, 0.0]),
    ("R", Some(SpeakerPos::Fr), [1.0, 1.0, 0.0]),
    ("C", Some(SpeakerPos::Fc), [0.0, 1.0, 0.0]),
    ("LFE", Some(SpeakerPos::Lfe), [-1.0, 1.0, -1.0]),
    ("Lss", Some(SpeakerPos::Sl), [-1.0, 0.0, 0.0]),
    ("Rss", Some(SpeakerPos::Sr), [1.0, 0.0, 0.0]),
    ("Lrs", Some(SpeakerPos::Bl), [-1.0, -1.0, 0.0]),
    ("Rrs", Some(SpeakerPos::Br), [1.0, -1.0, 0.0]),
    ("Lfh", Some(SpeakerPos::Tfl), [-1.0, 1.0, 1.0]),
    ("Rfh", Some(SpeakerPos::Tfr), [1.0, 1.0, 1.0]),
    ("Lts", Some(SpeakerPos::Tsl), [-1.0, 0.0, 1.0]),
    ("Rts", Some(SpeakerPos::Tsr), [1.0, 0.0, 1.0]),
    ("Lrh", Some(SpeakerPos::Tbl), [-1.0, -1.0, 1.0]),
    ("Rrh", Some(SpeakerPos::Tbr), [1.0, -1.0, 1.0]),
    ("Lw", Some(SpeakerPos::Wl), [-1.0, WIDE_Y, 0.0]),
    ("Rw", Some(SpeakerPos::Wr), [1.0, WIDE_Y, 0.0]),
    ("LFE2", Some(SpeakerPos::Lfe), [1.0, 1.0, -1.0]),
];

/// The channels of a presentation without objects, by index (the crate's
/// `ChannelLabel`): label and the app's speaker.
pub const CHANNEL_SPEAKERS: [(&str, Option<SpeakerPos>); 24] = [
    ("L", Some(SpeakerPos::Fl)),
    ("R", Some(SpeakerPos::Fr)),
    ("C", Some(SpeakerPos::Fc)),
    ("LFE", Some(SpeakerPos::Lfe)),
    ("Ls", Some(SpeakerPos::Sl)),
    ("Rs", Some(SpeakerPos::Sr)),
    ("Tfl", Some(SpeakerPos::Tfl)),
    ("Tfr", Some(SpeakerPos::Tfr)),
    ("Tsl", Some(SpeakerPos::Tsl)),
    ("Tsr", Some(SpeakerPos::Tsr)),
    ("Tbl", Some(SpeakerPos::Tbl)),
    ("Tbr", Some(SpeakerPos::Tbr)),
    ("Lsc", Some(SpeakerPos::Flc)),
    ("Rsc", Some(SpeakerPos::Frc)),
    ("Lb", Some(SpeakerPos::Bl)),
    ("Rb", Some(SpeakerPos::Br)),
    ("Cb", Some(SpeakerPos::Bc)),
    ("Tc", Some(SpeakerPos::Tc)),
    ("Lsd", Some(SpeakerPos::Sl)),
    ("Rsd", Some(SpeakerPos::Sr)),
    ("Lw", Some(SpeakerPos::Wl)),
    ("Rw", Some(SpeakerPos::Wr)),
    ("Tfc", Some(SpeakerPos::Tfc)),
    ("LFE2", Some(SpeakerPos::Lfe)),
];

/// One element's state in one metadata update.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObjectUpdate {
    /// A bed channel rather than an object.
    pub in_bed: bool,
    pub active: bool,
    /// Linear (0 for TrueHD's minus infinity).
    pub gain: f32,
    /// ADM Cartesian; ignored for a bed channel.
    pub pos: [f32; 3],
}

/// One metadata payload, timed.
#[derive(Clone, Debug, PartialEq)]
pub struct OamdUpdate {
    /// Where it applies, in samples from the start of the stream.
    pub sample_pos: u64,
    /// How long it glides in, in samples.
    pub ramp: u64,
    /// The bed's speakers, as indices into [`BED_SPEAKERS`].
    pub bed: Vec<usize>,
    /// Every element, in PCM channel order.
    pub objects: Vec<ObjectUpdate>,
}

/// TrueHD's object gain (dB, -128 for minus infinity) as linear.
pub fn gain_from_db(db: i8) -> f32 {
    if db == i8::MIN {
        0.0
    } else {
        10f32.powf(db as f32 / 20.0)
    }
}

/// The scene of a stream, built up one update at a time while it decodes.
#[derive(Default)]
pub struct OamdSceneBuilder {
    bed: Vec<usize>,
    in_bed: Vec<bool>,
    keyframes: Vec<Vec<Keyframe>>,
    updates: usize,
}

impl OamdSceneBuilder {
    pub fn has_objects(&self) -> bool {
        self.updates > 0
    }

    pub fn push(&mut self, update: &OamdUpdate, file_sr: u32) {
        let sr = f64::from(file_sr.max(1));
        if self.updates == 0 {
            self.bed = update.bed.clone();
            self.in_bed = update.objects.iter().map(|o| o.in_bed).collect();
            self.keyframes = vec![Vec::new(); update.objects.len()];
        }
        self.updates += 1;
        let ramp_secs = update.ramp as f64 / sr;
        let secs = (update.sample_pos + update.ramp) as f64 / sr;
        let mut bed_index = 0usize;
        for (index, object) in update.objects.iter().enumerate() {
            let Some(keys) = self.keyframes.get_mut(index) else {
                break;
            };
            let in_bed = self.in_bed.get(index).copied().unwrap_or(object.in_bed);
            let pos = if in_bed {
                let label = self.bed.get(bed_index).copied();
                bed_index += 1;
                label
                    .and_then(|label| BED_SPEAKERS.get(label))
                    .map(|(_, _, pos)| *pos)
                    .unwrap_or([0.0, 1.0, 0.0])
            } else {
                object.pos
            };
            keys.push(Keyframe {
                secs,
                ramp_secs,
                pos,
                gain: if object.active { object.gain } else { 0.0 },
                extras: None,
            });
        }
    }

    /// The finished scene of a stream of `shape`.
    pub fn finish(self, shape: SourceShape) -> ObjectScene {
        let mut elements = Vec::new();
        let mut bed_index = 0usize;
        let mut object_number = 0usize;
        for (track, keys) in self.keyframes.into_iter().enumerate() {
            if track as u32 >= shape.tracks || keys.is_empty() {
                continue;
            }
            let in_bed = self.in_bed.get(track).copied().unwrap_or(false);
            let (key, name, kind) = if in_bed {
                let label = self.bed.get(bed_index).copied().unwrap_or(usize::MAX);
                bed_index += 1;
                let (label_text, speaker, _) = BED_SPEAKERS
                    .get(label)
                    .copied()
                    .unwrap_or(("?", None, [0.0; 3]));
                (
                    format!("thd:bed:{label_text}"),
                    label_text.to_string(),
                    ElementKind::Bed {
                        speaker,
                        label: Arc::from(label_text),
                    },
                )
            } else {
                object_number += 1;
                (
                    format!("thd:obj{object_number}"),
                    format!("Object {object_number}"),
                    ElementKind::Object,
                )
            };
            elements.push(Element {
                key: Arc::from(key),
                name: Arc::from(name),
                group: Arc::from(if in_bed { "Bed" } else { "Objects" }),
                track: track as u32,
                kind,
                coords: Coords::Cartesian,
                keyframes: Arc::from(dedup_keyframes(keys)),
                active: None,
                gain: 1.0,
            });
        }
        let mut scene = ObjectScene {
            format: ObjectFormat::TrueHd,
            shape,
            programme: None,
            elements,
            diagnostics: Vec::new(),
        };
        scene.sort_elements();
        scene
    }
}

/// The scene of a presentation without objects: one bed channel per PCM
/// channel, by its label (indices into [`CHANNEL_SPEAKERS`]).
pub fn channel_scene(labels: &[usize], shape: SourceShape) -> ObjectScene {
    let elements = labels
        .iter()
        .enumerate()
        .filter(|(track, _)| (*track as u32) < shape.tracks)
        .map(|(track, &label)| {
            let (text, speaker) = CHANNEL_SPEAKERS.get(label).copied().unwrap_or(("?", None));
            let pos = speaker
                .and_then(super::coords::speaker_cart)
                .unwrap_or([0.0, 1.0, 0.0]);
            Element {
                key: Arc::from(format!("thd:ch{track}:{text}")),
                name: Arc::from(text),
                group: Arc::from("Channels"),
                track: track as u32,
                kind: ElementKind::Bed {
                    speaker,
                    label: Arc::from(text),
                },
                coords: Coords::Cartesian,
                keyframes: Arc::from(vec![Keyframe::at(0.0, pos)]),
                active: None,
                gain: 1.0,
            }
        })
        .collect();
    ObjectScene {
        format: ObjectFormat::TrueHd,
        shape,
        programme: None,
        elements,
        diagnostics: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn shape(tracks: u32) -> SourceShape {
        SourceShape {
            tracks,
            frames: SR as u64 * 2,
            file_sr: SR,
        }
    }

    fn update(sample_pos: u64, ramp: u64, object_x: f32, active: bool) -> OamdUpdate {
        OamdUpdate {
            sample_pos,
            ramp,
            // L, R, LFE.
            bed: vec![0, 1, 3],
            objects: vec![
                ObjectUpdate {
                    in_bed: true,
                    active: true,
                    gain: 1.0,
                    pos: [0.0; 3],
                },
                ObjectUpdate {
                    in_bed: true,
                    active: true,
                    gain: 1.0,
                    pos: [0.0; 3],
                },
                ObjectUpdate {
                    in_bed: true,
                    active: true,
                    gain: 1.0,
                    pos: [0.0; 3],
                },
                ObjectUpdate {
                    in_bed: false,
                    active,
                    gain: gain_from_db(-6),
                    pos: [object_x, 0.5, 0.25],
                },
            ],
        }
    }

    #[test]
    fn updates_become_bed_channels_and_a_gliding_object() {
        let mut builder = OamdSceneBuilder::default();
        builder.push(&update(0, 0, -1.0, true), SR);
        builder.push(&update(SR as u64 / 2, SR as u64 / 10, 1.0, true), SR);
        // A repeat of the state in force: no new keyframe once deduplicated.
        builder.push(&update(SR as u64, 0, 1.0, true), SR);
        assert!(builder.has_objects());
        let scene = builder.finish(shape(4));
        assert_eq!(scene.format, ObjectFormat::TrueHd);
        assert_eq!((scene.bed_count(), scene.object_count()), (3, 1));
        let lfe = scene.element("thd:bed:LFE").expect("the LFE");
        assert!(lfe.is_lfe());
        assert_eq!(lfe.track, 2);
        let object = scene.element("thd:obj1").expect("the object");
        assert_eq!(object.track, 3);
        assert_eq!(object.keyframes.len(), 2);
        let glide = &object.keyframes[1];
        assert!(
            (glide.secs - 0.6).abs() < 1e-9,
            "arrives after its ramp: {}",
            glide.secs
        );
        assert!((glide.ramp_secs - 0.1).abs() < 1e-9);
        assert!((glide.gain - 0.501).abs() < 1e-3, "-6 dB");
        assert_eq!(
            object.cart_at(0.55).unwrap()[0],
            0.0,
            "halfway through the glide"
        );
    }

    #[test]
    fn an_inactive_object_is_silent_and_minus_infinity_is_zero() {
        assert_eq!(gain_from_db(i8::MIN), 0.0);
        assert!((gain_from_db(0) - 1.0).abs() < 1e-6);
        let mut builder = OamdSceneBuilder::default();
        builder.push(&update(0, 0, 0.0, false), SR);
        let scene = builder.finish(shape(4));
        assert_eq!(scene.element("thd:obj1").unwrap().keyframes[0].gain, 0.0);
    }

    #[test]
    fn a_presentation_without_objects_is_its_channels() {
        // 7.1: L R C LFE Ls Rs Lb Rb.
        let scene = channel_scene(&[0, 1, 2, 3, 4, 5, 14, 15], shape(8));
        assert_eq!(scene.elements.len(), 8);
        let speakers: Vec<Option<SpeakerPos>> = scene
            .elements
            .iter()
            .map(|e| match &e.kind {
                ElementKind::Bed { speaker, .. } => *speaker,
                ElementKind::Object => None,
            })
            .collect();
        assert_eq!(speakers[4], Some(SpeakerPos::Sl));
        assert_eq!(speakers[6], Some(SpeakerPos::Bl));
        assert!(scene.elements[3].is_lfe());
    }
}
