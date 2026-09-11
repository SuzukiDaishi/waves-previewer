//! Deterministic test tones.
//!
//! Pure arithmetic, no clock and no entropy-seeded RNG: regenerating the
//! fixtures has to produce byte-identical files, or every run shows up as a
//! diff in a repository that stores them.
//!
//! The shapes here follow `.agents/skills/gui-screenshot-debug/SKILL.md`: a
//! flat sine tells you nothing about whether the waveform is drawn in the right
//! place, so every channel gets an envelope that peaks at its own moment.

use std::f32::consts::TAU;

/// Each channel's own pitch. Matches the spacing `make_multichannel_wav` in
/// `tests/editor_paged_waveform.rs` already uses, so fixtures and tests sound
/// alike. Stays under 2 kHz at 32 channels, well inside Nyquist even at 8 kHz.
pub fn channel_freq(channel: usize) -> f32 {
    110.0 + 55.0 * channel as f32
}

/// A raised-cosine bump over `[center - width/2, center + width/2]`, in
/// normalized time.
fn bump(t_norm: f32, center: f32, width: f32) -> f32 {
    let half = width * 0.5;
    let d = (t_norm - center).abs();
    if d >= half {
        0.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * d / half).cos())
    }
}

/// One channel of a multichannel fixture: its own pitch, and a burst at its own
/// point in the file so the lanes cannot be confused for one another.
///
/// A quiet floor keeps every lane visible even outside its burst -- a channel
/// that is silent most of the time reads as a channel that failed to load.
pub fn lane(channel: usize, channels: usize, sample_rate: u32, frames: usize) -> Vec<f32> {
    let freq = channel_freq(channel);
    let center = (channel as f32 + 0.5) / channels as f32;
    let width = (1.6 / channels as f32).max(0.12);
    (0..frames)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let t_norm = if frames > 1 {
                i as f32 / (frames - 1) as f32
            } else {
                0.0
            };
            let amp = 0.18 + 0.75 * bump(t_norm, center, width);
            (t * freq * TAU).sin() * amp
        })
        .collect()
}

/// `channels` lanes of [`lane`].
pub fn lanes(channels: usize, sample_rate: u32, frames: usize) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|ch| lane(ch, channels, sample_rate, frames))
        .collect()
}

/// A single tone at a fixed level, with a slow tremolo so the trace is never a
/// flat band.
pub fn tone(freq: f32, amp: f32, sample_rate: u32, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let tremolo = 0.72 + 0.28 * (t * 3.0 * TAU).sin();
            (t * freq * TAU).sin() * amp * tremolo
        })
        .collect()
}

pub fn silence(frames: usize) -> Vec<f32> {
    vec![0.0; frames]
}
