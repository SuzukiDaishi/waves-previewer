//! TrueHD streams (`.thd` / `.mlp`), behind the `truehd` feature.
//!
//! A TrueHD stream cannot be read at a random place without decoding from
//! the major sync before it, and its object presentation is up to sixteen
//! channels of 24-bit PCM with the positions alongside. So a stream is
//! decoded once, on a worker, into a 24-bit RF64 WAVE in the app's temp
//! cache; from there it plays exactly like an ADM master -- memory-mapped,
//! through the object mix -- and its metadata becomes the same kind of
//! scene (`spatial::oamd`). A presentation without objects becomes a scene
//! of bed channels, so a plain 5.1 or 7.1 stream reaches the right speakers
//! the same way.
//!
//! Decoding is the truehdd project's `truehd` crate (Apache-2.0), which
//! describes itself as a research implementation: damaged access units are
//! skipped, and what was lost is reported rather than hidden.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use truehd::process::decode::{DecodedAccessUnit, Decoder};
use truehd::process::extract::Extractor;
use truehd::process::parse::Parser;
use truehd::structs::oamd::ObjectAudioMetadataPayload;
use truehd::utils::errors::ExtractError;

use crate::spatial::oamd::{
    channel_scene, gain_from_db, OamdSceneBuilder, OamdUpdate, ObjectUpdate,
};
use crate::spatial::scene::{ObjectScene, SourceShape};
use crate::spatial::{ObjectAudioSummary, ObjectFormat};
use crate::wav_stream::StreamingWaveWriter;

/// Extensions of a bare TrueHD (or MLP) stream.
pub const TRUEHD_EXTS: &[&str] = &["thd", "mlp"];

/// The object presentation, or the widest there is (the decoder falls back
/// to the highest one present).
const WIDEST_PRESENTATION: usize = 3;
/// How much of a stream a probe reads: enough for a few hundred access
/// units, and so for the first object metadata payload.
const PROBE_BYTES: usize = 2 * 1024 * 1024;
/// How much is read from the file at a time while decoding.
const READ_CHUNK: usize = 1024 * 1024;
/// Major sync words: TrueHD, and MLP (DVD-Audio's ancestor of it).
const MAJOR_SYNC_TRUEHD: [u8; 4] = [0xF8, 0x72, 0x6F, 0xBA];
const MAJOR_SYNC_MLP: [u8; 4] = [0xF8, 0x72, 0x6F, 0xBB];

pub fn is_truehd_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            TRUEHD_EXTS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// Whether `head` holds a TrueHD / MLP major sync.
pub fn looks_like_truehd(head: &[u8]) -> bool {
    head.windows(4)
        .any(|w| w == MAJOR_SYNC_TRUEHD || w == MAJOR_SYNC_MLP)
}

/// The TrueHD bytes of `path`: the file itself, or the TrueHD stream of a
/// transport stream (`.m2ts` / `.mts`).
fn open_stream(path: &Path) -> Result<(Box<dyn Read>, u64)> {
    let file_len = std::fs::metadata(path)
        .with_context(|| format!("open {}", path.display()))?
        .len();
    if crate::mpegts::is_mpegts_path(path) {
        return Ok((
            Box::new(crate::audio_mpegts::TsTrueHdReader::open(path)?),
            file_len,
        ));
    }
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok((Box::new(BufReader::new(file)), file_len))
}

/// What the start of a stream says.
#[derive(Clone, Debug, PartialEq)]
pub struct TrueHdProbe {
    pub sample_rate: u32,
    pub channels: usize,
    pub beds: usize,
    pub objects: usize,
    /// From the bytes a decoded stretch took: TrueHD is variable rate, so
    /// this is an estimate until the stream has been decoded.
    pub estimated_frames: Option<u64>,
}

impl TrueHdProbe {
    pub fn summary(&self) -> ObjectAudioSummary {
        let beds = if self.objects == 0 {
            self.channels
        } else {
            self.beds
        };
        ObjectAudioSummary::new(
            ObjectFormat::TrueHd,
            self.channels as u32,
            beds as u32,
            self.objects as u32,
            0,
        )
    }
}

fn update_from(payload: &ObjectAudioMetadataPayload, samples_so_far: u64) -> Option<OamdUpdate> {
    let element = payload.object_element.as_ref()?;
    let positions = payload.get_damf_pos();
    let objects = element
        .object_data
        .iter()
        .enumerate()
        .take(payload.object_count)
        .filter_map(|(index, blocks)| {
            // Only the first block of a payload, as truehdd does.
            let block = blocks.first()?;
            let pos = positions
                .get(index)
                .and_then(|p| p.first())
                .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
                .unwrap_or([0.0, 1.0, 0.0]);
            Some(ObjectUpdate {
                in_bed: block.b_object_in_bed_or_isf,
                active: !block.b_object_not_active,
                gain: gain_from_db(block.object_basic_info.object_gain),
                pos,
            })
        })
        .collect();
    Some(OamdUpdate {
        sample_pos: samples_so_far
            + element.md_update_info.sample_offset as u64
            + payload.evo_sample_offset,
        ramp: element
            .md_update_info
            .block_update_info
            .first()
            .map(|block| block.ramp_duration as u64)
            .unwrap_or(0),
        bed: payload
            .program_assignment
            .bed_assignment
            .first()
            .map(|bed| bed.to_index_vec())
            .unwrap_or_default(),
        objects,
    })
}

/// The decode loop: every access unit of `input` handed to `on_unit` with
/// the count of samples before it. Damaged units are skipped and counted.
fn decode_stream<R: Read>(
    mut input: R,
    max_bytes: Option<usize>,
    cancel: Option<&AtomicBool>,
    mut on_progress: impl FnMut(u64),
    mut on_unit: impl FnMut(&DecodedAccessUnit) -> Result<bool>,
) -> Result<(u64, u32)> {
    let mut extractor = Extractor::default();
    let mut parser = Parser::default();
    let mut required = [false; truehd::process::MAX_PRESENTATIONS];
    required[WIDEST_PRESENTATION] = true;
    parser.set_required_presentations(&required);
    let mut decoder = Decoder::default();
    let mut chunk = vec![0u8; READ_CHUNK];
    let mut read_total = 0u64;
    let mut lost = 0u32;
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            bail!("cancelled");
        }
        let want = match max_bytes {
            Some(max) if read_total as usize >= max => break,
            Some(max) => (max - read_total as usize).min(chunk.len()),
            None => chunk.len(),
        };
        let read = input.read(&mut chunk[..want])?;
        if read == 0 {
            break;
        }
        read_total += read as u64;
        on_progress(read_total);
        extractor.push_bytes(&chunk[..read]);
        for frame in extractor.by_ref() {
            let frame = match frame {
                Ok(frame) => frame,
                Err(ExtractError::InsufficientData) => break,
                // The extractor resyncs on its own; the unit is lost.
                Err(_) => {
                    lost += 1;
                    continue;
                }
            };
            let unit = match parser.parse(&frame) {
                Ok(unit) => unit,
                Err(_) => {
                    lost += 1;
                    parser.reset_for_next_major_sync();
                    decoder.reset_for_next_major_sync();
                    continue;
                }
            };
            match decoder.decode_presentation(&unit, WIDEST_PRESENTATION) {
                Ok(decoded) => {
                    if !on_unit(&decoded)? {
                        return Ok((read_total, lost));
                    }
                }
                Err(_) => {
                    lost += 1;
                    parser.reset_for_next_major_sync();
                    decoder.reset_for_next_major_sync();
                }
            }
        }
    }
    Ok((read_total, lost))
}

/// Read the start of a stream: its rate, channels and whether it carries
/// objects. Blocks; a worker's job.
pub fn probe(path: &Path) -> Result<TrueHdProbe> {
    let (input, file_len) = open_stream(path)?;
    let mut found: Option<TrueHdProbe> = None;
    let mut samples = 0u64;
    let (bytes, _) = decode_stream(
        input,
        Some(PROBE_BYTES),
        None,
        |_| {},
        |unit| {
            if unit.is_duplicate {
                return Ok(true);
            }
            let probe = found.get_or_insert_with(|| TrueHdProbe {
                sample_rate: unit.sampling_frequency,
                channels: unit.channel_count,
                beds: 0,
                objects: 0,
                estimated_frames: None,
            });
            if let Some(payload) = unit.oamd.first() {
                let beds = payload
                    .program_assignment
                    .bed_assignment
                    .first()
                    .map(|bed| bed.to_index_vec().len())
                    .unwrap_or(0);
                probe.beds = beds;
                probe.objects = payload.object_count.saturating_sub(beds);
            }
            samples += unit.sample_length as u64;
            // Enough once the objects (if any) have shown themselves.
            Ok(probe.objects == 0 || samples < u64::from(unit.sampling_frequency))
        },
    )?;
    let mut probe = found.with_context(|| format!("{}: no TrueHD audio found", path.display()))?;
    if bytes > 0 && samples > 0 {
        probe.estimated_frames = Some((file_len as f64 * samples as f64 / bytes as f64) as u64);
    }
    Ok(probe)
}

/// Decode the whole of `path` into a 24-bit WAVE at `dest`, and return its
/// scene. Blocks for as long as the stream takes; `progress` gets 0..1.
pub fn decode_to_wave(
    path: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<ObjectScene> {
    let (input, file_len) = open_stream(path)?;
    let file_len = file_len.max(1);
    let mut writer: Option<StreamingWaveWriter> = None;
    let mut channels = 0usize;
    let mut sample_rate = 0u32;
    let mut labels: Vec<usize> = Vec::new();
    let mut builder = OamdSceneBuilder::default();
    let mut samples = 0u64;
    let mut interleaved: Vec<i32> = Vec::with_capacity(160 * 16);
    let (_, lost) = decode_stream(
        input,
        None,
        Some(cancel),
        |read| progress((read as f64 / file_len as f64).min(1.0) as f32 * 0.99),
        |unit| {
            if unit.is_duplicate {
                return Ok(true);
            }
            if writer.is_none() {
                channels = unit.channel_count.clamp(1, 16);
                sample_rate = unit.sampling_frequency;
                labels = unit
                    .channel_labels
                    .iter()
                    .map(|label| *label as usize)
                    .collect();
                writer = Some(StreamingWaveWriter::create_pcm24(
                    dest,
                    channels as u16,
                    sample_rate,
                )?);
            } else if unit.channel_count.clamp(1, 16) != channels
                || unit.sampling_frequency != sample_rate
            {
                bail!(
                    "the stream changes from {channels} channels at {sample_rate} Hz to {} at {} Hz part way through",
                    unit.channel_count,
                    unit.sampling_frequency
                );
            }
            for payload in &unit.oamd {
                if let Some(update) = update_from(payload, samples) {
                    builder.push(&update, sample_rate);
                }
            }
            interleaved.clear();
            for row in unit.pcm_data.iter().take(unit.sample_length) {
                interleaved.extend_from_slice(&row[..channels]);
            }
            writer
                .as_mut()
                .expect("made on the first unit")
                .write_interleaved_i24(&interleaved)?;
            samples += unit.sample_length as u64;
            Ok(true)
        },
    )?;
    let writer = writer.with_context(|| format!("{}: no TrueHD audio found", path.display()))?;
    writer.finalize()?;
    let shape = SourceShape {
        tracks: channels as u32,
        frames: samples,
        file_sr: sample_rate,
    };
    let mut scene = if builder.has_objects() {
        builder.finish(shape)
    } else {
        channel_scene(&labels, shape)
    };
    if lost > 0 {
        scene
            .diagnostics
            .push(format!("{lost} damaged access unit(s) were skipped"));
    }
    progress(1.0);
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn major_syncs_are_recognised_and_extensions_named() {
        let mut head = vec![0u8; 16];
        head[4..8].copy_from_slice(&MAJOR_SYNC_TRUEHD);
        assert!(looks_like_truehd(&head));
        assert!(!looks_like_truehd(b"RIFF\0\0\0\0WAVE"));
        assert!(is_truehd_path(Path::new("film.THD")));
        assert!(is_truehd_path(Path::new("dvd.mlp")));
        assert!(!is_truehd_path(Path::new("song.wav")));
    }

    /// A real stream, when one is at hand: `NEOWAVES_TRUEHD_SAMPLE` names a
    /// `.thd`; with an object presentation, its objects must have moved.
    #[test]
    fn a_sample_stream_decodes_when_one_is_given() {
        let Ok(sample) = std::env::var("NEOWAVES_TRUEHD_SAMPLE") else {
            return;
        };
        let path = Path::new(&sample);
        let probe = probe(path).expect("probe");
        assert!(probe.channels > 0 && probe.sample_rate > 0, "{probe:?}");
        let dest =
            std::env::temp_dir().join(format!("neowaves_truehd_test_{}.wav", std::process::id()));
        let scene =
            decode_to_wave(path, &dest, &AtomicBool::new(false), &mut |_| {}).expect("decode");
        let info = crate::wav_stream::read_wave_pcm_info(&dest)
            .unwrap()
            .unwrap();
        let _ = std::fs::remove_file(&dest);
        assert_eq!(info.channels as u32, scene.shape.tracks);
        assert_eq!(info.frame_count, scene.shape.frames);
        if probe.objects > 0 {
            assert!(scene.object_count() > 0, "{:?}", scene.diagnostics);
        }
    }
}
