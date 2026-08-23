//! Building real mp4 files in-process, for tests.
//!
//! There is no video fixture checked into `test_samples/` and no ffmpeg to
//! make one with (the project rules out an external CLI). Everything needed to
//! synthesise one is already a dependency, though: `openh264` ships an encoder
//! alongside the decoder, `fdk-aac` encodes the audio, and the `mp4` crate
//! writes the container.
//!
//! The video track is written **first** on purpose. That is the shape that
//! broke audio decoding before this change — symphonia's `default_track()`
//! returns the first track in the file — so every fixture here reproduces it.

use std::path::Path;

use anyhow::{Context, Result};
use bytes::Bytes;
use mp4::{
    AacConfig, AudioObjectType as Mp4AudioObjectType, AvcConfig, ChannelConfig, MediaConfig,
    Mp4Config, Mp4Sample, Mp4Writer, SampleFreqIndex, TrackConfig, TrackType,
};

/// Frames per second every fixture is written at.
pub const FIXTURE_FPS: u32 = 10;
/// The video timescale, chosen so one frame is a whole number of units.
const VIDEO_TIMESCALE: u32 = 1_000;
const AUDIO_SAMPLE_RATE: u32 = 48_000;

/// Colour of frame `i`, so a test can identify which frame it got back.
///
/// Red ramps with the frame index and blue counter-ramps; green is constant.
/// Both survive a YUV round trip and a downscale well enough to identify a
/// frame within a tolerance.
pub fn frame_color(index: usize, frames: usize) -> [u8; 3] {
    let span = frames.max(2) - 1;
    let t = (index.min(span) * 255 / span) as u8;
    [t, 128, 255 - t]
}

fn solid_rgb(width: usize, height: usize, color: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 3);
    for _ in 0..width * height {
        out.extend_from_slice(&color);
    }
    out
}

/// Split an Annex-B bitstream into `(nal_type, payload)` pairs.
fn annexb_nals(stream: &[u8]) -> Vec<(u8, Vec<u8>)> {
    openh264::nal_units(stream)
        .filter_map(|unit| {
            // `nal_units` yields each NAL with its start code still attached.
            let payload = unit
                .iter()
                .position(|b| *b == 1)
                .map(|i| &unit[i + 1..])
                .unwrap_or(&[]);
            payload
                .first()
                .map(|first| (first & 0x1F, payload.to_vec()))
        })
        .collect()
}

/// A synthesised movie, before it is written out.
struct EncodedVideo {
    sps: Vec<u8>,
    pps: Vec<u8>,
    /// One AVCC-formatted sample per frame.
    samples: Vec<Vec<u8>>,
    sync: Vec<bool>,
}

fn encode_video(width: usize, height: usize, frames: usize) -> Result<EncodedVideo> {
    use openh264::encoder::Encoder;
    use openh264::formats::{RgbSliceU8, YUVBuffer};

    let mut encoder = Encoder::new().map_err(|e| anyhow::anyhow!("openh264 encoder: {e}"))?;
    let mut sps = Vec::new();
    let mut pps = Vec::new();
    let mut samples = Vec::new();
    let mut sync = Vec::new();

    for i in 0..frames {
        let rgb = solid_rgb(width, height, frame_color(i, frames));
        let yuv = YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb, (width, height)));
        let bitstream = encoder
            .encode(&yuv)
            .map_err(|e| anyhow::anyhow!("openh264 encode frame {i}: {e}"))?;
        let annexb = bitstream.to_vec();

        let mut avcc = Vec::new();
        let mut is_sync = false;
        for (nal_type, payload) in annexb_nals(&annexb) {
            match nal_type {
                // Parameter sets live in `avcC`, not in the sample data.
                7 => sps = payload,
                8 => pps = payload,
                other => {
                    if other == 5 {
                        is_sync = true;
                    }
                    avcc.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                    avcc.extend_from_slice(&payload);
                }
            }
        }
        if avcc.is_empty() {
            anyhow::bail!("encoder produced no slice data for frame {i}");
        }
        samples.push(avcc);
        sync.push(is_sync);
    }
    if sps.is_empty() || pps.is_empty() {
        anyhow::bail!("encoder produced no parameter sets");
    }
    Ok(EncodedVideo {
        sps,
        pps,
        samples,
        sync,
    })
}

/// Write an mp4 holding `frames` frames of `width` x `height` H.264, with the
/// video track first and (optionally) an AAC audio track after it.
///
/// `audio_secs` of a 440 Hz tone is written when it is above zero.
pub fn write_test_mp4(
    dst: &Path,
    width: usize,
    height: usize,
    frames: usize,
    audio_secs: f32,
) -> Result<()> {
    let video = encode_video(width, height, frames)?;
    let file =
        std::fs::File::create(dst).with_context(|| format!("create fixture: {}", dst.display()))?;
    let config = Mp4Config {
        major_brand: "isom".parse().expect("FourCC"),
        minor_version: 512,
        compatible_brands: vec![
            "isom".parse().expect("FourCC"),
            "iso2".parse().expect("FourCC"),
            "avc1".parse().expect("FourCC"),
            "mp41".parse().expect("FourCC"),
        ],
        timescale: VIDEO_TIMESCALE,
    };
    let mut writer =
        Mp4Writer::write_start(file, &config).map_err(|e| anyhow::anyhow!("mp4 start: {e:?}"))?;

    // Track 1: video. First in the file, which is the case that used to break
    // audio decoding.
    writer
        .add_track(&TrackConfig {
            track_type: TrackType::Video,
            timescale: VIDEO_TIMESCALE,
            language: "und".to_string(),
            media_conf: MediaConfig::AvcConfig(AvcConfig {
                width: width as u16,
                height: height as u16,
                seq_param_set: video.sps.clone(),
                pic_param_set: video.pps.clone(),
            }),
        })
        .map_err(|e| anyhow::anyhow!("mp4 add video track: {e:?}"))?;

    let audio_track_id = if audio_secs > 0.0 {
        writer
            .add_track(&TrackConfig {
                track_type: TrackType::Audio,
                timescale: AUDIO_SAMPLE_RATE,
                language: "und".to_string(),
                media_conf: MediaConfig::AacConfig(AacConfig {
                    bitrate: 96_000,
                    profile: Mp4AudioObjectType::AacLowComplexity,
                    freq_index: SampleFreqIndex::Freq48000,
                    chan_conf: ChannelConfig::Mono,
                }),
            })
            .map_err(|e| anyhow::anyhow!("mp4 add audio track: {e:?}"))?;
        Some(2u32)
    } else {
        None
    };

    let frame_duration = VIDEO_TIMESCALE / FIXTURE_FPS.max(1);
    for (i, sample) in video.samples.iter().enumerate() {
        writer
            .write_sample(
                1,
                &Mp4Sample {
                    start_time: i as u64 * frame_duration as u64,
                    duration: frame_duration,
                    rendering_offset: 0,
                    is_sync: video.sync[i],
                    bytes: Bytes::copy_from_slice(sample),
                },
            )
            .map_err(|e| anyhow::anyhow!("mp4 write video sample: {e:?}"))?;
    }

    if let Some(track_id) = audio_track_id {
        write_aac_tone(&mut writer, track_id, audio_secs)?;
    }

    writer
        .write_end()
        .map_err(|e| anyhow::anyhow!("mp4 write end: {e:?}"))?;
    Ok(())
}

fn write_aac_tone<W: std::io::Write + std::io::Seek>(
    writer: &mut Mp4Writer<W>,
    track_id: u32,
    secs: f32,
) -> Result<()> {
    use fdk_aac::enc::{
        AudioObjectType, BitRate, ChannelMode, Encoder as AacEncoder, EncoderParams, Transport,
    };

    let total = (AUDIO_SAMPLE_RATE as f32 * secs).max(1.0) as usize;
    let pcm: Vec<i16> = (0..total)
        .map(|i| {
            let t = i as f32 / AUDIO_SAMPLE_RATE as f32;
            ((std::f32::consts::TAU * 440.0 * t).sin() * 12_000.0) as i16
        })
        .collect();

    let encoder = AacEncoder::new(EncoderParams {
        bit_rate: BitRate::Cbr(96_000),
        sample_rate: AUDIO_SAMPLE_RATE,
        transport: Transport::Raw,
        channels: ChannelMode::Mono,
        audio_object_type: AudioObjectType::Mpeg4LowComplexity,
    })
    .map_err(|e| anyhow::anyhow!("aac encoder: {e}"))?;
    let info = encoder
        .info()
        .map_err(|e| anyhow::anyhow!("aac encoder info: {e}"))?;
    let frame_len = (info.frameLength as usize).max(1);
    let max_out = (info.maxOutBufBytes as usize).max(4096);

    let mut frame_index = 0u64;
    let mut pos = 0usize;
    let emit = |writer: &mut Mp4Writer<W>, bytes: &[u8], frame_index: &mut u64| -> Result<()> {
        writer
            .write_sample(
                track_id,
                &Mp4Sample {
                    start_time: *frame_index * frame_len as u64,
                    duration: frame_len as u32,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: Bytes::copy_from_slice(bytes),
                },
            )
            .map_err(|e| anyhow::anyhow!("mp4 write audio sample: {e:?}"))?;
        *frame_index += 1;
        Ok(())
    };

    while pos < pcm.len() {
        let end = (pos + frame_len).min(pcm.len());
        let mut slice = &pcm[pos..end];
        let padded;
        if slice.len() < frame_len {
            padded = {
                let mut buf = vec![0i16; frame_len];
                buf[..slice.len()].copy_from_slice(slice);
                buf
            };
            slice = &padded;
        }
        let mut out = vec![0u8; max_out];
        let enc = encoder
            .encode(slice, &mut out)
            .map_err(|e| anyhow::anyhow!("aac encode: {e}"))?;
        if enc.output_size > 0 {
            emit(writer, &out[..enc.output_size], &mut frame_index)?;
        }
        if enc.input_consumed == 0 {
            break;
        }
        pos += enc.input_consumed;
    }
    loop {
        let mut out = vec![0u8; max_out];
        let enc = encoder
            .encode(&[], &mut out)
            .map_err(|e| anyhow::anyhow!("aac flush: {e}"))?;
        if enc.output_size == 0 {
            break;
        }
        emit(writer, &out[..enc.output_size], &mut frame_index)?;
    }
    Ok(())
}

/// A fixture file that deletes itself when the test ends.
pub struct FixtureFile {
    pub path: std::path::PathBuf,
}

impl FixtureFile {
    /// Build a fixture named after the calling test, so parallel tests do not
    /// collide on one path.
    pub fn build(tag: &str, frames: usize, audio_secs: f32) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let dir = std::env::temp_dir().join("neowaves_video_fixtures");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!(
            "{tag}_{}_{}.mp4",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        write_test_mp4(&path, 64, 64, frames, audio_secs)?;
        Ok(Self { path })
    }
}

impl Drop for FixtureFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
