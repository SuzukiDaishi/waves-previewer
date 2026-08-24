//! ISO-BMFF demux for the video track.
//!
//! The audio side also probes mp4 through the `mp4` crate (including the
//! unsupported-AAC check); this is the same reader pointed at
//! the other kind of track. It answers three questions and nothing else:
//! what is in this file, when is each picture shown, and which bytes make up
//! the picture at a given time.
//!
//! The sample index is built from the `stts` / `ctts` / `stss` tables rather
//! than by walking samples, so opening a two-hour movie reads its header and
//! not its body.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result};
use mp4::{MediaType, Mp4Reader, TrackType};

use super::annexb::ParameterSets;
use super::frame::Rotation;

/// Refuse to index a track with more samples than this.
///
/// 2 million samples is over 9 hours at 60 fps, and the index costs ~20 bytes
/// each. Past it, the file is either pathological or not what this preview is
/// for; the caller degrades to showing the poster frame alone rather than
/// spending 40 MB and a visible pause on an index nobody asked for.
pub const MAX_INDEXED_SAMPLES: usize = 2_000_000;

/// What the container says about the video track, before anything is decoded.
#[derive(Clone, Debug)]
pub struct VideoStreamInfo {
    /// Storage dimensions, before rotation.
    pub coded_width: u32,
    pub coded_height: u32,
    /// Display dimensions, after rotation.
    pub display_width: u32,
    pub display_height: u32,
    pub rotation: Rotation,
    pub duration_secs: f64,
    /// Average frame rate. A hint for cadence only — seeking always uses real
    /// per-sample timestamps, because variable-frame-rate recordings are
    /// common and this number lies about them.
    pub nominal_fps: f32,
    /// Human-readable codec name for the "no preview" message.
    pub codec_label: String,
    /// Whether a decoder in this build can handle the codec.
    pub codec: VideoCodec,
}

impl VideoStreamInfo {
    /// Display aspect ratio, guarded against a zero dimension.
    pub fn aspect(&self) -> f32 {
        if self.display_height == 0 {
            return 16.0 / 9.0;
        }
        (self.display_width as f32 / self.display_height as f32).clamp(0.05, 20.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
    Vp9,
    /// Parsed as a video track, but the sample entry is one the container
    /// reader does not name (ProRes, AV1, encrypted media, ...).
    Unknown,
}

impl VideoCodec {
    pub fn label(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::H265 => "HEVC",
            VideoCodec::Vp9 => "VP9",
            VideoCodec::Unknown => "unknown codec",
        }
    }
}

/// One entry of the presentation-order sample index.
#[derive(Clone, Copy, Debug)]
pub struct SampleEntry {
    /// 1-based sample id, as `Mp4Reader::read_sample` wants it.
    pub id: u32,
    /// Composition (presentation) time in track timescale units, already
    /// shifted by the edit list.
    pub pts: i64,
    pub is_sync: bool,
}

/// Presentation-ordered index of a video track's samples.
pub struct SampleIndex {
    /// Sorted by `pts`. Decode order is `id` order; these differ whenever the
    /// stream has B-frames.
    entries: Vec<SampleEntry>,
    /// `entries` positions of the sync samples, ascending.
    sync_positions: Vec<usize>,
    /// `entries` position of each 1-based sample id, so a decoder feeding in
    /// decode order can find the timestamp of what it just fed.
    position_of_id: Vec<u32>,
    timescale: u32,
}

impl SampleIndex {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn timescale(&self) -> u32 {
        self.timescale
    }

    pub fn entry(&self, position: usize) -> Option<SampleEntry> {
        self.entries.get(position).copied()
    }

    /// Presentation-order position of the sample with this 1-based id.
    pub fn position_of_id(&self, id: u32) -> Option<usize> {
        if id == 0 {
            return None;
        }
        self.position_of_id
            .get(id as usize - 1)
            .map(|pos| *pos as usize)
    }

    pub fn pts_secs(&self, position: usize) -> Option<f64> {
        self.entries
            .get(position)
            .map(|e| e.pts as f64 / self.timescale.max(1) as f64)
    }

    /// Position of the last sample shown at or before `secs`.
    ///
    /// Returns 0 for a time before the first sample, so a seek to the very
    /// start still yields a picture rather than nothing.
    pub fn position_at_or_before(&self, secs: f64) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }
        let target = (secs * self.timescale.max(1) as f64).floor() as i64;
        match self.entries.binary_search_by(|e| e.pts.cmp(&target)) {
            Ok(exact) => Some(exact),
            Err(0) => Some(0),
            Err(insert) => Some(insert - 1),
        }
    }

    /// Position of the latest sync sample at or before `position`.
    ///
    /// A decoder can only start on one of these, so this is where a seek that
    /// cannot be served by walking forward has to restart from.
    pub fn sync_at_or_before(&self, position: usize) -> usize {
        match self.sync_positions.binary_search(&position) {
            Ok(exact) => self.sync_positions[exact],
            Err(0) => 0,
            Err(insert) => self.sync_positions[insert - 1],
        }
    }
}

/// A file open on its video track.
pub struct VideoContainer {
    reader: Mp4Reader<BufReader<File>>,
    track_id: u32,
    pub info: VideoStreamInfo,
    pub params: ParameterSets,
    pub index: SampleIndex,
}

impl VideoContainer {
    /// Open `path` and index its first video track.
    ///
    /// `Err` here means "no video preview": either the file is not ISO-BMFF,
    /// or it has no video track. Neither says anything about the audio track,
    /// which is decoded by a wholly separate path.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open video: {}", path.display()))?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let reader = BufReader::new(file);
        let reader = Mp4Reader::read_header(reader, size)
            .map_err(|e| anyhow::anyhow!("mp4 header: {e:?}"))?;

        let mut picked: Option<u32> = None;
        // HashMap iteration order is arbitrary, so choose the lowest track id
        // rather than "whichever comes out first" — two video tracks (a movie
        // plus a thumbnail track) must resolve the same way every open.
        let mut best_id = u32::MAX;
        for (&track_id, track) in reader.tracks() {
            if matches!(track.track_type(), Ok(TrackType::Video)) && track_id < best_id {
                best_id = track_id;
                picked = Some(track_id);
            }
        }
        let track_id = picked.context("no video track")?;
        let track = reader
            .tracks()
            .get(&track_id)
            .context("video track vanished")?;

        let codec = match track.media_type() {
            Ok(MediaType::H264) => VideoCodec::H264,
            Ok(MediaType::H265) => VideoCodec::H265,
            Ok(MediaType::VP9) => VideoCodec::Vp9,
            _ => VideoCodec::Unknown,
        };
        let matrix = &track.trak.tkhd.matrix;
        let rotation = Rotation::from_matrix(matrix.a, matrix.b, matrix.c, matrix.d);
        let coded_width = track.width() as u32;
        let coded_height = track.height() as u32;
        let (display_width, display_height) = rotation.apply_to_size(coded_width, coded_height);
        let info = VideoStreamInfo {
            coded_width,
            coded_height,
            display_width,
            display_height,
            rotation,
            duration_secs: track.duration().as_secs_f64(),
            nominal_fps: track.frame_rate() as f32,
            codec_label: codec.label().to_string(),
            codec,
        };

        let params = avc_parameter_sets(track);
        let index = build_sample_index(track)?;

        Ok(Self {
            reader,
            track_id,
            info,
            params,
            index,
        })
    }

    /// Raw AVCC bytes of one sample, addressed by index position.
    pub fn read_sample_at(&mut self, position: usize) -> Result<Option<Vec<u8>>> {
        let Some(entry) = self.index.entry(position) else {
            return Ok(None);
        };
        let sample = self
            .reader
            .read_sample(self.track_id, entry.id)
            .map_err(|e| anyhow::anyhow!("read video sample {}: {e:?}", entry.id))?;
        Ok(sample.map(|s| s.bytes.to_vec()))
    }
}

fn avc_parameter_sets(track: &mp4::Mp4Track) -> ParameterSets {
    let Some(avc1) = track.trak.mdia.minf.stbl.stsd.avc1.as_ref() else {
        return ParameterSets::default();
    };
    let avcc = &avc1.avcc;
    ParameterSets::new(
        avcc.length_size_minus_one,
        avcc.sequence_parameter_sets
            .iter()
            .map(|n| n.bytes.clone())
            .collect(),
        avcc.picture_parameter_sets
            .iter()
            .map(|n| n.bytes.clone())
            .collect(),
    )
}

/// Composition-time shift declared by the track's edit list, in media
/// timescale units.
///
/// A phone camera routinely writes one edit whose `media_time` skips the
/// leading composition delay; without honouring it the picture runs a frame or
/// two ahead of the sound. An *empty* edit (`media_time` of -1, which arrives
/// here as an all-ones unsigned value because the crate parses the field
/// unsigned) declares blank presentation time and shifts nothing.
fn edit_list_media_time(track: &mp4::Mp4Track) -> i64 {
    let Some(elst) = track.trak.edts.as_ref().and_then(|e| e.elst.as_ref()) else {
        return 0;
    };
    for entry in &elst.entries {
        let raw = entry.media_time;
        let empty_edit = raw == u64::MAX || raw == u64::from(u32::MAX);
        if empty_edit {
            continue;
        }
        if raw <= i64::MAX as u64 {
            return raw as i64;
        }
    }
    0
}

fn build_sample_index(track: &mp4::Mp4Track) -> Result<SampleIndex> {
    let stbl = &track.trak.mdia.minf.stbl;
    let timescale = track.timescale().max(1);
    let total = track.sample_count() as usize;
    if total == 0 {
        anyhow::bail!("video track has no samples");
    }
    if total > MAX_INDEXED_SAMPLES {
        anyhow::bail!("video track has {total} samples, above the {MAX_INDEXED_SAMPLES} cap");
    }

    // Decode timestamps: run-length expand `stts`.
    let mut dts = Vec::with_capacity(total);
    let mut clock: i64 = 0;
    for entry in &stbl.stts.entries {
        for _ in 0..entry.sample_count {
            if dts.len() >= total {
                break;
            }
            dts.push(clock);
            clock += i64::from(entry.sample_delta);
        }
    }
    // A short or missing `stts` is malformed but recoverable: keep the cadence
    // of the last entry rather than dropping the tail of the movie.
    let fallback_delta = stbl
        .stts
        .entries
        .last()
        .map(|e| i64::from(e.sample_delta))
        .unwrap_or(1)
        .max(1);
    while dts.len() < total {
        dts.push(clock);
        clock += fallback_delta;
    }

    // Composition offsets: run-length expand `ctts`, absent means all zero.
    let mut ctts = vec![0i64; total];
    if let Some(box_ctts) = stbl.ctts.as_ref() {
        let mut i = 0usize;
        for entry in &box_ctts.entries {
            for _ in 0..entry.sample_count {
                if i >= total {
                    break;
                }
                ctts[i] = i64::from(entry.sample_offset);
                i += 1;
            }
        }
    }

    // Sync samples: absent `stss` means every sample is a sync sample, which
    // is how an all-intra codec is signalled.
    let mut is_sync = vec![stbl.stss.is_none(); total];
    if let Some(stss) = stbl.stss.as_ref() {
        for &id in &stss.entries {
            if id >= 1 && (id as usize) <= total {
                is_sync[id as usize - 1] = true;
            }
        }
        // A `stss` that names nothing would leave the track unseekable; treat
        // the first sample as a sync point so at least the start decodes.
        if !is_sync.iter().any(|v| *v) {
            is_sync[0] = true;
        }
    }

    let shift = edit_list_media_time(track);
    let mut entries: Vec<SampleEntry> = (0..total)
        .map(|i| SampleEntry {
            id: i as u32 + 1,
            pts: dts[i] + ctts[i] - shift,
            is_sync: is_sync[i],
        })
        .collect();
    // Presentation order. `sort_by_key` is stable, so samples sharing a
    // timestamp keep decode order between them.
    entries.sort_by_key(|e| e.pts);

    let sync_positions: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(pos, e)| e.is_sync.then_some(pos))
        .collect();
    let sync_positions = if sync_positions.is_empty() {
        vec![0]
    } else {
        sync_positions
    };

    let mut position_of_id = vec![0u32; total];
    for (pos, entry) in entries.iter().enumerate() {
        if let Some(slot) = position_of_id.get_mut(entry.id as usize - 1) {
            *slot = pos as u32;
        }
    }

    Ok(SampleIndex {
        entries,
        sync_positions,
        position_of_id,
        timescale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(entries: Vec<(u32, i64, bool)>, timescale: u32) -> SampleIndex {
        let entries: Vec<SampleEntry> = entries
            .into_iter()
            .map(|(id, pts, is_sync)| SampleEntry { id, pts, is_sync })
            .collect();
        let sync_positions: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.is_sync.then_some(i))
            .collect();
        let mut position_of_id = vec![0u32; entries.len()];
        for (pos, entry) in entries.iter().enumerate() {
            if let Some(slot) = position_of_id.get_mut(entry.id as usize - 1) {
                *slot = pos as u32;
            }
        }
        SampleIndex {
            entries,
            sync_positions,
            position_of_id,
            timescale,
        }
    }

    #[test]
    fn lookup_finds_the_frame_on_screen_at_a_time() {
        // 4 frames at 10 units apart, timescale 100 => 0.0, 0.1, 0.2, 0.3 s.
        let idx = index(
            vec![(1, 0, true), (2, 10, false), (3, 20, false), (4, 30, false)],
            100,
        );
        assert_eq!(idx.position_at_or_before(0.0), Some(0));
        // Mid-frame resolves to the frame that is showing.
        assert_eq!(idx.position_at_or_before(0.15), Some(1));
        assert_eq!(idx.position_at_or_before(0.2), Some(2));
        // Past the end holds on the last frame rather than blanking.
        assert_eq!(idx.position_at_or_before(9.0), Some(3));
        // Before the start still yields the first frame.
        assert_eq!(idx.position_at_or_before(-1.0), Some(0));
    }

    #[test]
    fn sync_lookup_walks_back_to_the_start_of_the_gop() {
        let idx = index(
            vec![
                (1, 0, true),
                (2, 10, false),
                (3, 20, false),
                (4, 30, true),
                (5, 40, false),
            ],
            100,
        );
        assert_eq!(idx.sync_at_or_before(0), 0);
        assert_eq!(idx.sync_at_or_before(2), 0);
        assert_eq!(idx.sync_at_or_before(3), 3);
        assert_eq!(idx.sync_at_or_before(4), 3);
    }

    #[test]
    fn an_empty_index_answers_nothing_rather_than_panicking() {
        let idx = index(Vec::new(), 100);
        assert!(idx.is_empty());
        assert_eq!(idx.position_at_or_before(1.0), None);
        assert_eq!(idx.pts_secs(0), None);
    }

    #[test]
    fn b_frame_composition_offsets_reorder_the_index() {
        // Decode order I P B: the P is coded before the B but shown after it.
        // dts 0,10,20 with ctts 0,+20,0 gives pts 0,30,20.
        let mut entries = vec![
            SampleEntry {
                id: 1,
                pts: 0,
                is_sync: true,
            },
            SampleEntry {
                id: 2,
                pts: 30,
                is_sync: false,
            },
            SampleEntry {
                id: 3,
                pts: 20,
                is_sync: false,
            },
        ];
        entries.sort_by_key(|e| e.pts);
        let ids: Vec<u32> = entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![1, 3, 2], "presentation order, not decode order");
    }
}
