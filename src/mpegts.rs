//! MPEG-2 transport stream demux, for AVCHD (`.mts`) and Blu-ray / camcorder
//! (`.m2ts`) files.
//!
//! Only as much of ISO/IEC 13818-1 as reading those files takes: packet
//! framing (188-byte TS, and the 192-byte packets AVCHD and Blu-ray write, a
//! 4-byte arrival stamp ahead of each), the PAT and the first program's PMT,
//! and PES reassembly for chosen PIDs. No codec knowledge lives here beyond
//! naming stream types; the audio is decoded by [`crate::audio_mpegts`] and
//! the picture by the video backends.
//!
//! [`TsProbe`] reads the first and last few megabytes of a file and nothing
//! in between, so describing a two-hour recording on a file share costs two
//! reads. The decoders stream the whole file through [`PacketReader`].

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result};

/// Bytes in one transport packet, without the M2TS arrival stamp.
pub const TS_PACKET_LEN: usize = 188;
const SYNC_BYTE: u8 = 0x47;
/// The clock every PTS counts in (ISO/IEC 13818-1 §2.4.3.7).
pub const PTS_CLOCK_HZ: f64 = 90_000.0;
/// PTS values are 33 bits and wrap about every 26.5 hours.
const PTS_MODULUS: u64 = 1 << 33;
/// Packets that must line up, one packet length apart, before a framing is
/// believed. Eight 0x47 bytes at the right spacing by chance is far less
/// likely than a damaged packet.
const SYNC_RUN: usize = 8;
/// How far into a file to look for the first packet. Real files start on
/// one; this only tolerates a little junk in front.
const MAX_LEADING_JUNK: usize = 64 * 1024;
/// Bytes read at a time while streaming a file.
const READ_CHUNK: usize = 1 << 20;
/// How much of the start of a file the probe reads, smallest first. The PAT
/// and PMT come first and a camcorder's streams all start within the first
/// half megabyte; the larger window, well over a second of the densest
/// Blu-ray stream, is read only when the small one ends before every stream
/// has shown [`MIN_HEAD_TIMESTAMPS`]. A folder of clips on a file share is
/// read a few hundred kilobytes a file, not megabytes.
const HEAD_WINDOWS: [u64; 2] = [512 << 10, 4 << 20];
/// The tail windows the probe tries, smallest first, when looking for each
/// stream's last timestamp. A stream absent from the last megabyte (audio
/// that ended early) widens the search, but never past the last.
const TAIL_WINDOWS: [u64; 4] = [1 << 20, 4 << 20, 16 << 20, 64 << 20];
/// Timestamps each stream must show in a head window before a larger one is
/// unnecessary: enough for the earliest picture and a frame step.
const MIN_HEAD_TIMESTAMPS: usize = 3;
/// Payload bytes an audio stream must show in a head window: a codec header.
const MIN_HEAD_PAYLOAD_BYTES: usize = 16;
/// PES packets per stream the probe collects timestamps from at the start.
const HEAD_PES_PER_STREAM: usize = 48;
/// Payload bytes kept from the start of each stream, for codec headers.
const HEAD_PAYLOAD_BYTES: usize = 16 * 1024;
/// PIDs that are tables, never elementary streams.
const PAT_PID: u16 = 0x0000;
const NULL_PID: u16 = 0x1FFF;

/// How packets are framed in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketFormat {
    /// Plain 188-byte packets.
    Ts,
    /// 192-byte packets: a 4-byte arrival timestamp ahead of each 188-byte
    /// packet. What AVCHD and Blu-ray write, whatever the extension says.
    M2ts,
}

impl PacketFormat {
    pub fn packet_len(self) -> usize {
        match self {
            PacketFormat::Ts => TS_PACKET_LEN,
            PacketFormat::M2ts => TS_PACKET_LEN + 4,
        }
    }

    fn sync_offset(self) -> usize {
        match self {
            PacketFormat::Ts => 0,
            PacketFormat::M2ts => 4,
        }
    }
}

/// Count the 0x47 bytes one packet length apart from `sync_at`, up to
/// [`SYNC_RUN`].
fn sync_run(bytes: &[u8], sync_at: usize, packet_len: usize) -> usize {
    (0..SYNC_RUN)
        .map(|i| sync_at + i * packet_len)
        .take_while(|&at| bytes.get(at) == Some(&SYNC_BYTE))
        .count()
}

/// Whether a run of `run` packets from `sync_at` is as long as `bytes` lets
/// it be: the full [`SYNC_RUN`], or every packet a short file holds.
fn run_is_convincing(bytes: &[u8], sync_at: usize, packet_len: usize, run: usize) -> bool {
    let room = bytes.len().saturating_sub(sync_at).div_ceil(packet_len);
    run > 0 && run >= SYNC_RUN.min(room)
}

/// The framing of `head` and the offset of its first whole packet.
///
/// Decided from the bytes, not the extension: a 188-byte stream saved as
/// `.mts` is as readable as a 192-byte one.
pub fn detect_format(head: &[u8]) -> Option<(PacketFormat, usize)> {
    let limit = head.len().min(MAX_LEADING_JUNK);
    for sync_at in 0..limit {
        if head[sync_at] != SYNC_BYTE {
            continue;
        }
        // M2TS first: its stamps make a 188-byte check fail, never the other
        // way round, so the order only matters for a one-packet file.
        for format in [PacketFormat::M2ts, PacketFormat::Ts] {
            let Some(start) = sync_at.checked_sub(format.sync_offset()) else {
                continue;
            };
            let run = sync_run(head, sync_at, format.packet_len());
            if run_is_convincing(head, sync_at, format.packet_len(), run) {
                return Some((format, start));
            }
        }
    }
    None
}

/// Whether the file at `path` holds a transport stream: what a `.mts` /
/// `.m2ts` must show to get a list row, since TypeScript writes `.mts` too
/// (`index.d.mts`). Reads at most the head [`detect_format`] looks at. A file
/// that cannot be read says nothing either way, and counts as one: its row
/// then reports why, rather than vanishing over a passing share hiccup.
pub fn file_looks_like_transport_stream(path: &Path) -> bool {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return true;
    };
    let window = MAX_LEADING_JUNK + SYNC_RUN * PacketFormat::M2ts.packet_len();
    let mut head = Vec::new();
    if file.take(window as u64).read_to_end(&mut head).is_err() {
        return true;
    }
    detect_format(&head).is_some()
}

/// One transport packet's header and payload.
#[derive(Clone, Copy, Debug)]
pub struct TsPacket<'a> {
    pub pid: u16,
    /// payload_unit_start_indicator: a PES packet or a section starts here.
    pub unit_start: bool,
    /// The transport error indicator: the demodulator or the camera knew this
    /// packet was damaged.
    pub transport_error: bool,
    pub continuity: u8,
    pub payload: &'a [u8],
}

/// Parse one 188-byte packet that starts with the sync byte.
fn parse_packet(packet: &[u8]) -> TsPacket<'_> {
    let pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
    let adaptation = (packet[3] >> 4) & 0x3;
    let mut payload: &[u8] = &[];
    if adaptation & 0x1 != 0 {
        let start = if adaptation & 0x2 != 0 {
            5 + usize::from(packet[4])
        } else {
            4
        };
        if start <= TS_PACKET_LEN {
            payload = &packet[start..TS_PACKET_LEN];
        }
    }
    TsPacket {
        pid,
        unit_start: packet[1] & 0x40 != 0,
        transport_error: packet[1] & 0x80 != 0,
        continuity: packet[3] & 0x0F,
        payload,
    }
}

/// Transport packets out of any byte source, in file order.
///
/// Keeps its own buffer rather than a `BufReader` so that losing sync can be
/// confirmed against the next packet before it is trusted again.
pub struct PacketReader<R> {
    src: R,
    format: PacketFormat,
    buf: Vec<u8>,
    start: usize,
    end: usize,
    eof: bool,
    synced: bool,
    /// Bytes dropped while hunting for sync.
    pub skipped_bytes: u64,
    /// Times sync was lost after it had been found.
    pub resyncs: u32,
}

impl<R: Read> PacketReader<R> {
    pub fn new(src: R, format: PacketFormat) -> Self {
        Self {
            src,
            format,
            buf: vec![0; READ_CHUNK],
            start: 0,
            end: 0,
            eof: false,
            synced: false,
            skipped_bytes: 0,
            resyncs: 0,
        }
    }

    /// Make at least `need` unread bytes available; false at end of input.
    fn fill(&mut self, need: usize) -> io::Result<bool> {
        if self.end - self.start >= need {
            return Ok(true);
        }
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        while self.end < need && !self.eof {
            match self.src.read(&mut self.buf[self.end..]) {
                Ok(0) => self.eof = true,
                Ok(n) => self.end += n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(self.end - self.start >= need)
    }

    /// The next packet, or `None` once fewer than a packet's bytes remain.
    pub fn next_packet(&mut self) -> io::Result<Option<TsPacket<'_>>> {
        let len = self.format.packet_len();
        let offset = self.format.sync_offset();
        loop {
            if !self.fill(len)? {
                return Ok(None);
            }
            let sync_at = self.start + offset;
            if self.buf[sync_at] == SYNC_BYTE {
                if self.synced {
                    break;
                }
                // Not yet trusted: the next packet must line up too, unless
                // this is the last one in the file.
                let confirmed = if self.fill(2 * len)? {
                    self.buf[self.start + len + offset] == SYNC_BYTE
                } else {
                    true
                };
                if confirmed {
                    self.synced = true;
                    break;
                }
            } else if self.synced {
                self.synced = false;
                self.resyncs = self.resyncs.saturating_add(1);
            }
            self.start += 1;
            self.skipped_bytes = self.skipped_bytes.saturating_add(1);
        }
        let at = self.start + offset;
        self.start += len;
        Ok(Some(parse_packet(&self.buf[at..at + TS_PACKET_LEN])))
    }
}

/// One reassembled PES packet's timing and payload.
#[derive(Clone, Debug)]
pub struct Pes {
    pub pid: u16,
    /// Presentation time in 90 kHz ticks, when the packet carries one.
    pub pts: Option<u64>,
    /// The elementary stream bytes after the PES header.
    pub data: Vec<u8>,
    /// A packet of this PES was flagged damaged, or one went missing.
    pub damaged: bool,
}

/// Read a 33-bit timestamp from the five bytes it is spread across.
fn read_timestamp(b: &[u8]) -> u64 {
    (u64::from(b[0] >> 1) & 0x07) << 30
        | u64::from(b[1]) << 22
        | u64::from(b[2] >> 1) << 15
        | u64::from(b[3]) << 7
        | u64::from(b[4] >> 1)
}

/// Stream ids whose PES packets have no optional header (§2.4.3.7): padding,
/// private_stream_2, and the system/directory streams.
fn stream_id_has_header(stream_id: u8) -> bool {
    !matches!(
        stream_id,
        0xBC | 0xBE | 0xBF | 0xF0 | 0xF1 | 0xF2 | 0xF8 | 0xFF
    )
}

/// The PTS of a PES packet that starts with `bytes`, if it has one. Needs
/// only the header, which always fits in the first transport packet.
pub fn pes_header_pts(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 9 || bytes[0..3] != [0, 0, 1] || !stream_id_has_header(bytes[3]) {
        return None;
    }
    let has_pts = bytes[7] & 0x80 != 0;
    (has_pts && bytes.len() >= 14).then(|| read_timestamp(&bytes[9..14]))
}

/// Split a whole PES packet into its timestamp and payload.
fn parse_pes(pid: u16, raw: &[u8], damaged: bool) -> Option<Pes> {
    if raw.len() < 6 || raw[0..3] != [0, 0, 1] {
        return None;
    }
    let declared = usize::from(u16::from_be_bytes([raw[4], raw[5]]));
    // Zero means "unbounded", which video PES packets are allowed to say.
    let raw = if declared > 0 && 6 + declared < raw.len() {
        &raw[..6 + declared]
    } else {
        raw
    };
    if !stream_id_has_header(raw[3]) {
        return Some(Pes {
            pid,
            pts: None,
            data: raw[6..].to_vec(),
            damaged,
        });
    }
    if raw.len() < 9 {
        return None;
    }
    let payload_at = 9 + usize::from(raw[8]);
    if payload_at > raw.len() {
        return None;
    }
    Some(Pes {
        pid,
        pts: pes_header_pts(raw),
        data: raw[payload_at..].to_vec(),
        damaged,
    })
}

/// Collects one PID's packets into whole PES packets.
#[derive(Default)]
pub struct PesAssembler {
    buf: Vec<u8>,
    active: bool,
    damaged: bool,
    last_continuity: Option<u8>,
}

impl PesAssembler {
    /// Feed the next packet of this PID. Returns the PES packet this one
    /// closed, if it started a new one.
    pub fn push(&mut self, packet: &TsPacket<'_>) -> Option<Pes> {
        if packet.payload.is_empty() {
            return None;
        }
        // The same counter twice is the one duplicate the standard allows;
        // anything else out of sequence means a packet went missing.
        if let Some(last) = self.last_continuity {
            if packet.continuity == last {
                return None;
            }
            if packet.continuity != (last + 1) & 0x0F {
                self.damaged = true;
            }
        }
        self.last_continuity = Some(packet.continuity);
        let mut closed = None;
        if packet.unit_start {
            closed = self.take(packet.pid);
            self.active = true;
            self.damaged = packet.transport_error;
        } else if !self.active {
            // The middle of a PES whose start was never seen.
            return None;
        } else if packet.transport_error {
            self.damaged = true;
        }
        self.buf.extend_from_slice(packet.payload);
        closed
    }

    /// The PES packet still being collected, at the end of the input.
    pub fn finish(&mut self, pid: u16) -> Option<Pes> {
        self.take(pid)
    }

    fn take(&mut self, pid: u16) -> Option<Pes> {
        if !self.active || self.buf.is_empty() {
            return None;
        }
        let pes = parse_pes(pid, &self.buf, self.damaged);
        self.buf.clear();
        self.damaged = false;
        pes
    }
}

/// CRC-32/MPEG-2, which every PSI section ends with.
fn crc32_mpeg2(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Collects one PID's packets into whole PSI sections.
#[derive(Default)]
struct SectionAssembler {
    buf: Vec<u8>,
    active: bool,
}

impl SectionAssembler {
    /// Feed a packet; returns every section it completed, CRC-checked.
    fn push(&mut self, packet: &TsPacket<'_>) -> Vec<Vec<u8>> {
        let mut done = Vec::new();
        let mut payload = packet.payload;
        if packet.unit_start {
            let Some((&pointer, rest)) = payload.split_first() else {
                return done;
            };
            let pointer = usize::from(pointer).min(rest.len());
            if self.active {
                self.buf.extend_from_slice(&rest[..pointer]);
                self.drain(&mut done);
            }
            self.buf.clear();
            self.active = true;
            payload = &rest[pointer..];
        } else if !self.active {
            return done;
        }
        self.buf.extend_from_slice(payload);
        self.drain(&mut done);
        done
    }

    fn drain(&mut self, done: &mut Vec<Vec<u8>>) {
        loop {
            // 0xFF is stuffing: the rest of the packet is padding.
            if self.buf.first().is_none_or(|&table_id| table_id == 0xFF) {
                self.buf.clear();
                return;
            }
            if self.buf.len() < 3 {
                return;
            }
            let len = 3 + ((usize::from(self.buf[1] & 0x0F) << 8) | usize::from(self.buf[2]));
            if self.buf.len() < len {
                return;
            }
            let section: Vec<u8> = self.buf.drain(..len).collect();
            if section.len() >= 12 && crc32_mpeg2(&section) == 0 {
                done.push(section);
            }
        }
    }
}

/// What a PMT says an elementary stream is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    H264,
    Hevc,
    MpegVideo,
    Ac3,
    Eac3,
    /// Blu-ray / AVCHD LPCM (stream type 0x80 in an HDMV stream).
    Lpcm,
    Dts,
    TrueHd,
    Aac,
    MpegAudio,
}

impl StreamKind {
    pub fn is_video(self) -> bool {
        matches!(
            self,
            StreamKind::H264 | StreamKind::Hevc | StreamKind::MpegVideo
        )
    }

    pub fn is_audio(self) -> bool {
        !self.is_video()
    }

    /// The name the UI uses, including in `<codec> UNSUPPORTED`.
    pub fn label(self) -> &'static str {
        match self {
            StreamKind::H264 => "H.264",
            StreamKind::Hevc => "HEVC",
            StreamKind::MpegVideo => "MPEG-2 video",
            StreamKind::Ac3 => "AC-3",
            StreamKind::Eac3 => "E-AC-3",
            StreamKind::Lpcm => "LPCM",
            StreamKind::Dts => "DTS",
            StreamKind::TrueHd => "TrueHD",
            StreamKind::Aac => "AAC",
            StreamKind::MpegAudio => "MPEG audio",
        }
    }

    /// Classify a PMT entry from its stream type and descriptors.
    fn classify(stream_type: u8, descriptors: &[u8]) -> Option<Self> {
        Some(match stream_type {
            0x01 | 0x02 => StreamKind::MpegVideo,
            0x1B => StreamKind::H264,
            0x24 => StreamKind::Hevc,
            0x03 | 0x04 => StreamKind::MpegAudio,
            0x0F | 0x11 => StreamKind::Aac,
            0x80 => StreamKind::Lpcm,
            0x81 => StreamKind::Ac3,
            0x82 | 0x85 | 0x86 | 0xA2 => StreamKind::Dts,
            0x83 => StreamKind::TrueHd,
            0x84 | 0x87 | 0xA1 => StreamKind::Eac3,
            // Private data: what it is lives in the descriptors (DVB).
            0x06 => return Self::from_descriptors(descriptors),
            _ => return None,
        })
    }

    fn from_descriptors(mut descriptors: &[u8]) -> Option<Self> {
        while descriptors.len() >= 2 {
            let tag = descriptors[0];
            let len = usize::from(descriptors[1]);
            let body = descriptors.get(2..2 + len)?;
            match tag {
                0x6A => return Some(StreamKind::Ac3),
                0x7A => return Some(StreamKind::Eac3),
                0x7B => return Some(StreamKind::Dts),
                0x7C => return Some(StreamKind::Aac),
                // registration_descriptor: a four-character format id.
                0x05 if body.len() >= 4 => match &body[..4] {
                    b"AC-3" => return Some(StreamKind::Ac3),
                    b"EAC3" => return Some(StreamKind::Eac3),
                    b"DTS1" | b"DTS2" | b"DTS3" => return Some(StreamKind::Dts),
                    _ => {}
                },
                _ => {}
            }
            descriptors = &descriptors[2 + len..];
        }
        None
    }
}

/// One elementary stream, as the probe found it.
#[derive(Clone, Debug)]
pub struct TsStream {
    pub pid: u16,
    pub stream_type: u8,
    pub kind: StreamKind,
    /// The earliest presentation time among the stream's first packets.
    /// Not simply the first one: with B-frames the first picture in decode
    /// order is not the first on screen.
    pub first_pts: Option<u64>,
    /// The latest presentation time in the tail of the file.
    pub last_pts: Option<u64>,
    /// The usual distance between consecutive timestamps at the start: a
    /// video frame, or one audio PES packet.
    pub pts_step: Option<u64>,
    /// Payload bytes from the stream's first PES packets, for codec headers.
    pub head_payload: Vec<u8>,
    /// How many timestamps the head window held.
    pub head_timestamps: usize,
}

impl TsStream {
    /// Seconds from the first to the last timestamp, plus one step, when
    /// both ends were found.
    pub fn span_secs(&self) -> Option<f64> {
        let first = self.first_pts?;
        let last = self.last_pts?;
        let span = pts_delta(last, first);
        (span >= 0).then(|| (span as u64 + self.pts_step.unwrap_or(0)) as f64 / PTS_CLOCK_HZ)
    }
}

/// `later - earlier` in 90 kHz ticks, across a 33-bit wrap. Values more than
/// half the clock range apart are taken to be the other way round.
pub fn pts_delta(later: u64, earlier: u64) -> i64 {
    let diff = later.wrapping_sub(earlier) % PTS_MODULUS;
    if diff >= PTS_MODULUS / 2 {
        diff as i64 - PTS_MODULUS as i64
    } else {
        diff as i64
    }
}

/// What is in a transport stream file, from its first and last megabytes.
#[derive(Clone, Debug)]
pub struct TsProbe {
    pub format: PacketFormat,
    /// Offset of the first whole packet.
    pub first_packet: u64,
    pub file_size: u64,
    /// The first program's streams, in PMT order. Streams of a kind this
    /// module does not name (subtitles, data, the MVC half of a 3D clip)
    /// are left out.
    pub streams: Vec<TsStream>,
}

impl TsProbe {
    /// Read the head and the tail of `path`: streams, codec headers, and
    /// first and last timestamps.
    pub fn open(path: &Path) -> Result<Self> {
        let mut probe = Self::open_head(path)?;
        probe.scan_tail(path)?;
        Ok(probe)
    }

    /// Read only the head: streams and codec headers, without last
    /// timestamps. Enough to start decoding.
    pub fn open_head(path: &Path) -> Result<Self> {
        let mut file =
            File::open(path).with_context(|| format!("open mpeg-ts: {}", path.display()))?;
        let file_size = file
            .metadata()
            .with_context(|| format!("stat mpeg-ts: {}", path.display()))?
            .len();
        let mut head = Vec::new();
        for (index, window) in HEAD_WINDOWS.into_iter().enumerate() {
            let more = window.saturating_sub(head.len() as u64);
            (&mut file)
                .take(more)
                .read_to_end(&mut head)
                .with_context(|| format!("read mpeg-ts: {}", path.display()))?;
            let last = index + 1 == HEAD_WINDOWS.len() || head.len() as u64 >= file_size;
            match Self::from_head(&head) {
                Ok(mut probe) if last || probe.head_is_complete() => {
                    probe.file_size = file_size;
                    return Ok(probe);
                }
                Err(err) if last => return Err(err),
                Ok(_) | Err(_) => {}
            }
        }
        unreachable!("the last head window always returns")
    }

    /// Whether the head window showed every stream starting, so a larger one
    /// would add nothing.
    fn head_is_complete(&self) -> bool {
        !self.streams.is_empty()
            && self.streams.iter().all(|stream| {
                stream.head_timestamps >= MIN_HEAD_TIMESTAMPS
                    && (stream.kind.is_video()
                        || stream.head_payload.len() >= MIN_HEAD_PAYLOAD_BYTES)
            })
    }

    /// Parse a head window already in memory.
    pub fn from_head(head: &[u8]) -> Result<Self> {
        let (format, first_packet) =
            detect_format(head).context("not an MPEG transport stream (no packet sync)")?;
        let streams = read_program(&head[first_packet..], format)?;
        let mut probe = Self {
            format,
            first_packet: first_packet as u64,
            file_size: head.len() as u64,
            streams,
        };
        probe.scan_head(&head[first_packet..]);
        Ok(probe)
    }

    fn scan_head(&mut self, head: &[u8]) {
        let mut reader = PacketReader::new(head, self.format);
        let mut assemblers: Vec<PesAssembler> = self
            .streams
            .iter()
            .map(|_| PesAssembler::default())
            .collect();
        let mut pts_seen: Vec<Vec<u64>> = self.streams.iter().map(|_| Vec::new()).collect();
        let mut keep = |index: usize, pes: Pes, streams: &mut [TsStream]| {
            let stream = &mut streams[index];
            if let Some(pts) = pes.pts {
                if pts_seen[index].len() < HEAD_PES_PER_STREAM {
                    pts_seen[index].push(pts);
                }
            }
            let room = HEAD_PAYLOAD_BYTES.saturating_sub(stream.head_payload.len());
            if room > 0 && !pes.damaged {
                let take = room.min(pes.data.len());
                stream.head_payload.extend_from_slice(&pes.data[..take]);
            }
        };
        while let Ok(Some(packet)) = reader.next_packet() {
            let Some(index) = self.streams.iter().position(|s| s.pid == packet.pid) else {
                continue;
            };
            if let Some(pes) = assemblers[index].push(&packet) {
                keep(index, pes, &mut self.streams);
            }
        }
        // The packet the window cut through is incomplete; it still has a
        // timestamp, but its payload is only good for a header.
        for (index, assembler) in assemblers.iter_mut().enumerate() {
            let pid = self.streams[index].pid;
            if let Some(pes) = assembler.finish(pid) {
                keep(index, pes, &mut self.streams);
            }
        }
        for (stream, seen) in self.streams.iter_mut().zip(pts_seen) {
            stream.head_timestamps = seen.len();
            let Some(&anchor) = seen.first() else {
                continue;
            };
            let mut offsets: Vec<i64> = seen.iter().map(|&p| pts_delta(p, anchor)).collect();
            offsets.sort_unstable();
            offsets.dedup();
            let earliest = offsets[0];
            stream.first_pts = Some(anchor.wrapping_add_signed(earliest) % PTS_MODULUS);
            // The smallest gap, not the median: with only a few pictures in
            // the window, B-frames leave gaps of two or three frames that a
            // median can land on.
            stream.pts_step = offsets
                .windows(2)
                .map(|w| w[1] - w[0])
                .min()
                .map(|step| step as u64);
        }
    }

    /// Find each stream's last timestamp in the end of the file, widening the
    /// window while a stream that started has not been seen ending.
    fn scan_tail(&mut self, path: &Path) -> Result<()> {
        let mut file =
            File::open(path).with_context(|| format!("open mpeg-ts: {}", path.display()))?;
        for window in TAIL_WINDOWS {
            let start = self.file_size.saturating_sub(window).max(self.first_packet);
            file.seek(SeekFrom::Start(start))
                .with_context(|| format!("seek mpeg-ts: {}", path.display()))?;
            let mut tail = Vec::new();
            (&mut file)
                .take(window)
                .read_to_end(&mut tail)
                .with_context(|| format!("read mpeg-ts: {}", path.display()))?;
            self.scan_tail_bytes(&tail);
            let missing = self
                .streams
                .iter()
                .any(|s| s.first_pts.is_some() && s.last_pts.is_none());
            if !missing || start <= self.first_packet {
                break;
            }
        }
        Ok(())
    }

    /// Record the latest timestamp of each stream in `tail`, which may start
    /// in the middle of a packet.
    pub fn scan_tail_bytes(&mut self, tail: &[u8]) {
        let mut reader = PacketReader::new(tail, self.format);
        while let Ok(Some(packet)) = reader.next_packet() {
            if !packet.unit_start {
                continue;
            }
            let Some(stream) = self.streams.iter_mut().find(|s| s.pid == packet.pid) else {
                continue;
            };
            let (Some(pts), Some(first)) = (pes_header_pts(packet.payload), stream.first_pts)
            else {
                continue;
            };
            let later = stream
                .last_pts
                .is_none_or(|last| pts_delta(pts, first) > pts_delta(last, first));
            if later {
                stream.last_pts = Some(pts);
            }
        }
    }

    pub fn first_video(&self) -> Option<&TsStream> {
        self.streams.iter().find(|s| s.kind.is_video())
    }

    pub fn audio_streams(&self) -> impl Iterator<Item = &TsStream> {
        self.streams.iter().filter(|s| s.kind.is_audio())
    }

    /// Seconds from `zero` to `pts`, negative when `pts` is earlier.
    pub fn secs_between(pts: u64, zero: u64) -> f64 {
        pts_delta(pts, zero) as f64 / PTS_CLOCK_HZ
    }
}

/// Read the PAT and the first program's PMT from the start of a stream.
fn read_program(bytes: &[u8], format: PacketFormat) -> Result<Vec<TsStream>> {
    let mut reader = PacketReader::new(bytes, format);
    let mut pat = SectionAssembler::default();
    let mut pmt = SectionAssembler::default();
    let mut pmt_pid: Option<u16> = None;
    while let Some(packet) = reader.next_packet()? {
        if packet.pid == PAT_PID && pmt_pid.is_none() {
            for section in pat.push(&packet) {
                pmt_pid = first_program_pmt_pid(&section);
                if pmt_pid.is_some() {
                    break;
                }
            }
        } else if Some(packet.pid) == pmt_pid {
            for section in pmt.push(&packet) {
                if let Some(streams) = parse_pmt(&section) {
                    return Ok(streams);
                }
            }
        }
    }
    if pmt_pid.is_none() {
        anyhow::bail!(
            "mpeg-ts: no program table in the first {} KB",
            bytes.len() >> 10
        );
    }
    anyhow::bail!(
        "mpeg-ts: no stream map in the first {} KB",
        bytes.len() >> 10
    )
}

/// The PMT PID of the first real program in a PAT section.
fn first_program_pmt_pid(section: &[u8]) -> Option<u16> {
    if section[0] != 0x00 {
        return None;
    }
    let body_end = section.len() - 4;
    section
        .get(8..body_end)?
        .chunks_exact(4)
        .find(|entry| u16::from_be_bytes([entry[0], entry[1]]) != 0)
        .map(|entry| (u16::from(entry[2] & 0x1F) << 8) | u16::from(entry[3]))
        .filter(|&pid| pid != NULL_PID)
}

/// The elementary streams of a PMT section, in its order.
fn parse_pmt(section: &[u8]) -> Option<Vec<TsStream>> {
    if section[0] != 0x02 || section.len() < 16 {
        return None;
    }
    let body_end = section.len() - 4;
    let program_info_len = (usize::from(section[10] & 0x0F) << 8) | usize::from(section[11]);
    let mut at = 12 + program_info_len;
    let mut streams = Vec::new();
    while at + 5 <= body_end {
        let stream_type = section[at];
        let pid = (u16::from(section[at + 1] & 0x1F) << 8) | u16::from(section[at + 2]);
        let info_len = (usize::from(section[at + 3] & 0x0F) << 8) | usize::from(section[at + 4]);
        let descriptors = section.get(at + 5..(at + 5 + info_len).min(body_end))?;
        if let Some(kind) = StreamKind::classify(stream_type, descriptors) {
            streams.push(TsStream {
                pid,
                stream_type,
                kind,
                first_pts: None,
                last_pts: None,
                pts_step: None,
                head_payload: Vec::new(),
                head_timestamps: 0,
            });
        }
        at += 5 + info_len;
    }
    Some(streams)
}

/// Whether a path names a transport stream this app opens (`.mts` /
/// `.m2ts`). `.ts` is left out on purpose: it is also TypeScript source.
pub fn is_mpegts_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            MPEGTS_EXTS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// The transport stream extensions, a subset of
/// [`crate::audio_io::SUPPORTED_VIDEO_EXTS`].
pub const MPEGTS_EXTS: &[&str] = &["mts", "m2ts"];

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_typescript_module_is_not_a_transport_stream() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves_ts_sniff_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let typescript = dir.join("index.d.mts");
        std::fs::write(
            &typescript,
            "export declare function map<T>(xs: T[], f: (x: T) => T): T[];\n".repeat(200),
        )
        .expect("write");
        assert!(!file_looks_like_transport_stream(&typescript));
        let empty = dir.join("empty.mts");
        std::fs::write(&empty, b"").expect("write");
        assert!(!file_looks_like_transport_stream(&empty));
        for format in [PacketFormat::Ts, PacketFormat::M2ts] {
            let mut writer = TsWriter::new(format);
            for _ in 0..12 {
                writer.packet(0x100, false, &[0u8; 184]);
            }
            let stream = dir.join(format!("{format:?}.m2ts"));
            std::fs::write(&stream, &writer.out).expect("write");
            assert!(file_looks_like_transport_stream(&stream), "{format:?}");
        }
        assert!(
            file_looks_like_transport_stream(&dir.join("missing.mts")),
            "unreadable is not proof of anything"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Builds transport streams byte by byte, so every framing case can be
    /// tested without a fixture file.
    pub(crate) struct TsWriter {
        pub format: PacketFormat,
        pub out: Vec<u8>,
        continuity: std::collections::HashMap<u16, u8>,
    }

    impl TsWriter {
        pub fn new(format: PacketFormat) -> Self {
            Self {
                format,
                out: Vec::new(),
                continuity: Default::default(),
            }
        }

        /// One packet. The payload is padded with an adaptation field, the
        /// way a muxer fills the last packet of a PES.
        pub fn packet(&mut self, pid: u16, unit_start: bool, payload: &[u8]) {
            assert!(payload.len() <= 184);
            let cc = self.continuity.entry(pid).or_insert(0);
            let mut packet = vec![SYNC_BYTE, (pid >> 8) as u8 & 0x1F, pid as u8, 0];
            if unit_start {
                packet[1] |= 0x40;
            }
            let stuffing = 184 - payload.len();
            if stuffing == 0 {
                packet[3] = 0x10 | *cc;
            } else {
                packet[3] = 0x30 | *cc;
                packet.push((stuffing - 1) as u8);
                if stuffing > 1 {
                    packet.push(0x00);
                    packet.extend(std::iter::repeat_n(0xFF, stuffing - 2));
                }
            }
            packet.extend_from_slice(payload);
            assert_eq!(packet.len(), TS_PACKET_LEN);
            *cc = (*cc + 1) & 0x0F;
            if self.format == PacketFormat::M2ts {
                self.out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
            }
            self.out.extend_from_slice(&packet);
        }

        /// A PSI section on `pid`, with its CRC.
        pub fn section(&mut self, pid: u16, table_id: u8, body: &[u8]) {
            let len = 5 + body.len() + 4;
            let mut section = vec![
                table_id,
                0xB0 | (len >> 8) as u8,
                len as u8,
                0,
                1,
                0xC1,
                0,
                0,
            ];
            section.extend_from_slice(body);
            let crc = crc32_mpeg2(&section);
            section.extend_from_slice(&crc.to_be_bytes());
            let mut payload = vec![0u8];
            payload.extend_from_slice(&section);
            self.packet(pid, true, &payload);
        }

        /// A PAT naming one program whose PMT is on `pmt_pid`.
        pub fn pat(&mut self, pmt_pid: u16) {
            self.section(
                PAT_PID,
                0x00,
                &[0, 1, 0xE0 | (pmt_pid >> 8) as u8, pmt_pid as u8],
            );
        }

        /// A PMT listing `(stream_type, pid, descriptors)`.
        pub fn pmt(&mut self, pmt_pid: u16, streams: &[(u8, u16, &[u8])]) {
            let mut body = vec![0xE1, 0x00, 0xF0, 0x00];
            for (stream_type, pid, descriptors) in streams {
                body.push(*stream_type);
                body.push(0xE0 | (pid >> 8) as u8);
                body.push(*pid as u8);
                body.push(0xF0 | (descriptors.len() >> 8) as u8);
                body.push(descriptors.len() as u8);
                body.extend_from_slice(descriptors);
            }
            self.section(pmt_pid, 0x02, &body);
        }

        /// A PES packet, split across as many transport packets as it needs.
        pub fn pes(&mut self, pid: u16, stream_id: u8, pts: Option<u64>, data: &[u8]) {
            let mut header = vec![0, 0, 1, stream_id, 0, 0, 0x80];
            match pts {
                Some(pts) => {
                    header.push(0x80);
                    header.push(5);
                    header.extend_from_slice(&encode_timestamp(0x2, pts));
                }
                None => {
                    header.push(0x00);
                    header.push(0);
                }
            }
            let pes_len = header.len() - 6 + data.len();
            if pes_len <= 0xFFFF {
                header[4] = (pes_len >> 8) as u8;
                header[5] = pes_len as u8;
            }
            let mut whole = header;
            whole.extend_from_slice(data);
            for (index, chunk) in whole.chunks(184).enumerate() {
                self.packet(pid, index == 0, chunk);
            }
        }
    }

    fn encode_timestamp(marker: u8, pts: u64) -> [u8; 5] {
        [
            (marker << 4) | (((pts >> 30) as u8 & 0x07) << 1) | 1,
            (pts >> 22) as u8,
            (((pts >> 15) as u8 & 0x7F) << 1) | 1,
            (pts >> 7) as u8,
            ((pts as u8 & 0x7F) << 1) | 1,
        ]
    }

    const PMT_PID: u16 = 0x100;
    const VIDEO_PID: u16 = 0x1011;
    const AUDIO_PID: u16 = 0x1100;

    /// Video every 3003 ticks (29.97 fps) and audio every 2880 (AC-3 at
    /// 48 kHz), the way a camcorder interleaves them.
    fn camcorder_stream(format: PacketFormat, video_start: u64, audio_start: u64) -> Vec<u8> {
        let mut w = TsWriter::new(format);
        w.pat(PMT_PID);
        w.pmt(PMT_PID, &[(0x1B, VIDEO_PID, &[]), (0x81, AUDIO_PID, &[])]);
        for i in 0..30u64 {
            w.pes(
                VIDEO_PID,
                0xE0,
                Some((video_start + i * 3003) % PTS_MODULUS),
                &vec![i as u8; 700],
            );
            w.pes(
                AUDIO_PID,
                0xBD,
                Some((audio_start + i * 2880) % PTS_MODULUS),
                &[0x0B, 0x77, i as u8, 0x55],
            );
        }
        w.out
    }

    #[test]
    fn both_packet_framings_are_recognised_from_the_bytes() {
        let ts = camcorder_stream(PacketFormat::Ts, 0, 0);
        let m2ts = camcorder_stream(PacketFormat::M2ts, 0, 0);
        assert_eq!(detect_format(&ts), Some((PacketFormat::Ts, 0)));
        assert_eq!(detect_format(&m2ts), Some((PacketFormat::M2ts, 0)));
        let mut junk = vec![0x47, 0x12, 0x00, 0x47, 0x99];
        junk.extend_from_slice(&m2ts);
        assert_eq!(detect_format(&junk), Some((PacketFormat::M2ts, 5)));
        assert_eq!(detect_format(b"RIFF....WAVEfmt "), None);
    }

    #[test]
    fn the_program_map_lists_streams_in_order_with_their_kinds() {
        let mut w = TsWriter::new(PacketFormat::M2ts);
        w.pat(PMT_PID);
        w.pmt(
            PMT_PID,
            &[
                (0x1B, VIDEO_PID, &[]),
                (0x80, 0x1100, &[]),
                (0x06, 0x1101, &[0x6A, 0x01, 0x00]),
                (0x06, 0x1102, &[0x05, 0x04, b'E', b'A', b'C', b'3']),
                (0x90, 0x1200, &[]),
            ],
        );
        let probe = TsProbe::from_head(&w.out).expect("probe");
        let kinds: Vec<_> = probe.streams.iter().map(|s| (s.pid, s.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                (VIDEO_PID, StreamKind::H264),
                (0x1100, StreamKind::Lpcm),
                (0x1101, StreamKind::Ac3),
                (0x1102, StreamKind::Eac3),
            ],
            "the PGS subtitle stream (0x90) is not one of ours"
        );
    }

    #[test]
    fn a_pes_split_over_packets_comes_back_whole_with_its_timestamp() {
        let mut w = TsWriter::new(PacketFormat::Ts);
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        w.pes(VIDEO_PID, 0xE0, Some(123_456), &data);
        w.pes(VIDEO_PID, 0xE0, Some(126_459), &[1, 2, 3]);
        let mut reader = PacketReader::new(&w.out[..], PacketFormat::Ts);
        let mut assembler = PesAssembler::default();
        let mut got = Vec::new();
        while let Some(packet) = reader.next_packet().expect("read") {
            got.extend(assembler.push(&packet));
        }
        got.extend(assembler.finish(VIDEO_PID));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].pts, Some(123_456));
        assert_eq!(got[0].data, data);
        assert!(!got[0].damaged);
        assert_eq!(got[1].data, vec![1, 2, 3]);
    }

    #[test]
    fn a_lost_packet_marks_its_pes_damaged_and_sync_recovers_after_junk() {
        let mut w = TsWriter::new(PacketFormat::Ts);
        let data = vec![7u8; 600];
        w.pes(VIDEO_PID, 0xE0, Some(0), &data);
        w.pes(VIDEO_PID, 0xE0, Some(3003), &data);
        w.pes(VIDEO_PID, 0xE0, Some(6006), &data);
        let mut bytes = w.out.clone();
        // Drop the second packet of the first PES, and splice junk with
        // false sync bytes in it in front of the third PES.
        bytes.drain(188..376);
        let third = 188 * 7;
        bytes.splice(third..third, [1, 0x47, 2, 3, 0x47, 9, 9].iter().copied());
        let mut reader = PacketReader::new(&bytes[..], PacketFormat::Ts);
        let mut assembler = PesAssembler::default();
        let mut got = Vec::new();
        while let Some(packet) = reader.next_packet().expect("read") {
            got.extend(assembler.push(&packet));
        }
        got.extend(assembler.finish(VIDEO_PID));
        assert_eq!(got.len(), 3);
        assert!(got[0].damaged, "a missing packet is a continuity gap");
        assert!(!got[1].damaged);
        assert_eq!(got[2].pts, Some(6006));
        assert_eq!(got[2].data, data);
        assert_eq!(reader.resyncs, 1);
        assert_eq!(reader.skipped_bytes, 7);
    }

    #[test]
    fn timestamps_are_compared_across_the_33_bit_wrap() {
        assert_eq!(pts_delta(100, 50), 50);
        assert_eq!(pts_delta(50, 100), -50);
        assert_eq!(pts_delta(10, PTS_MODULUS - 10), 20);
        assert_eq!(pts_delta(PTS_MODULUS - 10, 10), -20);
    }

    #[test]
    fn the_probe_finds_first_and_last_timestamps_even_across_a_wrap() {
        let video_start = PTS_MODULUS - 30_000;
        let audio_start = PTS_MODULUS - 27_000;
        let bytes = camcorder_stream(PacketFormat::M2ts, video_start, audio_start);
        let mut probe = TsProbe::from_head(&bytes).expect("probe");
        probe.scan_tail_bytes(&bytes[bytes.len() / 2 + 3..]);
        let video = probe.first_video().expect("video");
        assert_eq!(video.first_pts, Some(video_start));
        assert_eq!(video.pts_step, Some(3003));
        assert_eq!(
            video.last_pts,
            Some((video_start + 29 * 3003) % PTS_MODULUS)
        );
        let audio = probe.audio_streams().next().expect("audio");
        assert_eq!(audio.kind, StreamKind::Ac3);
        assert_eq!(audio.head_payload[..2], [0x0B, 0x77]);
        let span = audio.span_secs().expect("span");
        assert!((span - 30.0 * 2880.0 / PTS_CLOCK_HZ).abs() < 1e-9, "{span}");
        let gap = TsProbe::secs_between(audio.first_pts.unwrap(), video.first_pts.unwrap());
        assert!((gap - 3000.0 / PTS_CLOCK_HZ).abs() < 1e-9, "{gap}");
    }

    #[test]
    fn the_first_timestamp_is_the_earliest_not_the_first_in_decode_order() {
        // I P B B: the B-frames are shown before the P that precedes them.
        let mut w = TsWriter::new(PacketFormat::Ts);
        w.pat(PMT_PID);
        w.pmt(PMT_PID, &[(0x1B, VIDEO_PID, &[])]);
        for pts in [9009u64, 18018, 3003, 6006, 30030, 21021, 24024, 27027] {
            w.pes(VIDEO_PID, 0xE0, Some(pts), &[0; 10]);
        }
        let probe = TsProbe::from_head(&w.out).expect("probe");
        let video = &probe.streams[0];
        assert_eq!(video.first_pts, Some(3003));
        assert_eq!(video.pts_step, Some(3003));
    }

    #[test]
    fn a_file_with_no_program_table_is_refused_rather_than_guessed() {
        let mut w = TsWriter::new(PacketFormat::Ts);
        for _ in 0..10 {
            w.packet(0x200, true, &[0; 20]);
        }
        let err = TsProbe::from_head(&w.out).expect_err("no PAT");
        assert!(format!("{err:#}").contains("no program table"), "{err:#}");
    }

    #[test]
    fn a_damaged_program_table_is_skipped_by_its_crc() {
        let mut w = TsWriter::new(PacketFormat::Ts);
        w.pat(PMT_PID);
        // The section sits at the end of its packet; this is its PMT PID.
        let flip_at = w.out.len() - 6;
        w.out[flip_at] ^= 0xFF;
        w.pat(PMT_PID);
        w.pmt(PMT_PID, &[(0x1B, VIDEO_PID, &[])]);
        let probe = TsProbe::from_head(&w.out).expect("second PAT is good");
        assert_eq!(probe.streams.len(), 1);
    }

    #[test]
    fn only_mts_and_m2ts_are_transport_stream_paths() {
        assert!(is_mpegts_path(Path::new("00000.MTS")));
        assert!(is_mpegts_path(Path::new("clip.m2ts")));
        assert!(
            !is_mpegts_path(Path::new("index.ts")),
            "TypeScript, not MPEG"
        );
        assert!(!is_mpegts_path(Path::new("clip.mp4")));
    }
}
