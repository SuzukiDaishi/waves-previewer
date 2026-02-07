use std::path::Path;

use anyhow::{Context, Result};
use id3::frame::{Content, ExtendedText, Frame};
use id3::{Tag, TagLike, Version};
use mp4ameta::{Data, FreeformIdent, Tag as Mp4Tag};

const LOOPSTART_KEY: &str = "LOOPSTART";
const LOOPEND_KEY: &str = "LOOPEND";
const ITUNES_MEAN: &str = "com.apple.iTunes";

pub fn read_loop_markers(path: &Path) -> Option<(u64, u64)> {
    match ext_lower(path)?.as_str() {
        "wav" => crate::wave::read_wav_loop_markers(path).map(|(s, e)| (s as u64, e as u64)),
        "mp3" => read_mp3_loop_markers(path).ok().flatten(),
        "m4a" => read_m4a_loop_markers(path).ok().flatten(),
        _ => None,
    }
}

pub fn write_loop_markers(path: &Path, loop_opt: Option<(u64, u64)>) -> Result<()> {
    match ext_lower(path).as_deref() {
        Some("wav") => {
            let loop_opt = loop_opt.and_then(|(s, e)| u64_to_u32_pair(s, e));
            crate::wave::write_wav_loop_markers(path, loop_opt)
        }
        Some("mp3") => write_mp3_loop_markers(path, loop_opt),
        Some("m4a") => write_m4a_loop_markers(path, loop_opt),
        _ => anyhow::bail!("unsupported loop marker format: {}", path.display()),
    }
}

fn ext_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
}

fn u64_to_u32_pair(s: u64, e: u64) -> Option<(u32, u32)> {
    if e <= s || s > u32::MAX as u64 || e > u32::MAX as u64 {
        return None;
    }
    Some((s as u32, e as u32))
}

fn read_mp3_loop_markers(path: &Path) -> Result<Option<(u64, u64)>> {
    let tag = match Tag::read_from_path(path) {
        Ok(t) => t,
        Err(e) if matches!(e.kind, id3::ErrorKind::NoTag) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let start = tag
        .extended_texts()
        .find(|t| t.description == LOOPSTART_KEY)
        .and_then(|t| t.value.parse::<u64>().ok());
    let end = tag
        .extended_texts()
        .find(|t| t.description == LOOPEND_KEY)
        .and_then(|t| t.value.parse::<u64>().ok());
    Ok(match (start, end) {
        (Some(s), Some(e)) if e > s => Some((s, e)),
        _ => None,
    })
}

fn write_mp3_loop_markers(path: &Path, loop_opt: Option<(u64, u64)>) -> Result<()> {
    let mut tag = match Tag::read_from_path(path) {
        Ok(t) => t,
        Err(e) if matches!(e.kind, id3::ErrorKind::NoTag) => Tag::new(),
        Err(e) => return Err(e.into()),
    };
    tag.remove_extended_text(Some(LOOPSTART_KEY), None);
    tag.remove_extended_text(Some(LOOPEND_KEY), None);
    if let Some((s, e)) = loop_opt {
        if e > s {
            tag.add_frame(Frame::with_content(
                "TXXX",
                Content::ExtendedText(ExtendedText {
                    description: LOOPSTART_KEY.to_string(),
                    value: s.to_string(),
                }),
            ));
            tag.add_frame(Frame::with_content(
                "TXXX",
                Content::ExtendedText(ExtendedText {
                    description: LOOPEND_KEY.to_string(),
                    value: e.to_string(),
                }),
            ));
        }
    }
    tag.write_to_path(path, Version::Id3v24)
        .with_context(|| format!("write mp3 tags: {}", path.display()))?;
    Ok(())
}

fn read_m4a_loop_markers(path: &Path) -> Result<Option<(u64, u64)>> {
    let tag = Mp4Tag::read_from_path(path)?;
    let k_start = FreeformIdent::new_static(ITUNES_MEAN, LOOPSTART_KEY);
    let k_end = FreeformIdent::new_static(ITUNES_MEAN, LOOPEND_KEY);
    let start = tag
        .strings_of(&k_start)
        .next()
        .and_then(|s| s.parse::<u64>().ok());
    let end = tag
        .strings_of(&k_end)
        .next()
        .and_then(|s| s.parse::<u64>().ok());
    Ok(match (start, end) {
        (Some(s), Some(e)) if e > s => Some((s, e)),
        _ => None,
    })
}

fn write_m4a_loop_markers(path: &Path, loop_opt: Option<(u64, u64)>) -> Result<()> {
    let mut tag = Mp4Tag::read_from_path(path)?;
    let k_start = FreeformIdent::new_static(ITUNES_MEAN, LOOPSTART_KEY);
    let k_end = FreeformIdent::new_static(ITUNES_MEAN, LOOPEND_KEY);
    tag.remove_strings_of(&k_start);
    tag.remove_strings_of(&k_end);
    if let Some((s, e)) = loop_opt {
        if e > s {
            tag.set_data(k_start, Data::Utf8(s.to_string()));
            tag.set_data(k_end, Data::Utf8(e.to_string()));
        }
    }
    tag.write_to_path(path)
        .with_context(|| format!("write m4a tags: {}", path.display()))?;
    Ok(())
}
