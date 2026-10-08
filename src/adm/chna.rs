//! The `chna` chunk: which PCM track carries which ADM track UID.
//!
//! ```text
//! u16 numTracks
//! u16 numUIDs
//! numUIDs x {
//!     u16      trackIndex    (1-based; 0 is an unused slot)
//!     [u8;12]  UID           ATU_xxxxxxxx
//!     [u8;14]  trackRef      AT_yyyyxxxx_nn, or AC_yyyyxxxx (BS.2076-2)
//!     [u8;11]  packRef       AP_yyyyxxxx
//!     u8       padding
//! }
//! ```
//!
//! The `yyyy` of a pack or channel ID is its type -- 0001 DirectSpeakers,
//! 0002 Matrix, 0003 Objects, 0004 HOA, 0005 Binaural -- which is all a list
//! row needs to say "10 bed + 118 obj" without reading `axml`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};

use crate::spatial::{ObjectAudioSummary, ObjectFormat};

const ENTRY_LEN: usize = 40;
const HEADER_LEN: usize = 4;

/// ADM `typeDefinition` codes.
pub const TYPE_DIRECT_SPEAKERS: u16 = 0x0001;
pub const TYPE_MATRIX: u16 = 0x0002;
pub const TYPE_OBJECTS: u16 = 0x0003;
pub const TYPE_HOA: u16 = 0x0004;
pub const TYPE_BINAURAL: u16 = 0x0005;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChnaEntry {
    /// 1-based PCM track.
    pub track_index: u16,
    pub uid: String,
    pub track_ref: String,
    pub pack_ref: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Chna {
    pub num_tracks: u16,
    pub entries: Vec<ChnaEntry>,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string()
}

/// The type code of an ADM ID such as `AP_00031001`, `AC_00010003` or
/// `AT_00031001_01`.
pub fn type_of_id(id: &str) -> Option<u16> {
    let digits = id.get(3..7)?;
    u16::from_str_radix(digits, 16).ok()
}

impl Chna {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN {
            bail!("chna: {} bytes is too short", bytes.len());
        }
        let num_tracks = u16::from_le_bytes([bytes[0], bytes[1]]);
        let num_uids = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
        let mut entries = Vec::with_capacity(num_uids);
        for index in 0..num_uids {
            let at = HEADER_LEN + index * ENTRY_LEN;
            let Some(entry) = bytes.get(at..at + ENTRY_LEN) else {
                break;
            };
            let track_index = u16::from_le_bytes([entry[0], entry[1]]);
            if track_index == 0 {
                continue;
            }
            entries.push(ChnaEntry {
                track_index,
                uid: text(&entry[2..14]),
                track_ref: text(&entry[14..28]),
                pack_ref: text(&entry[28..39]),
            });
        }
        Ok(Self {
            num_tracks,
            entries,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.entries.len() * ENTRY_LEN);
        out.extend_from_slice(&self.num_tracks.to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        let field = |out: &mut Vec<u8>, value: &str, len: usize| {
            let bytes = value.as_bytes();
            let take = bytes.len().min(len);
            out.extend_from_slice(&bytes[..take]);
            out.extend(std::iter::repeat_n(0u8, len - take));
        };
        for entry in &self.entries {
            out.extend_from_slice(&entry.track_index.to_le_bytes());
            field(&mut out, &entry.uid, 12);
            field(&mut out, &entry.track_ref, 14);
            field(&mut out, &entry.pack_ref, 11);
            out.push(0);
        }
        out
    }

    pub fn entry_for_uid(&self, uid: &str) -> Option<&ChnaEntry> {
        self.entries
            .iter()
            .find(|entry| entry.uid.eq_ignore_ascii_case(uid))
    }

    /// What a list row shows. A track counts once, under the first type
    /// that claims it; a track that changes type over time is rare enough
    /// that being off by one there does not matter.
    pub fn summary(&self) -> ObjectAudioSummary {
        let mut kind_by_track: BTreeMap<u16, u16> = BTreeMap::new();
        for entry in &self.entries {
            let kind = type_of_id(&entry.pack_ref)
                .or_else(|| type_of_id(&entry.track_ref))
                .unwrap_or(0);
            kind_by_track.entry(entry.track_index).or_insert(kind);
        }
        let count = |wanted: &[u16]| {
            kind_by_track
                .values()
                .filter(|kind| wanted.contains(kind))
                .count() as u32
        };
        let tracks_used: BTreeSet<u16> = kind_by_track.keys().copied().collect();
        ObjectAudioSummary::new(
            ObjectFormat::Adm,
            (self.num_tracks as u32).max(tracks_used.len() as u32),
            count(&[TYPE_DIRECT_SPEAKERS]),
            count(&[TYPE_OBJECTS]),
            count(&[TYPE_MATRIX, TYPE_HOA, TYPE_BINAURAL]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(track: u16, uid: &str, track_ref: &str, pack: &str) -> ChnaEntry {
        ChnaEntry {
            track_index: track,
            uid: uid.into(),
            track_ref: track_ref.into(),
            pack_ref: pack.into(),
        }
    }

    #[test]
    fn a_chna_chunk_round_trips() {
        let chna = Chna {
            num_tracks: 3,
            entries: vec![
                entry(1, "ATU_00000001", "AT_00010001_01", "AP_00010002"),
                entry(2, "ATU_00000002", "AT_00010002_01", "AP_00010002"),
                entry(3, "ATU_00000003", "AC_00031001", "AP_00031001"),
            ],
        };
        let bytes = chna.encode();
        assert_eq!(bytes.len(), 4 + 3 * 40);
        assert_eq!(Chna::parse(&bytes).unwrap(), chna);
    }

    #[test]
    fn unused_slots_are_skipped_and_types_counted() {
        let mut chna = Chna {
            num_tracks: 4,
            entries: vec![
                entry(1, "ATU_00000001", "AT_00010001_01", "AP_00010002"),
                entry(2, "ATU_00000002", "AT_00010002_01", "AP_00010002"),
                entry(3, "ATU_00000003", "AT_00031001_01", "AP_00031001"),
                entry(4, "ATU_00000004", "AT_00041001_01", "AP_00041001"),
            ],
        };
        let mut bytes = chna.encode();
        // An allocated but unused slot, as writers reserve them.
        bytes[2..4].copy_from_slice(&5u16.to_le_bytes());
        bytes.extend(std::iter::repeat_n(0u8, 40));
        let read = Chna::parse(&bytes).unwrap();
        assert_eq!(read, chna);
        let summary = read.summary();
        assert_eq!(
            (
                summary.tracks,
                summary.bed_channels,
                summary.objects,
                summary.other
            ),
            (4, 2, 1, 1)
        );
        assert_eq!(&*summary.label, "ADM \u{b7} 2 bed + 1 obj + 1 other");
        chna.entries.truncate(2);
        assert_eq!(&*chna.summary().label, "ADM \u{b7} 2 bed");
    }

    #[test]
    fn type_codes_come_from_the_id_digits() {
        assert_eq!(type_of_id("AP_00031001"), Some(TYPE_OBJECTS));
        assert_eq!(type_of_id("AC_0001000a"), Some(TYPE_DIRECT_SPEAKERS));
        assert_eq!(type_of_id("AT_00041001_01"), Some(TYPE_HOA));
        assert_eq!(type_of_id("x"), None);
    }
}
