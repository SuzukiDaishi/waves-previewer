//! The audio of an MPEG-2 transport stream (`.mts` / `.m2ts`).
//!
//! AVCHD camcorders record AC-3, a few record LPCM, and Blu-ray `.m2ts` files
//! carry either. AC-3 is decoded by the bundled pure-Rust `oxideav-ac3` rather
//! than borrowed from the OS the way AAC is: Windows 11 24H2 removed its AC-3
//! decoder, so borrowing would leave most camcorder footage silent, and AC-3's
//! patents have expired, so shipping one costs nothing. E-AC-3 is refused (its
//! patents are younger), as are DTS, TrueHD and the AAC and MPEG audio a
//! transport stream can also carry: those rows say `<codec> UNSUPPORTED` and
//! the picture plays on a silent timeline. The one exception is TrueHD in a
//! build with the `truehd` feature: it is never decoded here either, but
//! read through `TsTrueHdReader` into a decoded copy that plays instead
//! (`crate::audio_truehd`).
//!
//! The decoded samples start at the audio stream's first PTS, and that is the
//! zero of the row's timeline ([`timeline_zero_pts`]); the picture is placed
//! against the same zero. Samples are never padded or trimmed to meet the
//! video, so what plays is exactly what the file holds.
//!
//! Channels come out in WAVE channel-mask order with the mask to match, so
//! the channel layout, the SURROUND meter and playback routing treat a 5.1
//! camcorder track exactly like a 5.1 WAV.

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::audio_channels::SpeakerPos;
use crate::audio_io::{AudioInfo, SampleValueKind};
use crate::mpegts::{PacketReader, PesAssembler, StreamKind, TsProbe, TsStream};

/// The rates AC-3's `fscod` selects (ATSC A/52 Table 5.6).
const AC3_SAMPLE_RATES: [u32; 3] = [48_000, 44_100, 32_000];
/// Samples per channel in one AC-3 syncframe: six blocks of 256.
const AC3_FRAME_SAMPLES: usize = 1536;
/// Nominal bit rate in kbit/s, by `frmsizecod / 2` (A/52 Table 5.18).
const AC3_BITRATES_KBPS: [u32; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];
/// 16-bit words per syncframe at 44.1 kHz, by `frmsizecod` (Table 5.18). At
/// 48 and 32 kHz a frame is a whole 2 or 3 words per kbit/s; 44.1 kHz pads
/// every other frame by a word instead.
const AC3_FRAME_WORDS_44K: [u16; 38] = [
    69, 70, 87, 88, 104, 105, 121, 122, 139, 140, 174, 175, 208, 209, 243, 244, 278, 279, 348, 349,
    417, 418, 487, 488, 557, 558, 696, 697, 835, 836, 975, 976, 1114, 1115, 1253, 1254, 1393, 1394,
];
/// Highest `bsid` that is AC-3 proper; 11..=16 is E-AC-3 (A/52 Annex E).
const AC3_MAX_BSID: u8 = 10;
/// The rates Blu-ray LPCM's 4-bit rate code selects.
const LPCM_SAMPLE_RATES: [(u8, u32); 3] = [(1, 48_000), (4, 96_000), (5, 192_000)];
/// Bytes of the header ahead of every Blu-ray LPCM PES payload.
const LPCM_HEADER_BYTES: usize = 4;

use SpeakerPos::{Bc, Bl, Br, Fc, Fl, Fr, Lfe, Sl, Sr};

/// The fields of an AC-3 or E-AC-3 syncframe header the decode needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Ac3Header {
    /// E-AC-3, which is recognised only to be refused.
    eac3: bool,
    sample_rate: u32,
    frame_bytes: usize,
    acmod: u8,
    lfeon: bool,
    bitrate_kbps: u32,
}

/// Parse the syncframe header at the start of `b`.
fn parse_ac3_header(b: &[u8]) -> Option<Ac3Header> {
    if b.len() < 8 || b[0] != 0x0B || b[1] != 0x77 {
        return None;
    }
    let bsid = b[5] >> 3;
    if bsid > AC3_MAX_BSID {
        if bsid > 16 {
            return None;
        }
        let frmsiz = (usize::from(b[2] & 0x07) << 8) | usize::from(b[3]);
        return Some(Ac3Header {
            eac3: true,
            sample_rate: 0,
            frame_bytes: (frmsiz + 1) * 2,
            acmod: (b[4] >> 1) & 0x07,
            lfeon: b[4] & 0x01 != 0,
            bitrate_kbps: 0,
        });
    }
    let fscod = usize::from(b[4] >> 6);
    let frmsizecod = usize::from(b[4] & 0x3F);
    let sample_rate = *AC3_SAMPLE_RATES.get(fscod)?;
    let bitrate_kbps = *AC3_BITRATES_KBPS.get(frmsizecod / 2)?;
    let words = match fscod {
        0 => bitrate_kbps as usize * 2,
        1 => usize::from(AC3_FRAME_WORDS_44K[frmsizecod]),
        _ => bitrate_kbps as usize * 3,
    };
    // acmod, then the mix-level fields that acmod switches on, then lfeon.
    let bits = u16::from_be_bytes([b[6], b[7]]);
    let acmod = (bits >> 13) as u8;
    let mut lfe_bit = 12;
    if acmod & 0x1 != 0 && acmod != 1 {
        lfe_bit -= 2; // cmixlev
    }
    if acmod & 0x4 != 0 {
        lfe_bit -= 2; // surmixlev
    }
    if acmod == 2 {
        lfe_bit -= 2; // dsurmod
    }
    Some(Ac3Header {
        eac3: false,
        sample_rate,
        frame_bytes: words * 2,
        acmod,
        lfeon: (bits >> lfe_bit) & 1 != 0,
        bitrate_kbps,
    })
}

/// The first believable syncframe header in `bytes`: a header that parses
/// and, where the bytes reach that far, is followed by another syncword.
fn first_ac3_header(bytes: &[u8]) -> Option<Ac3Header> {
    (0..bytes.len().saturating_sub(1)).find_map(|at| {
        let header = parse_ac3_header(&bytes[at..])?;
        let next = at + header.frame_bytes;
        let confirmed = match bytes.get(next..next + 2) {
            Some(sync) => sync == [0x0B, 0x77],
            None => true,
        };
        confirmed.then_some(header)
    })
}

/// The speakers of `oxideav-ac3`'s output channels, in its output order:
/// full-band channels in WAVE order, LFE after them, except that a 3/2
/// stream puts LFE fourth.
fn ac3_output_speakers(acmod: u8, lfeon: bool) -> Vec<SpeakerPos> {
    let mut speakers: Vec<SpeakerPos> = match acmod {
        0 | 2 => vec![Fl, Fr],
        1 => vec![Fc],
        3 => vec![Fl, Fr, Fc],
        4 => vec![Fl, Fr, Bc],
        5 => vec![Fl, Fr, Fc, Bc],
        6 => vec![Fl, Fr, Bl, Br],
        _ => vec![Fl, Fr, Fc, Bl, Br],
    };
    if lfeon {
        let at = if acmod == 7 { 3 } else { speakers.len() };
        speakers.insert(at, Lfe);
    }
    speakers
}

/// A Blu-ray LPCM header (the four bytes ahead of each PES payload).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LpcmHeader {
    sample_rate: u32,
    bits: u16,
    /// The speakers in storage order.
    speakers: &'static [SpeakerPos],
}

impl LpcmHeader {
    /// Channels as stored: always an even number, padded with silence.
    fn stored_channels(&self) -> usize {
        self.speakers.len().next_multiple_of(2)
    }

    fn bytes_per_sample(&self) -> usize {
        if self.bits == 16 {
            2
        } else {
            3
        }
    }
}

fn parse_lpcm_header(b: &[u8]) -> Option<LpcmHeader> {
    if b.len() < LPCM_HEADER_BYTES {
        return None;
    }
    // The Blu-ray channel assignments, surrounds named the way this app's
    // standard 5.1 names them (Bl/Br) so a camcorder 5.1 lays out like a WAV.
    let speakers: &'static [SpeakerPos] = match b[2] >> 4 {
        1 => &[Fc],
        3 => &[Fl, Fr],
        4 => &[Fl, Fr, Fc],
        5 => &[Fl, Fr, Bc],
        6 => &[Fl, Fr, Fc, Bc],
        7 => &[Fl, Fr, Bl, Br],
        8 => &[Fl, Fr, Fc, Bl, Br],
        9 => &[Fl, Fr, Fc, Bl, Br, Lfe],
        10 => &[Fl, Fr, Fc, Sl, Bl, Br, Sr],
        11 => &[Fl, Fr, Fc, Sl, Bl, Br, Sr, Lfe],
        _ => return None,
    };
    let rate_code = b[2] & 0x0F;
    let sample_rate = LPCM_SAMPLE_RATES
        .iter()
        .find(|(code, _)| *code == rate_code)?
        .1;
    let bits = match b[3] >> 6 {
        1 => 16,
        2 => 20,
        3 => 24,
        _ => return None,
    };
    Some(LpcmHeader {
        sample_rate,
        bits,
        speakers,
    })
}

/// How decoded channels map to channel-mask order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MaskOrder {
    /// For each output channel, the decoded channel it comes from.
    source_of: Vec<usize>,
    mask: u32,
}

impl MaskOrder {
    fn new(speakers: &[SpeakerPos]) -> Self {
        let mut source_of: Vec<usize> = (0..speakers.len()).collect();
        source_of.sort_by_key(|&i| speakers[i].mask_bit().unwrap_or(u32::MAX));
        let mask = speakers
            .iter()
            .filter_map(|s| s.mask_bit())
            .fold(0, |m, b| m | b);
        Self { source_of, mask }
    }

    fn channels(&self) -> usize {
        self.source_of.len()
    }
}

/// What a transport stream's audio is, from its head.
enum Choice<'a> {
    Absent,
    /// Audio this build does not decode; names the first such stream.
    Unsupported(&'static str),
    Ac3(&'a TsStream, Ac3Header),
    Lpcm(&'a TsStream, LpcmHeader),
    /// TrueHD, with the `truehd` feature: never decoded here, but through a
    /// decoded copy (`crate::audio_truehd`, `app::spatial_ops`).
    #[cfg(feature = "truehd")]
    TrueHd(&'a TsStream),
}

/// The first audio stream this build decodes, in PMT order.
fn choose(probe: &TsProbe) -> Choice<'_> {
    let mut unsupported: Option<&'static str> = None;
    for stream in probe.audio_streams() {
        match stream.kind {
            StreamKind::Ac3 => match first_ac3_header(&stream.head_payload) {
                Some(header) if !header.eac3 => return Choice::Ac3(stream, header),
                // An AC-3 stream type carrying E-AC-3 frames is E-AC-3.
                Some(_) => {
                    unsupported.get_or_insert(StreamKind::Eac3.label());
                }
                // Declared but never started in the head: nothing to play.
                None => {}
            },
            StreamKind::Lpcm => {
                if let Some(header) = parse_lpcm_header(&stream.head_payload) {
                    return Choice::Lpcm(stream, header);
                }
            }
            #[cfg(feature = "truehd")]
            StreamKind::TrueHd => return Choice::TrueHd(stream),
            other => {
                unsupported.get_or_insert(other.label());
            }
        }
    }
    match unsupported {
        Some(label) => Choice::Unsupported(label),
        None => Choice::Absent,
    }
}

/// Whether a transport stream has audio, and whether this build plays it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TsAudioPresence {
    Absent,
    /// The codec of the first audio stream, none of which this build decodes.
    Unsupported(&'static str),
    Decodable,
}

pub fn audio_presence(probe: &TsProbe) -> TsAudioPresence {
    match choose(probe) {
        Choice::Absent => TsAudioPresence::Absent,
        Choice::Unsupported(label) => TsAudioPresence::Unsupported(label),
        Choice::Ac3(..) | Choice::Lpcm(..) => TsAudioPresence::Decodable,
        #[cfg(feature = "truehd")]
        Choice::TrueHd(..) => TsAudioPresence::Decodable,
    }
}

/// The PTS at the start of the row's timeline: where the decoded audio
/// starts, or, with no audio this build plays, where the picture starts.
pub fn timeline_zero_pts(probe: &TsProbe) -> Option<u64> {
    match choose(probe) {
        Choice::Ac3(stream, _) | Choice::Lpcm(stream, _) => stream.first_pts,
        #[cfg(feature = "truehd")]
        Choice::TrueHd(stream) => stream.first_pts,
        Choice::Absent | Choice::Unsupported(_) => probe.first_video()?.first_pts,
    }
}

/// The list's description of a transport stream's audio, from its first and
/// last few megabytes.
pub fn read_info(
    path: &Path,
    created_at: Option<SystemTime>,
    modified_at: Option<SystemTime>,
) -> Result<AudioInfo> {
    let probe = TsProbe::open(path)?;
    let (stream, sample_rate, channels, mask, bits, kind, bit_rate_bps) = match choose(&probe) {
        Choice::Absent => anyhow::bail!("mpeg-ts: no audio stream: {}", path.display()),
        Choice::Unsupported(label) => {
            anyhow::bail!(
                "mpeg-ts: {label} audio is not supported: {}",
                path.display()
            )
        }
        Choice::Ac3(stream, header) => {
            let order = MaskOrder::new(&ac3_output_speakers(header.acmod, header.lfeon));
            (
                stream,
                header.sample_rate,
                order.channels(),
                order.mask,
                // Lossy, like AAC: no bit depth of its own.
                16,
                SampleValueKind::Unknown,
                header.bitrate_kbps * 1000,
            )
        }
        Choice::Lpcm(stream, header) => {
            let order = MaskOrder::new(header.speakers);
            let channels = order.channels();
            (
                stream,
                header.sample_rate,
                channels,
                order.mask,
                header.bits,
                SampleValueKind::Int,
                header.sample_rate * u32::from(header.bits) * channels as u32,
            )
        }
        #[cfg(feature = "truehd")]
        Choice::TrueHd(stream) => {
            let found = crate::audio_truehd::probe(path)?;
            (
                stream,
                found.sample_rate,
                found.channels,
                0,
                24,
                SampleValueKind::Int,
                0,
            )
        }
    };
    let duration_secs = stream.span_secs().filter(|secs| *secs > 0.0);
    Ok(AudioInfo {
        channels: channels as u16,
        sample_rate,
        bits_per_sample: bits,
        sample_value_kind: kind,
        // The file's size over its length would count the picture too.
        bit_rate_bps: Some(bit_rate_bps),
        duration_secs: duration_secs.map(|secs| secs as f32),
        total_frames: duration_secs.map(|secs| (secs * f64::from(sample_rate)).round() as u64),
        created_at,
        modified_at,
        channel_mask: (mask != 0).then_some(mask),
        has_adm_chunks: false,
    })
}

enum Codec {
    Ac3 {
        decoder: Box<dyn oxideav_core::Decoder>,
        /// Elementary stream bytes not yet cut into syncframes.
        pending: Vec<u8>,
        header: Ac3Header,
        /// Whether the next syncword can be trusted where the last frame
        /// said it would be. After damage, a frame has to pass its CRC
        /// before the stream counts as found again.
        locked: bool,
        speakers: usize,
    },
    Lpcm {
        header: LpcmHeader,
    },
}

/// Decodes one transport stream's audio from start to end.
pub struct TsAudioDecoder {
    reader: PacketReader<File>,
    pid: u16,
    pes: PesAssembler,
    codec: Codec,
    sample_rate: u32,
    order: MaskOrder,
    decode_errors: u32,
    produced_any: bool,
    finished: bool,
}

impl TsAudioDecoder {
    pub fn open(path: &Path) -> Result<Self> {
        let probe = TsProbe::open_head(path)?;
        let (pid, codec, sample_rate, order) = match choose(&probe) {
            Choice::Absent => anyhow::bail!("mpeg-ts: no audio stream: {}", path.display()),
            Choice::Unsupported(label) => {
                anyhow::bail!(
                    "mpeg-ts: {label} audio is not supported: {}",
                    path.display()
                )
            }
            Choice::Ac3(stream, header) => {
                let params = oxideav_core::CodecParameters::audio(oxideav_core::CodecId::new(
                    oxideav_ac3::CODEC_ID_STR,
                ));
                let decoder = oxideav_ac3::decoder::make_decoder(&params)
                    .map_err(|err| anyhow::anyhow!("ac-3 decoder: {err}"))?;
                let speakers = ac3_output_speakers(header.acmod, header.lfeon);
                (
                    stream.pid,
                    Codec::Ac3 {
                        decoder,
                        pending: Vec::new(),
                        header,
                        locked: false,
                        speakers: speakers.len(),
                    },
                    header.sample_rate,
                    MaskOrder::new(&speakers),
                )
            }
            Choice::Lpcm(stream, header) => (
                stream.pid,
                Codec::Lpcm { header },
                header.sample_rate,
                MaskOrder::new(header.speakers),
            ),
            #[cfg(feature = "truehd")]
            Choice::TrueHd(_) => anyhow::bail!(
                "mpeg-ts: TrueHD plays from a decoded copy, not from here: {}",
                path.display()
            ),
        };
        let mut file =
            File::open(path).with_context(|| format!("open mpeg-ts: {}", path.display()))?;
        file.seek(SeekFrom::Start(probe.first_packet))
            .with_context(|| format!("seek mpeg-ts: {}", path.display()))?;
        Ok(Self {
            reader: PacketReader::new(file, probe.format),
            pid,
            pes: PesAssembler::default(),
            codec,
            sample_rate,
            order,
            decode_errors: 0,
            produced_any: false,
            finished: false,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.order.channels()
    }

    pub fn channel_mask(&self) -> u32 {
        self.order.mask
    }

    /// Frames lost to damage so far, each replaced with silence of its length.
    pub fn decode_errors(&self) -> u32 {
        self.decode_errors
    }

    /// The next decoded block, planar in channel-mask order, or `None` at
    /// the end of the stream. Block length is whatever one PES held.
    pub fn next_block(&mut self) -> Result<Option<Vec<Vec<f32>>>> {
        while !self.finished {
            let pes = match self.next_pes() {
                Ok(pes) => pes,
                // A read error after audio has come out is a damaged end, not
                // a reason to lose what was decoded -- the same call symphonia
                // makes on an unexpected EOF.
                Err(_) if self.produced_any => None,
                Err(err) => return Err(err),
            };
            let Some(pes) = pes else {
                self.finished = true;
                break;
            };
            if pes.damaged {
                self.decode_errors = self.decode_errors.saturating_add(1);
            }
            let block = self.decode_pes(&pes.data)?;
            if block.first().is_some_and(|c| !c.is_empty()) {
                self.produced_any = true;
                return Ok(Some(block));
            }
        }
        Ok(None)
    }

    fn next_pes(&mut self) -> Result<Option<crate::mpegts::Pes>> {
        loop {
            let Some(packet) = self.reader.next_packet().context("read mpeg-ts")? else {
                return Ok(self.pes.finish(self.pid));
            };
            if packet.pid != self.pid {
                continue;
            }
            if let Some(pes) = self.pes.push(&packet) {
                return Ok(Some(pes));
            }
        }
    }

    /// Decode one PES payload into planar channels in decoded order, then
    /// put them in mask order.
    fn decode_pes(&mut self, data: &[u8]) -> Result<Vec<Vec<f32>>> {
        let decoded = match &mut self.codec {
            Codec::Ac3 {
                decoder,
                pending,
                header,
                locked,
                speakers,
            } => {
                pending.extend_from_slice(data);
                let mut out: Vec<Vec<f32>> = vec![Vec::new(); *speakers];
                let errors =
                    decode_ac3_frames(decoder.as_mut(), pending, header, locked, &mut out)?;
                self.decode_errors = self.decode_errors.saturating_add(errors);
                out
            }
            Codec::Lpcm { header } => {
                let Some(found) = parse_lpcm_header(data) else {
                    self.decode_errors = self.decode_errors.saturating_add(1);
                    return Ok(Vec::new());
                };
                if found != *header {
                    anyhow::bail!(
                        "mpeg-ts: LPCM format changed mid-stream: {header:?} then {found:?}"
                    );
                }
                decode_lpcm(header, &data[LPCM_HEADER_BYTES..])
            }
        };
        Ok(self
            .order
            .source_of
            .iter()
            .map(|&source| decoded.get(source).cloned().unwrap_or_default())
            .collect())
    }
}

/// Cut `pending` into syncframes and decode each into `out`. Returns how
/// many frames were damaged; each of those becomes a frame of silence, so the
/// audio keeps its length and stays level with the picture.
fn decode_ac3_frames(
    decoder: &mut dyn oxideav_core::Decoder,
    pending: &mut Vec<u8>,
    expected: &Ac3Header,
    locked: &mut bool,
    out: &mut [Vec<f32>],
) -> Result<u32> {
    let mut errors = 0u32;
    let mut at = 0usize;
    loop {
        let rest = &pending[at..];
        if rest.len() < 8 {
            break;
        }
        let Some(header) = parse_ac3_header(rest) else {
            // Not a syncframe here: look for the next one.
            if *locked {
                *locked = false;
                errors = errors.saturating_add(1);
            }
            at += 1;
            continue;
        };
        if rest.len() < header.frame_bytes {
            break; // the rest of this frame is in the next PES
        }
        let frame = &rest[..header.frame_bytes];
        if header.eac3 {
            // E-AC-3 interleaved in an AC-3 stream: not ours to decode.
            at += header.frame_bytes;
            continue;
        }
        let crc_ok = oxideav_ac3::decoder::verify_packet_crc(frame)
            .map(|status| status.all_ok())
            .unwrap_or(false);
        if !*locked && !crc_ok {
            // An 0x0B77 inside some other frame's data, most likely.
            at += 1;
            continue;
        }
        if crc_ok
            && (header.sample_rate != expected.sample_rate
                || header.acmod != expected.acmod
                || header.lfeon != expected.lfeon)
        {
            anyhow::bail!(
                "mpeg-ts: AC-3 format changed mid-stream: {} Hz acmod {} lfe {} then {} Hz acmod {} lfe {}",
                expected.sample_rate,
                expected.acmod,
                expected.lfeon,
                header.sample_rate,
                header.acmod,
                header.lfeon
            );
        }
        *locked = true;
        let decoded = if crc_ok {
            decode_ac3_frame(decoder, frame, expected.sample_rate, out.len())
        } else {
            None
        };
        match decoded {
            Some(samples) => {
                let channels = out.len();
                for (channel, dst) in out.iter_mut().enumerate() {
                    dst.extend(
                        samples
                            .iter()
                            .skip(channel)
                            .step_by(channels)
                            .map(|&s| f32::from(s) / 32768.0),
                    );
                }
            }
            None => {
                errors = errors.saturating_add(1);
                for dst in out.iter_mut() {
                    dst.resize(dst.len() + AC3_FRAME_SAMPLES, 0.0);
                }
            }
        }
        at += header.frame_bytes;
    }
    pending.drain(..at);
    Ok(errors)
}

/// One syncframe through the decoder: interleaved 16-bit samples in the
/// decoder's channel order, or `None` if it would not decode.
fn decode_ac3_frame(
    decoder: &mut dyn oxideav_core::Decoder,
    frame: &[u8],
    sample_rate: u32,
    channels: usize,
) -> Option<Vec<i16>> {
    let packet = oxideav_core::Packet::new(
        0,
        oxideav_core::TimeBase::new(1, i64::from(sample_rate)),
        frame.to_vec(),
    );
    decoder.send_packet(&packet).ok()?;
    let oxideav_core::Frame::Audio(audio) = decoder.receive_frame().ok()? else {
        return None;
    };
    let bytes = audio.data.first()?;
    if bytes.len() != audio.samples as usize * channels * 2 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
            .collect(),
    )
}

/// Blu-ray LPCM: big-endian samples, 16 bits in two bytes or 20/24 bits
/// left-justified in three, channels padded to an even count.
fn decode_lpcm(header: &LpcmHeader, payload: &[u8]) -> Vec<Vec<f32>> {
    let width = header.bytes_per_sample();
    let stride = header.stored_channels() * width;
    let frames = payload.len() / stride;
    let mut out: Vec<Vec<f32>> = (0..header.speakers.len())
        .map(|_| Vec::with_capacity(frames))
        .collect();
    for frame in payload.chunks_exact(stride) {
        for (channel, dst) in out.iter_mut().enumerate() {
            let s = &frame[channel * width..channel * width + width];
            dst.push(if width == 2 {
                f32::from(i16::from_be_bytes([s[0], s[1]])) / 32768.0
            } else {
                (i32::from_be_bytes([s[0], s[1], s[2], 0]) >> 8) as f32 / 8_388_608.0
            });
        }
    }
    out
}

/// Decode a whole stream, or its first `max_secs`. The third value is
/// whether the end of the stream was reached; the fourth, damaged frames.
pub fn decode_with_errors(
    path: &Path,
    max_secs: Option<f32>,
) -> Result<(Vec<Vec<f32>>, u32, bool, u32)> {
    let mut decoder = TsAudioDecoder::open(path)?;
    let sample_rate = decoder.sample_rate();
    let max_frames = max_secs
        .filter(|secs| *secs > 0.0)
        .map(|secs| ((sample_rate as f32) * secs).ceil() as usize)
        .filter(|frames| *frames > 0);
    let mut channels: Vec<Vec<f32>> = vec![Vec::new(); decoder.channels().max(1)];
    let mut reached_eof = true;
    while let Some(block) = decoder.next_block()? {
        for (out, decoded) in channels.iter_mut().zip(block) {
            out.extend_from_slice(&decoded);
        }
        if let Some(limit) = max_frames {
            if channels.first().map(|c| c.len()).unwrap_or(0) >= limit {
                reached_eof = false;
                for out in &mut channels {
                    out.truncate(limit);
                }
                break;
            }
        }
    }
    Ok((channels, sample_rate, reached_eof, decoder.decode_errors()))
}

/// [`decode_with_errors`] without the error count, in the shape of
/// `audio_mf::decode`.
pub fn decode(path: &Path, max_secs: Option<f32>) -> Result<(Vec<Vec<f32>>, u32, bool)> {
    let (channels, sample_rate, reached_eof, _) = decode_with_errors(path, max_secs)?;
    Ok((channels, sample_rate, reached_eof))
}

/// Decode a whole stream, handing back roughly `emit_every_secs` at a time
/// through the same emitter every other progressive decode uses.
pub fn decode_progressive_chunks<C, F>(
    path: &Path,
    emit_every_secs: f32,
    mut should_cancel: C,
    mut on_chunk: F,
) -> Result<()>
where
    C: FnMut() -> bool,
    F: FnMut(Vec<Vec<f32>>, u32, usize, bool) -> bool,
{
    let mut decoder = TsAudioDecoder::open(path)?;
    let sample_rate = decoder.sample_rate();
    let emit_frames = (((sample_rate as f32) * emit_every_secs.max(0.05)).ceil() as usize).max(1);
    let mut pending: Vec<Vec<f32>> = vec![Vec::new(); decoder.channels().max(1)];
    let mut decoded_frames = 0usize;
    loop {
        if should_cancel() {
            return Ok(());
        }
        let Some(block) = decoder.next_block()? else {
            break;
        };
        let frames = block.first().map(|c| c.len()).unwrap_or(0);
        for (out, decoded) in pending.iter_mut().zip(block) {
            out.extend_from_slice(&decoded);
        }
        decoded_frames = decoded_frames.saturating_add(frames);
        if pending.first().map(|c| c.len()).unwrap_or(0) >= emit_frames
            && !crate::audio_io::emit_ready_chunk(
                path,
                "decode_multi_progressive_mpegts_chunk",
                sample_rate,
                &mut pending,
                decoded_frames,
                false,
                &mut on_chunk,
            )
        {
            return Ok(());
        }
    }
    if should_cancel() {
        return Ok(());
    }
    let _ = crate::audio_io::emit_ready_chunk(
        path,
        "decode_multi_progressive_mpegts_final",
        sample_rate,
        &mut pending,
        decoded_frames,
        true,
        &mut on_chunk,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpegts::tests::TsWriter;
    use crate::mpegts::PacketFormat;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("test_samples")
            .join("video")
            .join(name)
    }

    /// The strength of `freq` in `x` (a single-bin DFT).
    fn tone_level(x: &[f32], freq: f32, sample_rate: u32) -> f32 {
        let w = std::f32::consts::TAU * freq / sample_rate as f32;
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (i, &v) in x.iter().enumerate() {
            re += v * (w * i as f32).cos();
            im += v * (w * i as f32).sin();
        }
        (re * re + im * im).sqrt() / x.len().max(1) as f32
    }

    /// Which of the fixture's tones dominates each channel.
    fn dominant_tones(channels: &[Vec<f32>], sample_rate: u32) -> Vec<u32> {
        const TONES: [u32; 6] = [300, 450, 600, 60, 750, 900];
        channels
            .iter()
            .map(|x| {
                // Skip the codec's start-up, then a whole number of 60 Hz cycles.
                let x = &x[2048.min(x.len())..];
                let x = &x[..(sample_rate as usize / 4).min(x.len())];
                *TONES
                    .iter()
                    .max_by(|a, b| {
                        tone_level(x, **a as f32, sample_rate).total_cmp(&tone_level(
                            x,
                            **b as f32,
                            sample_rate,
                        ))
                    })
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn ac3_headers_give_rate_size_and_layout() {
        // 48 kHz, 384 kbit/s (frmsizecod 28), bsid 8, acmod 7 (3/2) with LFE:
        // cmixlev and surmixlev come between acmod and lfeon.
        let frame = [0x0B, 0x77, 0, 0, 0b00_011100, 8 << 3, 0b111_01_01_1, 0];
        let header = parse_ac3_header(&frame).expect("header");
        assert_eq!(header.sample_rate, AC3_SAMPLE_RATES[0]);
        assert_eq!(header.frame_bytes, 1536);
        assert_eq!(header.bitrate_kbps, 384);
        assert_eq!((header.acmod, header.lfeon), (7, true));
        // 2/0 with dsurmod, no LFE, at 44.1 kHz and frmsizecod 1 (padded).
        let frame = [0x0B, 0x77, 0, 0, 0b01_000001, 8 << 3, 0b010_00_0_00, 0];
        let header = parse_ac3_header(&frame).expect("header");
        assert_eq!(header.sample_rate, AC3_SAMPLE_RATES[1]);
        assert_eq!(header.frame_bytes, 140);
        assert_eq!((header.acmod, header.lfeon), (2, false));
        // bsid 16 is E-AC-3.
        let frame = [0x0B, 0x77, 0x01, 0xFF, 0x3F, 16 << 3, 0, 0];
        let header = parse_ac3_header(&frame).expect("header");
        assert!(header.eac3);
        assert_eq!(header.frame_bytes, 1024);
    }

    #[test]
    fn decoded_channels_are_put_in_channel_mask_order() {
        let order = MaskOrder::new(&ac3_output_speakers(6, true));
        // Decoder: L R Ls Rs LFE; mask order: L R LFE Ls Rs.
        assert_eq!(order.source_of, vec![0, 1, 4, 2, 3]);
        assert_eq!(order.mask, 0x3B);
        let order = MaskOrder::new(&ac3_output_speakers(7, true));
        assert_eq!(order.source_of, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(order.mask, 0x3F);
        let lpcm_51 = parse_lpcm_header(&[0, 0, 0x91, 0xC0]).expect("lpcm");
        let order = MaskOrder::new(lpcm_51.speakers);
        // Stored: L R C Ls Rs LFE.
        assert_eq!(order.source_of, vec![0, 1, 2, 5, 3, 4]);
        assert_eq!(lpcm_51.stored_channels(), 6);
        let lpcm_30 = parse_lpcm_header(&[0, 0, 0x41, 0x40]).expect("lpcm");
        assert_eq!(
            lpcm_30.stored_channels(),
            4,
            "three channels stored as four"
        );
    }

    #[test]
    fn a_51_ac3_track_decodes_each_speaker_into_its_own_channel() {
        let path = fixture("m2ts_ac3_51.m2ts");
        let (channels, sample_rate, reached_eof, errors) =
            decode_with_errors(&path, None).expect("decode");
        assert!(reached_eof);
        assert_eq!(errors, 0);
        assert_eq!(sample_rate, AC3_SAMPLE_RATES[0]);
        // Mask order L R C LFE Ls Rs; the tones were 300 450 600 60 750 900.
        assert_eq!(
            dominant_tones(&channels, sample_rate),
            vec![300, 450, 600, 60, 750, 900]
        );
        let secs = channels[0].len() as f64 / f64::from(sample_rate);
        assert!((secs - 2.0).abs() < 0.05, "{secs} s");
        let info = read_info(&path, None, None).expect("info");
        assert_eq!(info.channels, 6);
        assert_eq!(info.channel_mask, Some(0x3F));
        assert_eq!(info.bit_rate_bps, Some(384_000));
        let duration = info.duration_secs.expect("duration");
        assert!((duration - 2.0).abs() < 0.05, "{duration} s");
    }

    #[test]
    fn a_51_lpcm_track_decodes_each_speaker_into_its_own_channel() {
        let path = fixture("m2ts_lpcm_51.m2ts");
        let (channels, sample_rate, _, errors) = decode_with_errors(&path, None).expect("decode");
        assert_eq!(errors, 0);
        assert_eq!(
            dominant_tones(&channels, sample_rate),
            vec![300, 450, 600, 60, 750, 900]
        );
        let info = read_info(&path, None, None).expect("info");
        assert_eq!(info.bits_per_sample, 24);
        assert_eq!(info.channel_mask, Some(0x3F));
        // From the timestamps, so good to a PES packet, not to a sample.
        let total = info.total_frames.expect("frames") as f64;
        let decoded = channels[0].len() as f64;
        assert!(
            (total - decoded).abs() / f64::from(sample_rate) < 0.01,
            "{total} vs {decoded}"
        );
    }

    #[test]
    fn the_prefix_and_streaming_decoders_agree_with_the_full_one() {
        let path = fixture("mts_sync_ac3_6s.mts");
        let (full, sample_rate, _, _) = decode_with_errors(&path, None).expect("full");
        let (prefix, _, reached_eof, _) = decode_with_errors(&path, Some(1.0)).expect("prefix");
        assert!(!reached_eof);
        assert_eq!(prefix[0].len(), sample_rate as usize);
        assert_eq!(prefix[0][..], full[0][..prefix[0].len()]);
        let mut streamed: Vec<Vec<f32>> = vec![Vec::new(); full.len()];
        decode_progressive_chunks(
            &path,
            0.5,
            || false,
            |chunk, _, _, _| {
                for (dst, src) in streamed.iter_mut().zip(chunk) {
                    dst.extend(src);
                }
                true
            },
        )
        .expect("stream");
        assert_eq!(streamed, full);
    }

    #[test]
    fn the_sync_fixture_ticks_on_whole_seconds_of_the_picture() {
        // The audio starts about 0.3 s after the picture, and the timeline's
        // zero is the audio's first sample. A tick at movie second N is at
        // timeline N - (audio start - picture start).
        let path = fixture("mts_sync_ac3_6s.mts");
        let probe = TsProbe::open(&path).expect("probe");
        let zero = timeline_zero_pts(&probe).expect("zero");
        let video_start = probe
            .first_video()
            .and_then(|v| v.first_pts)
            .expect("video");
        let picture_at = TsProbe::secs_between(video_start, zero);
        assert!(
            (picture_at + 0.295).abs() < 0.01,
            "picture starts at {picture_at}"
        );
        let (channels, sample_rate, _, _) = decode_with_errors(&path, None).expect("decode");
        // ffmpeg's sine is at 1/8 full scale; between ticks it is silent.
        let loudest = channels[0].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let first_loud = channels[0]
            .iter()
            .position(|v| v.abs() > loudest * 0.5)
            .expect("a tick");
        let tick_at = first_loud as f64 / f64::from(sample_rate);
        let expected = 1.0 + picture_at;
        assert!(
            (tick_at - expected).abs() < 0.03,
            "tick at {tick_at}, expected {expected}"
        );
    }

    #[test]
    fn a_damaged_frame_becomes_silence_of_its_length() {
        let path = fixture("m2ts_ac3_51.m2ts");
        let mut bytes = std::fs::read(&path).expect("read");
        let probe = TsProbe::from_head(&bytes).expect("probe");
        let audio_pid = probe.audio_streams().next().expect("audio").pid;
        // Flip a byte in the middle of the payload of the tenth audio packet.
        let mut seen = 0;
        let packet_len = probe.format.packet_len();
        for at in (0..bytes.len()).step_by(packet_len) {
            let ts = &bytes[at + 4..at + packet_len];
            let pid = (u16::from(ts[1] & 0x1F) << 8) | u16::from(ts[2]);
            if pid == audio_pid {
                seen += 1;
                if seen == 10 {
                    // The last byte is payload whatever the packet's
                    // adaptation field holds.
                    bytes[at + 4 + 187] ^= 0x5A;
                    break;
                }
            }
        }
        let damaged =
            std::env::temp_dir().join(format!("neowaves_damaged_{}.m2ts", std::process::id()));
        std::fs::write(&damaged, &bytes).expect("write");
        let (clean, _, _, _) = decode_with_errors(&path, None).expect("clean");
        let (broken, _, _, errors) = decode_with_errors(&damaged, None).expect("damaged");
        let _ = std::fs::remove_file(&damaged);
        assert_eq!(errors, 1);
        assert_eq!(broken[0].len(), clean[0].len(), "length is kept");
    }

    #[test]
    fn e_ac3_and_other_codecs_are_named_not_decoded() {
        let mut w = TsWriter::new(PacketFormat::M2ts);
        w.pat(0x100);
        w.pmt(
            0x100,
            &[
                (0x1B, 0x1011, &[]),
                (0x84, 0x1100, &[]),
                (0x82, 0x1101, &[]),
            ],
        );
        w.pes(
            0x1100,
            0xBD,
            Some(0),
            &[0x0B, 0x77, 0x01, 0xFF, 0x3F, 16 << 3, 0, 0],
        );
        let probe = TsProbe::from_head(&w.out).expect("probe");
        assert_eq!(
            audio_presence(&probe),
            TsAudioPresence::Unsupported("E-AC-3")
        );

        let mut w = TsWriter::new(PacketFormat::M2ts);
        w.pat(0x100);
        w.pmt(0x100, &[(0x1B, 0x1011, &[])]);
        w.pes(0x1011, 0xE0, Some(1000), &[0; 16]);
        let probe = TsProbe::from_head(&w.out).expect("probe");
        assert_eq!(audio_presence(&probe), TsAudioPresence::Absent);
        assert_eq!(
            timeline_zero_pts(&probe),
            Some(1000),
            "no audio: the picture is zero"
        );
    }

    #[test]
    fn a_video_without_audio_is_reported_as_such() {
        let probe = TsProbe::open(&fixture("mts_no_audio.mts")).expect("probe");
        assert_eq!(audio_presence(&probe), TsAudioPresence::Absent);
        let err = read_info(&fixture("mts_no_audio.mts"), None, None).expect_err("no audio");
        assert!(format!("{err:#}").contains("no audio stream"));
    }
}

/// Whether a transport stream's audio is TrueHD that this build plays (from
/// a decoded copy). Reads the head of the file: a worker's call.
#[cfg(feature = "truehd")]
pub fn is_truehd_stream(path: &Path) -> bool {
    TsProbe::open_head(path)
        .ok()
        .is_some_and(|probe| matches!(choose(&probe), Choice::TrueHd(_)))
}

/// The TrueHD stream of a transport stream, as the bytes of its access
/// units: every PES of its PID in order, without the AC-3 frames a Blu-ray
/// interleaves with them for players that decode only AC-3 (a TrueHD
/// parser would read those as damage).
#[cfg(feature = "truehd")]
pub struct TsTrueHdReader {
    reader: PacketReader<File>,
    pid: u16,
    pes: PesAssembler,
    pending: Vec<u8>,
    at: usize,
    finished: bool,
}

#[cfg(feature = "truehd")]
impl TsTrueHdReader {
    pub fn open(path: &Path) -> Result<Self> {
        let probe = TsProbe::open_head(path)?;
        let Choice::TrueHd(stream) = choose(&probe) else {
            anyhow::bail!("mpeg-ts: no TrueHD audio: {}", path.display());
        };
        let pid = stream.pid;
        let mut file =
            File::open(path).with_context(|| format!("open mpeg-ts: {}", path.display()))?;
        file.seek(SeekFrom::Start(probe.first_packet))
            .with_context(|| format!("seek mpeg-ts: {}", path.display()))?;
        Ok(Self {
            reader: PacketReader::new(file, probe.format),
            pid,
            pes: PesAssembler::default(),
            pending: Vec::new(),
            at: 0,
            finished: false,
        })
    }

    fn next_payload(&mut self) -> std::io::Result<bool> {
        loop {
            let pes = match self.reader.next_packet()? {
                Some(packet) if packet.pid != self.pid => continue,
                Some(packet) => match self.pes.push(&packet) {
                    Some(pes) => pes,
                    None => continue,
                },
                None => match self.pes.finish(self.pid) {
                    Some(pes) => pes,
                    None => return Ok(false),
                },
            };
            // An AC-3 sync word: the compatibility stream, not ours.
            if pes.data.starts_with(&[0x0B, 0x77]) {
                continue;
            }
            self.pending = pes.data;
            self.at = 0;
            return Ok(true);
        }
    }
}

#[cfg(feature = "truehd")]
impl std::io::Read for TsTrueHdReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at >= self.pending.len() {
            if self.finished || !self.next_payload()? {
                self.finished = true;
                return Ok(0);
            }
        }
        let take = out.len().min(self.pending.len() - self.at);
        out[..take].copy_from_slice(&self.pending[self.at..self.at + take]);
        self.at += take;
        Ok(take)
    }
}
