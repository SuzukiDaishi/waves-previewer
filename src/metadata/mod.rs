//! Read-only, bounded metadata/container inspection.
//!
//! This module deliberately does not share the legacy RIFF writers.  Physical
//! structure is represented by file ranges, and payload bytes are opened only
//! by an explicit read/search/hash/extract operation.

pub mod cache;
pub mod ucs;

use anyhow::{anyhow, bail, Context, Result};
use encoding_rs::{SHIFT_JIS, WINDOWS_1252};
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader as XmlReader;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

pub const PARSER_SCHEMA_VERSION: u32 = 1;
pub const UCS_DATA_VERSION: &str = "8.2.1";
pub const ASWG_SCHEMA_VERSION: &str = "1.1";
pub const MAX_XML_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_XML_DEPTH: usize = 128;
pub const MAX_XML_NODES: usize = 100_000;
pub const MAX_XML_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const PAYLOAD_COPY_LIMIT: u64 = 16 * 1024 * 1024;
pub const PAYLOAD_SEARCH_RESULT_LIMIT: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    RiffWave,
    Rf64,
    Bw64,
    Mp3,
    Mp4,
    Flac,
    Ogg,
    Aiff,
    Aifc,
    Unknown,
}

impl ContainerKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::RiffWave => "riff",
            Self::Rf64 => "rf64",
            Self::Bw64 => "bw64",
            Self::Mp3 => "id3",
            Self::Mp4 => "mp4",
            Self::Flac => "flac",
            Self::Ogg => "ogg",
            Self::Aiff => "aiff",
            Self::Aifc => "aifc",
            Self::Unknown => "binary",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Completion {
    Complete,
    Partial,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseStatus {
    Parsed,
    Deferred,
    Unsupported,
    Truncated,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Container,
    Audio,
    Xml,
    Text,
    Binary,
    Image,
    Padding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataDiagnostic {
    pub level: DiagnosticLevel,
    pub code: String,
    pub message: String,
    pub offset: Option<u64>,
    pub node: Option<NodeId>,
}

pub type NodeId = u32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadRef {
    pub file_offset: u64,
    pub length: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum MetadataValue {
    Text(String),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    Bool(bool),
    BinarySummary(String),
}

impl MetadataValue {
    pub fn display(&self) -> String {
        match self {
            Self::Text(v) => v.clone(),
            Self::Signed(v) => v.to_string(),
            Self::Unsigned(v) => v.to_string(),
            Self::Float(v) => format!("{v}"),
            Self::Bool(v) => v.to_string(),
            Self::BinarySummary(v) => v.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourcedValue {
    pub value: MetadataValue,
    pub source_path: String,
    pub source_node: NodeId,
    pub source_range: SourceRange,
    pub encoding: Option<String>,
    pub guessed_encoding: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalizedField {
    pub key: String,
    pub values: Vec<SourcedValue>,
    pub resolved_index: usize,
    pub conflict: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioByteMapping {
    pub node_id: NodeId,
    pub data_offset: u64,
    pub data_length: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub block_align: u16,
    pub container_bits: u16,
    pub valid_bits: u16,
    pub format_tag: u16,
    pub format_subtype: Option<u16>,
    pub format_name: String,
}

impl AudioByteMapping {
    pub fn frame_count(&self) -> u64 {
        self.data_length / self.block_align.max(1) as u64
    }

    pub fn frame_range(&self, source_frame: u64) -> Option<SourceRange> {
        if self.block_align == 0 || source_frame >= self.frame_count() {
            return None;
        }
        let relative = source_frame.checked_mul(self.block_align as u64)?;
        let offset = self.data_offset.checked_add(relative)?;
        Some(SourceRange {
            offset,
            length: self.block_align as u64,
        })
    }

    pub fn channel_range(&self, source_frame: u64, channel: u16) -> Option<SourceRange> {
        if channel >= self.channels || self.channels == 0 {
            return None;
        }
        let frame = self.frame_range(source_frame)?;
        let bytes_per_channel = self.block_align as u64 / self.channels as u64;
        Some(SourceRange {
            offset: frame
                .offset
                .checked_add(bytes_per_channel.checked_mul(channel as u64)?)?,
            length: bytes_per_channel,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetadataNode {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub name: String,
    pub path: String,
    pub offset: u64,
    pub header_size: u64,
    pub declared_size: u64,
    pub readable_size: u64,
    pub payload: PayloadRef,
    pub content: ContentKind,
    pub known: bool,
    pub status: ParseStatus,
    pub summary: Option<String>,
}

impl MetadataNode {
    pub fn total_range(&self) -> SourceRange {
        SourceRange {
            offset: self.offset,
            length: self.header_size.saturating_add(self.readable_size),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetadataDocument {
    pub schema_version: u32,
    pub source: PathBuf,
    pub container: ContainerKind,
    pub file_len: u64,
    pub roots: Vec<NodeId>,
    pub nodes: Vec<MetadataNode>,
    pub normalized: Vec<NormalizedField>,
    pub diagnostics: Vec<MetadataDiagnostic>,
    pub completion: Completion,
    pub audio_mapping: Option<AudioByteMapping>,
}

impl MetadataDocument {
    pub fn node(&self, id: NodeId) -> Option<&MetadataNode> {
        self.nodes.get(id as usize)
    }

    pub fn find_path(&self, path: &str, occurrence: usize) -> Option<&MetadataNode> {
        self.nodes
            .iter()
            .filter(|node| node.path == path)
            .nth(occurrence)
            .or_else(|| {
                self.nodes
                    .iter()
                    .filter(|node| {
                        node.path.trim_start_matches('/') == path.trim_start_matches('/')
                    })
                    .nth(occurrence)
            })
    }

    pub fn smallest_node_at(&self, offset: u64) -> Option<&MetadataNode> {
        self.nodes
            .iter()
            .filter(|node| {
                let range = node.total_range();
                offset >= range.offset && offset < range.offset.saturating_add(range.length)
            })
            .min_by_key(|node| node.total_range().length)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SummaryField {
    pub key: String,
    pub values: Vec<SourcedValue>,
    pub resolved: Option<MetadataValue>,
    pub conflict: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetadataSummary {
    pub schema_version: u32,
    pub source: PathBuf,
    pub container: ContainerKind,
    pub completion: Completion,
    pub fields: Vec<SummaryField>,
    pub raw_fields: BTreeMap<String, Vec<MetadataValue>>,
    pub unknown_nodes: usize,
    pub diagnostics: Vec<MetadataDiagnostic>,
    pub coverage: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SummaryRequest {
    pub fields: Vec<String>,
    pub include_raw: bool,
}

#[derive(Clone, Debug)]
pub struct ScanBudget {
    pub max_bytes_read: u64,
    pub max_nodes: usize,
    pub max_duration: Option<Duration>,
}

impl ScanBudget {
    pub fn list() -> Self {
        Self {
            max_bytes_read: 16 * 1024 * 1024,
            max_nodes: 8_192,
            max_duration: Some(Duration::from_millis(250)),
        }
    }

    pub fn selected() -> Self {
        Self {
            max_bytes_read: 64 * 1024 * 1024,
            max_nodes: 32_768,
            max_duration: Some(Duration::from_secs(2)),
        }
    }

    pub fn editor() -> Self {
        Self {
            max_bytes_read: 128 * 1024 * 1024,
            max_nodes: 100_000,
            max_duration: None,
        }
    }
}

impl Default for ScanBudget {
    fn default() -> Self {
        Self::editor()
    }
}

#[derive(Clone, Debug)]
pub struct InspectOptions {
    pub budget: ScanBudget,
    pub decode_xml: bool,
    pub decode_values: bool,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for InspectOptions {
    fn default() -> Self {
        Self {
            budget: ScanBudget::editor(),
            decode_xml: true,
            decode_values: true,
            cancel: None,
        }
    }
}

#[derive(Debug)]
struct BudgetState {
    started: Instant,
    bytes_read: u64,
    partial_reason: Option<String>,
}

struct ScanIo<'a, R: Read + Seek> {
    reader: &'a mut R,
    file_len: u64,
    options: &'a InspectOptions,
    state: BudgetState,
}

impl<'a, R: Read + Seek> ScanIo<'a, R> {
    fn check(&mut self) -> Result<()> {
        if self
            .options
            .cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            self.state.partial_reason = Some("cancelled".to_string());
            bail!("metadata scan cancelled");
        }
        if let Some(limit) = self.options.budget.max_duration {
            if self.state.started.elapsed() > limit {
                self.state.partial_reason = Some("time budget exceeded".to_string());
                bail!("metadata scan time budget exceeded");
            }
        }
        Ok(())
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.check()?;
        let remaining_budget = self
            .options
            .budget
            .max_bytes_read
            .saturating_sub(self.state.bytes_read);
        if len as u64 > remaining_budget {
            self.state.partial_reason = Some("read budget exceeded".to_string());
            bail!("metadata scan read budget exceeded");
        }
        if offset > self.file_len || (len as u64) > self.file_len.saturating_sub(offset) {
            bail!("requested range is outside the file");
        }
        self.reader.seek(SeekFrom::Start(offset))?;
        let mut out = vec![0u8; len];
        self.reader.read_exact(&mut out)?;
        self.state.bytes_read = self.state.bytes_read.saturating_add(len as u64);
        Ok(out)
    }

    fn read_prefix(&mut self, offset: u64, declared: u64, cap: usize) -> Result<Vec<u8>> {
        let available = self.file_len.saturating_sub(offset).min(declared);
        self.read_at(offset, available.min(cap as u64) as usize)
    }
}

struct DocumentBuilder {
    document: MetadataDocument,
    normalized_index: HashMap<String, usize>,
    max_nodes: usize,
}

impl DocumentBuilder {
    fn new(source: PathBuf, container: ContainerKind, file_len: u64, max_nodes: usize) -> Self {
        Self {
            document: MetadataDocument {
                schema_version: PARSER_SCHEMA_VERSION,
                source,
                container,
                file_len,
                roots: Vec::new(),
                nodes: Vec::new(),
                normalized: Vec::new(),
                diagnostics: Vec::new(),
                completion: Completion::Complete,
                audio_mapping: None,
            },
            normalized_index: HashMap::new(),
            max_nodes,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_node(
        &mut self,
        parent: Option<NodeId>,
        name: impl Into<String>,
        offset: u64,
        header_size: u64,
        declared_size: u64,
        readable_size: u64,
        payload: PayloadRef,
        content: ContentKind,
        known: bool,
        status: ParseStatus,
        summary: Option<String>,
    ) -> Result<NodeId> {
        if self.document.nodes.len() >= self.max_nodes {
            self.document.completion = Completion::Partial;
            bail!("metadata node budget exceeded");
        }
        let name = name.into();
        let path = if let Some(parent_id) = parent {
            let parent_path = &self.document.nodes[parent_id as usize].path;
            format!("{parent_path}/{name}")
        } else {
            format!("/{name}")
        };
        let id = self.document.nodes.len() as NodeId;
        self.document.nodes.push(MetadataNode {
            id,
            parent,
            children: Vec::new(),
            name,
            path,
            offset,
            header_size,
            declared_size,
            readable_size,
            payload,
            content,
            known,
            status,
            summary,
        });
        if let Some(parent_id) = parent {
            self.document.nodes[parent_id as usize].children.push(id);
        } else {
            self.document.roots.push(id);
        }
        Ok(id)
    }

    fn add_scalar(
        &mut self,
        parent: NodeId,
        name: impl Into<String>,
        value: impl Into<String>,
        range: SourceRange,
    ) -> Result<NodeId> {
        let value = value.into();
        self.add_node(
            Some(parent),
            name,
            range.offset,
            0,
            range.length,
            range.length,
            PayloadRef {
                file_offset: range.offset,
                length: range.length,
            },
            ContentKind::Text,
            true,
            ParseStatus::Parsed,
            Some(value),
        )
    }

    fn diagnostic(
        &mut self,
        level: DiagnosticLevel,
        code: impl Into<String>,
        message: impl Into<String>,
        offset: Option<u64>,
        node: Option<NodeId>,
    ) {
        if level == DiagnosticLevel::Error && self.document.completion == Completion::Complete {
            self.document.completion = Completion::Partial;
        }
        self.document.diagnostics.push(MetadataDiagnostic {
            level,
            code: code.into(),
            message: message.into(),
            offset,
            node,
        });
    }

    fn normalize(
        &mut self,
        key: impl Into<String>,
        mut value: MetadataValue,
        source_node: NodeId,
        range: SourceRange,
        encoding: Option<String>,
        guessed_encoding: bool,
    ) {
        let key = key.into();
        let mut ucs_category = None;
        if key == "ucs.cat_id" {
            if let MetadataValue::Text(cat_id) = &mut value {
                let original = cat_id.clone();
                let resolved = ucs::resolve_cat_id(&original);
                if resolved.was_alias {
                    self.diagnostic(
                        DiagnosticLevel::Info,
                        "ucs.alias",
                        format!(
                            "UCS {} resolved to {} using UCS {}",
                            original, resolved.canonical, UCS_DATA_VERSION
                        ),
                        Some(range.offset),
                        Some(source_node),
                    );
                    *cat_id = resolved.canonical;
                }
                if !resolved.known {
                    self.diagnostic(
                        DiagnosticLevel::Warning,
                        "ucs.unknown_cat_id",
                        format!("{} is not a UCS {} CatID", original, UCS_DATA_VERSION),
                        Some(range.offset),
                        Some(source_node),
                    );
                }
                ucs_category = resolved.category;
            }
        }
        let source_path = self.document.nodes[source_node as usize].path.clone();
        let derived_encoding = encoding.clone();
        let sourced = SourcedValue {
            value,
            source_path,
            source_node,
            source_range: range,
            encoding,
            guessed_encoding,
        };
        if let Some(&idx) = self.normalized_index.get(&key) {
            let field = &mut self.document.normalized[idx];
            let first = field
                .values
                .first()
                .map(|v| v.value.display())
                .unwrap_or_default();
            field.conflict |= first != sourced.value.display();
            field.values.push(sourced);
        } else {
            let idx = self.document.normalized.len();
            self.normalized_index.insert(key.clone(), idx);
            self.document.normalized.push(NormalizedField {
                key,
                values: vec![sourced],
                resolved_index: 0,
                conflict: false,
            });
        }
        if let Some(category) = ucs_category {
            self.normalize(
                "ucs.category",
                MetadataValue::Text(category.category.to_string()),
                source_node,
                range,
                derived_encoding.clone(),
                guessed_encoding,
            );
            self.normalize(
                "ucs.subcategory",
                MetadataValue::Text(category.subcategory.to_string()),
                source_node,
                range,
                derived_encoding,
                guessed_encoding,
            );
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WaveFormat {
    format_tag: u16,
    format_subtype: Option<u16>,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    container_bits: u16,
    valid_bits: u16,
    pcm_or_float: bool,
}

pub fn inspect_path(path: &Path, options: InspectOptions) -> Result<MetadataDocument> {
    let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut file = BufReader::new(
        File::open(path).with_context(|| format!("open metadata source: {}", path.display()))?,
    );
    let len = file
        .get_ref()
        .metadata()
        .with_context(|| format!("stat metadata source: {}", path.display()))?
        .len();
    inspect_reader(&mut file, len, absolute, options)
}

pub fn inspect_reader<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
    source: PathBuf,
    options: InspectOptions,
) -> Result<MetadataDocument> {
    let mut io = ScanIo {
        reader,
        file_len,
        options: &options,
        state: BudgetState {
            started: Instant::now(),
            bytes_read: 0,
            partial_reason: None,
        },
    };
    let sniff_len = file_len.min(16) as usize;
    let sniff = io.read_at(0, sniff_len).unwrap_or_default();
    let kind = sniff_container(&sniff, &mut io).unwrap_or(ContainerKind::Unknown);
    let mut builder = DocumentBuilder::new(source, kind, file_len, options.budget.max_nodes);
    let scan_result = match kind {
        ContainerKind::RiffWave | ContainerKind::Rf64 | ContainerKind::Bw64 => {
            scan_riff(&mut io, &mut builder)
        }
        ContainerKind::Mp3 => scan_id3_mp3(&mut io, &mut builder),
        ContainerKind::Mp4 => scan_mp4(&mut io, &mut builder),
        ContainerKind::Flac => scan_flac(&mut io, &mut builder),
        ContainerKind::Ogg => scan_ogg(&mut io, &mut builder),
        ContainerKind::Aiff | ContainerKind::Aifc => scan_aiff(&mut io, &mut builder),
        ContainerKind::Unknown => scan_unknown(&mut io, &mut builder),
    };
    if let Err(err) = scan_result {
        let budget_limited = io.state.partial_reason.is_some();
        builder.document.completion = if builder.document.nodes.is_empty() {
            Completion::Failed
        } else {
            Completion::Partial
        };
        builder.diagnostic(
            if budget_limited {
                DiagnosticLevel::Warning
            } else {
                DiagnosticLevel::Error
            },
            if budget_limited {
                "scan.partial"
            } else {
                "scan.error"
            },
            io.state
                .partial_reason
                .clone()
                .unwrap_or_else(|| err.to_string()),
            None,
            None,
        );
    }
    Ok(builder.document)
}

fn sniff_container<R: Read + Seek>(sniff: &[u8], io: &mut ScanIo<'_, R>) -> Result<ContainerKind> {
    if sniff.len() >= 12 && &sniff[8..12] == b"WAVE" {
        return Ok(match &sniff[0..4] {
            b"RF64" => ContainerKind::Rf64,
            b"BW64" => ContainerKind::Bw64,
            b"RIFF" => ContainerKind::RiffWave,
            _ => ContainerKind::Unknown,
        });
    }
    if sniff.len() >= 12 && &sniff[0..4] == b"FORM" {
        return Ok(if &sniff[8..12] == b"AIFC" {
            ContainerKind::Aifc
        } else if &sniff[8..12] == b"AIFF" {
            ContainerKind::Aiff
        } else {
            ContainerKind::Unknown
        });
    }
    if sniff.starts_with(b"fLaC") {
        return Ok(ContainerKind::Flac);
    }
    if sniff.starts_with(b"OggS") {
        return Ok(ContainerKind::Ogg);
    }
    if sniff.starts_with(b"ID3") {
        return Ok(ContainerKind::Mp3);
    }
    if sniff.len() >= 8 && &sniff[4..8] == b"ftyp" {
        return Ok(ContainerKind::Mp4);
    }
    let probe = io.read_prefix(0, io.file_len, 4096)?;
    if probe
        .windows(2)
        .any(|w| w[0] == 0xff && (w[1] & 0xe0) == 0xe0)
    {
        return Ok(ContainerKind::Mp3);
    }
    Ok(ContainerKind::Unknown)
}

fn read_u16_le(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32_le(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64_le(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn read_u16_be(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32_be(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64_be(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        data.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn fourcc(data: &[u8]) -> String {
    let mut value = String::new();
    for byte in data {
        if byte.is_ascii_graphic() || *byte == b' ' {
            value.push(*byte as char);
        } else {
            value.push_str(&format!("%{byte:02X}"));
        }
    }
    value
}

fn decode_legacy_text(data: &[u8]) -> (String, String, bool) {
    let trimmed = data
        .split(|b| *b == 0)
        .next()
        .unwrap_or(data)
        .strip_suffix(b"\r\n")
        .or_else(|| data.strip_suffix(b"\n"))
        .unwrap_or_else(|| data.split(|b| *b == 0).next().unwrap_or(data));
    if trimmed.starts_with(&[0xef, 0xbb, 0xbf]) {
        return (
            String::from_utf8_lossy(&trimmed[3..]).trim().to_string(),
            "UTF-8 BOM".to_string(),
            false,
        );
    }
    if let Ok(value) = std::str::from_utf8(trimmed) {
        return (value.trim().to_string(), "UTF-8".to_string(), false);
    }
    let (sjis, _, sjis_errors) = SHIFT_JIS.decode(trimmed);
    let (cp, _, cp_errors) = WINDOWS_1252.decode(trimmed);
    let sjis_controls = sjis
        .chars()
        .filter(|c| c.is_control() && !c.is_whitespace())
        .count();
    let cp_controls = cp
        .chars()
        .filter(|c| c.is_control() && !c.is_whitespace())
        .count();
    if !sjis_errors && (cp_errors || sjis_controls <= cp_controls) {
        (sjis.trim().to_string(), "Shift_JIS".to_string(), true)
    } else {
        (cp.trim().to_string(), "Windows-1252".to_string(), true)
    }
}

fn scan_riff<R: Read + Seek>(io: &mut ScanIo<'_, R>, builder: &mut DocumentBuilder) -> Result<()> {
    if io.file_len < 12 {
        bail!("truncated RIFF header");
    }
    let header = io.read_at(0, 12)?;
    let root_name = fourcc(&header[0..4]);
    let declared_root = read_u32_le(&header, 4).unwrap_or(0) as u64;
    let root = builder.add_node(
        None,
        format!("{root_name}/WAVE"),
        0,
        12,
        declared_root,
        io.file_len.saturating_sub(12),
        PayloadRef {
            file_offset: 12,
            length: io.file_len.saturating_sub(12),
        },
        ContentKind::Container,
        true,
        ParseStatus::Parsed,
        Some(format!("{} bytes", io.file_len)),
    )?;

    let mut pos = 12u64;
    let mut ds64_data_size = None;
    let mut ds64_table: HashMap<[u8; 4], Vec<u64>> = HashMap::new();
    let mut fmt = WaveFormat::default();
    let mut pending_data: Option<(NodeId, u64, u64)> = None;
    while pos.saturating_add(8) <= io.file_len {
        io.check()?;
        let chunk_header = io.read_at(pos, 8)?;
        let id: [u8; 4] = chunk_header[0..4].try_into().unwrap();
        let id_text = fourcc(&id);
        let size32 = read_u32_le(&chunk_header, 4).unwrap_or(0);
        let payload_offset = pos + 8;
        let mut declared = size32 as u64;
        if size32 == u32::MAX {
            declared = if &id == b"data" {
                ds64_data_size.unwrap_or(declared)
            } else {
                ds64_table
                    .get_mut(&id)
                    .and_then(|sizes| {
                        if sizes.is_empty() {
                            None
                        } else {
                            Some(sizes.remove(0))
                        }
                    })
                    .unwrap_or(declared)
            };
        }
        let readable = declared.min(io.file_len.saturating_sub(payload_offset));
        let truncated = readable < declared;
        let content = match &id {
            b"data" => ContentKind::Audio,
            b"iXML" | b"axml" | b"XMP " => ContentKind::Xml,
            b"JUNK" | b"PAD " | b"FLLR" => ContentKind::Padding,
            b"LIST" | b"RIFF" => ContentKind::Container,
            b"ID3 " | b"id3 " => ContentKind::Container,
            _ => ContentKind::Binary,
        };
        let known = matches!(
            &id,
            b"fmt "
                | b"data"
                | b"ds64"
                | b"bext"
                | b"LIST"
                | b"cue "
                | b"smpl"
                | b"acid"
                | b"iXML"
                | b"axml"
                | b"chna"
                | b"ID3 "
                | b"id3 "
                | b"JUNK"
                | b"PAD "
                | b"FLLR"
                | b"PEAK"
                | b"levl"
                | b"XMP "
        );
        let node = builder.add_node(
            Some(root),
            id_text.clone(),
            pos,
            8,
            declared,
            readable,
            PayloadRef {
                file_offset: payload_offset,
                length: readable,
            },
            content,
            known,
            if truncated {
                ParseStatus::Truncated
            } else if matches!(content, ContentKind::Xml | ContentKind::Image) {
                ParseStatus::Deferred
            } else {
                ParseStatus::Parsed
            },
            Some(format!("{} bytes @ 0x{payload_offset:08X}", declared)),
        )?;
        if truncated {
            builder.diagnostic(
                DiagnosticLevel::Error,
                "riff.truncated_chunk",
                format!("{id_text} declares {declared} bytes but only {readable} are readable"),
                Some(pos),
                Some(node),
            );
        }
        match &id {
            b"ds64" => {
                let bytes = io.read_prefix(payload_offset, readable, 1024 * 1024)?;
                if bytes.len() >= 28 {
                    let riff_size = read_u64_le(&bytes, 0).unwrap_or(0);
                    ds64_data_size = read_u64_le(&bytes, 8);
                    let sample_count = read_u64_le(&bytes, 16).unwrap_or(0);
                    builder.document.nodes[node as usize].summary = Some(format!(
                        "riff={riff_size}, data={}, samples={sample_count}",
                        ds64_data_size.unwrap_or(0)
                    ));
                    let table_len = read_u32_le(&bytes, 24).unwrap_or(0) as usize;
                    for idx in 0..table_len {
                        let off = 28 + idx * 12;
                        let Some(id_bytes) = bytes.get(off..off + 4) else {
                            break;
                        };
                        let Some(size) = read_u64_le(&bytes, off + 4) else {
                            break;
                        };
                        ds64_table
                            .entry(id_bytes.try_into().unwrap())
                            .or_default()
                            .push(size);
                    }
                }
            }
            b"fmt " => {
                let bytes = io.read_prefix(payload_offset, readable, 64)?;
                fmt = parse_wave_format(&bytes);
                builder.document.nodes[node as usize].summary = Some(format!(
                    "{} Hz, {} ch, {} bit, block {}",
                    fmt.sample_rate, fmt.channels, fmt.container_bits, fmt.block_align
                ));
                add_wave_fmt_children(builder, node, payload_offset, fmt)?;
            }
            b"data" => {
                pending_data.get_or_insert((node, payload_offset, readable));
            }
            b"LIST" => scan_riff_list(io, builder, node, payload_offset, readable)?,
            b"bext" => decode_bext(io, builder, node, payload_offset, readable)?,
            b"cue " => decode_cue(io, builder, node, payload_offset, readable)?,
            b"smpl" => decode_smpl(io, builder, node, payload_offset, readable)?,
            b"acid" => decode_acid(io, builder, node, payload_offset, readable)?,
            b"iXML" | b"axml" | b"XMP " if io.options.decode_xml => {
                decode_xml_payload(io, builder, node, payload_offset, readable)?;
            }
            b"ID3 " | b"id3 " => {
                scan_id3_tag_at(io, builder, node, payload_offset, readable)?;
            }
            _ => {}
        }
        if truncated {
            break;
        }
        pos = payload_offset
            .checked_add(declared)
            .and_then(|v| v.checked_add(declared & 1))
            .ok_or_else(|| anyhow!("RIFF chunk offset overflow"))?;
    }

    if let Some((node_id, data_offset, data_length)) = pending_data {
        if fmt.pcm_or_float
            && fmt.block_align > 0
            && fmt.channels > 0
            && fmt.block_align % fmt.channels == 0
        {
            let format_name = match fmt.format_tag {
                1 => "PCM",
                3 => "IEEE Float",
                0xfffe => "WAVE_FORMAT_EXTENSIBLE",
                _ => "PCM-compatible",
            };
            builder.document.audio_mapping = Some(AudioByteMapping {
                node_id,
                data_offset,
                data_length,
                sample_rate: fmt.sample_rate,
                channels: fmt.channels,
                block_align: fmt.block_align,
                container_bits: fmt.container_bits,
                valid_bits: fmt.valid_bits.max(1),
                format_tag: fmt.format_tag,
                format_subtype: fmt.format_subtype,
                format_name: format_name.to_string(),
            });
        } else if fmt.pcm_or_float {
            builder.diagnostic(
                DiagnosticLevel::Warning,
                "wave.invalid_mapping",
                "PCM/Float fmt values cannot produce an exact channel byte mapping",
                Some(data_offset),
                Some(node_id),
            );
        }
    }
    Ok(())
}

fn parse_wave_format(bytes: &[u8]) -> WaveFormat {
    let format_tag = read_u16_le(bytes, 0).unwrap_or(0);
    let container_bits = read_u16_le(bytes, 14).unwrap_or(0);
    let mut valid_bits = container_bits;
    let mut pcm_or_float = matches!(format_tag, 1 | 3);
    let mut format_subtype = None;
    if format_tag == 0xfffe && bytes.len() >= 40 {
        valid_bits = read_u16_le(bytes, 18).unwrap_or(container_bits);
        let subtype = read_u16_le(bytes, 24).unwrap_or(0);
        format_subtype = Some(subtype);
        pcm_or_float = matches!(subtype, 1 | 3);
    }
    WaveFormat {
        format_tag,
        format_subtype,
        channels: read_u16_le(bytes, 2).unwrap_or(0),
        sample_rate: read_u32_le(bytes, 4).unwrap_or(0),
        block_align: read_u16_le(bytes, 12).unwrap_or(0),
        container_bits,
        valid_bits,
        pcm_or_float,
    }
}

fn add_wave_fmt_children(
    builder: &mut DocumentBuilder,
    parent: NodeId,
    offset: u64,
    fmt: WaveFormat,
) -> Result<()> {
    for (name, value, rel, len) in [
        ("FormatTag", fmt.format_tag.to_string(), 0, 2),
        ("Channels", fmt.channels.to_string(), 2, 2),
        ("SampleRate", fmt.sample_rate.to_string(), 4, 4),
        ("BlockAlign", fmt.block_align.to_string(), 12, 2),
        ("ContainerBits", fmt.container_bits.to_string(), 14, 2),
        ("ValidBits", fmt.valid_bits.to_string(), 18, 2),
    ] {
        builder.add_scalar(
            parent,
            name,
            value,
            SourceRange {
                offset: offset + rel,
                length: len,
            },
        )?;
    }
    Ok(())
}

fn scan_riff_list<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    parent: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    if length < 4 {
        return Ok(());
    }
    let list_type = io.read_at(offset, 4)?;
    let type_name = fourcc(&list_type);
    builder.document.nodes[parent as usize].summary = Some(format!("LIST/{type_name}"));
    let type_node = builder.add_node(
        Some(parent),
        type_name.clone(),
        offset,
        4,
        length - 4,
        length - 4,
        PayloadRef {
            file_offset: offset + 4,
            length: length - 4,
        },
        ContentKind::Container,
        true,
        ParseStatus::Parsed,
        None,
    )?;
    let end = offset.saturating_add(length);
    let mut pos = offset + 4;
    while pos.saturating_add(8) <= end && pos.saturating_add(8) <= io.file_len {
        let header = io.read_at(pos, 8)?;
        let id = fourcc(&header[0..4]);
        let declared = read_u32_le(&header, 4).unwrap_or(0) as u64;
        let payload_offset = pos + 8;
        let readable = declared.min(end.saturating_sub(payload_offset));
        let is_text = type_name == "INFO" || matches!(id.as_str(), "labl" | "note" | "ltxt");
        let child = builder.add_node(
            Some(type_node),
            id.clone(),
            pos,
            8,
            declared,
            readable,
            PayloadRef {
                file_offset: payload_offset,
                length: readable,
            },
            if is_text {
                ContentKind::Text
            } else {
                ContentKind::Binary
            },
            true,
            if readable < declared {
                ParseStatus::Truncated
            } else {
                ParseStatus::Parsed
            },
            None,
        )?;
        if is_text && readable > 0 {
            let prefix_skip = if matches!(id.as_str(), "labl" | "note" | "ltxt") && readable >= 4 {
                4
            } else {
                0
            };
            let text_bytes = io.read_prefix(
                payload_offset + prefix_skip,
                readable - prefix_skip,
                1024 * 1024,
            )?;
            let (text, encoding, guessed) = decode_legacy_text(&text_bytes);
            builder.document.nodes[child as usize].summary = Some(text.clone());
            if type_name == "INFO" {
                if let Some(key) = normalize_info_key(&id) {
                    builder.normalize(
                        key,
                        MetadataValue::Text(text),
                        child,
                        SourceRange {
                            offset: payload_offset + prefix_skip,
                            length: readable - prefix_skip,
                        },
                        Some(encoding),
                        guessed,
                    );
                }
            }
        }
        if readable < declared {
            break;
        }
        pos = payload_offset
            .checked_add(declared)
            .and_then(|v| v.checked_add(declared & 1))
            .ok_or_else(|| anyhow!("LIST subchunk overflow"))?;
    }
    Ok(())
}

fn normalize_info_key(id: &str) -> Option<&'static str> {
    match id {
        "INAM" => Some("title"),
        "IART" => Some("artist"),
        "ICMT" => Some("comment"),
        "ICRD" => Some("date"),
        "IGNR" => Some("genre"),
        "IPRD" => Some("album"),
        "ISFT" => Some("software"),
        _ => None,
    }
}

fn decode_bext<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    node: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    let bytes = io.read_prefix(offset, length, 1024)?;
    for (name, start, len, normalized) in [
        ("Description", 0usize, 256usize, Some("description")),
        ("Originator", 256, 32, Some("originator")),
        ("OriginatorReference", 288, 32, Some("originator_reference")),
        ("OriginationDate", 320, 10, Some("date")),
        ("OriginationTime", 330, 8, Some("time")),
    ] {
        if let Some(raw) = bytes.get(start..start + len) {
            let (text, enc, guessed) = decode_legacy_text(raw);
            let child = builder.add_scalar(
                node,
                name,
                text.clone(),
                SourceRange {
                    offset: offset + start as u64,
                    length: len as u64,
                },
            )?;
            if let Some(key) = normalized {
                builder.normalize(
                    format!("bwf.{key}"),
                    MetadataValue::Text(text),
                    child,
                    SourceRange {
                        offset: offset + start as u64,
                        length: len as u64,
                    },
                    Some(enc),
                    guessed,
                );
            }
        }
    }
    if bytes.len() >= 346 {
        let low = read_u32_le(&bytes, 338).unwrap_or(0) as u64;
        let high = read_u32_le(&bytes, 342).unwrap_or(0) as u64;
        builder.add_scalar(
            node,
            "TimeReference",
            ((high << 32) | low).to_string(),
            SourceRange {
                offset: offset + 338,
                length: 8,
            },
        )?;
    }
    if bytes.len() >= 348 {
        builder.add_scalar(
            node,
            "Version",
            read_u16_le(&bytes, 346).unwrap_or(0).to_string(),
            SourceRange {
                offset: offset + 346,
                length: 2,
            },
        )?;
    }
    Ok(())
}

fn decode_cue<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    node: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    let bytes = io.read_prefix(offset, length, 4 + 24 * 100_000)?;
    let count = read_u32_le(&bytes, 0).unwrap_or(0) as usize;
    builder.document.nodes[node as usize].summary = Some(format!("{count} cue points"));
    for idx in 0..count.min(100_000) {
        let pos = 4 + idx * 24;
        let Some(raw) = bytes.get(pos..pos + 24) else {
            break;
        };
        let id = read_u32_le(raw, 0).unwrap_or(0);
        let sample = read_u32_le(raw, 20).unwrap_or(0);
        builder.add_scalar(
            node,
            format!("Cue {id}"),
            format!("sample {sample}"),
            SourceRange {
                offset: offset + pos as u64,
                length: 24,
            },
        )?;
    }
    Ok(())
}

fn decode_smpl<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    node: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    let bytes = io.read_prefix(offset, length, 36 + 24 * 65_536)?;
    if bytes.len() < 36 {
        return Ok(());
    }
    let unity = read_u32_le(&bytes, 12).unwrap_or(0);
    let loops = read_u32_le(&bytes, 28).unwrap_or(0) as usize;
    builder.document.nodes[node as usize].summary =
        Some(format!("{loops} loops, unity note {unity}"));
    builder.add_scalar(
        node,
        "MIDIUnityNote",
        unity.to_string(),
        SourceRange {
            offset: offset + 12,
            length: 4,
        },
    )?;
    for idx in 0..loops.min(65_536) {
        let pos = 36 + idx * 24;
        let Some(raw) = bytes.get(pos..pos + 24) else {
            break;
        };
        let start = read_u32_le(raw, 8).unwrap_or(0);
        let end = read_u32_le(raw, 12).unwrap_or(0);
        builder.add_scalar(
            node,
            format!("Loop {}", idx + 1),
            format!("{start}..{end}"),
            SourceRange {
                offset: offset + pos as u64,
                length: 24,
            },
        )?;
    }
    Ok(())
}

fn decode_acid<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    node: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    let bytes = io.read_prefix(offset, length, 64)?;
    if bytes.len() >= 24 {
        let beats = read_u32_le(&bytes, 12).unwrap_or(0);
        let tempo = f32::from_le_bytes(bytes[20..24].try_into().unwrap());
        builder.document.nodes[node as usize].summary =
            Some(format!("{tempo:.3} BPM, {beats} beats"));
        let bpm_node = builder.add_scalar(
            node,
            "Tempo",
            format!("{tempo:.3}"),
            SourceRange {
                offset: offset + 20,
                length: 4,
            },
        )?;
        builder.normalize(
            "bpm",
            MetadataValue::Float(tempo as f64),
            bpm_node,
            SourceRange {
                offset: offset + 20,
                length: 4,
            },
            None,
            false,
        );
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct XmlStackEntry {
    node: NodeId,
    name: String,
    text: String,
    text_truncated: bool,
    content_start: u64,
    in_aswg: bool,
}

fn decode_xml_payload<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    parent: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    if length > MAX_XML_BYTES {
        builder.document.completion = Completion::Partial;
        builder.diagnostic(
            DiagnosticLevel::Warning,
            "xml.too_large",
            format!("XML payload exceeds {} bytes", MAX_XML_BYTES),
            Some(offset),
            Some(parent),
        );
        return Ok(());
    }
    let bytes = io.read_at(offset, length as usize)?;
    let mut reader = XmlReader::from_reader(Cursor::new(&bytes));
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut stack: Vec<XmlStackEntry> = Vec::new();
    let mut xml_nodes = 0usize;
    loop {
        let event_start = reader.buffer_position() as u64;
        match reader.read_resolved_event_into(&mut buf) {
            Ok((namespace, Event::Start(event))) => {
                let namespace = match namespace {
                    ResolveResult::Bound(namespace) => {
                        Some(String::from_utf8_lossy(namespace.as_ref()).into_owned())
                    }
                    ResolveResult::Unbound => None,
                    ResolveResult::Unknown(prefix) => {
                        Some(format!("<unbound:{}>", String::from_utf8_lossy(&prefix)))
                    }
                };
                if stack.len() >= MAX_XML_DEPTH || xml_nodes >= MAX_XML_NODES {
                    builder.document.completion = Completion::Partial;
                    builder.diagnostic(
                        DiagnosticLevel::Warning,
                        "xml.limit",
                        "XML depth or node limit exceeded",
                        Some(offset + event_start),
                        Some(parent),
                    );
                    break;
                }
                let name = String::from_utf8_lossy(event.name().as_ref()).to_string();
                let parent_id = stack.last().map(|v| v.node).unwrap_or(parent);
                let local = name.rsplit(':').next().unwrap_or(&name);
                let in_aswg = local.eq_ignore_ascii_case("ASWG")
                    || stack.last().is_some_and(|entry| entry.in_aswg);
                let node = builder.add_node(
                    Some(parent_id),
                    name.clone(),
                    offset + event_start,
                    0,
                    0,
                    0,
                    PayloadRef {
                        file_offset: offset + event_start,
                        length: 0,
                    },
                    ContentKind::Xml,
                    true,
                    ParseStatus::Parsed,
                    None,
                )?;
                xml_nodes += 1;
                if local.eq_ignore_ascii_case("ASWG")
                    && namespace.as_deref() != Some("http://sce/aswg/ixml")
                {
                    builder.diagnostic(
                        DiagnosticLevel::Warning,
                        "aswg.namespace",
                        match namespace {
                            Some(namespace) => format!(
                                "ASWG 1.1 namespace is {namespace}, expected http://sce/aswg/ixml"
                            ),
                            None => "ASWG 1.1 namespace is missing; expected http://sce/aswg/ixml"
                                .to_string(),
                        },
                        Some(offset + event_start),
                        Some(node),
                    );
                }
                for attribute in event.attributes().with_checks(false).flatten() {
                    let attr_name = format!("@{}", String::from_utf8_lossy(attribute.key.as_ref()));
                    let attr_value = attribute
                        .decode_and_unescape_value(reader.decoder())
                        .map(|value| value.into_owned())
                        .unwrap_or_else(|_| {
                            String::from_utf8_lossy(attribute.value.as_ref()).to_string()
                        });
                    builder.add_scalar(
                        node,
                        attr_name,
                        attr_value,
                        SourceRange {
                            offset: offset + event_start,
                            length: 0,
                        },
                    )?;
                }
                stack.push(XmlStackEntry {
                    node,
                    name,
                    text: String::new(),
                    text_truncated: false,
                    content_start: offset + reader.buffer_position() as u64,
                    in_aswg,
                });
            }
            Ok((_, Event::Empty(event))) => {
                let name = String::from_utf8_lossy(event.name().as_ref()).to_string();
                let parent_id = stack.last().map(|v| v.node).unwrap_or(parent);
                builder.add_node(
                    Some(parent_id),
                    name,
                    offset + event_start,
                    reader.buffer_position() as u64 - event_start,
                    0,
                    0,
                    PayloadRef {
                        file_offset: offset + event_start,
                        length: reader.buffer_position() as u64 - event_start,
                    },
                    ContentKind::Xml,
                    true,
                    ParseStatus::Parsed,
                    None,
                )?;
                xml_nodes += 1;
            }
            Ok((_, Event::Text(text))) => {
                if let Some(current) = stack.last_mut() {
                    let decoded = text
                        .xml_content()
                        .map(|value| value.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(text.as_ref()).to_string());
                    if current.text.len().saturating_add(decoded.len()) <= MAX_XML_TEXT_BYTES {
                        current.text.push_str(&decoded);
                    } else {
                        current.text_truncated = true;
                    }
                }
            }
            Ok((_, Event::CData(text))) => {
                if let Some(current) = stack.last_mut() {
                    let decoded = text
                        .xml_content()
                        .map(|value| value.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(text.as_ref()).to_string());
                    if current.text.len().saturating_add(decoded.len()) <= MAX_XML_TEXT_BYTES {
                        current.text.push_str(&decoded);
                    } else {
                        current.text_truncated = true;
                    }
                }
            }
            Ok((_, Event::End(_))) => {
                let Some(entry) = stack.pop() else {
                    continue;
                };
                if entry.text_truncated {
                    builder.document.completion = Completion::Partial;
                    builder.diagnostic(
                        DiagnosticLevel::Warning,
                        "xml.text_limit",
                        "XML text limit exceeded",
                        Some(entry.content_start),
                        Some(entry.node),
                    );
                }
                let text = entry.text.trim().to_string();
                if !text.is_empty() {
                    builder.document.nodes[entry.node as usize].summary = Some(text.clone());
                    let range = SourceRange {
                        offset: entry.content_start,
                        length: reader
                            .buffer_position()
                            .saturating_sub(entry.content_start.saturating_sub(offset))
                            as u64,
                    };
                    if entry.in_aswg {
                        validate_aswg_value(builder, entry.node, &entry.name, &text, range);
                    }
                    if let Some(key) = normalize_xml_key(&entry.name).or_else(|| {
                        entry
                            .in_aswg
                            .then(|| format!("aswg.{}", snake_case_xml_name(&entry.name)))
                    }) {
                        let value = metadata_value_for_xml(&entry.name, text);
                        builder.normalize(
                            key,
                            value,
                            entry.node,
                            range,
                            Some("UTF-8/XML".to_string()),
                            false,
                        );
                    }
                }
            }
            Ok((_, Event::DocType(_))) => {
                builder.diagnostic(
                    DiagnosticLevel::Error,
                    "xml.doctype_rejected",
                    "DOCTYPE/DTD is not allowed in metadata XML",
                    Some(offset + event_start),
                    Some(parent),
                );
                break;
            }
            Ok((_, Event::Eof)) => break,
            Ok(_) => {}
            Err(err) => {
                builder.document.completion = Completion::Partial;
                builder.diagnostic(
                    DiagnosticLevel::Warning,
                    "xml.parse_error",
                    err.to_string(),
                    Some(offset + reader.error_position() as u64),
                    Some(parent),
                );
                break;
            }
        }
        buf.clear();
    }
    builder.document.nodes[parent as usize].status = ParseStatus::Parsed;
    Ok(())
}

fn metadata_value_for_xml(name: &str, text: String) -> MetadataValue {
    let compact = compact_xml_name(name);
    if matches!(
        compact.as_str(),
        "isgenerated"
            | "isdesigned"
            | "isunion"
            | "isformal"
            | "issource"
            | "isloop"
            | "isfinal"
            | "isost"
            | "iscinematic"
            | "islicensed"
            | "isdiegetic"
    ) {
        if let Ok(value) = text.parse::<bool>() {
            return MetadataValue::Bool(value);
        }
    }
    if matches!(
        compact.as_str(),
        "rmspower" | "loudness" | "loudnessrange" | "maxpeak" | "zerocrossrate" | "papr" | "tempo"
    ) {
        if let Ok(value) = text.parse::<f64>() {
            return MetadataValue::Float(value);
        }
    }
    if compact == "projection" {
        if let Ok(value) = text.parse::<u64>() {
            return MetadataValue::Unsigned(value);
        }
    }
    MetadataValue::Text(text)
}

fn compact_xml_name(name: &str) -> String {
    name.rsplit(':')
        .next()
        .unwrap_or(name)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn snake_case_xml_name(name: &str) -> String {
    let local = name.rsplit(':').next().unwrap_or(name);
    let mut output = String::new();
    for character in local.chars() {
        if character.is_ascii_uppercase() {
            if !output.is_empty() {
                output.push('_');
            }
            output.push(character.to_ascii_lowercase());
        } else if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
        } else if !output.ends_with('_') {
            output.push('_');
        }
    }
    output.trim_matches('_').to_string()
}

fn validate_aswg_value(
    builder: &mut DocumentBuilder,
    node: NodeId,
    name: &str,
    value: &str,
    range: SourceRange,
) {
    let compact = compact_xml_name(name);
    let valid = match compact.as_str() {
        "isgenerated" | "isdesigned" | "isunion" | "issource" | "isloop" | "isfinal" | "isost"
        | "iscinematic" | "islicensed" | "isdiegetic" => value.parse::<bool>().is_ok(),
        "rmspower" | "loudness" | "maxpeak" | "papr" => value.parse::<f64>().is_ok(),
        "loudnessrange" | "zerocrossrate" => value.parse::<f64>().is_ok_and(|number| number >= 0.0),
        "projection" => value
            .parse::<u64>()
            .is_ok_and(|number| number <= i32::MAX as u64),
        "originationdate" => {
            let bytes = value.as_bytes();
            bytes.len() == 10
                && bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
        }
        "language" | "devlanguage" => {
            value.is_empty()
                || (value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_alphabetic()))
        }
        "category" | "subcategory" | "catid" | "usercategory" | "userdata" | "vendorcategory"
        | "fxname" | "creatorid" => !value.contains(['_', '-']),
        "ambisonicchnorder" => ["Unknown", "Fuma", "ACN"].contains(&value),
        "ambisonicnorm" => ["Unknown", "SN3D", "MAXN", "N3D"].contains(&value),
        "channelconfig" => [
            "Unknown",
            "Mono",
            "Stereo",
            "LCR",
            "Quad",
            "5.0",
            "5.1",
            "7.0",
            "7.1",
            "7.1.2",
            "7.1.4",
            "12.2",
            "Ambisonic",
        ]
        .contains(&value),
        "micconfig" => ["Unknown", "Mono", "AB", "XY", "ORTF", "MS", "other"].contains(&value),
        "contenttype" => ["sfx", "music", "dialog", "haptic", "impulse", "mixed"]
            .contains(&value.to_ascii_lowercase().as_str()),
        "state" => ["mastered", "processed", "raw", "placeholder"]
            .contains(&value.to_ascii_lowercase().as_str()),
        _ => true,
    };
    if !valid {
        builder.diagnostic(
            DiagnosticLevel::Warning,
            "aswg.type",
            format!(
                "{} does not satisfy the embedded ASWG 1.1 rule",
                name.rsplit(':').next().unwrap_or(name)
            ),
            Some(range.offset),
            Some(node),
        );
    }
}

fn normalize_xml_key(name: &str) -> Option<String> {
    let compact = compact_xml_name(name);
    match compact.as_str() {
        "catid" | "categoryid" => Some("ucs.cat_id".to_string()),
        "category" => Some("ucs.category".to_string()),
        "subcategory" => Some("ucs.subcategory".to_string()),
        "fxname" | "fxnamefull" => Some("ucs.fx_name".to_string()),
        "creatorid" => Some("ucs.creator_id".to_string()),
        "sourceid" => Some("ucs.source_id".to_string()),
        "project" => Some("ixml.project".to_string()),
        "scene" => Some("ixml.scene".to_string()),
        "take" => Some("ixml.take".to_string()),
        "trackname" => Some("ixml.track_name".to_string()),
        _ => None,
    }
}

fn scan_aiff<R: Read + Seek>(io: &mut ScanIo<'_, R>, builder: &mut DocumentBuilder) -> Result<()> {
    if io.file_len < 12 {
        bail!("truncated AIFF header");
    }
    let header = io.read_at(0, 12)?;
    let form = fourcc(&header[8..12]);
    let root = builder.add_node(
        None,
        format!("FORM/{form}"),
        0,
        12,
        read_u32_be(&header, 4).unwrap_or(0) as u64,
        io.file_len.saturating_sub(12),
        PayloadRef {
            file_offset: 12,
            length: io.file_len.saturating_sub(12),
        },
        ContentKind::Container,
        true,
        ParseStatus::Parsed,
        Some(format!("{} bytes", io.file_len)),
    )?;
    let mut pos = 12u64;
    while pos.saturating_add(8) <= io.file_len {
        let header = io.read_at(pos, 8)?;
        let id = fourcc(&header[0..4]);
        let declared = read_u32_be(&header, 4).unwrap_or(0) as u64;
        let payload_offset = pos + 8;
        let readable = declared.min(io.file_len.saturating_sub(payload_offset));
        let content = match id.as_str() {
            "SSND" => ContentKind::Audio,
            "NAME" | "AUTH" | "ANNO" | "(c) " => ContentKind::Text,
            "MARK" | "INST" => ContentKind::Container,
            _ => ContentKind::Binary,
        };
        let known = matches!(
            id.as_str(),
            "COMM" | "SSND" | "MARK" | "INST" | "NAME" | "AUTH" | "ANNO" | "(c) "
        );
        let node = builder.add_node(
            Some(root),
            id.clone(),
            pos,
            8,
            declared,
            readable,
            PayloadRef {
                file_offset: payload_offset,
                length: readable,
            },
            content,
            known,
            if readable < declared {
                ParseStatus::Truncated
            } else {
                ParseStatus::Parsed
            },
            Some(format!("{} bytes @ 0x{payload_offset:08X}", declared)),
        )?;
        match id.as_str() {
            "COMM" => {
                let bytes = io.read_prefix(payload_offset, readable, 64)?;
                if bytes.len() >= 18 {
                    let channels = read_u16_be(&bytes, 0).unwrap_or(0);
                    let frames = read_u32_be(&bytes, 2).unwrap_or(0);
                    let bits = read_u16_be(&bytes, 6).unwrap_or(0);
                    let rate = decode_extended_80(&bytes[8..18]).unwrap_or(0.0);
                    builder.document.nodes[node as usize].summary = Some(format!(
                        "{rate:.0} Hz, {channels} ch, {bits} bit, {frames} frames"
                    ));
                }
            }
            "NAME" | "AUTH" | "ANNO" | "(c) " => {
                let bytes = io.read_prefix(payload_offset, readable, 1024 * 1024)?;
                let (text, encoding, guessed) = decode_legacy_text(&bytes);
                builder.document.nodes[node as usize].summary = Some(text.clone());
                let normalized = match id.as_str() {
                    "NAME" => Some("title"),
                    "AUTH" => Some("artist"),
                    "ANNO" => Some("comment"),
                    _ => None,
                };
                if let Some(key) = normalized {
                    builder.normalize(
                        key,
                        MetadataValue::Text(text),
                        node,
                        SourceRange {
                            offset: payload_offset,
                            length: readable,
                        },
                        Some(encoding),
                        guessed,
                    );
                }
            }
            "MARK" => decode_aiff_mark(io, builder, node, payload_offset, readable)?,
            "INST" => decode_aiff_inst(io, builder, node, payload_offset, readable)?,
            _ => {}
        }
        if readable < declared {
            break;
        }
        pos = payload_offset
            .checked_add(declared)
            .and_then(|v| v.checked_add(declared & 1))
            .ok_or_else(|| anyhow!("AIFF chunk overflow"))?;
    }
    Ok(())
}

fn decode_extended_80(data: &[u8]) -> Option<f64> {
    if data.len() < 10 {
        return None;
    }
    let exponent = u16::from_be_bytes([data[0], data[1]]);
    if exponent == 0 {
        return Some(0.0);
    }
    let sign = if exponent & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = (exponent & 0x7fff) as i32 - 16383;
    let mantissa = u64::from_be_bytes(data[2..10].try_into().ok()?);
    Some(sign * (mantissa as f64 / (1u64 << 63) as f64) * 2f64.powi(exp))
}

fn decode_aiff_mark<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    parent: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    let bytes = io.read_prefix(offset, length, 8 * 1024 * 1024)?;
    let count = read_u16_be(&bytes, 0).unwrap_or(0) as usize;
    builder.document.nodes[parent as usize].summary = Some(format!("{count} markers"));
    let mut pos = 2usize;
    for _ in 0..count {
        if pos + 7 > bytes.len() {
            break;
        }
        let id = read_u16_be(&bytes, pos).unwrap_or(0);
        let frame = read_u32_be(&bytes, pos + 2).unwrap_or(0);
        let name_len = bytes[pos + 6] as usize;
        if pos + 7 + name_len > bytes.len() {
            break;
        }
        let name = String::from_utf8_lossy(&bytes[pos + 7..pos + 7 + name_len]).to_string();
        let entry_len = 7 + name_len + ((name_len + 1) & 1);
        builder.add_scalar(
            parent,
            format!("Marker {id}"),
            format!("{frame}: {name}"),
            SourceRange {
                offset: offset + pos as u64,
                length: entry_len as u64,
            },
        )?;
        pos += entry_len;
    }
    Ok(())
}

fn decode_aiff_inst<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    parent: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    let bytes = io.read_prefix(offset, length, 32)?;
    if bytes.len() >= 20 {
        let base_note = bytes[0];
        let sustain_mode = read_u16_be(&bytes, 8).unwrap_or(0);
        let sustain_begin = read_u16_be(&bytes, 10).unwrap_or(0);
        let sustain_end = read_u16_be(&bytes, 12).unwrap_or(0);
        builder.document.nodes[parent as usize].summary = Some(format!(
            "base note {base_note}, sustain {sustain_mode} ({sustain_begin}..{sustain_end})"
        ));
    }
    Ok(())
}

fn scan_flac<R: Read + Seek>(io: &mut ScanIo<'_, R>, builder: &mut DocumentBuilder) -> Result<()> {
    let root = builder.add_node(
        None,
        "FLAC",
        0,
        4,
        io.file_len.saturating_sub(4),
        io.file_len.saturating_sub(4),
        PayloadRef {
            file_offset: 4,
            length: io.file_len.saturating_sub(4),
        },
        ContentKind::Container,
        true,
        ParseStatus::Parsed,
        Some(format!("{} bytes", io.file_len)),
    )?;
    let mut pos = 4u64;
    let mut last = false;
    while !last && pos.saturating_add(4) <= io.file_len {
        let header = io.read_at(pos, 4)?;
        last = header[0] & 0x80 != 0;
        let block_type = header[0] & 0x7f;
        let declared = ((header[1] as u64) << 16) | ((header[2] as u64) << 8) | header[3] as u64;
        let payload_offset = pos + 4;
        let readable = declared.min(io.file_len.saturating_sub(payload_offset));
        let name = match block_type {
            0 => "STREAMINFO",
            1 => "PADDING",
            2 => "APPLICATION",
            3 => "SEEKTABLE",
            4 => "VORBIS_COMMENT",
            5 => "CUESHEET",
            6 => "PICTURE",
            _ => "UNKNOWN",
        };
        let content = match block_type {
            1 => ContentKind::Padding,
            4 | 6 => ContentKind::Container,
            _ => ContentKind::Binary,
        };
        let node = builder.add_node(
            Some(root),
            name,
            pos,
            4,
            declared,
            readable,
            PayloadRef {
                file_offset: payload_offset,
                length: readable,
            },
            content,
            block_type <= 6,
            if readable < declared {
                ParseStatus::Truncated
            } else {
                ParseStatus::Parsed
            },
            Some(format!("type {block_type}, {declared} bytes")),
        )?;
        match block_type {
            0 => {
                let bytes = io.read_prefix(payload_offset, readable, 34)?;
                if bytes.len() >= 18 {
                    let packed = read_u64_be(&bytes, 10).unwrap_or(0);
                    let sample_rate = ((packed >> 44) & 0x000f_ffff) as u32;
                    let channels = ((packed >> 41) & 0x7) as u16 + 1;
                    let bits = ((packed >> 36) & 0x1f) as u16 + 1;
                    let samples = packed & 0x0000_000f_ffff_ffff;
                    builder.document.nodes[node as usize].summary = Some(format!(
                        "{sample_rate} Hz, {channels} ch, {bits} bit, {samples} samples"
                    ));
                }
            }
            4 => decode_vorbis_comment(io, builder, node, payload_offset, readable, false)?,
            6 => decode_flac_picture(io, builder, node, payload_offset, readable)?,
            _ => {}
        }
        if readable < declared {
            break;
        }
        pos = payload_offset
            .checked_add(declared)
            .ok_or_else(|| anyhow!("FLAC block overflow"))?;
    }
    if pos < io.file_len {
        builder.add_node(
            Some(root),
            "AUDIO_FRAMES",
            pos,
            0,
            io.file_len - pos,
            io.file_len - pos,
            PayloadRef {
                file_offset: pos,
                length: io.file_len - pos,
            },
            ContentKind::Audio,
            true,
            ParseStatus::Deferred,
            Some(format!("{} bytes", io.file_len - pos)),
        )?;
    }
    Ok(())
}

fn decode_vorbis_comment<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    parent: NodeId,
    offset: u64,
    length: u64,
    packet_prefix: bool,
) -> Result<()> {
    let bytes = io.read_prefix(offset, length, MAX_XML_BYTES as usize)?;
    let mut pos = if packet_prefix { 7usize } else { 0usize };
    if bytes.len() < pos + 4 {
        return Ok(());
    }
    let vendor_len = read_u32_le(&bytes, pos).unwrap_or(0) as usize;
    pos += 4;
    if pos + vendor_len > bytes.len() {
        return Ok(());
    }
    let vendor = String::from_utf8_lossy(&bytes[pos..pos + vendor_len]).to_string();
    builder.add_scalar(
        parent,
        "Vendor",
        vendor,
        SourceRange {
            offset: offset + pos as u64,
            length: vendor_len as u64,
        },
    )?;
    pos += vendor_len;
    let count = read_u32_le(&bytes, pos).unwrap_or(0) as usize;
    pos += 4;
    builder.document.nodes[parent as usize].summary = Some(format!("{count} comments"));
    for _ in 0..count.min(100_000) {
        let Some(item_len) = read_u32_le(&bytes, pos).map(|v| v as usize) else {
            break;
        };
        pos += 4;
        if pos + item_len > bytes.len() {
            break;
        }
        let raw = &bytes[pos..pos + item_len];
        let text = String::from_utf8_lossy(raw).to_string();
        let (key, value) = text.split_once('=').unwrap_or((&text, ""));
        let child = builder.add_scalar(
            parent,
            key,
            value,
            SourceRange {
                offset: offset + pos as u64,
                length: item_len as u64,
            },
        )?;
        if let Some(normalized) = normalize_comment_key(key) {
            builder.normalize(
                normalized,
                MetadataValue::Text(value.to_string()),
                child,
                SourceRange {
                    offset: offset + pos as u64,
                    length: item_len as u64,
                },
                Some("UTF-8".to_string()),
                false,
            );
        }
        pos += item_len;
    }
    Ok(())
}

fn normalize_comment_key(key: &str) -> Option<String> {
    match key.trim().to_ascii_uppercase().as_str() {
        "TITLE" => Some("title".to_string()),
        "ARTIST" => Some("artist".to_string()),
        "ALBUM" => Some("album".to_string()),
        "COMMENT" | "DESCRIPTION" => Some("comment".to_string()),
        "DATE" => Some("date".to_string()),
        "GENRE" => Some("genre".to_string()),
        "BPM" | "TEMPO" => Some("bpm".to_string()),
        "CATID" | "CATEGORY_ID" => Some("ucs.cat_id".to_string()),
        "CATEGORY" => Some("ucs.category".to_string()),
        "SUBCATEGORY" => Some("ucs.subcategory".to_string()),
        "FXNAME" | "FX_NAME" => Some("ucs.fx_name".to_string()),
        _ => None,
    }
}

fn decode_flac_picture<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    node: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    let bytes = io.read_prefix(offset, length, 1024 * 1024)?;
    if bytes.len() < 8 {
        return Ok(());
    }
    let picture_type = read_u32_be(&bytes, 0).unwrap_or(0);
    let mime_len = read_u32_be(&bytes, 4).unwrap_or(0) as usize;
    if bytes.len() < 8 + mime_len {
        return Ok(());
    }
    let mime = String::from_utf8_lossy(&bytes[8..8 + mime_len]);
    let mut pos = 8 + mime_len;
    let Some(description_len) = read_u32_be(&bytes, pos).map(|value| value as usize) else {
        return Ok(());
    };
    pos += 4;
    if pos.saturating_add(description_len).saturating_add(20) > bytes.len() {
        return Ok(());
    }
    pos += description_len;
    let width = read_u32_be(&bytes, pos).unwrap_or(0);
    let height = read_u32_be(&bytes, pos + 4).unwrap_or(0);
    pos += 16;
    let data_len = read_u32_be(&bytes, pos).unwrap_or(0) as u64;
    pos += 4;
    let readable = data_len.min(length.saturating_sub(pos as u64));
    builder.document.nodes[node as usize].summary = Some(format!(
        "picture type {picture_type}, {mime}, {width}x{height}, {data_len} bytes"
    ));
    builder.add_node(
        Some(node),
        "ImageData",
        offset + pos as u64,
        0,
        data_len,
        readable,
        PayloadRef {
            file_offset: offset + pos as u64,
            length: readable,
        },
        ContentKind::Image,
        true,
        if readable < data_len {
            ParseStatus::Truncated
        } else {
            ParseStatus::Parsed
        },
        Some(format!("{mime}, {width}x{height}")),
    )?;
    Ok(())
}

const MP4_CONTAINERS: &[[u8; 4]] = &[
    *b"moov", *b"trak", *b"mdia", *b"minf", *b"stbl", *b"edts", *b"udta", *b"meta", *b"ilst",
    *b"moof", *b"traf", *b"mvex", *b"dinf", *b"tref", *b"mfra",
];
const MP4_KNOWN_LEAVES: &[[u8; 4]] = &[
    *b"ftyp", *b"mdat", *b"free", *b"skip", *b"wide", *b"uuid", *b"data", *b"hdlr", *b"mvhd",
    *b"tkhd", *b"mdhd", *b"vmhd", *b"smhd", *b"dref", *b"url ", *b"urn ", *b"stsd", *b"stts",
    *b"ctts", *b"stsc", *b"stsz", *b"stz2", *b"stco", *b"co64", *b"stss", *b"elst", *b"mfhd",
    *b"tfhd", *b"tfdt", *b"trun", *b"trex", *b"mehd", *b"mfro", *b"tfra", *b"sidx", *b"emsg",
    *b"pssh", *b"sgpd", *b"sbgp", *b"subs",
];

fn scan_mp4<R: Read + Seek>(io: &mut ScanIo<'_, R>, builder: &mut DocumentBuilder) -> Result<()> {
    let root = builder.add_node(
        None,
        "ISO-BMFF",
        0,
        0,
        io.file_len,
        io.file_len,
        PayloadRef {
            file_offset: 0,
            length: io.file_len,
        },
        ContentKind::Container,
        true,
        ParseStatus::Parsed,
        Some(format!("{} bytes", io.file_len)),
    )?;
    scan_mp4_boxes(io, builder, root, 0, io.file_len, 0, false)
}

fn scan_mp4_boxes<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    parent: NodeId,
    start: u64,
    end: u64,
    depth: usize,
    ilst_child: bool,
) -> Result<()> {
    if depth > 64 {
        builder.diagnostic(
            DiagnosticLevel::Warning,
            "mp4.depth_limit",
            "MP4 box depth limit exceeded",
            Some(start),
            Some(parent),
        );
        return Ok(());
    }
    let mut pos = start;
    while pos.saturating_add(8) <= end && pos.saturating_add(8) <= io.file_len {
        let header = io.read_at(pos, 8)?;
        let size32 = read_u32_be(&header, 0).unwrap_or(0);
        let box_type: [u8; 4] = header[4..8].try_into().unwrap();
        let mut header_size = 8u64;
        let mut size = size32 as u64;
        if size32 == 1 {
            let ext = io.read_at(pos + 8, 8)?;
            size = read_u64_be(&ext, 0).unwrap_or(0);
            header_size = 16;
        } else if size32 == 0 {
            size = end.saturating_sub(pos);
        }
        if &box_type == b"uuid" {
            header_size = header_size.saturating_add(16);
        }
        if size < header_size {
            builder.diagnostic(
                DiagnosticLevel::Error,
                "mp4.invalid_size",
                format!(
                    "{} box size {size} is smaller than header",
                    fourcc(&box_type)
                ),
                Some(pos),
                Some(parent),
            );
            break;
        }
        let payload_offset = pos.saturating_add(header_size);
        let declared_payload = size - header_size;
        let readable = declared_payload.min(
            end.saturating_sub(payload_offset)
                .min(io.file_len.saturating_sub(payload_offset)),
        );
        let is_container = MP4_CONTAINERS.contains(&box_type) || ilst_child;
        let known = is_container || MP4_KNOWN_LEAVES.contains(&box_type) || &box_type == b"uuid";
        let name = fourcc(&box_type);
        let content = if &box_type == b"mdat" {
            ContentKind::Audio
        } else if &box_type == b"data" {
            ContentKind::Text
        } else if is_container {
            ContentKind::Container
        } else {
            ContentKind::Binary
        };
        let node = builder.add_node(
            Some(parent),
            name.clone(),
            pos,
            header_size,
            declared_payload,
            readable,
            PayloadRef {
                file_offset: payload_offset,
                length: readable,
            },
            content,
            known,
            if readable < declared_payload {
                ParseStatus::Truncated
            } else {
                ParseStatus::Parsed
            },
            Some(format!("{size} bytes @ 0x{pos:08X}")),
        )?;
        if &box_type == b"ftyp" {
            let bytes = io.read_prefix(payload_offset, readable, 256)?;
            if bytes.len() >= 8 {
                let major = fourcc(&bytes[0..4]);
                let minor = read_u32_be(&bytes, 4).unwrap_or(0);
                let brands = bytes[8..]
                    .chunks_exact(4)
                    .map(fourcc)
                    .collect::<Vec<_>>()
                    .join(", ");
                builder.document.nodes[node as usize].summary = Some(format!(
                    "major={major}, minor={minor}, compatible=[{brands}]"
                ));
            }
        } else if &box_type == b"data" {
            decode_mp4_data(io, builder, node, payload_offset, readable)?;
        }
        if is_container {
            let child_start = if &box_type == b"meta" {
                payload_offset.saturating_add(4)
            } else {
                payload_offset
            };
            let child_end = payload_offset.saturating_add(readable);
            if child_start <= child_end {
                scan_mp4_boxes(
                    io,
                    builder,
                    node,
                    child_start,
                    child_end,
                    depth + 1,
                    &box_type == b"ilst",
                )?;
            }
        }
        if readable < declared_payload || size == 0 {
            break;
        }
        pos = pos
            .checked_add(size)
            .ok_or_else(|| anyhow!("MP4 box offset overflow"))?;
    }
    Ok(())
}

fn decode_mp4_data<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    node: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    if length <= 8 {
        return Ok(());
    }
    let bytes = io.read_prefix(offset, length, 1024 * 1024)?;
    let data_type = read_u32_be(&bytes, 0).unwrap_or(0) & 0x00ff_ffff;
    let raw = &bytes[8..];
    let summary = match data_type {
        1 => String::from_utf8_lossy(raw).trim_matches('\0').to_string(),
        21 if raw.len() <= 8 => {
            let mut value = 0i64;
            for &byte in raw {
                value = (value << 8) | byte as i64;
            }
            value.to_string()
        }
        13 | 14 => format!("image payload, {} bytes", raw.len()),
        _ => format!("type {data_type}, {} bytes", raw.len()),
    };
    builder.document.nodes[node as usize].summary = Some(summary.clone());
    if matches!(data_type, 13 | 14) {
        let metadata_node = &mut builder.document.nodes[node as usize];
        metadata_node.content = ContentKind::Image;
        metadata_node.payload = PayloadRef {
            file_offset: offset + 8,
            length: length.saturating_sub(8),
        };
    }
    if let Some(parent) = builder.document.nodes[node as usize].parent {
        let key = builder.document.nodes[parent as usize].name.clone();
        let normalized = match key.as_str() {
            "%A9nam" => Some("title"),
            "%A9ART" | "aART" => Some("artist"),
            "%A9alb" => Some("album"),
            "%A9cmt" | "desc" => Some("comment"),
            "%A9day" => Some("date"),
            "%A9gen" | "gnre" => Some("genre"),
            "tmpo" => Some("bpm"),
            _ => None,
        };
        if let Some(normalized) = normalized {
            builder.normalize(
                normalized,
                MetadataValue::Text(summary),
                node,
                SourceRange {
                    offset: offset + 8,
                    length: length - 8,
                },
                Some("MP4 data".to_string()),
                false,
            );
        }
    }
    Ok(())
}

fn scan_id3_mp3<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
) -> Result<()> {
    let root = builder.add_node(
        None,
        "MP3",
        0,
        0,
        io.file_len,
        io.file_len,
        PayloadRef {
            file_offset: 0,
            length: io.file_len,
        },
        ContentKind::Container,
        true,
        ParseStatus::Parsed,
        Some(format!("{} bytes", io.file_len)),
    )?;
    let prefix = io.read_prefix(0, io.file_len, 10)?;
    let mut audio_start = 0u64;
    if prefix.starts_with(b"ID3") && prefix.len() >= 10 {
        let tag_size = synchsafe_u32(&prefix[6..10]).unwrap_or(0) as u64;
        let footer = prefix[5] & 0x10 != 0;
        let total = 10 + tag_size + if footer { 10 } else { 0 };
        let readable = total.min(io.file_len);
        let tag = builder.add_node(
            Some(root),
            format!("ID3v2.{}.{}", prefix[3], prefix[4]),
            0,
            10,
            tag_size,
            readable.saturating_sub(10),
            PayloadRef {
                file_offset: 10,
                length: readable.saturating_sub(10),
            },
            ContentKind::Container,
            true,
            if readable < total {
                ParseStatus::Truncated
            } else {
                ParseStatus::Parsed
            },
            Some(format!(
                "flags=0x{:02X}, {} bytes{}",
                prefix[5],
                total,
                if prefix[5] & 0x80 != 0 {
                    ", unsynchronised"
                } else {
                    ""
                }
            )),
        )?;
        scan_id3_tag_at(io, builder, tag, 0, readable)?;
        audio_start = readable;
    }
    if audio_start < io.file_len {
        builder.add_node(
            Some(root),
            "MPEG_AUDIO_STREAM",
            audio_start,
            0,
            io.file_len - audio_start,
            io.file_len - audio_start,
            PayloadRef {
                file_offset: audio_start,
                length: io.file_len - audio_start,
            },
            ContentKind::Audio,
            true,
            ParseStatus::Deferred,
            Some(format!("{} bytes", io.file_len - audio_start)),
        )?;
    }
    Ok(())
}

fn scan_id3_tag_at<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    parent: NodeId,
    offset: u64,
    length: u64,
) -> Result<()> {
    if length < 10 {
        return Ok(());
    }
    let header = io.read_at(offset, 10)?;
    if !header.starts_with(b"ID3") {
        return Ok(());
    }
    let version = header[3];
    let tag_unsynchronised = header[5] & 0x80 != 0;
    let tag_size = synchsafe_u32(&header[6..10]).unwrap_or(0) as u64;
    let tag_end = offset
        .saturating_add(10)
        .saturating_add(tag_size)
        .min(offset.saturating_add(length))
        .min(io.file_len);
    let mut pos = offset + 10;
    if header[5] & 0x40 != 0 {
        let ext_head = io.read_prefix(pos, tag_end.saturating_sub(pos), 10)?;
        let declared_ext_size = if version == 4 {
            synchsafe_u32(ext_head.get(0..4).unwrap_or_default()).unwrap_or(0) as u64
        } else {
            read_u32_be(&ext_head, 0).unwrap_or(0) as u64
        };
        let ext_size = if version == 3 {
            declared_ext_size.saturating_add(4)
        } else {
            declared_ext_size
        };
        let readable = ext_size.min(tag_end.saturating_sub(pos));
        builder.add_node(
            Some(parent),
            "EXTENDED_HEADER",
            pos,
            0,
            ext_size,
            readable,
            PayloadRef {
                file_offset: pos,
                length: readable,
            },
            ContentKind::Binary,
            true,
            ParseStatus::Parsed,
            Some(format!("{ext_size} bytes")),
        )?;
        pos = pos.saturating_add(ext_size);
    }
    while pos < tag_end {
        let frame_header_len = if version == 2 { 6 } else { 10 };
        if pos.saturating_add(frame_header_len) > tag_end {
            break;
        }
        let fh = io.read_at(pos, frame_header_len as usize)?;
        let id_len = if version == 2 { 3 } else { 4 };
        if fh[..id_len].iter().all(|b| *b == 0) {
            let padding = tag_end - pos;
            builder.add_node(
                Some(parent),
                "PADDING",
                pos,
                0,
                padding,
                padding,
                PayloadRef {
                    file_offset: pos,
                    length: padding,
                },
                ContentKind::Padding,
                true,
                ParseStatus::Parsed,
                Some(format!("{padding} bytes")),
            )?;
            break;
        }
        if !fh[..id_len]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            builder.diagnostic(
                DiagnosticLevel::Warning,
                "id3.invalid_frame",
                "invalid ID3 frame identifier",
                Some(pos),
                Some(parent),
            );
            break;
        }
        let id = fourcc(&fh[..id_len]);
        let declared = if version == 2 {
            ((fh[3] as u64) << 16) | ((fh[4] as u64) << 8) | fh[5] as u64
        } else if version == 4 {
            synchsafe_u32(&fh[4..8]).unwrap_or(0) as u64
        } else {
            read_u32_be(&fh, 4).unwrap_or(0) as u64
        };
        if declared == 0 {
            break;
        }
        let format_flags = if version == 2 { 0 } else { fh[9] };
        let unsupported = match version {
            3 => format_flags & (0x80 | 0x40) != 0,
            4 => format_flags & (0x08 | 0x04) != 0,
            _ => false,
        };
        let frame_unsynchronised = tag_unsynchronised || (version == 4 && format_flags & 0x02 != 0);
        let payload_offset = pos + frame_header_len;
        let readable = declared.min(tag_end.saturating_sub(payload_offset));
        let content = if unsupported {
            ContentKind::Binary
        } else if id.starts_with('T') || matches!(id.as_str(), "COMM" | "USLT") {
            ContentKind::Text
        } else if id == "APIC" || id == "PIC" {
            ContentKind::Image
        } else {
            ContentKind::Binary
        };
        let node = builder.add_node(
            Some(parent),
            id.clone(),
            pos,
            frame_header_len,
            declared,
            readable,
            PayloadRef {
                file_offset: payload_offset,
                length: readable,
            },
            content,
            known_id3_frame(&id),
            if readable < declared {
                ParseStatus::Truncated
            } else {
                ParseStatus::Parsed
            },
            Some(format!("{declared} bytes")),
        )?;
        if unsupported {
            builder.diagnostic(
                DiagnosticLevel::Warning,
                "id3.unsupported_frame",
                format!("{id} uses ID3 compression or encryption; retained as raw payload"),
                Some(pos),
                Some(node),
            );
        }
        if matches!(id.as_str(), "APIC" | "PIC") {
            decode_id3_picture(io, builder, node, payload_offset, readable, version == 2)?;
        }
        if content == ContentKind::Text && readable > 0 {
            let bytes = io.read_prefix(payload_offset, readable, 4 * 1024 * 1024)?;
            let logical_bytes;
            let bytes = if frame_unsynchronised {
                logical_bytes = id3_deunsynchronise(&bytes);
                logical_bytes.as_slice()
            } else {
                bytes.as_slice()
            };
            let skip = match version {
                3 => usize::from(format_flags & 0x20 != 0),
                4 => {
                    usize::from(format_flags & 0x40 != 0)
                        + if format_flags & 0x01 != 0 { 4 } else { 0 }
                }
                _ => 0,
            };
            let text = decode_id3_text(bytes.get(skip..).unwrap_or_default());
            builder.document.nodes[node as usize].summary = Some(text.clone());
            if let Some(key) = normalize_id3_key(&id, &text) {
                let value = if id == "TXXX" || id == "TXX" {
                    split_id3_user_text(&text)
                        .map(|(_, value)| value.to_string())
                        .unwrap_or_else(|| text.clone())
                } else {
                    text.clone()
                };
                builder.normalize(
                    key,
                    MetadataValue::Text(value),
                    node,
                    SourceRange {
                        offset: payload_offset,
                        length: readable,
                    },
                    Some("ID3 text encoding".to_string()),
                    false,
                );
            }
        }
        if readable < declared {
            break;
        }
        pos = payload_offset
            .checked_add(declared)
            .ok_or_else(|| anyhow!("ID3 frame overflow"))?;
    }
    Ok(())
}

fn known_id3_frame(id: &str) -> bool {
    id.starts_with('T')
        || id.starts_with('W')
        || matches!(
            id,
            "COMM"
                | "COM"
                | "USLT"
                | "ULT"
                | "SYLT"
                | "SLT"
                | "APIC"
                | "PIC"
                | "PRIV"
                | "GEOB"
                | "GEO"
                | "UFID"
                | "UFI"
                | "POPM"
                | "POP"
                | "PCNT"
                | "CNT"
                | "CHAP"
                | "CTOC"
                | "ETCO"
                | "ETC"
                | "GRID"
                | "ENCR"
                | "AENC"
                | "CRA"
                | "LINK"
                | "LNK"
                | "RVA2"
                | "EQU2"
                | "OWNE"
                | "COMR"
                | "USER"
                | "IPL"
                | "IPLS"
                | "MLLT"
                | "MLL"
                | "SEEK"
                | "SIGN"
                | "ASPI"
        )
}

fn decode_id3_picture<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
    node: NodeId,
    offset: u64,
    length: u64,
    v22: bool,
) -> Result<()> {
    let bytes = io.read_prefix(offset, length, 64 * 1024)?;
    if bytes.len() < 5 {
        return Ok(());
    }
    let encoding = bytes[0];
    let mut pos = 1usize;
    let mime = if v22 {
        if bytes.len() < 5 {
            return Ok(());
        }
        let value = String::from_utf8_lossy(&bytes[pos..pos + 3]).to_string();
        pos += 3;
        value
    } else {
        let Some(end) = bytes[pos..].iter().position(|byte| *byte == 0) else {
            return Ok(());
        };
        let value = String::from_utf8_lossy(&bytes[pos..pos + end]).to_string();
        pos += end + 1;
        value
    };
    if pos >= bytes.len() {
        return Ok(());
    }
    let picture_type = bytes[pos];
    pos += 1;
    if matches!(encoding, 1 | 2) {
        let Some(end) = bytes[pos..].windows(2).position(|window| window == [0, 0]) else {
            return Ok(());
        };
        pos += end + 2;
    } else {
        let Some(end) = bytes[pos..].iter().position(|byte| *byte == 0) else {
            return Ok(());
        };
        pos += end + 1;
    }
    if pos as u64 >= length {
        return Ok(());
    }
    let image_length = length - pos as u64;
    builder.document.nodes[node as usize].content = ContentKind::Container;
    builder.document.nodes[node as usize].summary = Some(format!(
        "{mime}, picture type {picture_type}, {image_length} bytes"
    ));
    builder.add_node(
        Some(node),
        "ImageData",
        offset + pos as u64,
        0,
        image_length,
        image_length,
        PayloadRef {
            file_offset: offset + pos as u64,
            length: image_length,
        },
        ContentKind::Image,
        true,
        ParseStatus::Parsed,
        Some(mime),
    )?;
    Ok(())
}

fn synchsafe_u32(data: &[u8]) -> Option<u32> {
    if data.len() < 4 || data[..4].iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    Some(
        ((data[0] as u32) << 21)
            | ((data[1] as u32) << 14)
            | ((data[2] as u32) << 7)
            | data[3] as u32,
    )
}

fn decode_id3_text(data: &[u8]) -> String {
    let Some((&encoding, body)) = data.split_first() else {
        return String::new();
    };
    match encoding {
        0 => WINDOWS_1252.decode(body).0.trim_matches('\0').to_string(),
        1 => {
            let (little, body) = if body.starts_with(&[0xff, 0xfe]) {
                (true, &body[2..])
            } else if body.starts_with(&[0xfe, 0xff]) {
                (false, &body[2..])
            } else {
                (false, body)
            };
            let values = body
                .chunks_exact(2)
                .map(|chunk| {
                    if little {
                        u16::from_le_bytes([chunk[0], chunk[1]])
                    } else {
                        u16::from_be_bytes([chunk[0], chunk[1]])
                    }
                })
                .take_while(|v| *v != 0)
                .collect::<Vec<_>>();
            String::from_utf16_lossy(&values)
        }
        2 => {
            let values = body
                .chunks_exact(2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .take_while(|v| *v != 0)
                .collect::<Vec<_>>();
            String::from_utf16_lossy(&values)
        }
        _ => String::from_utf8_lossy(body).trim_matches('\0').to_string(),
    }
}

fn id3_deunsynchronise(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    let mut index = 0usize;
    while index < data.len() {
        output.push(data[index]);
        if data[index] == 0xff && data.get(index + 1) == Some(&0) {
            index += 2;
        } else {
            index += 1;
        }
    }
    output
}

fn split_id3_user_text(text: &str) -> Option<(&str, &str)> {
    text.split_once('\0').or_else(|| text.split_once('='))
}

fn normalize_id3_key(id: &str, text: &str) -> Option<String> {
    match id {
        "TIT2" | "TT2" => Some("title".to_string()),
        "TPE1" | "TP1" => Some("artist".to_string()),
        "TALB" | "TAL" => Some("album".to_string()),
        "COMM" | "COM" => Some("comment".to_string()),
        "TCON" | "TCO" => Some("genre".to_string()),
        "TBPM" | "TBP" => Some("bpm".to_string()),
        "TXXX" | "TXX" => {
            let description = split_id3_user_text(text)
                .map(|(description, _)| description)
                .unwrap_or(text)
                .trim()
                .to_ascii_uppercase();
            normalize_comment_key(&description)
        }
        _ => None,
    }
}

#[derive(Default)]
struct OggStreamState {
    node: NodeId,
    start: u64,
    end: u64,
    pages: u64,
    packets: u64,
    packet_buf: Vec<u8>,
    packet_start: Option<u64>,
    packet_physical_end: u64,
    packet_noncontiguous: bool,
    comments_done: bool,
}

fn scan_ogg<R: Read + Seek>(io: &mut ScanIo<'_, R>, builder: &mut DocumentBuilder) -> Result<()> {
    let root = builder.add_node(
        None,
        "Ogg",
        0,
        0,
        io.file_len,
        io.file_len,
        PayloadRef {
            file_offset: 0,
            length: io.file_len,
        },
        ContentKind::Container,
        true,
        ParseStatus::Parsed,
        Some(format!("{} bytes", io.file_len)),
    )?;
    let mut streams: HashMap<u32, OggStreamState> = HashMap::new();
    let mut pos = 0u64;
    while pos.saturating_add(27) <= io.file_len {
        io.check()?;
        let header = io.read_at(pos, 27)?;
        if &header[0..4] != b"OggS" {
            builder.diagnostic(
                DiagnosticLevel::Warning,
                "ogg.capture_pattern",
                "Ogg capture pattern not found",
                Some(pos),
                Some(root),
            );
            break;
        }
        let flags = header[5];
        let granule = read_u64_le(&header, 6).unwrap_or(0);
        let serial = read_u32_le(&header, 14).unwrap_or(0);
        let page_no = read_u32_le(&header, 18).unwrap_or(0);
        let segment_count = header[26] as usize;
        let lacing = io.read_at(pos + 27, segment_count)?;
        let body_len: u64 = lacing.iter().map(|v| *v as u64).sum();
        let body_offset = pos + 27 + segment_count as u64;
        let readable = body_len.min(io.file_len.saturating_sub(body_offset));
        if !streams.contains_key(&serial) {
            let node = builder.add_node(
                Some(root),
                format!("Stream 0x{serial:08X}"),
                pos,
                0,
                0,
                0,
                PayloadRef {
                    file_offset: pos,
                    length: 0,
                },
                ContentKind::Container,
                true,
                ParseStatus::Parsed,
                Some(format!("serial=0x{serial:08X}")),
            )?;
            streams.insert(
                serial,
                OggStreamState {
                    node,
                    start: pos,
                    end: pos,
                    ..Default::default()
                },
            );
        }
        let state = streams.get_mut(&serial).unwrap();
        state.pages += 1;
        state.end = body_offset.saturating_add(body_len);
        let need_packet_data = !state.comments_done && state.packets < 3;
        let body = if need_packet_data && readable <= MAX_XML_BYTES {
            Some(io.read_at(body_offset, readable as usize)?)
        } else {
            None
        };
        if let Some(body) = body {
            let mut body_pos = 0usize;
            for &lace in &lacing {
                let lace = lace as usize;
                if body_pos + lace > body.len() {
                    break;
                }
                let segment_start = body_offset.saturating_add(body_pos as u64);
                if state.packet_buf.is_empty() {
                    state.packet_start = Some(segment_start);
                    state.packet_noncontiguous = false;
                } else if state.packet_physical_end != segment_start {
                    state.packet_noncontiguous = true;
                }
                if state.packet_buf.len().saturating_add(lace) <= MAX_XML_BYTES as usize {
                    state
                        .packet_buf
                        .extend_from_slice(&body[body_pos..body_pos + lace]);
                }
                body_pos += lace;
                state.packet_physical_end = segment_start.saturating_add(lace as u64);
                if lace < 255 {
                    state.packets += 1;
                    if state.packet_buf.starts_with(b"\x03vorbis")
                        || state.packet_buf.starts_with(b"OpusTags")
                    {
                        let packet_start = state.packet_start.unwrap_or(segment_start);
                        let physical_length =
                            state.packet_physical_end.saturating_sub(packet_start);
                        let packet_node = builder.add_node(
                            Some(state.node),
                            if state.packet_buf.starts_with(b"OpusTags") {
                                "OpusTags"
                            } else {
                                "VorbisComment"
                            },
                            packet_start,
                            0,
                            state.packet_buf.len() as u64,
                            physical_length,
                            PayloadRef {
                                file_offset: packet_start,
                                length: physical_length,
                            },
                            ContentKind::Container,
                            true,
                            ParseStatus::Parsed,
                            None,
                        )?;
                        decode_vorbis_packet_bytes(
                            builder,
                            packet_node,
                            packet_start,
                            &state.packet_buf,
                        )?;
                        if state.packet_noncontiguous {
                            builder.diagnostic(
                                DiagnosticLevel::Info,
                                "ogg.packet_physical_span",
                                "comment packet spans pages; the node payload is its physical span including intervening page headers",
                                Some(packet_start),
                                Some(packet_node),
                            );
                        }
                        state.comments_done = true;
                    }
                    state.packet_buf.clear();
                    state.packet_start = None;
                    state.packet_physical_end = 0;
                    state.packet_noncontiguous = false;
                }
            }
        }
        builder.document.nodes[state.node as usize].summary = Some(format!(
            "{} pages, {} packets, last page {}, granule {}{}",
            state.pages,
            state.packets,
            page_no,
            granule,
            if flags & 0x04 != 0 { ", EOS" } else { "" }
        ));
        if readable < body_len {
            builder.diagnostic(
                DiagnosticLevel::Error,
                "ogg.truncated_page",
                "Ogg page body is truncated",
                Some(pos),
                Some(state.node),
            );
            break;
        }
        pos = body_offset
            .checked_add(body_len)
            .ok_or_else(|| anyhow!("Ogg page offset overflow"))?;
    }
    for (serial, state) in streams {
        let length = state.end.saturating_sub(state.start);
        let stream_node = &mut builder.document.nodes[state.node as usize];
        stream_node.offset = state.start;
        stream_node.declared_size = length;
        stream_node.readable_size = length;
        stream_node.payload = PayloadRef {
            file_offset: state.start,
            length,
        };
        builder.add_node(
            Some(state.node),
            "COMPRESSED_AUDIO_PACKETS",
            state.start,
            0,
            length,
            length,
            PayloadRef {
                file_offset: state.start,
                length,
            },
            ContentKind::Audio,
            true,
            ParseStatus::Deferred,
            Some(format!(
                "logical stream 0x{serial:08X}; exact playback-to-bitstream mapping unavailable"
            )),
        )?;
    }
    Ok(())
}

fn decode_vorbis_packet_bytes(
    builder: &mut DocumentBuilder,
    parent: NodeId,
    offset: u64,
    packet: &[u8],
) -> Result<()> {
    let prefix = if packet.starts_with(b"\x03vorbis") {
        7
    } else if packet.starts_with(b"OpusTags") {
        8
    } else {
        0
    };
    let mut pos = prefix;
    let Some(vendor_len) = read_u32_le(packet, pos).map(|v| v as usize) else {
        return Ok(());
    };
    pos += 4;
    if pos + vendor_len > packet.len() {
        return Ok(());
    }
    let vendor = String::from_utf8_lossy(&packet[pos..pos + vendor_len]).to_string();
    builder.add_scalar(
        parent,
        "Vendor",
        vendor,
        SourceRange {
            offset: offset + pos as u64,
            length: vendor_len as u64,
        },
    )?;
    pos += vendor_len;
    let Some(count) = read_u32_le(packet, pos).map(|v| v as usize) else {
        return Ok(());
    };
    pos += 4;
    builder.document.nodes[parent as usize].summary = Some(format!("{count} comments"));
    for _ in 0..count.min(100_000) {
        let Some(len) = read_u32_le(packet, pos).map(|v| v as usize) else {
            break;
        };
        pos += 4;
        if pos + len > packet.len() {
            break;
        }
        let text = String::from_utf8_lossy(&packet[pos..pos + len]).to_string();
        let (key, value) = text.split_once('=').unwrap_or((&text, ""));
        let child = builder.add_scalar(
            parent,
            key,
            value,
            SourceRange {
                offset: offset + pos as u64,
                length: len as u64,
            },
        )?;
        if let Some(normalized) = normalize_comment_key(key) {
            builder.normalize(
                normalized,
                MetadataValue::Text(value.to_string()),
                child,
                SourceRange {
                    offset: offset + pos as u64,
                    length: len as u64,
                },
                Some("UTF-8".to_string()),
                false,
            );
        }
        pos += len;
    }
    Ok(())
}

fn scan_unknown<R: Read + Seek>(
    io: &mut ScanIo<'_, R>,
    builder: &mut DocumentBuilder,
) -> Result<()> {
    let prefix = io.read_prefix(0, io.file_len, 4096)?;
    let text = std::str::from_utf8(&prefix).ok().filter(|text| {
        let printable = text
            .chars()
            .filter(|character| !character.is_control() || character.is_whitespace())
            .count();
        printable.saturating_mul(100) >= text.chars().count().max(1).saturating_mul(85)
    });
    builder.add_node(
        None,
        "Binary",
        0,
        0,
        io.file_len,
        io.file_len,
        PayloadRef {
            file_offset: 0,
            length: io.file_len,
        },
        if text.is_some() {
            ContentKind::Text
        } else {
            ContentKind::Binary
        },
        false,
        ParseStatus::Unsupported,
        Some(
            text.map(|text| text.chars().take(512).collect())
                .unwrap_or_else(|| format!("{} bytes; unknown container", io.file_len)),
        ),
    )?;
    Ok(())
}

pub fn summarize_path(
    path: &Path,
    request: SummaryRequest,
    budget: ScanBudget,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<MetadataSummary> {
    let document = inspect_path(
        path,
        InspectOptions {
            budget,
            decode_xml: request
                .fields
                .iter()
                .any(|key| key.starts_with("ucs.") || key.starts_with("ixml.")),
            decode_values: true,
            cancel,
        },
    )?;
    Ok(summary_from_document(&document, &request))
}

pub fn summary_from_document(
    document: &MetadataDocument,
    request: &SummaryRequest,
) -> MetadataSummary {
    let wants_all = request.fields.is_empty();
    let fields = document
        .normalized
        .iter()
        .filter(|field| wants_all || request.fields.iter().any(|key| key == &field.key))
        .map(|field| SummaryField {
            key: field.key.clone(),
            values: field.values.clone(),
            resolved: field
                .values
                .get(field.resolved_index)
                .map(|value| value.value.clone()),
            conflict: field.conflict,
        })
        .collect();
    let mut raw_fields: BTreeMap<String, Vec<MetadataValue>> = BTreeMap::new();
    if request.include_raw {
        for node in &document.nodes {
            if let Some(summary) = &node.summary {
                raw_fields
                    .entry(raw_column_key(document.container, &node.path))
                    .or_default()
                    .push(MetadataValue::Text(summary.clone()));
            }
        }
    }
    MetadataSummary {
        schema_version: PARSER_SCHEMA_VERSION,
        source: document.source.clone(),
        container: document.container,
        completion: document.completion,
        fields,
        raw_fields,
        unknown_nodes: document.nodes.iter().filter(|node| !node.known).count(),
        diagnostics: document.diagnostics.clone(),
        coverage: {
            let mut coverage = if wants_all {
                let mut keys = document
                    .normalized
                    .iter()
                    .map(|field| field.key.clone())
                    .collect::<Vec<_>>();
                keys.push("__all__".to_string());
                keys
            } else {
                request.fields.clone()
            };
            if request.include_raw {
                coverage.push("__raw__".to_string());
            }
            coverage
        },
    }
}

pub fn raw_column_key(container: ContainerKind, path: &str) -> String {
    let encoded = path
        .trim_start_matches('/')
        .split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/");
    format!("raw:{}:{encoded}", container.key())
}

fn percent_encode_segment(segment: &str) -> String {
    let mut out = String::new();
    let bytes = segment.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%'
            && index + 2 < bytes.len()
            && bytes[index + 1].is_ascii_hexdigit()
            && bytes[index + 2].is_ascii_hexdigit()
        {
            out.push('%');
            out.push(bytes[index + 1] as char);
            out.push(bytes[index + 2] as char);
            index += 3;
            continue;
        }
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
        index += 1;
    }
    out
}

pub fn read_payload(path: &Path, payload: PayloadRef, max_len: u64) -> Result<Vec<u8>> {
    if payload.length > max_len {
        bail!(
            "payload length {} exceeds read limit {}",
            payload.length,
            max_len
        );
    }
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    if payload.file_offset > file_len
        || payload.length > file_len.saturating_sub(payload.file_offset)
    {
        bail!("payload range is outside the file");
    }
    file.seek(SeekFrom::Start(payload.file_offset))?;
    let mut out = vec![0u8; payload.length as usize];
    file.read_exact(&mut out)?;
    Ok(out)
}

pub fn read_payload_page(
    path: &Path,
    offset: u64,
    length: usize,
    allowed: Option<PayloadRef>,
) -> Result<Vec<u8>> {
    let file_len = fs::metadata(path)?.len();
    let allowed = allowed.unwrap_or(PayloadRef {
        file_offset: 0,
        length: file_len,
    });
    let allowed_end = allowed
        .file_offset
        .saturating_add(allowed.length)
        .min(file_len);
    if offset < allowed.file_offset || offset > allowed_end {
        bail!("page offset is outside the allowed payload");
    }
    let len = (length as u64).min(allowed_end.saturating_sub(offset)) as usize;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut out = vec![0u8; len];
    file.read_exact(&mut out)?;
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Ascii,
    Utf8,
    Hex,
    FourCc,
}

pub fn parse_search_pattern(kind: SearchKind, query: &str) -> Result<Vec<u8>> {
    match kind {
        SearchKind::Ascii => {
            if !query.is_ascii() {
                bail!("ASCII query contains non-ASCII characters");
            }
            Ok(query.as_bytes().to_vec())
        }
        SearchKind::Utf8 | SearchKind::FourCc => Ok(query.as_bytes().to_vec()),
        SearchKind::Hex => {
            let compact: String = query.chars().filter(|c| !c.is_whitespace()).collect();
            if compact.len() % 2 != 0 {
                bail!("hex query must contain an even number of digits");
            }
            (0..compact.len())
                .step_by(2)
                .map(|idx| {
                    u8::from_str_radix(&compact[idx..idx + 2], 16)
                        .with_context(|| format!("invalid hex at position {idx}"))
                })
                .collect()
        }
    }
}

pub fn search_payload(
    path: &Path,
    payload: PayloadRef,
    pattern: &[u8],
    limit: usize,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<u64>> {
    search_payload_with_progress(path, payload, pattern, limit, cancel, None)
}

pub fn search_payload_with_progress(
    path: &Path,
    payload: PayloadRef,
    pattern: &[u8],
    limit: usize,
    cancel: Option<&AtomicBool>,
    progress: Option<&AtomicU64>,
) -> Result<Vec<u64>> {
    if pattern.is_empty() {
        bail!("search pattern must not be empty");
    }
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    if payload.file_offset > file_len
        || payload.length > file_len.saturating_sub(payload.file_offset)
    {
        bail!("payload range is outside the file");
    }
    file.seek(SeekFrom::Start(payload.file_offset))?;
    let mut results = Vec::new();
    let mut remaining = payload.length;
    let mut absolute = payload.file_offset;
    let mut carry = Vec::new();
    let mut chunk = vec![0u8; 1024 * 1024];
    while remaining > 0 && results.len() < limit {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            bail!("payload search cancelled");
        }
        let count = remaining.min(chunk.len() as u64) as usize;
        file.read_exact(&mut chunk[..count])?;
        let mut window = Vec::with_capacity(carry.len() + count);
        window.extend_from_slice(&carry);
        window.extend_from_slice(&chunk[..count]);
        let window_base = absolute.saturating_sub(carry.len() as u64);
        for (idx, candidate) in window.windows(pattern.len()).enumerate() {
            if candidate == pattern {
                let hit = window_base + idx as u64;
                if results.last().copied() != Some(hit) {
                    results.push(hit);
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }
        let keep = pattern.len().saturating_sub(1).min(window.len());
        carry.clear();
        carry.extend_from_slice(&window[window.len() - keep..]);
        absolute += count as u64;
        remaining -= count as u64;
        if let Some(progress) = progress {
            progress.store(payload.length - remaining, Ordering::Relaxed);
        }
    }
    Ok(results)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgorithm {
    Md5,
    Sha256,
}

pub fn hash_payload(
    path: &Path,
    payload: PayloadRef,
    algorithm: HashAlgorithm,
    cancel: Option<&AtomicBool>,
) -> Result<String> {
    hash_payload_with_progress(path, payload, algorithm, cancel, None)
}

pub fn hash_payload_with_progress(
    path: &Path,
    payload: PayloadRef,
    algorithm: HashAlgorithm,
    cancel: Option<&AtomicBool>,
    progress: Option<&AtomicU64>,
) -> Result<String> {
    use md5::{Digest as _, Md5};
    use sha2::Sha256;
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    if payload.file_offset > file_len
        || payload.length > file_len.saturating_sub(payload.file_offset)
    {
        bail!("payload range is outside the file");
    }
    file.seek(SeekFrom::Start(payload.file_offset))?;
    let mut remaining = payload.length;
    let mut chunk = vec![0u8; 1024 * 1024];
    match algorithm {
        HashAlgorithm::Md5 => {
            let mut hasher = Md5::new();
            while remaining > 0 {
                if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    bail!("payload hash cancelled");
                }
                let count = remaining.min(chunk.len() as u64) as usize;
                file.read_exact(&mut chunk[..count])?;
                hasher.update(&chunk[..count]);
                remaining -= count as u64;
                if let Some(progress) = progress {
                    progress.store(payload.length - remaining, Ordering::Relaxed);
                }
            }
            Ok(format!("{:x}", hasher.finalize()))
        }
        HashAlgorithm::Sha256 => {
            let mut hasher = Sha256::new();
            while remaining > 0 {
                if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    bail!("payload hash cancelled");
                }
                let count = remaining.min(chunk.len() as u64) as usize;
                file.read_exact(&mut chunk[..count])?;
                hasher.update(&chunk[..count]);
                remaining -= count as u64;
                if let Some(progress) = progress {
                    progress.store(payload.length - remaining, Ordering::Relaxed);
                }
            }
            Ok(format!("{:x}", hasher.finalize()))
        }
    }
}

pub fn extract_payload(
    path: &Path,
    payload: PayloadRef,
    output: &Path,
    overwrite: bool,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    extract_payload_with_progress(path, payload, output, overwrite, cancel, None)
}

pub fn extract_payload_with_progress(
    path: &Path,
    payload: PayloadRef,
    output: &Path,
    overwrite: bool,
    cancel: Option<&AtomicBool>,
    progress: Option<&AtomicU64>,
) -> Result<()> {
    if output.exists() && !overwrite {
        bail!("output already exists; explicit overwrite is required");
    }
    let mut input = File::open(path)?;
    let file_len = input.metadata()?.len();
    if payload.file_offset > file_len
        || payload.length > file_len.saturating_sub(payload.file_offset)
    {
        bail!("payload range is outside the file");
    }
    if paths_refer_to_same_file(path, output) {
        bail!("payload output must not be the source file");
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let part = output.with_extension(format!(
        "{}.part",
        output
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("payload")
    ));
    if paths_refer_to_same_file(path, &part) {
        bail!("temporary payload output must not be the source file");
    }
    if part.exists() {
        fs::remove_file(&part)?;
    }
    input.seek(SeekFrom::Start(payload.file_offset))?;
    let result = (|| -> Result<()> {
        let mut out = File::create(&part)?;
        let mut remaining = payload.length;
        let mut chunk = vec![0u8; 1024 * 1024];
        while remaining > 0 {
            if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                bail!("payload extraction cancelled");
            }
            let count = remaining.min(chunk.len() as u64) as usize;
            input.read_exact(&mut chunk[..count])?;
            out.write_all(&chunk[..count])?;
            remaining -= count as u64;
            if let Some(progress) = progress {
                progress.store(payload.length - remaining, Ordering::Relaxed);
            }
        }
        out.flush()?;
        Ok(())
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(&part);
        return Err(err);
    }
    if output.exists() {
        fs::remove_file(output)?;
    }
    fs::rename(&part, output)?;
    Ok(())
}

fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    if let (Ok(left), Ok(right)) = (fs::canonicalize(left), fs::canonicalize(right)) {
        if left == right {
            return true;
        }
    }
    same_file::is_same_file(left, right).unwrap_or(false)
}

pub fn payload_for_selector(
    document: &MetadataDocument,
    node_path: Option<&str>,
    occurrence: usize,
    offset: Option<u64>,
    length: Option<u64>,
) -> Result<PayloadRef> {
    if let Some(path) = node_path {
        if offset.is_some() || length.is_some() {
            bail!("node path conflicts with explicit offset/length");
        }
        return document
            .find_path(path, occurrence)
            .map(|node| node.payload)
            .ok_or_else(|| anyhow!("metadata node path not found: {path}"));
    }
    let offset = offset.ok_or_else(|| anyhow!("payload selector requires node path or offset"))?;
    let length = length.ok_or_else(|| anyhow!("explicit offset requires length"))?;
    if offset > document.file_len || length > document.file_len.saturating_sub(offset) {
        bail!("explicit payload range is outside the file");
    }
    Ok(PayloadRef {
        file_offset: offset,
        length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct SparseReader {
        prefix: Vec<u8>,
        len: u64,
        position: u64,
        bytes_read: u64,
    }

    impl Read for SparseReader {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let available = self.len.saturating_sub(self.position);
            let count = available.min(output.len() as u64) as usize;
            output[..count].fill(0);
            let prefix_start = self.position.min(self.prefix.len() as u64) as usize;
            let prefix_end = self
                .position
                .saturating_add(count as u64)
                .min(self.prefix.len() as u64) as usize;
            if prefix_end > prefix_start {
                output[..prefix_end - prefix_start]
                    .copy_from_slice(&self.prefix[prefix_start..prefix_end]);
            }
            self.position = self.position.saturating_add(count as u64);
            self.bytes_read = self.bytes_read.saturating_add(count as u64);
            Ok(count)
        }
    }

    impl Seek for SparseReader {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            let next = match position {
                SeekFrom::Start(position) => position as i128,
                SeekFrom::Current(delta) => self.position as i128 + delta as i128,
                SeekFrom::End(delta) => self.len as i128 + delta as i128,
            };
            if next < 0 || next > u64::MAX as i128 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid sparse seek",
                ));
            }
            self.position = next as u64;
            Ok(self.position)
        }
    }

    fn riff_with_chunks(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, payload) in chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
            if payload.len() & 1 != 0 {
                body.push(0);
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn riff_scan_keeps_payload_as_ranges_and_maps_pcm_frames() {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&48_000u32.to_le_bytes());
        fmt.extend_from_slice(&192_000u32.to_le_bytes());
        fmt.extend_from_slice(&4u16.to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());
        let bytes = riff_with_chunks(&[(b"fmt ", fmt), (b"data", vec![0; 16])]);
        let mut cursor = Cursor::new(bytes.clone());
        let document = inspect_reader(
            &mut cursor,
            bytes.len() as u64,
            PathBuf::from("memory.wav"),
            InspectOptions::default(),
        )
        .unwrap();
        assert_eq!(document.container, ContainerKind::RiffWave);
        assert_eq!(
            document
                .nodes
                .iter()
                .find(|node| node.name == "data")
                .unwrap()
                .payload
                .length,
            16
        );
        let mapping = document.audio_mapping.unwrap();
        assert_eq!(mapping.frame_count(), 4);
        assert_eq!(mapping.channel_range(2, 1).unwrap().length, 2);
    }

    #[test]
    fn rf64_over_four_gib_is_scanned_without_allocating_the_payload() {
        let data_size = u32::MAX as u64 + 4097;
        let file_len = 80 + data_size + (data_size & 1);
        let mut prefix = Vec::new();
        prefix.extend_from_slice(b"RF64");
        prefix.extend_from_slice(&u32::MAX.to_le_bytes());
        prefix.extend_from_slice(b"WAVE");
        prefix.extend_from_slice(b"ds64");
        prefix.extend_from_slice(&28u32.to_le_bytes());
        prefix.extend_from_slice(&(file_len - 8).to_le_bytes());
        prefix.extend_from_slice(&data_size.to_le_bytes());
        prefix.extend_from_slice(&(data_size / 4).to_le_bytes());
        prefix.extend_from_slice(&0u32.to_le_bytes());
        prefix.extend_from_slice(b"fmt ");
        prefix.extend_from_slice(&16u32.to_le_bytes());
        prefix.extend_from_slice(&1u16.to_le_bytes());
        prefix.extend_from_slice(&2u16.to_le_bytes());
        prefix.extend_from_slice(&48_000u32.to_le_bytes());
        prefix.extend_from_slice(&192_000u32.to_le_bytes());
        prefix.extend_from_slice(&4u16.to_le_bytes());
        prefix.extend_from_slice(&16u16.to_le_bytes());
        prefix.extend_from_slice(b"data");
        prefix.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(prefix.len(), 80);

        let mut reader = SparseReader {
            prefix,
            len: file_len,
            position: 0,
            bytes_read: 0,
        };
        let document = inspect_reader(
            &mut reader,
            file_len,
            PathBuf::from("logical.rf64"),
            InspectOptions::default(),
        )
        .unwrap();
        assert_eq!(document.container, ContainerKind::Rf64);
        let mapping = document.audio_mapping.expect("RF64 PCM mapping");
        assert_eq!(mapping.data_offset, 80);
        assert_eq!(mapping.data_length, data_size);
        assert_eq!(mapping.frame_count(), data_size / 4);
        assert_eq!(document.completion, Completion::Complete);
        assert!(
            reader.bytes_read < 1024,
            "structure scan must not read the audio payload"
        );
    }

    #[test]
    fn truncated_riff_is_partial_not_a_panic() {
        let mut bytes = riff_with_chunks(&[(b"JUNK", vec![1, 2, 3, 4])]);
        bytes.truncate(bytes.len() - 2);
        let mut cursor = Cursor::new(bytes.clone());
        let document = inspect_reader(
            &mut cursor,
            bytes.len() as u64,
            PathBuf::from("broken.wav"),
            InspectOptions::default(),
        )
        .unwrap();
        assert_eq!(document.completion, Completion::Partial);
        assert!(!document.diagnostics.is_empty());
    }

    #[test]
    fn mp4_extended_size_is_scanned() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&16u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"M4A ");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(b"free");
        bytes.extend_from_slice(&24u64.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        let mut cursor = Cursor::new(bytes.clone());
        let document = inspect_reader(
            &mut cursor,
            bytes.len() as u64,
            PathBuf::from("memory.bin"),
            InspectOptions::default(),
        )
        .unwrap();
        assert_eq!(document.container, ContainerKind::Mp4);
        assert!(document.nodes.iter().any(|node| node.name == "free"));
    }

    #[test]
    fn raw_column_paths_are_lossless_and_occurrence_free() {
        assert_eq!(
            raw_column_key(ContainerKind::Mp4, "/ISO-BMFF/©nam"),
            "raw:mp4:ISO-BMFF/%C2%A9nam"
        );
    }

    #[test]
    fn hex_search_pattern_accepts_spaces() {
        assert_eq!(
            parse_search_pattern(SearchKind::Hex, "69 58 4d 4c").unwrap(),
            b"iXML"
        );
    }

    fn inspect_memory(bytes: Vec<u8>) -> MetadataDocument {
        let mut cursor = Cursor::new(bytes.clone());
        inspect_reader(
            &mut cursor,
            bytes.len() as u64,
            PathBuf::from("fixture.audio"),
            InspectOptions::default(),
        )
        .unwrap()
    }

    fn id3v23(frames: &[(&[u8; 4], Vec<u8>)], flags: u8) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, payload) in frames {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            body.extend_from_slice(&[0, 0]);
            body.extend_from_slice(payload);
        }
        assert!(body.len() < 128);
        let mut tag = b"ID3\x03\0".to_vec();
        tag.push(flags);
        tag.extend_from_slice(&[0, 0, 0, body.len() as u8]);
        tag.extend_from_slice(&body);
        tag
    }

    fn ogg_page(serial: u32, sequence: u32, flags: u8, lacing: &[u8], body: &[u8]) -> Vec<u8> {
        assert_eq!(
            lacing.iter().map(|value| *value as usize).sum::<usize>(),
            body.len()
        );
        let mut page = Vec::new();
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(flags);
        page.extend_from_slice(&0u64.to_le_bytes());
        page.extend_from_slice(&serial.to_le_bytes());
        page.extend_from_slice(&sequence.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.push(lacing.len() as u8);
        page.extend_from_slice(lacing);
        page.extend_from_slice(body);
        page
    }

    fn mp4_box(box_type: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        output.extend_from_slice(&box_type);
        output.extend_from_slice(&payload);
        output
    }

    #[test]
    fn all_supported_containers_are_detected_by_content() {
        let mut aiff = Vec::new();
        aiff.extend_from_slice(b"FORM");
        aiff.extend_from_slice(&4u32.to_be_bytes());
        aiff.extend_from_slice(b"AIFF");

        let mut flac = b"fLaC".to_vec();
        flac.extend_from_slice(&[0x80, 0, 0, 34]);
        flac.extend_from_slice(&[0; 34]);

        let mut id3 = b"ID3\x04\0\0\0\0\0\0".to_vec();
        id3.extend_from_slice(&[0xff, 0xfb, 0x90, 0x64]);

        let mut mp4 = Vec::new();
        mp4.extend_from_slice(&16u32.to_be_bytes());
        mp4.extend_from_slice(b"ftyp");
        mp4.extend_from_slice(b"M4A ");
        mp4.extend_from_slice(&0u32.to_be_bytes());
        mp4.extend_from_slice(&8u32.to_be_bytes());
        mp4.extend_from_slice(b"mdat");

        let mut ogg = vec![0; 27];
        ogg[0..4].copy_from_slice(b"OggS");
        ogg[4] = 0;
        ogg[5] = 4;
        ogg[14..18].copy_from_slice(&1u32.to_le_bytes());
        ogg[18..22].copy_from_slice(&0u32.to_le_bytes());
        ogg[26] = 0;

        for (bytes, expected) in [
            (aiff, ContainerKind::Aiff),
            (flac, ContainerKind::Flac),
            (id3, ContainerKind::Mp3),
            (mp4, ContainerKind::Mp4),
            (ogg, ContainerKind::Ogg),
        ] {
            let document = inspect_memory(bytes);
            assert_eq!(document.container, expected);
            assert!(!document.roots.is_empty());
        }
    }

    #[test]
    fn id3_duplicate_frames_and_unsynchronisation_remain_physical_and_decoded() {
        let tag = id3v23(
            &[
                (b"TIT2", vec![0, b'A', 0xff, 0, 0xe1, b'B']),
                (b"TIT2", [vec![0], b"Other".to_vec()].concat()),
            ],
            0x80,
        );
        let document = inspect_memory(tag);
        assert_eq!(
            document
                .nodes
                .iter()
                .filter(|node| node.name == "TIT2")
                .count(),
            2
        );
        let title_nodes = document
            .nodes
            .iter()
            .filter(|node| node.name == "TIT2")
            .collect::<Vec<_>>();
        assert_eq!(title_nodes[0].path, title_nodes[1].path);
        assert_eq!(
            payload_for_selector(&document, Some(&title_nodes[0].path), 1, None, None)
                .unwrap()
                .file_offset,
            title_nodes[1].payload.file_offset
        );
        let title = document
            .normalized
            .iter()
            .find(|field| field.key == "title")
            .expect("normalized duplicate title");
        assert_eq!(title.values.len(), 2);
        assert!(title.conflict);
        assert_eq!(
            title.values[0].value,
            MetadataValue::Text("AÿáB".to_string())
        );
    }

    #[test]
    fn ogg_vorbis_comment_packet_can_span_pages() {
        let title = format!("TITLE={}", "x".repeat(260));
        let mut packet = b"\x03vorbis".to_vec();
        packet.extend_from_slice(&3u32.to_le_bytes());
        packet.extend_from_slice(b"UCS");
        packet.extend_from_slice(&1u32.to_le_bytes());
        packet.extend_from_slice(&(title.len() as u32).to_le_bytes());
        packet.extend_from_slice(title.as_bytes());
        let split = 255;
        let mut ogg = ogg_page(7, 0, 0x02, &[255], &packet[..split]);
        ogg.extend_from_slice(&ogg_page(
            7,
            1,
            0x05,
            &[(packet.len() - split) as u8],
            &packet[split..],
        ));
        let document = inspect_memory(ogg);
        let normalized = document
            .normalized
            .iter()
            .find(|field| field.key == "title")
            .expect("Vorbis title");
        assert_eq!(
            normalized.values[normalized.resolved_index].value,
            MetadataValue::Text("x".repeat(260))
        );
        assert_eq!(
            document
                .nodes
                .iter()
                .filter(|node| node.name.starts_with("Stream "))
                .count(),
            1,
            "pages are summarized under one logical stream"
        );
    }

    #[test]
    fn flac_vorbis_comment_is_normalized() {
        let item = b"TITLE=Forest";
        let mut comment = Vec::new();
        comment.extend_from_slice(&3u32.to_le_bytes());
        comment.extend_from_slice(b"UCS");
        comment.extend_from_slice(&1u32.to_le_bytes());
        comment.extend_from_slice(&(item.len() as u32).to_le_bytes());
        comment.extend_from_slice(item);
        let mut flac = b"fLaC".to_vec();
        flac.push(0x80 | 4);
        flac.extend_from_slice(&[
            ((comment.len() >> 16) & 0xff) as u8,
            ((comment.len() >> 8) & 0xff) as u8,
            (comment.len() & 0xff) as u8,
        ]);
        flac.extend_from_slice(&comment);
        let document = inspect_memory(flac);
        let title = document
            .normalized
            .iter()
            .find(|field| field.key == "title")
            .expect("FLAC title");
        assert_eq!(
            title.values[title.resolved_index].value,
            MetadataValue::Text("Forest".to_string())
        );
    }

    #[test]
    fn mp4_ilst_data_is_normalized_through_meta_fullbox() {
        let mut data_payload = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data_payload.extend_from_slice(b"Forest");
        let data = mp4_box(*b"data", data_payload);
        let title = mp4_box([0xa9, b'n', b'a', b'm'], data);
        let ilst = mp4_box(*b"ilst", title);
        let mut meta_payload = vec![0, 0, 0, 0];
        meta_payload.extend_from_slice(&ilst);
        let meta = mp4_box(*b"meta", meta_payload);
        let moov = mp4_box(*b"moov", meta);
        let ftyp = mp4_box(*b"ftyp", b"M4A \0\0\0\0M4A ".to_vec());
        let document = inspect_memory([ftyp, moov].concat());
        let title = document
            .normalized
            .iter()
            .find(|field| field.key == "title")
            .expect("MP4 title");
        assert_eq!(
            title.values[title.resolved_index].value,
            MetadataValue::Text("Forest".to_string())
        );
    }

    #[test]
    fn xml_doctype_is_rejected_without_expansion() {
        let xml = br#"<!DOCTYPE BWFXML [<!ENTITY x "boom">]><BWFXML><NOTE>&x;</NOTE></BWFXML>"#;
        let bytes = riff_with_chunks(&[(b"iXML", xml.to_vec())]);
        let document = inspect_memory(bytes);
        assert!(document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "xml.doctype_rejected"));
    }

    #[test]
    fn aiff_big_endian_chunk_size_and_padding_are_respected() {
        let mut body = Vec::new();
        body.extend_from_slice(b"NAME");
        body.extend_from_slice(&3u32.to_be_bytes());
        body.extend_from_slice(b"abc");
        body.push(0);
        body.extend_from_slice(b"ANNO");
        body.extend_from_slice(&2u32.to_be_bytes());
        body.extend_from_slice(b"ok");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FORM");
        bytes.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
        bytes.extend_from_slice(b"AIFF");
        bytes.extend_from_slice(&body);
        let document = inspect_memory(bytes);
        assert!(document.nodes.iter().any(|node| node.name == "NAME"));
        assert!(document.nodes.iter().any(|node| node.name == "ANNO"));
    }

    #[test]
    fn raw_fourcc_bytes_remain_lossless_in_column_keys() {
        assert_eq!(
            raw_column_key(ContainerKind::Mp4, "/ISO-BMFF/%A9nam"),
            "raw:mp4:ISO-BMFF/%A9nam"
        );
    }

    #[test]
    fn aswg_namespace_types_and_ucs_aliases_are_validated() {
        let xml = br#"<BWFXML><ASWG xmlns="http://sce/aswg/ixml"><catId>WOOD-HANDLE</catId><isGenerated>maybe</isGenerated></ASWG></BWFXML>"#;
        let document = inspect_memory(riff_with_chunks(&[(b"iXML", xml.to_vec())]));
        let cat_id = document
            .normalized
            .iter()
            .find(|field| field.key == "ucs.cat_id")
            .expect("normalized UCS CatID");
        assert_eq!(
            cat_id.values[cat_id.resolved_index].value,
            MetadataValue::Text("WOODHndl".to_string())
        );
        assert_eq!(
            document
                .normalized
                .iter()
                .find(|field| field.key == "ucs.category")
                .expect("derived UCS category")
                .values[0]
                .value,
            MetadataValue::Text("WOOD".to_string())
        );
        assert_eq!(
            document
                .normalized
                .iter()
                .find(|field| field.key == "ucs.subcategory")
                .expect("derived UCS subcategory")
                .values[0]
                .value,
            MetadataValue::Text("HANDLE".to_string())
        );
        assert!(document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ucs.alias"));
        assert!(document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "aswg.type"));
        assert!(!document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "aswg.namespace"));
    }

    #[test]
    fn unknown_ucs_cat_id_is_preserved_and_reported() {
        let xml =
            br#"<BWFXML><ASWG xmlns="http://sce/aswg/ixml"><catId>CUSTOMCat</catId></ASWG></BWFXML>"#;
        let document = inspect_memory(riff_with_chunks(&[(b"iXML", xml.to_vec())]));
        let cat_id = document
            .normalized
            .iter()
            .find(|field| field.key == "ucs.cat_id")
            .expect("normalized UCS CatID");
        assert_eq!(
            cat_id.values[cat_id.resolved_index].value,
            MetadataValue::Text("CUSTOMCat".to_string())
        );
        assert!(document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ucs.unknown_cat_id"));
    }

    #[test]
    fn aswg_without_namespace_is_reported() {
        let xml = br#"<BWFXML><ASWG><catId>WOODHndl</catId></ASWG></BWFXML>"#;
        let document = inspect_memory(riff_with_chunks(&[(b"iXML", xml.to_vec())]));
        assert!(document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "aswg.namespace"));
    }

    #[test]
    fn shift_jis_legacy_text_is_marked_as_guessed() {
        let (bytes, _, _) = SHIFT_JIS.encode("効果音");
        let (decoded, encoding, guessed) = decode_legacy_text(&bytes);
        assert_eq!(decoded, "効果音");
        assert_eq!(encoding, "Shift_JIS");
        assert!(guessed);
    }

    #[test]
    fn extract_never_uses_the_source_as_its_part_file() {
        let dir = std::env::temp_dir().join(format!(
            "neowaves-metadata-extract-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("payload.bin.part");
        let output = dir.join("payload.bin");
        std::fs::write(&source, b"source bytes").unwrap();
        let result = extract_payload(
            &source,
            PayloadRef {
                file_offset: 0,
                length: 6,
            },
            &output,
            true,
            None,
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"source bytes");
        let _ = std::fs::remove_dir_all(dir);
    }
}
