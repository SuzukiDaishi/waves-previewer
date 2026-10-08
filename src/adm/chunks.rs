//! A RIFF / RF64 / BW64 chunk list that does not stop at `data`.
//!
//! `wav_stream::read_wave_pcm_info` stops at the audio, which is all playback
//! needs; ADM keeps `axml` (and often `chna`) after it. This walks every
//! chunk header -- 8 bytes each, plus the `ds64` table -- and reads no
//! payload, so it costs a few seeks however large the file is. Sizes past
//! 4 GiB come from `ds64` (the same rules as `metadata::scan_riff`).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// Where one chunk is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkLoc {
    pub id: [u8; 4],
    /// Offset of the 8-byte header.
    pub header_offset: u64,
    pub payload_offset: u64,
    /// The size the chunk declares (from `ds64` when it says `0xFFFFFFFF`).
    pub size: u64,
    /// How much of it the file actually holds.
    pub readable: u64,
}

impl ChunkLoc {
    /// Offset just past the chunk, padding included. Saturates: a size taken
    /// from a damaged `ds64` can be anything.
    pub fn end(&self) -> u64 {
        self.payload_offset
            .saturating_add(self.size)
            .saturating_add(self.size & 1)
    }
}

/// Every chunk of a WAVE file, in file order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkList {
    /// `RIFF`, `RF64` or `BW64`.
    pub root: [u8; 4],
    pub file_len: u64,
    pub chunks: Vec<ChunkLoc>,
}

impl ChunkList {
    pub fn find(&self, id: &[u8; 4]) -> Option<&ChunkLoc> {
        self.chunks.iter().find(|chunk| &chunk.id == id)
    }

    pub fn is_64bit(&self) -> bool {
        matches!(&self.root, b"RF64" | b"BW64")
    }
}

/// More chunks than any real WAVE carries. A damaged or hostile file of
/// nothing but empty chunks would otherwise list one per 8 bytes.
const MAX_CHUNKS: usize = 4096;

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// The chunk list of a WAVE stream, or `None` for anything that is not one.
pub fn walk<R: Read + Seek>(reader: &mut R, file_len: u64) -> Result<Option<ChunkList>> {
    if file_len < 12 {
        return Ok(None);
    }
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 12];
    reader.read_exact(&mut header)?;
    let root: [u8; 4] = header[0..4].try_into().unwrap();
    if !matches!(&root, b"RIFF" | b"RF64" | b"BW64") || &header[8..12] != b"WAVE" {
        return Ok(None);
    }
    let mut chunks = Vec::new();
    let mut ds64_data_size = None;
    let mut ds64_table: HashMap<[u8; 4], Vec<u64>> = HashMap::new();
    let mut pos = 12u64;
    while pos.saturating_add(8) <= file_len {
        reader.seek(SeekFrom::Start(pos))?;
        let mut chunk_header = [0u8; 8];
        reader.read_exact(&mut chunk_header)?;
        let id: [u8; 4] = chunk_header[0..4].try_into().unwrap();
        let size32 = u32_at(&chunk_header, 4).unwrap_or(0);
        let payload_offset = pos + 8;
        let mut size = size32 as u64;
        if size32 == u32::MAX {
            size = if &id == b"data" {
                ds64_data_size.unwrap_or(size)
            } else {
                ds64_table
                    .get_mut(&id)
                    .and_then(|sizes| (!sizes.is_empty()).then(|| sizes.remove(0)))
                    .unwrap_or(size)
            };
        }
        let readable = size.min(file_len.saturating_sub(payload_offset));
        if &id == b"ds64" {
            let mut bytes = vec![0u8; readable.min(64 * 1024) as usize];
            reader.read_exact(&mut bytes)?;
            ds64_data_size = u64_at(&bytes, 8);
            let table_len = u32_at(&bytes, 24).unwrap_or(0) as usize;
            for index in 0..table_len {
                let at = 28 + index * 12;
                let (Some(table_id), Some(table_size)) =
                    (bytes.get(at..at + 4), u64_at(&bytes, at + 4))
                else {
                    break;
                };
                ds64_table
                    .entry(table_id.try_into().unwrap())
                    .or_default()
                    .push(table_size);
            }
        }
        chunks.push(ChunkLoc {
            id,
            header_offset: pos,
            payload_offset,
            size,
            readable,
        });
        if readable < size || chunks.len() >= MAX_CHUNKS {
            break;
        }
        pos = payload_offset
            .checked_add(size)
            .and_then(|end| end.checked_add(size & 1))
            .ok_or_else(|| anyhow!("WAVE chunk offset overflow"))?;
    }
    Ok(Some(ChunkList {
        root,
        file_len,
        chunks,
    }))
}

/// [`walk`] over a file. Blocks: never call it from the UI thread.
pub fn walk_path(path: &Path) -> Result<Option<ChunkList>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let file_len = file.metadata()?.len();
    walk(&mut BufReader::new(file), file_len)
}

/// The first `max` bytes of a chunk's payload.
pub fn read_payload<R: Read + Seek>(reader: &mut R, chunk: &ChunkLoc, max: u64) -> Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(chunk.payload_offset))?;
    let len = chunk.readable.min(max) as usize;
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn chunk(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    #[test]
    fn chunks_after_data_and_odd_sizes_are_found() {
        let mut body = b"WAVE".to_vec();
        body.extend(chunk(b"fmt ", &[0u8; 16]));
        body.extend(chunk(b"chna", &[1, 2, 3]));
        body.extend(chunk(b"data", &[0u8; 8]));
        body.extend(chunk(b"axml", b"<x/>"));
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend(body);
        let len = file.len() as u64;
        let list = walk(&mut Cursor::new(file), len).unwrap().unwrap();
        let ids: Vec<&[u8; 4]> = list.chunks.iter().map(|c| &c.id).collect();
        assert_eq!(ids, [b"fmt ", b"chna", b"data", b"axml"]);
        assert_eq!(list.find(b"chna").unwrap().size, 3);
        assert_eq!(list.find(b"axml").unwrap().size, 4);
        assert!(!list.is_64bit());
    }

    #[test]
    fn ds64_supplies_sizes_that_do_not_fit_32_bits() {
        let mut ds64 = Vec::new();
        ds64.extend_from_slice(&0u64.to_le_bytes()); // riff size
        ds64.extend_from_slice(&8u64.to_le_bytes()); // data size
        ds64.extend_from_slice(&1u64.to_le_bytes()); // sample count
        ds64.extend_from_slice(&1u32.to_le_bytes()); // table length
        ds64.extend_from_slice(b"axml");
        ds64.extend_from_slice(&6u64.to_le_bytes());
        let mut file = b"BW64".to_vec();
        file.extend_from_slice(&u32::MAX.to_le_bytes());
        file.extend_from_slice(b"WAVE");
        file.extend(chunk(b"ds64", &ds64));
        file.extend_from_slice(b"data");
        file.extend_from_slice(&u32::MAX.to_le_bytes());
        file.extend_from_slice(&[0u8; 8]);
        file.extend_from_slice(b"axml");
        file.extend_from_slice(&u32::MAX.to_le_bytes());
        file.extend_from_slice(b"<a/>  ");
        let len = file.len() as u64;
        let list = walk(&mut Cursor::new(file), len).unwrap().unwrap();
        assert!(list.is_64bit());
        assert_eq!(list.find(b"data").unwrap().size, 8);
        assert_eq!(list.find(b"axml").unwrap().size, 6);
    }

    #[test]
    fn a_file_of_empty_chunks_stops_at_the_limit() {
        let mut body = b"WAVE".to_vec();
        for _ in 0..(MAX_CHUNKS + 100) {
            body.extend(chunk(b"JUNK", &[]));
        }
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend(body);
        let len = file.len() as u64;
        let list = walk(&mut Cursor::new(file), len).unwrap().unwrap();
        assert_eq!(list.chunks.len(), MAX_CHUNKS);
    }

    #[test]
    fn anything_else_is_not_a_wave() {
        let bytes = b"OggS\0\0\0\0\0\0\0\0\0\0".to_vec();
        let len = bytes.len() as u64;
        assert!(walk(&mut Cursor::new(bytes), len).unwrap().is_none());
    }
}
