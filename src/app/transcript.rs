use std::path::{Path, PathBuf};

use super::types::{Transcript, TranscriptSegment};

fn parse_timestamp_ms(s: &str) -> Option<u64> {
    let s = s.trim().replace(',', ".");
    let mut parts = s.split(':');
    let h = parts.next()?.trim().parse::<u64>().ok()?;
    let m = parts.next()?.trim().parse::<u64>().ok()?;
    let sec_ms = parts.next()?.trim();
    let mut sec_parts = sec_ms.split('.');
    let sec = sec_parts.next()?.parse::<u64>().ok()?;
    let ms = sec_parts
        .next()
        .unwrap_or("0")
        .chars()
        .take(3)
        .collect::<String>()
        .parse::<u64>()
        .ok()
        .unwrap_or(0);
    Some((((h * 60 + m) * 60) + sec) * 1000 + ms)
}

pub fn srt_path_for_audio(audio_path: &Path) -> Option<PathBuf> {
    let stem = audio_path.file_stem()?.to_string_lossy();
    let parent = audio_path.parent()?;
    Some(parent.join(format!("{}.srt", stem)))
}

pub fn load_srt(path: &Path) -> Option<Transcript> {
    let bytes = std::fs::read(path).ok()?;
    let text = decode_srt_bytes(&bytes);
    let mut transcript = parse_srt(&text);
    if transcript.full_text.trim().is_empty() {
        let fallback = fallback_plain_text(&text);
        if !fallback.is_empty() {
            transcript.full_text = fallback;
        }
    }
    Some(transcript)
}

fn decode_srt_bytes(bytes: &[u8]) -> String {
    if let Ok(s) = String::from_utf8(bytes.to_vec()) {
        return s;
    }
    if bytes.len() >= 2 {
        // UTF-16 BOM
        if bytes[0] == 0xFF && bytes[1] == 0xFE {
            return decode_utf16_lossy(&bytes[2..], true);
        }
        if bytes[0] == 0xFE && bytes[1] == 0xFF {
            return decode_utf16_lossy(&bytes[2..], false);
        }
    }
    // Heuristic for UTF-16 without BOM (common in Windows exports)
    if bytes.len() >= 4 {
        let even_zero = bytes.iter().step_by(2).filter(|&&b| b == 0).count();
        let odd_zero = bytes.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
        let pairs = bytes.len() / 2;
        if odd_zero > pairs / 3 {
            return decode_utf16_lossy(bytes, true);
        }
        if even_zero > pairs / 3 {
            return decode_utf16_lossy(bytes, false);
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16_lossy(bytes: &[u8], little_endian: bool) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        let pair = [bytes[i], bytes[i + 1]];
        let u = if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        };
        units.push(u);
        i += 2;
    }
    String::from_utf16_lossy(&units)
}

pub fn parse_srt(text: &str) -> Transcript {
    let mut segments = Vec::new();
    let mut full_text = String::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Optional index line
        if line.chars().all(|c| c.is_ascii_digit()) {
            // consume next line for timing
            if let Some(timing) = lines.next() {
                if let Some((start_ms, end_ms)) = parse_timing_line(timing) {
                    let mut text_lines = Vec::new();
                    while let Some(t) = lines.peek() {
                        if t.trim().is_empty() {
                            lines.next();
                            break;
                        }
                        text_lines.push(lines.next().unwrap_or_default());
                    }
                    let text_block = text_lines.join(" ");
                    if !text_block.is_empty() {
                        if !full_text.is_empty() {
                            full_text.push(' ');
                        }
                        full_text.push_str(&text_block);
                    }
                    segments.push(TranscriptSegment {
                        start_ms,
                        end_ms,
                        text: text_block,
                    });
                }
            }
            continue;
        }
        // Timing line without explicit index
        if let Some((start_ms, end_ms)) = parse_timing_line(line) {
            let mut text_lines = Vec::new();
            while let Some(t) = lines.peek() {
                if t.trim().is_empty() {
                    lines.next();
                    break;
                }
                text_lines.push(lines.next().unwrap_or_default());
            }
            let text_block = text_lines.join(" ");
            if !text_block.is_empty() {
                if !full_text.is_empty() {
                    full_text.push(' ');
                }
                full_text.push_str(&text_block);
            }
            segments.push(TranscriptSegment {
                start_ms,
                end_ms,
                text: text_block,
            });
        }
    }
    Transcript {
        segments,
        full_text,
    }
}

fn format_timestamp_ms(ms: u64) -> String {
    let h = ms / 3_600_000;
    let rem1 = ms % 3_600_000;
    let m = rem1 / 60_000;
    let rem2 = rem1 % 60_000;
    let s = rem2 / 1_000;
    let milli = rem2 % 1_000;
    format!("{h:02}:{m:02}:{s:02},{milli:03}")
}

pub fn write_srt(path: &Path, transcript: &Transcript) -> std::io::Result<()> {
    let mut out = String::new();
    let mut index = 1usize;
    if transcript.segments.is_empty() {
        let text = transcript.full_text.trim();
        if !text.is_empty() {
            out.push_str(&format!(
                "{}\n{} --> {}\n{}\n\n",
                index,
                format_timestamp_ms(0),
                format_timestamp_ms(500),
                text
            ));
        }
    } else {
        for seg in &transcript.segments {
            let start_ms = seg.start_ms;
            let mut end_ms = seg.end_ms.max(start_ms.saturating_add(1));
            if end_ms <= start_ms {
                end_ms = start_ms.saturating_add(500);
            }
            out.push_str(&format!(
                "{}\n{} --> {}\n{}\n\n",
                index,
                format_timestamp_ms(start_ms),
                format_timestamp_ms(end_ms),
                seg.text.trim()
            ));
            index += 1;
        }
    }
    std::fs::write(path, out)
}

fn parse_timing_line(line: &str) -> Option<(u64, u64)> {
    let mut parts = line.split("-->");
    let start = parts.next()?.trim();
    let end = parts.next()?.trim();
    let start_ms = parse_timestamp_ms(start)?;
    let end_ms = parse_timestamp_ms(end)?;
    if end_ms > start_ms {
        Some((start_ms, end_ms))
    } else {
        None
    }
}

fn fallback_plain_text(text: &str) -> String {
    let mut out = Vec::<String>::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if t.contains("-->") {
            continue;
        }
        out.push(t.to_string());
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_srt_roundtrip() {
        let transcript = Transcript {
            segments: vec![
                TranscriptSegment {
                    start_ms: 0,
                    end_ms: 1500,
                    text: "hello".to_string(),
                },
                TranscriptSegment {
                    start_ms: 1600,
                    end_ms: 2600,
                    text: "world".to_string(),
                },
            ],
            full_text: "hello world".to_string(),
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("neowaves_transcript_{nonce}.srt"));
        write_srt(&path, &transcript).expect("write_srt");
        let loaded = load_srt(&path).expect("load_srt");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.segments.len(), 2);
        assert!(loaded.full_text.contains("hello"));
        assert!(loaded.full_text.contains("world"));
    }
}
