//! A RIFF/RF64 writer with no opinions.
//!
//! Every ordinary WAV writer decides the header for you: hound picks the
//! channel mask (and clamps it to 18 bits), always agrees `data` with the bytes
//! it wrote, and only ever writes `RIFF`. Half the fixtures here exist to prove
//! what the app does with headers no such writer would produce, so this module
//! takes each field as a parameter and writes exactly what it is told.

/// How samples are laid out in the `data` chunk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    /// 8-bit PCM, unsigned and biased by 128 -- the one PCM depth that is not
    /// two's complement.
    PcmU8,
    PcmI16,
    PcmI24,
    PcmI32,
    F32,
    F64,
}

impl Encoding {
    pub fn bits(self) -> u16 {
        match self {
            Self::PcmU8 => 8,
            Self::PcmI16 => 16,
            Self::PcmI24 => 24,
            Self::PcmI32 | Self::F32 => 32,
            Self::F64 => 64,
        }
    }

    pub fn bytes_per_sample(self) -> usize {
        self.bits() as usize / 8
    }

    /// The `wFormatTag` a non-extensible header would carry: 1 for PCM, 3 for
    /// IEEE float.
    pub fn format_tag(self) -> u16 {
        match self {
            Self::F32 | Self::F64 => 3,
            _ => 1,
        }
    }

    pub fn encode(self, value: f32, out: &mut Vec<u8>) {
        let v = value.clamp(-1.0, 1.0);
        match self {
            Self::PcmU8 => out.push(((v * 127.0).round() + 128.0).clamp(0.0, 255.0) as u8),
            Self::PcmI16 => out.extend_from_slice(&((v * 32_767.0).round() as i16).to_le_bytes()),
            Self::PcmI24 => {
                let q = (v * 8_388_607.0).round() as i32;
                out.extend_from_slice(&q.to_le_bytes()[0..3]);
            }
            Self::PcmI32 => {
                out.extend_from_slice(&((v as f64 * 2_147_483_647.0).round() as i32).to_le_bytes())
            }
            Self::F32 => out.extend_from_slice(&v.to_le_bytes()),
            Self::F64 => out.extend_from_slice(&(v as f64).to_le_bytes()),
        }
    }
}

/// The four-byte form at the head of the file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Container {
    Riff,
    /// RF64 and BW64 are the same layout; only the marker differs.
    Rf64,
    Bw64,
}

impl Container {
    fn marker(self) -> &'static [u8; 4] {
        match self {
            Self::Riff => b"RIFF",
            Self::Rf64 => b"RF64",
            Self::Bw64 => b"BW64",
        }
    }

    fn is_64bit(self) -> bool {
        !matches!(self, Self::Riff)
    }
}

pub const SUBFORMAT_PCM: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];
pub const SUBFORMAT_IEEE_FLOAT: [u8; 16] = [
    0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];
/// Not a format anything here can decode. Used to prove the app reports an
/// unreadable file rather than showing an empty editor.
pub const SUBFORMAT_UNKNOWN: [u8; 16] = [
    0x99, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

/// The `WAVE_FORMAT_EXTENSIBLE` extension, written verbatim.
#[derive(Clone, Copy, Debug)]
pub struct Extensible {
    pub valid_bits: u16,
    /// `dwChannelMask`. Deliberately free to disagree with the channel count.
    pub channel_mask: u32,
    pub subformat: [u8; 16],
}

impl Extensible {
    /// The mask a well-behaved writer emits: the lowest `channels` speaker bits.
    pub fn sequential_mask(channels: u16) -> u32 {
        if channels >= 32 {
            u32::MAX
        } else {
            (1u32 << channels) - 1
        }
    }

    pub fn pcm(encoding: Encoding, channels: u16) -> Self {
        Self {
            valid_bits: encoding.bits(),
            channel_mask: Self::sequential_mask(channels),
            subformat: if encoding.format_tag() == 3 {
                SUBFORMAT_IEEE_FLOAT
            } else {
                SUBFORMAT_PCM
            },
        }
    }
}

/// A chunk to splice into the file, in the position named by the field holding
/// it.
pub struct ExtraChunk {
    pub id: [u8; 4],
    pub payload: Vec<u8>,
}

/// Everything the writer needs, with nothing inferred that a fixture might want
/// to bend.
pub struct WavSpec {
    pub container: Container,
    pub channels: u16,
    pub sample_rate: u32,
    pub encoding: Encoding,
    /// `None` writes a plain 16-byte `fmt `; `Some` writes the 40-byte
    /// extensible form with `wFormatTag = 0xFFFE`.
    pub extensible: Option<Extensible>,
    /// Overrides `nBlockAlign`. Left `None` this is `channels * bytes_per_sample`.
    pub block_align_override: Option<u16>,
    /// Overrides the `data` chunk's declared length without changing the bytes
    /// actually written, for files that claim more or less than they hold.
    pub data_len_override: Option<u32>,
    pub before_fmt: Vec<ExtraChunk>,
    pub after_data: Vec<ExtraChunk>,
}

impl WavSpec {
    pub fn new(channels: u16, sample_rate: u32, encoding: Encoding) -> Self {
        Self {
            container: Container::Riff,
            channels,
            sample_rate,
            encoding,
            extensible: None,
            block_align_override: None,
            data_len_override: None,
            before_fmt: Vec::new(),
            after_data: Vec::new(),
        }
    }

    /// The extensible form every writer uses past two channels or 16 bits.
    pub fn extensible(mut self) -> Self {
        self.extensible = Some(Extensible::pcm(self.encoding, self.channels));
        self
    }

    pub fn with_extensible(mut self, ext: Extensible) -> Self {
        self.extensible = Some(ext);
        self
    }

    pub fn container(mut self, container: Container) -> Self {
        self.container = container;
        self
    }

    fn block_align(&self) -> u16 {
        self.block_align_override.unwrap_or_else(|| {
            (self.channels as usize * self.encoding.bytes_per_sample()).min(u16::MAX as usize)
                as u16
        })
    }

    fn fmt_payload(&self) -> Vec<u8> {
        let mut fmt = Vec::with_capacity(40);
        let tag = match &self.extensible {
            Some(_) => 0xFFFEu16,
            None => self.encoding.format_tag(),
        };
        let block_align = self.block_align();
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&self.channels.to_le_bytes());
        fmt.extend_from_slice(&self.sample_rate.to_le_bytes());
        fmt.extend_from_slice(&(self.sample_rate * block_align as u32).to_le_bytes());
        fmt.extend_from_slice(&block_align.to_le_bytes());
        fmt.extend_from_slice(&self.encoding.bits().to_le_bytes());
        if let Some(ext) = &self.extensible {
            fmt.extend_from_slice(&22u16.to_le_bytes());
            fmt.extend_from_slice(&ext.valid_bits.to_le_bytes());
            fmt.extend_from_slice(&ext.channel_mask.to_le_bytes());
            fmt.extend_from_slice(&ext.subformat);
        }
        fmt
    }

    /// Interleave and encode `channels`, then wrap the whole file.
    ///
    /// `channels` is per-channel planar; every channel must be the same length.
    pub fn render(&self, channels: &[Vec<f32>]) -> Vec<u8> {
        let frames = channels.first().map(|c| c.len()).unwrap_or(0);
        assert!(
            channels.iter().all(|c| c.len() == frames),
            "channels must be the same length"
        );
        assert_eq!(
            channels.len(),
            self.channels as usize,
            "channel count must match the header"
        );
        let mut data =
            Vec::with_capacity(frames * channels.len() * self.encoding.bytes_per_sample());
        for frame in 0..frames {
            for channel in channels {
                self.encoding.encode(channel[frame], &mut data);
            }
        }
        self.wrap(data)
    }

    /// Wrap already-encoded `data` bytes in the header this spec describes.
    pub fn wrap(&self, data: Vec<u8>) -> Vec<u8> {
        let declared_data_len = self.data_len_override.unwrap_or(data.len() as u32);

        let mut body = Vec::new();
        // RF64 puts `ds64` first, immediately after the form type, and the
        // 32-bit size fields it replaces carry -1.
        if self.container.is_64bit() {
            body.extend_from_slice(b"ds64");
            body.extend_from_slice(&28u32.to_le_bytes());
            // riffSize and dataSize are patched once the total is known; the
            // sample count and table length are written now.
            body.extend_from_slice(&0u64.to_le_bytes());
            body.extend_from_slice(&(declared_data_len as u64).to_le_bytes());
            let block_align = self.block_align().max(1) as u64;
            body.extend_from_slice(&(declared_data_len as u64 / block_align).to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
        }
        for chunk in &self.before_fmt {
            push_chunk(&mut body, &chunk.id, &chunk.payload);
        }
        push_chunk(&mut body, b"fmt ", &self.fmt_payload());

        body.extend_from_slice(b"data");
        let data_size_field = if self.container.is_64bit() {
            u32::MAX
        } else {
            declared_data_len
        };
        body.extend_from_slice(&data_size_field.to_le_bytes());
        body.extend_from_slice(&data);
        // A chunk with an odd payload is followed by a pad byte. The declared
        // length stays odd; only the file grows.
        if data.len() & 1 != 0 {
            body.push(0);
        }
        for chunk in &self.after_data {
            push_chunk(&mut body, &chunk.id, &chunk.payload);
        }

        let mut out = Vec::with_capacity(body.len() + 12);
        out.extend_from_slice(self.container.marker());
        let riff_size = (body.len() + 4) as u64;
        if self.container.is_64bit() {
            out.extend_from_slice(&u32::MAX.to_le_bytes());
        } else {
            out.extend_from_slice(&(riff_size as u32).to_le_bytes());
        }
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(&body);
        if self.container.is_64bit() {
            // Patch ds64's riffSize now that the body is complete: it sits
            // 12 (form) + 8 (chunk header) bytes in.
            let at = 12 + 8;
            out[at..at + 8].copy_from_slice(&riff_size.to_le_bytes());
        }
        out
    }
}

fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() & 1 != 0 {
        out.push(0);
    }
}

/// A `cue ` chunk naming `positions` in frames.
pub fn cue_chunk(positions: &[u32]) -> ExtraChunk {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(positions.len() as u32).to_le_bytes());
    for (index, &position) in positions.iter().enumerate() {
        payload.extend_from_slice(&(index as u32 + 1).to_le_bytes()); // dwName
        payload.extend_from_slice(&position.to_le_bytes()); // dwPosition
        payload.extend_from_slice(b"data"); // fccChunk
        payload.extend_from_slice(&0u32.to_le_bytes()); // dwChunkStart
        payload.extend_from_slice(&0u32.to_le_bytes()); // dwBlockStart
        payload.extend_from_slice(&position.to_le_bytes()); // dwSampleOffset
    }
    ExtraChunk {
        id: *b"cue ",
        payload,
    }
}

/// A `smpl` chunk carrying one forward loop over `[start, end)` in frames.
pub fn smpl_chunk(sample_rate: u32, start: u32, end: u32) -> ExtraChunk {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_le_bytes()); // dwManufacturer
    payload.extend_from_slice(&0u32.to_le_bytes()); // dwProduct
    payload.extend_from_slice(&(1_000_000_000u32 / sample_rate.max(1)).to_le_bytes()); // dwSamplePeriod
    payload.extend_from_slice(&60u32.to_le_bytes()); // dwMIDIUnityNote
    payload.extend_from_slice(&0u32.to_le_bytes()); // dwMIDIPitchFraction
    payload.extend_from_slice(&0u32.to_le_bytes()); // dwSMPTEFormat
    payload.extend_from_slice(&0u32.to_le_bytes()); // dwSMPTEOffset
    payload.extend_from_slice(&1u32.to_le_bytes()); // cSampleLoops
    payload.extend_from_slice(&0u32.to_le_bytes()); // cbSamplerData
    payload.extend_from_slice(&0u32.to_le_bytes()); // dwIdentifier
    payload.extend_from_slice(&0u32.to_le_bytes()); // dwType (0 = forward)
    payload.extend_from_slice(&start.to_le_bytes()); // dwStart
    payload.extend_from_slice(&end.to_le_bytes()); // dwEnd
    payload.extend_from_slice(&0u32.to_le_bytes()); // dwFraction
    payload.extend_from_slice(&0u32.to_le_bytes()); // dwPlayCount
    ExtraChunk {
        id: *b"smpl",
        payload,
    }
}

/// A `LIST`/`INFO` chunk with a single name field.
pub fn list_info_chunk(name: &str) -> ExtraChunk {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"INFO");
    let mut value = name.as_bytes().to_vec();
    value.push(0);
    payload.extend_from_slice(b"INAM");
    payload.extend_from_slice(&(value.len() as u32).to_le_bytes());
    payload.extend_from_slice(&value);
    if value.len() & 1 != 0 {
        payload.push(0);
    }
    ExtraChunk {
        id: *b"LIST",
        payload,
    }
}

/// A Broadcast Wave `bext` chunk.
///
/// Every field is fixed text: the timestamps a real encoder writes would make
/// the fixture differ byte-for-byte on every regeneration.
pub fn bext_chunk(description: &str) -> ExtraChunk {
    let mut payload = vec![0u8; 602];
    let desc = description.as_bytes();
    let len = desc.len().min(256);
    payload[..len].copy_from_slice(&desc[..len]);
    payload[256..256 + 9].copy_from_slice(b"NeoWaves\0"); // Originator
    payload[320..320 + 10].copy_from_slice(b"2026-01-01"); // OriginationDate
    payload[330..330 + 8].copy_from_slice(b"00:00:00"); // OriginationTime
    ExtraChunk {
        id: *b"bext",
        payload,
    }
}

/// A `JUNK` chunk, the filler every DAW leaves room with.
pub fn junk_chunk(len: usize) -> ExtraChunk {
    ExtraChunk {
        id: *b"JUNK",
        payload: vec![0u8; len],
    }
}
