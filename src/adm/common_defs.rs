//! The ITU-R BS.2094 common definitions an ADM file may refer to without
//! writing them out: the DirectSpeakers channels `AC_00010001` ...
//! `AC_00010028`. A bed in a master usually points at these, so without the
//! table its speakers would have no position at all.
//!
//! Common track formats follow one convention, `AT_0001xxxx_01` ->
//! `AS_0001xxxx` -> `AC_0001xxxx`, which [`channel_for_track_format`]
//! applies instead of embedding the stream and track tables.

use crate::audio_channels::SpeakerPos;

/// One common DirectSpeakers channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CommonChannel {
    /// The low 16 bits of `AC_0001xxxx`.
    pub index: u16,
    /// The BS.2051 speaker label (`M+030`).
    pub label: &'static str,
    pub name: &'static str,
    /// Nominal ADM azimuth (left-positive) and elevation, in degrees.
    pub azimuth: f32,
    pub elevation: f32,
    /// The app's speaker for it, when there is one.
    pub speaker: Option<SpeakerPos>,
}

const fn ch(
    index: u16,
    label: &'static str,
    name: &'static str,
    azimuth: f32,
    elevation: f32,
    speaker: Option<SpeakerPos>,
) -> CommonChannel {
    CommonChannel {
        index,
        label,
        name,
        azimuth,
        elevation,
        speaker,
    }
}

use SpeakerPos::*;

/// BS.2094-1, the DirectSpeakers part of the common definitions.
pub const COMMON_CHANNELS: &[CommonChannel] = &[
    ch(0x01, "M+030", "FrontLeft", 30.0, 0.0, Some(Fl)),
    ch(0x02, "M-030", "FrontRight", -30.0, 0.0, Some(Fr)),
    ch(0x03, "M+000", "FrontCentre", 0.0, 0.0, Some(Fc)),
    ch(0x04, "LFE", "LowFrequencyEffects", 0.0, -30.0, Some(Lfe)),
    ch(0x05, "M+110", "SurroundLeft", 110.0, 0.0, Some(Bl)),
    ch(0x06, "M-110", "SurroundRight", -110.0, 0.0, Some(Br)),
    ch(0x07, "M+022", "FrontLeftOfCentre", 22.5, 0.0, Some(Flc)),
    ch(0x08, "M-022", "FrontRightOfCentre", -22.5, 0.0, Some(Frc)),
    ch(0x09, "M+180", "BackCentre", 180.0, 0.0, Some(Bc)),
    ch(0x0a, "M+090", "SideLeft", 90.0, 0.0, Some(Sl)),
    ch(0x0b, "M-090", "SideRight", -90.0, 0.0, Some(Sr)),
    ch(0x0c, "T+000", "TopCentre", 0.0, 90.0, Some(Tc)),
    ch(0x0d, "U+030", "TopFrontLeft", 30.0, 30.0, Some(Tfl)),
    ch(0x0e, "U+000", "TopFrontCentre", 0.0, 30.0, Some(Tfc)),
    ch(0x0f, "U-030", "TopFrontRight", -30.0, 30.0, Some(Tfr)),
    ch(0x10, "U+110", "TopSurroundLeft", 110.0, 30.0, Some(Tbl)),
    ch(0x11, "U+180", "TopBackCentre", 180.0, 30.0, Some(Tbc)),
    ch(0x12, "U-110", "TopSurroundRight", -110.0, 30.0, Some(Tbr)),
    ch(0x13, "U+090", "TopSideLeft", 90.0, 30.0, Some(Tsl)),
    ch(0x14, "U-090", "TopSideRight", -90.0, 30.0, Some(Tsr)),
    ch(0x15, "B+000", "BottomFrontCentre", 0.0, -30.0, None),
    ch(0x16, "B+045", "BottomFrontLeftMid", 45.0, -30.0, None),
    ch(0x17, "B-045", "BottomFrontRightMid", -45.0, -30.0, None),
    ch(0x18, "M+060", "FrontLeftWide", 60.0, 0.0, Some(Wl)),
    ch(0x19, "M-060", "FrontRightWide", -60.0, 0.0, Some(Wr)),
    ch(
        0x1a,
        "M+135_Diff",
        "BackLeftMidDiffuse",
        135.0,
        0.0,
        Some(Bl),
    ),
    ch(
        0x1b,
        "M-135_Diff",
        "BackRightMidDiffuse",
        -135.0,
        0.0,
        Some(Br),
    ),
    ch(0x1c, "M+135", "BackLeftMid", 135.0, 0.0, Some(Bl)),
    ch(0x1d, "M-135", "BackRightMid", -135.0, 0.0, Some(Br)),
    ch(0x1e, "U+135", "TopBackLeftMid", 135.0, 30.0, Some(Tbl)),
    ch(0x1f, "U-135", "TopBackRightMid", -135.0, 30.0, Some(Tbr)),
    ch(0x20, "LFEL", "LowFrequencyEffectsL", 45.0, -30.0, Some(Lfe)),
    ch(
        0x21,
        "LFER",
        "LowFrequencyEffectsR",
        -45.0,
        -30.0,
        Some(Lfe),
    ),
    ch(0x22, "U+045", "TopFrontLeftMid", 45.0, 30.0, Some(Tfl)),
    ch(0x23, "U-045", "TopFrontRightMid", -45.0, 30.0, Some(Tfr)),
    ch(0x24, "M+SC", "FrontLeftScreen", 25.0, 0.0, None),
    ch(0x25, "M-SC", "FrontRightScreen", -25.0, 0.0, None),
    ch(0x26, "M+045", "FrontLeftMid", 45.0, 0.0, None),
    ch(0x27, "M-045", "FrontRightMid", -45.0, 0.0, None),
    ch(0x28, "UH+180", "UpperTopBackCentre", 180.0, 45.0, None),
];

/// The `xxxx` of a common ID (`AC_0001xxxx`, `AT_0001xxxx_nn`), or `None`
/// when the ID is not a common DirectSpeakers one.
fn common_index(id: &str) -> Option<u16> {
    let prefix_ok = id.len() >= 11
        && (id.starts_with("AC_") || id.starts_with("AT_") || id.starts_with("AS_"))
        && id[3..7].eq_ignore_ascii_case("0001");
    if !prefix_ok {
        return None;
    }
    let index = u16::from_str_radix(&id[7..11], 16).ok()?;
    // Below 0x1000 is the common range; files number their own from 0x1001.
    (index < 0x1000).then_some(index)
}

/// The common channel an `AC_0001xxxx` (or `AT_` / `AS_`) ID names.
pub fn common_channel(id: &str) -> Option<&'static CommonChannel> {
    let index = common_index(id)?;
    COMMON_CHANNELS.iter().find(|ch| ch.index == index)
}

/// The channel format a common track format implies: `AT_0001xxxx_01`
/// -> `AC_0001xxxx`.
pub fn channel_for_track_format(track_format: &str) -> Option<String> {
    let index = common_index(track_format)?;
    track_format
        .starts_with("AT_")
        .then(|| format!("AC_0001{index:04x}"))
}

/// The common channel a speaker label names, with or without its
/// `urn:itu:bs:2051:n:speaker:` prefix.
pub fn channel_for_label(label: &str) -> Option<&'static CommonChannel> {
    let code = label.rsplit(':').next().unwrap_or(label).trim();
    COMMON_CHANNELS
        .iter()
        .find(|ch| ch.label.eq_ignore_ascii_case(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_ids_resolve_in_either_case() {
        assert_eq!(common_channel("AC_00010001").unwrap().speaker, Some(Fl));
        assert_eq!(common_channel("AC_0001000A").unwrap().label, "M+090");
        assert_eq!(common_channel("AC_0001000a").unwrap().label, "M+090");
        assert!(common_channel("AC_00031001").is_none(), "an object channel");
        assert!(
            common_channel("AC_00011001").is_none(),
            "a file's own channel"
        );
        assert_eq!(
            channel_for_track_format("AT_00010004_01").as_deref(),
            Some("AC_00010004")
        );
        assert_eq!(channel_for_track_format("AT_00031001_01"), None);
    }

    #[test]
    fn labels_resolve_with_or_without_the_urn() {
        assert_eq!(channel_for_label("M-030").unwrap().index, 0x02);
        assert_eq!(
            channel_for_label("urn:itu:bs:2051:0:speaker:U+090")
                .unwrap()
                .speaker,
            Some(Tsl)
        );
        assert!(channel_for_label("nonsense").is_none());
    }

    #[test]
    fn the_table_is_complete_and_unique() {
        assert_eq!(COMMON_CHANNELS.len(), 0x28);
        for (i, ch) in COMMON_CHANNELS.iter().enumerate() {
            assert_eq!(ch.index as usize, i + 1, "{}", ch.name);
        }
    }
}
