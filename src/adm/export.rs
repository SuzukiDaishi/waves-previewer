//! Writing a new ADM BWF with the session's spatial edits in it.
//!
//! The source is never touched. The new file has the source's chunks in the
//! source's order: the audio copied byte for byte (no decode, so it is
//! bit-exact), `chna` and every other chunk (`bext`, `dbmd`, `iXML`, `LIST`,
//! ones the app has never heard of) verbatim, and `axml` copied event by
//! event with one change -- the `audioBlockFormat`s of each edited
//! element's channel format are written from its keyframes, in the
//! element's own coordinates, with the children the app does not interpret
//! (`extras`) put back. A file with no edits comes out identical to its
//! source.
//!
//! Blocking file I/O from start to end: a worker's job, with progress and a
//! cancel flag.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use quick_xml::Writer;

use super::chunks::{self, ChunkList, ChunkLoc};
use super::xml::{format_time, norm_id};
use crate::spatial::scene::{Coords, Element, Keyframe, ObjectScene, SceneEdits};

/// The audio is copied in blocks of this size.
const COPY_BLOCK: usize = 4 * 1024 * 1024;

/// The channel format an element's key names: `adm:<object>:<channel>`.
fn channel_of_key(key: &str) -> Option<&str> {
    key.strip_prefix("adm:")?.rsplit(':').next()
}

fn number(value: f32) -> String {
    let text = format!("{value:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-" || text == "-0" {
        "0".to_string()
    } else {
        text.to_string()
    }
}

/// `audioBlockFormat` elements for `keys`, in `element`'s coordinates.
///
/// Block `i` starts where keyframe `i`'s ramp starts and lasts until the
/// next block starts (the last one to the object's end, or the file's), and
/// glides to its position over the ramp. The first block starts at the
/// object's start whatever the first keyframe says: a keyframe's value holds
/// before it, so that is exactly what the time before it sounds like.
pub fn block_formats(
    element: &Element,
    keys: &[Keyframe],
    channel_id: &str,
    file_secs: f64,
) -> String {
    let object_start = element.active.map(|(start, _)| start).unwrap_or(0.0);
    let object_end = element
        .active
        .map(|(_, end)| end)
        .filter(|end| end.is_finite())
        .unwrap_or(file_secs)
        .max(object_start);
    let digits = channel_id.get(3..11).unwrap_or("00031001");
    let starts: Vec<f64> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            if i == 0 {
                object_start
            } else {
                (key.secs - key.ramp_secs).max(object_start)
            }
        })
        .collect();
    let mut out = String::new();
    for (i, key) in keys.iter().enumerate() {
        let start = starts[i];
        let end = starts.get(i + 1).copied().unwrap_or(object_end).max(start);
        let duration = end - start;
        let ramp = if i == 0 {
            0.0
        } else {
            key.ramp_secs.min(duration)
        };
        out.push_str(&format!(
            "<audioBlockFormat audioBlockFormatID=\"AB_{digits}_{:08x}\" rtime=\"{}\" duration=\"{}\">",
            i + 1,
            format_time(start - object_start),
            format_time(duration)
        ));
        match element.coords {
            Coords::Cartesian => {
                out.push_str("<cartesian>1</cartesian>");
                for (axis, value) in ["X", "Y", "Z"].iter().zip(key.pos) {
                    out.push_str(&format!(
                        "<position coordinate=\"{axis}\">{}</position>",
                        number(value)
                    ));
                }
            }
            Coords::Polar => {
                for (axis, value) in ["azimuth", "elevation", "distance"].iter().zip(key.pos) {
                    out.push_str(&format!(
                        "<position coordinate=\"{axis}\">{}</position>",
                        number(value)
                    ));
                }
            }
        }
        if key.gain != 1.0 {
            out.push_str(&format!("<gain>{}</gain>", number(key.gain)));
        }
        // A glide that fills the block is jumpPosition 0, the default.
        if (ramp - duration).abs() > 1e-9 {
            if ramp > 0.0 {
                out.push_str(&format!(
                    "<jumpPosition interpolationLength=\"{}\">1</jumpPosition>",
                    number(ramp as f32)
                ));
            } else {
                out.push_str("<jumpPosition>1</jumpPosition>");
            }
        }
        if let Some(extras) = &key.extras {
            out.push_str(extras);
        }
        out.push_str("</audioBlockFormat>");
    }
    out
}

/// The `axml` document with the blocks of every channel in `replace` (by
/// upper-cased channel format ID) written anew.
pub fn patch_axml(original: &[u8], replace: &HashMap<String, String>) -> Result<Vec<u8>> {
    if replace.is_empty() {
        return Ok(original.to_vec());
    }
    let mut reader = Reader::from_reader(original);
    let mut writer = Writer::new(Vec::with_capacity(original.len() + 1024));
    let mut buf = Vec::new();
    let mut depth = 0usize;
    // (depth of the channel element, its new blocks) while inside one.
    let mut replacing: Option<(usize, &String)> = None;
    // Depth of a block being skipped.
    let mut skipping: Option<usize> = None;
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .with_context(|| format!("axml: XML error at byte {}", reader.buffer_position()))?;
        match &event {
            Event::Eof => break,
            Event::Start(e) => {
                depth += 1;
                let name = e.local_name();
                if skipping.is_some() {
                    buf.clear();
                    continue;
                }
                if let Some((channel_depth, _)) = replacing {
                    if depth == channel_depth + 1 && name.as_ref() == b"audioBlockFormat" {
                        skipping = Some(depth);
                        buf.clear();
                        continue;
                    }
                } else if name.as_ref() == b"audioChannelFormat" {
                    let id = channel_id_attr(e);
                    if let Some(blocks) = id.and_then(|id| replace.get(&id)) {
                        replacing = Some((depth, blocks));
                    }
                }
            }
            Event::Empty(e) => {
                if skipping.is_some() {
                    buf.clear();
                    continue;
                }
                if let Some((channel_depth, _)) = replacing {
                    if depth == channel_depth && e.local_name().as_ref() == b"audioBlockFormat" {
                        buf.clear();
                        continue;
                    }
                }
            }
            Event::End(e) => {
                let closing = depth;
                depth = depth.saturating_sub(1);
                if let Some(skip_depth) = skipping {
                    if closing == skip_depth {
                        skipping = None;
                    }
                    buf.clear();
                    continue;
                }
                if let Some((channel_depth, blocks)) = replacing {
                    if closing == channel_depth && e.local_name().as_ref() == b"audioChannelFormat"
                    {
                        writer.get_mut().extend_from_slice(blocks.as_bytes());
                        replacing = None;
                    }
                }
            }
            _ => {
                if skipping.is_some() {
                    buf.clear();
                    continue;
                }
            }
        }
        writer.write_event(event.borrow())?;
        buf.clear();
    }
    Ok(writer.into_inner())
}

fn channel_id_attr(e: &BytesStart<'_>) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.local_name().as_ref() == b"audioChannelFormatID")
        .map(|a| norm_id(&String::from_utf8_lossy(&a.value)))
}

/// The new `axml` for `scene` with `edits`, read from `source`.
pub fn edited_axml<R: Read + Seek>(
    source: &mut R,
    list: &ChunkList,
    scene: &ObjectScene,
    edits: &SceneEdits,
) -> Result<Vec<u8>> {
    let axml = list.find(b"axml").context("the source has no axml chunk")?;
    let original = chunks::read_payload(source, axml, u64::MAX)?;
    let mut replace: HashMap<String, String> = HashMap::new();
    for element in &scene.elements {
        let Some(keys) = edits.elements.get(&element.key) else {
            continue;
        };
        let Some(channel) = channel_of_key(&element.key) else {
            continue;
        };
        let channel = norm_id(channel);
        let blocks = block_formats(element, keys, &channel, scene.shape.duration_secs());
        replace.entry(channel).or_insert(blocks);
    }
    patch_axml(&original, &replace)
}

/// Where the export is going, written as `dest.partial` and renamed when
/// complete, so a cancelled or failed export never leaves half a file under
/// the name asked for.
fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".partial");
    dest.with_file_name(name)
}

fn write_chunk_header<W: Write>(out: &mut W, id: &[u8; 4], size: u64, wide: bool) -> Result<()> {
    out.write_all(id)?;
    let field = if wide && (id == b"data" || size >= u32::MAX as u64) {
        u32::MAX
    } else {
        u32::try_from(size).context("a chunk too large for RIFF")?
    };
    out.write_all(&field.to_le_bytes())?;
    Ok(())
}

/// Export `source` to `dest` with `edits` applied. `progress` gets 0..1.
pub fn export_adm(
    source: &Path,
    dest: &Path,
    scene: &ObjectScene,
    edits: Option<&SceneEdits>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<()> {
    let same = match (source.canonicalize(), dest.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => source == dest,
    };
    if same {
        bail!("the export would overwrite its own source; choose another file");
    }
    let file = File::open(source).with_context(|| format!("open {}", source.display()))?;
    let file_len = file.metadata()?.len();
    let mut input = BufReader::new(file);
    let list = chunks::walk(&mut input, file_len)?.context("the source is not a WAVE file")?;
    let empty = SceneEdits::default();
    let edits = edits.filter(|e| e.fits(scene)).unwrap_or(&empty);
    let axml = edited_axml(&mut input, &list, scene, edits)?;

    // Sizes first: the header and ds64 need them before anything is written.
    let payload_len = |chunk: &ChunkLoc| -> u64 {
        if &chunk.id == b"axml" {
            axml.len() as u64
        } else {
            chunk.readable
        }
    };
    let wide = list.is_64bit();
    let body: u64 = 4 + list
        .chunks
        .iter()
        .map(|c| {
            let len = payload_len(c);
            8 + len + (len & 1)
        })
        .sum::<u64>();
    if !wide && body > u32::MAX as u64 {
        bail!("the result is larger than a RIFF file can hold");
    }
    let data_len = list.find(b"data").map(|c| c.readable).unwrap_or(0);
    let info = crate::wav_stream::read_wave_pcm_info(source)?;
    let block_align = info.map(|i| i.block_align.max(1) as u64).unwrap_or(1);

    let partial = partial_path(dest);
    let out_file =
        File::create(&partial).with_context(|| format!("create {}", partial.display()))?;
    let mut out = BufWriter::with_capacity(COPY_BLOCK, out_file);
    let result = (|| -> Result<()> {
        out.write_all(&list.root)?;
        out.write_all(&(if wide { u32::MAX } else { body as u32 }).to_le_bytes())?;
        out.write_all(b"WAVE")?;
        let total = list.chunks.iter().map(payload_len).sum::<u64>().max(1);
        let mut written = 0u64;
        let mut block = vec![0u8; COPY_BLOCK];
        for chunk in &list.chunks {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            let len = payload_len(chunk);
            match &chunk.id {
                b"ds64" => {
                    // riffSize, dataSize, sampleCount, then a table of every
                    // other chunk too large for its 32-bit size field.
                    let big: Vec<(&[u8; 4], u64)> = list
                        .chunks
                        .iter()
                        .filter(|c| &c.id != b"data" && &c.id != b"ds64")
                        .map(|c| (&c.id, payload_len(c)))
                        .filter(|(_, len)| *len >= u32::MAX as u64)
                        .collect();
                    let mut payload = Vec::with_capacity(28 + big.len() * 12);
                    payload.extend_from_slice(&(body).to_le_bytes());
                    payload.extend_from_slice(&data_len.to_le_bytes());
                    payload.extend_from_slice(&(data_len / block_align).to_le_bytes());
                    payload.extend_from_slice(&(big.len() as u32).to_le_bytes());
                    for (id, len) in big {
                        payload.extend_from_slice(id);
                        payload.extend_from_slice(&len.to_le_bytes());
                    }
                    // Keep the source's ds64 length (writers leave room in
                    // it); pad what the table does not fill.
                    let room = (chunk.readable as usize).max(payload.len());
                    payload.resize(room, 0);
                    // The body was sized with the source's ds64 length.
                    if room as u64 != chunk.readable {
                        bail!("the source's ds64 has no room for the new size table");
                    }
                    write_chunk_header(&mut out, b"ds64", room as u64, false)?;
                    out.write_all(&payload)?;
                }
                b"axml" => {
                    write_chunk_header(&mut out, b"axml", len, wide)?;
                    out.write_all(&axml)?;
                }
                id => {
                    write_chunk_header(&mut out, id, len, wide)?;
                    input.seek(SeekFrom::Start(chunk.payload_offset))?;
                    let mut left = len;
                    while left > 0 {
                        if cancel.load(Ordering::Relaxed) {
                            bail!("cancelled");
                        }
                        let take = (left as usize).min(COPY_BLOCK);
                        input.read_exact(&mut block[..take])?;
                        out.write_all(&block[..take])?;
                        left -= take as u64;
                        written += take as u64;
                        progress((written as f64 / total as f64).min(1.0) as f32);
                    }
                }
            }
            if len & 1 == 1 {
                out.write_all(&[0])?;
            }
        }
        out.flush()?;
        out.get_ref().sync_all()?;
        Ok(())
    })();
    drop(out);
    if let Err(err) = result {
        let _ = std::fs::remove_file(&partial);
        return Err(err);
    }
    if dest.exists() {
        std::fs::remove_file(dest).with_context(|| format!("replace {}", dest.display()))?;
    }
    std::fs::rename(&partial, dest)
        .with_context(|| format!("rename {} to {}", partial.display(), dest.display()))?;
    progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_short() {
        assert_eq!(number(1.0), "1");
        assert_eq!(number(-0.5), "-0.5");
        assert_eq!(number(0.0), "0");
        assert_eq!(number(0.123456789), "0.123457");
    }

    #[test]
    fn only_the_named_channels_blocks_are_replaced() {
        let xml = br#"<a><audioChannelFormat audioChannelFormatID="AC_00031001"><audioBlockFormat audioBlockFormatID="AB_1"><x>1</x></audioBlockFormat><frequency typeDefinition="lowPass">120</frequency></audioChannelFormat><audioChannelFormat audioChannelFormatID="AC_00031002"><audioBlockFormat audioBlockFormatID="AB_2"/></audioChannelFormat></a>"#;
        let mut replace = HashMap::new();
        replace.insert(
            "AC_00031001".to_string(),
            "<audioBlockFormat audioBlockFormatID=\"NEW\"/>".to_string(),
        );
        let out = String::from_utf8(patch_axml(xml, &replace).unwrap()).unwrap();
        assert!(out.contains("NEW"), "{out}");
        assert!(!out.contains("AB_1"), "the old block is gone: {out}");
        assert!(out.contains("<frequency"), "other children stay: {out}");
        assert!(
            out.contains("AB_2"),
            "the other channel is untouched: {out}"
        );
        assert_eq!(patch_axml(xml, &HashMap::new()).unwrap(), xml.to_vec());
    }
}
