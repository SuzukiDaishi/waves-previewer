//! Object-based audio: a scene of beds and moving objects, in seconds.
//!
//! A file such as an ADM BWF master or a TrueHD stream with object audio
//! carries PCM tracks plus a description of where each track is heard, and
//! when. This module is that description without any file format: the scene
//! model (`scene`), the coordinate conversions (`coords`) and the monitoring
//! panner (`panner`). `crate::adm` and `crate::audio_truehd` fill it in;
//! `crate::object_mix` plays it. See `docs/SPATIAL_AUDIO_SPEC.md`.
//!
//! Positions are ADM Cartesian throughout -- X left (-1) to right (+1),
//! Y back (-1) to front (+1), Z floor (0) to ceiling (+1), with -1 below the
//! listener -- except where an element was written in polar coordinates, in
//! which case its keyframes keep them (azimuth positive to the LEFT, as ADM
//! has it; the app's own `SpeakerPos::direction` is positive to the right).

pub mod coords;
pub mod oamd;
pub mod panner;
pub mod scene;

use std::sync::Arc;

/// The kind of file a scene came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectFormat {
    /// ITU-R BS.2076 metadata (`axml` + `chna`) in a RIFF / RF64 / BW64 WAVE.
    Adm,
    /// The object presentation of a TrueHD stream.
    TrueHd,
}

impl ObjectFormat {
    pub fn label(self) -> &'static str {
        match self {
            ObjectFormat::Adm => "ADM",
            ObjectFormat::TrueHd => "TrueHD",
        }
    }
}

/// What a list row says about an object-based file. Read from the header
/// alone (`chna` for ADM), never from the full scene, so it costs a few
/// kilobytes of I/O per file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectAudioSummary {
    pub format: ObjectFormat,
    /// PCM tracks in the file.
    pub tracks: u32,
    /// Tracks that feed a fixed speaker (DirectSpeakers / a bed).
    pub bed_channels: u32,
    /// Tracks that carry objects.
    pub objects: u32,
    /// Tracks of a kind the app does not render (HOA, Matrix, Binaural).
    pub other: u32,
    /// "ADM · 10 bed + 118 obj", built once here so a list row never
    /// formats it per frame.
    pub label: Arc<str>,
}

impl ObjectAudioSummary {
    pub fn new(
        format: ObjectFormat,
        tracks: u32,
        bed_channels: u32,
        objects: u32,
        other: u32,
    ) -> Self {
        let mut parts = Vec::new();
        if bed_channels > 0 {
            parts.push(format!("{bed_channels} bed"));
        }
        if objects > 0 {
            parts.push(format!("{objects} obj"));
        }
        if other > 0 {
            parts.push(format!("{other} other"));
        }
        let label = if parts.is_empty() {
            format!("{} \u{b7} no objects", format.label())
        } else {
            format!("{} \u{b7} {}", format.label(), parts.join(" + "))
        };
        Self {
            format,
            tracks,
            bed_channels,
            objects,
            other,
            label: Arc::from(label),
        }
    }
}
