//! ADM BWF: ITU-R BS.2076 object metadata in a RIFF / RF64 / BW64 WAVE --
//! the interchange format of object-based masters.
//!
//! Reading only: chunk discovery past `data` (`chunks`), the `chna` track
//! table (`chna`), the `axml` document (`xml`) with the BS.2094 common
//! definitions it may lean on (`common_defs`), and an export that writes a
//! new file with edited positions (`export`). Everything here blocks on
//! file I/O and belongs on a worker. See `docs/SPATIAL_AUDIO_SPEC.md`.

pub mod chna;
pub mod chunks;
pub mod common_defs;
pub mod export;
pub mod xml;

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::{anyhow, Context, Result};

use crate::spatial::scene::{ObjectScene, SourceShape};
use crate::spatial::ObjectAudioSummary;

/// `chna` is a few hundred bytes per hundred tracks; anything larger than
/// this is not a `chna` worth reading.
const MAX_CHNA_BYTES: u64 = 4 * 1024 * 1024;

/// What a list row needs to know: `Some` for a WAVE with both `axml` and
/// `chna`. Reads the chunk headers and `chna` only -- never `axml`.
pub fn probe_summary(path: &Path) -> Result<Option<ObjectAudioSummary>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let Some(list) = chunks::walk(&mut reader, file_len)? else {
        return Ok(None);
    };
    let (Some(chna_loc), Some(_)) = (list.find(b"chna"), list.find(b"axml")) else {
        return Ok(None);
    };
    let bytes = chunks::read_payload(&mut reader, chna_loc, MAX_CHNA_BYTES)?;
    Ok(Some(chna::Chna::parse(&bytes)?.summary()))
}

/// The PCM shape of a WAVE, from its `fmt ` / `data`.
pub fn source_shape(path: &Path) -> Result<SourceShape> {
    let info = crate::wav_stream::read_wave_pcm_info(path)?
        .ok_or_else(|| anyhow!("{}: not a PCM WAVE", path.display()))?;
    Ok(SourceShape {
        tracks: info.channels as u32,
        frames: info.frame_count,
        file_sr: info.sample_rate,
    })
}

/// Read a whole ADM file's scene. Blocks for as long as the `axml` takes
/// to stream through; `progress` gets 0..1, and setting `cancel` stops it.
pub fn load_scene(
    path: &Path,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(f32),
) -> Result<ObjectScene> {
    let shape = source_shape(path)?;
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let list = chunks::walk(&mut reader, file_len)?
        .ok_or_else(|| anyhow!("{}: not a WAVE file", path.display()))?;
    let chna_loc = list
        .find(b"chna")
        .ok_or_else(|| anyhow!("{}: no chna chunk", path.display()))?;
    let axml_loc = *list
        .find(b"axml")
        .ok_or_else(|| anyhow!("{}: no axml chunk", path.display()))?;
    let chna = chna::Chna::parse(&chunks::read_payload(
        &mut reader,
        chna_loc,
        MAX_CHNA_BYTES,
    )?)?;
    let mut file = reader.into_inner();
    file.seek(SeekFrom::Start(axml_loc.payload_offset))?;
    let xml_input = BufReader::new(file.take(axml_loc.readable));
    let mut doc = xml::parse_axml(xml_input, axml_loc.readable, cancel, progress)?;
    if axml_loc.readable < axml_loc.size {
        doc.diagnostics.push(format!(
            "axml declares {} bytes but the file holds {}",
            axml_loc.size, axml_loc.readable
        ));
    }
    Ok(xml::build_scene(doc, &chna, shape))
}
