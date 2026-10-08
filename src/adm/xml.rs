//! The `axml` chunk: ITU-R BS.2076 XML, read as a stream.
//!
//! An Atmos-scale master can carry hundreds of megabytes of XML, one block
//! per object per few milliseconds, so the reader never holds the document:
//! it walks the events once, keeping only the elements a scene needs
//! (programme -> content -> object -> pack -> channel -> block, and the
//! stream / track / track UID references that tie a channel to a PCM track),
//! then [`build_scene`] resolves them against `chna`.
//!
//! A block's children the app does not interpret (width, diffuse,
//! channelLock, objectDivergence, zoneExclusion, ...) are kept verbatim in
//! the keyframe's `extras`, so an export that rewrites an edited element
//! writes them back.

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use quick_xml::Writer;

use super::chna::{self, Chna};
use super::common_defs;
use crate::spatial::scene::{
    dedup_keyframes, from_cart, to_cart, Coords, Element, ElementKind, Keyframe, ObjectScene,
    SourceShape,
};
use crate::spatial::ObjectFormat;

/// How many events pass between checks of the cancel flag and progress.
const EVENTS_PER_CHECK: usize = 4096;
/// The ID of a track UID that is silent by definition.
const SILENT_TRACK_UID: &str = "ATU_00000000";

#[derive(Clone, Debug, Default)]
pub struct RawBlock {
    pub id: String,
    pub rtime: Option<f64>,
    pub duration: Option<f64>,
    pub cartesian: Option<bool>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub z: Option<f32>,
    pub azimuth: Option<f32>,
    pub elevation: Option<f32>,
    pub distance: Option<f32>,
    pub gain: Option<f32>,
    pub jump: Option<bool>,
    pub interpolation: Option<f64>,
    pub speaker_labels: Vec<String>,
    pub extras: String,
}

#[derive(Clone, Debug, Default)]
pub struct RawChannel {
    pub id: String,
    pub name: String,
    pub type_code: Option<u16>,
    pub blocks: Vec<RawBlock>,
}

#[derive(Clone, Debug, Default)]
pub struct RawPack {
    pub id: String,
    pub name: String,
    pub type_code: Option<u16>,
    pub channels: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RawObject {
    pub id: String,
    pub name: String,
    pub start: Option<f64>,
    pub duration: Option<f64>,
    pub packs: Vec<String>,
    pub uids: Vec<String>,
    pub children: Vec<String>,
    pub gain: Option<f32>,
    pub mute: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RawNamed {
    pub id: String,
    pub name: String,
    pub refs: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RawTrackUid {
    pub id: String,
    pub track_format: Option<String>,
    pub channel_format: Option<String>,
}

/// What the reader kept of an `axml` document. IDs are upper-cased: files
/// and the common definitions disagree on the case of hex digits.
#[derive(Clone, Debug, Default)]
pub struct AxmlDoc {
    pub programmes: Vec<RawNamed>,
    pub contents: HashMap<String, RawNamed>,
    pub objects: HashMap<String, RawObject>,
    pub object_order: Vec<String>,
    pub packs: HashMap<String, RawPack>,
    pub channels: HashMap<String, RawChannel>,
    /// Stream format -> its channel format.
    pub streams: HashMap<String, String>,
    /// Track format -> its stream format.
    pub track_formats: HashMap<String, String>,
    pub track_uids: HashMap<String, RawTrackUid>,
    pub diagnostics: Vec<String>,
}

pub fn norm_id(id: &str) -> String {
    id.trim().to_ascii_uppercase()
}

/// `hh:mm:ss.zzzzz`, or BS.2076-2's `hh:mm:ss.zzzzzSddddd` (a fraction of
/// `zzzzz` over `ddddd`), in seconds.
pub fn parse_time(text: &str) -> Option<f64> {
    let text = text.trim();
    let (clock, fraction) = match text.split_once('S') {
        Some((clock, denominator)) => {
            let (whole, numerator) = clock.rsplit_once('.')?;
            let numerator: f64 = numerator.parse().ok()?;
            let denominator: f64 = denominator.parse().ok()?;
            if denominator <= 0.0 {
                return None;
            }
            (whole, numerator / denominator)
        }
        None => (text, 0.0),
    };
    let mut parts = clock.split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    // `inf` and `NaN` parse as numbers; a time is neither, nor negative.
    Some(hours * 3600.0 + minutes * 60.0 + seconds + fraction).filter(|t| t.is_finite() && *t >= 0.0)
}

/// A number as ADM writes one, or nothing. `inf` and `NaN` parse as floats
/// but mean nothing here -- and would reach the output callback's gains.
fn finite_f32(text: &str) -> Option<f32> {
    text.trim().parse::<f32>().ok().filter(|v| v.is_finite())
}

/// Seconds as ADM writes them: `hh:mm:ss.zzzzz`.
pub fn format_time(secs: f64) -> String {
    let secs = secs.max(0.0);
    let total_hundred_thousandths = (secs * 100_000.0).round() as u64;
    let fraction = total_hundred_thousandths % 100_000;
    let whole = total_hundred_thousandths / 100_000;
    format!(
        "{:02}:{:02}:{:02}.{:05}",
        whole / 3600,
        (whole / 60) % 60,
        whole % 60,
        fraction
    )
}

fn type_code(type_label: Option<&str>, type_definition: Option<&str>, id: &str) -> Option<u16> {
    if let Some(code) = type_label.and_then(|label| u16::from_str_radix(label.trim(), 16).ok()) {
        return Some(code);
    }
    let by_name = match type_definition.map(str::trim) {
        Some("DirectSpeakers") => Some(chna::TYPE_DIRECT_SPEAKERS),
        Some("Matrix") => Some(chna::TYPE_MATRIX),
        Some("Objects") => Some(chna::TYPE_OBJECTS),
        Some("HOA") => Some(chna::TYPE_HOA),
        Some("Binaural") => Some(chna::TYPE_BINAURAL),
        _ => None,
    };
    by_name.or_else(|| chna::type_of_id(id))
}

fn attributes(
    event: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for attribute in event.attributes().with_checks(false).flatten() {
        let key = String::from_utf8_lossy(attribute.key.local_name().as_ref()).into_owned();
        if let Ok(value) = attribute.decode_and_unescape_value(decoder) {
            out.insert(key, value.into_owned());
        }
    }
    out
}

fn parse_gain(text: &str, unit: Option<&str>) -> Option<f32> {
    let value = finite_f32(text)?;
    let gain = if unit.is_some_and(|unit| unit.eq_ignore_ascii_case("dB")) {
        10f32.powf(value / 20.0)
    } else {
        value
    };
    gain.is_finite().then_some(gain)
}

fn parse_flag(text: &str) -> bool {
    matches!(text.trim(), "1" | "true")
}

/// The element a leaf's text belongs to, with what it needs from its
/// start tag.
#[derive(Default)]
struct Leaf {
    name: String,
    attrs: HashMap<String, String>,
    text: String,
}

/// Where in the document the reader is.
#[derive(Default)]
struct Cursor {
    programme: Option<usize>,
    content: Option<String>,
    object: Option<String>,
    pack: Option<String>,
    channel: Option<String>,
    block: Option<RawBlock>,
    stream: Option<String>,
    track_format: Option<String>,
    track_uid: Option<String>,
}

/// Verbatim capture of a block child the reader does not interpret.
struct Capture {
    depth: usize,
    writer: Writer<Vec<u8>>,
}

/// Read an `axml` payload. `total_len` is only for progress, reported in
/// 0..1. Returns an error when cancelled.
pub fn parse_axml<R: BufRead>(
    input: R,
    total_len: u64,
    cancel: Option<&AtomicBool>,
    progress: &mut dyn FnMut(f32),
) -> Result<AxmlDoc> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(true);
    let mut doc = AxmlDoc::default();
    let mut buf = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut cursor = Cursor::default();
    let mut leaf: Option<Leaf> = None;
    let mut capture: Option<Capture> = None;
    let mut events = 0usize;
    loop {
        events += 1;
        if events % EVENTS_PER_CHECK == 0 {
            if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                bail!("cancelled");
            }
            if total_len > 0 {
                progress((reader.buffer_position() as f64 / total_len as f64).min(1.0) as f32);
            }
        }
        let event = match reader.read_event_into(&mut buf) {
            Ok(event) => event,
            Err(err) => {
                doc.diagnostics.push(format!(
                    "axml: XML error at byte {}: {err}",
                    reader.buffer_position()
                ));
                break;
            }
        };
        // A block child the reader does not interpret goes out verbatim.
        if let Some(cap) = capture.as_mut() {
            let ends_capture = matches!(&event, Event::End(_)) && stack.len() == cap.depth;
            let _ = cap.writer.write_event(event.borrow());
            match &event {
                Event::Start(e) => {
                    stack.push(String::from_utf8_lossy(e.local_name().as_ref()).into_owned())
                }
                Event::End(_) => {
                    stack.pop();
                }
                Event::Eof => break,
                _ => {}
            }
            if ends_capture {
                let cap = capture.take().unwrap();
                if let Some(block) = cursor.block.as_mut() {
                    block
                        .extras
                        .push_str(&String::from_utf8_lossy(&cap.writer.into_inner()));
                }
            }
            buf.clear();
            continue;
        }
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_empty = matches!(event, Event::Empty(_));
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                let attrs = attributes(e, reader.decoder());
                let parent = stack.last().map(String::as_str).unwrap_or("");
                // Inside a block, anything not interpreted is captured.
                if parent == "audioBlockFormat"
                    && !matches!(
                        name.as_str(),
                        "cartesian" | "position" | "gain" | "jumpPosition" | "speakerLabel"
                    )
                {
                    let mut writer = Writer::new(Vec::new());
                    let _ = writer.write_event(event.borrow());
                    if is_empty {
                        if let Some(block) = cursor.block.as_mut() {
                            block
                                .extras
                                .push_str(&String::from_utf8_lossy(&writer.into_inner()));
                        }
                    } else {
                        stack.push(name);
                        capture = Some(Capture {
                            depth: stack.len(),
                            writer,
                        });
                    }
                    buf.clear();
                    continue;
                }
                open_element(&mut doc, &mut cursor, &name, &attrs);
                leaf = Some(Leaf {
                    name: name.clone(),
                    attrs,
                    text: String::new(),
                });
                if is_empty {
                    if let Some(done) = leaf.take() {
                        close_element(&mut doc, &mut cursor, done);
                    }
                } else {
                    stack.push(name);
                }
            }
            Event::Text(ref text) => {
                if let Some(leaf) = leaf.as_mut() {
                    if let Ok(value) = text.xml_content() {
                        leaf.text.push_str(&value);
                    }
                }
            }
            Event::CData(ref text) => {
                if let Some(leaf) = leaf.as_mut() {
                    leaf.text.push_str(&String::from_utf8_lossy(text.as_ref()));
                }
            }
            Event::GeneralRef(ref reference) => {
                if let (Some(leaf), Ok(name)) = (leaf.as_mut(), reference.decode()) {
                    if let Ok(value) = quick_xml::escape::unescape(&format!("&{name};")) {
                        leaf.text.push_str(&value);
                    }
                }
            }
            Event::End(ref e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                stack.pop();
                let done = match leaf.take() {
                    Some(current) if current.name == name => current,
                    other => {
                        // A container closing: its own text (if any) is gone.
                        drop(other);
                        Leaf {
                            name,
                            ..Leaf::default()
                        }
                    }
                };
                close_element(&mut doc, &mut cursor, done);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if !stack.is_empty() {
        doc.diagnostics.push(format!(
            "axml: the document ends inside <{}> (truncated?)",
            stack.join("/")
        ));
    }
    progress(1.0);
    Ok(doc)
}

fn attr<'a>(attrs: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    attrs.get(key).map(String::as_str)
}

fn open_element(
    doc: &mut AxmlDoc,
    cursor: &mut Cursor,
    name: &str,
    attrs: &HashMap<String, String>,
) {
    match name {
        "audioProgramme" => {
            doc.programmes.push(RawNamed {
                id: norm_id(attr(attrs, "audioProgrammeID").unwrap_or("")),
                name: attr(attrs, "audioProgrammeName").unwrap_or("").to_string(),
                refs: Vec::new(),
            });
            cursor.programme = Some(doc.programmes.len() - 1);
        }
        "audioContent" => {
            let id = norm_id(attr(attrs, "audioContentID").unwrap_or(""));
            doc.contents.insert(
                id.clone(),
                RawNamed {
                    id: id.clone(),
                    name: attr(attrs, "audioContentName").unwrap_or("").to_string(),
                    refs: Vec::new(),
                },
            );
            cursor.content = Some(id);
        }
        "audioObject" => {
            let id = norm_id(attr(attrs, "audioObjectID").unwrap_or(""));
            doc.object_order.push(id.clone());
            doc.objects.insert(
                id.clone(),
                RawObject {
                    id: id.clone(),
                    name: attr(attrs, "audioObjectName").unwrap_or("").to_string(),
                    start: attr(attrs, "start").and_then(parse_time),
                    duration: attr(attrs, "duration").and_then(parse_time),
                    ..RawObject::default()
                },
            );
            cursor.object = Some(id);
        }
        "audioPackFormat" if cursor.pack.is_none() => {
            let id = norm_id(attr(attrs, "audioPackFormatID").unwrap_or(""));
            let type_code = type_code(attr(attrs, "typeLabel"), attr(attrs, "typeDefinition"), &id);
            doc.packs.insert(
                id.clone(),
                RawPack {
                    id: id.clone(),
                    name: attr(attrs, "audioPackFormatName").unwrap_or("").to_string(),
                    type_code,
                    channels: Vec::new(),
                },
            );
            cursor.pack = Some(id);
        }
        "audioChannelFormat" => {
            let id = norm_id(attr(attrs, "audioChannelFormatID").unwrap_or(""));
            let type_code = type_code(attr(attrs, "typeLabel"), attr(attrs, "typeDefinition"), &id);
            doc.channels.insert(
                id.clone(),
                RawChannel {
                    id: id.clone(),
                    name: attr(attrs, "audioChannelFormatName")
                        .unwrap_or("")
                        .to_string(),
                    type_code,
                    blocks: Vec::new(),
                },
            );
            cursor.channel = Some(id);
        }
        "audioBlockFormat" if cursor.channel.is_some() => {
            cursor.block = Some(RawBlock {
                id: attr(attrs, "audioBlockFormatID").unwrap_or("").to_string(),
                rtime: attr(attrs, "rtime").and_then(parse_time),
                duration: attr(attrs, "duration").and_then(parse_time),
                ..RawBlock::default()
            });
        }
        "audioStreamFormat" => {
            cursor.stream = Some(norm_id(attr(attrs, "audioStreamFormatID").unwrap_or("")));
        }
        "audioTrackFormat" => {
            cursor.track_format = Some(norm_id(attr(attrs, "audioTrackFormatID").unwrap_or("")));
        }
        "audioTrackUID" => {
            let id = norm_id(attr(attrs, "UID").unwrap_or(""));
            doc.track_uids.insert(
                id.clone(),
                RawTrackUid {
                    id: id.clone(),
                    ..RawTrackUid::default()
                },
            );
            cursor.track_uid = Some(id);
        }
        _ => {}
    }
}

fn close_element(doc: &mut AxmlDoc, cursor: &mut Cursor, leaf: Leaf) {
    let text = leaf.text.trim();
    let Leaf { name, attrs, .. } = &leaf;
    // Inside a block: its interpreted children.
    if let Some(block) = cursor.block.as_mut() {
        match name.as_str() {
            "cartesian" => block.cartesian = Some(parse_flag(text)),
            "position" => {
                let value = finite_f32(text);
                match attr(attrs, "coordinate").unwrap_or("") {
                    "X" | "x" => block.x = value,
                    "Y" | "y" => block.y = value,
                    "Z" | "z" => block.z = value,
                    "azimuth" => block.azimuth = value,
                    "elevation" => block.elevation = value,
                    "distance" => block.distance = value,
                    _ => {}
                }
            }
            "gain" => block.gain = parse_gain(text, attr(attrs, "gainUnit")),
            "jumpPosition" => {
                block.jump = Some(parse_flag(text));
                block.interpolation = attr(attrs, "interpolationLength")
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .filter(|secs| secs.is_finite() && *secs >= 0.0);
            }
            "speakerLabel" => block.speaker_labels.push(text.to_string()),
            "audioBlockFormat" => {
                let block = cursor.block.take().unwrap();
                if let Some(channel) = cursor
                    .channel
                    .as_ref()
                    .and_then(|id| doc.channels.get_mut(id))
                {
                    channel.blocks.push(block);
                }
            }
            _ => {}
        }
        return;
    }
    match name.as_str() {
        "audioContentIDRef" => {
            if let Some(programme) = cursor.programme.and_then(|i| doc.programmes.get_mut(i)) {
                programme.refs.push(norm_id(text));
            }
        }
        "audioObjectIDRef" => {
            if let Some(object) = cursor
                .object
                .as_ref()
                .and_then(|id| doc.objects.get_mut(id))
            {
                object.children.push(norm_id(text));
            } else if let Some(content) = cursor
                .content
                .as_ref()
                .and_then(|id| doc.contents.get_mut(id))
            {
                content.refs.push(norm_id(text));
            }
        }
        "audioPackFormatIDRef" => {
            if let Some(object) = cursor
                .object
                .as_ref()
                .and_then(|id| doc.objects.get_mut(id))
            {
                object.packs.push(norm_id(text));
            }
        }
        "audioTrackUIDRef" => {
            if let Some(object) = cursor
                .object
                .as_ref()
                .and_then(|id| doc.objects.get_mut(id))
            {
                object.uids.push(norm_id(text));
            }
        }
        "gain" => {
            if let Some(object) = cursor
                .object
                .as_ref()
                .and_then(|id| doc.objects.get_mut(id))
            {
                object.gain = parse_gain(text, attr(attrs, "gainUnit"));
            }
        }
        "mute" => {
            if let Some(object) = cursor
                .object
                .as_ref()
                .and_then(|id| doc.objects.get_mut(id))
            {
                object.mute = parse_flag(text);
            }
        }
        "audioChannelFormatIDRef" => {
            if let Some(pack) = cursor.pack.as_ref().and_then(|id| doc.packs.get_mut(id)) {
                pack.channels.push(norm_id(text));
            } else if let Some(stream) = cursor.stream.clone() {
                doc.streams.insert(stream, norm_id(text));
            } else if let Some(uid) = cursor
                .track_uid
                .as_ref()
                .and_then(|id| doc.track_uids.get_mut(id))
            {
                uid.channel_format = Some(norm_id(text));
            }
        }
        "audioStreamFormatIDRef" => {
            if let Some(track_format) = cursor.track_format.clone() {
                doc.track_formats.insert(track_format, norm_id(text));
            }
        }
        "audioTrackFormatIDRef" => {
            if let Some(uid) = cursor
                .track_uid
                .as_ref()
                .and_then(|id| doc.track_uids.get_mut(id))
            {
                uid.track_format = Some(norm_id(text));
            }
        }
        "audioProgramme" => cursor.programme = None,
        "audioContent" => cursor.content = None,
        "audioObject" => cursor.object = None,
        "audioPackFormat" => cursor.pack = None,
        "audioChannelFormat" => cursor.channel = None,
        "audioStreamFormat" => cursor.stream = None,
        "audioTrackFormat" => cursor.track_format = None,
        "audioTrackUID" => cursor.track_uid = None,
        _ => {}
    }
}

/// The channel format a track UID plays, through whichever chain the file
/// uses: the UID's own reference, `chna`'s `AC_` reference, or
/// track format -> stream format -> channel format (the common definitions
/// standing in for streams the file does not write out).
fn channel_for_uid(doc: &AxmlDoc, entry: &chna::ChnaEntry) -> Option<String> {
    if let Some(channel) = doc
        .track_uids
        .get(&norm_id(&entry.uid))
        .and_then(|uid| uid.channel_format.clone())
    {
        return Some(channel);
    }
    let track_ref = norm_id(&entry.track_ref);
    if track_ref.starts_with("AC_") {
        return Some(track_ref);
    }
    let track_format = doc
        .track_uids
        .get(&norm_id(&entry.uid))
        .and_then(|uid| uid.track_format.clone())
        .unwrap_or(track_ref);
    if let Some(channel) = doc
        .track_formats
        .get(&track_format)
        .and_then(|stream| doc.streams.get(stream))
    {
        return Some(channel.clone());
    }
    if let Some(common) = common_defs::channel_for_track_format(&track_format) {
        return Some(norm_id(&common));
    }
    // Last resort, the naming convention every writer follows:
    // `AT_yyyyxxxx_nn` is a track of `AC_yyyyxxxx`. Reached when the file's
    // stream and track definitions are missing (a truncated `axml`).
    track_format
        .strip_prefix("AT_")
        .and_then(|rest| rest.get(..8))
        .map(|digits| format!("AC_{digits}"))
}

/// A channel the file names but does not define: a common definition
/// becomes a static DirectSpeakers channel at its nominal direction.
fn common_channel(id: &str) -> Option<RawChannel> {
    let common = common_defs::common_channel(id)?;
    Some(RawChannel {
        id: id.to_string(),
        name: common.name.to_string(),
        type_code: Some(chna::TYPE_DIRECT_SPEAKERS),
        blocks: vec![RawBlock {
            azimuth: Some(common.azimuth),
            elevation: Some(common.elevation),
            distance: Some(1.0),
            speaker_labels: vec![common.label.to_string()],
            ..RawBlock::default()
        }],
    })
}

fn block_keyframe(block: &RawBlock, coords: Coords, object_start: f64) -> Keyframe {
    let block_cart = block.cartesian.unwrap_or(false)
        || (block.azimuth.is_none() && (block.x.is_some() || block.y.is_some()));
    let native = if block_cart {
        [
            block.x.unwrap_or(0.0),
            block.y.unwrap_or(0.0),
            block.z.unwrap_or(0.0),
        ]
    } else {
        [
            block.azimuth.unwrap_or(0.0),
            block.elevation.unwrap_or(0.0),
            block.distance.unwrap_or(1.0),
        ]
    };
    let block_coords = if block_cart {
        Coords::Cartesian
    } else {
        Coords::Polar
    };
    let pos = if block_coords == coords {
        native
    } else {
        from_cart(coords, to_cart(block_coords, native))
    };
    let (secs, ramp_secs) = match (block.rtime, block.duration) {
        (None, None) => (object_start, 0.0),
        (rtime, duration) => {
            let start = object_start + rtime.unwrap_or(0.0);
            let ramp = if block.jump.unwrap_or(false) {
                block.interpolation.unwrap_or(0.0)
            } else {
                duration.unwrap_or(0.0)
            };
            (start + ramp, ramp.max(0.0))
        }
    };
    Keyframe {
        secs,
        ramp_secs,
        pos,
        gain: block.gain.unwrap_or(1.0),
        extras: (!block.extras.is_empty()).then(|| Arc::from(block.extras.as_str())),
    }
}

/// The objects a scene plays: the first programme's, depth first through
/// nested objects; every top-level object when there is no programme.
fn programme_objects(
    doc: &AxmlDoc,
    diagnostics: &mut Vec<String>,
) -> (Option<String>, Vec<(String, String)>) {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    fn walk(
        doc: &AxmlDoc,
        id: &str,
        group: &str,
        out: &mut Vec<(String, String)>,
        seen: &mut HashSet<String>,
    ) {
        if !seen.insert(id.to_string()) {
            return;
        }
        let Some(object) = doc.objects.get(id) else {
            return;
        };
        out.push((id.to_string(), group.to_string()));
        for child in &object.children {
            walk(doc, child, group, out, seen);
        }
    }
    if let Some(programme) = doc.programmes.first() {
        if doc.programmes.len() > 1 {
            diagnostics.push(format!(
                "{} programmes; playing the first ({})",
                doc.programmes.len(),
                if programme.name.is_empty() {
                    &programme.id
                } else {
                    &programme.name
                }
            ));
        }
        for content_id in &programme.refs {
            let Some(content) = doc.contents.get(content_id) else {
                diagnostics.push(format!("programme refers to missing content {content_id}"));
                continue;
            };
            let group = if content.name.is_empty() {
                &content.id
            } else {
                &content.name
            };
            for object_id in &content.refs {
                walk(doc, object_id, group, &mut out, &mut seen);
            }
        }
        let name = if programme.name.is_empty() {
            programme.id.clone()
        } else {
            programme.name.clone()
        };
        return (Some(name), out);
    }
    let nested: HashSet<&String> = doc
        .objects
        .values()
        .flat_map(|o| o.children.iter())
        .collect();
    for id in &doc.object_order {
        if !nested.contains(id) {
            let name = doc
                .objects
                .get(id)
                .map(|o| o.name.clone())
                .unwrap_or_default();
            walk(doc, id, &name, &mut out, &mut seen);
        }
    }
    (None, out)
}

/// Resolve a parsed document against `chna` into a playable scene.
pub fn build_scene(mut doc: AxmlDoc, chna: &Chna, shape: SourceShape) -> ObjectScene {
    let mut diagnostics = std::mem::take(&mut doc.diagnostics);
    let (programme, objects) = programme_objects(&doc, &mut diagnostics);
    let mut elements = Vec::new();
    let mut unrendered = 0usize;
    for (object_id, group) in objects {
        let Some(object) = doc.objects.get(&object_id) else {
            continue;
        };
        let start = object.start.unwrap_or(0.0);
        let active = match (object.start, object.duration) {
            (None, None) => None,
            (_, duration) => Some((start, duration.map(|d| start + d).unwrap_or(f64::INFINITY))),
        };
        let object_gain = if object.mute {
            0.0
        } else {
            object.gain.unwrap_or(1.0)
        };
        let channels_in_pack = object
            .packs
            .iter()
            .filter_map(|pack| doc.packs.get(pack))
            .map(|pack| pack.channels.len())
            .sum::<usize>()
            .max(object.uids.len());
        for uid in &object.uids {
            if uid.eq_ignore_ascii_case(SILENT_TRACK_UID) {
                continue;
            }
            let Some(entry) = chna.entry_for_uid(uid) else {
                diagnostics.push(format!("{object_id}: track UID {uid} is not in chna"));
                continue;
            };
            let track = entry.track_index as u32 - 1;
            if track >= shape.tracks {
                diagnostics.push(format!(
                    "{object_id}: {uid} names track {} of {}",
                    entry.track_index, shape.tracks
                ));
                continue;
            }
            let Some(channel_id) = channel_for_uid(&doc, entry) else {
                diagnostics.push(format!("{object_id}: no channel format for {uid}"));
                continue;
            };
            let channel = match doc
                .channels
                .get(&channel_id)
                .cloned()
                .or_else(|| common_channel(&channel_id))
            {
                Some(channel) => channel,
                None => {
                    diagnostics.push(format!(
                        "{object_id}: channel format {channel_id} is missing"
                    ));
                    continue;
                }
            };
            let type_code = channel.type_code.or_else(|| chna::type_of_id(&channel.id));
            let is_object = match type_code {
                Some(chna::TYPE_OBJECTS) => true,
                Some(chna::TYPE_DIRECT_SPEAKERS) => false,
                _ => {
                    unrendered += 1;
                    continue;
                }
            };
            let first = channel.blocks.first();
            let coords = if first.is_some_and(|b| b.cartesian == Some(true)) {
                Coords::Cartesian
            } else if is_object && first.is_some_and(|b| b.azimuth.is_none() && b.x.is_some()) {
                Coords::Cartesian
            } else {
                Coords::Polar
            };
            let kind = if is_object {
                ElementKind::Object
            } else {
                let common = first
                    .and_then(|b| {
                        b.speaker_labels
                            .iter()
                            .find_map(|l| common_defs::channel_for_label(l))
                    })
                    .or_else(|| common_defs::common_channel(&channel.id));
                let label = first
                    .and_then(|b| b.speaker_labels.first())
                    .map(|l| l.rsplit(':').next().unwrap_or(l).to_string())
                    .or_else(|| common.map(|c| c.label.to_string()))
                    .unwrap_or_else(|| channel.name.clone());
                ElementKind::Bed {
                    speaker: common.and_then(|c| c.speaker),
                    label: Arc::from(label),
                }
            };
            let mut keyframes: Vec<Keyframe> = channel
                .blocks
                .iter()
                .map(|block| block_keyframe(block, coords, start))
                .collect();
            if keyframes.is_empty() {
                let nominal = common_defs::common_channel(&channel.id)
                    .map(|c| [c.azimuth, c.elevation, 1.0])
                    .unwrap_or([0.0, 0.0, 1.0]);
                keyframes.push(Keyframe::at(
                    start,
                    from_cart(coords, to_cart(Coords::Polar, nominal)),
                ));
            }
            keyframes.sort_by(|a, b| a.secs.total_cmp(&b.secs));
            let keyframes = dedup_keyframes(keyframes);
            let object_name = if object.name.is_empty() {
                object_id.clone()
            } else {
                object.name.clone()
            };
            let name = if channels_in_pack > 1 && !channel.name.is_empty() {
                format!("{object_name} / {}", channel.name)
            } else {
                object_name
            };
            elements.push(Element {
                key: Arc::from(format!("adm:{object_id}:{}", channel.id)),
                name: Arc::from(name),
                group: Arc::from(group.as_str()),
                track,
                kind,
                coords,
                keyframes: Arc::from(keyframes),
                active,
                gain: object_gain,
            });
        }
    }
    if unrendered > 0 {
        diagnostics.push(format!(
            "{unrendered} HOA / Matrix / Binaural channel(s) are not rendered"
        ));
    }
    let mut scene = ObjectScene {
        format: ObjectFormat::Adm,
        shape,
        programme: programme.map(Arc::from),
        elements,
        diagnostics,
    };
    scene.sort_elements();
    scene
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_parse_in_both_forms_and_format_back() {
        assert_eq!(parse_time("00:00:01.50000"), Some(1.5));
        assert_eq!(parse_time("01:02:03.00000"), Some(3723.0));
        let fractional = parse_time("00:00:00.00001S48000").unwrap();
        assert!((fractional - 1.0 / 48_000.0).abs() < 1e-12);
        assert_eq!(parse_time("nonsense"), None);
        assert_eq!(format_time(3723.25), "01:02:03.25000");
        assert_eq!(parse_time(&format_time(12.34567)), Some(12.34567));
    }

    #[test]
    fn gains_in_db_become_linear() {
        assert_eq!(parse_gain("0.5", None), Some(0.5));
        assert_eq!(parse_gain("NaN", None), None);
        assert_eq!(parse_gain("inf", None), None);
        assert_eq!(parse_gain("4000", Some("dB")), None, "overflows to infinity");
        assert_eq!(parse_time("inf:00:00"), None);
        assert_eq!(parse_time("-01:00:00"), None, "a time before zero");
        assert_eq!(finite_f32("NaN"), None);
        let g = parse_gain("-6.0206", Some("dB")).unwrap();
        assert!((g - 0.5).abs() < 1e-4);
    }
}
