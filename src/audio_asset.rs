//! Stable audio identity and revision-based access.
//!
//! UI paths are labels/locations, not audio identity. `AudioAssetId` remains
//! stable while the current revision can move between resident, managed and
//! external backing without changing a `MediaItem` into a different source.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const MAX_RESIDENT_DECODE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioAssetId(pub u128);

impl AudioAssetId {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed) as u128;
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0);
        Self((time << 32) ^ ((std::process::id() as u128) << 64) ^ sequence)
    }

    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }

    pub fn from_hex(value: &str) -> Option<Self> {
        u128::from_str_radix(value.trim(), 16).ok().map(Self)
    }
}

impl Default for AudioAssetId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetRevision(pub u64);

impl AssetRevision {
    pub const INITIAL: Self = Self(1);

    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1).max(1))
    }
}

#[derive(Clone, Debug)]
pub enum AudioBacking {
    ExternalFile(PathBuf),
    ManagedFile(PathBuf),
    ResidentBuffer(Arc<crate::audio::AudioBuffer>),
    DerivedRevision {
        path: PathBuf,
        parent_revision: AssetRevision,
    },
}

impl AudioBacking {
    pub fn file_path(&self) -> Option<&Path> {
        match self {
            Self::ExternalFile(path) | Self::ManagedFile(path) => Some(path),
            Self::DerivedRevision { path, .. } => Some(path),
            Self::ResidentBuffer(_) => None,
        }
    }

    pub fn is_managed(&self) -> bool {
        matches!(self, Self::ManagedFile(_) | Self::DerivedRevision { .. })
    }
}

#[derive(Clone, Debug)]
pub struct AudioAssetDescriptor {
    pub id: AudioAssetId,
    pub revision: AssetRevision,
    pub backing: AudioBacking,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub frame_count: Option<u64>,
}

impl AudioAssetDescriptor {
    pub fn external(path: PathBuf) -> Self {
        Self::from_file_backing(AudioBacking::ExternalFile(path))
    }

    /// Create a descriptor without touching the source path. List rows are
    /// constructed on the UI thread and a removable/network file can block
    /// even for a small header probe; the metadata worker fills these facts
    /// later.
    pub fn external_unprobed(path: PathBuf) -> Self {
        Self {
            id: AudioAssetId::new(),
            revision: AssetRevision::INITIAL,
            backing: AudioBacking::ExternalFile(path),
            sample_rate: 0,
            channels: 0,
            bits_per_sample: 0,
            frame_count: None,
        }
    }

    pub fn managed(path: PathBuf) -> Self {
        Self::from_file_backing(AudioBacking::ManagedFile(path))
    }

    fn from_file_backing(backing: AudioBacking) -> Self {
        let info = backing
            .file_path()
            .and_then(|path| crate::wav_stream::read_wave_pcm_info(path).ok().flatten());
        Self {
            id: AudioAssetId::new(),
            revision: AssetRevision::INITIAL,
            backing,
            sample_rate: info.map(|value| value.sample_rate).unwrap_or(0),
            channels: info.map(|value| value.channels).unwrap_or(0),
            bits_per_sample: info.map(|value| value.bits_per_sample).unwrap_or(0),
            frame_count: info.map(|value| value.frame_count),
        }
    }

    pub fn resident(
        audio: Arc<crate::audio::AudioBuffer>,
        sample_rate: u32,
        bits_per_sample: u16,
    ) -> Self {
        Self {
            id: AudioAssetId::new(),
            revision: AssetRevision::INITIAL,
            sample_rate: sample_rate.max(1),
            channels: audio.channel_count() as u16,
            bits_per_sample,
            frame_count: Some(audio.len() as u64),
            backing: AudioBacking::ResidentBuffer(audio),
        }
    }

    pub fn estimated_decoded_bytes(&self) -> Option<u64> {
        self.frame_count.map(|frames| {
            frames
                .saturating_mul(self.channels.max(1) as u64)
                .saturating_mul(4)
        })
    }

    pub fn may_reside_in_memory(&self) -> bool {
        self.estimated_decoded_bytes()
            .map(|bytes| bytes <= MAX_RESIDENT_DECODE_BYTES)
            .unwrap_or(false)
    }

    /// Whether a known-size asset is too large for a resident editor buffer.
    ///
    /// An unprobed list row has no size yet. Treating "unknown" as oversized
    /// made every freshly scanned WAV open as a paged, zero-sample tab before
    /// its metadata worker had a chance to publish the header. Editor decode
    /// is already single-flight, so unknown inputs are safely decoded one at
    /// a time; only a positively known oversized asset takes the paged path.
    pub fn requires_paged_editor(&self) -> bool {
        self.estimated_decoded_bytes()
            .is_some_and(|bytes| bytes > MAX_RESIDENT_DECODE_BYTES)
    }

    pub fn access(&self) -> AssetAccess<'_> {
        AssetAccess { descriptor: self }
    }
}

pub struct AssetAccess<'a> {
    descriptor: &'a AudioAssetDescriptor,
}

impl AssetAccess<'_> {
    pub fn open_pcm_stream(&self) -> Result<AssetPcmStream> {
        match &self.descriptor.backing {
            AudioBacking::ResidentBuffer(audio) => Ok(AssetPcmStream::Resident {
                audio: Arc::clone(audio),
                position: 0,
            }),
            backing => {
                let path = backing.file_path().context("asset has no file backing")?;
                let info = crate::wav_stream::read_wave_pcm_info(path)?.with_context(|| {
                    format!("asset is not streamable PCM WAVE: {}", path.display())
                })?;
                Ok(AssetPcmStream::Wave {
                    file: File::open(path)?,
                    info,
                    position: 0,
                })
            }
        }
    }

    pub fn read_range(
        &self,
        start_frame: u64,
        frame_count: usize,
    ) -> Result<crate::audio::AudioBuffer> {
        let mut stream = self.open_pcm_stream()?;
        stream.seek_frame(start_frame)?;
        stream.read_frames(frame_count)
    }

    pub fn materialize_current_revision(&self, destination: &Path) -> Result<()> {
        if let Some(source) = self.descriptor.backing.file_path() {
            if source == destination {
                return Ok(());
            }
            if !destination.exists() && std::fs::hard_link(source, destination).is_ok() {
                return Ok(());
            }
            std::fs::copy(source, destination).with_context(|| {
                format!(
                    "materialize asset {} -> {}",
                    source.display(),
                    destination.display()
                )
            })?;
            return Ok(());
        }
        let mut stream = self.open_pcm_stream()?;
        let mut writer = crate::wav_stream::StreamingWaveWriter::create_float32(
            destination,
            self.descriptor.channels.max(1),
            self.descriptor.sample_rate.max(1),
        )?;
        loop {
            let block = stream.read_frames(16_384)?;
            let frames = block.len();
            if frames == 0 {
                break;
            }
            let interleaved = interleave(&block.channels, frames);
            writer.write_interleaved_f32(&interleaved)?;
        }
        writer.finalize()?;
        Ok(())
    }

    pub fn render_new_revision<F>(
        &self,
        destination: &Path,
        mut process: F,
    ) -> Result<AudioAssetDescriptor>
    where
        F: FnMut(&[Vec<f32>], u32) -> Result<Vec<Vec<f32>>>,
    {
        let mut stream = self.open_pcm_stream()?;
        let mut writer = crate::wav_stream::StreamingWaveWriter::create_float32(
            destination,
            self.descriptor.channels.max(1),
            self.descriptor.sample_rate.max(1),
        )?;
        let mut output_frames = 0u64;
        loop {
            let block = stream.read_frames(16_384)?;
            if block.is_empty() {
                break;
            }
            let processed = process(&block.channels, self.descriptor.sample_rate.max(1))?;
            anyhow::ensure!(
                processed.len() == self.descriptor.channels.max(1) as usize,
                "streaming revision changed channel count"
            );
            let frames = processed.iter().map(Vec::len).min().unwrap_or(0);
            writer.write_interleaved_f32(&interleave(&processed, frames))?;
            output_frames = output_frames.saturating_add(frames as u64);
        }
        writer.finalize()?;
        Ok(AudioAssetDescriptor {
            id: self.descriptor.id,
            revision: self.descriptor.revision.next(),
            backing: AudioBacking::DerivedRevision {
                path: destination.to_path_buf(),
                parent_revision: self.descriptor.revision,
            },
            sample_rate: self.descriptor.sample_rate,
            channels: self.descriptor.channels,
            bits_per_sample: 32,
            frame_count: Some(output_frames),
        })
    }
}

pub enum AssetPcmStream {
    Resident {
        audio: Arc<crate::audio::AudioBuffer>,
        position: u64,
    },
    Wave {
        file: File,
        info: crate::wav_stream::WavePcmInfo,
        position: u64,
    },
}

impl AssetPcmStream {
    pub fn seek_frame(&mut self, frame: u64) -> Result<()> {
        match self {
            Self::Resident { audio, position } => {
                *position = frame.min(audio.len() as u64);
            }
            Self::Wave { info, position, .. } => {
                *position = frame.min(info.frame_count);
            }
        }
        Ok(())
    }

    pub fn read_frames(&mut self, requested: usize) -> Result<crate::audio::AudioBuffer> {
        match self {
            Self::Resident { audio, position } => {
                let start = (*position as usize).min(audio.len());
                let end = start.saturating_add(requested).min(audio.len());
                *position = end as u64;
                Ok(crate::audio::AudioBuffer::from_channels(
                    audio
                        .channels
                        .iter()
                        .map(|channel| channel[start..end].to_vec())
                        .collect(),
                ))
            }
            Self::Wave {
                file,
                info,
                position,
            } => {
                let available = info.frame_count.saturating_sub(*position);
                let frames = requested.min(usize::try_from(available).unwrap_or(usize::MAX));
                if frames == 0 {
                    return Ok(crate::audio::AudioBuffer::from_channels(vec![
                        Vec::new();
                        info.channels.max(1)
                            as usize
                    ]));
                }
                let byte_len = frames.saturating_mul(info.block_align as usize);
                let mut bytes = vec![0u8; byte_len];
                let offset = info
                    .data_offset
                    .saturating_add(position.saturating_mul(info.block_align as u64));
                file.seek(SeekFrom::Start(offset))?;
                file.read_exact(&mut bytes)?;
                let mut channels = vec![Vec::with_capacity(frames); info.channels as usize];
                let bytes_per_sample = info.block_align as usize / info.channels.max(1) as usize;
                for frame in bytes.chunks_exact(info.block_align as usize) {
                    for (channel_index, output) in channels.iter_mut().enumerate() {
                        let start = channel_index * bytes_per_sample;
                        output.push(decode_sample(
                            &frame[start..start + bytes_per_sample],
                            *info,
                        ));
                    }
                }
                *position = position.saturating_add(frames as u64);
                Ok(crate::audio::AudioBuffer::from_channels(channels))
            }
        }
    }
}

fn decode_sample(bytes: &[u8], info: crate::wav_stream::WavePcmInfo) -> f32 {
    match (info.audio_format, info.bits_per_sample) {
        (3, 32) if bytes.len() >= 4 => {
            f32::from_le_bytes(bytes[0..4].try_into().unwrap()).clamp(-1.0, 1.0)
        }
        (1, 8) if !bytes.is_empty() => ((bytes[0] as f32 - 128.0) / 128.0).clamp(-1.0, 1.0),
        (1, 16) if bytes.len() >= 2 => {
            (i16::from_le_bytes(bytes[0..2].try_into().unwrap()) as f32 / 32768.0).clamp(-1.0, 1.0)
        }
        (1, 24) if bytes.len() >= 3 => {
            let sign = if bytes[2] & 0x80 != 0 { 0xff } else { 0 };
            (i32::from_le_bytes([bytes[0], bytes[1], bytes[2], sign]) as f32 / 8_388_607.0)
                .clamp(-1.0, 1.0)
        }
        (1, 32) if bytes.len() >= 4 => (i32::from_le_bytes(bytes[0..4].try_into().unwrap()) as f32
            / i32::MAX as f32)
            .clamp(-1.0, 1.0),
        _ => 0.0,
    }
}

fn interleave(channels: &[Vec<f32>], frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames.saturating_mul(channels.len()));
    for frame in 0..frames {
        for channel in channels {
            out.push(channel[frame]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resident_range_is_bounded() {
        let audio = Arc::new(crate::audio::AudioBuffer::from_channels(vec![
            vec![0.0, 0.1, 0.2, 0.3],
            vec![0.0, -0.1, -0.2, -0.3],
        ]));
        let asset = AudioAssetDescriptor::resident(audio, 48_000, 32);
        let range = asset.access().read_range(1, 2).unwrap();
        assert_eq!(range.channels[0], vec![0.1, 0.2]);
        assert_eq!(range.channels[1], vec![-0.1, -0.2]);
    }

    #[test]
    fn resident_limit_is_256_mib() {
        let asset = AudioAssetDescriptor {
            id: AudioAssetId::new(),
            revision: AssetRevision::INITIAL,
            backing: AudioBacking::ExternalFile(PathBuf::from("large.wav")),
            sample_rate: 48_000,
            channels: 2,
            bits_per_sample: 32,
            frame_count: Some(MAX_RESIDENT_DECODE_BYTES / 8 + 1),
        };
        assert!(!asset.may_reside_in_memory());
        assert!(asset.requires_paged_editor());
    }

    #[test]
    fn an_unprobed_asset_decodes_single_flight_instead_of_becoming_paged() {
        let asset = AudioAssetDescriptor::external_unprobed(PathBuf::from("pending.wav"));
        assert!(!asset.may_reside_in_memory());
        assert!(!asset.requires_paged_editor());
    }
}
