//! Sample rates, and which one a given number is.
//!
//! A sample rate here is never "the" rate. Several are live at once, and
//! mixing them up is how a 96 kHz file ends up edited on a 48 kHz timeline or
//! a one-second window turns into half a second. Code names the one it means:
//!
//! | name         | what it is                                            | where it comes from                      |
//! |--------------|-------------------------------------------------------|------------------------------------------|
//! | `file_sr`    | the rate the file on disk was recorded at             | metadata, an override, a virtual row      |
//! | `buffer_sr`  | the rate of the samples an editor tab is working on   | `EditorTab::buffer_sample_rate`           |
//! | `out_sr`     | the output device's rate, what playback runs at       | `AudioShared::out_sample_rate`            |
//! | `capture_sr` | the input device's rate while recording               | the capture stream, once opened           |
//! | `timeline_sr`| a Multi Edits timeline's mixdown rate                 | `MultiEditDoc::timeline_sr`: its first clip's `file_sr` |
//! | fixed        | a rate a standard or model requires                   | a named `const` beside the code using it  |
//!
//! Nothing here holds a device or a file; this module is only the vocabulary
//! and the few numbers every one of those shares.

/// Lowest rate the app accepts as a conversion target or override. Below
/// this, speech is unintelligible and several codecs refuse the stream.
pub const MIN_SAMPLE_RATE: u32 = 8_000;

/// Highest rate the app accepts as a conversion target or override: DXD /
/// 8x 48 kHz, the top of what current interfaces and codecs deliver.
pub const MAX_SAMPLE_RATE: u32 = 384_000;

/// The rate assumed when nothing better is known -- not the file, not a
/// device. Only for `Default` values and devices that have not reported yet;
/// a file whose rate is unknown is assumed to play at `out_sr` instead, see
/// [`SampleRateOrigin::AssumedOutput`].
pub const FALLBACK_SAMPLE_RATE: u32 = 48_000;

/// Where a new conversion (the effect-graph Resampler node) points by
/// default: the rate video, broadcast and most game engines deliver at.
pub const DEFAULT_CONVERSION_TARGET_SAMPLE_RATE: u32 = 48_000;

/// Edges of human hearing, for frequency controls and displays.
pub const AUDIBLE_LOW_HZ: f32 = 20.0;
pub const AUDIBLE_HIGH_HZ: f32 = 20_000.0;

/// Where a file's sample rate came from, most to least trustworthy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleRateOrigin {
    /// The user converted (or is about to convert) the file to this rate.
    Override,
    /// Read from the file's header by the metadata pass.
    File,
    /// The rate a `(virtual)` row was made at.
    Virtual,
    /// Read from the header on demand, before the metadata pass got there.
    Probed,
    /// Nothing reported a rate. The file is treated as if it were at the
    /// output device's rate, which is what playback would do with it anyway.
    AssumedOutput,
}

/// A file's sample rate together with how sure we are of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedSampleRate {
    pub hz: u32,
    pub origin: SampleRateOrigin,
}

impl ResolvedSampleRate {
    pub fn new(hz: u32, origin: SampleRateOrigin) -> Self {
        Self {
            hz: hz.max(1),
            origin,
        }
    }

    /// True when no source reported the rate and `hz` is a stand-in.
    pub fn is_assumed(&self) -> bool {
        self.origin == SampleRateOrigin::AssumedOutput
    }
}

/// Takes the first rate that is actually known (non-zero), in priority order.
pub fn resolve_first(
    candidates: impl IntoIterator<Item = (Option<u32>, SampleRateOrigin)>,
    out_sr: u32,
) -> ResolvedSampleRate {
    candidates
        .into_iter()
        .find_map(|(hz, origin)| hz.filter(|v| *v > 0).map(|v| ResolvedSampleRate::new(v, origin)))
        .unwrap_or_else(|| ResolvedSampleRate::new(out_sr, SampleRateOrigin::AssumedOutput))
}

/// Whether `hz` is a rate the app will convert to.
pub fn is_supported_target(hz: u32) -> bool {
    (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&hz)
}

/// Frames in `secs` seconds at `sample_rate`, rounded to the nearest frame.
pub fn frames_for_secs(secs: f64, sample_rate: u32) -> usize {
    (secs.max(0.0) * sample_rate.max(1) as f64).round() as usize
}

/// Seconds spanned by `frames` frames at `sample_rate`.
pub fn secs_for_frames(frames: usize, sample_rate: u32) -> f64 {
    frames as f64 / sample_rate.max(1) as f64
}

/// Label for a sample-rate cell: the rate, or a `?` when it is only assumed.
/// `None` while nothing has been read yet (the metadata pass is pending).
pub fn rate_label(rate: Option<ResolvedSampleRate>) -> String {
    match rate {
        None => "-".to_string(),
        Some(rate) if rate.is_assumed() => "?".to_string(),
        Some(rate) => rate.hz.to_string(),
    }
}

/// Explanation shown on hover when a file's rate is only assumed.
pub fn assumed_rate_hint(rate: ResolvedSampleRate) -> String {
    format!(
        "Sample rate unknown — treated as {} Hz (the output device's rate)",
        rate.hz
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_known_rate_wins_in_priority_order() {
        let rate = resolve_first(
            [
                (None, SampleRateOrigin::Override),
                (Some(0), SampleRateOrigin::File),
                (Some(96_000), SampleRateOrigin::Virtual),
                (Some(44_100), SampleRateOrigin::Probed),
            ],
            48_000,
        );
        assert_eq!(rate, ResolvedSampleRate::new(96_000, SampleRateOrigin::Virtual));
        assert!(!rate.is_assumed());
    }

    #[test]
    fn nothing_known_assumes_the_output_rate_and_says_so() {
        let rate = resolve_first([(None, SampleRateOrigin::File)], 44_100);
        assert_eq!(rate.hz, 44_100);
        assert!(rate.is_assumed());
        assert_eq!(rate_label(Some(rate)), "?");
        assert!(assumed_rate_hint(rate).contains("44100"));
    }

    #[test]
    fn labels_tell_pending_from_known() {
        assert_eq!(rate_label(None), "-");
        assert_eq!(
            rate_label(Some(ResolvedSampleRate::new(88_200, SampleRateOrigin::File))),
            "88200"
        );
    }

    #[test]
    fn frames_and_seconds_scale_with_the_rate() {
        assert_eq!(frames_for_secs(1.0, 48_000), 48_000);
        assert_eq!(frames_for_secs(1.0, 96_000), 96_000);
        assert_eq!(frames_for_secs(0.5, 44_100), 22_050);
        assert_eq!(frames_for_secs(-1.0, 48_000), 0);
        assert!((secs_for_frames(22_050, 44_100) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn supported_range_is_inclusive() {
        assert!(is_supported_target(MIN_SAMPLE_RATE));
        assert!(is_supported_target(MAX_SAMPLE_RATE));
        assert!(!is_supported_target(MIN_SAMPLE_RATE - 1));
        assert!(!is_supported_target(MAX_SAMPLE_RATE + 1));
    }
}
