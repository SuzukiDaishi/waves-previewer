//! Recoverable streaming WAVE writer used by long recordings.
//!
//! The file starts as ordinary RIFF/WAVE with a reserved 28-byte `JUNK`
//! chunk.  Once the payload no longer fits in a 32-bit RIFF chunk, the same
//! bytes are promoted in-place to RF64/`ds64`; audio data never has to move.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const DATA_OFFSET: u64 = 80;
const DS64_PAYLOAD_LEN: u32 = 28;
const INLINE_METADATA_REWRITE_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveContainer {
    Riff,
    Rf64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaveCheckpoint {
    pub container: WaveContainer,
    pub frames: u64,
    pub data_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavePcmInfo {
    pub container: WaveContainer,
    pub audio_format: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub block_align: u16,
    pub data_offset: u64,
    pub data_len: u64,
    pub frame_count: u64,
}

/// Returns true when mutating RIFF chunks would require an unsafe/expensive
/// whole-file rewrite. RF64/BW64 also need sidecars because their positions
/// may not fit the 32-bit `cue`/`smpl` fields.
pub fn wave_metadata_requires_sidecar(path: &Path) -> bool {
    if std::fs::metadata(path)
        .map(|meta| meta.len() > INLINE_METADATA_REWRITE_LIMIT)
        .unwrap_or(false)
    {
        return true;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut id = [0u8; 4];
    file.read_exact(&mut id).is_ok() && matches!(&id, b"RF64" | b"BW64")
}

/// Read the PCM mapping from RIFF or RF64/BW64 without allocating audio.
pub fn read_wave_pcm_info(path: &Path) -> Result<Option<WavePcmInfo>> {
    let mut file =
        File::open(path).with_context(|| format!("open WAVE header: {}", path.display()))?;
    let file_len = file.metadata()?.len();
    let mut root = [0u8; 12];
    file.read_exact(&mut root)
        .with_context(|| format!("read WAVE header: {}", path.display()))?;
    let container = match &root[0..4] {
        b"RIFF" => WaveContainer::Riff,
        b"RF64" | b"BW64" => WaveContainer::Rf64,
        _ => return Ok(None),
    };
    if &root[8..12] != b"WAVE" {
        return Ok(None);
    }

    let mut ds64_data_size = None;
    let mut audio_format = 0u16;
    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut bits_per_sample = 0u16;
    let mut block_align = 0u16;
    let mut data = None;
    loop {
        let mut header = [0u8; 8];
        match file.read_exact(&mut header) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err.into()),
        }
        let declared = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
        let payload = file.stream_position()?;
        match &header[0..4] {
            b"ds64" => {
                let read_len = declared.min(28) as usize;
                let mut bytes = vec![0u8; read_len];
                file.read_exact(&mut bytes)?;
                if bytes.len() >= 16 {
                    ds64_data_size = Some(u64::from_le_bytes(bytes[8..16].try_into().unwrap()));
                }
            }
            b"fmt " => {
                let read_len = declared.min(64) as usize;
                let mut bytes = vec![0u8; read_len];
                file.read_exact(&mut bytes)?;
                if bytes.len() >= 16 {
                    audio_format = u16::from_le_bytes(bytes[0..2].try_into().unwrap());
                    channels = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
                    sample_rate = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
                    block_align = u16::from_le_bytes(bytes[12..14].try_into().unwrap());
                    bits_per_sample = u16::from_le_bytes(bytes[14..16].try_into().unwrap());
                    if audio_format == 0xFFFE && bytes.len() >= 26 {
                        audio_format = u16::from_le_bytes(bytes[24..26].try_into().unwrap());
                    }
                }
            }
            b"data" => {
                let logical_len = if declared == u32::MAX as u64 {
                    ds64_data_size.unwrap_or(declared)
                } else {
                    declared
                };
                let available = file_len.saturating_sub(payload);
                data = Some((payload, logical_len.min(available)));
                break;
            }
            _ => {}
        }
        let next = payload
            .checked_add(declared)
            .and_then(|value| value.checked_add(declared & 1))
            .context("WAVE chunk offset overflow")?;
        if next > file_len {
            break;
        }
        file.seek(SeekFrom::Start(next))?;
    }
    let Some((data_offset, data_len)) = data else {
        return Ok(None);
    };
    let supported = matches!(
        (audio_format, bits_per_sample),
        (1, 8) | (1, 16) | (1, 24) | (1, 32) | (3, 32)
    );
    if !supported || channels == 0 || sample_rate == 0 || block_align == 0 {
        return Ok(None);
    }
    Ok(Some(WavePcmInfo {
        container,
        audio_format,
        channels,
        sample_rate,
        bits_per_sample,
        block_align,
        data_offset,
        data_len,
        frame_count: data_len / block_align as u64,
    }))
}

pub struct StreamingWaveWriter {
    path: PathBuf,
    writer: BufWriter<File>,
    channels: u16,
    sample_rate: u32,
    frames: u64,
    samples_in_frame: u16,
    data_bytes: u64,
}

impl StreamingWaveWriter {
    pub fn create_float32(path: &Path, channels: u16, sample_rate: u32) -> Result<Self> {
        anyhow::ensure!(channels > 0, "recording channel count must be non-zero");
        anyhow::ensure!(sample_rate > 0, "recording sample rate must be non-zero");
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("create recording WAV: {}", path.display()))?;
        let mut this = Self {
            path: path.to_path_buf(),
            writer: BufWriter::new(file),
            channels,
            sample_rate,
            frames: 0,
            samples_in_frame: 0,
            data_bytes: 0,
        };
        this.write_initial_header()?;
        Ok(this)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn data_bytes(&self) -> u64 {
        self.data_bytes
    }

    pub fn write_interleaved_f32(&mut self, samples: &[f32]) -> Result<()> {
        for &sample in samples {
            self.writer
                .write_all(&sample.to_le_bytes())
                .with_context(|| format!("write recording audio: {}", self.path.display()))?;
            self.data_bytes = self.data_bytes.saturating_add(4);
            self.samples_in_frame += 1;
            if self.samples_in_frame == self.channels {
                self.samples_in_frame = 0;
                self.frames = self.frames.saturating_add(1);
            }
        }
        Ok(())
    }

    /// Flush audio and update the size fields so a process/device failure
    /// leaves a playable partial take. This is safe to call while recording.
    pub fn checkpoint(&mut self) -> Result<WaveCheckpoint> {
        self.rewrite_header(false)
    }

    pub fn finalize(mut self) -> Result<WaveCheckpoint> {
        let checkpoint = self.rewrite_header(true)?;
        self.writer
            .get_ref()
            .sync_data()
            .with_context(|| format!("sync recording WAV: {}", self.path.display()))?;
        Ok(checkpoint)
    }

    fn write_initial_header(&mut self) -> Result<()> {
        self.writer.write_all(b"RIFF")?;
        self.writer.write_all(&0u32.to_le_bytes())?;
        self.writer.write_all(b"WAVE")?;
        self.writer.write_all(b"JUNK")?;
        self.writer.write_all(&DS64_PAYLOAD_LEN.to_le_bytes())?;
        self.writer.write_all(&[0u8; DS64_PAYLOAD_LEN as usize])?;
        self.writer.write_all(b"fmt ")?;
        self.writer.write_all(&16u32.to_le_bytes())?;
        self.writer.write_all(&3u16.to_le_bytes())?; // IEEE float
        self.writer.write_all(&self.channels.to_le_bytes())?;
        self.writer.write_all(&self.sample_rate.to_le_bytes())?;
        let block_align = self.channels.saturating_mul(4);
        let byte_rate = self.sample_rate.saturating_mul(block_align as u32);
        self.writer.write_all(&byte_rate.to_le_bytes())?;
        self.writer.write_all(&block_align.to_le_bytes())?;
        self.writer.write_all(&32u16.to_le_bytes())?;
        self.writer.write_all(b"data")?;
        self.writer.write_all(&0u32.to_le_bytes())?;
        debug_assert_eq!(self.writer.stream_position()?, DATA_OFFSET);
        Ok(())
    }

    fn rewrite_header(&mut self, finalizing: bool) -> Result<WaveCheckpoint> {
        self.writer.flush()?;
        let end = DATA_OFFSET.saturating_add(self.data_bytes);
        let riff_size = end.saturating_sub(8);
        let rf64 = self.data_bytes > u32::MAX as u64 || riff_size > u32::MAX as u64;

        self.writer.seek(SeekFrom::Start(0))?;
        self.writer
            .write_all(if rf64 { b"RF64" } else { b"RIFF" })?;
        self.writer
            .write_all(&(if rf64 { u32::MAX } else { riff_size as u32 }).to_le_bytes())?;
        self.writer.write_all(b"WAVE")?;
        self.writer
            .write_all(if rf64 { b"ds64" } else { b"JUNK" })?;
        self.writer.write_all(&DS64_PAYLOAD_LEN.to_le_bytes())?;
        if rf64 {
            self.writer.write_all(&riff_size.to_le_bytes())?;
            self.writer.write_all(&self.data_bytes.to_le_bytes())?;
            self.writer.write_all(&self.frames.to_le_bytes())?;
            self.writer.write_all(&0u32.to_le_bytes())?;
        } else {
            self.writer.write_all(&[0u8; DS64_PAYLOAD_LEN as usize])?;
        }
        self.writer.seek(SeekFrom::Start(76))?;
        self.writer.write_all(
            &(if rf64 {
                u32::MAX
            } else {
                self.data_bytes as u32
            })
            .to_le_bytes(),
        )?;
        self.writer.flush()?;
        if !finalizing {
            self.writer.seek(SeekFrom::Start(end))?;
        }
        Ok(WaveCheckpoint {
            container: if rf64 {
                WaveContainer::Rf64
            } else {
                WaveContainer::Riff
            },
            frames: self.frames,
            data_bytes: self.data_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "neowaves_wav_stream_{}_{}_{}.wav",
            std::process::id(),
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn checkpoint_and_finalize_keep_all_frames() {
        let path = temp_path("frames");
        let mut writer = StreamingWaveWriter::create_float32(&path, 2, 48_000).unwrap();
        writer
            .write_interleaved_f32(&[0.1, -0.1, 0.2, -0.2])
            .unwrap();
        let checkpoint = writer.checkpoint().unwrap();
        assert_eq!(checkpoint.frames, 2);
        writer.write_interleaved_f32(&[0.3, -0.3]).unwrap();
        let final_state = writer.finalize().unwrap();
        assert_eq!(final_state.frames, 3);
        assert_eq!(final_state.data_bytes, 24);
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 3);
        drop(reader);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reserved_header_is_standard_riff_below_4gib() {
        let path = temp_path("riff");
        let writer = StreamingWaveWriter::create_float32(&path, 1, 48_000).unwrap();
        writer.finalize().unwrap();
        let mut bytes = Vec::new();
        File::open(&path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[12..16], b"JUNK");
        assert_eq!(&bytes[72..76], b"data");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rf64_promotion_layout_uses_ds64_without_moving_audio() {
        let path = temp_path("rf64_layout");
        let mut writer = StreamingWaveWriter::create_float32(&path, 2, 48_000).unwrap();
        // Header promotion is tested without writing 4 GiB. The writer's
        // counters are the exact boundary decision used by real recordings.
        writer.data_bytes = u32::MAX as u64 + 1;
        writer.frames = writer.data_bytes / 8;
        writer.writer.get_mut().set_len(DATA_OFFSET).unwrap();
        let state = writer.checkpoint().unwrap();
        assert_eq!(state.container, WaveContainer::Rf64);
        let mut header = [0u8; DATA_OFFSET as usize];
        File::open(&path).unwrap().read_exact(&mut header).unwrap();
        assert_eq!(&header[0..4], b"RF64");
        assert_eq!(&header[12..16], b"ds64");
        assert_eq!(
            u64::from_le_bytes(header[36..44].try_into().unwrap()),
            state.frames
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn six_hour_stereo_counting_checkpoint_keeps_64_bit_frame_count() {
        let path = temp_path("six_hour_count");
        let mut writer = StreamingWaveWriter::create_float32(&path, 2, 48_000).unwrap();
        let six_hour_frames = 6u64 * 60 * 60 * 48_000;
        writer.frames = six_hour_frames;
        writer.data_bytes = six_hour_frames * 2 * 4;
        writer.writer.get_mut().set_len(DATA_OFFSET).unwrap();
        let state = writer.checkpoint().unwrap();
        assert_eq!(state.frames, six_hour_frames);
        assert_eq!(state.data_bytes, six_hour_frames * 8);
        assert_eq!(state.container, WaveContainer::Rf64);
        let mut header = [0u8; DATA_OFFSET as usize];
        File::open(&path).unwrap().read_exact(&mut header).unwrap();
        assert_eq!(
            u64::from_le_bytes(header[36..44].try_into().unwrap()),
            six_hour_frames
        );
        let _ = std::fs::remove_file(path);
    }
}
