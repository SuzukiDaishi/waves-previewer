use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::helpers::sanitize_filename_component;
use super::types::{
    AppliedEffectGraphStamp, CachedEdit, EffectGraphAudioBus, EffectGraphChannelFlowHint,
    EffectGraphChannelLayout, EffectGraphChannelLayoutEntry, EffectGraphCombineMode,
    EffectGraphDebugPreview, EffectGraphDebugViewState, EffectGraphDocument, EffectGraphEdge,
    EffectGraphLibraryEntry, EffectGraphNode, EffectGraphNodeData, EffectGraphNodeKind,
    EffectGraphNodeRunPhase, EffectGraphNodeRunStatus, EffectGraphPendingAction,
    EffectGraphPortKey, EffectGraphPredictedFormat, EffectGraphRunMode, EffectGraphSeverity,
    EffectGraphSpectrumMode, EffectGraphTemplateFile, EffectGraphUndoState,
    EffectGraphValidationIssue, EffectGraphWorkerEvent, MediaSource, SpectrogramConfig,
    SpectrogramScale, ToolKind, ToolState, UndoScope, WorkspaceView,
};
use super::WavesPreviewer;
use crate::audio::AudioBuffer;
use crate::markers::MarkerEntry;

const EFFECT_GRAPH_SCHEMA_VERSION: u32 = 2;
const EFFECT_GRAPH_CLIPBOARD_VERSION: u32 = 1;
const EFFECT_GRAPH_CLIPBOARD_MARKER: &str = "neowaves://effect-graph";
const EFFECT_GRAPH_EMBEDDED_SAMPLE_LABEL: &str = "Embedded sample (10s chirp + white noise)";
const EFFECT_GRAPH_EMBEDDED_SAMPLE_WORKER_PATH: &str = "[embedded effect graph sample]";
const EFFECT_GRAPH_EMBEDDED_SAMPLE_WAV: &[u8] =
    include_bytes!("assets/effect_graph_test_sample.wav");

static EFFECT_GRAPH_EMBEDDED_SAMPLE_DECODED: OnceLock<Result<(Vec<Vec<f32>>, u32), String>> =
    OnceLock::new();

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct EffectGraphClipboardPayload {
    version: u32,
    origin: [f32; 2],
    nodes: Vec<EffectGraphNode>,
    edges: Vec<EffectGraphEdge>,
}

#[derive(Clone)]
struct EffectGraphWorkerInput {
    path: PathBuf,
    input_bus: Option<EffectGraphAudioBus>,
    bit_depth: Option<crate::wave::WavBitDepth>,
    monitor_sr: u32,
    resample_quality: crate::wave::ResampleQuality,
}

#[derive(Clone)]
struct AdaptivePlacementBlock {
    anchor_slot: Option<usize>,
    channels: Vec<Vec<f32>>,
    socket_order: usize,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn decode_embedded_effect_graph_sample() -> Result<(Vec<Vec<f32>>, u32), String> {
    let cursor = Cursor::new(EFFECT_GRAPH_EMBEDDED_SAMPLE_WAV);
    let mut reader = hound::WavReader::new(cursor)
        .map_err(|err| format!("embedded sample open failed: {err}"))?;
    let spec = reader.spec();
    let channel_count = usize::from(spec.channels.max(1));
    let mut channels = vec![Vec::new(); channel_count];
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for (index, sample) in reader.samples::<f32>().enumerate() {
                let value =
                    sample.map_err(|err| format!("embedded sample decode failed: {err}"))?;
                channels[index % channel_count].push(value.clamp(-1.0, 1.0));
            }
        }
        hound::SampleFormat::Int => {
            let max_abs = ((1_i64 << spec.bits_per_sample.saturating_sub(1)) - 1).max(1) as f32;
            for (index, sample) in reader.samples::<i32>().enumerate() {
                let value =
                    sample.map_err(|err| format!("embedded sample decode failed: {err}"))?;
                channels[index % channel_count].push((value as f32 / max_abs).clamp(-1.0, 1.0));
            }
        }
    }
    Ok((channels, spec.sample_rate.max(1)))
}

fn embedded_effect_graph_sample_channels() -> Result<(Vec<Vec<f32>>, u32), String> {
    EFFECT_GRAPH_EMBEDDED_SAMPLE_DECODED
        .get_or_init(decode_embedded_effect_graph_sample)
        .clone()
}

fn default_tool_state() -> ToolState {
    ToolState {
        fade_in_ms: 0.0,
        fade_out_ms: 0.0,
        gain_db: 0.0,
        normalize_target_db: -6.0,
        loudness_target_lufs: -14.0,
        pitch_semitones: 0.0,
        stretch_rate: 1.0,
        loop_repeat: 2,
    }
}

fn effect_graph_default_node_size(kind: EffectGraphNodeKind) -> [f32; 2] {
    match kind {
        EffectGraphNodeKind::Input | EffectGraphNodeKind::Output => [260.0, 136.0],
        EffectGraphNodeKind::Duplicate => [250.0, 152.0],
        EffectGraphNodeKind::MonoMix => [320.0, 226.0],
        EffectGraphNodeKind::SplitChannels => [260.0, 220.0],
        EffectGraphNodeKind::CombineChannels => [300.0, 250.0],
        EffectGraphNodeKind::DebugWaveform => [340.0, 250.0],
        EffectGraphNodeKind::DebugSpectrum => [360.0, 300.0],
        EffectGraphNodeKind::Gain
        | EffectGraphNodeKind::PitchShift
        | EffectGraphNodeKind::TimeStretch
        | EffectGraphNodeKind::Speed => [280.0, 182.0],
    }
}

fn ensure_effect_graph_node_layout(document: &mut EffectGraphDocument) {
    for node in document.nodes.iter_mut() {
        let [min_w, min_h] = effect_graph_default_node_size(node.data.kind());
        if node.ui_size[0] < min_w {
            node.ui_size[0] = min_w;
        }
        if node.ui_size[1] < min_h {
            node.ui_size[1] = min_h;
        }
    }
}

fn mixdown_channels_local(channels: &[Vec<f32>]) -> Vec<f32> {
    let len = channels
        .iter()
        .map(|channel| channel.len())
        .max()
        .unwrap_or(0);
    if len == 0 {
        return Vec::new();
    }
    if channels.is_empty() {
        return vec![0.0; len];
    }
    let mut mono = vec![0.0f32; len];
    for channel in channels.iter() {
        for (index, sample) in channel.iter().enumerate() {
            mono[index] += *sample;
        }
    }
    let scale = 1.0 / channels.len().max(1) as f32;
    for sample in mono.iter_mut() {
        *sample *= scale;
    }
    mono
}

fn mono_mix_channels_with_ignored(
    channels: &[Vec<f32>],
    ignored_channels: &[bool],
) -> (Vec<f32>, usize) {
    let len = channels_frame_len(channels);
    if len == 0 {
        return (Vec::new(), 0);
    }
    let mut mono = vec![0.0f32; len];
    let mut included_count = 0usize;
    for (channel_index, channel) in channels.iter().enumerate() {
        if ignored_channels
            .get(channel_index)
            .copied()
            .unwrap_or(false)
        {
            continue;
        }
        included_count = included_count.saturating_add(1);
        for (index, sample) in channel.iter().enumerate() {
            mono[index] += *sample;
        }
    }
    if included_count > 0 {
        let scale = 1.0 / included_count as f32;
        for sample in mono.iter_mut() {
            *sample *= scale;
        }
    }
    (mono, included_count)
}

fn effect_graph_channel_label(channel_index: usize) -> String {
    const LABELS: [&str; 8] = ["L", "R", "C", "LFE", "Ls", "Rs", "Lrs", "Rrs"];
    if let Some(label) = LABELS.get(channel_index) {
        format!("Ch{} {label}", channel_index.saturating_add(1))
    } else {
        format!("Ch{}", channel_index.saturating_add(1))
    }
}

fn channels_frame_len(channels: &[Vec<f32>]) -> usize {
    channels
        .iter()
        .map(|channel| channel.len())
        .max()
        .unwrap_or(0)
}

fn pad_channels_with_silence(channels: &mut [Vec<f32>], len: usize) {
    for channel in channels.iter_mut() {
        if channel.len() < len {
            channel.resize(len, 0.0);
        }
    }
}

fn make_dense_layout(channel_count: usize) -> EffectGraphChannelLayout {
    EffectGraphChannelLayout {
        declared_width: channel_count,
        entries: vec![EffectGraphChannelLayoutEntry::Dense; channel_count],
    }
}

fn dense_audio_bus(channels: Vec<Vec<f32>>, sample_rate: u32) -> EffectGraphAudioBus {
    EffectGraphAudioBus {
        channel_layout: make_dense_layout(channels.len()),
        channels,
        sample_rate: sample_rate.max(1),
    }
}

fn split_slot_entry_for_index(
    input_layout: &EffectGraphChannelLayout,
    channel_index: usize,
) -> EffectGraphChannelLayoutEntry {
    match input_layout.entries.get(channel_index) {
        Some(EffectGraphChannelLayoutEntry::Dense) => EffectGraphChannelLayoutEntry::Slotted {
            slot_index: channel_index,
        },
        Some(EffectGraphChannelLayoutEntry::AutoPlaced { .. }) => {
            EffectGraphChannelLayoutEntry::Slotted {
                slot_index: channel_index,
            }
        }
        Some(EffectGraphChannelLayoutEntry::Slotted { slot_index }) => {
            EffectGraphChannelLayoutEntry::Slotted {
                slot_index: *slot_index,
            }
        }
        Some(EffectGraphChannelLayoutEntry::Vacant { requested_slot }) => {
            EffectGraphChannelLayoutEntry::Vacant {
                requested_slot: *requested_slot,
            }
        }
        None => EffectGraphChannelLayoutEntry::Vacant {
            requested_slot: channel_index,
        },
    }
}

fn make_split_output_layout(
    input_layout: &EffectGraphChannelLayout,
    output_port_index: usize,
) -> EffectGraphChannelLayout {
    let entry = split_slot_entry_for_index(input_layout, output_port_index);
    let declared_width = match entry {
        EffectGraphChannelLayoutEntry::Slotted { slot_index } => input_layout
            .declared_width
            .max(slot_index.saturating_add(1)),
        _ => input_layout.declared_width,
    };
    EffectGraphChannelLayout {
        declared_width,
        entries: vec![entry],
    }
}

fn silent_mono_bus(
    len: usize,
    sample_rate: u32,
    declared_width: usize,
    requested_slot: usize,
) -> EffectGraphAudioBus {
    EffectGraphAudioBus {
        channels: vec![vec![0.0; len]],
        sample_rate: sample_rate.max(1),
        channel_layout: EffectGraphChannelLayout {
            declared_width,
            entries: vec![EffectGraphChannelLayoutEntry::Vacant { requested_slot }],
        },
    }
}

fn format_only_audio_bus(channel_count: usize, sample_rate: u32) -> EffectGraphAudioBus {
    dense_audio_bus(vec![vec![0.0; 1]; channel_count.max(1)], sample_rate.max(1))
}

fn make_duplicate_output_layout(
    bus: &EffectGraphAudioBus,
    branch_group_id: &str,
) -> EffectGraphChannelLayout {
    let mut entries = Vec::with_capacity(bus.channels.len());
    for channel_index in 0..bus.channels.len() {
        let entry = match bus.channel_layout.entries.get(channel_index) {
            Some(EffectGraphChannelLayoutEntry::Dense) | None => {
                EffectGraphChannelLayoutEntry::AutoPlaced {
                    origin_slot: Some(channel_index),
                    branch_group_id: branch_group_id.to_string(),
                    branch_channel_index: channel_index,
                }
            }
            Some(EffectGraphChannelLayoutEntry::Slotted { slot_index }) => {
                EffectGraphChannelLayoutEntry::AutoPlaced {
                    origin_slot: Some(*slot_index),
                    branch_group_id: branch_group_id.to_string(),
                    branch_channel_index: channel_index,
                }
            }
            Some(EffectGraphChannelLayoutEntry::Vacant { requested_slot }) => {
                EffectGraphChannelLayoutEntry::Vacant {
                    requested_slot: *requested_slot,
                }
            }
            Some(EffectGraphChannelLayoutEntry::AutoPlaced {
                origin_slot,
                branch_channel_index,
                ..
            }) => EffectGraphChannelLayoutEntry::AutoPlaced {
                origin_slot: *origin_slot,
                branch_group_id: branch_group_id.to_string(),
                branch_channel_index: *branch_channel_index,
            },
        };
        entries.push(entry);
    }
    EffectGraphChannelLayout {
        declared_width: bus.channel_layout.declared_width,
        entries,
    }
}

fn normalize_audio_bus_lengths(bus: &mut EffectGraphAudioBus) {
    let max_len = channels_frame_len(&bus.channels);
    pad_channels_with_silence(&mut bus.channels, max_len);
}

fn resample_audio_bus(
    bus: &EffectGraphAudioBus,
    target_sample_rate: u32,
    quality: crate::wave::ResampleQuality,
) -> EffectGraphAudioBus {
    if bus.sample_rate == target_sample_rate || bus.channels.is_empty() {
        return EffectGraphAudioBus {
            channels: bus.channels.clone(),
            sample_rate: target_sample_rate.max(1),
            channel_layout: bus.channel_layout.clone(),
        };
    }
    let mut channels = bus.channels.clone();
    for channel in channels.iter_mut() {
        *channel =
            crate::wave::resample_quality(channel, bus.sample_rate, target_sample_rate, quality);
    }
    EffectGraphAudioBus {
        channels,
        sample_rate: target_sample_rate.max(1),
        channel_layout: bus.channel_layout.clone(),
    }
}

fn monitor_channels_from_bus(bus: &EffectGraphAudioBus) -> Vec<Vec<f32>> {
    match bus.channels.len() {
        0 => vec![Vec::new()],
        1 => vec![bus.channels[0].clone(), bus.channels[0].clone()],
        2 => bus.channels.clone(),
        _ => {
            let len = channels_frame_len(&bus.channels);
            let mut left = vec![0.0f32; len];
            let mut right = vec![0.0f32; len];
            let mut left_count = 0usize;
            let mut right_count = 0usize;
            for (index, channel) in bus.channels.iter().enumerate() {
                if index % 2 == 0 {
                    left_count += 1;
                    for (sample_index, sample) in channel.iter().enumerate() {
                        left[sample_index] += *sample;
                    }
                } else {
                    right_count += 1;
                    for (sample_index, sample) in channel.iter().enumerate() {
                        right[sample_index] += *sample;
                    }
                }
            }
            if left_count > 0 {
                let scale = 1.0 / left_count as f32;
                for sample in left.iter_mut() {
                    *sample *= scale;
                }
            }
            if right_count > 0 {
                let scale = 1.0 / right_count as f32;
                for sample in right.iter_mut() {
                    *sample *= scale;
                }
            }
            if left_count == 0 {
                left.clone_from(&right);
            }
            if right_count == 0 {
                right.clone_from(&left);
            }
            vec![left, right]
        }
    }
}

fn monitor_channels_from_bus_at_rate(
    bus: &EffectGraphAudioBus,
    target_sample_rate: u32,
    quality: crate::wave::ResampleQuality,
) -> Vec<Vec<f32>> {
    let target_sample_rate = target_sample_rate.max(1);
    let mut monitor_channels = monitor_channels_from_bus(bus);
    if bus.sample_rate != target_sample_rate {
        for channel in monitor_channels.iter_mut() {
            *channel = crate::wave::resample_quality(
                channel,
                bus.sample_rate,
                target_sample_rate,
                quality,
            );
        }
    }
    monitor_channels
}

fn effect_graph_debug_spectrogram_config(mode: EffectGraphSpectrumMode) -> SpectrogramConfig {
    let mut cfg = SpectrogramConfig::default();
    cfg.fft_size = 1024;
    cfg.max_frames = 256;
    cfg.max_freq_hz = 0.0;
    match mode {
        EffectGraphSpectrumMode::Linear => {
            cfg.scale = SpectrogramScale::Linear;
        }
        EffectGraphSpectrumMode::Log => {
            cfg.scale = SpectrogramScale::Log;
        }
        EffectGraphSpectrumMode::Mel => {
            cfg.mel_scale = SpectrogramScale::Linear;
        }
    }
    cfg
}

#[derive(Clone, Debug)]
enum EffectGraphRuntimeEvent {
    NodeStarted(String),
    NodeFinished {
        node_id: String,
        elapsed_ms: f32,
    },
    NodeLog {
        node_id: String,
        severity: EffectGraphSeverity,
        message: String,
    },
    NodeDebugPreview {
        node_id: String,
        preview: EffectGraphDebugPreview,
    },
}

fn clamp_node_data(data: &mut EffectGraphNodeData) {
    match data {
        EffectGraphNodeData::Gain { gain_db } => {
            *gain_db = gain_db.clamp(-24.0, 24.0);
        }
        EffectGraphNodeData::MonoMix { ignored_channels } => {
            ignored_channels.truncate(8);
            if ignored_channels.len() < 8 {
                ignored_channels.resize(8, false);
            }
        }
        EffectGraphNodeData::PitchShift { semitones } => {
            *semitones = semitones.clamp(-12.0, 12.0);
        }
        EffectGraphNodeData::TimeStretch { rate } | EffectGraphNodeData::Speed { rate } => {
            *rate = rate.clamp(0.25, 4.0);
        }
        EffectGraphNodeData::DebugWaveform { zoom } => {
            *zoom = zoom.clamp(1.0, 32.0);
        }
        EffectGraphNodeData::DebugSpectrum { zoom, .. } => {
            *zoom = zoom.clamp(1.0, 16.0);
        }
        EffectGraphNodeData::Input
        | EffectGraphNodeData::Output
        | EffectGraphNodeData::Duplicate
        | EffectGraphNodeData::SplitChannels
        | EffectGraphNodeData::CombineChannels => {}
    }
}

fn clone_sanitized_document(document: &EffectGraphDocument) -> EffectGraphDocument {
    let mut graph = document.clone();
    graph.schema_version = EFFECT_GRAPH_SCHEMA_VERSION;
    graph.canvas.zoom = graph.canvas.zoom.clamp(0.25, 2.5);
    ensure_effect_graph_node_layout(&mut graph);
    for node in graph.nodes.iter_mut() {
        clamp_node_data(&mut node.data);
    }
    graph
}

fn effect_graph_node_is_copyable(data: &EffectGraphNodeData) -> bool {
    !matches!(
        data,
        EffectGraphNodeData::Input | EffectGraphNodeData::Output
    )
}

fn effect_graph_selection_origin(nodes: &[EffectGraphNode]) -> Option<[f32; 2]> {
    let min_x = nodes
        .iter()
        .map(|node| node.ui_pos[0])
        .min_by(|left, right| left.total_cmp(right))?;
    let min_y = nodes
        .iter()
        .map(|node| node.ui_pos[1])
        .min_by(|left, right| left.total_cmp(right))?;
    Some([min_x, min_y])
}

fn effect_graph_build_clipboard_payload(
    document: &EffectGraphDocument,
    selected_nodes: &HashSet<String>,
) -> Option<EffectGraphClipboardPayload> {
    let mut nodes = document
        .nodes
        .iter()
        .filter(|node| {
            selected_nodes.contains(&node.id) && effect_graph_node_is_copyable(&node.data)
        })
        .cloned()
        .collect::<Vec<_>>();
    let origin = effect_graph_selection_origin(&nodes)?;
    for node in nodes.iter_mut() {
        node.ui_pos[0] -= origin[0];
        node.ui_pos[1] -= origin[1];
    }
    let copied_ids = nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<HashSet<_>>();
    let edges = document
        .edges
        .iter()
        .filter(|edge| {
            copied_ids.contains(&edge.from_node_id) && copied_ids.contains(&edge.to_node_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    Some(EffectGraphClipboardPayload {
        version: EFFECT_GRAPH_CLIPBOARD_VERSION,
        origin,
        nodes,
        edges,
    })
}

fn effect_graph_clipboard_payload_to_text(
    payload: &EffectGraphClipboardPayload,
) -> Result<String, String> {
    let json = serde_json::to_string(payload).map_err(|err| err.to_string())?;
    Ok(format!("{EFFECT_GRAPH_CLIPBOARD_MARKER}\n{json}"))
}

fn effect_graph_clipboard_payload_from_text(text: &str) -> Option<EffectGraphClipboardPayload> {
    let json = text
        .strip_prefix(EFFECT_GRAPH_CLIPBOARD_MARKER)?
        .trim_start();
    let payload = serde_json::from_str::<EffectGraphClipboardPayload>(json).ok()?;
    (payload.version == EFFECT_GRAPH_CLIPBOARD_VERSION).then_some(payload)
}

fn effect_graph_unique_id(existing_ids: &mut HashSet<String>, base: &str) -> String {
    if existing_ids.insert(base.to_string()) {
        return base.to_string();
    }
    let mut next = 2usize;
    loop {
        let candidate = format!("{base}_{next}");
        if existing_ids.insert(candidate.clone()) {
            return candidate;
        }
        next = next.saturating_add(1);
    }
}

fn node_label(kind: EffectGraphNodeKind) -> &'static str {
    match kind {
        EffectGraphNodeKind::Input => "Input",
        EffectGraphNodeKind::Output => "Output",
        EffectGraphNodeKind::Gain => "Gain",
        EffectGraphNodeKind::MonoMix => "Mono Mix",
        EffectGraphNodeKind::PitchShift => "PitchShift",
        EffectGraphNodeKind::TimeStretch => "TimeStretch",
        EffectGraphNodeKind::Speed => "Speed",
        EffectGraphNodeKind::Duplicate => "Duplicate",
        EffectGraphNodeKind::SplitChannels => "Split Channels",
        EffectGraphNodeKind::CombineChannels => "Combine Channels",
        EffectGraphNodeKind::DebugWaveform => "Waveform",
        EffectGraphNodeKind::DebugSpectrum => "Spectrum",
    }
}

fn node_parameter_summary(data: &EffectGraphNodeData) -> String {
    match data {
        EffectGraphNodeData::Input => "Source audio".to_string(),
        EffectGraphNodeData::Output => "Rendered audio".to_string(),
        EffectGraphNodeData::Gain { gain_db } => format!("{gain_db:+.1} dB"),
        EffectGraphNodeData::MonoMix { ignored_channels } => {
            let ignored_count = ignored_channels
                .iter()
                .copied()
                .filter(|value| *value)
                .count();
            if ignored_count == 0 {
                "Mono downmix".to_string()
            } else {
                format!("Mono / {ignored_count} ignored")
            }
        }
        EffectGraphNodeData::PitchShift { semitones } => format!("{semitones:+.1} st"),
        EffectGraphNodeData::TimeStretch { rate } => format!("{rate:.2}x"),
        EffectGraphNodeData::Speed { rate } => format!("{rate:.2}x"),
        EffectGraphNodeData::Duplicate => "1 in / 2 auto branches".to_string(),
        EffectGraphNodeData::SplitChannels => "1 in / 8 routed mono outs".to_string(),
        EffectGraphNodeData::CombineChannels => "Auto format combine".to_string(),
        EffectGraphNodeData::DebugWaveform { zoom } => format!("Test-only waveform / {zoom:.1}x"),
        EffectGraphNodeData::DebugSpectrum { mode, zoom } => match mode {
            EffectGraphSpectrumMode::Linear => format!("Debug spectrum / linear / {zoom:.1}x"),
            EffectGraphSpectrumMode::Log => format!("Debug spectrum / log / {zoom:.1}x"),
            EffectGraphSpectrumMode::Mel => format!("Debug spectrum / mel / {zoom:.1}x"),
        },
    }
}

fn channel_layout_declared_width(layout: &EffectGraphChannelLayout) -> Option<usize> {
    (layout.declared_width > 0).then_some(layout.declared_width)
}

fn channel_layout_max_live_slot(layout: &EffectGraphChannelLayout) -> Option<usize> {
    layout
        .entries
        .iter()
        .filter_map(|entry| match entry {
            EffectGraphChannelLayoutEntry::Dense => None,
            EffectGraphChannelLayoutEntry::Slotted { slot_index } => Some(*slot_index),
            EffectGraphChannelLayoutEntry::AutoPlaced {
                origin_slot: Some(slot_index),
                ..
            } => Some(*slot_index),
            EffectGraphChannelLayoutEntry::AutoPlaced {
                origin_slot: None, ..
            } => None,
            EffectGraphChannelLayoutEntry::Vacant { .. } => None,
        })
        .max()
}

fn channel_layout_kind(layout: &EffectGraphChannelLayout) -> EffectGraphChannelFlowHint {
    if layout.entries.is_empty() {
        return EffectGraphChannelFlowHint::Unknown;
    }
    if layout
        .entries
        .iter()
        .all(|entry| matches!(entry, EffectGraphChannelLayoutEntry::Dense))
    {
        return EffectGraphChannelFlowHint::PlainDense;
    }
    if layout.entries.iter().all(|entry| {
        matches!(
            entry,
            EffectGraphChannelLayoutEntry::Slotted { .. }
                | EffectGraphChannelLayoutEntry::Vacant { .. }
        )
    }) {
        let slot_indices = layout
            .entries
            .iter()
            .filter_map(|entry| match entry {
                EffectGraphChannelLayoutEntry::Dense => None,
                EffectGraphChannelLayoutEntry::Slotted { slot_index } => Some(*slot_index),
                EffectGraphChannelLayoutEntry::Vacant { requested_slot } => Some(*requested_slot),
                EffectGraphChannelLayoutEntry::AutoPlaced { .. } => None,
            })
            .collect::<Vec<_>>();
        return EffectGraphChannelFlowHint::Slotted {
            declared_width_hint: channel_layout_declared_width(layout),
            slot_indices,
        };
    }
    if layout.entries.iter().all(|entry| {
        matches!(
            entry,
            EffectGraphChannelLayoutEntry::AutoPlaced { .. }
                | EffectGraphChannelLayoutEntry::Vacant { .. }
        )
    }) {
        let origin_slots = layout
            .entries
            .iter()
            .filter_map(|entry| match entry {
                EffectGraphChannelLayoutEntry::AutoPlaced { origin_slot, .. } => Some(*origin_slot),
                EffectGraphChannelLayoutEntry::Vacant { .. } => None,
                _ => None,
            })
            .collect::<Vec<_>>();
        let branch_group_count = layout
            .entries
            .iter()
            .filter_map(|entry| match entry {
                EffectGraphChannelLayoutEntry::AutoPlaced {
                    branch_group_id, ..
                } => Some(branch_group_id.as_str()),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>()
            .len()
            .max(1);
        return EffectGraphChannelFlowHint::AutoPlaced {
            declared_width_hint: channel_layout_declared_width(layout),
            origin_slots,
            branch_group_count,
            predicted_channels_hint: layout
                .entries
                .iter()
                .filter(|entry| !matches!(entry, EffectGraphChannelLayoutEntry::Vacant { .. }))
                .count(),
        };
    }
    EffectGraphChannelFlowHint::Unknown
}

fn flow_hint_slot_indices(hint: &EffectGraphChannelFlowHint) -> Vec<usize> {
    match hint {
        EffectGraphChannelFlowHint::Slotted { slot_indices, .. } => slot_indices.clone(),
        EffectGraphChannelFlowHint::AutoPlaced { origin_slots, .. } => {
            origin_slots.iter().filter_map(|slot| *slot).collect()
        }
        _ => Vec::new(),
    }
}

fn flow_hint_declared_width(hint: &EffectGraphChannelFlowHint) -> Option<usize> {
    match hint {
        EffectGraphChannelFlowHint::Slotted {
            declared_width_hint,
            ..
        }
        | EffectGraphChannelFlowHint::AutoPlaced {
            declared_width_hint,
            ..
        } => *declared_width_hint,
        _ => None,
    }
}

fn flow_hint_lane_centroid(hint: &EffectGraphChannelFlowHint) -> Option<f32> {
    let slot_indices = flow_hint_slot_indices(hint);
    if slot_indices.is_empty() {
        None
    } else {
        Some(
            slot_indices
                .iter()
                .copied()
                .map(|value| value as f32)
                .sum::<f32>()
                / slot_indices.len() as f32,
        )
    }
}

fn combine_mode_from_hints<'a>(
    hints: impl IntoIterator<Item = &'a EffectGraphChannelFlowHint>,
) -> Option<EffectGraphCombineMode> {
    let mut saw_plain = false;
    let mut saw_slotted = false;
    let mut saw_auto = false;
    let mut saw_unknown = false;
    let mut saw_any = false;
    for hint in hints {
        match hint {
            EffectGraphChannelFlowHint::PlainDense => {
                saw_plain = true;
                saw_any = true;
            }
            EffectGraphChannelFlowHint::Slotted { .. } => {
                saw_slotted = true;
                saw_any = true;
            }
            EffectGraphChannelFlowHint::AutoPlaced { .. } => {
                saw_auto = true;
                saw_any = true;
            }
            EffectGraphChannelFlowHint::Unknown => saw_unknown = true,
        }
    }
    if !saw_any {
        None
    } else if saw_unknown {
        Some(EffectGraphCombineMode::Mixed)
    } else if saw_auto || (saw_plain && saw_slotted) {
        Some(EffectGraphCombineMode::Adaptive)
    } else if saw_slotted {
        Some(EffectGraphCombineMode::Restore)
    } else {
        Some(EffectGraphCombineMode::Concat)
    }
}

fn combine_mode_from_buses(buses: &[EffectGraphAudioBus]) -> Option<EffectGraphCombineMode> {
    let hints = buses
        .iter()
        .map(|bus| channel_layout_kind(&bus.channel_layout))
        .collect::<Vec<_>>();
    combine_mode_from_hints(hints.iter())
}

fn effect_graph_infer_flow_hints(
    document: &EffectGraphDocument,
) -> HashMap<EffectGraphPortKey, EffectGraphChannelFlowHint> {
    let order = effect_graph_topological_order(document);
    let node_map = document
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    let (input_sources, _) = build_port_edge_maps(document);
    let mut output_hints = HashMap::<EffectGraphPortKey, EffectGraphChannelFlowHint>::new();

    for node_id in order {
        let Some(node) = node_map.get(&node_id) else {
            continue;
        };
        let input_hints = node
            .data
            .input_ports()
            .iter()
            .filter_map(|port_id| {
                input_sources
                    .get(&make_port_key(&node.id, port_id))
                    .and_then(|source| output_hints.get(source))
                    .cloned()
            })
            .collect::<Vec<_>>();
        match &node.data {
            EffectGraphNodeData::Input => {
                output_hints.insert(
                    make_port_key(&node.id, "out"),
                    EffectGraphChannelFlowHint::PlainDense,
                );
            }
            EffectGraphNodeData::MonoMix { .. } => {
                output_hints.insert(
                    make_port_key(&node.id, "out"),
                    EffectGraphChannelFlowHint::PlainDense,
                );
            }
            EffectGraphNodeData::Duplicate => {
                let input_hint = input_hints
                    .into_iter()
                    .next()
                    .unwrap_or(EffectGraphChannelFlowHint::Unknown);
                for port_id in node.data.output_ports().iter() {
                    let output_hint = match &input_hint {
                        EffectGraphChannelFlowHint::PlainDense => {
                            EffectGraphChannelFlowHint::AutoPlaced {
                                declared_width_hint: None,
                                origin_slots: Vec::new(),
                                branch_group_count: 1,
                                predicted_channels_hint: 1,
                            }
                        }
                        EffectGraphChannelFlowHint::Slotted {
                            declared_width_hint,
                            slot_indices,
                        } => EffectGraphChannelFlowHint::AutoPlaced {
                            declared_width_hint: *declared_width_hint,
                            origin_slots: slot_indices
                                .iter()
                                .copied()
                                .map(Some)
                                .collect::<Vec<_>>(),
                            branch_group_count: 1,
                            predicted_channels_hint: slot_indices.len().max(1),
                        },
                        EffectGraphChannelFlowHint::AutoPlaced {
                            declared_width_hint,
                            origin_slots,
                            predicted_channels_hint,
                            ..
                        } => EffectGraphChannelFlowHint::AutoPlaced {
                            declared_width_hint: *declared_width_hint,
                            origin_slots: origin_slots.clone(),
                            branch_group_count: 1,
                            predicted_channels_hint: (*predicted_channels_hint).max(1),
                        },
                        EffectGraphChannelFlowHint::Unknown => EffectGraphChannelFlowHint::Unknown,
                    };
                    let output_hint = match output_hint {
                        EffectGraphChannelFlowHint::AutoPlaced {
                            declared_width_hint,
                            origin_slots,
                            predicted_channels_hint,
                            ..
                        } => EffectGraphChannelFlowHint::AutoPlaced {
                            declared_width_hint,
                            origin_slots,
                            branch_group_count: 1,
                            predicted_channels_hint,
                        },
                        other => other,
                    };
                    output_hints.insert(make_port_key(&node.id, port_id), output_hint);
                }
            }
            EffectGraphNodeData::SplitChannels => {
                let declared_width_hint = input_hints.iter().find_map(flow_hint_declared_width);
                for (index, port_id) in node.data.output_ports().iter().enumerate() {
                    output_hints.insert(
                        make_port_key(&node.id, port_id),
                        EffectGraphChannelFlowHint::Slotted {
                            declared_width_hint,
                            slot_indices: vec![index],
                        },
                    );
                }
            }
            EffectGraphNodeData::CombineChannels => {
                let mode = combine_mode_from_hints(input_hints.iter());
                let output_hint = match mode {
                    Some(EffectGraphCombineMode::Concat) => EffectGraphChannelFlowHint::PlainDense,
                    Some(EffectGraphCombineMode::Restore) => {
                        let mut slot_indices = input_hints
                            .iter()
                            .flat_map(flow_hint_slot_indices)
                            .collect::<Vec<_>>();
                        slot_indices.sort_unstable();
                        slot_indices.dedup();
                        EffectGraphChannelFlowHint::Slotted {
                            declared_width_hint: input_hints
                                .iter()
                                .filter_map(flow_hint_declared_width)
                                .max(),
                            slot_indices,
                        }
                    }
                    Some(EffectGraphCombineMode::Adaptive) => {
                        EffectGraphChannelFlowHint::PlainDense
                    }
                    _ => EffectGraphChannelFlowHint::Unknown,
                };
                output_hints.insert(make_port_key(&node.id, "out"), output_hint);
            }
            EffectGraphNodeData::Output => {}
            _ => {
                output_hints.insert(
                    make_port_key(&node.id, "out"),
                    input_hints
                        .into_iter()
                        .next()
                        .unwrap_or(EffectGraphChannelFlowHint::Unknown),
                );
            }
        }
    }

    output_hints
}

fn effect_graph_combine_mode_for_node_from_maps(
    node: &EffectGraphNode,
    input_sources: &HashMap<EffectGraphPortKey, EffectGraphPortKey>,
    flow_hints: &HashMap<EffectGraphPortKey, EffectGraphChannelFlowHint>,
) -> Option<EffectGraphCombineMode> {
    if !matches!(node.data, EffectGraphNodeData::CombineChannels) {
        return None;
    }
    let hints = node
        .data
        .input_ports()
        .iter()
        .filter_map(|port_id| {
            input_sources
                .get(&make_port_key(&node.id, port_id))
                .and_then(|source| flow_hints.get(source))
        })
        .collect::<Vec<_>>();
    combine_mode_from_hints(hints.iter().copied())
}

fn effect_graph_node_lane_hint(
    node: &EffectGraphNode,
    input_sources: &HashMap<EffectGraphPortKey, EffectGraphPortKey>,
    flow_hints: &HashMap<EffectGraphPortKey, EffectGraphChannelFlowHint>,
) -> Option<f32> {
    let mut lane_values = node
        .data
        .input_ports()
        .iter()
        .filter_map(|port_id| {
            input_sources
                .get(&make_port_key(&node.id, port_id))
                .and_then(|source| flow_hints.get(source))
                .and_then(flow_hint_lane_centroid)
        })
        .collect::<Vec<_>>();
    if lane_values.is_empty() {
        lane_values.extend(node.data.output_ports().iter().filter_map(|port_id| {
            flow_hints
                .get(&make_port_key(&node.id, port_id))
                .and_then(flow_hint_lane_centroid)
        }));
    }
    if lane_values.is_empty() {
        None
    } else {
        Some(lane_values.iter().sum::<f32>() / lane_values.len() as f32)
    }
}

fn effect_graph_combine_slot_labels_for_node(
    node: &EffectGraphNode,
    input_sources: &HashMap<EffectGraphPortKey, EffectGraphPortKey>,
    flow_hints: &HashMap<EffectGraphPortKey, EffectGraphChannelFlowHint>,
) -> HashMap<String, usize> {
    let mut labels = HashMap::new();
    for port_id in node.data.input_ports().iter() {
        let Some(source) = input_sources.get(&make_port_key(&node.id, port_id)) else {
            continue;
        };
        let Some(hint) = flow_hints.get(source) else {
            continue;
        };
        let slot_indices = flow_hint_slot_indices(hint);
        if let Some(slot_index) = slot_indices.into_iter().min() {
            labels.insert((*port_id).to_string(), slot_index);
        }
    }
    labels
}

fn effect_graph_combine_display_labels_for_node(
    node: &EffectGraphNode,
    input_sources: &HashMap<EffectGraphPortKey, EffectGraphPortKey>,
    flow_hints: &HashMap<EffectGraphPortKey, EffectGraphChannelFlowHint>,
) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    for port_id in node.data.input_ports().iter() {
        let Some(source) = input_sources.get(&make_port_key(&node.id, port_id)) else {
            continue;
        };
        let Some(hint) = flow_hints.get(source) else {
            continue;
        };
        let label = match hint {
            EffectGraphChannelFlowHint::Slotted { slot_indices, .. } => slot_indices
                .first()
                .map(|slot| format!("slot {}", slot.saturating_add(1))),
            EffectGraphChannelFlowHint::AutoPlaced { origin_slots, .. } => origin_slots
                .iter()
                .flatten()
                .min()
                .map(|slot| format!("slot {} -> auto", slot.saturating_add(1)))
                .or_else(|| Some("auto block".to_string())),
            EffectGraphChannelFlowHint::PlainDense => Some("auto block".to_string()),
            EffectGraphChannelFlowHint::Unknown => None,
        };
        if let Some(label) = label {
            labels.insert((*port_id).to_string(), label);
        }
    }
    labels
}

fn remap_sample(sample: usize, old_len: usize, new_len: usize) -> Option<usize> {
    if old_len == 0 || new_len == 0 {
        return None;
    }
    if old_len == new_len {
        return Some(sample.min(new_len.saturating_sub(1)));
    }
    let ratio = new_len as f64 / old_len as f64;
    let mapped = ((sample as f64) * ratio).round().max(0.0) as usize;
    Some(mapped.min(new_len.saturating_sub(1)))
}

fn remap_range(
    range: Option<(usize, usize)>,
    old_len: usize,
    new_len: usize,
) -> Option<(usize, usize)> {
    let (start, end) = range?;
    let mapped_start = remap_sample(start, old_len, new_len)?;
    let mapped_end = remap_sample(end, old_len, new_len)?.max(mapped_start.saturating_add(1));
    if mapped_end > mapped_start && mapped_end <= new_len {
        Some((mapped_start, mapped_end))
    } else {
        None
    }
}

fn remap_markers(markers: &[MarkerEntry], old_len: usize, new_len: usize) -> Vec<MarkerEntry> {
    markers
        .iter()
        .filter_map(|marker| {
            let sample = remap_sample(marker.sample, old_len, new_len)?;
            Some(MarkerEntry {
                sample,
                label: marker.label.clone(),
            })
        })
        .collect()
}

fn sync_channel_lengths(channels: &mut Vec<Vec<f32>>) {
    let max_len = channels.iter().map(|c| c.len()).max().unwrap_or(0);
    for channel in channels.iter_mut() {
        if channel.len() < max_len {
            let fill = channel.last().copied().unwrap_or(0.0);
            channel.resize(max_len, fill);
        }
    }
}

fn make_port_key(node_id: &str, port_id: &str) -> EffectGraphPortKey {
    EffectGraphPortKey {
        node_id: node_id.to_string(),
        port_id: port_id.to_string(),
    }
}

fn build_graph_maps(
    document: &EffectGraphDocument,
) -> (
    HashMap<String, EffectGraphNode>,
    HashMap<String, Vec<String>>,
    HashMap<String, Vec<String>>,
) {
    let node_map = document
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    let mut incoming_sets: HashMap<String, HashSet<String>> = HashMap::new();
    let mut outgoing_sets: HashMap<String, HashSet<String>> = HashMap::new();
    for edge in document.edges.iter() {
        incoming_sets
            .entry(edge.to_node_id.clone())
            .or_default()
            .insert(edge.from_node_id.clone());
        outgoing_sets
            .entry(edge.from_node_id.clone())
            .or_default()
            .insert(edge.to_node_id.clone());
    }
    let incoming = incoming_sets
        .into_iter()
        .map(|(node_id, values)| {
            let mut values = values.into_iter().collect::<Vec<_>>();
            values.sort();
            (node_id, values)
        })
        .collect::<HashMap<_, _>>();
    let outgoing = outgoing_sets
        .into_iter()
        .map(|(node_id, values)| {
            let mut values = values.into_iter().collect::<Vec<_>>();
            values.sort();
            (node_id, values)
        })
        .collect::<HashMap<_, _>>();
    (node_map, incoming, outgoing)
}

fn build_port_edge_maps(
    document: &EffectGraphDocument,
) -> (
    HashMap<EffectGraphPortKey, EffectGraphPortKey>,
    HashMap<EffectGraphPortKey, EffectGraphPortKey>,
) {
    let mut input_sources = HashMap::new();
    let mut output_targets = HashMap::new();
    for edge in document.edges.iter() {
        let from_key = make_port_key(&edge.from_node_id, &edge.from_port_id);
        let to_key = make_port_key(&edge.to_node_id, &edge.to_port_id);
        input_sources.insert(to_key.clone(), from_key.clone());
        output_targets.insert(from_key, to_key);
    }
    (input_sources, output_targets)
}

fn effect_graph_input_id(document: &EffectGraphDocument) -> Option<String> {
    document
        .nodes
        .iter()
        .find(|node| matches!(node.data, EffectGraphNodeData::Input))
        .map(|node| node.id.clone())
}

fn effect_graph_output_id(document: &EffectGraphDocument) -> Option<String> {
    document
        .nodes
        .iter()
        .find(|node| matches!(node.data, EffectGraphNodeData::Output))
        .map(|node| node.id.clone())
}

fn effect_graph_reachable_from(
    start: &str,
    graph: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let mut visited = HashSet::new();
    let mut stack = vec![start.to_string()];
    while let Some(node_id) = stack.pop() {
        if !visited.insert(node_id.clone()) {
            continue;
        }
        if let Some(nexts) = graph.get(&node_id) {
            stack.extend(nexts.iter().cloned());
        }
    }
    visited
}

fn effect_graph_active_nodes(document: &EffectGraphDocument) -> HashSet<String> {
    let Some(input_id) = effect_graph_input_id(document) else {
        return HashSet::new();
    };
    let Some(output_id) = effect_graph_output_id(document) else {
        return HashSet::new();
    };
    let (_, incoming, outgoing) = build_graph_maps(document);
    let from_input = effect_graph_reachable_from(&input_id, &outgoing);
    let to_output = effect_graph_reachable_from(&output_id, &incoming);
    from_input
        .intersection(&to_output)
        .cloned()
        .collect::<HashSet<_>>()
}

fn effect_graph_layout_priority(data: &EffectGraphNodeData) -> i32 {
    match data {
        EffectGraphNodeData::Input => 0,
        EffectGraphNodeData::Gain { .. } => 10,
        EffectGraphNodeData::MonoMix { .. } => 15,
        EffectGraphNodeData::PitchShift { .. } => 20,
        EffectGraphNodeData::TimeStretch { .. } => 30,
        EffectGraphNodeData::Speed { .. } => 40,
        EffectGraphNodeData::Duplicate => 45,
        EffectGraphNodeData::SplitChannels => 50,
        EffectGraphNodeData::CombineChannels => 60,
        EffectGraphNodeData::DebugWaveform { .. } => 70,
        EffectGraphNodeData::DebugSpectrum { .. } => 80,
        EffectGraphNodeData::Output => 100,
    }
}

fn effect_graph_topological_order(document: &EffectGraphDocument) -> Vec<String> {
    effect_graph_topological_order_strict(document).unwrap_or_else(|_| {
        let mut nodes = document.nodes.clone();
        nodes.sort_by(|left, right| {
            (
                effect_graph_layout_priority(&left.data),
                left.ui_pos[0] as i32,
                left.ui_pos[1] as i32,
                left.id.as_str(),
            )
                .cmp(&(
                    effect_graph_layout_priority(&right.data),
                    right.ui_pos[0] as i32,
                    right.ui_pos[1] as i32,
                    right.id.as_str(),
                ))
        });
        nodes.into_iter().map(|node| node.id).collect()
    })
}

fn effect_graph_topological_order_strict(
    document: &EffectGraphDocument,
) -> Result<Vec<String>, String> {
    let (node_map, incoming, outgoing) = build_graph_maps(document);
    let mut indegree = document
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.clone(),
                incoming
                    .get(&node.id)
                    .map(|parents| parents.len())
                    .unwrap_or(0),
            )
        })
        .collect::<HashMap<_, _>>();
    let mut ready = document
        .nodes
        .iter()
        .filter(|node| indegree.get(&node.id).copied().unwrap_or(0) == 0)
        .map(|node| node.id.clone())
        .collect::<Vec<_>>();
    ready.sort_by(|left, right| {
        let left_node = node_map.get(left);
        let right_node = node_map.get(right);
        let left_key = left_node
            .map(|node| {
                (
                    effect_graph_layout_priority(&node.data),
                    node.ui_pos[0] as i32,
                    node.ui_pos[1] as i32,
                )
            })
            .unwrap_or((999, 0, 0));
        let right_key = right_node
            .map(|node| {
                (
                    effect_graph_layout_priority(&node.data),
                    node.ui_pos[0] as i32,
                    node.ui_pos[1] as i32,
                )
            })
            .unwrap_or((999, 0, 0));
        left_key.cmp(&right_key).then_with(|| left.cmp(right))
    });

    let mut order = Vec::with_capacity(document.nodes.len());
    while let Some(node_id) = ready.first().cloned() {
        ready.remove(0);
        order.push(node_id.clone());
        for next in outgoing.get(&node_id).into_iter().flatten() {
            if let Some(value) = indegree.get_mut(next) {
                *value = value.saturating_sub(1);
                if *value == 0 {
                    ready.push(next.clone());
                }
            }
        }
        ready.sort_by(|left, right| {
            let left_node = node_map.get(left);
            let right_node = node_map.get(right);
            let left_key = left_node
                .map(|node| {
                    (
                        effect_graph_layout_priority(&node.data),
                        node.ui_pos[0] as i32,
                        node.ui_pos[1] as i32,
                    )
                })
                .unwrap_or((999, 0, 0));
            let right_key = right_node
                .map(|node| {
                    (
                        effect_graph_layout_priority(&node.data),
                        node.ui_pos[0] as i32,
                        node.ui_pos[1] as i32,
                    )
                })
                .unwrap_or((999, 0, 0));
            left_key.cmp(&right_key).then_with(|| left.cmp(right))
        });
    }

    if order.len() != document.nodes.len() {
        return Err("cycle detected".to_string());
    }
    Ok(order)
}

fn validate_effect_graph_document(
    document: &EffectGraphDocument,
) -> Vec<EffectGraphValidationIssue> {
    let mut issues = Vec::new();
    let mut node_ids = HashSet::new();
    let node_map = document
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<HashMap<_, _>>();
    for node in document.nodes.iter() {
        if !node_ids.insert(node.id.clone()) {
            issues.push(EffectGraphValidationIssue {
                severity: EffectGraphSeverity::Error,
                code: "duplicate_node_id".to_string(),
                message: format!("Duplicate node id: {}", node.id),
                node_id: Some(node.id.clone()),
            });
        }
        match &node.data {
            EffectGraphNodeData::Gain { gain_db } if *gain_db < -24.0 || *gain_db > 24.0 => {
                issues.push(EffectGraphValidationIssue {
                    severity: EffectGraphSeverity::Warning,
                    code: "gain_out_of_range".to_string(),
                    message: "Gain is outside -24..24 dB and will be clamped on save".to_string(),
                    node_id: Some(node.id.clone()),
                });
            }
            EffectGraphNodeData::PitchShift { semitones }
                if *semitones < -12.0 || *semitones > 12.0 =>
            {
                issues.push(EffectGraphValidationIssue {
                    severity: EffectGraphSeverity::Warning,
                    code: "pitch_out_of_range".to_string(),
                    message: "PitchShift is outside -12..12 st and will be clamped on save"
                        .to_string(),
                    node_id: Some(node.id.clone()),
                });
            }
            EffectGraphNodeData::TimeStretch { rate } | EffectGraphNodeData::Speed { rate }
                if *rate < 0.25 || *rate > 4.0 =>
            {
                issues.push(EffectGraphValidationIssue {
                    severity: EffectGraphSeverity::Warning,
                    code: "rate_out_of_range".to_string(),
                    message: "Rate is outside 0.25..4.0 and will be clamped on save".to_string(),
                    node_id: Some(node.id.clone()),
                });
            }
            EffectGraphNodeData::DebugWaveform { zoom } if *zoom < 1.0 || *zoom > 32.0 => {
                issues.push(EffectGraphValidationIssue {
                    severity: EffectGraphSeverity::Warning,
                    code: "waveform_zoom_out_of_range".to_string(),
                    message: "Waveform zoom is outside 1..32 and will be clamped on save"
                        .to_string(),
                    node_id: Some(node.id.clone()),
                });
            }
            EffectGraphNodeData::DebugSpectrum { zoom, .. } if *zoom < 1.0 || *zoom > 16.0 => {
                issues.push(EffectGraphValidationIssue {
                    severity: EffectGraphSeverity::Warning,
                    code: "spectrum_zoom_out_of_range".to_string(),
                    message: "Spectrum zoom is outside 1..16 and will be clamped on save"
                        .to_string(),
                    node_id: Some(node.id.clone()),
                });
            }
            EffectGraphNodeData::Input
            | EffectGraphNodeData::Output
            | EffectGraphNodeData::MonoMix { .. }
            | EffectGraphNodeData::Duplicate
            | EffectGraphNodeData::SplitChannels
            | EffectGraphNodeData::CombineChannels => {}
            _ => {}
        }
    }

    let input_nodes = document
        .nodes
        .iter()
        .filter(|node| matches!(node.data, EffectGraphNodeData::Input))
        .count();
    let output_nodes = document
        .nodes
        .iter()
        .filter(|node| matches!(node.data, EffectGraphNodeData::Output))
        .count();
    if input_nodes != 1 {
        issues.push(EffectGraphValidationIssue {
            severity: EffectGraphSeverity::Error,
            code: "input_count".to_string(),
            message: "Effect Graph requires exactly one Input node".to_string(),
            node_id: None,
        });
    }
    if output_nodes != 1 {
        issues.push(EffectGraphValidationIssue {
            severity: EffectGraphSeverity::Error,
            code: "output_count".to_string(),
            message: "Effect Graph requires exactly one Output node".to_string(),
            node_id: None,
        });
    }

    let mut incoming_port_counts = HashMap::<EffectGraphPortKey, usize>::new();
    let mut outgoing_port_counts = HashMap::<EffectGraphPortKey, usize>::new();
    for edge in document.edges.iter() {
        let Some(from_node) = node_map.get(&edge.from_node_id) else {
            issues.push(EffectGraphValidationIssue {
                severity: EffectGraphSeverity::Error,
                code: "dangling_edge".to_string(),
                message: format!("Dangling source node in edge: {}", edge.id),
                node_id: None,
            });
            continue;
        };
        let Some(to_node) = node_map.get(&edge.to_node_id) else {
            issues.push(EffectGraphValidationIssue {
                severity: EffectGraphSeverity::Error,
                code: "dangling_edge".to_string(),
                message: format!("Dangling target node in edge: {}", edge.id),
                node_id: None,
            });
            continue;
        };
        if !from_node.data.has_output_port(&edge.from_port_id) {
            issues.push(EffectGraphValidationIssue {
                severity: EffectGraphSeverity::Error,
                code: "invalid_from_port".to_string(),
                message: format!(
                    "{} has no output port '{}'",
                    from_node.data.display_name(),
                    edge.from_port_id
                ),
                node_id: Some(from_node.id.clone()),
            });
        }
        if !to_node.data.has_input_port(&edge.to_port_id) {
            issues.push(EffectGraphValidationIssue {
                severity: EffectGraphSeverity::Error,
                code: "invalid_to_port".to_string(),
                message: format!(
                    "{} has no input port '{}'",
                    to_node.data.display_name(),
                    edge.to_port_id
                ),
                node_id: Some(to_node.id.clone()),
            });
        }
        *incoming_port_counts
            .entry(make_port_key(&edge.to_node_id, &edge.to_port_id))
            .or_default() += 1;
        *outgoing_port_counts
            .entry(make_port_key(&edge.from_node_id, &edge.from_port_id))
            .or_default() += 1;
    }

    for (port_key, count) in incoming_port_counts.iter() {
        if *count > 1 {
            issues.push(EffectGraphValidationIssue {
                severity: EffectGraphSeverity::Error,
                code: "multi_input_port".to_string(),
                message: format!(
                    "{}:{} has multiple incoming connections",
                    port_key.node_id, port_key.port_id
                ),
                node_id: Some(port_key.node_id.clone()),
            });
        }
    }
    for (port_key, count) in outgoing_port_counts.iter() {
        if *count > 1 {
            issues.push(EffectGraphValidationIssue {
                severity: EffectGraphSeverity::Error,
                code: "multi_output_port".to_string(),
                message: format!(
                    "{}:{} has multiple outgoing connections",
                    port_key.node_id, port_key.port_id
                ),
                node_id: Some(port_key.node_id.clone()),
            });
        }
    }

    if effect_graph_topological_order_strict(document).is_err() {
        issues.push(EffectGraphValidationIssue {
            severity: EffectGraphSeverity::Error,
            code: "cycle".to_string(),
            message: "Effect Graph cannot contain cycles".to_string(),
            node_id: None,
        });
    }

    let active_nodes = effect_graph_active_nodes(document);
    let (input_sources, _) = build_port_edge_maps(document);
    let flow_hints = effect_graph_infer_flow_hints(document);
    if input_nodes == 1 && output_nodes == 1 && active_nodes.is_empty() {
        issues.push(EffectGraphValidationIssue {
            severity: EffectGraphSeverity::Error,
            code: "output_unreachable".to_string(),
            message: "Output is not reachable from Input".to_string(),
            node_id: None,
        });
    }

    for node in document.nodes.iter() {
        let active = active_nodes.contains(&node.id);
        let input_count_for = |port_id: &str| {
            incoming_port_counts
                .get(&make_port_key(&node.id, port_id))
                .copied()
                .unwrap_or(0)
        };
        let output_count_for = |port_id: &str| {
            outgoing_port_counts
                .get(&make_port_key(&node.id, port_id))
                .copied()
                .unwrap_or(0)
        };
        let connected_input_ports = node
            .data
            .input_ports()
            .iter()
            .filter(|port_id| input_count_for(port_id) > 0)
            .count();
        let connected_output_ports = node
            .data
            .output_ports()
            .iter()
            .filter(|port_id| output_count_for(port_id) > 0)
            .count();
        match &node.data {
            EffectGraphNodeData::Input => {
                if connected_input_ports > 0 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "input_has_input".to_string(),
                        message: "Input node cannot have incoming connections".to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
                if active && output_count_for("out") != 1 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "input_outgoing".to_string(),
                        message: "Input.out must connect to exactly one port".to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
            }
            EffectGraphNodeData::Output => {
                if connected_output_ports > 0 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "output_has_output".to_string(),
                        message: "Output node cannot have outgoing connections".to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
                if active && input_count_for("in") != 1 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "output_incoming".to_string(),
                        message: "Output.in must receive exactly one connection".to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
            }
            EffectGraphNodeData::Duplicate => {
                if active && input_count_for("in") != 1 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "duplicate_incoming".to_string(),
                        message: "Duplicate requires exactly one input".to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
                if active && connected_output_ports == 0 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "duplicate_outgoing".to_string(),
                        message: "Duplicate requires at least one connected output".to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
            }
            EffectGraphNodeData::SplitChannels => {
                if active && input_count_for("in") != 1 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "split_incoming".to_string(),
                        message: "Split Channels requires exactly one input".to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
                if active && connected_output_ports == 0 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "split_outgoing".to_string(),
                        message: "Split Channels requires at least one connected output"
                            .to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
            }
            EffectGraphNodeData::CombineChannels => {
                if active && connected_input_ports == 0 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "combine_incoming".to_string(),
                        message: "Combine Channels requires at least one connected input"
                            .to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
                if active && output_count_for("out") != 1 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "combine_outgoing".to_string(),
                        message: "Combine Channels requires exactly one output connection"
                            .to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
                let connected_hints = node
                    .data
                    .input_ports()
                    .iter()
                    .filter_map(|port_id| {
                        input_sources
                            .get(&make_port_key(&node.id, port_id))
                            .and_then(|source| flow_hints.get(source))
                    })
                    .collect::<Vec<_>>();
                if matches!(
                    combine_mode_from_hints(connected_hints.iter().copied()),
                    Some(EffectGraphCombineMode::Mixed)
                ) {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "combine_mixed_layout".to_string(),
                        message: "Combine Channels received an unsupported channel layout mix"
                            .to_string(),
                        node_id: Some(node.id.clone()),
                    });
                }
                if matches!(
                    combine_mode_from_hints(connected_hints.iter().copied()),
                    Some(EffectGraphCombineMode::Restore)
                ) {
                    let mut slot_counts = HashMap::<usize, usize>::new();
                    for slot_index in connected_hints
                        .iter()
                        .flat_map(|hint| flow_hint_slot_indices(hint))
                    {
                        *slot_counts.entry(slot_index).or_default() += 1;
                    }
                    for (slot_index, count) in
                        slot_counts.into_iter().filter(|(_, count)| *count > 1)
                    {
                        issues.push(EffectGraphValidationIssue {
                            severity: EffectGraphSeverity::Warning,
                            code: "combine_duplicate_slot".to_string(),
                            message: format!(
                                "Combine Channels will mix {} inputs into slot {}",
                                count,
                                slot_index.saturating_add(1)
                            ),
                            node_id: Some(node.id.clone()),
                        });
                    }
                    let declared_widths = connected_hints
                        .iter()
                        .filter_map(|hint| flow_hint_declared_width(hint))
                        .collect::<HashSet<_>>();
                    if declared_widths.len() > 1 {
                        issues.push(EffectGraphValidationIssue {
                            severity: EffectGraphSeverity::Warning,
                            code: "restore_declared_width_mismatch".to_string(),
                            message: "Combine Channels restore inputs disagree on declared width"
                                .to_string(),
                            node_id: Some(node.id.clone()),
                        });
                    }
                }
            }
            _ => {
                if active && input_count_for("in") != 1 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "node_incoming".to_string(),
                        message: format!(
                            "{} requires exactly one input connection",
                            node.data.display_name()
                        ),
                        node_id: Some(node.id.clone()),
                    });
                }
                if active && output_count_for("out") != 1 {
                    issues.push(EffectGraphValidationIssue {
                        severity: EffectGraphSeverity::Error,
                        code: "node_outgoing".to_string(),
                        message: format!(
                            "{} requires exactly one output connection",
                            node.data.display_name()
                        ),
                        node_id: Some(node.id.clone()),
                    });
                }
            }
        }
        if !active {
            issues.push(EffectGraphValidationIssue {
                severity: EffectGraphSeverity::Warning,
                code: "unused_node".to_string(),
                message: format!(
                    "{} is not part of the active graph",
                    node.data.display_name()
                ),
                node_id: Some(node.id.clone()),
            });
        }
    }

    issues
}

fn process_speed_offline_channels(channels: &[Vec<f32>], rate: f32) -> Vec<Vec<f32>> {
    channels
        .iter()
        .map(|channel| crate::wave::process_speed_offline(channel, rate))
        .collect()
}

fn effect_graph_input_bus_for_port(
    node_id: &str,
    port_id: &str,
    input_sources: &HashMap<EffectGraphPortKey, EffectGraphPortKey>,
    output_buses: &HashMap<EffectGraphPortKey, EffectGraphAudioBus>,
) -> Option<EffectGraphAudioBus> {
    let source = input_sources.get(&make_port_key(node_id, port_id))?;
    output_buses.get(source).cloned()
}

fn restore_channels_by_layout(
    mut buses: Vec<EffectGraphAudioBus>,
    target_sample_rate: u32,
) -> Result<(EffectGraphAudioBus, Vec<String>), String> {
    let mut longest_len = 0usize;
    let mut declared_width = 0usize;
    for bus in buses.iter_mut() {
        normalize_audio_bus_lengths(bus);
        longest_len = longest_len.max(channels_frame_len(&bus.channels));
        declared_width = declared_width.max(bus.channel_layout.declared_width);
        declared_width = declared_width.max(
            channel_layout_max_live_slot(&bus.channel_layout)
                .unwrap_or(0)
                .saturating_add(1),
        );
    }
    let mut restored_channels = vec![vec![0.0; longest_len]; declared_width];
    let mut filled_slots = vec![false; declared_width];
    let mut slot_mix_counts = vec![0usize; declared_width];
    let mut warnings = Vec::new();

    for bus in buses.iter_mut() {
        pad_channels_with_silence(&mut bus.channels, longest_len);
        for (channel_index, channel) in bus.channels.iter().enumerate() {
            let entry = bus
                .channel_layout
                .entries
                .get(channel_index)
                .cloned()
                .unwrap_or(EffectGraphChannelLayoutEntry::Dense);
            match entry {
                EffectGraphChannelLayoutEntry::Dense => {
                    return Err("Combine restore mode received a dense channel layout".to_string());
                }
                EffectGraphChannelLayoutEntry::Slotted { slot_index } => {
                    if slot_index >= restored_channels.len() {
                        return Err(format!(
                            "Combine restore mode received out-of-range slot {}",
                            slot_index.saturating_add(1)
                        ));
                    }
                    if filled_slots[slot_index] {
                        for (sample_index, sample) in channel.iter().enumerate() {
                            restored_channels[slot_index][sample_index] += *sample;
                        }
                        slot_mix_counts[slot_index] = slot_mix_counts[slot_index].saturating_add(1);
                        continue;
                    }
                    restored_channels[slot_index] = channel.clone();
                    filled_slots[slot_index] = true;
                    slot_mix_counts[slot_index] = 1;
                }
                EffectGraphChannelLayoutEntry::Vacant { requested_slot } => {
                    warnings.push(format!(
                        "slot {} is vacant and will be restored as silence",
                        requested_slot.saturating_add(1)
                    ));
                }
                EffectGraphChannelLayoutEntry::AutoPlaced { .. } => {
                    return Err(
                        "Combine restore mode received an adaptive branch layout".to_string()
                    );
                }
            }
        }
    }
    for (slot_index, count) in slot_mix_counts.iter().copied().enumerate() {
        if count > 1 {
            warnings.push(format!(
                "slot {} mixed from {} inputs",
                slot_index.saturating_add(1),
                count
            ));
        }
    }

    Ok((
        dense_audio_bus(restored_channels, target_sample_rate),
        warnings,
    ))
}

fn adaptive_combine_channels_by_layout(
    mut buses: Vec<EffectGraphAudioBus>,
    target_sample_rate: u32,
) -> Result<(EffectGraphAudioBus, Vec<String>), String> {
    let mut longest_len = 0usize;
    let mut declared_width = 0usize;
    for bus in buses.iter_mut() {
        normalize_audio_bus_lengths(bus);
        longest_len = longest_len.max(channels_frame_len(&bus.channels));
        declared_width = declared_width.max(bus.channel_layout.declared_width);
    }

    let mut anchored_auto_blocks =
        std::collections::BTreeMap::<usize, Vec<AdaptivePlacementBlock>>::new();
    let mut slotted_channels = std::collections::BTreeMap::<usize, Vec<Vec<f32>>>::new();
    let mut vacant_slots = std::collections::BTreeSet::<usize>::new();
    let mut unanchored_blocks = Vec::<AdaptivePlacementBlock>::new();
    let mut warnings = Vec::<String>::new();

    for (socket_order, bus) in buses.iter_mut().enumerate() {
        pad_channels_with_silence(&mut bus.channels, longest_len);

        let mut grouped_auto = HashMap::<String, Vec<(usize, Option<usize>, Vec<f32>)>>::new();
        let mut dense_channels = Vec::<Vec<f32>>::new();

        for (channel_index, channel) in bus.channels.iter().enumerate() {
            let entry = bus
                .channel_layout
                .entries
                .get(channel_index)
                .cloned()
                .unwrap_or(EffectGraphChannelLayoutEntry::Dense);
            match entry {
                EffectGraphChannelLayoutEntry::Dense => dense_channels.push(channel.clone()),
                EffectGraphChannelLayoutEntry::Slotted { slot_index } => {
                    slotted_channels
                        .entry(slot_index)
                        .or_default()
                        .push(channel.clone());
                }
                EffectGraphChannelLayoutEntry::Vacant { requested_slot } => {
                    vacant_slots.insert(requested_slot);
                }
                EffectGraphChannelLayoutEntry::AutoPlaced {
                    origin_slot,
                    branch_group_id,
                    branch_channel_index,
                } => {
                    grouped_auto.entry(branch_group_id).or_default().push((
                        branch_channel_index,
                        origin_slot,
                        channel.clone(),
                    ));
                }
            }
        }

        for (_, mut group) in grouped_auto {
            group.sort_by_key(|(branch_channel_index, _, _)| *branch_channel_index);
            let anchor_slot = group
                .iter()
                .filter_map(|(_, origin_slot, _)| *origin_slot)
                .min();
            let channels = group
                .into_iter()
                .map(|(_, _, channel)| channel)
                .collect::<Vec<_>>();
            let block = AdaptivePlacementBlock {
                anchor_slot,
                channels,
                socket_order,
            };
            if let Some(slot_index) = block.anchor_slot {
                anchored_auto_blocks
                    .entry(slot_index)
                    .or_default()
                    .push(block);
            } else {
                unanchored_blocks.push(block);
            }
        }

        if !dense_channels.is_empty() {
            unanchored_blocks.push(AdaptivePlacementBlock {
                anchor_slot: None,
                channels: dense_channels,
                socket_order,
            });
        }
    }

    let max_slot = anchored_auto_blocks
        .keys()
        .copied()
        .chain(slotted_channels.keys().copied())
        .max()
        .map(|slot| slot.saturating_add(1))
        .unwrap_or(0);
    let baseline_width = declared_width.max(max_slot);
    let mut output_channels = Vec::<Vec<f32>>::new();

    for slot_index in 0..baseline_width {
        let mut had_real_content = false;
        if let Some(mut auto_blocks) = anchored_auto_blocks.remove(&slot_index) {
            auto_blocks.sort_by_key(|block| block.socket_order);
            if auto_blocks.len() > 1 {
                warnings.push(format!(
                    "slot {} widened into {} channels from duplicate branches",
                    slot_index.saturating_add(1),
                    auto_blocks
                        .iter()
                        .map(|block| block.channels.len())
                        .sum::<usize>()
                ));
            }
            for block in auto_blocks {
                if !block.channels.is_empty() {
                    had_real_content = true;
                    output_channels.extend(block.channels);
                }
            }
        }
        if let Some(channels) = slotted_channels.remove(&slot_index) {
            if !channels.is_empty() {
                had_real_content = true;
                if channels.len() > 1 {
                    warnings.push(format!(
                        "slot {} mixed from {} inputs",
                        slot_index.saturating_add(1),
                        channels.len()
                    ));
                }
                let mut mixed = vec![0.0f32; longest_len];
                for channel in channels.iter() {
                    for (sample_index, sample) in channel.iter().enumerate() {
                        mixed[sample_index] += *sample;
                    }
                }
                output_channels.push(mixed);
            }
        }
        if !had_real_content && slot_index < declared_width && vacant_slots.contains(&slot_index) {
            output_channels.push(vec![0.0; longest_len]);
        }
    }

    for (slot_index, mut blocks) in anchored_auto_blocks {
        blocks.sort_by_key(|block| block.socket_order);
        warnings.push(format!(
            "slot {} widened beyond the declared width",
            slot_index.saturating_add(1)
        ));
        for block in blocks {
            output_channels.extend(block.channels);
        }
    }

    unanchored_blocks.sort_by_key(|block| block.socket_order);
    for block in unanchored_blocks {
        warnings.push("adaptive combine appended an unanchored block".to_string());
        output_channels.extend(block.channels);
    }

    if output_channels.is_empty() {
        output_channels.push(vec![0.0; longest_len.max(1)]);
    }

    Ok((
        dense_audio_bus(output_channels, target_sample_rate),
        warnings,
    ))
}

fn run_effect_graph_document<F>(
    document: &EffectGraphDocument,
    input_bus: EffectGraphAudioBus,
    run_mode: EffectGraphRunMode,
    resample_quality: crate::wave::ResampleQuality,
    mut on_event: F,
) -> Result<EffectGraphAudioBus, String>
where
    F: FnMut(EffectGraphRuntimeEvent),
{
    let order = effect_graph_topological_order_strict(document)?;
    let active_nodes = effect_graph_active_nodes(document);
    let node_map = document
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    let (input_sources, _) = build_port_edge_maps(document);
    let mut output_buses = HashMap::<EffectGraphPortKey, EffectGraphAudioBus>::new();
    let mut final_output = None;

    for node_id in order {
        if !active_nodes.contains(&node_id) {
            continue;
        }
        let Some(node) = node_map.get(&node_id) else {
            return Err(format!("missing node: {node_id}"));
        };
        on_event(EffectGraphRuntimeEvent::NodeStarted(node_id.clone()));
        let started = Instant::now();
        match &node.data {
            EffectGraphNodeData::Input => {
                output_buses.insert(make_port_key(&node.id, "out"), input_bus.clone());
            }
            EffectGraphNodeData::Output => {
                let bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| "Output node did not receive audio".to_string())?;
                final_output = Some(bus);
            }
            EffectGraphNodeData::Duplicate => {
                let bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| format!("{} input is missing", node.id))?;
                let out1 = EffectGraphAudioBus {
                    channels: bus.channels.clone(),
                    sample_rate: bus.sample_rate,
                    channel_layout: make_duplicate_output_layout(
                        &bus,
                        &format!("{}:out1", node.id),
                    ),
                };
                let out2 = EffectGraphAudioBus {
                    channels: bus.channels.clone(),
                    sample_rate: bus.sample_rate,
                    channel_layout: make_duplicate_output_layout(
                        &bus,
                        &format!("{}:out2", node.id),
                    ),
                };
                output_buses.insert(make_port_key(&node.id, "out1"), out1);
                output_buses.insert(make_port_key(&node.id, "out2"), out2);
            }
            EffectGraphNodeData::MonoMix { ignored_channels } => {
                let bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| format!("{} input is missing", node.id))?;
                let (mono, included_count) =
                    mono_mix_channels_with_ignored(&bus.channels, ignored_channels);
                if included_count == 0 {
                    on_event(EffectGraphRuntimeEvent::NodeLog {
                        node_id: node.id.clone(),
                        severity: EffectGraphSeverity::Warning,
                        message: "Mono Mix ignored every channel and rendered silence".to_string(),
                    });
                }
                output_buses.insert(
                    make_port_key(&node.id, "out"),
                    dense_audio_bus(vec![mono], bus.sample_rate),
                );
            }
            EffectGraphNodeData::Gain { gain_db } => {
                let mut bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| format!("{} input is missing", node.id))?;
                let gain = 10.0f32.powf(*gain_db / 20.0);
                for channel in bus.channels.iter_mut() {
                    for sample in channel.iter_mut() {
                        *sample *= gain;
                    }
                }
                output_buses.insert(make_port_key(&node.id, "out"), bus);
            }
            EffectGraphNodeData::PitchShift { semitones } => {
                let bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| format!("{} input is missing", node.id))?;
                let mut channels = bus
                    .channels
                    .iter()
                    .map(|channel| {
                        crate::wave::process_pitchshift_offline(
                            channel,
                            bus.sample_rate,
                            bus.sample_rate,
                            *semitones,
                        )
                    })
                    .collect::<Vec<_>>();
                sync_channel_lengths(&mut channels);
                output_buses.insert(
                    make_port_key(&node.id, "out"),
                    EffectGraphAudioBus {
                        channels,
                        sample_rate: bus.sample_rate,
                        channel_layout: bus.channel_layout.clone(),
                    },
                );
            }
            EffectGraphNodeData::TimeStretch { rate } => {
                let bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| format!("{} input is missing", node.id))?;
                let mut channels = bus
                    .channels
                    .iter()
                    .map(|channel| {
                        crate::wave::process_timestretch_offline(
                            channel,
                            bus.sample_rate,
                            bus.sample_rate,
                            *rate,
                        )
                    })
                    .collect::<Vec<_>>();
                sync_channel_lengths(&mut channels);
                output_buses.insert(
                    make_port_key(&node.id, "out"),
                    EffectGraphAudioBus {
                        channels,
                        sample_rate: bus.sample_rate,
                        channel_layout: bus.channel_layout.clone(),
                    },
                );
            }
            EffectGraphNodeData::Speed { rate } => {
                let bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| format!("{} input is missing", node.id))?;
                let mut channels = process_speed_offline_channels(&bus.channels, *rate);
                sync_channel_lengths(&mut channels);
                output_buses.insert(
                    make_port_key(&node.id, "out"),
                    EffectGraphAudioBus {
                        channels,
                        sample_rate: bus.sample_rate,
                        channel_layout: bus.channel_layout.clone(),
                    },
                );
            }
            EffectGraphNodeData::SplitChannels => {
                let bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| format!("{} input is missing", node.id))?;
                let max_len = channels_frame_len(&bus.channels);
                for (index, port_id) in node.data.output_ports().iter().enumerate() {
                    let output_bus = if let Some(channel) = bus.channels.get(index) {
                        EffectGraphAudioBus {
                            channels: vec![channel.clone()],
                            sample_rate: bus.sample_rate,
                            channel_layout: make_split_output_layout(&bus.channel_layout, index),
                        }
                    } else {
                        silent_mono_bus(
                            max_len,
                            bus.sample_rate,
                            bus.channel_layout.declared_width,
                            index,
                        )
                    };
                    output_buses.insert(make_port_key(&node.id, port_id), output_bus);
                }
            }
            EffectGraphNodeData::CombineChannels => {
                let mut buses = node
                    .data
                    .input_ports()
                    .iter()
                    .filter_map(|port_id| {
                        effect_graph_input_bus_for_port(
                            &node.id,
                            port_id,
                            &input_sources,
                            &output_buses,
                        )
                    })
                    .collect::<Vec<_>>();
                if buses.is_empty() {
                    return Err(
                        "Combine Channels requires at least one connected input".to_string()
                    );
                }
                let combine_mode = combine_mode_from_buses(&buses)
                    .ok_or_else(|| "Combine Channels could not infer channel layout".to_string())?;
                if combine_mode == EffectGraphCombineMode::Mixed {
                    return Err(
                        "Combine Channels received an unsupported channel layout mix".to_string(),
                    );
                }
                let target_sample_rate = buses
                    .iter()
                    .map(|bus| bus.sample_rate.max(1))
                    .max()
                    .unwrap_or(input_bus.sample_rate.max(1));
                let mut longest_len = 0usize;
                for bus in buses.iter_mut() {
                    if bus.sample_rate != target_sample_rate {
                        *bus = resample_audio_bus(bus, target_sample_rate, resample_quality);
                    }
                    normalize_audio_bus_lengths(bus);
                    longest_len = longest_len.max(channels_frame_len(&bus.channels));
                }
                match combine_mode {
                    EffectGraphCombineMode::Concat => {
                        let mut channels = Vec::new();
                        for bus in buses.iter_mut() {
                            pad_channels_with_silence(&mut bus.channels, longest_len);
                            channels.extend(bus.channels.iter().cloned());
                        }
                        output_buses.insert(
                            make_port_key(&node.id, "out"),
                            dense_audio_bus(channels, target_sample_rate),
                        );
                    }
                    EffectGraphCombineMode::Restore => {
                        let (restored_bus, warnings) =
                            restore_channels_by_layout(buses, target_sample_rate)?;
                        for warning in warnings {
                            on_event(EffectGraphRuntimeEvent::NodeLog {
                                node_id: node.id.clone(),
                                severity: EffectGraphSeverity::Warning,
                                message: warning,
                            });
                        }
                        output_buses.insert(make_port_key(&node.id, "out"), restored_bus);
                    }
                    EffectGraphCombineMode::Adaptive => {
                        let (adaptive_bus, warnings) =
                            adaptive_combine_channels_by_layout(buses, target_sample_rate)?;
                        for warning in warnings {
                            on_event(EffectGraphRuntimeEvent::NodeLog {
                                node_id: node.id.clone(),
                                severity: EffectGraphSeverity::Info,
                                message: warning,
                            });
                        }
                        output_buses.insert(make_port_key(&node.id, "out"), adaptive_bus);
                    }
                    EffectGraphCombineMode::Mixed => unreachable!(),
                }
            }
            EffectGraphNodeData::DebugWaveform { .. } => {
                let bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| format!("{} input is missing", node.id))?;
                if run_mode == EffectGraphRunMode::TestPreview {
                    on_event(EffectGraphRuntimeEvent::NodeDebugPreview {
                        node_id: node.id.clone(),
                        preview: EffectGraphDebugPreview::Waveform {
                            mono: mixdown_channels_local(&bus.channels),
                            sample_rate: bus.sample_rate,
                        },
                    });
                }
                output_buses.insert(make_port_key(&node.id, "out"), bus);
            }
            EffectGraphNodeData::DebugSpectrum { mode, .. } => {
                let bus =
                    effect_graph_input_bus_for_port(&node.id, "in", &input_sources, &output_buses)
                        .ok_or_else(|| format!("{} input is missing", node.id))?;
                if run_mode == EffectGraphRunMode::TestPreview {
                    let mono = mixdown_channels_local(&bus.channels);
                    let cfg = effect_graph_debug_spectrogram_config(*mode);
                    let params =
                        crate::app::render::spectrogram::spectrogram_params(mono.len(), &cfg);
                    let values_db = crate::app::render::spectrogram::compute_spectrogram_tile(
                        &mono,
                        bus.sample_rate,
                        &params,
                        0,
                        params.frames,
                    );
                    on_event(EffectGraphRuntimeEvent::NodeDebugPreview {
                        node_id: node.id.clone(),
                        preview: EffectGraphDebugPreview::Spectrum {
                            spectrogram: crate::app::types::SpectrogramData {
                                frames: params.frames,
                                bins: params.bins,
                                frame_step: params.frame_step,
                                sample_rate: bus.sample_rate,
                                values_db,
                            },
                        },
                    });
                }
                output_buses.insert(make_port_key(&node.id, "out"), bus);
            }
        }
        on_event(EffectGraphRuntimeEvent::NodeFinished {
            node_id,
            elapsed_ms: started.elapsed().as_secs_f32() * 1000.0,
        });
    }
    final_output.ok_or_else(|| "Output node did not receive audio".to_string())
}

fn combine_mode_label(mode: EffectGraphCombineMode) -> &'static str {
    match mode {
        EffectGraphCombineMode::Concat => "concat",
        EffectGraphCombineMode::Restore => "restore",
        EffectGraphCombineMode::Adaptive => "adaptive",
        EffectGraphCombineMode::Mixed => "invalid",
    }
}

fn predict_effect_graph_output_format(
    document: &EffectGraphDocument,
    input_bus: &EffectGraphAudioBus,
    resample_quality: crate::wave::ResampleQuality,
) -> Result<EffectGraphPredictedFormat, String> {
    let output_bus = run_effect_graph_document(
        document,
        format_only_audio_bus(input_bus.channels.len(), input_bus.sample_rate),
        EffectGraphRunMode::ApplyToListSelection,
        resample_quality,
        |_| {},
    )?;
    let input_sources = build_port_edge_maps(document).0;
    let flow_hints = effect_graph_infer_flow_hints(document);
    let active_nodes = effect_graph_active_nodes(document);
    let combine_mode = effect_graph_topological_order(document)
        .into_iter()
        .filter_map(|node_id| {
            let node = document.nodes.iter().find(|node| node.id == node_id)?;
            if !active_nodes.contains(&node.id)
                || !matches!(node.data, EffectGraphNodeData::CombineChannels)
            {
                return None;
            }
            effect_graph_combine_mode_for_node_from_maps(node, &input_sources, &flow_hints)
        })
        .last();
    let summary = if let Some(mode) = combine_mode {
        format!(
            "Predicted: {} ch / {} Hz / {}",
            output_bus.channels.len().max(1),
            output_bus.sample_rate.max(1),
            combine_mode_label(mode)
        )
    } else {
        format!(
            "Predicted: {} ch / {} Hz",
            output_bus.channels.len().max(1),
            output_bus.sample_rate.max(1)
        )
    };
    Ok(EffectGraphPredictedFormat {
        channel_count: output_bus.channels.len().max(1),
        sample_rate: output_bus.sample_rate.max(1),
        combine_mode,
        summary,
    })
}

impl WavesPreviewer {
    pub(super) fn is_list_workspace_active(&self) -> bool {
        self.workspace_view == WorkspaceView::List
    }

    pub(super) fn is_editor_workspace_active(&self) -> bool {
        self.workspace_view == WorkspaceView::Editor && self.active_tab.is_some()
    }

    pub(super) fn is_effect_graph_workspace_active(&self) -> bool {
        self.workspace_view == WorkspaceView::EffectGraph && self.effect_graph.workspace_open
    }

    fn effect_graph_capture_undo_state(&self) -> EffectGraphUndoState {
        let mut draft = self.effect_graph.draft.clone();
        draft.canvas.zoom = self.effect_graph.canvas.zoom;
        draft.canvas.pan = self.effect_graph.canvas.pan;
        EffectGraphUndoState {
            active_template_id: self.effect_graph.active_template_id.clone(),
            draft,
            draft_dirty: self.effect_graph.draft_dirty,
        }
    }

    fn effect_graph_push_undo_state(&mut self, state: EffectGraphUndoState) {
        self.effect_graph.redo_stack.clear();
        if self.effect_graph.undo_stack.last() == Some(&state) {
            return;
        }
        self.effect_graph.undo_stack.push(state);
        while self.effect_graph.undo_stack.len() > 100 {
            self.effect_graph.undo_stack.remove(0);
        }
        self.last_undo_scope = UndoScope::EffectGraph;
    }

    pub(super) fn effect_graph_push_undo_snapshot(&mut self) {
        let state = self.effect_graph_capture_undo_state();
        self.effect_graph_push_undo_state(state);
    }

    fn effect_graph_restore_undo_state(&mut self, state: EffectGraphUndoState) {
        self.effect_graph.active_template_id = state.active_template_id;
        self.effect_graph.draft = state.draft;
        self.effect_graph.draft_dirty = state.draft_dirty;
        self.effect_graph.canvas.zoom = self.effect_graph.draft.canvas.zoom;
        self.effect_graph.canvas.pan = self.effect_graph.draft.canvas.pan;
        self.effect_graph.canvas.selected_nodes.clear();
        self.effect_graph.canvas.selected_edge_id = None;
        self.effect_graph.debug_previews.clear();
        self.revalidate_effect_graph_draft();
    }

    pub(super) fn effect_graph_undo(&mut self) -> bool {
        let Some(state) = self.effect_graph.undo_stack.pop() else {
            return false;
        };
        let redo_state = self.effect_graph_capture_undo_state();
        self.effect_graph.redo_stack.push(redo_state);
        self.effect_graph_restore_undo_state(state);
        self.last_undo_scope = UndoScope::EffectGraph;
        true
    }

    pub(super) fn effect_graph_redo(&mut self) -> bool {
        let Some(state) = self.effect_graph.redo_stack.pop() else {
            return false;
        };
        let undo_state = self.effect_graph_capture_undo_state();
        self.effect_graph.undo_stack.push(undo_state);
        self.effect_graph_restore_undo_state(state);
        self.last_undo_scope = UndoScope::EffectGraph;
        true
    }

    fn effect_graph_reset_history(&mut self) {
        self.effect_graph.undo_stack.clear();
        self.effect_graph.redo_stack.clear();
    }

    fn sync_effect_graph_debug_ui_state(&mut self) {
        let valid_node_ids = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<HashSet<_>>();
        self.effect_graph
            .debug_previews
            .retain(|node_id, _| valid_node_ids.contains(node_id));
        self.effect_graph
            .debug_view_state
            .retain(|node_id, _| valid_node_ids.contains(node_id));
        for node in self.effect_graph.draft.nodes.iter() {
            if matches!(
                node.data,
                EffectGraphNodeData::DebugWaveform { .. }
                    | EffectGraphNodeData::DebugSpectrum { .. }
            ) {
                self.effect_graph
                    .debug_view_state
                    .entry(node.id.clone())
                    .or_insert_with(EffectGraphDebugViewState::default);
            }
        }
    }

    pub(super) fn effect_graph_templates_dir() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
        let mut path = PathBuf::from(base);
        path.push("NeoWaves");
        path.push("effect_graph_templates");
        let _ = std::fs::create_dir_all(&path);
        Some(path)
    }

    pub(super) fn push_effect_graph_console(
        &mut self,
        severity: EffectGraphSeverity,
        scope: impl Into<String>,
        message: impl Into<String>,
        node_id: Option<String>,
    ) {
        let line = super::types::EffectGraphConsoleLine {
            timestamp_unix_ms: now_unix_ms(),
            severity,
            scope: scope.into(),
            message: message.into(),
            node_id,
        };
        self.effect_graph.console.lines.push_back(line);
        while self.effect_graph.console.lines.len() > self.effect_graph.console.max_lines {
            self.effect_graph.console.lines.pop_front();
        }
    }

    pub(super) fn load_effect_graph_library(&mut self) {
        let mut entries = Vec::new();
        let Some(dir) = Self::effect_graph_templates_dir() else {
            self.effect_graph.library.entries.clear();
            self.effect_graph.library.last_error =
                Some("Could not resolve effect graph template directory".to_string());
            return;
        };
        self.effect_graph.library.last_error = None;
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(v) => v,
            Err(err) => {
                self.effect_graph.library.entries.clear();
                self.effect_graph.library.last_error = Some(err.to_string());
                return;
            }
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or_default();
            if !file_name.ends_with(".nwgraph.json") {
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(v) => v,
                Err(err) => {
                    self.push_effect_graph_console(
                        EffectGraphSeverity::Warning,
                        "library",
                        format!("template read failed: {} ({err})", path.display()),
                        None,
                    );
                    continue;
                }
            };
            let parsed = match serde_json::from_str::<EffectGraphTemplateFile>(&text) {
                Ok(v) => v,
                Err(err) => {
                    self.push_effect_graph_console(
                        EffectGraphSeverity::Warning,
                        "library",
                        format!("template parse failed: {} ({err})", path.display()),
                        None,
                    );
                    continue;
                }
            };
            let issues = validate_effect_graph_document(&parsed.graph);
            let valid = !issues
                .iter()
                .any(|issue| issue.severity == EffectGraphSeverity::Error);
            entries.push(EffectGraphLibraryEntry {
                template_id: parsed.template_id,
                name: parsed.name,
                path,
                created_at_unix_ms: parsed.created_at_unix_ms,
                updated_at_unix_ms: parsed.updated_at_unix_ms,
                valid,
            });
        }
        entries.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
                .then_with(|| a.template_id.cmp(&b.template_id))
        });
        self.effect_graph.library.entries = entries;
    }

    pub(super) fn effect_graph_entry_by_id(
        &self,
        template_id: &str,
    ) -> Option<&EffectGraphLibraryEntry> {
        self.effect_graph
            .library
            .entries
            .iter()
            .find(|entry| entry.template_id == template_id)
    }

    fn read_effect_graph_template(path: &Path) -> Result<EffectGraphTemplateFile, String> {
        let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
        let mut file = serde_json::from_str::<EffectGraphTemplateFile>(&text)
            .map_err(|err| err.to_string())?;
        file.schema_version = EFFECT_GRAPH_SCHEMA_VERSION;
        file.graph = clone_sanitized_document(&file.graph);
        Ok(file)
    }

    pub(super) fn revalidate_effect_graph_draft(&mut self) {
        ensure_effect_graph_node_layout(&mut self.effect_graph.draft);
        self.effect_graph.validation = validate_effect_graph_document(&self.effect_graph.draft);
        self.sync_effect_graph_debug_ui_state();
        if self.effect_graph.canvas.zoom <= 0.0 {
            self.effect_graph.canvas.zoom = self.effect_graph.draft.canvas.zoom.max(0.25);
        }
    }

    pub(super) fn effect_graph_has_errors(&self) -> bool {
        self.effect_graph
            .validation
            .iter()
            .any(|issue| issue.severity == EffectGraphSeverity::Error)
    }

    pub(super) fn open_effect_graph_workspace(&mut self) {
        self.effect_graph.workspace_open = true;
        self.effect_graph.last_editor_tab = self.active_tab;
        self.workspace_view = WorkspaceView::EffectGraph;
        self.load_effect_graph_library();
        if let Some(template_id) = self.effect_graph.active_template_id.clone() {
            if self.effect_graph_entry_by_id(&template_id).is_some() {
                let _ = self.load_effect_graph_template_into_draft(&template_id);
            }
        }
        self.revalidate_effect_graph_draft();
    }

    pub(super) fn request_close_effect_graph_workspace(&mut self) {
        if self.effect_graph.draft_dirty {
            self.effect_graph.pending_action = Some(EffectGraphPendingAction::CloseWorkspace);
            self.effect_graph.show_unsaved_prompt = true;
            return;
        }
        self.close_effect_graph_workspace_now();
    }

    pub(super) fn close_effect_graph_workspace_now(&mut self) {
        if matches!(
            self.playback_session.source,
            crate::app::PlaybackSourceKind::EffectGraph
        ) {
            self.audio.stop();
            self.playback_session.source = crate::app::PlaybackSourceKind::None;
            self.playback_session.is_playing = false;
        }
        self.effect_graph.workspace_open = false;
        self.effect_graph.pending_action = None;
        self.effect_graph.show_unsaved_prompt = false;
        if let Some(tab_idx) = self.effect_graph.last_editor_tab {
            if tab_idx < self.tabs.len() {
                self.active_tab = Some(tab_idx);
                self.workspace_view = WorkspaceView::Editor;
                return;
            }
        }
        self.workspace_view = WorkspaceView::List;
    }

    pub(super) fn execute_effect_graph_pending_action(&mut self, discard_changes: bool) {
        let Some(action) = self.effect_graph.pending_action.clone() else {
            self.effect_graph.show_unsaved_prompt = false;
            return;
        };
        self.effect_graph.show_unsaved_prompt = false;
        self.effect_graph.pending_action = None;
        if discard_changes {
            if !matches!(action, EffectGraphPendingAction::DeleteTemplate(_)) {
                if let Some(template_id) = self.effect_graph.active_template_id.clone() {
                    let _ = self.load_effect_graph_template_into_draft(&template_id);
                } else {
                    self.effect_graph.draft = EffectGraphDocument::default();
                    self.effect_graph.draft_dirty = false;
                    self.effect_graph.canvas.zoom = self.effect_graph.draft.canvas.zoom;
                    self.effect_graph.canvas.pan = self.effect_graph.draft.canvas.pan;
                    self.revalidate_effect_graph_draft();
                }
            }
        }
        match action {
            EffectGraphPendingAction::CloseWorkspace => self.close_effect_graph_workspace_now(),
            EffectGraphPendingAction::SwitchTemplate(template_id) => {
                let _ = self.load_effect_graph_template_into_draft(&template_id);
            }
            EffectGraphPendingAction::DeleteTemplate(template_id) => {
                if let Err(err) = self.delete_effect_graph_template(&template_id) {
                    self.push_effect_graph_console(
                        EffectGraphSeverity::Error,
                        "library",
                        err,
                        None,
                    );
                }
            }
        }
    }

    pub(super) fn effect_graph_new_unsaved_template(&mut self, name: Option<String>) {
        let mut draft = EffectGraphDocument::default();
        if let Some(name) = name.filter(|value| !value.trim().is_empty()) {
            draft.name = name.trim().to_string();
        }
        self.effect_graph.active_template_id = None;
        self.effect_graph.draft = draft;
        self.effect_graph.draft_dirty = false;
        self.effect_graph.canvas.zoom = self.effect_graph.draft.canvas.zoom;
        self.effect_graph.canvas.pan = self.effect_graph.draft.canvas.pan;
        self.effect_graph.canvas.selected_nodes.clear();
        self.effect_graph.canvas.selected_edge_id = None;
        self.effect_graph.clipboard_paste_serial = 0;
        self.effect_graph.debug_previews.clear();
        self.sync_effect_graph_debug_ui_state();
        self.effect_graph_reset_history();
        self.revalidate_effect_graph_draft();
    }

    pub(super) fn load_effect_graph_template_into_draft(
        &mut self,
        template_id: &str,
    ) -> Result<(), String> {
        let entry = self
            .effect_graph_entry_by_id(template_id)
            .cloned()
            .ok_or_else(|| format!("template not found: {template_id}"))?;
        let file = Self::read_effect_graph_template(&entry.path)?;
        self.effect_graph.active_template_id = Some(file.template_id.clone());
        self.effect_graph.draft = file.graph;
        self.effect_graph.draft.name = file.name;
        self.effect_graph.draft_dirty = false;
        self.effect_graph.canvas.zoom = self.effect_graph.draft.canvas.zoom;
        self.effect_graph.canvas.pan = self.effect_graph.draft.canvas.pan;
        self.effect_graph.canvas.selected_nodes.clear();
        self.effect_graph.canvas.selected_edge_id = None;
        self.effect_graph.clipboard_paste_serial = 0;
        self.effect_graph.debug_previews.clear();
        self.sync_effect_graph_debug_ui_state();
        self.effect_graph_reset_history();
        self.revalidate_effect_graph_draft();
        Ok(())
    }

    pub(super) fn request_switch_effect_graph_template(&mut self, template_id: &str) {
        if self.effect_graph.draft_dirty {
            self.effect_graph.pending_action = Some(EffectGraphPendingAction::SwitchTemplate(
                template_id.to_string(),
            ));
            self.effect_graph.show_unsaved_prompt = true;
            return;
        }
        let _ = self.load_effect_graph_template_into_draft(template_id);
    }

    pub(super) fn request_delete_effect_graph_template(&mut self, template_id: &str) {
        self.effect_graph.pending_action = Some(EffectGraphPendingAction::DeleteTemplate(
            template_id.to_string(),
        ));
        self.effect_graph.show_unsaved_prompt = true;
    }

    pub(super) fn delete_effect_graph_template(&mut self, template_id: &str) -> Result<(), String> {
        let entry = self
            .effect_graph_entry_by_id(template_id)
            .cloned()
            .ok_or_else(|| format!("template not found: {template_id}"))?;
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(format!(
                    "failed to delete template: {} ({err})",
                    entry.path.display()
                ))
            }
        }
        let deleting_active = self.effect_graph.active_template_id.as_deref() == Some(template_id);
        let was_dirty = self.effect_graph.draft_dirty;
        self.load_effect_graph_library();
        if deleting_active {
            self.effect_graph.active_template_id = None;
            self.effect_graph.draft_dirty = was_dirty;
        }
        self.effect_graph.debug_previews.clear();
        self.sync_effect_graph_debug_ui_state();
        self.effect_graph.pending_action = None;
        self.effect_graph.show_unsaved_prompt = false;
        self.revalidate_effect_graph_draft();
        self.push_effect_graph_console(
            EffectGraphSeverity::Info,
            "library",
            format!("template deleted: {}", entry.path.display()),
            None,
        );
        Ok(())
    }

    pub(super) fn save_effect_graph_draft(&mut self, save_as_new: bool) -> Result<(), String> {
        let mut graph = clone_sanitized_document(&self.effect_graph.draft);
        let now = now_unix_ms();
        let mut template_id = self.effect_graph.active_template_id.clone();
        let mut created_at = now;
        let path = if save_as_new || template_id.is_none() {
            let name = graph.name.trim();
            if name.is_empty() {
                return Err("Template name is empty".to_string());
            }
            let id = format!("{}_{}", sanitize_filename_component(name), now);
            template_id = Some(id.clone());
            let mut path = Self::effect_graph_templates_dir()
                .ok_or_else(|| "Could not resolve effect graph template directory".to_string())?;
            path.push(format!("{}.nwgraph.json", sanitize_filename_component(&id)));
            path
        } else {
            let id = template_id.clone().unwrap_or_default();
            if let Some(entry) = self.effect_graph_entry_by_id(&id) {
                created_at = entry.created_at_unix_ms;
                entry.path.clone()
            } else {
                let mut path = Self::effect_graph_templates_dir().ok_or_else(|| {
                    "Could not resolve effect graph template directory".to_string()
                })?;
                path.push(format!("{}.nwgraph.json", sanitize_filename_component(&id)));
                path
            }
        };
        graph.canvas.zoom = self.effect_graph.canvas.zoom.clamp(0.25, 2.5);
        graph.canvas.pan = self.effect_graph.canvas.pan;
        let template = EffectGraphTemplateFile {
            schema_version: EFFECT_GRAPH_SCHEMA_VERSION,
            template_id: template_id.clone().unwrap_or_default(),
            name: graph.name.clone(),
            created_at_unix_ms: created_at,
            updated_at_unix_ms: now,
            graph,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let text = serde_json::to_string_pretty(&template).map_err(|err| err.to_string())?;
        std::fs::write(&path, text).map_err(|err| err.to_string())?;
        self.effect_graph.active_template_id = template_id;
        self.effect_graph.draft_dirty = false;
        self.load_effect_graph_library();
        self.effect_graph_reset_history();
        self.revalidate_effect_graph_draft();
        self.push_effect_graph_console(
            EffectGraphSeverity::Info,
            "library",
            format!("template saved: {}", path.display()),
            None,
        );
        Ok(())
    }

    fn effective_audio_bus_for_path(&self, path: &Path) -> Option<EffectGraphAudioBus> {
        if let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.path.as_path() == path && tab.dirty)
        {
            return Some(dense_audio_bus(
                tab.ch_samples.clone(),
                self.effective_sample_rate_for_path(path).unwrap_or(48_000),
            ));
        }
        if let Some(cached) = self.edited_cache.get(path) {
            return Some(dense_audio_bus(
                cached.ch_samples.clone(),
                self.effective_sample_rate_for_path(path).unwrap_or(48_000),
            ));
        }
        if let Some(item) = self.item_for_path(path) {
            if let Some(audio) = item.virtual_audio.as_ref() {
                return Some(dense_audio_bus(
                    audio.channels.clone(),
                    item.virtual_state
                        .as_ref()
                        .map(|state| state.sample_rate)
                        .or_else(|| item.meta.as_ref().map(|meta| meta.sample_rate))
                        .filter(|sample_rate| *sample_rate > 0)
                        .unwrap_or(48_000),
                ));
            }
            if matches!(item.source, MediaSource::External) {
                return None;
            }
        }
        if !path.is_file() {
            return None;
        }
        let (mut channels, in_sr) = crate::wave::decode_wav_multi(path).ok()?;
        if let Some(depth) = self.bit_depth_override.get(path).copied() {
            crate::wave::quantize_channels_in_place(&mut channels, depth);
        }
        Some(dense_audio_bus(channels, in_sr.max(1)))
    }

    fn effect_graph_monitor_audio_from_bus(
        &mut self,
        bus: &EffectGraphAudioBus,
    ) -> Arc<AudioBuffer> {
        let device_sr = self.audio.shared.out_sample_rate.max(1);
        let quality = Self::to_wave_resample_quality(self.src_quality);
        let monitor_channels = monitor_channels_from_bus_at_rate(bus, device_sr, quality);
        if bus.channels.len() > 2 {
            self.push_effect_graph_console(
                EffectGraphSeverity::Info,
                "monitor",
                format!(
                    "Preview monitor downmixed from {}ch to stereo",
                    bus.channels.len()
                ),
                None,
            );
        }
        if bus.sample_rate != device_sr {
            self.push_effect_graph_console(
                EffectGraphSeverity::Info,
                "monitor",
                format!(
                    "Preview monitor resampled {} -> {} Hz",
                    bus.sample_rate.max(1),
                    device_sr
                ),
                None,
            );
        }
        Arc::new(AudioBuffer::from_channels(monitor_channels))
    }

    fn build_effect_graph_worker_inputs(&self, paths: &[PathBuf]) -> Vec<EffectGraphWorkerInput> {
        let monitor_sr = self.audio.shared.out_sample_rate.max(1);
        let resample_quality = Self::to_wave_resample_quality(self.src_quality);
        paths
            .iter()
            .cloned()
            .map(|path| EffectGraphWorkerInput {
                bit_depth: self.bit_depth_override.get(&path).copied(),
                input_bus: self.effective_audio_bus_for_path(&path),
                monitor_sr,
                path,
                resample_quality,
            })
            .collect()
    }

    fn spawn_effect_graph_worker(
        &mut self,
        mode: EffectGraphRunMode,
        document: EffectGraphDocument,
        template_stamp: AppliedEffectGraphStamp,
        inputs: Vec<EffectGraphWorkerInput>,
    ) {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc;

        self.cancel_effect_graph_run();
        let (tx, rx) = mpsc::channel::<EffectGraphWorkerEvent>();
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let cancel_thread = cancel_requested.clone();
        self.effect_graph.runner.mode = Some(mode);
        self.effect_graph.runner.started_at = Some(Instant::now());
        self.effect_graph.runner.total = inputs.len();
        self.effect_graph.runner.done = 0;
        self.effect_graph.runner.current_path = None;
        self.effect_graph.runner.template_stamp = Some(template_stamp);
        self.effect_graph.runner.rx = Some(rx);
        self.effect_graph.runner.cancel_requested = Some(cancel_requested);
        self.effect_graph.runner.node_status.clear();
        for node in document.nodes.iter() {
            self.effect_graph.runner.node_status.insert(
                node.id.clone(),
                EffectGraphNodeRunStatus {
                    phase: EffectGraphNodeRunPhase::Idle,
                    elapsed_ms: None,
                    error: None,
                },
            );
        }
        std::thread::spawn(move || {
            let total = inputs.len();
            let _ = tx.send(EffectGraphWorkerEvent::RunStarted { mode, total });
            for (index, input) in inputs.into_iter().enumerate() {
                if cancel_thread.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(EffectGraphWorkerEvent::PathStarted {
                    path: input.path.clone(),
                    index: index + 1,
                    total,
                });
                let input_bus = if let Some(input_bus) = input.input_bus.clone() {
                    input_bus
                } else {
                    let decoded = crate::wave::decode_wav_multi(&input.path);
                    let (mut channels, in_sr) = match decoded {
                        Ok(v) => v,
                        Err(err) => {
                            let _ = tx.send(EffectGraphWorkerEvent::Failed {
                                path: Some(input.path.clone()),
                                node_id: None,
                                message: format!("decode failed: {err}"),
                            });
                            continue;
                        }
                    };
                    if let Some(depth) = input.bit_depth {
                        crate::wave::quantize_channels_in_place(&mut channels, depth);
                    }
                    dense_audio_bus(channels, in_sr.max(1))
                };
                let started = Instant::now();
                let result = run_effect_graph_document(
                    &document,
                    input_bus,
                    mode,
                    input.resample_quality,
                    |event| {
                        let _ = match event {
                            EffectGraphRuntimeEvent::NodeStarted(node_id) => {
                                tx.send(EffectGraphWorkerEvent::NodeStarted { node_id })
                            }
                            EffectGraphRuntimeEvent::NodeFinished {
                                node_id,
                                elapsed_ms,
                            } => tx.send(EffectGraphWorkerEvent::NodeFinished {
                                node_id,
                                elapsed_ms,
                            }),
                            EffectGraphRuntimeEvent::NodeLog {
                                node_id,
                                severity,
                                message,
                            } => tx.send(EffectGraphWorkerEvent::NodeLog {
                                node_id,
                                severity,
                                message,
                            }),
                            EffectGraphRuntimeEvent::NodeDebugPreview { node_id, preview } => tx
                                .send(EffectGraphWorkerEvent::NodeDebugPreview {
                                    node_id,
                                    preview,
                                }),
                        };
                    },
                );
                match result {
                    Ok(output_bus) => {
                        let monitor_audio = monitor_channels_from_bus_at_rate(
                            &output_bus,
                            input.monitor_sr,
                            input.resample_quality,
                        );
                        let _ = tx.send(EffectGraphWorkerEvent::PathFinished {
                            path: input.path,
                            output_bus,
                            monitor_audio,
                            total_elapsed_ms: started.elapsed().as_secs_f32() * 1000.0,
                        });
                    }
                    Err(message) => {
                        let _ = tx.send(EffectGraphWorkerEvent::Failed {
                            path: Some(input.path),
                            node_id: None,
                            message,
                        });
                    }
                }
            }
            let _ = tx.send(EffectGraphWorkerEvent::Finished);
        });
    }

    pub(super) fn cancel_effect_graph_run(&mut self) {
        if let Some(cancel) = self.effect_graph.runner.cancel_requested.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.effect_graph.runner.rx = None;
        self.effect_graph.runner.mode = None;
        self.effect_graph.runner.started_at = None;
        self.effect_graph.runner.current_path = None;
    }

    pub(super) fn effect_graph_use_current_selection_target(&mut self) {
        if let Some(path) = self.selected_path_buf() {
            self.effect_graph.tester.target_path_input = path.display().to_string();
            self.effect_graph.tester.target_path = Some(path);
        }
    }

    pub(super) fn effect_graph_test_target_candidate(&self) -> Option<PathBuf> {
        self.effect_graph
            .tester
            .target_path
            .clone()
            .or_else(|| {
                let trimmed = self.effect_graph.tester.target_path_input.trim();
                (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
            })
            .or_else(|| self.selected_path_buf())
    }

    pub(super) fn effect_graph_uses_embedded_sample(&self) -> bool {
        self.effect_graph_test_target_candidate().is_none()
    }

    fn effect_graph_format_only_bus_for_path(&self, path: &Path) -> Option<EffectGraphAudioBus> {
        if let Some(tab) = self
            .tabs
            .iter()
            .find(|tab| tab.path.as_path() == path && tab.dirty)
        {
            return Some(format_only_audio_bus(
                tab.ch_samples.len(),
                self.effective_sample_rate_for_path(path).unwrap_or(48_000),
            ));
        }
        if let Some(cached) = self.edited_cache.get(path) {
            return Some(format_only_audio_bus(
                cached.ch_samples.len(),
                self.effective_sample_rate_for_path(path).unwrap_or(48_000),
            ));
        }
        if let Some(item) = self.item_for_path(path) {
            if let Some(audio) = item.virtual_audio.as_ref() {
                return Some(format_only_audio_bus(
                    audio.channels.len(),
                    item.virtual_state
                        .as_ref()
                        .map(|state| state.sample_rate)
                        .or_else(|| item.meta.as_ref().map(|meta| meta.sample_rate))
                        .filter(|sample_rate| *sample_rate > 0)
                        .unwrap_or(48_000),
                ));
            }
            if let Some(meta) = item.meta.as_ref() {
                return Some(format_only_audio_bus(
                    usize::from(meta.channels.max(1)),
                    meta.sample_rate.max(1),
                ));
            }
            if matches!(item.source, MediaSource::External) {
                return None;
            }
        }
        None
    }

    fn effect_graph_prediction_input_bus(&self) -> Result<EffectGraphAudioBus, String> {
        if let Some(target_path) = self.effect_graph_test_target_candidate() {
            self.effect_graph_format_only_bus_for_path(&target_path)
                .ok_or_else(|| format!("Could not inspect input audio: {}", target_path.display()))
        } else {
            let (_, sample_rate) = embedded_effect_graph_sample_channels()?;
            Ok(format_only_audio_bus(1, sample_rate))
        }
    }

    pub(super) fn effect_graph_predicted_output_format(
        &self,
    ) -> Result<EffectGraphPredictedFormat, String> {
        let input_bus = self.effect_graph_prediction_input_bus()?;
        predict_effect_graph_output_format(
            &clone_sanitized_document(&self.effect_graph.draft),
            &input_bus,
            Self::to_wave_resample_quality(self.src_quality),
        )
    }

    pub(super) fn effect_graph_predicted_output_summary(&self) -> Option<String> {
        self.effect_graph_predicted_output_format()
            .ok()
            .map(|predicted| predicted.summary)
    }

    pub(super) fn effect_graph_test_input_summary(&self) -> String {
        if let Some(path) = self.effect_graph_test_target_candidate() {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| format!("Source: {name}"))
                .unwrap_or_else(|| format!("Source: {}", path.display()))
        } else {
            "Source: Embedded sample".to_string()
        }
    }

    fn effect_graph_embedded_sample_bus(&self) -> Result<EffectGraphAudioBus, String> {
        let (channels, in_sr) = embedded_effect_graph_sample_channels()?;
        Ok(dense_audio_bus(channels, in_sr.max(1)))
    }

    pub(super) fn effect_graph_preview_input_audio(&mut self) -> Result<Arc<AudioBuffer>, String> {
        let bus = if let Some(target_path) = self.effect_graph_test_target_candidate() {
            self.effective_audio_bus_for_path(&target_path)
                .ok_or_else(|| format!("Could not load input audio: {}", target_path.display()))?
        } else {
            self.effect_graph_embedded_sample_bus()?
        };
        Ok(self.effect_graph_monitor_audio_from_bus(&bus))
    }

    fn effect_graph_resolve_test_input_audio(
        &self,
    ) -> Result<(EffectGraphAudioBus, Option<PathBuf>, PathBuf), String> {
        if let Some(target_path) = self.effect_graph_test_target_candidate() {
            let input_bus = self
                .effective_audio_bus_for_path(&target_path)
                .ok_or_else(|| format!("Could not load input audio: {}", target_path.display()))?;
            Ok((input_bus, Some(target_path.clone()), target_path))
        } else {
            Ok((
                self.effect_graph_embedded_sample_bus()?,
                None,
                PathBuf::from(EFFECT_GRAPH_EMBEDDED_SAMPLE_WORKER_PATH),
            ))
        }
    }

    pub(super) fn start_effect_graph_test_run(&mut self) -> Result<(), String> {
        self.revalidate_effect_graph_draft();
        if self.effect_graph_has_errors() {
            return Err("Effect Graph has validation errors".to_string());
        }
        self.effect_graph.debug_previews.clear();
        let (input_bus, target_path, worker_path) = self.effect_graph_resolve_test_input_audio()?;
        self.effect_graph.tester.target_path = target_path.clone();
        if let Some(path) = target_path {
            self.effect_graph.tester.target_path_input = path.display().to_string();
        } else {
            self.effect_graph.tester.target_path_input.clear();
            self.push_effect_graph_console(
                EffectGraphSeverity::Info,
                "test",
                format!("using {EFFECT_GRAPH_EMBEDDED_SAMPLE_LABEL}"),
                None,
            );
        }
        self.effect_graph.tester.last_input_bus = Some(input_bus.clone());
        self.effect_graph.tester.last_input_audio =
            Some(self.effect_graph_monitor_audio_from_bus(&input_bus));
        self.effect_graph.tester.last_output_bus = None;
        self.effect_graph.tester.last_output_audio = None;
        self.effect_graph.tester.last_error = None;
        self.effect_graph.tester.last_run_ms = None;
        self.effect_graph.tester.last_output_summary.clear();
        let stamp = if let Some(template_id) = self.effect_graph.active_template_id.clone() {
            if let Some(entry) = self.effect_graph_entry_by_id(&template_id) {
                AppliedEffectGraphStamp {
                    template_id,
                    template_name: entry.name.clone(),
                    template_updated_at_unix_ms: entry.updated_at_unix_ms,
                }
            } else {
                AppliedEffectGraphStamp {
                    template_id: "draft".to_string(),
                    template_name: self.effect_graph.draft.name.clone(),
                    template_updated_at_unix_ms: 0,
                }
            }
        } else {
            AppliedEffectGraphStamp {
                template_id: "draft".to_string(),
                template_name: self.effect_graph.draft.name.clone(),
                template_updated_at_unix_ms: 0,
            }
        };
        self.spawn_effect_graph_worker(
            EffectGraphRunMode::TestPreview,
            clone_sanitized_document(&self.effect_graph.draft),
            stamp,
            vec![EffectGraphWorkerInput {
                path: worker_path,
                input_bus: Some(input_bus),
                bit_depth: None,
                monitor_sr: self.audio.shared.out_sample_rate.max(1),
                resample_quality: Self::to_wave_resample_quality(self.src_quality),
            }],
        );
        Ok(())
    }

    pub(super) fn apply_effect_graph_template_to_paths(
        &mut self,
        template_id: &str,
        paths: &[PathBuf],
    ) -> Result<(), String> {
        let entry = self
            .effect_graph_entry_by_id(template_id)
            .cloned()
            .ok_or_else(|| format!("template not found: {template_id}"))?;
        let file = Self::read_effect_graph_template(&entry.path)?;
        let validation = validate_effect_graph_document(&file.graph);
        if validation
            .iter()
            .any(|issue| issue.severity == EffectGraphSeverity::Error)
        {
            return Err(format!("template has validation errors: {}", entry.name));
        }
        let inputs = self.build_effect_graph_worker_inputs(paths);
        if inputs.is_empty() {
            return Err("No paths selected".to_string());
        }
        self.spawn_effect_graph_worker(
            EffectGraphRunMode::ApplyToListSelection,
            file.graph,
            AppliedEffectGraphStamp {
                template_id: entry.template_id,
                template_name: entry.name,
                template_updated_at_unix_ms: entry.updated_at_unix_ms,
            },
            inputs,
        );
        Ok(())
    }

    fn waveform_from_channels(channels: &[Vec<f32>]) -> Vec<(f32, f32)> {
        let len = channels.get(0).map(|channel| channel.len()).unwrap_or(0);
        let mono = Self::mixdown_channels(channels, len);
        let mut waveform = Vec::new();
        crate::wave::build_minmax(&mut waveform, &mono, 2048);
        waveform
    }

    fn apply_effect_graph_result_to_path(
        &mut self,
        path: &Path,
        channels: Vec<Vec<f32>>,
        final_sample_rate: u32,
    ) {
        let bits = self.effective_bits_for_path(path).unwrap_or(32);
        let waveform = Self::waveform_from_channels(&channels);
        let display_meta = Some(Self::build_meta_from_audio(
            &channels,
            final_sample_rate.max(1),
            bits,
        ));
        let new_len = channels.get(0).map(|channel| channel.len()).unwrap_or(0);
        let template_stamp = self.effect_graph.runner.template_stamp.clone();

        if let Some(tab_idx) = self.tabs.iter().position(|tab| tab.path.as_path() == path) {
            self.clear_preview_if_any(tab_idx);
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                let old_len = tab.samples_len.max(1);
                tab.selection = remap_range(tab.selection, old_len, new_len);
                tab.loop_region = remap_range(tab.loop_region, old_len, new_len);
                tab.loop_region_committed =
                    remap_range(tab.loop_region_committed, old_len, new_len);
                tab.loop_region_applied = remap_range(tab.loop_region_applied, old_len, new_len);
                tab.loop_markers_saved = remap_range(tab.loop_markers_saved, old_len, new_len);
                tab.trim_range = remap_range(tab.trim_range, old_len, new_len);
                tab.fade_in_range = remap_range(tab.fade_in_range, old_len, new_len);
                tab.fade_out_range = remap_range(tab.fade_out_range, old_len, new_len);
                tab.markers = remap_markers(&tab.markers, old_len, new_len);
                tab.markers_committed = remap_markers(&tab.markers_committed, old_len, new_len);
                tab.markers_saved = remap_markers(&tab.markers_saved, old_len, new_len);
                tab.markers_applied = remap_markers(&tab.markers_applied, old_len, new_len);
                tab.ch_samples = channels.clone();
                tab.samples_len = new_len;
                tab.waveform_minmax = waveform.clone();
                tab.dirty = true;
                tab.preview_overlay = None;
                tab.preview_audio_tool = None;
            }
            if self.active_tab == Some(tab_idx) && self.is_editor_workspace_active() {
                self.preview_restore_audio_for_tab(tab_idx);
            }
        }

        let mut cached = if let Some(tab) = self.tabs.iter().find(|tab| tab.path.as_path() == path)
        {
            CachedEdit {
                ch_samples: tab.ch_samples.clone(),
                samples_len: tab.samples_len,
                waveform_minmax: tab.waveform_minmax.clone(),
                display_meta: display_meta.clone(),
                dirty: tab.dirty,
                loop_region: tab.loop_region,
                loop_region_committed: tab.loop_region_committed,
                loop_region_applied: tab.loop_region_applied,
                loop_markers_saved: tab.loop_markers_saved,
                loop_markers_dirty: tab.loop_markers_dirty,
                markers: tab.markers.clone(),
                markers_saved: tab.markers_saved.clone(),
                markers_committed: tab.markers_committed.clone(),
                markers_applied: tab.markers_applied.clone(),
                markers_dirty: tab.markers_dirty,
                trim_range: tab.trim_range,
                loop_xfade_samples: tab.loop_xfade_samples,
                loop_xfade_shape: tab.loop_xfade_shape,
                fade_in_range: tab.fade_in_range,
                fade_out_range: tab.fade_out_range,
                fade_in_shape: tab.fade_in_shape,
                fade_out_shape: tab.fade_out_shape,
                loop_mode: tab.loop_mode,
                bpm_enabled: tab.bpm_enabled,
                bpm_value: tab.bpm_value,
                bpm_user_set: tab.bpm_user_set,
                bpm_offset_sec: tab.bpm_offset_sec,
                snap_zero_cross: tab.snap_zero_cross,
                tool_state: tab.tool_state,
                active_tool: tab.active_tool,
                plugin_fx_draft: tab.plugin_fx_draft.clone(),
                show_waveform_overlay: tab.show_waveform_overlay,
                applied_effect_graph: template_stamp.clone(),
            }
        } else if let Some(existing) = self.edited_cache.get(path).cloned() {
            let old_len = existing.samples_len.max(1);
            CachedEdit {
                ch_samples: channels.clone(),
                samples_len: new_len,
                waveform_minmax: waveform.clone(),
                display_meta: display_meta.clone(),
                dirty: true,
                loop_region: remap_range(existing.loop_region, old_len, new_len),
                loop_region_committed: remap_range(
                    existing.loop_region_committed,
                    old_len,
                    new_len,
                ),
                loop_region_applied: remap_range(existing.loop_region_applied, old_len, new_len),
                loop_markers_saved: remap_range(existing.loop_markers_saved, old_len, new_len),
                loop_markers_dirty: existing.loop_markers_dirty,
                markers: remap_markers(&existing.markers, old_len, new_len),
                markers_saved: remap_markers(&existing.markers_saved, old_len, new_len),
                markers_committed: remap_markers(&existing.markers_committed, old_len, new_len),
                markers_applied: remap_markers(&existing.markers_applied, old_len, new_len),
                markers_dirty: existing.markers_dirty,
                trim_range: remap_range(existing.trim_range, old_len, new_len),
                loop_xfade_samples: existing.loop_xfade_samples,
                loop_xfade_shape: existing.loop_xfade_shape,
                fade_in_range: remap_range(existing.fade_in_range, old_len, new_len),
                fade_out_range: remap_range(existing.fade_out_range, old_len, new_len),
                fade_in_shape: existing.fade_in_shape,
                fade_out_shape: existing.fade_out_shape,
                loop_mode: existing.loop_mode,
                bpm_enabled: existing.bpm_enabled,
                bpm_value: existing.bpm_value,
                bpm_user_set: existing.bpm_user_set,
                bpm_offset_sec: existing.bpm_offset_sec,
                snap_zero_cross: existing.snap_zero_cross,
                tool_state: existing.tool_state,
                active_tool: existing.active_tool,
                plugin_fx_draft: existing.plugin_fx_draft.clone(),
                show_waveform_overlay: existing.show_waveform_overlay,
                applied_effect_graph: template_stamp.clone(),
            }
        } else {
            CachedEdit {
                ch_samples: channels.clone(),
                samples_len: new_len,
                waveform_minmax: waveform.clone(),
                display_meta: display_meta.clone(),
                dirty: true,
                loop_region: None,
                loop_region_committed: None,
                loop_region_applied: None,
                loop_markers_saved: None,
                loop_markers_dirty: false,
                markers: Vec::new(),
                markers_saved: Vec::new(),
                markers_committed: Vec::new(),
                markers_applied: Vec::new(),
                markers_dirty: false,
                trim_range: None,
                loop_xfade_samples: 0,
                loop_xfade_shape: super::types::LoopXfadeShape::EqualPower,
                fade_in_range: None,
                fade_out_range: None,
                fade_in_shape: super::types::FadeShape::SCurve,
                fade_out_shape: super::types::FadeShape::SCurve,
                loop_mode: super::types::LoopMode::Off,
                bpm_enabled: false,
                bpm_value: self
                    .meta_for_path(path)
                    .and_then(|meta| meta.bpm)
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .unwrap_or(0.0),
                bpm_user_set: false,
                bpm_offset_sec: 0.0,
                snap_zero_cross: true,
                tool_state: default_tool_state(),
                active_tool: ToolKind::LoopEdit,
                plugin_fx_draft: super::types::PluginFxDraft::default(),
                show_waveform_overlay: true,
                applied_effect_graph: template_stamp.clone(),
            }
        };
        cached.ch_samples = channels;
        cached.samples_len = new_len;
        cached.waveform_minmax = waveform;
        cached.display_meta = display_meta;
        cached.dirty = true;
        cached.applied_effect_graph = template_stamp;
        self.edited_cache.insert(path.to_path_buf(), cached);
        self.evict_list_preview_cache_path(path);
        self.lufs_override.remove(path);
        self.push_effect_graph_console(
            EffectGraphSeverity::Info,
            "apply",
            format!("applied graph to {}", path.display()),
            None,
        );
    }

    pub(super) fn drain_effect_graph_runner(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.effect_graph.runner.rx.take() else {
            return;
        };
        let mut keep_rx = true;
        loop {
            match rx.try_recv() {
                Ok(event) => match event {
                    EffectGraphWorkerEvent::RunStarted { mode, total } => {
                        self.effect_graph.runner.mode = Some(mode);
                        self.effect_graph.runner.total = total;
                        self.effect_graph.runner.done = 0;
                        if mode == EffectGraphRunMode::TestPreview {
                            self.effect_graph.debug_previews.clear();
                        }
                    }
                    EffectGraphWorkerEvent::PathStarted { path, index, total } => {
                        self.effect_graph.runner.current_path = Some(path.clone());
                        for status in self.effect_graph.runner.node_status.values_mut() {
                            status.phase = EffectGraphNodeRunPhase::Idle;
                            status.elapsed_ms = None;
                            status.error = None;
                        }
                        self.push_effect_graph_console(
                            EffectGraphSeverity::Info,
                            "run",
                            format!("start {}/{} {}", index, total, path.display()),
                            None,
                        );
                    }
                    EffectGraphWorkerEvent::NodeStarted { node_id } => {
                        let status = self
                            .effect_graph
                            .runner
                            .node_status
                            .entry(node_id.clone())
                            .or_default();
                        status.phase = EffectGraphNodeRunPhase::Running;
                        status.error = None;
                    }
                    EffectGraphWorkerEvent::NodeFinished {
                        node_id,
                        elapsed_ms,
                    } => {
                        let status = self
                            .effect_graph
                            .runner
                            .node_status
                            .entry(node_id.clone())
                            .or_default();
                        status.phase = EffectGraphNodeRunPhase::Success;
                        status.elapsed_ms = Some(elapsed_ms);
                        self.push_effect_graph_console(
                            EffectGraphSeverity::Info,
                            "node",
                            format!("{node_id}: {elapsed_ms:.1} ms"),
                            Some(node_id),
                        );
                    }
                    EffectGraphWorkerEvent::NodeLog {
                        node_id,
                        severity,
                        message,
                    } => {
                        self.push_effect_graph_console(
                            severity,
                            "node",
                            format!("{node_id}: {message}"),
                            Some(node_id),
                        );
                    }
                    EffectGraphWorkerEvent::NodeDebugPreview { node_id, preview } => {
                        self.effect_graph
                            .debug_previews
                            .insert(node_id, Arc::new(preview));
                    }
                    EffectGraphWorkerEvent::PathFinished {
                        path,
                        output_bus,
                        monitor_audio,
                        total_elapsed_ms,
                    } => {
                        self.effect_graph.runner.done =
                            self.effect_graph.runner.done.saturating_add(1);
                        match self.effect_graph.runner.mode {
                            Some(EffectGraphRunMode::TestPreview) => {
                                self.effect_graph.tester.last_output_bus = Some(output_bus.clone());
                                self.effect_graph.tester.last_output_audio =
                                    Some(Arc::new(AudioBuffer::from_channels(monitor_audio)));
                                self.effect_graph.tester.last_run_ms = Some(total_elapsed_ms);
                                self.effect_graph.tester.last_output_summary = format!(
                                    "{} ch / {:.2}s @ {} Hz",
                                    output_bus.channels.len().max(1),
                                    output_bus
                                        .channels
                                        .get(0)
                                        .map(|channel| channel.len() as f32
                                            / output_bus.sample_rate.max(1) as f32)
                                        .unwrap_or(0.0),
                                    output_bus.sample_rate.max(1),
                                );
                                self.push_effect_graph_console(
                                    EffectGraphSeverity::Info,
                                    "test",
                                    format!(
                                        "test finished: {} ({total_elapsed_ms:.1} ms)",
                                        path.display()
                                    ),
                                    None,
                                );
                            }
                            Some(EffectGraphRunMode::ApplyToListSelection) => {
                                self.apply_effect_graph_result_to_path(
                                    &path,
                                    output_bus.channels,
                                    output_bus.sample_rate,
                                );
                            }
                            None => {}
                        }
                    }
                    EffectGraphWorkerEvent::Failed {
                        path,
                        node_id,
                        message,
                    } => {
                        if let Some(node_id) = node_id.clone() {
                            let status = self
                                .effect_graph
                                .runner
                                .node_status
                                .entry(node_id.clone())
                                .or_default();
                            status.phase = EffectGraphNodeRunPhase::Failed;
                            status.error = Some(message.clone());
                        }
                        if self.effect_graph.runner.mode == Some(EffectGraphRunMode::TestPreview) {
                            self.effect_graph.tester.last_error = Some(message.clone());
                        }
                        let scope = if self.effect_graph.runner.mode
                            == Some(EffectGraphRunMode::TestPreview)
                        {
                            "test"
                        } else {
                            "apply"
                        };
                        let prefix = path
                            .map(|value| value.display().to_string())
                            .unwrap_or_else(|| "effect graph".to_string());
                        self.push_effect_graph_console(
                            EffectGraphSeverity::Error,
                            scope,
                            format!("{prefix}: {message}"),
                            node_id,
                        );
                    }
                    EffectGraphWorkerEvent::Finished => {
                        keep_rx = false;
                        break;
                    }
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    keep_rx = false;
                    break;
                }
            }
        }
        if keep_rx {
            self.effect_graph.runner.rx = Some(rx);
            ctx.request_repaint();
        } else {
            self.effect_graph.runner.rx = None;
            self.effect_graph.runner.cancel_requested = None;
            self.effect_graph.runner.mode = None;
            self.effect_graph.runner.started_at = None;
            self.effect_graph.runner.current_path = None;
            self.effect_graph.runner.template_stamp = None;
        }
    }

    pub(super) fn effect_graph_add_node(
        &mut self,
        kind: EffectGraphNodeKind,
        world_pos: [f32; 2],
    ) -> Option<String> {
        if matches!(kind, EffectGraphNodeKind::Input)
            && self
                .effect_graph
                .draft
                .nodes
                .iter()
                .any(|node| matches!(node.data, EffectGraphNodeData::Input))
        {
            return None;
        }
        if matches!(kind, EffectGraphNodeKind::Output)
            && self
                .effect_graph
                .draft
                .nodes
                .iter()
                .any(|node| matches!(node.data, EffectGraphNodeData::Output))
        {
            return None;
        }
        let mut next_id = 1usize;
        loop {
            let id = format!("{}_{}", node_label(kind).to_ascii_lowercase(), next_id);
            if !self
                .effect_graph
                .draft
                .nodes
                .iter()
                .any(|node| node.id == id)
            {
                self.effect_graph_push_undo_snapshot();
                self.effect_graph.draft.nodes.push(EffectGraphNode {
                    id: id.clone(),
                    ui_pos: world_pos,
                    ui_size: effect_graph_default_node_size(kind),
                    data: EffectGraphNodeData::default_for_kind(kind),
                });
                self.effect_graph.draft_dirty = true;
                self.revalidate_effect_graph_draft();
                return Some(id);
            }
            next_id = next_id.saturating_add(1);
        }
    }

    pub(super) fn effect_graph_can_copy_selection(&self) -> bool {
        self.effect_graph.draft.nodes.iter().any(|node| {
            self.effect_graph.canvas.selected_nodes.contains(&node.id)
                && effect_graph_node_is_copyable(&node.data)
        })
    }

    pub(super) fn effect_graph_clipboard_text_is_supported(&self, text: &str) -> bool {
        effect_graph_clipboard_payload_from_text(text).is_some()
    }

    pub(super) fn effect_graph_copy_selection_to_clipboard(&mut self, ctx: &egui::Context) -> bool {
        let Some(payload) = effect_graph_build_clipboard_payload(
            &self.effect_graph.draft,
            &self.effect_graph.canvas.selected_nodes,
        ) else {
            self.push_effect_graph_console(
                EffectGraphSeverity::Warning,
                "clipboard",
                "Nothing copyable selected. Input/Output are not duplicated.".to_string(),
                None,
            );
            return false;
        };
        match effect_graph_clipboard_payload_to_text(&payload) {
            Ok(text) => {
                ctx.copy_text(text);
                self.effect_graph.clipboard_paste_serial = 0;
                self.push_effect_graph_console(
                    EffectGraphSeverity::Info,
                    "clipboard",
                    format!(
                        "copied {} node(s) and {} edge(s)",
                        payload.nodes.len(),
                        payload.edges.len()
                    ),
                    None,
                );
                true
            }
            Err(err) => {
                self.push_effect_graph_console(
                    EffectGraphSeverity::Error,
                    "clipboard",
                    format!("copy failed: {err}"),
                    None,
                );
                false
            }
        }
    }

    pub(super) fn effect_graph_paste_from_clipboard_text(
        &mut self,
        text: &str,
    ) -> Result<usize, String> {
        let Some(payload) = effect_graph_clipboard_payload_from_text(text) else {
            return Err("Clipboard does not contain Effect Graph nodes".to_string());
        };
        let EffectGraphClipboardPayload {
            origin,
            nodes,
            edges,
            ..
        } = payload;
        let mut copied_nodes = nodes
            .into_iter()
            .filter(|node| effect_graph_node_is_copyable(&node.data))
            .collect::<Vec<_>>();
        if copied_nodes.is_empty() {
            return Err("Clipboard selection is empty".to_string());
        }
        let mut copied_edges = edges;
        let copied_ids = copied_nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<HashSet<_>>();
        copied_edges.retain(|edge| {
            copied_ids.contains(&edge.from_node_id) && copied_ids.contains(&edge.to_node_id)
        });
        let paste_origin =
            if let Some(pointer_world) = self.effect_graph.canvas.last_canvas_pointer_world {
                pointer_world
            } else {
                self.effect_graph.clipboard_paste_serial =
                    self.effect_graph.clipboard_paste_serial.saturating_add(1);
                let offset = 36.0 * self.effect_graph.clipboard_paste_serial as f32;
                [origin[0] + offset, origin[1] + offset]
            };

        self.effect_graph_push_undo_snapshot();

        let mut used_node_ids = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .map(|node| node.id.clone())
            .collect::<HashSet<_>>();
        let mut id_map = HashMap::<String, String>::new();
        let mut new_node_ids = HashSet::<String>::new();
        let mut new_nodes = Vec::with_capacity(copied_nodes.len());
        for mut node in copied_nodes.drain(..) {
            let new_id = effect_graph_unique_id(&mut used_node_ids, &node.id);
            id_map.insert(node.id.clone(), new_id.clone());
            node.id = new_id.clone();
            node.ui_pos = [
                paste_origin[0] + node.ui_pos[0],
                paste_origin[1] + node.ui_pos[1],
            ];
            clamp_node_data(&mut node.data);
            new_node_ids.insert(new_id);
            new_nodes.push(node);
        }
        let focus_node_id = new_nodes.first().map(|node| node.id.clone());

        let mut used_edge_ids = self
            .effect_graph
            .draft
            .edges
            .iter()
            .map(|edge| edge.id.clone())
            .collect::<HashSet<_>>();
        let mut new_edges = Vec::with_capacity(copied_edges.len());
        for edge in copied_edges.into_iter() {
            let Some(from_node_id) = id_map.get(&edge.from_node_id) else {
                continue;
            };
            let Some(to_node_id) = id_map.get(&edge.to_node_id) else {
                continue;
            };
            let edge_id = effect_graph_unique_id(
                &mut used_edge_ids,
                &format!(
                    "edge_{}_{}_{}_{}",
                    from_node_id, edge.from_port_id, to_node_id, edge.to_port_id
                ),
            );
            new_edges.push(EffectGraphEdge {
                id: edge_id,
                from_node_id: from_node_id.clone(),
                from_port_id: edge.from_port_id,
                to_node_id: to_node_id.clone(),
                to_port_id: edge.to_port_id,
            });
        }

        self.effect_graph.draft.nodes.extend(new_nodes);
        self.effect_graph.draft.edges.extend(new_edges);
        self.effect_graph.canvas.selected_nodes = new_node_ids.clone();
        self.effect_graph.canvas.selected_edge_id = None;
        self.effect_graph.canvas.focus_node_id = focus_node_id;
        self.effect_graph.draft_dirty = true;
        self.revalidate_effect_graph_draft();
        self.push_effect_graph_console(
            EffectGraphSeverity::Info,
            "clipboard",
            format!("pasted {} node(s)", new_node_ids.len()),
            None,
        );
        Ok(new_node_ids.len())
    }

    pub(super) fn effect_graph_remove_selected_items(&mut self) {
        let selected_nodes = self.effect_graph.canvas.selected_nodes.clone();
        let selected_edge = self.effect_graph.canvas.selected_edge_id.clone();
        if !selected_nodes.is_empty() {
            self.effect_graph_push_undo_snapshot();
            self.effect_graph
                .draft
                .nodes
                .retain(|node| !selected_nodes.contains(&node.id));
            self.effect_graph.draft.edges.retain(|edge| {
                !selected_nodes.contains(&edge.from_node_id)
                    && !selected_nodes.contains(&edge.to_node_id)
            });
            for node_id in selected_nodes.iter() {
                self.effect_graph.debug_previews.remove(node_id);
            }
            self.effect_graph.canvas.selected_nodes.clear();
            self.effect_graph.canvas.selected_edge_id = None;
            self.effect_graph.draft_dirty = true;
            self.revalidate_effect_graph_draft();
            return;
        }
        if let Some(edge_id) = selected_edge {
            self.effect_graph_push_undo_snapshot();
            self.effect_graph
                .draft
                .edges
                .retain(|edge| edge.id != edge_id);
            self.effect_graph.canvas.selected_edge_id = None;
            self.effect_graph.draft_dirty = true;
            self.revalidate_effect_graph_draft();
        }
    }

    pub(super) fn effect_graph_connect_nodes(
        &mut self,
        from_node_id: &str,
        from_port_id: &str,
        to_node_id: &str,
        to_port_id: &str,
    ) -> Result<(), String> {
        if from_node_id == to_node_id {
            return Err("cannot connect a node to itself".to_string());
        }
        let Some(from_node) = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .find(|node| node.id == from_node_id)
        else {
            return Err("source node not found".to_string());
        };
        let Some(to_node) = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .find(|node| node.id == to_node_id)
        else {
            return Err("target node not found".to_string());
        };
        if !from_node.data.has_output_port(from_port_id) {
            return Err(format!(
                "{} has no output port '{}'",
                from_node.data.display_name(),
                from_port_id
            ));
        }
        if !to_node.data.has_input_port(to_port_id) {
            return Err(format!(
                "{} has no input port '{}'",
                to_node.data.display_name(),
                to_port_id
            ));
        }
        self.effect_graph_push_undo_snapshot();
        self.effect_graph.draft.edges.retain(|edge| {
            !(edge.from_node_id == from_node_id && edge.from_port_id == from_port_id)
                && !(edge.to_node_id == to_node_id && edge.to_port_id == to_port_id)
        });
        self.effect_graph.draft.edges.push(EffectGraphEdge {
            id: format!(
                "edge_{}_{}_{}_{}",
                from_node_id, from_port_id, to_node_id, to_port_id
            ),
            from_node_id: from_node_id.to_string(),
            from_port_id: from_port_id.to_string(),
            to_node_id: to_node_id.to_string(),
            to_port_id: to_port_id.to_string(),
        });
        self.effect_graph.draft_dirty = true;
        self.revalidate_effect_graph_draft();
        Ok(())
    }

    pub(super) fn effect_graph_tidy_layout(&mut self) -> bool {
        if self.effect_graph.draft.nodes.is_empty() {
            return false;
        }
        self.effect_graph_push_undo_snapshot();
        ensure_effect_graph_node_layout(&mut self.effect_graph.draft);

        let (_, incoming, outgoing) = build_graph_maps(&self.effect_graph.draft);
        let (input_sources, _) = build_port_edge_maps(&self.effect_graph.draft);
        let flow_hints = effect_graph_infer_flow_hints(&self.effect_graph.draft);
        let topo_order = effect_graph_topological_order(&self.effect_graph.draft);
        let input_id = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .find(|node| matches!(node.data, EffectGraphNodeData::Input))
            .map(|node| node.id.clone());

        let mut reachable = HashSet::new();
        if let Some(root) = input_id.clone() {
            let mut stack = vec![root];
            while let Some(node_id) = stack.pop() {
                if !reachable.insert(node_id.clone()) {
                    continue;
                }
                if let Some(nexts) = outgoing.get(&node_id) {
                    stack.extend(nexts.iter().cloned());
                }
            }
        }

        let mut depth = HashMap::<String, usize>::new();
        if let Some(root) = input_id {
            depth.insert(root, 0);
        }
        for node_id in topo_order.iter() {
            let current_depth = if let Some(value) = depth.get(node_id).copied() {
                value
            } else if incoming
                .get(node_id)
                .map(|items| items.is_empty())
                .unwrap_or(true)
            {
                0
            } else {
                incoming
                    .get(node_id)
                    .into_iter()
                    .flatten()
                    .filter_map(|parent| depth.get(parent).copied())
                    .max()
                    .map(|value| value.saturating_add(1))
                    .unwrap_or(0)
            };
            depth.entry(node_id.clone()).or_insert(current_depth);
            if let Some(nexts) = outgoing.get(node_id) {
                for next in nexts.iter() {
                    let next_depth = current_depth.saturating_add(1);
                    depth
                        .entry(next.clone())
                        .and_modify(|value| *value = (*value).max(next_depth))
                        .or_insert(next_depth);
                }
            }
        }

        let mut next_disconnected_depth = depth.values().copied().max().unwrap_or(0) + 1;
        for node_id in topo_order.iter() {
            if !reachable.is_empty() && !reachable.contains(node_id) {
                depth.insert(node_id.clone(), next_disconnected_depth);
                next_disconnected_depth = next_disconnected_depth.saturating_add(1);
            }
        }

        let node_map = self
            .effect_graph
            .draft
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect::<HashMap<_, _>>();
        let mut columns = HashMap::<usize, Vec<String>>::new();
        for node_id in topo_order.iter() {
            let column = depth.get(node_id).copied().unwrap_or(0);
            columns.entry(column).or_default().push(node_id.clone());
        }

        let mut ordered_columns = columns.into_iter().collect::<Vec<_>>();
        ordered_columns.sort_by_key(|(column, _)| *column);

        let mut x = 80.0f32;
        let column_gap = 120.0f32;
        let row_gap = 72.0f32;
        let top = 92.0f32;
        for (_, node_ids) in ordered_columns.iter_mut() {
            node_ids.sort_by(|left, right| {
                let left_node = node_map.get(left);
                let right_node = node_map.get(right);
                let left_key = left_node
                    .map(|node| {
                        (
                            effect_graph_node_lane_hint(node, &input_sources, &flow_hints)
                                .map(|value| (value * 100.0) as i32)
                                .unwrap_or(i32::MAX),
                            effect_graph_layout_priority(&node.data),
                            node.ui_pos[1] as i32,
                            node.ui_pos[0] as i32,
                        )
                    })
                    .unwrap_or((i32::MAX, 999, 0, 0));
                let right_key = right_node
                    .map(|node| {
                        (
                            effect_graph_node_lane_hint(node, &input_sources, &flow_hints)
                                .map(|value| (value * 100.0) as i32)
                                .unwrap_or(i32::MAX),
                            effect_graph_layout_priority(&node.data),
                            node.ui_pos[1] as i32,
                            node.ui_pos[0] as i32,
                        )
                    })
                    .unwrap_or((i32::MAX, 999, 0, 0));
                left_key.cmp(&right_key).then_with(|| left.cmp(right))
            });

            let max_width = node_ids
                .iter()
                .filter_map(|node_id| node_map.get(node_id))
                .map(|node| node.ui_size[0])
                .fold(220.0f32, f32::max);

            let mut y = top;
            for node_id in node_ids.iter() {
                if let Some(node_mut) = self
                    .effect_graph
                    .draft
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == *node_id)
                {
                    node_mut.ui_pos = [x, y];
                    y += node_mut.ui_size[1] + row_gap;
                }
            }
            x += max_width + column_gap;
        }

        self.effect_graph.draft_dirty = true;
        self.revalidate_effect_graph_draft();
        true
    }

    pub(super) fn effect_graph_flow_hints(
        &self,
    ) -> HashMap<EffectGraphPortKey, EffectGraphChannelFlowHint> {
        effect_graph_infer_flow_hints(&self.effect_graph.draft)
    }

    pub(super) fn effect_graph_input_sources(
        &self,
    ) -> HashMap<EffectGraphPortKey, EffectGraphPortKey> {
        build_port_edge_maps(&self.effect_graph.draft).0
    }

    pub(super) fn effect_graph_combine_mode_hint(
        &self,
        node: &EffectGraphNode,
        input_sources: &HashMap<EffectGraphPortKey, EffectGraphPortKey>,
        flow_hints: &HashMap<EffectGraphPortKey, EffectGraphChannelFlowHint>,
    ) -> Option<EffectGraphCombineMode> {
        effect_graph_combine_mode_for_node_from_maps(node, input_sources, flow_hints)
    }

    pub(super) fn effect_graph_combine_slot_labels(
        &self,
        node: &EffectGraphNode,
        input_sources: &HashMap<EffectGraphPortKey, EffectGraphPortKey>,
        flow_hints: &HashMap<EffectGraphPortKey, EffectGraphChannelFlowHint>,
    ) -> HashMap<String, usize> {
        effect_graph_combine_slot_labels_for_node(node, input_sources, flow_hints)
    }

    pub(super) fn effect_graph_combine_display_labels(
        &self,
        node: &EffectGraphNode,
        input_sources: &HashMap<EffectGraphPortKey, EffectGraphPortKey>,
        flow_hints: &HashMap<EffectGraphPortKey, EffectGraphChannelFlowHint>,
    ) -> HashMap<String, String> {
        effect_graph_combine_display_labels_for_node(node, input_sources, flow_hints)
    }

    pub(super) fn effect_graph_node_parameter_summary(data: &EffectGraphNodeData) -> String {
        node_parameter_summary(data)
    }

    pub(super) fn effect_graph_channel_label(channel_index: usize) -> String {
        effect_graph_channel_label(channel_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with_nodes(
        nodes: Vec<EffectGraphNode>,
        edges: Vec<EffectGraphEdge>,
    ) -> EffectGraphDocument {
        EffectGraphDocument {
            schema_version: EFFECT_GRAPH_SCHEMA_VERSION,
            name: "Test".to_string(),
            nodes,
            edges,
            canvas: Default::default(),
        }
    }

    fn edge(
        id: &str,
        from_node_id: &str,
        from_port_id: &str,
        to_node_id: &str,
        to_port_id: &str,
    ) -> EffectGraphEdge {
        EffectGraphEdge {
            id: id.to_string(),
            from_node_id: from_node_id.to_string(),
            from_port_id: from_port_id.to_string(),
            to_node_id: to_node_id.to_string(),
            to_port_id: to_port_id.to_string(),
        }
    }

    fn test_bus(channels: Vec<Vec<f32>>, sample_rate: u32) -> EffectGraphAudioBus {
        dense_audio_bus(channels, sample_rate)
    }

    #[test]
    fn effect_graph_clipboard_payload_copies_selected_internal_fragment() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "gain".to_string(),
                    ui_pos: [120.0, 60.0],
                    ui_size: [280.0, 182.0],
                    data: EffectGraphNodeData::Gain { gain_db: 3.0 },
                },
                EffectGraphNode {
                    id: "pitch".to_string(),
                    ui_pos: [280.0, 90.0],
                    ui_size: [280.0, 182.0],
                    data: EffectGraphNodeData::PitchShift { semitones: 2.0 },
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [480.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "gain", "in"),
                edge("b", "gain", "out", "pitch", "in"),
                edge("c", "pitch", "out", "output", "in"),
            ],
        );
        let selected = ["input", "gain", "pitch", "output"]
            .into_iter()
            .map(str::to_string)
            .collect::<HashSet<_>>();
        let payload = effect_graph_build_clipboard_payload(&doc, &selected)
            .expect("clipboard payload should exist");
        assert_eq!(payload.origin, [120.0, 60.0]);
        assert_eq!(payload.nodes.len(), 2);
        assert_eq!(payload.edges.len(), 1);
        assert_eq!(payload.nodes[0].id, "gain");
        assert_eq!(payload.nodes[0].ui_pos, [0.0, 0.0]);
        assert_eq!(payload.nodes[1].id, "pitch");
        assert_eq!(payload.nodes[1].ui_pos, [160.0, 30.0]);
        assert_eq!(payload.edges[0].from_node_id, "gain");
        assert_eq!(payload.edges[0].to_node_id, "pitch");
    }

    #[test]
    fn effect_graph_clipboard_text_roundtrips_payload() {
        let payload = EffectGraphClipboardPayload {
            version: EFFECT_GRAPH_CLIPBOARD_VERSION,
            origin: [32.0, 48.0],
            nodes: vec![EffectGraphNode {
                id: "gain".to_string(),
                ui_pos: [0.0, 0.0],
                ui_size: [280.0, 182.0],
                data: EffectGraphNodeData::Gain { gain_db: 6.0 },
            }],
            edges: Vec::new(),
        };
        let text = effect_graph_clipboard_payload_to_text(&payload).expect("serialize payload");
        let parsed = effect_graph_clipboard_payload_from_text(&text).expect("parse payload");
        assert_eq!(parsed, payload);
    }

    #[test]
    fn effect_graph_validation_rejects_missing_output() {
        let doc = doc_with_nodes(
            vec![EffectGraphNode {
                id: "input".to_string(),
                ui_pos: [0.0, 0.0],
                ui_size: [200.0, 100.0],
                data: EffectGraphNodeData::Input,
            }],
            vec![],
        );
        let issues = validate_effect_graph_document(&doc);
        assert!(issues
            .iter()
            .any(|issue| issue.code == "output_count"
                && issue.severity == EffectGraphSeverity::Error));
    }

    #[test]
    fn effect_graph_runtime_gain_changes_peak() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [200.0, 100.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "gain".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [200.0, 100.0],
                    data: EffectGraphNodeData::Gain { gain_db: 6.0 },
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [200.0, 0.0],
                    ui_size: [200.0, 100.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "gain", "in"),
                edge("b", "gain", "out", "output", "in"),
            ],
        );
        let out = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.25, -0.25]], 48_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |_| {},
        )
        .expect("runtime ok");
        assert!(out.channels[0][0] > 0.45);
    }

    #[test]
    fn effect_graph_mono_mix_downmixes_while_ignoring_selected_channels() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "mono".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [280.0, 182.0],
                    data: EffectGraphNodeData::MonoMix {
                        ignored_channels: vec![
                            false, false, false, true, false, false, false, false,
                        ],
                    },
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [220.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "mono", "in"),
                edge("b", "mono", "out", "output", "in"),
            ],
        );
        let out = run_effect_graph_document(
            &doc,
            test_bus(
                vec![
                    vec![1.0, 1.0],
                    vec![3.0, 3.0],
                    vec![5.0, 5.0],
                    vec![10.0, 10.0],
                ],
                48_000,
            ),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |_| {},
        )
        .expect("runtime ok");
        assert_eq!(out.channels.len(), 1);
        assert_eq!(out.channels[0], vec![3.0, 3.0]);
        assert_eq!(out.sample_rate, 48_000);
    }

    #[test]
    fn effect_graph_runtime_speed_changes_length() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [200.0, 100.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "speed".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [200.0, 100.0],
                    data: EffectGraphNodeData::Speed { rate: 2.0 },
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [200.0, 0.0],
                    ui_size: [200.0, 100.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "speed", "in"),
                edge("b", "speed", "out", "output", "in"),
            ],
        );
        let out = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.0, 0.2, 0.4, 0.6, 0.8, 1.0]], 48_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |_| {},
        )
        .expect("runtime ok");
        assert!(out.channels[0].len() < 6);
    }

    #[test]
    fn effect_graph_embedded_sample_decodes() {
        let (channels, sr) = embedded_effect_graph_sample_channels().expect("sample available");
        assert_eq!(sr, 48_000);
        assert_eq!(channels.len(), 1);
        assert!(channels[0].len() >= 48_000 * 9);
    }

    #[test]
    fn effect_graph_debug_nodes_emit_only_for_test_preview() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "debug".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [320.0, 220.0],
                    data: EffectGraphNodeData::DebugWaveform { zoom: 1.0 },
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [200.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "debug", "in"),
                edge("b", "debug", "out", "output", "in"),
            ],
        );
        let mut test_previews = 0usize;
        let _ = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.1, 0.2, 0.3]], 48_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |event| {
                if matches!(event, EffectGraphRuntimeEvent::NodeDebugPreview { .. }) {
                    test_previews += 1;
                }
            },
        )
        .expect("test preview runtime ok");
        let mut apply_previews = 0usize;
        let _ = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.1, 0.2, 0.3]], 48_000),
            EffectGraphRunMode::ApplyToListSelection,
            crate::wave::ResampleQuality::Good,
            |event| {
                if matches!(event, EffectGraphRuntimeEvent::NodeDebugPreview { .. }) {
                    apply_previews += 1;
                }
            },
        )
        .expect("apply runtime ok");
        assert_eq!(test_previews, 1);
        assert_eq!(apply_previews, 0);
    }

    #[test]
    fn effect_graph_split_channel_emits_mono_output() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "split".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [260.0, 220.0],
                    data: EffectGraphNodeData::SplitChannels,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [200.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "split", "in"),
                edge("b", "split", "ch2", "output", "in"),
            ],
        );
        let out = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.1, 0.2], vec![0.9, 0.8]], 48_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |_| {},
        )
        .expect("runtime ok");
        assert_eq!(out.channels.len(), 1);
        assert_eq!(out.channels[0], vec![0.9, 0.8]);
    }

    #[test]
    fn effect_graph_duplicate_branches_same_bus_to_two_outputs() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "dup".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [250.0, 152.0],
                    data: EffectGraphNodeData::Duplicate,
                },
                EffectGraphNode {
                    id: "combine".to_string(),
                    ui_pos: [220.0, 0.0],
                    ui_size: [300.0, 250.0],
                    data: EffectGraphNodeData::CombineChannels,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [340.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "dup", "in"),
                edge("b", "dup", "out1", "combine", "in1"),
                edge("c", "dup", "out2", "combine", "in2"),
                edge("d", "combine", "out", "output", "in"),
            ],
        );
        let out = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.1, 0.2], vec![0.9, 0.8]], 48_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |_| {},
        )
        .expect("runtime ok");
        assert_eq!(out.channels.len(), 4);
        assert_eq!(out.channels[0], vec![0.1, 0.2]);
        assert_eq!(out.channels[1], vec![0.9, 0.8]);
        assert_eq!(out.channels[2], vec![0.1, 0.2]);
        assert_eq!(out.channels[3], vec![0.9, 0.8]);
    }

    #[test]
    fn effect_graph_split_then_combine_restores_channel_layout() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "split".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [260.0, 220.0],
                    data: EffectGraphNodeData::SplitChannels,
                },
                EffectGraphNode {
                    id: "combine".to_string(),
                    ui_pos: [220.0, 0.0],
                    ui_size: [300.0, 250.0],
                    data: EffectGraphNodeData::CombineChannels,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [340.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "split", "in"),
                edge("b", "split", "ch1", "combine", "in1"),
                edge("c", "split", "ch2", "combine", "in2"),
                edge("d", "combine", "out", "output", "in"),
            ],
        );
        let out = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.1, 0.2, 0.3], vec![0.9, 0.8, 0.7]], 96_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |_| {},
        )
        .expect("runtime ok");
        assert_eq!(out.sample_rate, 96_000);
        assert_eq!(out.channels.len(), 2);
        assert_eq!(out.channels[0], vec![0.1, 0.2, 0.3]);
        assert_eq!(out.channels[1], vec![0.9, 0.8, 0.7]);
    }

    #[test]
    fn effect_graph_combine_mixed_layout_uses_adaptive_mode() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "split".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [260.0, 220.0],
                    data: EffectGraphNodeData::SplitChannels,
                },
                EffectGraphNode {
                    id: "combine".to_string(),
                    ui_pos: [220.0, 0.0],
                    ui_size: [300.0, 250.0],
                    data: EffectGraphNodeData::CombineChannels,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [340.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "combine", "in1"),
                edge("b", "input", "out", "split", "in"),
                edge("c", "split", "ch1", "combine", "in2"),
                edge("d", "combine", "out", "output", "in"),
            ],
        );
        let issues = validate_effect_graph_document(&doc);
        assert!(!issues
            .iter()
            .any(|issue| issue.code == "combine_mixed_layout"));
    }

    #[test]
    fn effect_graph_combine_duplicate_slot_warns_and_allows_restore_mix() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "split".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [260.0, 220.0],
                    data: EffectGraphNodeData::SplitChannels,
                },
                EffectGraphNode {
                    id: "combine".to_string(),
                    ui_pos: [220.0, 0.0],
                    ui_size: [300.0, 250.0],
                    data: EffectGraphNodeData::CombineChannels,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [340.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "split", "in"),
                edge("b", "split", "ch1", "combine", "in1"),
                edge("c", "split", "ch1", "combine", "in2"),
                edge("d", "combine", "out", "output", "in"),
            ],
        );
        let issues = validate_effect_graph_document(&doc);
        assert!(issues
            .iter()
            .any(|issue| issue.code == "combine_duplicate_slot"
                && issue.severity == EffectGraphSeverity::Warning));
    }

    #[test]
    fn effect_graph_combine_duplicate_slot_mixes_restore_inputs() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "split".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [260.0, 220.0],
                    data: EffectGraphNodeData::SplitChannels,
                },
                EffectGraphNode {
                    id: "combine".to_string(),
                    ui_pos: [220.0, 0.0],
                    ui_size: [300.0, 250.0],
                    data: EffectGraphNodeData::CombineChannels,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [340.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "split", "in"),
                edge("b", "split", "ch1", "combine", "in1"),
                edge("c", "split", "ch1", "combine", "in2"),
                edge("d", "split", "ch2", "combine", "in3"),
                edge("e", "combine", "out", "output", "in"),
            ],
        );
        let mut warnings = Vec::new();
        let out = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.1, 0.2], vec![0.9, 0.8]], 48_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |event| {
                if let EffectGraphRuntimeEvent::NodeLog {
                    severity: EffectGraphSeverity::Warning,
                    message,
                    ..
                } = event
                {
                    warnings.push(message);
                }
            },
        )
        .expect("runtime ok");
        assert_eq!(out.channels.len(), 2);
        assert_eq!(out.channels[0], vec![0.2, 0.4]);
        assert_eq!(out.channels[1], vec![0.9, 0.8]);
        assert!(warnings
            .iter()
            .any(|message| message.contains("slot 1 mixed from 2 inputs")));
    }

    #[test]
    fn effect_graph_adaptive_combine_widens_duplicate_mono_branch() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "split".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [260.0, 220.0],
                    data: EffectGraphNodeData::SplitChannels,
                },
                EffectGraphNode {
                    id: "dup".to_string(),
                    ui_pos: [180.0, 0.0],
                    ui_size: [250.0, 152.0],
                    data: EffectGraphNodeData::Duplicate,
                },
                EffectGraphNode {
                    id: "combine".to_string(),
                    ui_pos: [320.0, 0.0],
                    ui_size: [300.0, 250.0],
                    data: EffectGraphNodeData::CombineChannels,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [460.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "split", "in"),
                edge("b", "split", "ch1", "dup", "in"),
                edge("c", "dup", "out1", "combine", "in1"),
                edge("d", "dup", "out2", "combine", "in2"),
                edge("e", "combine", "out", "output", "in"),
            ],
        );
        let out = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.1, 0.2]], 48_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |_| {},
        )
        .expect("runtime ok");
        assert_eq!(out.channels.len(), 2);
        assert_eq!(out.channels[0], vec![0.1, 0.2]);
        assert_eq!(out.channels[1], vec![0.1, 0.2]);
    }

    #[test]
    fn effect_graph_adaptive_combine_keeps_untouched_slots() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "split".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [260.0, 220.0],
                    data: EffectGraphNodeData::SplitChannels,
                },
                EffectGraphNode {
                    id: "dup".to_string(),
                    ui_pos: [180.0, 0.0],
                    ui_size: [250.0, 152.0],
                    data: EffectGraphNodeData::Duplicate,
                },
                EffectGraphNode {
                    id: "combine".to_string(),
                    ui_pos: [320.0, 0.0],
                    ui_size: [300.0, 250.0],
                    data: EffectGraphNodeData::CombineChannels,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [460.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "split", "in"),
                edge("b", "split", "ch1", "dup", "in"),
                edge("c", "dup", "out1", "combine", "in1"),
                edge("d", "dup", "out2", "combine", "in2"),
                edge("e", "split", "ch2", "combine", "in3"),
                edge("f", "combine", "out", "output", "in"),
            ],
        );
        let out = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.1, 0.2], vec![0.9, 0.8]], 48_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |_| {},
        )
        .expect("runtime ok");
        assert_eq!(out.channels.len(), 3);
        assert_eq!(out.channels[0], vec![0.1, 0.2]);
        assert_eq!(out.channels[1], vec![0.1, 0.2]);
        assert_eq!(out.channels[2], vec![0.9, 0.8]);
    }

    #[test]
    fn effect_graph_restore_keeps_declared_width_for_vacant_split_outputs() {
        let doc = doc_with_nodes(
            vec![
                EffectGraphNode {
                    id: "input".to_string(),
                    ui_pos: [0.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Input,
                },
                EffectGraphNode {
                    id: "split".to_string(),
                    ui_pos: [100.0, 0.0],
                    ui_size: [260.0, 220.0],
                    data: EffectGraphNodeData::SplitChannels,
                },
                EffectGraphNode {
                    id: "combine".to_string(),
                    ui_pos: [220.0, 0.0],
                    ui_size: [300.0, 250.0],
                    data: EffectGraphNodeData::CombineChannels,
                },
                EffectGraphNode {
                    id: "output".to_string(),
                    ui_pos: [340.0, 0.0],
                    ui_size: [260.0, 136.0],
                    data: EffectGraphNodeData::Output,
                },
            ],
            vec![
                edge("a", "input", "out", "split", "in"),
                edge("b", "split", "ch8", "combine", "in8"),
                edge("c", "combine", "out", "output", "in"),
            ],
        );
        let mut warnings = Vec::new();
        let out = run_effect_graph_document(
            &doc,
            test_bus(vec![vec![0.1, 0.2], vec![0.9, 0.8]], 48_000),
            EffectGraphRunMode::TestPreview,
            crate::wave::ResampleQuality::Good,
            |event| {
                if let EffectGraphRuntimeEvent::NodeLog {
                    severity: EffectGraphSeverity::Warning,
                    message,
                    ..
                } = event
                {
                    warnings.push(message);
                }
            },
        )
        .expect("runtime ok");
        assert_eq!(out.channels.len(), 2);
        assert!(out
            .channels
            .iter()
            .all(|channel| channel == &vec![0.0, 0.0]));
        assert!(warnings
            .iter()
            .any(|message| message.contains("slot 8 is vacant")));
    }
}
