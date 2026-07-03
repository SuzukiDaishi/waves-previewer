//! Minimal FLAC metadata-block reader/writer.
//!
//! Only the metadata section (everything between the `fLaC` magic and the
//! first audio frame) is touched; audio frames are copied through verbatim.
//! Loop markers use the same `LOOPSTART` / `LOOPEND` convention as the MP3
//! (TXXX) and M4A (freeform) paths, stored as Vorbis comments.

use std::path::Path;

use anyhow::{Context, Result};

const LOOPSTART_KEY: &str = "LOOPSTART";
const LOOPEND_KEY: &str = "LOOPEND";

const BLOCK_STREAMINFO: u8 = 0;
const BLOCK_VORBIS_COMMENT: u8 = 4;
const BLOCK_PICTURE: u8 = 6;

struct FlacBlock {
    block_type: u8,
    payload: Vec<u8>,
}

struct FlacFile {
    blocks: Vec<FlacBlock>,
    /// Audio frames (and anything after the last metadata block), verbatim.
    frames: Vec<u8>,
}

fn parse_flac(path: &Path) -> Result<FlacFile> {
    let data = std::fs::read(path).with_context(|| format!("read flac: {}", path.display()))?;
    if data.len() < 8 || &data[0..4] != b"fLaC" {
        anyhow::bail!("not a FLAC file: {}", path.display());
    }
    let mut blocks = Vec::new();
    let mut pos = 4usize;
    loop {
        if pos + 4 > data.len() {
            anyhow::bail!("truncated FLAC metadata: {}", path.display());
        }
        let header = data[pos];
        let is_last = header & 0x80 != 0;
        let block_type = header & 0x7F;
        let size =
            u32::from_be_bytes([0, data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        let start = pos + 4;
        let end = start.saturating_add(size);
        if end > data.len() {
            anyhow::bail!("truncated FLAC metadata block: {}", path.display());
        }
        blocks.push(FlacBlock {
            block_type,
            payload: data[start..end].to_vec(),
        });
        pos = end;
        if is_last {
            break;
        }
    }
    Ok(FlacFile {
        blocks,
        frames: data[pos..].to_vec(),
    })
}

fn encode_flac_file(path: &Path, file: &FlacFile) -> Result<()> {
    let mut out = Vec::with_capacity(file.frames.len() + 1024);
    out.extend_from_slice(b"fLaC");
    let last_index = file.blocks.len().saturating_sub(1);
    for (i, block) in file.blocks.iter().enumerate() {
        let mut header = block.block_type & 0x7F;
        if i == last_index {
            header |= 0x80;
        }
        out.push(header);
        let size = (block.payload.len() as u32).min(0x00FF_FFFF);
        out.extend_from_slice(&size.to_be_bytes()[1..4]);
        out.extend_from_slice(&block.payload[..size as usize]);
    }
    out.extend_from_slice(&file.frames);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(
        ".wvp_tmp_flacmeta_{}_{}.flac",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, out).with_context(|| format!("write flac tmp: {}", tmp.display()))?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Windows: rename fails while the target exists; replace via copy.
            let res = std::fs::copy(&tmp, path)
                .map(|_| ())
                .with_context(|| format!("replace flac: {}", path.display()));
            let _ = std::fs::remove_file(&tmp);
            res
        }
    }
}

fn parse_vorbis_comments(payload: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let read_u32 = |data: &[u8], pos: usize| -> Option<u32> {
        data.get(pos..pos + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let Some(vendor_len) = read_u32(payload, 0) else {
        return out;
    };
    let mut pos = 4 + vendor_len as usize;
    let Some(count) = read_u32(payload, pos) else {
        return out;
    };
    pos += 4;
    for _ in 0..count {
        let Some(len) = read_u32(payload, pos) else {
            break;
        };
        pos += 4;
        let end = pos.saturating_add(len as usize);
        let Some(bytes) = payload.get(pos..end) else {
            break;
        };
        if let Ok(text) = std::str::from_utf8(bytes) {
            if let Some((key, value)) = text.split_once('=') {
                out.push((key.to_string(), value.to_string()));
            }
        }
        pos = end;
    }
    out
}

fn build_vorbis_comment_payload(vendor: &str, comments: &[(String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor.as_bytes());
    out.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for (key, value) in comments {
        let entry = format!("{key}={value}");
        out.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        out.extend_from_slice(entry.as_bytes());
    }
    out
}

fn vendor_string(payload: &[u8]) -> String {
    let len = payload
        .get(0..4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        .unwrap_or(0);
    payload
        .get(4..4 + len)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
        .to_string()
}

/// Read all Vorbis comments (KEY=value pairs) from a FLAC file.
pub fn read_flac_vorbis_comments(path: &Path) -> Result<Vec<(String, String)>> {
    let file = parse_flac(path)?;
    Ok(file
        .blocks
        .iter()
        .filter(|b| b.block_type == BLOCK_VORBIS_COMMENT)
        .flat_map(|b| parse_vorbis_comments(&b.payload))
        .collect())
}

/// Upsert (`Some`) or remove (`None`) Vorbis comment keys, preserving all
/// other comments and metadata blocks. Keys compare case-insensitively per
/// the Vorbis comment spec.
pub fn update_flac_vorbis_comments(path: &Path, changes: &[(&str, Option<String>)]) -> Result<()> {
    let mut file = parse_flac(path)?;
    let existing = file
        .blocks
        .iter()
        .find(|b| b.block_type == BLOCK_VORBIS_COMMENT);
    let vendor = existing
        .map(|b| vendor_string(&b.payload))
        .unwrap_or_default();
    let mut comments: Vec<(String, String)> = existing
        .map(|b| parse_vorbis_comments(&b.payload))
        .unwrap_or_default();
    for (key, value) in changes {
        comments.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
        if let Some(value) = value {
            comments.push((key.to_string(), value.clone()));
        }
    }
    let payload = build_vorbis_comment_payload(&vendor, &comments);
    if let Some(block) = file
        .blocks
        .iter_mut()
        .find(|b| b.block_type == BLOCK_VORBIS_COMMENT)
    {
        block.payload = payload;
    } else {
        // STREAMINFO must stay first; append the comment block after it.
        file.blocks.push(FlacBlock {
            block_type: BLOCK_VORBIS_COMMENT,
            payload,
        });
    }
    encode_flac_file(path, &file)
}

pub fn read_flac_loop_markers(path: &Path) -> Result<Option<(u64, u64)>> {
    let comments = read_flac_vorbis_comments(path)?;
    let find = |key: &str| {
        comments
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| v.trim().parse::<u64>().ok())
    };
    Ok(match (find(LOOPSTART_KEY), find(LOOPEND_KEY)) {
        (Some(s), Some(e)) if e > s => Some((s, e)),
        _ => None,
    })
}

pub fn write_flac_loop_markers(path: &Path, loop_opt: Option<(u64, u64)>) -> Result<()> {
    let (start, end) = match loop_opt.filter(|(s, e)| e > s) {
        Some((s, e)) => (Some(s.to_string()), Some(e.to_string())),
        None => (None, None),
    };
    update_flac_vorbis_comments(path, &[(LOOPSTART_KEY, start), (LOOPEND_KEY, end)])
}

pub fn read_flac_bpm(path: &Path) -> Option<f32> {
    let comments = read_flac_vorbis_comments(path).ok()?;
    comments
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("BPM") || k.eq_ignore_ascii_case("TEMPO"))
        .and_then(|(_, v)| crate::audio_io::parse_bpm_text(v))
}

/// Extract the image bytes of the first PICTURE block (cover art).
pub fn read_flac_artwork(path: &Path) -> Option<Vec<u8>> {
    let file = parse_flac(path).ok()?;
    let block = file.blocks.iter().find(|b| b.block_type == BLOCK_PICTURE)?;
    let payload = &block.payload;
    let read_u32 = |pos: usize| -> Option<u32> {
        payload
            .get(pos..pos + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let mut pos = 4; // skip picture type
    let mime_len = read_u32(pos)? as usize;
    pos += 4 + mime_len;
    let desc_len = read_u32(pos)? as usize;
    pos += 4 + desc_len;
    pos += 16; // width, height, depth, colors
    let data_len = read_u32(pos)? as usize;
    pos += 4;
    payload.get(pos..pos + data_len).map(|b| b.to_vec())
}

/// Carry Vorbis comments and PICTURE blocks from `src` into `dst` (both FLAC).
/// STREAMINFO / SEEKTABLE / CUESHEET are not copied: they describe the source
/// audio stream and would be invalid for freshly encoded audio.
pub fn copy_flac_metadata_from_source(src: &Path, dst: &Path) -> Result<()> {
    let src_file = parse_flac(src)?;
    let mut dst_file = parse_flac(dst)?;
    let carried: Vec<FlacBlock> = src_file
        .blocks
        .into_iter()
        .filter(|b| matches!(b.block_type, BLOCK_VORBIS_COMMENT | BLOCK_PICTURE))
        .collect();
    if carried.is_empty() {
        return Ok(());
    }
    dst_file
        .blocks
        .retain(|b| !matches!(b.block_type, BLOCK_VORBIS_COMMENT | BLOCK_PICTURE));
    // Keep STREAMINFO first (required by the spec), then the carried blocks.
    let insert_at = if dst_file
        .blocks
        .first()
        .map(|b| b.block_type == BLOCK_STREAMINFO)
        .unwrap_or(false)
    {
        1
    } else {
        0
    };
    for (offset, block) in carried.into_iter().enumerate() {
        dst_file.blocks.insert(insert_at + offset, block);
    }
    encode_flac_file(dst, &dst_file)
}
