use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use id3::frame::{Content, ExtendedText, Frame};
use id3::{Tag, TagLike, Version};
use mp4ameta::{Data, FreeformIdent, Tag as Mp4Tag};
use serde::{Deserialize, Serialize};

const LOOPSTART_KEY: &str = "LOOPSTART";
const LOOPEND_KEY: &str = "LOOPEND";
const ITUNES_MEAN: &str = "com.apple.iTunes";

pub fn read_loop_markers(path: &Path) -> Option<(u64, u64)> {
    match ext_lower(path)?.as_str() {
        "wav"
            if loop_sidecar_path(path).is_file()
                || crate::wav_stream::wave_metadata_requires_sidecar(path) =>
        {
            read_sidecar_loop_markers(path)
        }
        "wav" => crate::wave::read_wav_loop_markers(path).map(|(s, e)| (s as u64, e as u64)),
        "aiff" | "aif" => {
            crate::wave::read_aiff_loop_markers(path).map(|(s, e)| (s as u64, e as u64))
        }
        "flac" => crate::flac_meta::read_flac_loop_markers(path)
            .ok()
            .flatten(),
        "mp3" => read_mp3_loop_markers(path).ok().flatten(),
        "m4a" => read_m4a_loop_markers(path).ok().flatten(),
        // Video containers: sidecar only, matching how they are written.
        // Formats without in-file loop support (ogg): JSON sidecar.
        _ => read_sidecar_loop_markers(path),
    }
}

pub fn write_loop_markers(path: &Path, loop_opt: Option<(u64, u64)>) -> Result<()> {
    match ext_lower(path).as_deref() {
        Some("wav")
            if crate::wav_stream::wave_metadata_requires_sidecar(path)
                || loop_opt
                    .map(|(s, e)| s > u32::MAX as u64 || e > u32::MAX as u64)
                    .unwrap_or(false) =>
        {
            write_sidecar_loop_markers(path, loop_opt)
        }
        Some("wav") => {
            let loop_opt = loop_opt.and_then(|(s, e)| u64_to_u32_pair(s, e));
            crate::wave::write_wav_loop_markers(path, loop_opt)?;
            let stale_sidecar = loop_sidecar_path(path);
            if stale_sidecar.is_file() {
                let _ = std::fs::remove_file(stale_sidecar);
            }
            Ok(())
        }
        Some("aiff") | Some("aif") => {
            let loop_opt = loop_opt.and_then(|(s, e)| u64_to_u32_pair(s, e));
            crate::wave::write_aiff_loop_markers(path, loop_opt)
        }
        Some("flac") => crate::flac_meta::write_flac_loop_markers(path, loop_opt),
        Some("mp3") => write_mp3_loop_markers(path, loop_opt),
        Some("m4a") => write_m4a_loop_markers(path, loop_opt),
        // A video container has the same freeform atom an .m4a does, but the
        // app never rewrites a video file — see `crate::media_kind`. The loop
        // region goes to the sidecar instead, so the picture is untouched.
        Some("mp4") | Some("mov") | Some("m4v") | Some("3gp") | Some("3g2") => {
            write_sidecar_loop_markers(path, loop_opt)
        }
        // Formats without in-file loop support (ogg): JSON sidecar so a save
        // with a loop region no longer counts as a failure.
        _ => write_sidecar_loop_markers(path, loop_opt),
    }
}

#[derive(Serialize, Deserialize)]
struct LoopSidecar {
    version: u32,
    loop_start: u64,
    loop_end: u64,
}

fn loop_sidecar_path(path: &Path) -> PathBuf {
    path.with_extension("loop.json")
}

fn read_sidecar_loop_markers(path: &Path) -> Option<(u64, u64)> {
    let bytes = std::fs::read(loop_sidecar_path(path)).ok()?;
    let data: LoopSidecar = serde_json::from_slice(&bytes).ok()?;
    (data.loop_end > data.loop_start).then_some((data.loop_start, data.loop_end))
}

fn write_sidecar_loop_markers(path: &Path, loop_opt: Option<(u64, u64)>) -> Result<()> {
    let sidecar = loop_sidecar_path(path);
    match loop_opt.filter(|(s, e)| e > s) {
        Some((start, end)) => {
            let payload = LoopSidecar {
                version: 1,
                loop_start: start,
                loop_end: end,
            };
            let text = serde_json::to_vec_pretty(&payload)?;
            std::fs::write(&sidecar, text)
                .with_context(|| format!("write loop sidecar: {}", sidecar.display()))?;
        }
        None => {
            if sidecar.is_file() {
                std::fs::remove_file(&sidecar)
                    .with_context(|| format!("remove loop sidecar: {}", sidecar.display()))?;
            }
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rf64_loop_sidecar_preserves_64_bit_positions() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves_rf64_loop_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long.wav");
        std::fs::write(&path, b"RF64\xff\xff\xff\xffWAVE").unwrap();
        let expected = (u32::MAX as u64 + 100, u32::MAX as u64 + 10_000);
        write_loop_markers(&path, Some(expected)).unwrap();
        assert_eq!(read_loop_markers(&path), Some(expected));
        assert_eq!(std::fs::read(&path).unwrap(), b"RF64\xff\xff\xff\xffWAVE");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
