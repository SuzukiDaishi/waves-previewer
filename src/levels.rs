//! Signal levels in dB that more than one part of the app has to agree on.
//!
//! Each constant is one meaning. Two that happen to share a value today
//! (the volume floor and the silence threshold are both -80) stay separate,
//! because changing one must not move the other.

/// Output volume control, in dB of gain. The floor is effectively mute; the
/// ceiling leaves a little boost for quiet material without inviting clipping.
pub const VOLUME_MIN_DB: f32 = -80.0;
pub const VOLUME_MAX_DB: f32 = 6.0;

/// Output level meter, in dBFS: what it shows when nothing is playing, and
/// the most it reads (matching the largest volume boost).
pub const METER_FLOOR_DB: f32 = -80.0;
pub const METER_CEILING_DB: f32 = VOLUME_MAX_DB;

/// A peak below this is treated as digital silence when measuring a file:
/// under the noise floor of any real recording, but above float rounding
/// noise, so a silent file does not report a peak of -300 dB.
pub const SILENCE_DBFS: f32 = -80.0;

/// Linear amplitude of [`SILENCE_DBFS`].
pub fn silence_amplitude() -> f32 {
    db_to_amplitude(SILENCE_DBFS)
}

/// What a level reads when there is no signal at all (an all-zero buffer),
/// instead of -infinity. Low enough to sort below any real level.
pub const NO_SIGNAL_DB: f32 = -120.0;

/// The blank-detection threshold the user can set, in dBFS.
pub const BLANK_THRESHOLD_MIN_DBFS: f32 = -120.0;
pub const BLANK_THRESHOLD_MAX_DBFS: f32 = 0.0;

/// Spectrogram colour range. The floor (darkest colour) defaults to the
/// no-signal level and may go down to -160 dB, the dynamic range of 24-bit
/// audio plus FFT gain; the ceiling (brightest) may be pulled down to -80 dB.
/// The two always stay `SPECTRO_MIN_SPAN_DB` apart so the map never collapses.
pub const SPECTRO_DB_FLOOR_DEFAULT: f32 = NO_SIGNAL_DB;
pub const SPECTRO_DB_FLOOR_MIN: f32 = -160.0;
pub const SPECTRO_DB_FLOOR_MAX: f32 = -20.0;
pub const SPECTRO_DB_CEILING_MIN: f32 = -80.0;
pub const SPECTRO_DB_CEILING_MAX: f32 = 0.0;
pub const SPECTRO_MIN_SPAN_DB: f32 = 10.0;

pub fn db_to_amplitude(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_amplitude_matches_its_db() {
        assert!((silence_amplitude() - 1.0e-4).abs() < 1.0e-9);
        assert!((db_to_amplitude(0.0) - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn spectrogram_range_leaves_room_for_the_minimum_span() {
        assert!(SPECTRO_DB_FLOOR_MAX + SPECTRO_MIN_SPAN_DB <= SPECTRO_DB_CEILING_MAX);
        assert!(SPECTRO_DB_FLOOR_MIN < SPECTRO_DB_FLOOR_DEFAULT);
        assert!(SPECTRO_DB_FLOOR_DEFAULT < SPECTRO_DB_FLOOR_MAX);
    }
}
