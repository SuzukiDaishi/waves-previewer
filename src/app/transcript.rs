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
    let text = std::fs::read_to_string(path).ok()?;
    Some(parse_srt(&text))
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
    Transcript { segments, full_text }
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
