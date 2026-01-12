use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

const MARKER_FILE_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct MarkerEntry {
    pub sample: usize,
    pub label: String,
}

#[derive(Serialize, Deserialize)]
struct MarkerFile {
    version: u32,
    sample_rate: u32,
    markers: Vec<MarkerRecord>,
}

#[derive(Serialize, Deserialize)]
struct MarkerRecord {
    sample: u64,
    label: String,
}

#[derive(Deserialize)]
struct LegacyMarkerFile {
    sample_rate: u32,
    markers: Vec<u64>,
}

fn sidecar_path(path: &Path) -> PathBuf {
    path.with_extension("markers.json")
}

pub fn read_markers(path: &Path, out_sr: u32, file_sr: u32) -> Result<Vec<MarkerEntry>> {
    if path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("wav"))
        .unwrap_or(false)
    {
        if let Ok(markers) = read_wav_markers(path) {
            return Ok(map_markers_to_output(markers, out_sr, file_sr));
        }
    }
    let sidecar = sidecar_path(path);
    if !sidecar.is_file() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&sidecar)?;
    let data: MarkerFile = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            let legacy: LegacyMarkerFile = serde_json::from_slice(&bytes)?;
            MarkerFile {
                version: MARKER_FILE_VERSION,
                sample_rate: legacy.sample_rate,
                markers: legacy
                    .markers
                    .into_iter()
                    .enumerate()
                    .map(|(i, sample)| MarkerRecord {
                        sample,
                        label: format!("M{:02}", i + 1),
                    })
                    .collect(),
            }
        }
    };
    let src_sr = if data.sample_rate > 0 {
        data.sample_rate
    } else {
        file_sr.max(1)
    };
    Ok(map_markers_to_output(data.markers, out_sr, src_sr))
}

pub fn write_markers(
    path: &Path,
    out_sr: u32,
    file_sr: u32,
    markers: &[MarkerEntry],
) -> Result<()> {
    if path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("wav"))
        .unwrap_or(false)
    {
        let mapped = map_markers_to_file(markers, out_sr, file_sr);
        write_wav_markers(path, &mapped)?;
        return Ok(());
    }
    let sidecar = sidecar_path(path);
    let src_sr = file_sr.max(1);
    let stored = map_markers_to_file(markers, out_sr, src_sr);
    let payload = MarkerFile {
        version: MARKER_FILE_VERSION,
        sample_rate: src_sr,
        markers: stored,
    };
    let text = serde_json::to_vec_pretty(&payload)?;
    std::fs::write(sidecar, text)?;
    Ok(())
}

fn map_markers_to_output(
    records: Vec<MarkerRecord>,
    out_sr: u32,
    src_sr: u32,
) -> Vec<MarkerEntry> {
    let dst_sr = out_sr.max(1);
    let ratio = dst_sr as f64 / src_sr.max(1) as f64;
    let mut markers: Vec<MarkerEntry> = records
        .into_iter()
        .map(|m| MarkerEntry {
            sample: ((m.sample as f64) * ratio).round().max(0.0) as usize,
            label: m.label,
        })
        .collect();
    markers.sort_by_key(|m| m.sample);
    markers.dedup_by(|a, b| a.sample == b.sample && a.label == b.label);
    markers
}

fn map_markers_to_file(
    markers: &[MarkerEntry],
    out_sr: u32,
    file_sr: u32,
) -> Vec<MarkerRecord> {
    let dst_sr = out_sr.max(1);
    let ratio = file_sr.max(1) as f64 / dst_sr as f64;
    let mut stored: Vec<MarkerRecord> = markers
        .iter()
        .map(|m| MarkerRecord {
            sample: ((m.sample as f64) * ratio).round().max(0.0) as u64,
            label: m.label.clone(),
        })
        .collect();
    stored.sort_by_key(|m| m.sample);
    stored.dedup_by(|a, b| a.sample == b.sample && a.label == b.label);
    stored
}

fn read_wav_markers(path: &Path) -> Result<Vec<MarkerRecord>> {
    let data = std::fs::read(path)?;
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        anyhow::bail!("not a RIFF/WAVE file");
    }
    let mut cue_points: Vec<(u32, u32)> = Vec::new();
    let mut labels: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([
            data[pos + 4],
            data[pos + 5],
            data[pos + 6],
            data[pos + 7],
        ]) as usize;
        let chunk_start = pos + 8;
        let chunk_end = chunk_start.saturating_add(size).min(data.len());
        if id == b"cue " && chunk_end >= chunk_start + 4 {
            let count = u32::from_le_bytes([
                data[chunk_start],
                data[chunk_start + 1],
                data[chunk_start + 2],
                data[chunk_start + 3],
            ]) as usize;
            let mut off = chunk_start + 4;
            for _ in 0..count {
                if off + 24 > chunk_end {
                    break;
                }
                let cue_id = u32::from_le_bytes([
                    data[off],
                    data[off + 1],
                    data[off + 2],
                    data[off + 3],
                ]);
                let sample_offset = u32::from_le_bytes([
                    data[off + 20],
                    data[off + 21],
                    data[off + 22],
                    data[off + 23],
                ]);
                cue_points.push((cue_id, sample_offset));
                off += 24;
            }
        } else if id == b"LIST" && chunk_end >= chunk_start + 4 {
            if &data[chunk_start..chunk_start + 4] == b"adtl" {
                let mut off = chunk_start + 4;
                while off + 8 <= chunk_end {
                    let sub_id = &data[off..off + 4];
                    let sub_size = u32::from_le_bytes([
                        data[off + 4],
                        data[off + 5],
                        data[off + 6],
                        data[off + 7],
                    ]) as usize;
                    let sub_start = off + 8;
                    let sub_end = sub_start.saturating_add(sub_size).min(chunk_end);
                    if sub_id == b"labl" && sub_end >= sub_start + 4 {
                        let cue_id = u32::from_le_bytes([
                            data[sub_start],
                            data[sub_start + 1],
                            data[sub_start + 2],
                            data[sub_start + 3],
                        ]);
                        let text_bytes = &data[sub_start + 4..sub_end];
                        let label = String::from_utf8_lossy(text_bytes)
                            .trim_end_matches('\0')
                            .to_string();
                        labels.insert(cue_id, label);
                    }
                    let pad = sub_size & 1;
                    off = sub_start.saturating_add(sub_size + pad);
                }
            }
        }
        let advance = 8 + size + (size & 1);
        if pos + advance <= pos {
            break;
        }
        pos = pos.saturating_add(advance);
    }
    cue_points.sort_by_key(|(_, sample)| *sample);
    let mut out = Vec::new();
    for (idx, (cue_id, sample)) in cue_points.into_iter().enumerate() {
        let label = labels
            .remove(&cue_id)
            .unwrap_or_else(|| format!("M{:02}", idx + 1));
        out.push(MarkerRecord {
            sample: sample as u64,
            label,
        });
    }
    Ok(out)
}

fn write_wav_markers(path: &Path, markers: &[MarkerRecord]) -> Result<()> {
    use std::fs;
    let data = fs::read(path)?;
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        anyhow::bail!("not a RIFF/WAVE file");
    }
    let mut out: Vec<u8> = Vec::with_capacity(data.len() + 512);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&[0, 0, 0, 0]); // placeholder size
    out.extend_from_slice(b"WAVE");
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([
            data[pos + 4],
            data[pos + 5],
            data[pos + 6],
            data[pos + 7],
        ]) as usize;
        let chunk_start = pos + 8;
        let chunk_end = chunk_start.saturating_add(size).min(data.len());
        let is_cue = id == b"cue ";
        let is_adtl = if id == b"LIST" && chunk_end >= chunk_start + 4 {
            &data[chunk_start..chunk_start + 4] == b"adtl"
        } else {
            false
        };
        if !is_cue && !is_adtl {
            out.extend_from_slice(id);
            out.extend_from_slice(&(size as u32).to_le_bytes());
            out.extend_from_slice(&data[chunk_start..chunk_end]);
            if size & 1 == 1 {
                out.push(0);
            }
        }
        let advance = 8 + size + (size & 1);
        if pos + advance <= pos {
            break;
        }
        pos = pos.saturating_add(advance);
    }

    if !markers.is_empty() {
        let mut cue_chunk: Vec<u8> = Vec::new();
        cue_chunk.extend_from_slice(&(markers.len() as u32).to_le_bytes());
        for (idx, m) in markers.iter().enumerate() {
            let cue_id = (idx as u32) + 1;
            let sample = (m.sample.min(u32::MAX as u64)) as u32;
            cue_chunk.extend_from_slice(&cue_id.to_le_bytes()); // cue_point_id
            cue_chunk.extend_from_slice(&sample.to_le_bytes()); // position
            cue_chunk.extend_from_slice(b"data"); // data_chunk_id
            cue_chunk.extend_from_slice(&0u32.to_le_bytes()); // chunk_start
            cue_chunk.extend_from_slice(&0u32.to_le_bytes()); // block_start
            cue_chunk.extend_from_slice(&sample.to_le_bytes()); // sample_offset
        }
        out.extend_from_slice(b"cue ");
        out.extend_from_slice(&(cue_chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&cue_chunk);
        if cue_chunk.len() & 1 == 1 {
            out.push(0);
        }

        let mut adtl_data: Vec<u8> = Vec::new();
        adtl_data.extend_from_slice(b"adtl");
        for (idx, m) in markers.iter().enumerate() {
            let cue_id = (idx as u32) + 1;
            let mut label_bytes = m.label.clone().into_bytes();
            label_bytes.push(0);
            let chunk_size = 4 + label_bytes.len();
            adtl_data.extend_from_slice(b"labl");
            adtl_data.extend_from_slice(&(chunk_size as u32).to_le_bytes());
            adtl_data.extend_from_slice(&cue_id.to_le_bytes());
            adtl_data.extend_from_slice(&label_bytes);
            if chunk_size & 1 == 1 {
                adtl_data.push(0);
            }
        }
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&(adtl_data.len() as u32).to_le_bytes());
        out.extend_from_slice(&adtl_data);
        if adtl_data.len() & 1 == 1 {
            out.push(0);
        }
    }
    let riff_size = (out.len().saturating_sub(8)) as u32;
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join("._wvp_tmp_markers.wav");
    if tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    fs::write(&tmp, out)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
