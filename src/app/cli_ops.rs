use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use image::{ColorType, ImageBuffer, ImageEncoder, Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use walkdir::WalkDir;

use super::project::{
    self, deserialize_project, loop_mode_from_str, marker_entry_to_project,
    primary_view_from_project, project_other_sub_view_string, project_primary_view_string,
    project_spec_sub_view_string, project_tool_state_to_tool_state, serialize_project, ProjectApp,
    ProjectChannelView, ProjectExportPolicy, ProjectFile, ProjectList,
    ProjectListColumns, ProjectListItem, ProjectMarker, ProjectPluginFxDraft, ProjectTab,
    ProjectToolState,
};
use super::render::spectrogram;
use super::cli_workspace::{resolve_playback_range, CliWorkspace};
use super::types::{
    EditorPrimaryView, EditorSpecSubView, EffectGraphDocument, EffectGraphEdge,
    EffectGraphNode, EffectGraphNodeData, EffectGraphNodeKind, EffectGraphSeverity,
    EffectGraphSpectrumMode, EffectGraphTemplateFile, ListColumnConfig, LoopMode,
    LoopXfadeShape, SpectrogramConfig, SpectrogramData, ToolKind, ToolState, ViewMode,
};
use super::WavesPreviewer;
use crate::audio_io::{
    decode_audio_mono, decode_audio_multi, is_supported_audio_path, read_audio_info,
    read_embedded_artwork, AudioInfo,
};
use crate::cli::{
    BatchCommand, BatchExportArgs, BatchLoudnessApplyArgs, BatchLoudnessCommand,
    BatchLoudnessPlanArgs, CliCommand, CliCursorSnap, CliEffectGraphSpectrumMode,
    CliLoopXfadeShape, CliRoot, CliSpectralViewMode, CliToggle, DebugCommand,
    DebugScreenshotArgs, DebugSummaryArgs, EditorCommand, EditorCursorCommand,
    EditorCursorGetArgs, EditorCursorNudgeArgs, EditorCursorSetArgs, EditorInspectArgs,
    EditorLoopApplyArgs, EditorLoopClearArgs, EditorLoopCommand, EditorLoopGetArgs,
    EditorLoopModeArgs, EditorLoopRepeatArgs, EditorLoopSetArgs, EditorLoopXfadeArgs,
    EditorMarkersAddArgs, EditorMarkersApplyArgs, EditorMarkersClearArgs,
    EditorMarkersCommand, EditorMarkersListArgs, EditorMarkersRemoveArgs, EditorMarkersSetArgs,
    EditorPlaybackCommand, EditorPlaybackPlayArgs, EditorSelectionClearArgs,
    EditorSelectionCommand, EditorSelectionGetArgs, EditorSelectionSetArgs, EditorSourceArgs,
    EditorToolApplyArgs, EditorToolCommand, EditorToolGetArgs, EditorToolSetArgs,
    EditorViewCommand, EditorViewGetArgs, EditorViewSetArgs, EffectGraphCommand,
    EffectGraphEdgeCommand, EffectGraphEdgeConnectArgs, EffectGraphEdgeDisconnectArgs,
    EffectGraphExportArgs, EffectGraphImportArgs, EffectGraphInspectArgs, EffectGraphListArgs,
    EffectGraphNewArgs, EffectGraphNodeAddArgs, EffectGraphNodeCommand, EffectGraphNodeRemoveArgs,
    EffectGraphNodeSetArgs, EffectGraphRefArgs, EffectGraphRenderArgs, EffectGraphSaveArgs,
    EffectGraphTestArgs, EffectGraphValidateArgs, ExportCommand, ExportFileArgs,
    ExportVerifyLoopTagsArgs, ExternalCommand, ExternalConfigCommand, ExternalConfigGetArgs,
    ExternalConfigSetArgs, ExternalInspectArgs, ExternalRenderArgs, ExternalRowsArgs,
    ExternalSourceAddArgs, ExternalSourceClearArgs, ExternalSourceCommand,
    ExternalSourceListArgs, ExternalSourceReloadArgs, ExternalSourceRemoveArgs, ItemArtworkArgs,
    ItemCommand, ItemInspectArgs, ItemMetaArgs, ListColumnsArgs, ListCommand, ListQueryArgs,
    ListRenderArgs, ListSaveQueryArgs, ListSearchArgs, ListSelectArgs, ListSortArgs,
    ListSourceArgs, MusicAiAnalyzeArgs, MusicAiApplyMarkersArgs, MusicAiCommand,
    MusicAiExportStemsArgs, MusicAiInspectArgs, MusicAiModelCommand, MusicAiModelDownloadArgs,
    MusicAiModelStatusArgs, MusicAiModelUninstallArgs, PluginCommand, PluginListArgs,
    PluginProbeArgs, PluginScanArgs, PluginSearchPathAddArgs, PluginSearchPathCommand,
    PluginSearchPathListArgs, PluginSearchPathRemoveArgs, PluginSearchPathResetArgs,
    PluginSessionApplyArgs, PluginSessionClearArgs, PluginSessionCommand,
    PluginSessionInspectArgs, PluginSessionPreviewArgs, PluginSessionSetArgs, RenderCommand,
    RenderEditorArgs, RenderListArgs, RenderSpectrumArgs, RenderWaveformArgs, SessionCommand,
    SessionInspectArgs, SessionNewArgs, TranscriptBatchCommand, TranscriptBatchGenerateArgs,
    TranscriptCommand, TranscriptConfigCommand, TranscriptConfigGetArgs,
    TranscriptConfigSetArgs, TranscriptExportSrtArgs, TranscriptGenerateArgs,
    TranscriptInspectArgs, TranscriptModelCommand, TranscriptModelDownloadArgs,
    TranscriptModelStatusArgs, TranscriptModelUninstallArgs,
};
use crate::loop_markers;
use crate::markers::{self, MarkerEntry};
use crate::plugin::{WorkerRequest, WorkerResponse};
use crate::wave;

const DEFAULT_LIST_COLUMNS: &str =
    "file,folder,length,channels,sample_rate,bits,peak,lufs,gain,wave";
const DEFAULT_WAVEFORM_BG: [u8; 4] = [14, 18, 24, 255];
const DEFAULT_WAVEFORM_LINE: [u8; 4] = [98, 208, 181, 255];
const DEFAULT_WAVEFORM_LINE_B: [u8; 4] = [120, 170, 240, 220];
const DEFAULT_LOOP_FILL: [u8; 4] = [107, 185, 90, 48];
const DEFAULT_LOOP_EDGE: [u8; 4] = [124, 220, 108, 220];
const DEFAULT_MARKER: [u8; 4] = [250, 196, 96, 255];
const DEFAULT_ZERO_LINE: [u8; 4] = [70, 80, 92, 255];
const CLI_PLAYBACK_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Serialize)]
struct CliEnvelope {
    ok: bool,
    command: String,
    result: Value,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug)]
struct CliCommandOutput {
    result: Value,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct LoadedSession {
    path: PathBuf,
    base_dir: PathBuf,
    project: ProjectFile,
}

#[derive(Debug, Clone)]
struct SessionListEntry {
    path: PathBuf,
    pending_gain_db: f32,
}

#[derive(Debug, Clone)]
struct NormalizedOverlay {
    markers: Vec<f32>,
    loop_region: Option<(f32, f32)>,
    source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueryHandleSpec {
    query: Option<String>,
    sort_key: Option<String>,
    sort_dir: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedQueryFilter {
    query: Option<String>,
    sort_key: Option<String>,
    sort_dir: Option<String>,
    query_id: String,
}

#[derive(Debug, Clone, Serialize)]
struct BatchLoudnessRow {
    row_id: String,
    path: String,
    file: String,
    folder: String,
    measured_lufs: Option<f32>,
    raw_peak_db: Option<f32>,
    existing_gain_db: f32,
    effective_lufs: Option<f32>,
    target_lufs: f32,
    estimated_gain_db: Option<f32>,
    proposed_gain_db: Option<f32>,
    clipping_risk: bool,
    warning: Option<String>,
}

#[derive(Debug, Clone)]
struct EffectGraphResolved {
    path: PathBuf,
    file: EffectGraphTemplateFile,
}

#[derive(Clone)]
struct EditorTargetState {
    #[allow(dead_code)]
    path: PathBuf,
    display_path: String,
    total_samples: Option<usize>,
    sample_rate: Option<u32>,
    view_mode: ViewMode,
    waveform_overlay: bool,
    view_offset: usize,
    samples_per_px: f32,
    vertical_zoom: f32,
    vertical_center: f32,
    cursor_sample: Option<usize>,
    selection: Option<(usize, usize)>,
    markers: Vec<MarkerEntry>,
    loop_current: Option<(usize, usize)>,
    loop_applied: Option<(usize, usize)>,
    loop_committed: Option<(usize, usize)>,
    loop_mode: LoopMode,
    loop_xfade_samples: usize,
    loop_xfade_shape: LoopXfadeShape,
    active_tool: ToolKind,
    tool_state: ToolState,
    dirty: bool,
    markers_dirty: bool,
    loop_dirty: bool,
}

#[derive(Debug, Serialize)]
struct ColumnDescriptor {
    key: &'static str,
    description: &'static str,
    enabled_by_default: bool,
}

pub fn run_cli(root: CliRoot) -> Result<()> {
    let command_name = cli_command_name(&root.command).to_string();
    match dispatch_cli(root.command) {
        Ok(out) => {
            emit_envelope(CliEnvelope {
                ok: true,
                command: command_name,
                result: out.result,
                warnings: out.warnings,
                errors: Vec::new(),
            })?;
            Ok(())
        }
        Err(err) => {
            let _ = emit_envelope(CliEnvelope {
                ok: false,
                command: command_name,
                result: Value::Null,
                warnings: Vec::new(),
                errors: error_chain_messages(&err),
            });
            Err(err)
        }
    }
}

fn dispatch_cli(command: CliCommand) -> Result<CliCommandOutput> {
    match command {
        CliCommand::Session(cmd) => dispatch_session(cmd),
        CliCommand::Item(cmd) => dispatch_item(cmd),
        CliCommand::List(cmd) => dispatch_list(cmd),
        CliCommand::Batch(cmd) => dispatch_batch(cmd),
        CliCommand::Editor(cmd) => dispatch_editor(cmd),
        CliCommand::External(cmd) => dispatch_external(cmd),
        CliCommand::Transcript(cmd) => dispatch_transcript(cmd),
        CliCommand::MusicAi(cmd) => dispatch_music_ai(cmd),
        CliCommand::Plugin(cmd) => dispatch_plugin(cmd),
        CliCommand::EffectGraph(cmd) => dispatch_effect_graph(cmd),
        CliCommand::Render(cmd) => dispatch_render(cmd),
        CliCommand::Export(cmd) => dispatch_export(cmd),
        CliCommand::Debug(cmd) => dispatch_debug(cmd),
    }
}

fn cli_command_name(command: &CliCommand) -> &'static str {
    match command {
        CliCommand::Session(SessionCommand::New(_)) => "session.new",
        CliCommand::Session(SessionCommand::Inspect(_)) => "session.inspect",
        CliCommand::Item(ItemCommand::Inspect(_)) => "item.inspect",
        CliCommand::Item(ItemCommand::Meta(_)) => "item.meta",
        CliCommand::Item(ItemCommand::Artwork(_)) => "item.artwork",
        CliCommand::List(ListCommand::Columns(_)) => "list.columns",
        CliCommand::List(ListCommand::Query(_)) => "list.query",
        CliCommand::List(ListCommand::Sort(_)) => "list.sort",
        CliCommand::List(ListCommand::Search(_)) => "list.search",
        CliCommand::List(ListCommand::Select(_)) => "list.select",
        CliCommand::List(ListCommand::SaveQuery(_)) => "list.save-query",
        CliCommand::Batch(BatchCommand::Loudness(BatchLoudnessCommand::Plan(_))) => {
            "batch.loudness.plan"
        }
        CliCommand::Batch(BatchCommand::Loudness(BatchLoudnessCommand::Apply(_))) => {
            "batch.loudness.apply"
        }
        CliCommand::Batch(BatchCommand::Export(_)) => "batch.export",
        CliCommand::List(ListCommand::Render(_)) => "list.render",
        CliCommand::Editor(EditorCommand::Inspect(_)) => "editor.inspect",
        CliCommand::Editor(EditorCommand::View(EditorViewCommand::Get(_))) => "editor.view.get",
        CliCommand::Editor(EditorCommand::View(EditorViewCommand::Set(_))) => "editor.view.set",
        CliCommand::Editor(EditorCommand::Selection(EditorSelectionCommand::Get(_))) => {
            "editor.selection.get"
        }
        CliCommand::Editor(EditorCommand::Selection(EditorSelectionCommand::Set(_))) => {
            "editor.selection.set"
        }
        CliCommand::Editor(EditorCommand::Selection(EditorSelectionCommand::Clear(_))) => {
            "editor.selection.clear"
        }
        CliCommand::Editor(EditorCommand::Cursor(EditorCursorCommand::Get(_))) => {
            "editor.cursor.get"
        }
        CliCommand::Editor(EditorCommand::Cursor(EditorCursorCommand::Set(_))) => {
            "editor.cursor.set"
        }
        CliCommand::Editor(EditorCommand::Cursor(EditorCursorCommand::Nudge(_))) => {
            "editor.cursor.nudge"
        }
        CliCommand::Editor(EditorCommand::Playback(EditorPlaybackCommand::Play(_))) => {
            "editor.playback.play"
        }
        CliCommand::Editor(EditorCommand::Tool(EditorToolCommand::Get(_))) => "editor.tool.get",
        CliCommand::Editor(EditorCommand::Tool(EditorToolCommand::Set(_))) => "editor.tool.set",
        CliCommand::Editor(EditorCommand::Tool(EditorToolCommand::Apply(_))) => {
            "editor.tool.apply"
        }
        CliCommand::Editor(EditorCommand::Markers(EditorMarkersCommand::List(_))) => {
            "editor.markers.list"
        }
        CliCommand::Editor(EditorCommand::Markers(EditorMarkersCommand::Add(_))) => {
            "editor.markers.add"
        }
        CliCommand::Editor(EditorCommand::Markers(EditorMarkersCommand::Set(_))) => {
            "editor.markers.set"
        }
        CliCommand::Editor(EditorCommand::Markers(EditorMarkersCommand::Remove(_))) => {
            "editor.markers.remove"
        }
        CliCommand::Editor(EditorCommand::Markers(EditorMarkersCommand::Clear(_))) => {
            "editor.markers.clear"
        }
        CliCommand::Editor(EditorCommand::Markers(EditorMarkersCommand::Apply(_))) => {
            "editor.markers.apply"
        }
        CliCommand::Editor(EditorCommand::Loop(EditorLoopCommand::Get(_))) => "editor.loop.get",
        CliCommand::Editor(EditorCommand::Loop(EditorLoopCommand::Set(_))) => "editor.loop.set",
        CliCommand::Editor(EditorCommand::Loop(EditorLoopCommand::Clear(_))) => {
            "editor.loop.clear"
        }
        CliCommand::Editor(EditorCommand::Loop(EditorLoopCommand::Apply(_))) => {
            "editor.loop.apply"
        }
        CliCommand::Editor(EditorCommand::Loop(EditorLoopCommand::Mode(_))) => {
            "editor.loop.mode"
        }
        CliCommand::Editor(EditorCommand::Loop(EditorLoopCommand::Xfade(_))) => {
            "editor.loop.xfade"
        }
        CliCommand::Editor(EditorCommand::Loop(EditorLoopCommand::Repeat(_))) => {
            "editor.loop.repeat"
        }
        CliCommand::External(ExternalCommand::Inspect(_)) => "external.inspect",
        CliCommand::External(ExternalCommand::Render(_)) => "external.render",
        CliCommand::External(ExternalCommand::Rows(_)) => "external.rows",
        CliCommand::External(ExternalCommand::Source(ExternalSourceCommand::List(_))) => {
            "external.source.list"
        }
        CliCommand::External(ExternalCommand::Source(ExternalSourceCommand::Add(_))) => {
            "external.source.add"
        }
        CliCommand::External(ExternalCommand::Source(ExternalSourceCommand::Reload(_))) => {
            "external.source.reload"
        }
        CliCommand::External(ExternalCommand::Source(ExternalSourceCommand::Remove(_))) => {
            "external.source.remove"
        }
        CliCommand::External(ExternalCommand::Source(ExternalSourceCommand::Clear(_))) => {
            "external.source.clear"
        }
        CliCommand::External(ExternalCommand::Config(ExternalConfigCommand::Get(_))) => {
            "external.config.get"
        }
        CliCommand::External(ExternalCommand::Config(ExternalConfigCommand::Set(_))) => {
            "external.config.set"
        }
        CliCommand::Transcript(TranscriptCommand::Inspect(_)) => "transcript.inspect",
        CliCommand::Transcript(TranscriptCommand::Model(TranscriptModelCommand::Status(_))) => {
            "transcript.model.status"
        }
        CliCommand::Transcript(TranscriptCommand::Model(TranscriptModelCommand::Download(_))) => {
            "transcript.model.download"
        }
        CliCommand::Transcript(TranscriptCommand::Model(TranscriptModelCommand::Uninstall(_))) => {
            "transcript.model.uninstall"
        }
        CliCommand::Transcript(TranscriptCommand::Config(TranscriptConfigCommand::Get(_))) => {
            "transcript.config.get"
        }
        CliCommand::Transcript(TranscriptCommand::Config(TranscriptConfigCommand::Set(_))) => {
            "transcript.config.set"
        }
        CliCommand::Transcript(TranscriptCommand::Generate(_)) => "transcript.generate",
        CliCommand::Transcript(TranscriptCommand::Batch(TranscriptBatchCommand::Generate(_))) => {
            "transcript.batch.generate"
        }
        CliCommand::Transcript(TranscriptCommand::ExportSrt(_)) => "transcript.export-srt",
        CliCommand::MusicAi(MusicAiCommand::Inspect(_)) => "music-ai.inspect",
        CliCommand::MusicAi(MusicAiCommand::Model(MusicAiModelCommand::Status(_))) => {
            "music-ai.model.status"
        }
        CliCommand::MusicAi(MusicAiCommand::Model(MusicAiModelCommand::Download(_))) => {
            "music-ai.model.download"
        }
        CliCommand::MusicAi(MusicAiCommand::Model(MusicAiModelCommand::Uninstall(_))) => {
            "music-ai.model.uninstall"
        }
        CliCommand::MusicAi(MusicAiCommand::Analyze(_)) => "music-ai.analyze",
        CliCommand::MusicAi(MusicAiCommand::ApplyMarkers(_)) => "music-ai.apply-markers",
        CliCommand::MusicAi(MusicAiCommand::ExportStems(_)) => "music-ai.export-stems",
        CliCommand::Plugin(PluginCommand::SearchPath(PluginSearchPathCommand::List(_))) => {
            "plugin.search-path.list"
        }
        CliCommand::Plugin(PluginCommand::SearchPath(PluginSearchPathCommand::Add(_))) => {
            "plugin.search-path.add"
        }
        CliCommand::Plugin(PluginCommand::SearchPath(PluginSearchPathCommand::Remove(_))) => {
            "plugin.search-path.remove"
        }
        CliCommand::Plugin(PluginCommand::SearchPath(PluginSearchPathCommand::Reset(_))) => {
            "plugin.search-path.reset"
        }
        CliCommand::Plugin(PluginCommand::Scan(_)) => "plugin.scan",
        CliCommand::Plugin(PluginCommand::List(_)) => "plugin.list",
        CliCommand::Plugin(PluginCommand::Probe(_)) => "plugin.probe",
        CliCommand::Plugin(PluginCommand::Session(PluginSessionCommand::Inspect(_))) => {
            "plugin.session.inspect"
        }
        CliCommand::Plugin(PluginCommand::Session(PluginSessionCommand::Set(_))) => {
            "plugin.session.set"
        }
        CliCommand::Plugin(PluginCommand::Session(PluginSessionCommand::Preview(_))) => {
            "plugin.session.preview"
        }
        CliCommand::Plugin(PluginCommand::Session(PluginSessionCommand::Apply(_))) => {
            "plugin.session.apply"
        }
        CliCommand::Plugin(PluginCommand::Session(PluginSessionCommand::Clear(_))) => {
            "plugin.session.clear"
        }
        CliCommand::Render(RenderCommand::Waveform(_)) => "render.waveform",
        CliCommand::Render(RenderCommand::Spectrum(_)) => "render.spectrum",
        CliCommand::Render(RenderCommand::Editor(_)) => "render.editor",
        CliCommand::Render(RenderCommand::List(_)) => "render.list",
        CliCommand::Export(ExportCommand::File(_)) => "export.file",
        CliCommand::Export(ExportCommand::VerifyLoopTags(_)) => "export.verify-loop-tags",
        CliCommand::EffectGraph(EffectGraphCommand::List(_)) => "effect-graph.list",
        CliCommand::EffectGraph(EffectGraphCommand::New(_)) => "effect-graph.new",
        CliCommand::EffectGraph(EffectGraphCommand::Inspect(_)) => "effect-graph.inspect",
        CliCommand::EffectGraph(EffectGraphCommand::Render(_)) => "effect-graph.render",
        CliCommand::EffectGraph(EffectGraphCommand::Validate(_)) => "effect-graph.validate",
        CliCommand::EffectGraph(EffectGraphCommand::Test(_)) => "effect-graph.test",
        CliCommand::EffectGraph(EffectGraphCommand::Save(_)) => "effect-graph.save",
        CliCommand::EffectGraph(EffectGraphCommand::Import(_)) => "effect-graph.import",
        CliCommand::EffectGraph(EffectGraphCommand::Export(_)) => "effect-graph.export",
        CliCommand::EffectGraph(EffectGraphCommand::Node(EffectGraphNodeCommand::Add(_))) => {
            "effect-graph.node.add"
        }
        CliCommand::EffectGraph(EffectGraphCommand::Node(EffectGraphNodeCommand::Remove(_))) => {
            "effect-graph.node.remove"
        }
        CliCommand::EffectGraph(EffectGraphCommand::Node(EffectGraphNodeCommand::Set(_))) => {
            "effect-graph.node.set"
        }
        CliCommand::EffectGraph(EffectGraphCommand::Edge(EffectGraphEdgeCommand::Connect(_))) => {
            "effect-graph.edge.connect"
        }
        CliCommand::EffectGraph(EffectGraphCommand::Edge(EffectGraphEdgeCommand::Disconnect(_))) => {
            "effect-graph.edge.disconnect"
        }
        CliCommand::Debug(DebugCommand::Summary(_)) => "debug.summary",
        CliCommand::Debug(DebugCommand::Screenshot(_)) => "debug.screenshot",
    }
}

fn emit_envelope(envelope: CliEnvelope) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&envelope).context("serialize CLI envelope")?
    );
    Ok(())
}

fn error_chain_messages(err: &anyhow::Error) -> Vec<String> {
    err.chain().map(|part| part.to_string()).collect()
}

fn dispatch_session(command: SessionCommand) -> Result<CliCommandOutput> {
    match command {
        SessionCommand::New(args) => session_new(args),
        SessionCommand::Inspect(args) => session_inspect(args),
    }
}

fn dispatch_item(command: ItemCommand) -> Result<CliCommandOutput> {
    match command {
        ItemCommand::Inspect(args) => item_inspect(args),
        ItemCommand::Meta(args) => item_meta(args),
        ItemCommand::Artwork(args) => item_artwork(args),
    }
}

fn dispatch_list(command: ListCommand) -> Result<CliCommandOutput> {
    match command {
        ListCommand::Columns(args) => list_columns(args),
        ListCommand::Query(args) => list_query(args),
        ListCommand::Sort(args) => list_sort(args),
        ListCommand::Search(args) => list_search(args),
        ListCommand::Select(args) => list_select(args),
        ListCommand::SaveQuery(args) => list_save_query(args),
        ListCommand::Render(args) => list_render(args),
    }
}

fn dispatch_batch(command: BatchCommand) -> Result<CliCommandOutput> {
    match command {
        BatchCommand::Loudness(BatchLoudnessCommand::Plan(args)) => batch_loudness_plan(args),
        BatchCommand::Loudness(BatchLoudnessCommand::Apply(args)) => batch_loudness_apply(args),
        BatchCommand::Export(args) => batch_export(args),
    }
}

fn dispatch_editor(command: EditorCommand) -> Result<CliCommandOutput> {
    match command {
        EditorCommand::Inspect(args) => editor_inspect(args),
        EditorCommand::View(EditorViewCommand::Get(args)) => editor_view_get(args),
        EditorCommand::View(EditorViewCommand::Set(args)) => editor_view_set(args),
        EditorCommand::Selection(EditorSelectionCommand::Get(args)) => editor_selection_get(args),
        EditorCommand::Selection(EditorSelectionCommand::Set(args)) => editor_selection_set(args),
        EditorCommand::Selection(EditorSelectionCommand::Clear(args)) => {
            editor_selection_clear(args)
        }
        EditorCommand::Cursor(EditorCursorCommand::Get(args)) => editor_cursor_get(args),
        EditorCommand::Cursor(EditorCursorCommand::Set(args)) => editor_cursor_set(args),
        EditorCommand::Cursor(EditorCursorCommand::Nudge(args)) => editor_cursor_nudge(args),
        EditorCommand::Playback(EditorPlaybackCommand::Play(args)) => editor_playback_play(args),
        EditorCommand::Tool(EditorToolCommand::Get(args)) => editor_tool_get(args),
        EditorCommand::Tool(EditorToolCommand::Set(args)) => editor_tool_set(args),
        EditorCommand::Tool(EditorToolCommand::Apply(args)) => editor_tool_apply(args),
        EditorCommand::Markers(EditorMarkersCommand::List(args)) => editor_markers_list(args),
        EditorCommand::Markers(EditorMarkersCommand::Add(args)) => editor_markers_add(args),
        EditorCommand::Markers(EditorMarkersCommand::Set(args)) => editor_markers_set(args),
        EditorCommand::Markers(EditorMarkersCommand::Remove(args)) => editor_markers_remove(args),
        EditorCommand::Markers(EditorMarkersCommand::Clear(args)) => editor_markers_clear(args),
        EditorCommand::Markers(EditorMarkersCommand::Apply(args)) => editor_markers_apply(args),
        EditorCommand::Loop(EditorLoopCommand::Get(args)) => editor_loop_get(args),
        EditorCommand::Loop(EditorLoopCommand::Set(args)) => editor_loop_set(args),
        EditorCommand::Loop(EditorLoopCommand::Clear(args)) => editor_loop_clear(args),
        EditorCommand::Loop(EditorLoopCommand::Apply(args)) => editor_loop_apply(args),
        EditorCommand::Loop(EditorLoopCommand::Mode(args)) => editor_loop_mode(args),
        EditorCommand::Loop(EditorLoopCommand::Xfade(args)) => editor_loop_xfade(args),
        EditorCommand::Loop(EditorLoopCommand::Repeat(args)) => editor_loop_repeat(args),
    }
}

fn dispatch_external(command: ExternalCommand) -> Result<CliCommandOutput> {
    match command {
        ExternalCommand::Inspect(args) => external_inspect(args),
        ExternalCommand::Render(args) => external_render(args),
        ExternalCommand::Rows(args) => external_rows(args),
        ExternalCommand::Source(ExternalSourceCommand::List(args)) => external_source_list(args),
        ExternalCommand::Source(ExternalSourceCommand::Add(args)) => external_source_add(args),
        ExternalCommand::Source(ExternalSourceCommand::Reload(args)) => {
            external_source_reload(args)
        }
        ExternalCommand::Source(ExternalSourceCommand::Remove(args)) => {
            external_source_remove(args)
        }
        ExternalCommand::Source(ExternalSourceCommand::Clear(args)) => external_source_clear(args),
        ExternalCommand::Config(ExternalConfigCommand::Get(args)) => external_config_get(args),
        ExternalCommand::Config(ExternalConfigCommand::Set(args)) => external_config_set(args),
    }
}

fn dispatch_transcript(command: TranscriptCommand) -> Result<CliCommandOutput> {
    match command {
        TranscriptCommand::Inspect(args) => transcript_inspect(args),
        TranscriptCommand::Model(TranscriptModelCommand::Status(args)) => {
            transcript_model_status(args)
        }
        TranscriptCommand::Model(TranscriptModelCommand::Download(args)) => {
            transcript_model_download(args)
        }
        TranscriptCommand::Model(TranscriptModelCommand::Uninstall(args)) => {
            transcript_model_uninstall(args)
        }
        TranscriptCommand::Config(TranscriptConfigCommand::Get(args)) => transcript_config_get(args),
        TranscriptCommand::Config(TranscriptConfigCommand::Set(args)) => transcript_config_set(args),
        TranscriptCommand::Generate(args) => transcript_generate(args),
        TranscriptCommand::Batch(TranscriptBatchCommand::Generate(args)) => {
            transcript_batch_generate(args)
        }
        TranscriptCommand::ExportSrt(args) => transcript_export_srt(args),
    }
}

fn dispatch_music_ai(command: MusicAiCommand) -> Result<CliCommandOutput> {
    match command {
        MusicAiCommand::Inspect(args) => music_ai_inspect(args),
        MusicAiCommand::Model(MusicAiModelCommand::Status(args)) => music_ai_model_status(args),
        MusicAiCommand::Model(MusicAiModelCommand::Download(args)) => music_ai_model_download(args),
        MusicAiCommand::Model(MusicAiModelCommand::Uninstall(args)) => {
            music_ai_model_uninstall(args)
        }
        MusicAiCommand::Analyze(args) => music_ai_analyze(args),
        MusicAiCommand::ApplyMarkers(args) => music_ai_apply_markers(args),
        MusicAiCommand::ExportStems(args) => music_ai_export_stems(args),
    }
}

fn dispatch_plugin(command: PluginCommand) -> Result<CliCommandOutput> {
    match command {
        PluginCommand::SearchPath(PluginSearchPathCommand::List(args)) => {
            plugin_search_path_list(args)
        }
        PluginCommand::SearchPath(PluginSearchPathCommand::Add(args)) => {
            plugin_search_path_add(args)
        }
        PluginCommand::SearchPath(PluginSearchPathCommand::Remove(args)) => {
            plugin_search_path_remove(args)
        }
        PluginCommand::SearchPath(PluginSearchPathCommand::Reset(args)) => {
            plugin_search_path_reset(args)
        }
        PluginCommand::Scan(args) => plugin_scan(args),
        PluginCommand::List(args) => plugin_list(args),
        PluginCommand::Probe(args) => plugin_probe(args),
        PluginCommand::Session(PluginSessionCommand::Inspect(args)) => plugin_session_inspect(args),
        PluginCommand::Session(PluginSessionCommand::Set(args)) => plugin_session_set(args),
        PluginCommand::Session(PluginSessionCommand::Preview(args)) => plugin_session_preview(args),
        PluginCommand::Session(PluginSessionCommand::Apply(args)) => plugin_session_apply(args),
        PluginCommand::Session(PluginSessionCommand::Clear(args)) => plugin_session_clear(args),
    }
}

fn dispatch_effect_graph(command: EffectGraphCommand) -> Result<CliCommandOutput> {
    match command {
        EffectGraphCommand::List(args) => effect_graph_list(args),
        EffectGraphCommand::New(args) => effect_graph_new(args),
        EffectGraphCommand::Inspect(args) => effect_graph_inspect(args),
        EffectGraphCommand::Render(args) => effect_graph_render(args),
        EffectGraphCommand::Validate(args) => effect_graph_validate(args),
        EffectGraphCommand::Test(args) => effect_graph_test(args),
        EffectGraphCommand::Save(args) => effect_graph_save(args),
        EffectGraphCommand::Import(args) => effect_graph_import(args),
        EffectGraphCommand::Export(args) => effect_graph_export(args),
        EffectGraphCommand::Node(EffectGraphNodeCommand::Add(args)) => effect_graph_node_add(args),
        EffectGraphCommand::Node(EffectGraphNodeCommand::Remove(args)) => {
            effect_graph_node_remove(args)
        }
        EffectGraphCommand::Node(EffectGraphNodeCommand::Set(args)) => effect_graph_node_set(args),
        EffectGraphCommand::Edge(EffectGraphEdgeCommand::Connect(args)) => {
            effect_graph_edge_connect(args)
        }
        EffectGraphCommand::Edge(EffectGraphEdgeCommand::Disconnect(args)) => {
            effect_graph_edge_disconnect(args)
        }
    }
}

fn dispatch_render(command: RenderCommand) -> Result<CliCommandOutput> {
    match command {
        RenderCommand::Waveform(args) => render_waveform(args),
        RenderCommand::Spectrum(args) => render_spectrum(args),
        RenderCommand::Editor(args) => render_editor(args),
        RenderCommand::List(args) => render_list(args),
    }
}

fn dispatch_export(command: ExportCommand) -> Result<CliCommandOutput> {
    match command {
        ExportCommand::File(args) => export_file(args),
        ExportCommand::VerifyLoopTags(args) => export_verify_loop_tags(args),
    }
}

fn dispatch_debug(command: DebugCommand) -> Result<CliCommandOutput> {
    match command {
        DebugCommand::Summary(args) => debug_summary(args),
        DebugCommand::Screenshot(args) => debug_screenshot(args),
    }
}

fn session_new(args: SessionNewArgs) -> Result<CliCommandOutput> {
    let entries = session_entries_from_sources(args.folder.as_deref(), &args.input)?;
    let project = build_project_file_from_entries(&entries)?;
    let output_path = match args.output {
        Some(path) => {
            write_project_file(&path, &project)?;
            Some(absolute_string(&path)?)
        }
        None => None,
    };
    Ok(CliCommandOutput {
        result: json!({
            "session_path": output_path,
            "file_count": entries.len(),
            "files": entries.iter().map(|entry| pathbuf_to_string(&entry.path)).collect::<Vec<_>>(),
            "open_first": args.open_first,
            "project_version": project.version,
        }),
        warnings: Vec::new(),
    })
}

fn session_inspect(args: SessionInspectArgs) -> Result<CliCommandOutput> {
    let session = load_session(&args.session)?;
    let entries = session_list_entries(&session);
    let active_tab_path = session
        .project
        .active_tab
        .and_then(|idx| session.project.tabs.get(idx))
        .map(|tab| project::resolve_path(&tab.path, &session.base_dir))
        .map(|path| pathbuf_to_string(&path));
    Ok(CliCommandOutput {
        result: json!({
            "session_path": absolute_string(&session.path)?,
            "project_version": session.project.version,
            "file_count": entries.len(),
            "files": entries.iter().map(|entry| pathbuf_to_string(&entry.path)).collect::<Vec<_>>(),
            "tab_count": session.project.tabs.len(),
            "cached_edit_count": session.project.cached_edits.len(),
            "active_tab_path": active_tab_path,
            "tabs": session.project.tabs.iter().map(|tab| json!({
                "path": pathbuf_to_string(&project::resolve_path(&tab.path, &session.base_dir)),
                "view_mode": tab.view_mode,
                "dirty": tab.dirty,
                "markers": tab.markers.len(),
                "loop_region": tab.loop_region,
            })).collect::<Vec<_>>(),
        }),
        warnings: Vec::new(),
    })
}

fn item_inspect(args: ItemInspectArgs) -> Result<CliCommandOutput> {
    let path = absolute_existing_path(&args.input)?;
    let info = read_audio_info(&path)?;
    let markers = read_markers_in_file_space(&path, &info)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": pathbuf_to_string(&path),
            "meta": audio_info_json(&info),
            "markers": markers.iter().map(marker_json).collect::<Vec<_>>(),
            "loop_region": read_loop_range_usize(&path),
            "artwork_embedded": read_embedded_artwork(&path).is_some(),
        }),
        warnings: Vec::new(),
    })
}

fn item_meta(args: ItemMetaArgs) -> Result<CliCommandOutput> {
    let path = absolute_existing_path(&args.input)?;
    let info = read_audio_info(&path)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": pathbuf_to_string(&path),
            "meta": audio_info_json(&info),
        }),
        warnings: Vec::new(),
    })
}

fn item_artwork(args: ItemArtworkArgs) -> Result<CliCommandOutput> {
    let path = absolute_existing_path(&args.input)?;
    let Some(bytes) = read_embedded_artwork(&path) else {
        return Ok(CliCommandOutput {
            result: json!({
                "path": pathbuf_to_string(&path),
                "artwork_found": false,
                "output": Value::Null,
            }),
            warnings: Vec::new(),
        });
    };
    let output = prepare_output_path(args.output, "artwork", "png")?;
    let image = image::load_from_memory(&bytes).context("decode embedded artwork")?;
    image
        .to_rgba8()
        .save(&output)
        .with_context(|| format!("save artwork png: {}", output.display()))?;
    Ok(CliCommandOutput {
        result: json!({
            "path": pathbuf_to_string(&path),
            "artwork_found": true,
            "output": absolute_string(&output)?,
        }),
        warnings: Vec::new(),
    })
}

fn list_columns(_args: ListColumnsArgs) -> Result<CliCommandOutput> {
    let columns = vec![
        ColumnDescriptor { key: "edited", description: "Dirty/edit badge", enabled_by_default: true },
        ColumnDescriptor { key: "cover_art", description: "Cover art thumbnail", enabled_by_default: false },
        ColumnDescriptor { key: "type_badge", description: "File type badge", enabled_by_default: false },
        ColumnDescriptor { key: "file", description: "File name", enabled_by_default: true },
        ColumnDescriptor { key: "folder", description: "Parent folder", enabled_by_default: true },
        ColumnDescriptor { key: "transcript", description: "Transcript summary", enabled_by_default: false },
        ColumnDescriptor { key: "transcript_language", description: "Transcript language", enabled_by_default: false },
        ColumnDescriptor { key: "external", description: "Merged external columns", enabled_by_default: true },
        ColumnDescriptor { key: "length", description: "Duration", enabled_by_default: true },
        ColumnDescriptor { key: "channels", description: "Channel count", enabled_by_default: true },
        ColumnDescriptor { key: "sample_rate", description: "Sample rate", enabled_by_default: true },
        ColumnDescriptor { key: "bits", description: "Bit depth", enabled_by_default: true },
        ColumnDescriptor { key: "bit_rate", description: "Bit rate", enabled_by_default: false },
        ColumnDescriptor { key: "peak", description: "Peak dBFS", enabled_by_default: true },
        ColumnDescriptor { key: "lufs", description: "Integrated LUFS", enabled_by_default: true },
        ColumnDescriptor { key: "bpm", description: "BPM", enabled_by_default: false },
        ColumnDescriptor { key: "created_at", description: "Created timestamp", enabled_by_default: false },
        ColumnDescriptor { key: "modified_at", description: "Modified timestamp", enabled_by_default: false },
        ColumnDescriptor { key: "gain", description: "Pending gain dB", enabled_by_default: true },
        ColumnDescriptor { key: "wave", description: "Wave thumbnail column", enabled_by_default: true },
    ];
    Ok(CliCommandOutput {
        result: json!({
            "columns": columns,
            "default": DEFAULT_LIST_COLUMNS.split(',').collect::<Vec<_>>(),
        }),
        warnings: Vec::new(),
    })
}

fn list_query(args: ListQueryArgs) -> Result<CliCommandOutput> {
    let filter = resolve_query_filter(&args.filter)?;
    let columns = parse_list_column_keys(&args.columns)?;
    let source = load_list_source(&args.source)?;
    let mut rows = list_rows_from_source(&source, args.include_overlays)?;
    apply_list_query_filter_sort(
        &mut rows,
        filter.query.as_deref(),
        filter.sort_key.as_deref(),
        filter.sort_dir.as_deref(),
    );
    let total = rows.len();
    let rows = slice_rows(rows, args.offset, args.limit);
    Ok(CliCommandOutput {
        result: json!({
            "query_id": filter.query_id,
            "total": total,
            "offset": args.offset,
            "limit": args.limit,
            "columns": columns,
            "rows": rows.into_iter().map(Value::Object).collect::<Vec<_>>(),
        }),
        warnings: Vec::new(),
    })
}

fn list_sort(args: ListSortArgs) -> Result<CliCommandOutput> {
    let mut session = load_session(&args.session)?;
    session.project.app.sort_key = args.sort_key.clone();
    session.project.app.sort_dir = args.sort_dir.clone();
    save_session(&session)?;
    list_query(ListQueryArgs {
        source: ListSourceArgs {
            folder: None,
            session: Some(args.session),
        },
        columns: args.columns,
        filter: crate::cli::CliQueryFilterArgs {
            query: args.query,
            sort_key: Some(args.sort_key),
            sort_dir: Some(args.sort_dir),
            query_id: None,
        },
        offset: args.offset,
        limit: args.limit,
        include_overlays: args.include_overlays,
    })
}

fn list_search(args: ListSearchArgs) -> Result<CliCommandOutput> {
    let mut session = load_session(&args.session)?;
    session.project.app.search_query = args.query.clone();
    session.project.app.search_regex = false;
    save_session(&session)?;
    list_query(ListQueryArgs {
        source: ListSourceArgs {
            folder: None,
            session: Some(args.session),
        },
        columns: args.columns,
        filter: crate::cli::CliQueryFilterArgs {
            query: Some(args.query),
            sort_key: args.sort_key,
            sort_dir: args.sort_dir,
            query_id: None,
        },
        offset: args.offset,
        limit: args.limit,
        include_overlays: args.include_overlays,
    })
}

fn list_select(args: ListSelectArgs) -> Result<CliCommandOutput> {
    let mut session = load_session(&args.session)?;
    let entries = session_list_entries(&session);
    let selected = if let Some(path) = args.path.as_deref() {
        absolute_output_path(path)?
    } else {
        let query = args
            .query
            .as_deref()
            .context("list select requires --path or --query")?;
        let mut rows = entries
            .iter()
            .map(|entry| list_row_for_entry(entry, Some(&session), false))
            .collect::<Result<Vec<_>>>()?;
        apply_list_query_filter_sort(&mut rows, Some(query), None, None);
        let row = rows
            .get(args.index)
            .context("list select index out of range for query result")?;
        PathBuf::from(
            row.get("path")
                .and_then(Value::as_str)
                .context("selected row missing path")?,
        )
    };
    session.project.app.selected_path = Some(project::rel_path(&selected, &session.base_dir));
    save_session(&session)?;
    Ok(CliCommandOutput {
        result: json!({
            "session": absolute_string(&args.session)?,
            "selected_path": pathbuf_to_string(&selected),
            "selected_row_id": stable_row_id_for_path(&selected),
        }),
        warnings: Vec::new(),
    })
}

fn list_save_query(args: ListSaveQueryArgs) -> Result<CliCommandOutput> {
    let filter = resolve_query_filter(&args.filter)?;
    let source = render_source_json_for_list(&args.source)?;
    Ok(CliCommandOutput {
        result: json!({
            "query_id": filter.query_id,
            "query": filter.query,
            "sort_key": filter.sort_key,
            "sort_dir": filter.sort_dir,
            "source": source,
        }),
        warnings: Vec::new(),
    })
}

fn list_render(args: ListRenderArgs) -> Result<CliCommandOutput> {
    let source = load_list_source(&args.source)?;
    let project = project_for_list_render(source, &args.columns, args.offset, args.limit)?;
    let temp_session = write_temp_project_file("list-render", &project)?;
    let output = prepare_output_path(args.output, "list", "png")?;
    let abs_output = render_gui_session_screenshot(&temp_session, &output, None, None, false)?;
    let (width, height) = image_dimensions(&abs_output)?;
    Ok(CliCommandOutput {
        result: json!({
            "session": absolute_string(&temp_session)?,
            "path": absolute_string(&abs_output)?,
            "width": width,
            "height": height,
            "source": render_source_json_for_list(&args.source)?,
        }),
        warnings: Vec::new(),
    })
}

fn batch_loudness_plan(args: BatchLoudnessPlanArgs) -> Result<CliCommandOutput> {
    let session = load_session(&args.session)?;
    let filter = resolve_query_filter(&args.filter)?;
    let rows = build_batch_loudness_rows(&session, &filter, args.target_lufs)?;
    if let Some(report) = args.report.as_deref() {
        write_batch_loudness_report(report, &rows, args.target_lufs, false)?;
    }
    Ok(CliCommandOutput {
        result: json!({
            "query_id": filter.query_id,
            "target_lufs": args.target_lufs,
            "matched_paths": rows.iter().map(|row| row.path.clone()).collect::<Vec<_>>(),
            "rows": rows,
            "report_path": args.report.as_deref().map(absolute_string).transpose()?,
        }),
        warnings: Vec::new(),
    })
}

fn batch_loudness_apply(args: BatchLoudnessApplyArgs) -> Result<CliCommandOutput> {
    let mut session = load_session(&args.session)?;
    let filter = resolve_query_filter(&args.filter)?;
    let rows = build_batch_loudness_rows(&session, &filter, args.target_lufs)?;
    let before = session_pending_gain_map(&session);
    let mut updated_paths = Vec::new();
    let mut unchanged_paths = Vec::new();
    let mut failed_paths = Vec::<Value>::new();
    for row in &rows {
        let path = PathBuf::from(&row.path);
        match row.proposed_gain_db {
            Some(gain) if gain.is_finite() => {
                let old = pending_gain_for_session_path(&session, &path);
                if (old - gain).abs() > 0.0001 {
                    set_pending_gain_for_session_path(&mut session, &path, gain);
                    updated_paths.push(pathbuf_to_string(&path));
                } else {
                    unchanged_paths.push(pathbuf_to_string(&path));
                }
            }
            _ => failed_paths.push(json!({
                "path": row.path,
                "error": row.warning.clone().unwrap_or_else(|| "could not compute loudness plan".to_string()),
            })),
        }
    }
    save_session(&session)?;
    let after = session_pending_gain_map(&session);
    if let Some(report) = args.report.as_deref() {
        write_batch_loudness_report(report, &rows, args.target_lufs, true)?;
    }
    Ok(CliCommandOutput {
        result: json!({
            "query_id": filter.query_id,
            "target_lufs": args.target_lufs,
            "before": before,
            "after": after,
            "mutated_paths": updated_paths,
            "updated_paths": updated_paths,
            "unchanged_paths": unchanged_paths,
            "failed_paths": failed_paths,
            "session_dirty": true,
            "report_path": args.report.as_deref().map(absolute_string).transpose()?,
        }),
        warnings: Vec::new(),
    })
}

fn batch_export(args: BatchExportArgs) -> Result<CliCommandOutput> {
    if args.overwrite == args.output_dir.is_some() {
        bail!("batch export requires exactly one of --overwrite or --output-dir");
    }
    let session = load_session(&args.session)?;
    let filter = resolve_query_filter(&args.filter)?;
    let matched = matched_session_entries(&session, &filter)?;
    let before = session_pending_gain_map(&session);
    let mut workspace = CliWorkspace::load(&args.session)?;
    let mut mutated_paths = Vec::new();
    let mut skipped_paths = Vec::new();
    let mut failed_paths = Vec::<Value>::new();
    let output_dir = args
        .output_dir
        .as_deref()
        .map(absolute_output_path)
        .transpose()?;
    if let Some(dir) = output_dir.as_deref() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create batch export output dir: {}", dir.display()))?;
    }
    for entry in matched {
        let has_session_edit = find_project_tab_index(&session, &entry.path).is_some()
            || session.project.cached_edits.iter().any(|edit| {
                path_key(&project::resolve_path(&edit.path, &session.base_dir)) == path_key(&entry.path)
            });
        if args.overwrite && entry.pending_gain_db.abs() <= 0.0001 && !has_session_edit {
            skipped_paths.push(pathbuf_to_string(&entry.path));
            continue;
        }
        let dst = output_dir
            .as_ref()
            .map(|dir| dir.join(entry.path.file_name().unwrap_or_default()));
        match workspace.export_target(
            Some(&entry.path),
            dst.as_deref(),
            args.overwrite,
            (entry.pending_gain_db.abs() > 0.0001).then_some(entry.pending_gain_db),
            None,
            None,
        ) {
            Ok((src, dst, _markers, _loop_region)) => {
                mutated_paths.push(json!({
                    "source": pathbuf_to_string(&src),
                    "destination": pathbuf_to_string(&dst),
                    "gain_db": entry.pending_gain_db,
                }));
                if args.overwrite {
                    workspace.set_pending_gain_db_for_path(&src, 0.0);
                }
            }
            Err(err) => failed_paths.push(json!({
                "path": pathbuf_to_string(&entry.path),
                "error": err.to_string(),
            })),
        }
    }
    if args.overwrite {
        workspace.save()?;
    }
    let after_workspace = CliWorkspace::load(&args.session)?;
    let after = after_workspace.pending_gain_map();
    if let Some(report) = args.report.as_deref() {
        write_batch_export_report(report, &mutated_paths, &skipped_paths, &failed_paths)?;
    }
    Ok(CliCommandOutput {
        result: json!({
            "query_id": filter.query_id,
            "before": before,
            "after": after,
            "mutated_paths": mutated_paths,
            "skipped_paths": skipped_paths,
            "failed_paths": failed_paths,
            "report_path": args.report.as_deref().map(absolute_string).transpose()?,
        }),
        warnings: Vec::new(),
    })
}

fn effect_graph_list(_args: EffectGraphListArgs) -> Result<CliCommandOutput> {
    let entries = effect_graph_library_entries()?;
    Ok(CliCommandOutput {
        result: json!({
            "templates_dir": absolute_string(&super::effect_graph_ops::effect_graph_templates_dir_for_cli().map_err(anyhow::Error::msg)?)?,
            "graphs": entries,
        }),
        warnings: Vec::new(),
    })
}

fn effect_graph_new(args: EffectGraphNewArgs) -> Result<CliCommandOutput> {
    let mut template = if let Some(graph) = args.template.as_deref() {
        let resolved = load_effect_graph(graph)?;
        resolved.file
    } else {
        EffectGraphTemplateFile {
            schema_version: 3,
            template_id: String::new(),
            name: args.name.clone(),
            created_at_unix_ms: now_unix_ms_local(),
            updated_at_unix_ms: now_unix_ms_local(),
            graph: EffectGraphDocument::default(),
        }
    };
    template.name = args.name.trim().to_string();
    template.graph.name = template.name.clone();
    template.created_at_unix_ms = now_unix_ms_local();
    template.updated_at_unix_ms = template.created_at_unix_ms;
    template.template_id = format!(
        "{}_{}",
        sanitize_cli_token(&template.name),
        template.updated_at_unix_ms
    );
    let output = match args.output {
        Some(path) => absolute_output_path(&path)?,
        None => default_effect_graph_output_path_for_new(&template.name)?,
    };
    save_effect_graph_file(&output, &template)?;
    effect_graph_inspect(EffectGraphInspectArgs {
        graph: EffectGraphRefArgs {
            graph: pathbuf_to_string(&output),
        },
    })
}

fn effect_graph_inspect(args: EffectGraphInspectArgs) -> Result<CliCommandOutput> {
    let resolved = load_effect_graph(&args.graph.graph)?;
    let issues = super::effect_graph_ops::effect_graph_validate_for_cli(&resolved.file.graph);
    let summary = effect_graph_issue_summary(&issues);
    Ok(CliCommandOutput {
        result: json!({
            "path": pathbuf_to_string(&resolved.path),
            "template_id": resolved.file.template_id,
            "name": resolved.file.name,
            "created_at_unix_ms": resolved.file.created_at_unix_ms,
            "updated_at_unix_ms": resolved.file.updated_at_unix_ms,
            "validation": summary,
            "node_count": resolved.file.graph.nodes.len(),
            "edge_count": resolved.file.graph.edges.len(),
            "graph": resolved.file,
        }),
        warnings: Vec::new(),
    })
}

fn effect_graph_render(args: EffectGraphRenderArgs) -> Result<CliCommandOutput> {
    let resolved = load_effect_graph(&args.graph.graph)?;
    let image = draw_effect_graph_document_image(&resolved.file.graph, 1440, 900);
    let output = prepare_output_path(args.output, "effect-graph", "png")?;
    save_rgba_image(&image, &output)?;
    Ok(CliCommandOutput {
        result: json!({
            "graph": pathbuf_to_string(&resolved.path),
            "path": absolute_string(&output)?,
            "width": image.width(),
            "height": image.height(),
        }),
        warnings: Vec::new(),
    })
}

fn effect_graph_validate(args: EffectGraphValidateArgs) -> Result<CliCommandOutput> {
    let resolved = load_effect_graph(&args.graph.graph)?;
    let issues = super::effect_graph_ops::effect_graph_validate_for_cli(&resolved.file.graph);
    let summary = effect_graph_issue_summary(&issues);
    if let Some(report) = args.report.as_deref() {
        write_effect_graph_validation_report(report, &resolved, &issues)?;
    }
    Ok(CliCommandOutput {
        result: json!({
            "graph": pathbuf_to_string(&resolved.path),
            "template_id": resolved.file.template_id,
            "graph_name": resolved.file.name,
            "validation": summary,
            "issues": issues.iter().map(effect_graph_issue_json).collect::<Vec<_>>(),
            "report_path": args.report.as_deref().map(absolute_string).transpose()?,
        }),
        warnings: Vec::new(),
    })
}

fn effect_graph_test(args: EffectGraphTestArgs) -> Result<CliCommandOutput> {
    let resolved = load_effect_graph(&args.graph.graph)?;
    let issues = super::effect_graph_ops::effect_graph_validate_for_cli(&resolved.file.graph);
    if issues
        .iter()
        .any(|issue| issue.severity == EffectGraphSeverity::Error)
    {
        bail!("effect graph has validation errors");
    }
    let input = args.input.as_deref().map(absolute_existing_path).transpose()?;
    let report = super::effect_graph_ops::effect_graph_test_document_for_cli(
        &resolved.file.graph,
        input.as_deref(),
    )
    .map_err(anyhow::Error::msg)?;
    let preview = draw_rough_waveform_preview(&report.rough_waveform, 1280, 320);
    let preview_path = prepare_output_path(args.output, "effect-graph-test", "png")?;
    save_rgba_image(&preview, &preview_path)?;
    if let Some(report_path) = args.report.as_deref() {
        write_effect_graph_test_report(report_path, &resolved, input.as_deref(), &report, &preview_path)?;
    }
    Ok(CliCommandOutput {
        result: json!({
            "graph": pathbuf_to_string(&resolved.path),
            "input": input.as_ref().map(|path| pathbuf_to_string(path.as_path())),
            "used_embedded_sample": report.used_embedded_sample,
            "output_channels": report.output_channel_count,
            "output_sample_rate": report.output_sample_rate,
            "per_channel_peak_db": report.per_channel_peak_db,
            "silent_outputs": report.silent_outputs,
            "debug_preview": report.debug_preview.as_ref().map(|preview| match preview {
                super::types::EffectGraphDebugPreview::Waveform { mono, sample_rate } => json!({
                    "kind": "waveform",
                    "sample_rate": sample_rate,
                    "sample_count": mono.len(),
                }),
                super::types::EffectGraphDebugPreview::Spectrum { spectrogram } => json!({
                    "kind": "spectrum",
                    "rows": spectrogram.bins,
                    "cols": spectrogram.frames,
                    "sample_rate": spectrogram.sample_rate,
                }),
            }),
            "rendered_preview_path": absolute_string(&preview_path)?,
            "report_path": args.report.as_deref().map(absolute_string).transpose()?,
        }),
        warnings: Vec::new(),
    })
}

fn effect_graph_save(args: EffectGraphSaveArgs) -> Result<CliCommandOutput> {
    let mut resolved = load_effect_graph(&args.graph.graph)?;
    resolved.file.updated_at_unix_ms = now_unix_ms_local();
    save_effect_graph_file(&resolved.path, &resolved.file)?;
    effect_graph_inspect(EffectGraphInspectArgs { graph: args.graph })
}

fn effect_graph_import(args: EffectGraphImportArgs) -> Result<CliCommandOutput> {
    let input = absolute_existing_path(&args.input)?;
    let text = std::fs::read_to_string(&input)
        .with_context(|| format!("read effect graph import: {}", input.display()))?;
    let mut file = serde_json::from_str::<EffectGraphTemplateFile>(&text)
        .with_context(|| format!("parse effect graph import: {}", input.display()))?;
    file.updated_at_unix_ms = now_unix_ms_local();
    let output = match args.output {
        Some(path) => absolute_output_path(&path)?,
        None => default_effect_graph_output_path_for_new(&file.name)?,
    };
    save_effect_graph_file(&output, &file)?;
    effect_graph_inspect(EffectGraphInspectArgs {
        graph: EffectGraphRefArgs {
            graph: pathbuf_to_string(&output),
        },
    })
}

fn effect_graph_export(args: EffectGraphExportArgs) -> Result<CliCommandOutput> {
    let resolved = load_effect_graph(&args.graph.graph)?;
    let output = absolute_output_path(&args.output)?;
    save_effect_graph_file(&output, &resolved.file)?;
    Ok(CliCommandOutput {
        result: json!({
            "source": pathbuf_to_string(&resolved.path),
            "destination": absolute_string(&output)?,
        }),
        warnings: Vec::new(),
    })
}

fn effect_graph_node_add(args: EffectGraphNodeAddArgs) -> Result<CliCommandOutput> {
    let mut resolved = load_effect_graph(&args.graph.graph)?;
    let kind = match args.kind {
        crate::cli::CliEffectGraphNodeKind::Input => EffectGraphNodeKind::Input,
        crate::cli::CliEffectGraphNodeKind::Output => EffectGraphNodeKind::Output,
        crate::cli::CliEffectGraphNodeKind::Gain => EffectGraphNodeKind::Gain,
        crate::cli::CliEffectGraphNodeKind::Loudness => EffectGraphNodeKind::Loudness,
        crate::cli::CliEffectGraphNodeKind::MonoMix => EffectGraphNodeKind::MonoMix,
        crate::cli::CliEffectGraphNodeKind::Pitch => EffectGraphNodeKind::PitchShift,
        crate::cli::CliEffectGraphNodeKind::Stretch => EffectGraphNodeKind::TimeStretch,
        crate::cli::CliEffectGraphNodeKind::Speed => EffectGraphNodeKind::Speed,
        crate::cli::CliEffectGraphNodeKind::PluginFx => EffectGraphNodeKind::PluginFx,
        crate::cli::CliEffectGraphNodeKind::Duplicate => EffectGraphNodeKind::Duplicate,
        crate::cli::CliEffectGraphNodeKind::SplitChannels => EffectGraphNodeKind::SplitChannels,
        crate::cli::CliEffectGraphNodeKind::CombineChannels => EffectGraphNodeKind::CombineChannels,
        crate::cli::CliEffectGraphNodeKind::DebugWaveform => EffectGraphNodeKind::DebugWaveform,
        crate::cli::CliEffectGraphNodeKind::DebugSpectrum => EffectGraphNodeKind::DebugSpectrum,
    };
    let node_id = args
        .node_id
        .unwrap_or_else(|| unique_effect_graph_node_id(&resolved.file.graph, &format!("{:?}", kind)));
    let node = EffectGraphNode {
        id: node_id,
        ui_pos: [args.x, args.y],
        ui_size: default_effect_graph_node_size(kind),
        data: EffectGraphNodeData::default_for_kind(kind),
    };
    resolved.file.graph.nodes.push(node);
    resolved.file.updated_at_unix_ms = now_unix_ms_local();
    save_effect_graph_file(&resolved.path, &resolved.file)?;
    effect_graph_inspect(EffectGraphInspectArgs {
        graph: EffectGraphRefArgs {
            graph: pathbuf_to_string(&resolved.path),
        },
    })
}

fn effect_graph_node_remove(args: EffectGraphNodeRemoveArgs) -> Result<CliCommandOutput> {
    let mut resolved = load_effect_graph(&args.graph.graph)?;
    resolved.file.graph.nodes.retain(|node| node.id != args.node_id);
    resolved
        .file
        .graph
        .edges
        .retain(|edge| edge.from_node_id != args.node_id && edge.to_node_id != args.node_id);
    resolved.file.updated_at_unix_ms = now_unix_ms_local();
    save_effect_graph_file(&resolved.path, &resolved.file)?;
    effect_graph_inspect(EffectGraphInspectArgs {
        graph: EffectGraphRefArgs {
            graph: pathbuf_to_string(&resolved.path),
        },
    })
}

fn effect_graph_node_set(args: EffectGraphNodeSetArgs) -> Result<CliCommandOutput> {
    let mut resolved = load_effect_graph(&args.graph.graph)?;
    let node = resolved
        .file
        .graph
        .nodes
        .iter_mut()
        .find(|node| node.id == args.node_id)
        .context("effect graph node not found")?;
    if let Some(x) = args.x {
        node.ui_pos[0] = x;
    }
    if let Some(y) = args.y {
        node.ui_pos[1] = y;
    }
    if let Some(width) = args.width {
        node.ui_size[0] = width.max(80.0);
    }
    if let Some(height) = args.height {
        node.ui_size[1] = height.max(60.0);
    }
    match &mut node.data {
        EffectGraphNodeData::Gain { gain_db } => {
            if let Some(value) = args.gain_db {
                *gain_db = value;
            }
        }
        EffectGraphNodeData::Loudness { target_lufs } => {
            if let Some(value) = args.target_lufs {
                *target_lufs = value;
            }
        }
        EffectGraphNodeData::PitchShift { semitones } => {
            if let Some(value) = args.semitones {
                *semitones = value;
            }
        }
        EffectGraphNodeData::TimeStretch { rate } | EffectGraphNodeData::Speed { rate } => {
            if let Some(value) = args.rate {
                *rate = value.max(0.05);
            }
        }
        EffectGraphNodeData::MonoMix { ignored_channels } => {
            if !args.ignore_channels.is_empty() {
                ignored_channels.clear();
                ignored_channels.resize(8, false);
                for idx in &args.ignore_channels {
                    if *idx < ignored_channels.len() {
                        ignored_channels[*idx] = true;
                    }
                }
            }
        }
        EffectGraphNodeData::DebugSpectrum { mode, .. } => {
            if let Some(value) = args.spectrum_mode {
                *mode = match value {
                    CliEffectGraphSpectrumMode::Spec => EffectGraphSpectrumMode::Linear,
                    CliEffectGraphSpectrumMode::Log => EffectGraphSpectrumMode::Log,
                    CliEffectGraphSpectrumMode::Mel => EffectGraphSpectrumMode::Mel,
                };
            }
        }
        _ => {}
    }
    resolved.file.updated_at_unix_ms = now_unix_ms_local();
    save_effect_graph_file(&resolved.path, &resolved.file)?;
    effect_graph_inspect(EffectGraphInspectArgs {
        graph: EffectGraphRefArgs {
            graph: pathbuf_to_string(&resolved.path),
        },
    })
}

fn effect_graph_edge_connect(args: EffectGraphEdgeConnectArgs) -> Result<CliCommandOutput> {
    let mut resolved = load_effect_graph(&args.graph.graph)?;
    let from_node = resolved
        .file
        .graph
        .nodes
        .iter()
        .find(|node| node.id == args.from_node)
        .context("from-node not found")?;
    let to_node = resolved
        .file
        .graph
        .nodes
        .iter()
        .find(|node| node.id == args.to_node)
        .context("to-node not found")?;
    if !from_node.data.has_output_port(&args.from_port) {
        bail!("from-node does not have output port {}", args.from_port);
    }
    if !to_node.data.has_input_port(&args.to_port) {
        bail!("to-node does not have input port {}", args.to_port);
    }
    let edge_id = args.edge_id.unwrap_or_else(|| {
        unique_effect_graph_edge_id(
            &resolved.file.graph,
            &format!("{}_{}_{}_{}", args.from_node, args.from_port, args.to_node, args.to_port),
        )
    });
    resolved.file.graph.edges.push(EffectGraphEdge {
        id: edge_id,
        from_node_id: args.from_node,
        from_port_id: args.from_port,
        to_node_id: args.to_node,
        to_port_id: args.to_port,
    });
    resolved.file.updated_at_unix_ms = now_unix_ms_local();
    save_effect_graph_file(&resolved.path, &resolved.file)?;
    effect_graph_inspect(EffectGraphInspectArgs {
        graph: EffectGraphRefArgs {
            graph: pathbuf_to_string(&resolved.path),
        },
    })
}

fn effect_graph_edge_disconnect(args: EffectGraphEdgeDisconnectArgs) -> Result<CliCommandOutput> {
    let mut resolved = load_effect_graph(&args.graph.graph)?;
    resolved.file.graph.edges.retain(|edge| {
        if let Some(edge_id) = args.edge_id.as_deref() {
            return edge.id != edge_id;
        }
        let from_node_ok = args
            .from_node
            .as_deref()
            .map(|value| edge.from_node_id == value)
            .unwrap_or(true);
        let from_port_ok = args
            .from_port
            .as_deref()
            .map(|value| edge.from_port_id == value)
            .unwrap_or(true);
        let to_node_ok = args
            .to_node
            .as_deref()
            .map(|value| edge.to_node_id == value)
            .unwrap_or(true);
        let to_port_ok = args
            .to_port
            .as_deref()
            .map(|value| edge.to_port_id == value)
            .unwrap_or(true);
        !(from_node_ok && from_port_ok && to_node_ok && to_port_ok)
    });
    resolved.file.updated_at_unix_ms = now_unix_ms_local();
    save_effect_graph_file(&resolved.path, &resolved.file)?;
    effect_graph_inspect(EffectGraphInspectArgs {
        graph: EffectGraphRefArgs {
            graph: pathbuf_to_string(&resolved.path),
        },
    })
}

fn editor_inspect(args: EditorInspectArgs) -> Result<CliCommandOutput> {
    let state = resolve_editor_state(&args.source)?;
    Ok(CliCommandOutput {
        result: editor_state_json(&state),
        warnings: Vec::new(),
    })
}

fn editor_view_get(args: EditorViewGetArgs) -> Result<CliCommandOutput> {
    let state = resolve_editor_state(&args.source)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": state.display_path,
            "view_mode": format!("{:?}", state.view_mode),
            "waveform_overlay": state.waveform_overlay,
            "view_offset": state.view_offset,
            "samples_per_px": state.samples_per_px,
            "vertical_zoom": state.vertical_zoom,
            "vertical_center": state.vertical_center,
        }),
        warnings: Vec::new(),
    })
}

fn editor_view_set(args: EditorViewSetArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    if let Some(mode) = args.view_mode {
        let mode: ViewMode = mode.into();
        session.project.tabs[tab_idx].view_mode = format!("{mode:?}");
        session.project.tabs[tab_idx].primary_view =
            Some(project_primary_view_string(EditorPrimaryView::from_mode(mode)));
        session.project.tabs[tab_idx].spec_sub_view =
            Some(project_spec_sub_view_string(EditorSpecSubView::from_mode(mode)));
        session.project.tabs[tab_idx].other_sub_view = Some(project_other_sub_view_string(
            super::types::EditorOtherSubView::from_mode(mode),
        ));
    }
    if let Some(toggle) = args.waveform_overlay {
        session.project.tabs[tab_idx].show_waveform_overlay = toggle.into_bool();
    }
    if let Some(v) = args.view_offset {
        session.project.tabs[tab_idx].view_offset = v;
    }
    if let Some(v) = args.samples_per_px {
        session.project.tabs[tab_idx].samples_per_px = v.max(0.0001);
    }
    if let Some(v) = args.vertical_zoom {
        session.project.tabs[tab_idx].vertical_zoom = v.max(0.01);
    }
    if let Some(v) = args.vertical_center {
        session.project.tabs[tab_idx].vertical_view_center = v.clamp(-1.0, 1.0);
    }
    save_session(&session)?;
    editor_view_get(EditorViewGetArgs { source: args.source })
}

fn editor_selection_get(args: EditorSelectionGetArgs) -> Result<CliCommandOutput> {
    let state = resolve_editor_state(&args.source)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": state.display_path,
            "selection": state.selection,
        }),
        warnings: Vec::new(),
    })
}

fn editor_selection_set(args: EditorSelectionSetArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let total_samples = total_samples_for_session_path(&session, &target)?;
    let range = parse_optional_range(
        args.start_sample,
        args.end_sample,
        args.start_frac,
        args.end_frac,
        total_samples,
    )?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].selection = Some(range_to_array(range));
    save_session(&session)?;
    editor_selection_get(EditorSelectionGetArgs { source: args.source })
}

fn editor_selection_clear(args: EditorSelectionClearArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].selection = None;
    save_session(&session)?;
    editor_selection_get(EditorSelectionGetArgs { source: args.source })
}

fn editor_cursor_get(args: EditorCursorGetArgs) -> Result<CliCommandOutput> {
    let state = resolve_editor_state(&args.source)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": state.display_path,
            "cursor_sample": effective_cursor_sample(&state),
            "stored_cursor_sample": state.cursor_sample,
            "snap_zero_cross": state.sample_rate.is_some(),
        }),
        warnings: Vec::new(),
    })
}

fn editor_cursor_set(args: EditorCursorSetArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let total_samples = total_samples_for_session_path(&session, &target)?;
    let mut cursor = match (args.sample, args.frac) {
        (Some(sample), None) => sample.min(total_samples.saturating_sub(1)),
        (None, Some(frac)) => {
            ((frac.clamp(0.0, 1.0) * total_samples.max(1) as f32).round() as usize)
                .min(total_samples.saturating_sub(1))
        }
        _ => bail!("cursor set requires exactly one of --sample or --frac"),
    };
    if matches!(args.snap, CliCursorSnap::ZeroCross) {
        cursor = snap_cursor_to_zero_cross(&target, cursor, 0)?;
    }
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].cursor_sample = Some(cursor);
    save_session(&session)?;
    editor_cursor_get(EditorCursorGetArgs { source: args.source })
}

fn editor_cursor_nudge(args: EditorCursorNudgeArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let total_samples = total_samples_for_session_path(&session, &target)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    let state = resolve_editor_state(&args.source)?;
    let current = effective_cursor_sample(&state).unwrap_or(0);
    let mut cursor = if args.samples >= 0 {
        current.saturating_add(args.samples as usize)
    } else {
        current.saturating_sub(args.samples.unsigned_abs() as usize)
    }
    .min(total_samples.saturating_sub(1));
    if matches!(args.snap, CliCursorSnap::ZeroCross) {
        let dir = if args.samples > 0 {
            1
        } else if args.samples < 0 {
            -1
        } else {
            0
        };
        cursor = snap_cursor_to_zero_cross(&target, cursor, dir)?;
    }
    session.project.tabs[tab_idx].cursor_sample = Some(cursor);
    save_session(&session)?;
    editor_cursor_get(EditorCursorGetArgs { source: args.source })
}

fn editor_playback_play(args: EditorPlaybackPlayArgs) -> Result<CliCommandOutput> {
    let volume_linear = 10.0f32.powf(args.volume_db / 20.0).clamp(0.0, 1.0);
    let engine = crate::audio::AudioEngine::new_with_output_device_name(args.output_device.as_deref())
        .context("open playback output device")?;
    if !engine.has_output_stream() {
        bail!("output device is not available for playback");
    }
    engine.set_volume(volume_linear);
    engine.set_rate(args.rate);
    engine.set_loop_enabled(false);
    let (path, sample_rate, range, transport) = if let Some(session_path) = args.source.session.as_deref() {
        let mut workspace = CliWorkspace::load(session_path)?;
        let tab_idx = workspace.ensure_target_tab_loaded(args.source.path.as_deref())?;
        let (path, sample_rate, total_samples, selection, loop_region, dirty, channels) = {
            let tab = workspace
                .app
                .tabs
                .get(tab_idx)
                .context("missing target tab")?;
            (
                tab.path.clone(),
                tab.buffer_sample_rate.max(1),
                tab.samples_len,
                tab.selection,
                tab.loop_region,
                tab.dirty,
                tab.ch_samples.clone(),
            )
        };
        let explicit_range = parse_optional_playback_range_for_total(
            args.start_sample,
            args.end_sample,
            args.start_frac,
            args.end_frac,
            total_samples,
        )?;
        let spec = resolve_playback_range(
            total_samples,
            args.selection,
            selection,
            args.loop_range,
            loop_region,
            explicit_range,
        );
        if is_exact_stream_playback_candidate(&path, dirty) {
            play_exact_stream(&engine, &path, spec.range, args.rate)?;
            (path, sample_rate, spec.range, "exact_stream")
        } else {
            play_buffer_range(&engine, channels, sample_rate, spec.range, args.rate)?;
            (path, sample_rate, spec.range, "buffer")
        }
    } else {
        let input = args
            .source
            .input
            .as_deref()
            .context("playback requires --input or --session")?;
        let path = absolute_existing_path(input)?;
        let info = read_audio_info(&path)?;
        let total_samples = infer_total_frames(&info).context("determine total samples")?;
        let explicit_range = parse_optional_playback_range_for_total(
            args.start_sample,
            args.end_sample,
            args.start_frac,
            args.end_frac,
            total_samples,
        )?;
        let spec = resolve_playback_range(
            total_samples,
            false,
            None,
            false,
            None,
            explicit_range,
        );
        if is_exact_stream_playback_candidate(&path, false) {
            play_exact_stream(&engine, &path, spec.range, args.rate)?;
            (path, info.sample_rate.max(1), spec.range, "exact_stream")
        } else {
            let (channels, sr) = decode_audio_multi(&path)
                .with_context(|| format!("decode audio for playback: {}", path.display()))?;
            play_buffer_range(&engine, channels, sr.max(1), spec.range, args.rate)?;
            (path, sr.max(1), spec.range, "buffer")
        }
    };
    Ok(CliCommandOutput {
        result: json!({
            "path": pathbuf_to_string(&path),
            "range": range,
            "duration_secs": ((range.1.saturating_sub(range.0)) as f64 / sample_rate.max(1) as f64) / args.rate.max(0.25) as f64,
            "rate": args.rate,
            "volume_db": args.volume_db,
            "transport": transport,
            "output_device": engine.output_device_name(),
        }),
        warnings: Vec::new(),
    })
}

fn editor_tool_get(args: EditorToolGetArgs) -> Result<CliCommandOutput> {
    let state = resolve_editor_state(&args.source)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": state.display_path,
            "active_tool": format!("{:?}", state.active_tool),
            "tool_state": tool_state_json(&state.tool_state),
            "dirty": state.dirty,
        }),
        warnings: Vec::new(),
    })
}

fn editor_tool_set(args: EditorToolSetArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    let tab = &mut session.project.tabs[tab_idx];
    if let Some(tool) = args.tool {
        tab.active_tool = format!("{:?}", ToolKind::from(tool));
    }
    if let Some(value) = args.fade_in_ms {
        tab.tool_state.fade_in_ms = value.max(0.0);
    }
    if let Some(value) = args.fade_out_ms {
        tab.tool_state.fade_out_ms = value.max(0.0);
    }
    if let Some(value) = args.gain_db {
        tab.tool_state.gain_db = value;
    }
    if let Some(value) = args.normalize_target_db {
        tab.tool_state.normalize_target_db = value;
    }
    if let Some(value) = args.loudness_target_lufs {
        tab.tool_state.loudness_target_lufs = value;
    }
    if let Some(value) = args.pitch_semitones {
        tab.tool_state.pitch_semitones = value;
    }
    if let Some(value) = args.stretch_rate {
        tab.tool_state.stretch_rate = value.max(0.05);
    }
    if let Some(value) = args.loop_repeat {
        tab.tool_state.loop_repeat = value.max(2);
    }
    save_session(&session)?;
    editor_tool_get(EditorToolGetArgs { source: args.source })
}

fn editor_tool_apply(args: EditorToolApplyArgs) -> Result<CliCommandOutput> {
    let session_path = args
        .source
        .session
        .as_deref()
        .context("tool apply requires --session")?;
    let mut workspace = CliWorkspace::load(session_path)?;
    workspace.apply_tool_for_target(args.source.path.as_deref())?;
    workspace.save()?;
    editor_tool_get(EditorToolGetArgs { source: args.source })
}

fn editor_markers_list(args: EditorMarkersListArgs) -> Result<CliCommandOutput> {
    let state = resolve_editor_state(&args.source)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": state.display_path,
            "current": state.markers.iter().map(marker_json).collect::<Vec<_>>(),
            "applied": state.markers.iter().map(marker_json).collect::<Vec<_>>(),
            "committed": state.markers.iter().map(marker_json).collect::<Vec<_>>(),
            "saved": state.markers.iter().map(marker_json).collect::<Vec<_>>(),
            "dirty": state.markers_dirty,
        }),
        warnings: Vec::new(),
    })
}

fn editor_markers_add(args: EditorMarkersAddArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    let label = args.label.unwrap_or_else(|| {
        format!("M{:02}", session.project.tabs[tab_idx].markers.len().saturating_add(1))
    });
    session.project.tabs[tab_idx].markers.push(ProjectMarker {
        sample: args.sample,
        label,
    });
    session.project.tabs[tab_idx].markers.sort_by_key(|m| m.sample);
    session.project.tabs[tab_idx].markers_dirty = true;
    save_session(&session)?;
    editor_markers_list(EditorMarkersListArgs { source: args.source })
}

fn editor_markers_set(args: EditorMarkersSetArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    let markers = parse_marker_specs(&args.markers)?;
    session.project.tabs[tab_idx].markers = markers.iter().map(marker_entry_to_project).collect();
    session.project.tabs[tab_idx].markers_dirty = true;
    save_session(&session)?;
    editor_markers_list(EditorMarkersListArgs { source: args.source })
}

fn editor_markers_remove(args: EditorMarkersRemoveArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    let tab = &mut session.project.tabs[tab_idx];
    if args.index >= tab.markers.len() {
        bail!("marker index out of range: {}", args.index);
    }
    tab.markers.remove(args.index);
    tab.markers_dirty = true;
    save_session(&session)?;
    editor_markers_list(EditorMarkersListArgs { source: args.source })
}

fn editor_markers_clear(args: EditorMarkersClearArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].markers.clear();
    session.project.tabs[tab_idx].markers_dirty = true;
    save_session(&session)?;
    editor_markers_list(EditorMarkersListArgs { source: args.source })
}

fn editor_markers_apply(args: EditorMarkersApplyArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].markers_dirty = false;
    save_session(&session)?;
    editor_markers_list(EditorMarkersListArgs { source: args.source })
}

fn editor_loop_get(args: EditorLoopGetArgs) -> Result<CliCommandOutput> {
    let state = resolve_editor_state(&args.source)?;
    Ok(CliCommandOutput {
        result: editor_loop_result_json(&state),
        warnings: Vec::new(),
    })
}

fn editor_loop_set(args: EditorLoopSetArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let total_samples = total_samples_for_session_path(&session, &target)?;
    let range = parse_optional_range(
        args.start_sample,
        args.end_sample,
        args.start_frac,
        args.end_frac,
        total_samples,
    )?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].loop_region = Some(range_to_array(range));
    session.project.tabs[tab_idx].loop_mode = "Marker".to_string();
    session.project.tabs[tab_idx].loop_markers_dirty = true;
    save_session(&session)?;
    editor_loop_get(EditorLoopGetArgs { source: args.source })
}

fn editor_loop_clear(args: EditorLoopClearArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].loop_region = None;
    session.project.tabs[tab_idx].loop_markers_dirty = true;
    save_session(&session)?;
    editor_loop_get(EditorLoopGetArgs { source: args.source })
}

fn editor_loop_apply(args: EditorLoopApplyArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].loop_markers_dirty = false;
    save_session(&session)?;
    editor_loop_get(EditorLoopGetArgs { source: args.source })
}

fn editor_loop_mode(args: EditorLoopModeArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].loop_mode = format!("{:?}", LoopMode::from(args.mode));
    save_session(&session)?;
    editor_loop_get(EditorLoopGetArgs { source: args.source })
}

fn editor_loop_xfade(args: EditorLoopXfadeArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].loop_xfade_samples = args.samples;
    session.project.tabs[tab_idx].loop_xfade_shape = loop_xfade_shape_string(args.shape).to_string();
    save_session(&session)?;
    editor_loop_get(EditorLoopGetArgs { source: args.source })
}

fn editor_loop_repeat(args: EditorLoopRepeatArgs) -> Result<CliCommandOutput> {
    let mut session = load_required_editor_session(&args.source)?;
    let target = resolve_session_target_path(&session, &args.source)?;
    let tab_idx = ensure_project_tab_for_path(&mut session, &target)?;
    session.project.tabs[tab_idx].tool_state.loop_repeat = args.count.max(2);
    save_session(&session)?;
    editor_loop_get(EditorLoopGetArgs { source: args.source })
}

fn render_waveform(args: RenderWaveformArgs) -> Result<CliCommandOutput> {
    let (path, mut channels, total_samples, selection, loop_region, markers, source_kind) =
        if let Some(session_path) = args.session.as_deref() {
            let mut workspace = CliWorkspace::load(session_path)?;
            let tab_idx = workspace.ensure_target_tab_loaded(args.path.as_deref())?;
            let tab = workspace
                .app
                .tabs
                .get(tab_idx)
                .context("missing target tab for waveform render")?;
            (
                tab.path.clone(),
                tab.ch_samples.clone(),
                tab.samples_len.max(1),
                tab.selection,
                tab.loop_region,
                tab.markers.clone(),
                "session",
            )
        } else {
            let input = absolute_existing_path(
                args.input
                    .as_deref()
                    .context("render waveform requires --input or --session")?,
            )?;
            let info = read_audio_info(&input)?;
            let (channels, _) = decode_audio_multi(&input)
                .with_context(|| format!("decode waveform source: {}", input.display()))?;
            let total = infer_total_frames(&info)
                .unwrap_or_else(|| channels.iter().map(Vec::len).max().unwrap_or(1));
            (
                input.clone(),
                channels,
                total,
                None,
                read_loop_range_usize(&input),
                read_markers_in_file_space(&input, &info)?,
                "input",
            )
        };
    if args.mixdown {
        channels = vec![mixdown_channels(&channels)];
    }
    let explicit_range = parse_optional_playback_range_for_total(
        args.start_sample,
        args.end_sample,
        args.start_frac,
        args.end_frac,
        total_samples,
    )?;
    let range_spec = resolve_playback_range(
        total_samples,
        args.selection,
        selection,
        args.loop_range,
        loop_region,
        explicit_range,
    );
    let window_channels = slice_channels_for_range(&channels, range_spec.range);
    let markers = if args.show_markers {
        normalize_markers_for_range(&markers, range_spec.range)
    } else {
        Vec::new()
    };
    let loop_overlay = if args.show_loop {
        normalize_loop_for_range(loop_region, range_spec.range)
    } else {
        None
    };
    let image = draw_waveform_image(
        &window_channels,
        range_spec.range.1.saturating_sub(range_spec.range.0),
        args.width.max(16),
        args.height.max(16),
        markers,
        loop_overlay,
    );
    let output = prepare_output_path(args.output, "waveform", "png")?;
    save_rgba_image(&image, &output)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": absolute_string(&output)?,
            "width": image.width(),
            "height": image.height(),
            "source": pathbuf_to_string(&path),
            "view_params": {
                "mixdown": args.mixdown,
                "channels_rendered": window_channels.len(),
                "range": range_spec.range,
                "range_source": format!("{:?}", range_spec.source),
                "show_markers": args.show_markers,
                "show_loop": args.show_loop,
                "source_kind": source_kind,
            },
        }),
        warnings: Vec::new(),
    })
}

fn render_spectrum(args: RenderSpectrumArgs) -> Result<CliCommandOutput> {
    let input = absolute_existing_path(&args.input)?;
    let (mono, sr) = decode_audio_mono(&input)
        .with_context(|| format!("decode mono for spectrum: {}", input.display()))?;
    let cfg = SpectrogramConfig::default();
    let params = spectrogram::spectrogram_params(mono.len(), &cfg);
    let spec = SpectrogramData {
        frames: params.frames,
        bins: params.bins,
        frame_step: params.frame_step,
        sample_rate: sr,
        values_db: spectrogram::compute_spectrogram_tile(&mono, sr, &params, 0, params.frames),
    };
    let view_mode: ViewMode = args.view_mode.into();
    let image = WavesPreviewer::render_spectral_viewport_image(
        &[spec],
        &[0],
        args.width.max(16) as usize,
        args.height.max(16) as usize,
        1,
        0,
        mono.len(),
        1.0,
        0.0,
        &cfg,
        view_mode,
        super::types::EditorViewportRenderQuality::Fine,
    );
    let output = prepare_output_path(args.output, "spectrum", "png")?;
    save_color_image(&image, &output)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": absolute_string(&output)?,
            "width": image.size[0],
            "height": image.size[1],
            "source": pathbuf_to_string(&input),
            "view_params": {
                "view_mode": spectral_mode_name(args.view_mode),
                "sample_rate": sr,
            },
        }),
        warnings: Vec::new(),
    })
}

fn render_editor(args: RenderEditorArgs) -> Result<CliCommandOutput> {
    let mut warnings = Vec::new();
    if args.include_inspector {
        warnings.push(
            "render editor captures the full editor window; inspector inclusion follows the default layout state".to_string(),
        );
    }
    let temp_session = build_editor_render_session(&args)?;
    let output = prepare_output_path(args.output, "editor", "png")?;
    let abs_output = render_gui_session_screenshot(
        &temp_session,
        &output,
        args.view_mode.map(Into::into),
        args.waveform_overlay.map(|t| t.into_bool()),
        true,
    )?;
    let (width, height) = image_dimensions(&abs_output)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": absolute_string(&abs_output)?,
            "width": width,
            "height": height,
            "session": absolute_string(&temp_session)?,
            "view_params": {
                "view_mode": args.view_mode.map(view_mode_name),
                "waveform_overlay": args.waveform_overlay.map(CliToggle::into_bool),
                "include_inspector": args.include_inspector,
            },
        }),
        warnings,
    })
}

fn render_list(args: RenderListArgs) -> Result<CliCommandOutput> {
    list_render(ListRenderArgs {
        source: args.source,
        output: args.output,
        columns: args.columns,
        offset: args.offset,
        limit: args.limit,
        show_markers: true,
        show_loop: true,
    })
}

fn export_file(args: ExportFileArgs) -> Result<CliCommandOutput> {
    match (args.input.as_deref(), args.session.as_deref()) {
        (Some(input), None) => export_file_from_input(input, &args),
        (None, Some(session_path)) => export_file_from_session(session_path, &args),
        _ => bail!("exactly one of --input or --session is required"),
    }
}

fn export_verify_loop_tags(args: ExportVerifyLoopTagsArgs) -> Result<CliCommandOutput> {
    let input = absolute_existing_path(&args.input)?;
    let info = read_audio_info(&input)?;
    let markers = read_markers_in_file_space(&input, &info)?;
    let loop_region = read_loop_range_usize(&input);
    Ok(CliCommandOutput {
        result: json!({
            "path": pathbuf_to_string(&input),
            "format": input.extension().and_then(|ext| ext.to_str()).unwrap_or_default(),
            "sample_rate": info.sample_rate,
            "markers": markers.iter().map(marker_json).collect::<Vec<_>>(),
            "marker_count": markers.len(),
            "loop_region": loop_region,
            "has_loop_region": loop_region.is_some(),
        }),
        warnings: Vec::new(),
    })
}

fn export_file_from_input(input: &Path, args: &ExportFileArgs) -> Result<CliCommandOutput> {
    if args.overwrite {
        bail!("--overwrite is only supported with --session");
    }
    let output = args
        .output
        .as_deref()
        .context("direct export requires --output")?;
    let input = absolute_existing_path(input)?;
    let output = absolute_output_path(output)?;
    ensure_parent_dir(&output)?;
    if let Some(gain_db) = args.gain_db {
        wave::export_gain_audio(&input, &output, gain_db).with_context(|| {
            format!("export gain audio: {} -> {}", input.display(), output.display())
        })?;
    } else {
        std::fs::copy(&input, &output).with_context(|| {
            format!("copy source audio: {} -> {}", input.display(), output.display())
        })?;
    }
    let src_info = read_audio_info(&input)?;
    let dst_info = read_audio_info(&output)?;
    let markers = if args.markers.is_empty() {
        read_markers_in_file_space(&input, &src_info)?
    } else {
        parse_marker_specs(&args.markers)?
    };
    let loop_region = parse_loop_override(args.loop_start_sample, args.loop_end_sample)?
        .or_else(|| read_loop_range_usize(&input));
    markers::write_markers(
        &output,
        dst_info.sample_rate.max(1),
        dst_info.sample_rate.max(1),
        &markers,
    )?;
    loop_markers::write_loop_markers(
        &output,
        loop_region.map(|(start, end)| (start as u64, end as u64)),
    )?;
    let verified_marker_count = read_markers_in_file_space(&output, &dst_info)?.len();
    let verified_loop_region = read_loop_range_usize(&output);
    Ok(CliCommandOutput {
        result: json!({
            "source": pathbuf_to_string(&input),
            "destination": absolute_string(&output)?,
            "mode": "new_file",
            "saved_markers": markers.iter().map(marker_json).collect::<Vec<_>>(),
            "saved_loop": loop_region,
            "loop_verification": {
                "marker_count": verified_marker_count,
                "loop_region": verified_loop_region,
            },
        }),
        warnings: Vec::new(),
    })
}

fn export_file_from_session(session_path: &Path, args: &ExportFileArgs) -> Result<CliCommandOutput> {
    if args.overwrite == args.output.is_some() {
        bail!("session export requires exactly one of --overwrite or --output");
    }
    let marker_override = if args.markers.is_empty() {
        None
    } else {
        Some(parse_marker_specs(&args.markers)?)
    };
    let loop_override = parse_loop_override(args.loop_start_sample, args.loop_end_sample)?;
    let mut workspace = CliWorkspace::load(session_path)?;
    let (src, dst, markers, loop_region) = workspace.export_target(
        args.path.as_deref(),
        args.output.as_deref(),
        args.overwrite,
        args.gain_db,
        marker_override,
        loop_override,
    )?;
    let verified_marker_count = {
        let info = read_audio_info(&dst)?;
        read_markers_in_file_space(&dst, &info)?.len()
    };
    let verified_loop_region = read_loop_range_usize(&dst);
    Ok(CliCommandOutput {
        result: json!({
            "source": pathbuf_to_string(&src),
            "destination": pathbuf_to_string(&dst),
            "mode": if args.overwrite { "overwrite" } else { "new_file" },
            "saved_markers": markers.iter().map(marker_json).collect::<Vec<_>>(),
            "saved_loop": loop_region,
            "loop_verification": {
                "marker_count": verified_marker_count,
                "loop_region": verified_loop_region,
            },
        }),
        warnings: Vec::new(),
    })
}

fn debug_summary(args: DebugSummaryArgs) -> Result<CliCommandOutput> {
    let temp_summary = prepare_output_path(None, "debug-summary", "txt")?;
    let (session_path, open_first) = debug_session_target(&args.source)?;
    let summary_path = render_gui_debug_summary(&session_path, &temp_summary, open_first)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": absolute_string(&summary_path)?,
            "summary": std::fs::read_to_string(&summary_path)
                .with_context(|| format!("read summary: {}", summary_path.display()))?,
        }),
        warnings: Vec::new(),
    })
}

fn debug_screenshot(args: DebugScreenshotArgs) -> Result<CliCommandOutput> {
    render_editor(RenderEditorArgs {
        source: args.source,
        output: args.output,
        view_mode: args.view_mode,
        waveform_overlay: args.waveform_overlay,
        include_inspector: false,
    })
}

fn load_session(path: &Path) -> Result<LoadedSession> {
    let path = absolute_existing_path(path)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read session file: {}", path.display()))?;
    let project = deserialize_project(&text)
        .with_context(|| format!("parse session file: {}", path.display()))?;
    let base_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(LoadedSession {
        path,
        base_dir,
        project,
    })
}

fn save_session(session: &LoadedSession) -> Result<()> {
    write_project_file(&session.path, &session.project)
}

fn write_project_file(path: &Path, project: &ProjectFile) -> Result<()> {
    ensure_parent_dir(path)?;
    let text = serialize_project(project).context("serialize session file")?;
    std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn write_temp_project_file(prefix: &str, project: &ProjectFile) -> Result<PathBuf> {
    let dir = cli_render_dir()?.join("sessions");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}_{}.nwsess", prefix, timestamp_token()));
    write_project_file(&path, project)?;
    Ok(path)
}

fn session_entries_from_sources(
    folder: Option<&Path>,
    inputs: &[PathBuf],
) -> Result<Vec<SessionListEntry>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    if let Some(folder) = folder {
        for path in scan_audio_paths(folder)? {
            if seen.insert(path_key(&path)) {
                out.push(SessionListEntry {
                    path,
                    pending_gain_db: 0.0,
                });
            }
        }
    }
    for input in inputs {
        let path = absolute_existing_path(input)?;
        if path.is_dir() {
            for nested in scan_audio_paths(&path)? {
                if seen.insert(path_key(&nested)) {
                    out.push(SessionListEntry {
                        path: nested,
                        pending_gain_db: 0.0,
                    });
                }
            }
        } else if is_supported_audio_path(&path) && seen.insert(path_key(&path)) {
            out.push(SessionListEntry {
                path,
                pending_gain_db: 0.0,
            });
        }
    }
    out.sort_by_key(|entry| path_key(&entry.path));
    Ok(out)
}

fn build_project_file_from_entries(entries: &[SessionListEntry]) -> Result<ProjectFile> {
    let cols = parse_list_column_config(DEFAULT_LIST_COLUMNS)?;
    Ok(ProjectFile {
        version: 1,
        name: None,
        base_dir: None,
        list: ProjectList {
            root: None,
            files: entries
                .iter()
                .map(|entry| entry.path.to_string_lossy().to_string())
                .collect(),
            items: entries
                .iter()
                .map(|entry| ProjectListItem {
                    path: entry.path.to_string_lossy().to_string(),
                    pending_gain_db: entry.pending_gain_db,
                })
                .collect(),
            sample_rate_overrides: Vec::new(),
            bit_depth_overrides: Vec::new(),
            format_overrides: Vec::new(),
            virtual_items: Vec::new(),
            transcript_languages: Vec::new(),
        },
        app: ProjectApp {
            theme: "Dark".to_string(),
            sort_key: "File".to_string(),
            sort_dir: "Asc".to_string(),
            search_query: String::new(),
            search_regex: false,
            selected_path: None,
            list_columns: project_list_columns_from_config(cols),
            auto_play_list_nav: false,
            export_policy: Some(ProjectExportPolicy {
                save_mode: "new_file".to_string(),
                conflict: "rename".to_string(),
                backup_bak: true,
                export_srt: false,
                name_template: "{name} (gain{gain:+.1}dB)".to_string(),
                dest_folder: None,
            }),
            external_state: None,
            effect_graph_ui: None,
            transcript_ai_config: None,
        },
        spectrogram: super::project::project_spectrogram_from_cfg(&SpectrogramConfig::default()),
        tabs: Vec::new(),
        active_tab: None,
        cached_edits: Vec::new(),
    })
}

fn session_list_entries(session: &LoadedSession) -> Vec<SessionListEntry> {
    let mut gains = HashMap::new();
    for item in &session.project.list.items {
        gains.insert(
            path_key(&project::resolve_path(&item.path, &session.base_dir)),
            item.pending_gain_db,
        );
    }
    let raws: Vec<String> = if !session.project.list.items.is_empty() {
        session
            .project
            .list
            .items
            .iter()
            .map(|item| item.path.clone())
            .collect()
    } else {
        session.project.list.files.clone()
    };
    raws.into_iter()
        .map(|raw| {
            let path = project::resolve_path(&raw, &session.base_dir);
            let pending_gain_db = gains.get(&path_key(&path)).copied().unwrap_or(0.0);
            SessionListEntry {
                path,
                pending_gain_db,
            }
        })
        .collect()
}

fn load_required_editor_session(source: &EditorSourceArgs) -> Result<LoadedSession> {
    let session_path = source
        .session
        .as_deref()
        .context("editor mutations require --session")?;
    load_session(session_path)
}

fn find_project_tab_index(session: &LoadedSession, target: &Path) -> Option<usize> {
    let target_key = path_key(target);
    session.project.tabs.iter().position(|tab| {
        path_key(&project::resolve_path(&tab.path, &session.base_dir)) == target_key
    })
}

fn resolve_session_target_path(
    session: &LoadedSession,
    source: &EditorSourceArgs,
) -> Result<PathBuf> {
    if let Some(path) = source.path.as_deref() {
        return absolute_output_path(path);
    }
    if let Some(idx) = session.project.active_tab {
        if let Some(tab) = session.project.tabs.get(idx) {
            return Ok(project::resolve_path(&tab.path, &session.base_dir));
        }
    }
    if let Some(tab) = session.project.tabs.first() {
        return Ok(project::resolve_path(&tab.path, &session.base_dir));
    }
    if let Some(entry) = session_list_entries(session).first() {
        return Ok(entry.path.clone());
    }
    bail!("session does not contain any target audio")
}

fn default_project_tab_for_path(path: &Path, session_base: &Path) -> Result<ProjectTab> {
    let info = read_audio_info(path)?;
    Ok(ProjectTab {
        path: project::rel_path(path, session_base),
        primary_view: Some("wave".to_string()),
        spec_sub_view: Some("spec".to_string()),
        other_sub_view: Some("tempogram".to_string()),
        view_mode: "Waveform".to_string(),
        show_waveform_overlay: false,
        channel_view: ProjectChannelView {
            mode: "mixdown".to_string(),
            selected: Vec::new(),
        },
        active_tool: "LoopEdit".to_string(),
        tool_state: ProjectToolState {
            fade_in_ms: 0.0,
            fade_out_ms: 0.0,
            gain_db: 0.0,
            normalize_target_db: -6.0,
            loudness_target_lufs: -14.0,
            pitch_semitones: 0.0,
            stretch_rate: 1.0,
            loop_repeat: 2,
        },
        bpm_enabled: false,
        bpm_value: 0.0,
        bpm_user_set: false,
        bpm_offset_sec: 0.0,
        preview_tool: None,
        preview_audio: None,
        loop_mode: "Off".to_string(),
        loop_region: None,
        loop_xfade_samples: 0,
        loop_xfade_shape: "linear".to_string(),
        trim_range: None,
        selection: None,
        cursor_sample: None,
        markers: Vec::new(),
        markers_dirty: false,
        loop_markers_dirty: false,
        fade_in_range: None,
        fade_out_range: None,
        fade_in_shape: "SCurve".to_string(),
        fade_out_shape: "SCurve".to_string(),
        snap_zero_cross: false,
        view_offset: 0,
        samples_per_px: (info.sample_rate.max(1) as f32 / 120.0).max(1.0),
        vertical_zoom: 1.0,
        vertical_view_center: 0.0,
        dirty: false,
        buffer_sample_rate: Some(info.sample_rate.max(1)),
        edited_audio: None,
        plugin_fx_draft: ProjectPluginFxDraft::default(),
        music_analysis: None,
    })
}

fn ensure_project_tab_for_path(session: &mut LoadedSession, target: &Path) -> Result<usize> {
    if let Some(idx) = find_project_tab_index(session, target) {
        session.project.active_tab = Some(idx);
        return Ok(idx);
    }
    let tab = default_project_tab_for_path(target, &session.base_dir)?;
    session.project.tabs.push(tab);
    let idx = session.project.tabs.len().saturating_sub(1);
    session.project.active_tab = Some(idx);
    Ok(idx)
}

enum ListSourceLoaded {
    Session(LoadedSession),
    Folder(Vec<SessionListEntry>, PathBuf),
}

fn load_list_source(source: &ListSourceArgs) -> Result<ListSourceLoaded> {
    match (source.folder.as_deref(), source.session.as_deref()) {
        (Some(folder), None) => Ok(ListSourceLoaded::Folder(
            session_entries_from_sources(Some(folder), &[])?,
            absolute_existing_path(folder)?,
        )),
        (None, Some(session)) => Ok(ListSourceLoaded::Session(load_session(session)?)),
        _ => bail!("exactly one of --folder or --session is required"),
    }
}

fn render_source_json_for_list(source: &ListSourceArgs) -> Result<Value> {
    match (source.folder.as_deref(), source.session.as_deref()) {
        (Some(folder), None) => Ok(json!({"kind": "folder", "path": absolute_string(folder)?})),
        (None, Some(session)) => Ok(json!({"kind": "session", "path": absolute_string(session)?})),
        _ => bail!("exactly one of --folder or --session is required"),
    }
}

fn list_rows_from_source(
    source: &ListSourceLoaded,
    include_overlays: bool,
) -> Result<Vec<Map<String, Value>>> {
    match source {
        ListSourceLoaded::Session(session) => session_list_entries(session)
            .into_iter()
            .map(|entry| list_row_for_entry(&entry, Some(session), include_overlays))
            .collect(),
        ListSourceLoaded::Folder(entries, _) => entries
            .iter()
            .map(|entry| list_row_for_entry(entry, None, include_overlays))
            .collect(),
    }
}

fn list_row_for_entry(
    entry: &SessionListEntry,
    session: Option<&LoadedSession>,
    include_overlays: bool,
) -> Result<Map<String, Value>> {
    let path = absolute_output_path(&entry.path)?;
    let info = read_audio_info(&path).ok();
    let mut row = Map::new();
    row.insert("row_id".to_string(), json!(stable_row_id_for_path(&path)));
    row.insert("path".to_string(), Value::String(pathbuf_to_string(&path)));
    row.insert(
        "file".to_string(),
        Value::String(
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
        ),
    );
    row.insert(
        "folder".to_string(),
        Value::String(
            path.parent()
                .map(|parent| parent.to_string_lossy().to_string())
                .unwrap_or_default(),
        ),
    );
    row.insert("gain".to_string(), json!(entry.pending_gain_db));
    if let Some(info) = info.as_ref() {
        row.insert("length".to_string(), json!(info.duration_secs));
        row.insert("channels".to_string(), json!(info.channels));
        row.insert("sample_rate".to_string(), json!(info.sample_rate));
        row.insert("bits".to_string(), json!(info.bits_per_sample));
        row.insert("bit_rate".to_string(), json!(info.bit_rate_bps));
        row.insert("peak".to_string(), Value::Null);
        row.insert("lufs".to_string(), Value::Null);
        row.insert("bpm".to_string(), Value::Null);
        row.insert("created_at".to_string(), system_time_json(info.created_at));
        row.insert("modified_at".to_string(), system_time_json(info.modified_at));
    }
    row.insert("wave".to_string(), json!(true));
    if include_overlays {
        if let Some(overlay) = resolve_list_overlay(session, &path, info.as_ref())? {
            row.insert(
                "overlay".to_string(),
                json!({
                    "source": overlay.source,
                    "markers": overlay.markers,
                    "loop": overlay.loop_region,
                }),
            );
        }
    }
    Ok(row)
}

fn project_for_list_render(
    source: ListSourceLoaded,
    columns: &str,
    offset: usize,
    limit: Option<usize>,
) -> Result<ProjectFile> {
    let column_config = parse_list_column_config(columns)?;
    match source {
        ListSourceLoaded::Session(mut session) => {
            let entries = slice_entries(session_list_entries(&session), offset, limit);
            session.project.list.files = entries
                .iter()
                .map(|entry| project::rel_path(&entry.path, &session.base_dir))
                .collect();
            session.project.list.items = entries
                .iter()
                .map(|entry| ProjectListItem {
                    path: project::rel_path(&entry.path, &session.base_dir),
                    pending_gain_db: entry.pending_gain_db,
                })
                .collect();
            session.project.app.list_columns = project_list_columns_from_config(column_config);
            session.project.active_tab = None;
            Ok(session.project)
        }
        ListSourceLoaded::Folder(entries, folder) => {
            let entries = slice_entries(entries, offset, limit);
            let mut project = build_project_file_from_entries(&entries)?;
            project.list.root = Some(folder.to_string_lossy().to_string());
            project.app.list_columns = project_list_columns_from_config(column_config);
            Ok(project)
        }
    }
}

fn resolve_editor_state(source: &EditorSourceArgs) -> Result<EditorTargetState> {
    if let Some(input) = source.input.as_deref() {
        return resolve_editor_state_for_input(input);
    }
    let session = load_required_editor_session(source)?;
    let target = resolve_session_target_path(&session, source)?;
    let info = read_audio_info(&target).ok();
    let loop_from_file = info
        .as_ref()
        .and_then(|info| normalized_loop_samples(&target, info));
    let markers_from_file = if let Some(info) = info.as_ref() {
        read_markers_in_file_space(&target, info)?
    } else {
        Vec::new()
    };
    let Some(tab_idx) = find_project_tab_index(&session, &target) else {
        return resolve_editor_state_for_input(&target);
    };
    let tab = &session.project.tabs[tab_idx];
    let (primary, spec, other) = primary_view_from_project(
        tab.primary_view.as_deref(),
        tab.spec_sub_view.as_deref(),
        tab.other_sub_view.as_deref(),
        &tab.view_mode,
    );
    let view_mode = match primary {
        EditorPrimaryView::Wave => ViewMode::Waveform,
        EditorPrimaryView::Spec => spec.to_mode(),
        EditorPrimaryView::Other => other.to_mode(),
    };
    let markers = if tab.markers.is_empty() {
        markers_from_file
    } else {
        tab.markers.iter().map(project_marker_to_entry).collect()
    };
    let loop_current = tab.loop_region.map(array_to_range).or(loop_from_file);
    Ok(EditorTargetState {
        path: target.clone(),
        display_path: pathbuf_to_string(&target),
        total_samples: info.as_ref().and_then(infer_total_frames),
        sample_rate: info.as_ref().map(|info| info.sample_rate),
        view_mode,
        waveform_overlay: tab.show_waveform_overlay,
        view_offset: tab.view_offset,
        samples_per_px: tab.samples_per_px,
        vertical_zoom: tab.vertical_zoom,
        vertical_center: tab.vertical_view_center,
        cursor_sample: tab.cursor_sample,
        selection: tab.selection.map(array_to_range),
        markers,
        loop_current,
        loop_applied: loop_current,
        loop_committed: loop_current,
        loop_mode: loop_mode_from_str(&tab.loop_mode),
        loop_xfade_samples: tab.loop_xfade_samples,
        loop_xfade_shape: super::project::loop_shape_from_str(&tab.loop_xfade_shape),
        active_tool: super::project::tool_kind_from_str(&tab.active_tool),
        tool_state: project_tool_state_to_tool_state(&tab.tool_state),
        dirty: tab.dirty,
        markers_dirty: tab.markers_dirty,
        loop_dirty: tab.loop_markers_dirty,
    })
}

fn resolve_editor_state_for_input(input: &Path) -> Result<EditorTargetState> {
    let path = absolute_existing_path(input)?;
    let info = read_audio_info(&path)?;
    let markers = read_markers_in_file_space(&path, &info)?;
    let loop_region = read_loop_range_usize(&path);
    Ok(EditorTargetState {
        path: path.clone(),
        display_path: pathbuf_to_string(&path),
        total_samples: infer_total_frames(&info),
        sample_rate: Some(info.sample_rate),
        view_mode: ViewMode::Waveform,
        waveform_overlay: false,
        view_offset: 0,
        samples_per_px: (info.sample_rate.max(1) as f32 / 120.0).max(1.0),
        vertical_zoom: 1.0,
        vertical_center: 0.0,
        cursor_sample: None,
        selection: None,
        markers,
        loop_current: loop_region,
        loop_applied: loop_region,
        loop_committed: loop_region,
        loop_mode: if loop_region.is_some() {
            LoopMode::Marker
        } else {
            LoopMode::Off
        },
        loop_xfade_samples: 0,
        loop_xfade_shape: LoopXfadeShape::Linear,
        active_tool: ToolKind::LoopEdit,
        tool_state: ToolState {
            fade_in_ms: 0.0,
            fade_out_ms: 0.0,
            gain_db: 0.0,
            normalize_target_db: -6.0,
            loudness_target_lufs: -14.0,
            pitch_semitones: 0.0,
            stretch_rate: 1.0,
            loop_repeat: 2,
        },
        dirty: false,
        markers_dirty: false,
        loop_dirty: false,
    })
}

fn editor_state_json(state: &EditorTargetState) -> Value {
    json!({
        "path": state.display_path,
        "file_meta": {
            "sample_rate": state.sample_rate,
            "total_samples": state.total_samples,
        },
        "view": {
            "view_mode": format!("{:?}", state.view_mode),
            "waveform_overlay": state.waveform_overlay,
            "view_offset": state.view_offset,
            "samples_per_px": state.samples_per_px,
            "vertical_zoom": state.vertical_zoom,
            "vertical_center": state.vertical_center,
        },
        "cursor": {
            "current": effective_cursor_sample(state),
            "stored": state.cursor_sample,
        },
        "selection": state.selection,
        "markers": state.markers.iter().map(marker_json).collect::<Vec<_>>(),
        "loop": {
            "current": state.loop_current,
            "applied": state.loop_applied,
            "committed": state.loop_committed,
            "mode": format!("{:?}", state.loop_mode),
            "xfade": {
                "samples": state.loop_xfade_samples,
                "shape": format!("{:?}", state.loop_xfade_shape),
            }
        },
        "tool": {
            "active": format!("{:?}", state.active_tool),
            "state": tool_state_json(&state.tool_state)
        },
        "dirty": {
            "tab": state.dirty,
            "markers": state.markers_dirty,
            "loop": state.loop_dirty,
        }
    })
}

fn tool_state_json(state: &ToolState) -> Value {
    json!({
        "fade_in_ms": state.fade_in_ms,
        "fade_out_ms": state.fade_out_ms,
        "gain_db": state.gain_db,
        "normalize_target_db": state.normalize_target_db,
        "loudness_target_lufs": state.loudness_target_lufs,
        "pitch_semitones": state.pitch_semitones,
        "stretch_rate": state.stretch_rate,
        "loop_repeat": state.loop_repeat,
    })
}

fn editor_loop_result_json(state: &EditorTargetState) -> Value {
    json!({
        "path": state.display_path,
        "current": state.loop_current,
        "applied": state.loop_applied,
        "committed": state.loop_committed,
        "saved": state.loop_committed,
        "dirty": state.loop_dirty,
        "mode": format!("{:?}", state.loop_mode),
        "xfade": {
            "samples": state.loop_xfade_samples,
            "shape": format!("{:?}", state.loop_xfade_shape),
        }
    })
}

fn resolve_list_overlay(
    session: Option<&LoadedSession>,
    path: &Path,
    info: Option<&AudioInfo>,
) -> Result<Option<NormalizedOverlay>> {
    if let Some(session) = session {
        if let Some(idx) = find_project_tab_index(session, path) {
            let tab = &session.project.tabs[idx];
            let total = info.and_then(infer_total_frames).unwrap_or(1).max(1);
            return Ok(Some(NormalizedOverlay {
                markers: tab
                    .markers
                    .iter()
                    .map(|marker| normalize_sample(marker.sample, total))
                    .collect(),
                loop_region: tab.loop_region.map(array_to_range).map(|(start, end)| {
                    (normalize_sample(start, total), normalize_sample(end, total))
                }),
                source: "tab".to_string(),
            }));
        }
    }
    if let Some(info) = info {
        return Ok(Some(NormalizedOverlay {
            markers: normalized_markers(path, info)?,
            loop_region: normalized_loop(path, info),
            source: "file".to_string(),
        }));
    }
    Ok(None)
}

fn apply_list_query_filter_sort(
    rows: &mut Vec<Map<String, Value>>,
    query: Option<&str>,
    sort_key: Option<&str>,
    sort_dir: Option<&str>,
) {
    if let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) {
        let query = query.to_ascii_lowercase();
        rows.retain(|row| {
            row.get("file")
                .and_then(Value::as_str)
                .map(|value| value.to_ascii_lowercase().contains(&query))
                .unwrap_or(false)
                || row
                    .get("folder")
                    .and_then(Value::as_str)
                    .map(|value| value.to_ascii_lowercase().contains(&query))
                    .unwrap_or(false)
        });
    }
    if let Some(sort_key) = sort_key {
        rows.sort_by(|a, b| compare_row_values(a.get(sort_key), b.get(sort_key)));
        if sort_dir
            .map(|dir| dir.eq_ignore_ascii_case("desc"))
            .unwrap_or(false)
        {
            rows.reverse();
        }
    }
}

fn compare_row_values(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(Value::String(a)), Some(Value::String(b))) => a.cmp(b),
        (Some(Value::Number(a)), Some(Value::Number(b))) => a
            .as_f64()
            .partial_cmp(&b.as_f64())
            .unwrap_or(std::cmp::Ordering::Equal),
        (Some(Value::Bool(a)), Some(Value::Bool(b))) => a.cmp(b),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        _ => std::cmp::Ordering::Equal,
    }
}

fn resolve_query_filter(filter: &crate::cli::CliQueryFilterArgs) -> Result<ResolvedQueryFilter> {
    let spec = if let Some(query_id) = filter.query_id.as_deref() {
        if filter.query.is_some() || filter.sort_key.is_some() || filter.sort_dir.is_some() {
            bail!("--query-id cannot be combined with --query/--sort-key/--sort-dir");
        }
        decode_query_handle_spec(query_id)?
    } else {
        QueryHandleSpec {
            query: filter.query.clone().filter(|value| !value.trim().is_empty()),
            sort_key: filter.sort_key.clone().filter(|value| !value.trim().is_empty()),
            sort_dir: filter.sort_dir.clone().filter(|value| !value.trim().is_empty()),
        }
    };
    Ok(ResolvedQueryFilter {
        query: spec.query.clone(),
        sort_key: spec.sort_key.clone(),
        sort_dir: spec.sort_dir.clone(),
        query_id: encode_query_handle_spec(&spec)?,
    })
}

fn encode_query_handle_spec(spec: &QueryHandleSpec) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(spec).context("serialize query handle")?,
    ))
}

fn decode_query_handle_spec(raw: &str) -> Result<QueryHandleSpec> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .context("decode query handle")?;
    serde_json::from_slice(&bytes).context("parse query handle")
}

fn matched_session_entries(
    session: &LoadedSession,
    filter: &ResolvedQueryFilter,
) -> Result<Vec<SessionListEntry>> {
    let entries = session_list_entries(session);
    let mut rows = entries
        .iter()
        .map(|entry| list_row_for_entry(entry, Some(session), false))
        .collect::<Result<Vec<_>>>()?;
    apply_list_query_filter_sort(
        &mut rows,
        filter.query.as_deref(),
        filter.sort_key.as_deref(),
        filter.sort_dir.as_deref(),
    );
    let entry_map: HashMap<String, SessionListEntry> = entries
        .into_iter()
        .map(|entry| (path_key(&entry.path), entry))
        .collect();
    let mut out = Vec::new();
    for row in rows {
        if let Some(path) = row.get("path").and_then(Value::as_str) {
            if let Some(entry) = entry_map.get(&path_key(Path::new(path))) {
                out.push(entry.clone());
            }
        }
    }
    Ok(out)
}

fn pending_gain_for_session_path(session: &LoadedSession, path: &Path) -> f32 {
    let key = path_key(path);
    session
        .project
        .list
        .items
        .iter()
        .find_map(|item| {
            let item_path = project::resolve_path(&item.path, &session.base_dir);
            (path_key(&item_path) == key).then_some(item.pending_gain_db)
        })
        .unwrap_or(0.0)
}

fn set_pending_gain_for_session_path(session: &mut LoadedSession, path: &Path, gain_db: f32) {
    let key = path_key(path);
    for item in &mut session.project.list.items {
        let item_path = project::resolve_path(&item.path, &session.base_dir);
        if path_key(&item_path) == key {
            item.pending_gain_db = gain_db;
            return;
        }
    }
    session.project.list.items.push(ProjectListItem {
        path: project::rel_path(path, &session.base_dir),
        pending_gain_db: gain_db,
    });
}

fn session_pending_gain_map(session: &LoadedSession) -> Value {
    let mut map = Map::new();
    for entry in session_list_entries(session) {
        map.insert(pathbuf_to_string(&entry.path), json!(entry.pending_gain_db));
    }
    Value::Object(map)
}

fn build_batch_loudness_rows(
    session: &LoadedSession,
    filter: &ResolvedQueryFilter,
    target_lufs: f32,
) -> Result<Vec<BatchLoudnessRow>> {
    let entries = matched_session_entries(session, filter)?;
    entries
        .into_iter()
        .map(|entry| build_batch_loudness_row(entry, target_lufs))
        .collect()
}

fn build_batch_loudness_row(entry: SessionListEntry, target_lufs: f32) -> Result<BatchLoudnessRow> {
    let path = absolute_output_path(&entry.path)?;
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let folder = path
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default();
    let (measured_lufs, raw_peak_db, warning) = measure_file_loudness_and_peak(&path);
    let effective_lufs = measured_lufs.map(|value| value + entry.pending_gain_db);
    let estimated_gain_db = effective_lufs.map(|value| target_lufs - value);
    let proposed_gain_db = estimated_gain_db.map(|value| entry.pending_gain_db + value);
    let clipping_risk = match (raw_peak_db, proposed_gain_db) {
        (Some(peak), Some(gain)) => peak + gain > 0.0,
        _ => false,
    };
    Ok(BatchLoudnessRow {
        row_id: stable_row_id_for_path(&path),
        path: pathbuf_to_string(&path),
        file,
        folder,
        measured_lufs,
        raw_peak_db,
        existing_gain_db: entry.pending_gain_db,
        effective_lufs,
        target_lufs,
        estimated_gain_db,
        proposed_gain_db,
        clipping_risk,
        warning,
    })
}

fn measure_file_loudness_and_peak(path: &Path) -> (Option<f32>, Option<f32>, Option<String>) {
    let (channels, sample_rate) = match decode_audio_multi(path) {
        Ok(value) => value,
        Err(err) => {
            return (
                None,
                None,
                Some(format!("decode failed: {err}")),
            )
        }
    };
    let peak = channels
        .iter()
        .flat_map(|channel| channel.iter().copied())
        .fold(0.0f32, |acc, sample| acc.max(sample.abs()));
    let peak_db = (peak > 0.0).then_some(20.0 * peak.log10());
    match wave::lufs_integrated_from_multi(&channels, sample_rate.max(1)) {
        Ok(lufs) if lufs.is_finite() => (Some(lufs), peak_db, None),
        Ok(_) => (
            None,
            peak_db,
            Some("integrated loudness was not finite".to_string()),
        ),
        Err(err) => (
            None,
            peak_db,
            Some(format!("loudness measurement failed: {err}")),
        ),
    }
}

fn normalize_markers_for_range(markers: &[MarkerEntry], range: (usize, usize)) -> Vec<f32> {
    let len = range.1.saturating_sub(range.0).max(1);
    markers
        .iter()
        .filter_map(|marker| {
            (marker.sample >= range.0 && marker.sample <= range.1).then_some(normalize_sample(
                marker.sample.saturating_sub(range.0),
                len,
            ))
        })
        .collect()
}

fn normalize_loop_for_range(
    loop_region: Option<(usize, usize)>,
    range: (usize, usize),
) -> Option<(f32, f32)> {
    let (start, end) = loop_region?;
    let window_len = range.1.saturating_sub(range.0).max(1);
    let clipped_start = start.max(range.0).min(range.1);
    let clipped_end = end.max(range.0).min(range.1);
    (clipped_end > clipped_start).then_some((
        normalize_sample(clipped_start.saturating_sub(range.0), window_len),
        normalize_sample(clipped_end.saturating_sub(range.0), window_len),
    ))
}

fn effective_cursor_sample(state: &EditorTargetState) -> Option<usize> {
    state
        .cursor_sample
        .or_else(|| state.selection.map(|(start, _)| start))
        .or_else(|| state.loop_current.map(|(start, _)| start))
        .or(state.total_samples.map(|_| state.view_offset))
}

fn snap_cursor_to_zero_cross(path: &Path, cursor: usize, dir: i32) -> Result<usize> {
    let (channels, _) = decode_audio_multi(path)
        .with_context(|| format!("decode zero-cross source: {}", path.display()))?;
    if channels.is_empty() {
        return Ok(cursor);
    }
    let mixed = mixdown_channels(&channels);
    if mixed.is_empty() {
        return Ok(cursor);
    }
    let len = mixed.len();
    let cur = cursor.min(len.saturating_sub(1));
    let is_cross = |prev: f32, cur: f32| -> bool {
        let eps = 1.0e-4f32;
        cur.abs() <= eps
            || prev.abs() <= eps
            || (prev > 0.0 && cur < 0.0)
            || (prev < 0.0 && cur > 0.0)
    };
    if dir > 0 {
        let mut prev = mixed[cur];
        for (idx, sample) in mixed.iter().enumerate().skip(cur.saturating_add(1)) {
            if is_cross(prev, *sample) {
                return Ok(idx);
            }
            prev = *sample;
        }
    } else if dir < 0 {
        let mut prev = mixed[cur];
        let mut idx = cur.saturating_sub(1);
        loop {
            let sample = mixed[idx];
            if is_cross(prev, sample) {
                return Ok(idx);
            }
            prev = sample;
            if idx == 0 {
                break;
            }
            idx -= 1;
        }
    } else if cur > 0 && cur + 1 < len {
        if is_cross(mixed[cur - 1], mixed[cur]) || is_cross(mixed[cur], mixed[cur + 1]) {
            return Ok(cur);
        }
    }
    Ok(cur)
}

fn write_batch_loudness_report(
    path: &Path,
    rows: &[BatchLoudnessRow],
    target_lufs: f32,
    applied: bool,
) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("md")
        .to_ascii_lowercase();
    ensure_parent_dir(path)?;
    match ext.as_str() {
        "json" => {
            std::fs::write(path, serde_json::to_string_pretty(rows)?)
                .with_context(|| format!("write report: {}", path.display()))?;
        }
        "csv" => {
            let mut lines = vec![
                "path,file,existing_gain_db,measured_lufs,effective_lufs,target_lufs,estimated_gain_db,proposed_gain_db,clipping_risk,warning".to_string()
            ];
            for row in rows {
                lines.push(format!(
                    "\"{}\",\"{}\",{:.3},{},{},{:.3},{},{},{},\"{}\"",
                    row.path.replace('"', "'"),
                    row.file.replace('"', "'"),
                    row.existing_gain_db,
                    opt_f32_csv(row.measured_lufs),
                    opt_f32_csv(row.effective_lufs),
                    target_lufs,
                    opt_f32_csv(row.estimated_gain_db),
                    opt_f32_csv(row.proposed_gain_db),
                    row.clipping_risk,
                    row.warning.clone().unwrap_or_default().replace('"', "'"),
                ));
            }
            std::fs::write(path, lines.join("\n"))
                .with_context(|| format!("write report: {}", path.display()))?;
        }
        "txt" => {
            let mut text = String::new();
            text.push_str(if applied {
                "NeoWaves Batch Loudness Apply\n"
            } else {
                "NeoWaves Batch Loudness Plan\n"
            });
            text.push_str(&format!("Target LUFS: {target_lufs:.2}\n\n"));
            for row in rows {
                text.push_str(&format!(
                    "{} | measured={:?} effective={:?} proposed={:?} clip_risk={} {}\n",
                    row.path,
                    row.measured_lufs,
                    row.effective_lufs,
                    row.proposed_gain_db,
                    row.clipping_risk,
                    row.warning.clone().unwrap_or_default()
                ));
            }
            std::fs::write(path, text)
                .with_context(|| format!("write report: {}", path.display()))?;
        }
        _ => {
            let mut text = String::new();
            text.push_str(if applied {
                "# NeoWaves Batch Loudness Apply\n\n"
            } else {
                "# NeoWaves Batch Loudness Plan\n\n"
            });
            text.push_str(&format!("Target LUFS: `{target_lufs:.2}`\n\n"));
            text.push_str("| File | Measured | Effective | Proposed Gain | Clip Risk | Warning |\n");
            text.push_str("| --- | ---: | ---: | ---: | --- | --- |\n");
            for row in rows {
                text.push_str(&format!(
                    "| `{}` | {} | {} | {} | {} | {} |\n",
                    row.file,
                    opt_f32_md(row.measured_lufs),
                    opt_f32_md(row.effective_lufs),
                    opt_f32_md(row.proposed_gain_db),
                    if row.clipping_risk { "yes" } else { "no" },
                    row.warning.clone().unwrap_or_default().replace('|', "/")
                ));
            }
            std::fs::write(path, text)
                .with_context(|| format!("write report: {}", path.display()))?;
        }
    }
    Ok(())
}

fn write_batch_export_report(
    path: &Path,
    mutated_paths: &[Value],
    skipped_paths: &[String],
    failed_paths: &[Value],
) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("md")
        .to_ascii_lowercase();
    ensure_parent_dir(path)?;
    match ext.as_str() {
        "json" => std::fs::write(
            path,
            serde_json::to_string_pretty(&json!({
                "mutated_paths": mutated_paths,
                "skipped_paths": skipped_paths,
                "failed_paths": failed_paths,
            }))?,
        )
        .with_context(|| format!("write report: {}", path.display()))?,
        _ => {
            let mut text = String::new();
            text.push_str("# NeoWaves Batch Export Report\n\n");
            text.push_str(&format!("Mutated: {}\n\n", mutated_paths.len()));
            for value in mutated_paths {
                let src = value.get("source").and_then(Value::as_str).unwrap_or_default();
                let dst = value
                    .get("destination")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                text.push_str(&format!("- `{src}` -> `{dst}`\n"));
            }
            if !skipped_paths.is_empty() {
                text.push_str("\n## Skipped\n");
                for path in skipped_paths {
                    text.push_str(&format!("- `{path}`\n"));
                }
            }
            if !failed_paths.is_empty() {
                text.push_str("\n## Failed\n");
                for value in failed_paths {
                    let src = value.get("path").and_then(Value::as_str).unwrap_or_default();
                    let err = value.get("error").and_then(Value::as_str).unwrap_or_default();
                    text.push_str(&format!("- `{src}`: {err}\n"));
                }
            }
            std::fs::write(path, text)
                .with_context(|| format!("write report: {}", path.display()))?;
        }
    }
    Ok(())
}

fn effect_graph_library_entries() -> Result<Vec<Value>> {
    let dir = super::effect_graph_ops::effect_graph_templates_dir_for_cli().map_err(anyhow::Error::msg)?;
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("read effect graph templates dir: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.ends_with(".nwgraph.json"))
            .unwrap_or(false)
        {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read effect graph file: {}", path.display()))?;
        let file = serde_json::from_str::<EffectGraphTemplateFile>(&text)
            .with_context(|| format!("parse effect graph file: {}", path.display()))?;
        let issues = super::effect_graph_ops::effect_graph_validate_for_cli(&file.graph);
        out.push(json!({
            "path": pathbuf_to_string(&path),
            "template_id": file.template_id,
            "name": file.name,
            "created_at_unix_ms": file.created_at_unix_ms,
            "updated_at_unix_ms": file.updated_at_unix_ms,
            "validation": effect_graph_issue_summary(&issues),
        }));
    }
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(b.get("name").and_then(Value::as_str).unwrap_or_default())
    });
    Ok(out)
}

fn load_effect_graph(reference: &str) -> Result<EffectGraphResolved> {
    let path = resolve_effect_graph_reference(reference)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read effect graph: {}", path.display()))?;
    let file = serde_json::from_str::<EffectGraphTemplateFile>(&text)
        .with_context(|| format!("parse effect graph: {}", path.display()))?;
    Ok(EffectGraphResolved { path, file })
}

fn resolve_effect_graph_reference(reference: &str) -> Result<PathBuf> {
    let raw = Path::new(reference);
    if raw.components().count() > 1 || reference.ends_with(".json") || raw.is_absolute() {
        return absolute_existing_path(raw);
    }
    let dir = super::effect_graph_ops::effect_graph_templates_dir_for_cli().map_err(anyhow::Error::msg)?;
    let direct = dir.join(format!("{}.nwgraph.json", sanitize_cli_token(reference)));
    if direct.exists() {
        return Ok(direct);
    }
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("read effect graph templates dir: {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.ends_with(".nwgraph.json"))
            .unwrap_or(false)
        {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read effect graph file: {}", path.display()))?;
        let file = serde_json::from_str::<EffectGraphTemplateFile>(&text)
            .with_context(|| format!("parse effect graph file: {}", path.display()))?;
        if file.template_id == reference || file.name == reference {
            return Ok(path);
        }
    }
    bail!("effect graph not found: {reference}")
}

fn default_effect_graph_output_path_for_new(name: &str) -> Result<PathBuf> {
    let dir = super::effect_graph_ops::effect_graph_templates_dir_for_cli().map_err(anyhow::Error::msg)?;
    let token = format!("{}_{}", sanitize_cli_token(name), now_unix_ms_local());
    Ok(dir.join(format!("{token}.nwgraph.json")))
}

fn save_effect_graph_file(path: &Path, file: &EffectGraphTemplateFile) -> Result<()> {
    ensure_parent_dir(path)?;
    std::fs::write(
        path,
        serde_json::to_string_pretty(file).context("serialize effect graph file")?,
    )
    .with_context(|| format!("write effect graph: {}", path.display()))?;
    Ok(())
}

fn effect_graph_issue_summary(issues: &[super::types::EffectGraphValidationIssue]) -> Value {
    json!({
        "ok": !issues.iter().any(|issue| issue.severity == EffectGraphSeverity::Error),
        "error_count": issues.iter().filter(|issue| issue.severity == EffectGraphSeverity::Error).count(),
        "warning_count": issues.iter().filter(|issue| issue.severity == EffectGraphSeverity::Warning).count(),
        "info_count": issues.iter().filter(|issue| issue.severity == EffectGraphSeverity::Info).count(),
    })
}

fn effect_graph_issue_json(issue: &super::types::EffectGraphValidationIssue) -> Value {
    json!({
        "severity": format!("{:?}", issue.severity),
        "code": issue.code,
        "message": issue.message,
        "node_id": issue.node_id,
    })
}

fn now_unix_ms_local() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sanitize_cli_token(value: &str) -> String {
    let mut out = String::new();
    let mut last_was_sep = false;
    for ch in value.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if matches!(ch, '_' | '-' | ' ') {
            Some('_')
        } else {
            None
        };
        if let Some(ch) = normalized {
            if ch == '_' {
                if !last_was_sep && !out.is_empty() {
                    out.push(ch);
                }
                last_was_sep = true;
            } else {
                out.push(ch);
                last_was_sep = false;
            }
        }
    }
    if out.is_empty() {
        "graph".to_string()
    } else {
        out.trim_matches('_').to_string()
    }
}

fn default_effect_graph_node_size(kind: EffectGraphNodeKind) -> [f32; 2] {
    match kind {
        EffectGraphNodeKind::Input | EffectGraphNodeKind::Output => [260.0, 136.0],
        EffectGraphNodeKind::Duplicate => [250.0, 152.0],
        EffectGraphNodeKind::MonoMix => [320.0, 226.0],
        EffectGraphNodeKind::PluginFx => [360.0, 320.0],
        EffectGraphNodeKind::SplitChannels => [260.0, 220.0],
        EffectGraphNodeKind::CombineChannels => [300.0, 250.0],
        EffectGraphNodeKind::DebugWaveform => [340.0, 250.0],
        EffectGraphNodeKind::DebugSpectrum => [360.0, 300.0],
        EffectGraphNodeKind::Gain
        | EffectGraphNodeKind::Loudness
        | EffectGraphNodeKind::PitchShift
        | EffectGraphNodeKind::TimeStretch
        | EffectGraphNodeKind::Speed => [280.0, 182.0],
    }
}

fn unique_effect_graph_node_id(graph: &EffectGraphDocument, prefix: &str) -> String {
    let base = sanitize_cli_token(prefix);
    if !graph.nodes.iter().any(|node| node.id == base) {
        return base;
    }
    for idx in 2..=999 {
        let candidate = format!("{base}_{idx:02}");
        if !graph.nodes.iter().any(|node| node.id == candidate) {
            return candidate;
        }
    }
    format!("{base}_{}", now_unix_ms_local())
}

fn unique_effect_graph_edge_id(graph: &EffectGraphDocument, prefix: &str) -> String {
    let base = sanitize_cli_token(prefix);
    if !graph.edges.iter().any(|edge| edge.id == base) {
        return base;
    }
    for idx in 2..=999 {
        let candidate = format!("{base}_{idx:02}");
        if !graph.edges.iter().any(|edge| edge.id == candidate) {
            return candidate;
        }
    }
    format!("{base}_{}", now_unix_ms_local())
}

fn draw_effect_graph_document_image(document: &EffectGraphDocument, width: u32, height: u32) -> RgbaImage {
    let mut image = ImageBuffer::from_pixel(width, height, Rgba([18, 20, 26, 255]));
    for edge in &document.edges {
        let Some(from) = document.nodes.iter().find(|node| node.id == edge.from_node_id) else {
            continue;
        };
        let Some(to) = document.nodes.iter().find(|node| node.id == edge.to_node_id) else {
            continue;
        };
        let x0 = (from.ui_pos[0] + from.ui_size[0]).max(0.0) as u32;
        let y0 = (from.ui_pos[1] + from.ui_size[1] * 0.5).max(0.0) as u32;
        let x1 = to.ui_pos[0].max(0.0) as u32;
        let y1 = (to.ui_pos[1] + to.ui_size[1] * 0.5).max(0.0) as u32;
        draw_line(&mut image, x0, y0, x1, y1, [130, 145, 172, 255]);
    }
    for node in &document.nodes {
        let color = match node.data.kind() {
            EffectGraphNodeKind::Input => [82, 178, 255, 255],
            EffectGraphNodeKind::Output => [112, 220, 120, 255],
            EffectGraphNodeKind::DebugWaveform | EffectGraphNodeKind::DebugSpectrum => [245, 196, 92, 255],
            EffectGraphNodeKind::PluginFx => [220, 120, 120, 255],
            _ => [76, 88, 110, 255],
        };
        let fill = [color[0], color[1], color[2], 48];
        let x0 = node.ui_pos[0].max(0.0) as u32;
        let y0 = node.ui_pos[1].max(0.0) as u32;
        let x1 = (node.ui_pos[0] + node.ui_size[0]).max(0.0) as u32;
        let y1 = (node.ui_pos[1] + node.ui_size[1]).max(0.0) as u32;
        for x in x0.min(width.saturating_sub(1))..x1.min(width.saturating_sub(1)) {
            for y in y0.min(height.saturating_sub(1))..y1.min(height.saturating_sub(1)) {
                blend_pixel(&mut image, x, y, fill);
            }
        }
        draw_rect_outline(&mut image, x0, y0, x1, y1, color);
    }
    image
}

fn draw_rect_outline(image: &mut RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32, color: [u8; 4]) {
    draw_horizontal_segment(image, x0, x1, y0, color);
    draw_horizontal_segment(image, x0, x1, y1.saturating_sub(1), color);
    draw_vertical_line(image, x0, y0, y1.saturating_sub(1), color);
    draw_vertical_line(image, x1.saturating_sub(1), y0, y1.saturating_sub(1), color);
}

fn draw_horizontal_segment(image: &mut RgbaImage, x0: u32, x1: u32, y: u32, color: [u8; 4]) {
    if y >= image.height() {
        return;
    }
    for x in x0.min(x1)..=x0.max(x1).min(image.width().saturating_sub(1)) {
        image.put_pixel(x, y, Rgba(color));
    }
}

fn draw_line(image: &mut RgbaImage, x0: u32, y0: u32, x1: u32, y1: u32, color: [u8; 4]) {
    let mut x0 = x0 as i32;
    let mut y0 = y0 as i32;
    let x1 = x1 as i32;
    let y1 = y1 as i32;
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        if x0 >= 0 && y0 >= 0 && (x0 as u32) < image.width() && (y0 as u32) < image.height() {
            image.put_pixel(x0 as u32, y0 as u32, Rgba(color));
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

fn draw_rough_waveform_preview(minmax: &[(f32, f32)], width: u32, height: u32) -> RgbaImage {
    let mut image = ImageBuffer::from_pixel(width, height, Rgba(DEFAULT_WAVEFORM_BG));
    let mid = height / 2;
    draw_horizontal_line(&mut image, mid, DEFAULT_ZERO_LINE);
    if minmax.is_empty() {
        return image;
    }
    for x in 0..width {
        let idx = ((x as usize) * minmax.len() / width.max(1) as usize).min(minmax.len() - 1);
        let (mn, mx) = minmax[idx];
        let top = sample_to_y(mx, 0, height);
        let bottom = sample_to_y(mn, 0, height);
        draw_vertical_line(&mut image, x, top, bottom, DEFAULT_WAVEFORM_LINE);
    }
    image
}

fn write_effect_graph_validation_report(
    path: &Path,
    graph: &EffectGraphResolved,
    issues: &[super::types::EffectGraphValidationIssue],
) -> Result<()> {
    ensure_parent_dir(path)?;
    let mut text = String::new();
    text.push_str("# Effect Graph Validation\n\n");
    text.push_str(&format!("Graph: `{}`\n\n", graph.file.name));
    for issue in issues {
        text.push_str(&format!(
            "- {:?} `{}` {} {}\n",
            issue.severity,
            issue.code,
            issue.message,
            issue
                .node_id
                .as_ref()
                .map(|id| format!("(node `{id}`)"))
                .unwrap_or_default()
        ));
    }
    std::fs::write(path, text).with_context(|| format!("write report: {}", path.display()))?;
    Ok(())
}

fn write_effect_graph_test_report(
    path: &Path,
    graph: &EffectGraphResolved,
    input: Option<&Path>,
    report: &super::effect_graph_ops::EffectGraphCliTestReport,
    preview_path: &Path,
) -> Result<()> {
    ensure_parent_dir(path)?;
    let mut text = String::new();
    text.push_str("# Effect Graph Test\n\n");
    text.push_str(&format!("Graph: `{}`\n\n", graph.file.name));
    text.push_str(&format!(
        "Input: `{}`\n\n",
        input
            .map(pathbuf_to_string)
            .unwrap_or_else(|| "[embedded sample]".to_string())
    ));
    text.push_str(&format!(
        "Output channels: `{}` at `{}` Hz\n\n",
        report.output_channel_count,
        report.output_sample_rate
    ));
    text.push_str(&format!("Preview: `{}`\n\n", pathbuf_to_string(preview_path)));
    text.push_str("## Per-channel Peaks\n");
    for (idx, peak) in report.per_channel_peak_db.iter().enumerate() {
        text.push_str(&format!("- Ch{}: {}\n", idx + 1, opt_f32_md(*peak)));
    }
    if !report.silent_outputs.is_empty() {
        text.push_str("\n## Silent Outputs\n");
        for idx in &report.silent_outputs {
            text.push_str(&format!("- Ch{}\n", idx + 1));
        }
    }
    std::fs::write(path, text).with_context(|| format!("write report: {}", path.display()))?;
    Ok(())
}

fn opt_f32_md(value: Option<f32>) -> String {
    value
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "-".to_string())
}

fn opt_f32_csv(value: Option<f32>) -> String {
    value
        .map(|value| format!("{value:.6}"))
        .unwrap_or_default()
}

fn parse_list_column_keys(raw: &str) -> Result<Vec<String>> {
    let cfg = parse_list_column_config(raw)?;
    let mut out = Vec::new();
    for (key, enabled) in [
        ("edited", cfg.edited),
        ("cover_art", cfg.cover_art),
        ("type_badge", cfg.type_badge),
        ("file", cfg.file),
        ("folder", cfg.folder),
        ("transcript", cfg.transcript),
        ("transcript_language", cfg.transcript_language),
        ("external", cfg.external),
        ("length", cfg.length),
        ("channels", cfg.channels),
        ("sample_rate", cfg.sample_rate),
        ("bits", cfg.bits),
        ("bit_rate", cfg.bit_rate),
        ("peak", cfg.peak),
        ("lufs", cfg.lufs),
        ("bpm", cfg.bpm),
        ("created_at", cfg.created_at),
        ("modified_at", cfg.modified_at),
        ("gain", cfg.gain),
        ("wave", cfg.wave),
    ] {
        if enabled {
            out.push(key.to_string());
        }
    }
    Ok(out)
}

fn parse_list_column_config(raw: &str) -> Result<ListColumnConfig> {
    let mut cfg = ListColumnConfig {
        edited: false,
        cover_art: false,
        type_badge: false,
        file: false,
        folder: false,
        transcript: false,
        transcript_language: false,
        external: false,
        length: false,
        channels: false,
        sample_rate: false,
        bits: false,
        bit_rate: false,
        peak: false,
        lufs: false,
        bpm: false,
        created_at: false,
        modified_at: false,
        gain: false,
        wave: false,
    };
    for key in raw.split(',').map(str::trim).filter(|key| !key.is_empty()) {
        match key {
            "edited" => cfg.edited = true,
            "cover_art" => cfg.cover_art = true,
            "type_badge" => cfg.type_badge = true,
            "file" => cfg.file = true,
            "folder" => cfg.folder = true,
            "transcript" => cfg.transcript = true,
            "transcript_language" => cfg.transcript_language = true,
            "external" => cfg.external = true,
            "length" => cfg.length = true,
            "channels" => cfg.channels = true,
            "sample_rate" => cfg.sample_rate = true,
            "bits" => cfg.bits = true,
            "bit_rate" => cfg.bit_rate = true,
            "peak" => cfg.peak = true,
            "lufs" => cfg.lufs = true,
            "bpm" => cfg.bpm = true,
            "created_at" => cfg.created_at = true,
            "modified_at" => cfg.modified_at = true,
            "gain" => cfg.gain = true,
            "wave" => cfg.wave = true,
            other => bail!("unknown list column key: {other}"),
        }
    }
    Ok(cfg)
}

fn project_list_columns_from_config(cfg: ListColumnConfig) -> ProjectListColumns {
    ProjectListColumns {
        edited: cfg.edited,
        cover_art: cfg.cover_art,
        type_badge: cfg.type_badge,
        file: cfg.file,
        folder: cfg.folder,
        transcript: cfg.transcript,
        transcript_language: cfg.transcript_language,
        external: cfg.external,
        length: cfg.length,
        ch: cfg.channels,
        sr: cfg.sample_rate,
        bits: cfg.bits,
        bit_rate: cfg.bit_rate,
        peak: cfg.peak,
        lufs: cfg.lufs,
        bpm: cfg.bpm,
        created_at: cfg.created_at,
        modified_at: cfg.modified_at,
        gain: cfg.gain,
        wave: cfg.wave,
    }
}

fn parse_marker_specs(raws: &[String]) -> Result<Vec<MarkerEntry>> {
    let mut out = Vec::new();
    for (idx, raw) in raws.iter().enumerate() {
        let mut parts = raw.splitn(2, ':');
        let sample = parts
            .next()
            .context("marker spec missing sample")?
            .parse::<usize>()
            .with_context(|| format!("parse marker sample: {raw}"))?;
        let label = parts
            .next()
            .map(str::to_string)
            .unwrap_or_else(|| format!("M{:02}", idx + 1));
        out.push(MarkerEntry { sample, label });
    }
    out.sort_by_key(|marker| marker.sample);
    Ok(out)
}

fn project_marker_to_entry(marker: &ProjectMarker) -> MarkerEntry {
    MarkerEntry {
        sample: marker.sample,
        label: marker.label.clone(),
    }
}

fn marker_json(marker: &MarkerEntry) -> Value {
    json!({
        "sample": marker.sample,
        "label": marker.label,
    })
}

fn loop_xfade_shape_string(shape: CliLoopXfadeShape) -> &'static str {
    match shape {
        CliLoopXfadeShape::Linear => "linear",
        CliLoopXfadeShape::Equal => "equal",
        CliLoopXfadeShape::LinearDip => "linear_dip",
        CliLoopXfadeShape::EqualDip => "equal_dip",
    }
}

fn read_markers_in_file_space(path: &Path, info: &AudioInfo) -> Result<Vec<MarkerEntry>> {
    markers::read_markers(path, info.sample_rate.max(1), info.sample_rate.max(1))
        .with_context(|| format!("read markers: {}", path.display()))
}

fn read_loop_range_usize(path: &Path) -> Option<(usize, usize)> {
    loop_markers::read_loop_markers(path).map(|(start, end)| (start as usize, end as usize))
}

fn normalized_markers(path: &Path, info: &AudioInfo) -> Result<Vec<f32>> {
    let total = infer_total_frames(info).unwrap_or(0);
    if total == 0 {
        return Ok(Vec::new());
    }
    Ok(read_markers_in_file_space(path, info)?
        .into_iter()
        .map(|marker| normalize_sample(marker.sample, total))
        .collect())
}

fn normalized_loop(path: &Path, info: &AudioInfo) -> Option<(f32, f32)> {
    normalized_loop_samples(path, info).map(|(start, end)| {
        let total = infer_total_frames(info).unwrap_or(1).max(1);
        (normalize_sample(start, total), normalize_sample(end, total))
    })
}

fn normalized_loop_samples(path: &Path, info: &AudioInfo) -> Option<(usize, usize)> {
    let total = infer_total_frames(info)?;
    let (start, end) = read_loop_range_usize(path)?;
    Some((start.min(total), end.min(total)))
}

fn parse_optional_range(
    start_sample: Option<usize>,
    end_sample: Option<usize>,
    start_frac: Option<f32>,
    end_frac: Option<f32>,
    total_samples: usize,
) -> Result<(usize, usize)> {
    match (start_sample, end_sample, start_frac, end_frac) {
        (Some(start), Some(end), None, None) => Ok((start.min(end), start.max(end))),
        (None, None, Some(start), Some(end)) => {
            let start = (start.clamp(0.0, 1.0) * total_samples.max(1) as f32).round() as usize;
            let end = (end.clamp(0.0, 1.0) * total_samples.max(1) as f32).round() as usize;
            Ok((start.min(end), start.max(end)))
        }
        _ => bail!("sample range and fraction range must be provided as complete pairs"),
    }
}

fn parse_optional_playback_range_for_total(
    start_sample: Option<usize>,
    end_sample: Option<usize>,
    start_frac: Option<f32>,
    end_frac: Option<f32>,
    total_samples: usize,
) -> Result<Option<(usize, usize)>> {
    match (start_sample, end_sample, start_frac, end_frac) {
        (None, None, None, None) => Ok(None),
        (Some(start), Some(end), None, None) => Ok(Some((start.min(end), start.max(end)))),
        (None, None, Some(start), Some(end)) => Ok(Some(parse_optional_range(
            None,
            None,
            Some(start),
            Some(end),
            total_samples,
        )?)),
        _ => bail!("playback range must be provided as complete sample or fraction pairs"),
    }
}

fn parse_loop_override(
    start_sample: Option<usize>,
    end_sample: Option<usize>,
) -> Result<Option<(usize, usize)>> {
    match (start_sample, end_sample) {
        (Some(start), Some(end)) => Ok(Some((start.min(end), start.max(end)))),
        (None, None) => Ok(None),
        _ => bail!("loop start/end must be set together"),
    }
}

fn is_exact_stream_playback_candidate(path: &Path, dirty: bool) -> bool {
    !dirty
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("wav"))
            .unwrap_or(false)
}

fn play_exact_stream(
    engine: &crate::audio::AudioEngine,
    path: &Path,
    range: (usize, usize),
    rate: f32,
) -> Result<()> {
    engine
        .set_streaming_wav_path(path)
        .with_context(|| format!("open streaming wav: {}", path.display()))?;
    engine.set_rate(rate);
    engine.set_loop_enabled(false);
    engine.seek_to_sample(range.0);
    engine.play();
    let started = Instant::now();
    loop {
        let pos = engine
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        let playing = engine
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        if pos >= range.1 || !playing {
            break;
        }
        if started.elapsed() > CLI_PLAYBACK_TIMEOUT {
            engine.stop();
            bail!("playback timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    engine.stop();
    Ok(())
}

fn play_buffer_range(
    engine: &crate::audio::AudioEngine,
    channels: Vec<Vec<f32>>,
    source_sr: u32,
    range: (usize, usize),
    rate: f32,
) -> Result<()> {
    let mut sliced = slice_channels_for_range(&channels, range);
    let output_sr = engine.shared.out_sample_rate.max(1);
    if source_sr.max(1) != output_sr {
        for channel in &mut sliced {
            *channel = wave::resample_quality(
                channel,
                source_sr.max(1),
                output_sr,
                wave::ResampleQuality::Good,
            );
        }
    }
    engine.set_samples_channels(sliced);
    engine.set_rate(rate);
    engine.set_loop_enabled(false);
    engine.play();
    let started = Instant::now();
    loop {
        let playing = engine
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        if !playing {
            break;
        }
        if started.elapsed() > CLI_PLAYBACK_TIMEOUT {
            engine.stop();
            bail!("playback timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

fn slice_channels_for_range(channels: &[Vec<f32>], range: (usize, usize)) -> Vec<Vec<f32>> {
    channels
        .iter()
        .map(|channel| {
            let start = range.0.min(channel.len());
            let end = range.1.min(channel.len());
            if end > start {
                channel[start..end].to_vec()
            } else {
                Vec::new()
            }
        })
        .collect()
}

fn infer_total_frames(info: &AudioInfo) -> Option<usize> {
    info.total_frames.map(|frames| frames as usize).or_else(|| {
        info.duration_secs.map(|secs| {
            ((secs.max(0.0) as f64) * info.sample_rate.max(1) as f64).round() as usize
        })
    })
}

fn total_samples_for_session_path(session: &LoadedSession, target: &Path) -> Result<usize> {
    if let Some(idx) = find_project_tab_index(session, target) {
        if let Some(info) = read_audio_info(target).ok() {
            if let Some(total) = infer_total_frames(&info) {
                return Ok(total);
            }
        }
        let tab = &session.project.tabs[idx];
        if let Some(buffer_sr) = tab.buffer_sample_rate {
            return Ok(buffer_sr as usize);
        }
    }
    infer_total_frames(&read_audio_info(target)?).context("determine total samples")
}

fn mixdown_channels(channels: &[Vec<f32>]) -> Vec<f32> {
    let len = channels.iter().map(Vec::len).min().unwrap_or(0);
    if len == 0 {
        return Vec::new();
    }
    let mut out = vec![0.0; len];
    for channel in channels {
        for (idx, sample) in channel.iter().take(len).enumerate() {
            out[idx] += *sample;
        }
    }
    let denom = channels.len().max(1) as f32;
    for sample in &mut out {
        *sample /= denom;
    }
    out
}

fn stable_row_id_for_path(path: &Path) -> String {
    URL_SAFE_NO_PAD.encode(pathbuf_to_string(path))
}

fn draw_waveform_image(
    channels: &[Vec<f32>],
    _total_samples: usize,
    width: u32,
    height: u32,
    markers: Vec<f32>,
    loop_region: Option<(f32, f32)>,
) -> RgbaImage {
    let mut image = ImageBuffer::from_pixel(width, height, Rgba(DEFAULT_WAVEFORM_BG));
    if channels.is_empty() {
        return image;
    }
    let lane_count = channels.len().max(1) as u32;
    let lane_height = (height / lane_count).max(1);
    if let Some((start, end)) = loop_region {
        let x0 = frac_to_x(start, width);
        let x1 = frac_to_x(end, width);
        for x in x0.min(x1)..=x0.max(x1).min(width.saturating_sub(1)) {
            for y in 0..height {
                blend_pixel(&mut image, x, y, DEFAULT_LOOP_FILL);
            }
        }
        draw_vertical_line(
            &mut image,
            x0.min(width.saturating_sub(1)),
            0,
            height.saturating_sub(1),
            DEFAULT_LOOP_EDGE,
        );
        draw_vertical_line(
            &mut image,
            x1.min(width.saturating_sub(1)),
            0,
            height.saturating_sub(1),
            DEFAULT_LOOP_EDGE,
        );
    }
    for lane_idx in 0..lane_count {
        let channel = &channels[lane_idx as usize];
        let mut minmax = Vec::new();
        wave::build_minmax(&mut minmax, channel, width as usize);
        let y0 = lane_idx * lane_height;
        let y1 = if lane_idx + 1 == lane_count {
            height
        } else {
            ((lane_idx + 1) * lane_height).min(height)
        };
        let mid = y0 + (y1.saturating_sub(y0)) / 2;
        draw_horizontal_line(&mut image, mid, DEFAULT_ZERO_LINE);
        let color = if lane_idx % 2 == 0 {
            DEFAULT_WAVEFORM_LINE
        } else {
            DEFAULT_WAVEFORM_LINE_B
        };
        for (idx, (mn, mx)) in minmax.iter().enumerate() {
            let x = idx as u32;
            let top = sample_to_y(*mx, y0, y1);
            let bottom = sample_to_y(*mn, y0, y1);
            draw_vertical_line(
                &mut image,
                x.min(width.saturating_sub(1)),
                top,
                bottom,
                color,
            );
        }
    }
    let mut cols = HashSet::new();
    for marker in markers {
        let x = frac_to_x(marker, width).min(width.saturating_sub(1));
        if cols.insert(x) {
            draw_vertical_line(&mut image, x, 0, height.saturating_sub(1), DEFAULT_MARKER);
        }
    }
    image
}

fn draw_horizontal_line(image: &mut RgbaImage, y: u32, color: [u8; 4]) {
    if y >= image.height() {
        return;
    }
    for x in 0..image.width() {
        image.put_pixel(x, y, Rgba(color));
    }
}

fn draw_vertical_line(image: &mut RgbaImage, x: u32, y0: u32, y1: u32, color: [u8; 4]) {
    if x >= image.width() {
        return;
    }
    for y in y0.min(y1)..=y0.max(y1).min(image.height().saturating_sub(1)) {
        image.put_pixel(x, y, Rgba(color));
    }
}

fn blend_pixel(image: &mut RgbaImage, x: u32, y: u32, color: [u8; 4]) {
    if x >= image.width() || y >= image.height() {
        return;
    }
    let dst = image.get_pixel_mut(x, y);
    let alpha = color[3] as f32 / 255.0;
    for idx in 0..3 {
        dst.0[idx] =
            ((dst.0[idx] as f32 * (1.0 - alpha)) + (color[idx] as f32 * alpha)).round() as u8;
    }
}

fn sample_to_y(sample: f32, y0: u32, y1: u32) -> u32 {
    let lane_h = y1.saturating_sub(y0).max(1);
    let frac = ((1.0 - sample.clamp(-1.0, 1.0)) * 0.5).clamp(0.0, 1.0);
    y0 + ((lane_h.saturating_sub(1)) as f32 * frac).round() as u32
}

fn normalize_sample(sample: usize, total_samples: usize) -> f32 {
    if total_samples <= 1 {
        return 0.0;
    }
    sample as f32 / total_samples.saturating_sub(1) as f32
}

fn frac_to_x(frac: f32, width: u32) -> u32 {
    ((width.saturating_sub(1)) as f32 * frac.clamp(0.0, 1.0)).round() as u32
}

fn save_rgba_image(image: &RgbaImage, path: &Path) -> Result<()> {
    ensure_parent_dir(path)?;
    image.save(path)?;
    Ok(())
}

fn save_color_image(image: &egui::ColorImage, path: &Path) -> Result<()> {
    ensure_parent_dir(path)?;
    let rgba = image
        .pixels
        .iter()
        .flat_map(|pixel| [pixel.r(), pixel.g(), pixel.b(), pixel.a()])
        .collect::<Vec<_>>();
    image::codecs::png::PngEncoder::new(std::fs::File::create(path)?).write_image(
        &rgba,
        image.size[0] as u32,
        image.size[1] as u32,
        ColorType::Rgba8.into(),
    )?;
    Ok(())
}

fn image_dimensions(path: &Path) -> Result<(u32, u32)> {
    image::image_dimensions(path).with_context(|| format!("read image size: {}", path.display()))
}

fn build_editor_render_session(args: &RenderEditorArgs) -> Result<PathBuf> {
    if let Some(session_path) = args.source.session.as_deref() {
        let mut session = load_session(session_path)?;
        let target = resolve_session_target_path(&session, &args.source)?;
        let idx = ensure_project_tab_for_path(&mut session, &target)?;
        if let Some(mode) = args.view_mode {
            let mode: ViewMode = mode.into();
            session.project.tabs[idx].view_mode = format!("{mode:?}");
            session.project.tabs[idx].primary_view =
                Some(project_primary_view_string(EditorPrimaryView::from_mode(mode)));
            session.project.tabs[idx].spec_sub_view =
                Some(project_spec_sub_view_string(EditorSpecSubView::from_mode(mode)));
            session.project.tabs[idx].other_sub_view = Some(project_other_sub_view_string(
                super::types::EditorOtherSubView::from_mode(mode),
            ));
        }
        if let Some(toggle) = args.waveform_overlay {
            session.project.tabs[idx].show_waveform_overlay = toggle.into_bool();
        }
        return write_temp_project_file("editor-render", &session.project);
    }
    let input = args
        .source
        .input
        .as_deref()
        .context("render editor requires --input or --session")?;
    let mut project = build_project_file_from_entries(&[SessionListEntry {
        path: absolute_existing_path(input)?,
        pending_gain_db: 0.0,
    }])?;
    let input_path = project
        .list
        .items
        .first()
        .map(|item| PathBuf::from(&item.path))
        .context("missing render editor source")?;
    let mut tab = default_project_tab_for_path(
        project
            .list
            .items
            .first()
            .map(|item| Path::new(&item.path))
            .context("missing render editor source")?,
        &cli_render_dir()?.join("sessions"),
    )?;
    if let Some(mode) = args.view_mode {
        let mode: ViewMode = mode.into();
        tab.view_mode = format!("{mode:?}");
        tab.primary_view = Some(project_primary_view_string(EditorPrimaryView::from_mode(mode)));
        tab.spec_sub_view = Some(project_spec_sub_view_string(EditorSpecSubView::from_mode(mode)));
        tab.other_sub_view = Some(project_other_sub_view_string(
            super::types::EditorOtherSubView::from_mode(mode),
        ));
    }
    if let Some(toggle) = args.waveform_overlay {
        tab.show_waveform_overlay = toggle.into_bool();
    }
    if let Ok(info) = read_audio_info(&input_path) {
        if let Ok(markers) = read_markers_in_file_space(&input_path, &info) {
            tab.markers = markers.iter().map(marker_entry_to_project).collect();
        }
        tab.loop_region = read_loop_range_usize(&input_path).map(range_to_array);
    }
    project.tabs.push(tab);
    project.active_tab = Some(0);
    write_temp_project_file("editor-render", &project)
}

fn debug_session_target(source: &EditorSourceArgs) -> Result<(PathBuf, bool)> {
    if let Some(session_path) = source.session.as_deref() {
        return Ok((absolute_existing_path(session_path)?, true));
    }
    let input = source
        .input
        .as_deref()
        .context("debug summary requires --input or --session")?;
    let mut project = build_project_file_from_entries(&[SessionListEntry {
        path: absolute_existing_path(input)?,
        pending_gain_db: 0.0,
    }])?;
    let input_path = project
        .list
        .items
        .first()
        .map(|item| PathBuf::from(&item.path))
        .context("missing debug source")?;
    let mut tab = default_project_tab_for_path(
        project
            .list
            .items
            .first()
            .map(|item| Path::new(&item.path))
            .context("missing debug source")?,
        &cli_render_dir()?.join("sessions"),
    )?;
    if let Ok(info) = read_audio_info(&input_path) {
        if let Ok(markers) = read_markers_in_file_space(&input_path, &info) {
            tab.markers = markers.iter().map(marker_entry_to_project).collect();
        }
        tab.loop_region = read_loop_range_usize(&input_path).map(range_to_array);
    }
    project.tabs.push(tab);
    project.active_tab = Some(0);
    Ok((write_temp_project_file("debug-summary", &project)?, true))
}

fn render_gui_session_screenshot(
    session_path: &Path,
    output_path: &Path,
    view_mode: Option<ViewMode>,
    waveform_overlay: Option<bool>,
    open_first: bool,
) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolve current executable")?;
    let mut args = vec![
        OsString::from("--open-session"),
        session_path.as_os_str().to_os_string(),
        OsString::from("--screenshot"),
        output_path.as_os_str().to_os_string(),
        OsString::from("--exit-after-screenshot"),
        OsString::from("--screenshot-delay"),
        OsString::from(if open_first { "24" } else { "18" }),
        OsString::from("--no-ipc-forward"),
    ];
    if open_first {
        args.push(OsString::from("--open-first"));
    }
    if let Some(mode) = view_mode {
        args.push(OsString::from("--open-view-mode"));
        args.push(OsString::from(match mode {
            ViewMode::Waveform => "wave",
            ViewMode::Spectrogram => "spec",
            ViewMode::Log => "log",
            ViewMode::Mel => "mel",
            ViewMode::Tempogram => "tempogram",
            ViewMode::Chromagram => "chromagram",
        }));
    }
    if let Some(flag) = waveform_overlay {
        args.push(OsString::from("--waveform-overlay"));
        args.push(OsString::from(if flag { "on" } else { "off" }));
    }
    run_gui_child(&exe, &args)?;
    absolute_existing_path(output_path)
}

fn render_gui_debug_summary(
    session_path: &Path,
    output_path: &Path,
    open_first: bool,
) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolve current executable")?;
    let mut args = vec![
        OsString::from("--open-session"),
        session_path.as_os_str().to_os_string(),
        OsString::from("--debug-summary"),
        output_path.as_os_str().to_os_string(),
        OsString::from("--debug-summary-delay"),
        OsString::from(if open_first { "24" } else { "18" }),
        OsString::from("--no-ipc-forward"),
    ];
    if open_first {
        args.push(OsString::from("--open-first"));
    }
    run_gui_child(&exe, &args)?;
    absolute_existing_path(output_path)
}

fn run_gui_child(exe: &Path, args: &[OsString]) -> Result<()> {
    let status = std::process::Command::new(exe).args(args).status()?;
    if !status.success() {
        bail!("GUI child exited with status {status}");
    }
    Ok(())
}

fn audio_info_json(info: &AudioInfo) -> Value {
    json!({
        "channels": info.channels,
        "sample_rate": info.sample_rate,
        "bits_per_sample": info.bits_per_sample,
        "sample_value_kind": format!("{:?}", info.sample_value_kind),
        "bit_rate_bps": info.bit_rate_bps,
        "duration_secs": info.duration_secs,
        "total_frames": info.total_frames,
        "created_at_unix": info.created_at.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()),
        "modified_at_unix": info.modified_at.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()),
    })
}

fn spectral_mode_name(mode: CliSpectralViewMode) -> &'static str {
    match mode {
        CliSpectralViewMode::Spec => "spec",
        CliSpectralViewMode::Log => "log",
        CliSpectralViewMode::Mel => "mel",
    }
}

fn view_mode_name(mode: crate::cli::CliViewMode) -> &'static str {
    match mode {
        crate::cli::CliViewMode::Wave => "wave",
        crate::cli::CliViewMode::Spec => "spec",
        crate::cli::CliViewMode::Log => "log",
        crate::cli::CliViewMode::Mel => "mel",
        crate::cli::CliViewMode::Tempogram => "tempogram",
        crate::cli::CliViewMode::Chromagram => "chromagram",
    }
}

fn prepare_output_path(path: Option<PathBuf>, stem: &str, ext: &str) -> Result<PathBuf> {
    match path {
        Some(path) => absolute_output_path(&path),
        None => {
            let dir = cli_render_dir()?;
            std::fs::create_dir_all(&dir)?;
            absolute_output_path(&dir.join(format!("{}_{}.{}", stem, timestamp_token(), ext)))
        }
    }
}

fn cli_render_dir() -> Result<PathBuf> {
    Ok(std::env::current_dir()?.join("debug").join("cli-renders"))
}

fn timestamp_token() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn absolute_existing_path(path: &Path) -> Result<PathBuf> {
    let path = absolute_output_path(path)?;
    if !path.exists() {
        bail!("path does not exist: {}", path.display());
    }
    Ok(path)
}

fn absolute_output_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn absolute_string(path: &Path) -> Result<String> {
    Ok(pathbuf_to_string(&absolute_output_path(path)?))
}

fn system_time_json(value: Option<SystemTime>) -> Value {
    value
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|dur| json!(dur.as_secs()))
        .unwrap_or(Value::Null)
}

fn scan_audio_paths(folder: &Path) -> Result<Vec<PathBuf>> {
    let root = absolute_existing_path(folder)?;
    let mut out = Vec::new();
    for entry in WalkDir::new(&root).into_iter().filter_map(|entry| entry.ok()) {
        if entry.file_type().is_file() && is_supported_audio_path(entry.path()) {
            out.push(entry.into_path());
        }
    }
    out.sort_by_key(|path| path_key(path));
    Ok(out)
}

fn path_key(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    #[cfg(windows)]
    {
        abs.to_string_lossy().replace('/', "\\").to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        abs.to_string_lossy().to_string()
    }
}

fn pathbuf_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn range_to_array(range: (usize, usize)) -> [usize; 2] {
    [range.0, range.1]
}

fn array_to_range(range: [usize; 2]) -> (usize, usize) {
    (range[0], range[1])
}

fn slice_rows(
    rows: Vec<Map<String, Value>>,
    offset: usize,
    limit: Option<usize>,
) -> Vec<Map<String, Value>> {
    let iter = rows.into_iter().skip(offset);
    match limit {
        Some(limit) => iter.take(limit).collect(),
        None => iter.collect(),
    }
}

fn slice_entries(
    entries: Vec<SessionListEntry>,
    offset: usize,
    limit: Option<usize>,
) -> Vec<SessionListEntry> {
    let iter = entries.into_iter().skip(offset);
    match limit {
        Some(limit) => iter.take(limit).collect(),
        None => iter.collect(),
    }
}

fn external_inspect(args: ExternalInspectArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    Ok(CliCommandOutput {
        result: external_state_json(&workspace.app),
        warnings: Vec::new(),
    })
}

fn external_rows(args: ExternalRowsArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    let session = load_session(&args.session)?;
    let headers = workspace.app.external_headers.clone();
    let unmatched: HashSet<usize> = workspace.app.external_unmatched_rows.iter().copied().collect();
    let merged_rows = slice_rows(
        workspace
            .app
            .external_rows
            .iter()
            .enumerate()
            .filter(|(idx, _)| args.include_unmatched || !unmatched.contains(idx))
            .map(|(idx, row)| external_row_json(&headers, row, unmatched.contains(&idx), idx))
            .collect(),
        args.offset,
        args.limit,
    );
    let resolved_rows = slice_rows(
        session_list_entries(&session)
            .into_iter()
            .filter_map(|entry| {
                workspace
                    .app
                    .external_row_for_path(&entry.path)
                    .map(|row| external_resolved_row_json(&entry.path, &row))
            })
            .collect(),
        args.offset,
        args.limit,
    );
    Ok(CliCommandOutput {
        result: json!({
            "headers": headers,
            "visible_columns": workspace.app.external_visible_columns,
            "merged_rows": merged_rows,
            "resolved_rows": resolved_rows,
            "match_count": workspace.app.external_match_count,
            "unmatched_count": workspace.app.external_unmatched_count,
        }),
        warnings: Vec::new(),
    })
}

fn external_render(args: ExternalRenderArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    let output = prepare_output_path(args.output, "external", "png")?;
    let image = draw_external_rows_image(
        &workspace.app.external_headers,
        &workspace.app.external_visible_columns,
        &workspace.app.external_rows,
        &workspace
            .app
            .external_unmatched_rows
            .iter()
            .copied()
            .collect::<HashSet<_>>(),
        args.width.max(320),
        args.height.max(180),
    );
    save_rgba_image(&image, &output)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": absolute_string(&output)?,
            "width": image.width(),
            "height": image.height(),
            "row_count": workspace.app.external_rows.len(),
            "headers": workspace.app.external_headers,
            "visible_columns": workspace.app.external_visible_columns,
        }),
        warnings: Vec::new(),
    })
}

fn external_source_list(args: ExternalSourceListArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    Ok(CliCommandOutput {
        result: json!({
            "sources": workspace.app.external_sources.iter().enumerate().map(|(index, source)| external_source_json(index, source)).collect::<Vec<_>>(),
            "active_source": workspace.app.external_active_source,
        }),
        warnings: Vec::new(),
    })
}

fn external_source_add(args: ExternalSourceAddArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    let before = external_state_json(&workspace.app);
    let input = absolute_existing_path(&args.input)?;
    workspace.app.queue_external_load_with_settings(
        input.clone(),
        args.sheet_name.clone(),
        args.has_header.map(CliToggle::into_bool).unwrap_or(true),
        args.header_row.and_then(|row| row.checked_sub(1)),
        args.data_row.and_then(|row| row.checked_sub(1)),
        super::external_ops::ExternalLoadTarget::New,
    );
    let _ = workspace.app.start_next_external_load_from_queue();
    workspace.wait_for_external_loads()?;
    workspace.save()?;
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": external_state_json(&workspace.app),
            "mutated_sources": [pathbuf_to_string(&input)],
        }),
        warnings: Vec::new(),
    })
}

fn external_source_reload(args: ExternalSourceReloadArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    let before = external_state_json(&workspace.app);
    let index = args
        .index
        .or(workspace.app.external_active_source)
        .context("no external source selected")?;
    let source = workspace
        .app
        .external_sources
        .get(index)
        .cloned()
        .context("external source index out of range")?;
    workspace.app.external_active_source = Some(index);
    workspace.app.sync_active_external_source();
    workspace.app.external_load_target =
        Some(super::external_ops::ExternalLoadTarget::Reload(index));
    workspace.app.begin_external_load(source.path.clone());
    workspace.wait_for_external_loads()?;
    workspace.save()?;
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": external_state_json(&workspace.app),
            "mutated_sources": [pathbuf_to_string(&source.path)],
        }),
        warnings: Vec::new(),
    })
}

fn external_source_remove(args: ExternalSourceRemoveArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    let before = external_state_json(&workspace.app);
    if args.index >= workspace.app.external_sources.len() {
        bail!("external source index out of range: {}", args.index);
    }
    let removed = workspace.app.external_sources.remove(args.index);
    if workspace.app.external_sources.is_empty() {
        workspace.app.clear_external_data();
    } else {
        let next_active = args.index.min(workspace.app.external_sources.len().saturating_sub(1));
        workspace.app.external_active_source = Some(next_active);
        workspace.app.rebuild_external_merged();
        workspace.app.sync_active_external_source();
        workspace.app.apply_external_mapping();
        workspace.app.apply_filter_from_search();
        workspace.app.apply_sort();
    }
    workspace.save()?;
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": external_state_json(&workspace.app),
            "mutated_sources": [pathbuf_to_string(&removed.path)],
        }),
        warnings: Vec::new(),
    })
}

fn external_source_clear(args: ExternalSourceClearArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    let before = external_state_json(&workspace.app);
    workspace.app.clear_external_data();
    workspace.save()?;
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": external_state_json(&workspace.app),
            "mutated_sources": Vec::<String>::new(),
        }),
        warnings: Vec::new(),
    })
}

fn external_config_get(args: ExternalConfigGetArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    Ok(CliCommandOutput {
        result: external_config_json(&workspace.app),
        warnings: Vec::new(),
    })
}

fn external_config_set(args: ExternalConfigSetArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    workspace.wait_for_external_loads()?;
    let before = external_config_json(&workspace.app);
    if let Some(active_source) = args.active_source {
        if active_source >= workspace.app.external_sources.len() {
            bail!("external source index out of range: {active_source}");
        }
        workspace.app.external_active_source = Some(active_source);
        workspace.app.sync_active_external_source();
    }
    if let Some(key_rule) = args.key_rule {
        workspace.app.external_key_rule = key_rule.into();
    }
    if let Some(regex_input) = args.regex_input {
        workspace.app.external_match_input = regex_input.into();
    }
    if let Some(regex) = args.regex {
        workspace.app.external_match_regex = regex;
    }
    if let Some(replace) = args.replace {
        workspace.app.external_match_replace = replace;
    }
    if let Some(scope_regex) = args.scope_regex {
        workspace.app.external_scope_regex = scope_regex;
    }
    if let Some(show_unmatched) = args.show_unmatched {
        workspace.app.external_show_unmatched = show_unmatched.into_bool();
    }
    workspace.app.rebuild_external_merged();
    if let Some(key_column) = args.key_column.as_deref() {
        let key_idx = workspace
            .app
            .external_headers
            .iter()
            .position(|header| header == key_column)
            .with_context(|| format!("external key column not found: {key_column}"))?;
        workspace.app.external_key_index = Some(key_idx);
        workspace.app.rebuild_external_merged();
    }
    if !args.visible_columns.is_empty() {
        workspace.app.external_visible_columns = args.visible_columns;
    }
    workspace.app.apply_external_mapping();
    workspace.app.apply_filter_from_search();
    workspace.app.apply_sort();
    workspace.save()?;
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": external_config_json(&workspace.app),
            "mutated_sources": workspace.app.external_sources.iter().map(|src| pathbuf_to_string(&src.path)).collect::<Vec<_>>(),
        }),
        warnings: Vec::new(),
    })
}

fn transcript_inspect(args: TranscriptInspectArgs) -> Result<CliCommandOutput> {
    let (audio_path, language) = resolve_transcript_target_and_language(&args)?;
    let srt_path = super::transcript::srt_path_for_audio(&audio_path)
        .context("transcript inspect requires an audio file path")?;
    let transcript = super::transcript::load_srt(&srt_path);
    Ok(CliCommandOutput {
        result: json!({
            "audio_path": pathbuf_to_string(&audio_path),
            "srt_path": pathbuf_to_string(&srt_path),
            "exists": transcript.is_some(),
            "language": language,
            "segments": transcript.as_ref().map(|value| value.segments.iter().map(transcript_segment_json).collect::<Vec<_>>()).unwrap_or_default(),
            "full_text": transcript.as_ref().map(|value| value.full_text.clone()).unwrap_or_default(),
        }),
        warnings: Vec::new(),
    })
}

fn transcript_model_status(_: TranscriptModelStatusArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    app.refresh_transcript_ai_status();
    Ok(CliCommandOutput {
        result: transcript_model_status_json(&app),
        warnings: Vec::new(),
    })
}

fn transcript_model_download(_: TranscriptModelDownloadArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    app.refresh_transcript_ai_status();
    app.queue_transcript_model_download();
    wait_for_transcript_model_download_app(&mut app)?;
    if let Some(err) = app.transcript_ai_last_error.clone() {
        bail!(err);
    }
    Ok(CliCommandOutput {
        result: transcript_model_status_json(&app),
        warnings: Vec::new(),
    })
}

fn transcript_model_uninstall(_: TranscriptModelUninstallArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    app.uninstall_transcript_model_cache();
    if let Some(err) = app.transcript_ai_last_error.clone() {
        bail!(err);
    }
    Ok(CliCommandOutput {
        result: transcript_model_status_json(&app),
        warnings: Vec::new(),
    })
}

fn transcript_config_get(args: TranscriptConfigGetArgs) -> Result<CliCommandOutput> {
    let workspace = CliWorkspace::load(&args.session)?;
    Ok(CliCommandOutput {
        result: transcript_config_json(&workspace.app.transcript_ai_cfg),
        warnings: Vec::new(),
    })
}

fn transcript_config_set(args: TranscriptConfigSetArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let before = transcript_config_json(&workspace.app.transcript_ai_cfg);
    let cfg = &mut workspace.app.transcript_ai_cfg;
    if let Some(language) = args.language {
        cfg.language = language;
    }
    if let Some(task) = args.task {
        cfg.task = task;
    }
    if let Some(value) = args.max_new_tokens {
        cfg.max_new_tokens = value.max(1).min(512);
    }
    if let Some(value) = args.overwrite_existing_srt {
        cfg.overwrite_existing_srt = value.into_bool();
    }
    if let Some(value) = args.perf_mode {
        cfg.perf_mode = value.into();
    }
    if let Some(value) = args.model_variant {
        cfg.model_variant = value.into();
    }
    if let Some(value) = args.omit_language_token {
        cfg.omit_language_token = value.into_bool();
    }
    if let Some(value) = args.omit_notimestamps_token {
        cfg.omit_notimestamps_token = value.into_bool();
    }
    if let Some(value) = args.vad_enabled {
        cfg.vad_enabled = value.into_bool();
    }
    if args.clear_vad_model_path {
        cfg.vad_model_path = None;
    } else if let Some(path) = args.vad_model_path {
        cfg.vad_model_path = Some(absolute_output_path(&path)?);
    }
    if let Some(value) = args.vad_threshold {
        cfg.vad_threshold = value.clamp(0.01, 0.99);
    }
    if let Some(value) = args.vad_min_speech_ms {
        cfg.vad_min_speech_ms = value.clamp(10, 10_000);
    }
    if let Some(value) = args.vad_min_silence_ms {
        cfg.vad_min_silence_ms = value.clamp(10, 10_000);
    }
    if let Some(value) = args.vad_speech_pad_ms {
        cfg.vad_speech_pad_ms = value.min(5_000);
    }
    if let Some(value) = args.max_window_ms {
        cfg.max_window_ms = value.clamp(1_000, 30_000);
    }
    if args.clear_no_speech_threshold {
        cfg.no_speech_threshold = None;
    } else if let Some(value) = args.no_speech_threshold {
        cfg.no_speech_threshold = Some(value.clamp(0.0, 1.0));
    }
    if args.clear_logprob_threshold {
        cfg.logprob_threshold = None;
    } else if let Some(value) = args.logprob_threshold {
        cfg.logprob_threshold = Some(value.clamp(-10.0, 0.0));
    }
    if let Some(value) = args.compute_target {
        cfg.compute_target = value.into();
    }
    if let Some(value) = args.dml_device_id {
        cfg.dml_device_id = value.clamp(0, 16);
    }
    if let Some(value) = args.cpu_intra_threads {
        cfg.cpu_intra_threads = value.min(64);
    }
    workspace.app.sanitize_transcript_ai_config();
    workspace.app.refresh_transcript_ai_status();
    workspace.save()?;
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": transcript_config_json(&workspace.app.transcript_ai_cfg),
            "mutated_paths": Vec::<String>::new(),
        }),
        warnings: Vec::new(),
    })
}

fn transcript_generate(args: TranscriptGenerateArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let path = workspace.resolve_target_path(args.path.as_deref())?;
    let srt_path = super::transcript::srt_path_for_audio(&path)
        .context("transcript generate requires an audio file path")?;
    let existed_before = srt_path.is_file();
    if args.overwrite_existing {
        workspace.app.transcript_ai_cfg.overwrite_existing_srt = true;
    }
    workspace.app.run_transcript_ai_for_selected(vec![path.clone()]);
    if workspace.app.transcript_ai_state.is_some() {
        workspace.wait_for_transcript_ai()?;
    } else if let Some(err) = workspace.app.transcript_ai_last_error.clone() {
        bail!(err);
    }
    workspace.save()?;
    let completed = srt_path.is_file() && (args.overwrite_existing || !existed_before);
    let skipped = srt_path.is_file() && existed_before && !args.overwrite_existing;
    Ok(CliCommandOutput {
        result: json!({
            "completed_paths": if completed { vec![pathbuf_to_string(&path)] } else { Vec::<String>::new() },
            "skipped_paths": if skipped { vec![pathbuf_to_string(&path)] } else { Vec::<String>::new() },
            "failed_paths": if !srt_path.is_file() { vec![json!({"path": pathbuf_to_string(&path), "error": "SRT was not produced"})] } else { Vec::<Value>::new() },
            "output_srt_paths": if srt_path.is_file() { vec![pathbuf_to_string(&srt_path)] } else { Vec::<String>::new() },
        }),
        warnings: Vec::new(),
    })
}

fn transcript_batch_generate(args: TranscriptBatchGenerateArgs) -> Result<CliCommandOutput> {
    let session = load_session(&args.session)?;
    let filter = resolve_query_filter(&args.filter)?;
    let paths: Vec<PathBuf> = matched_session_entries(&session, &filter)?
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    if paths.is_empty() {
        return Ok(CliCommandOutput {
            result: json!({
                "completed_paths": Vec::<String>::new(),
                "skipped_paths": Vec::<String>::new(),
                "failed_paths": Vec::<Value>::new(),
                "output_srt_paths": Vec::<String>::new(),
            }),
            warnings: Vec::new(),
        });
    }
    let existed_before: HashSet<String> = paths
        .iter()
        .filter_map(|path| super::transcript::srt_path_for_audio(path))
        .filter(|path| path.is_file())
        .map(|path| path_key(&path))
        .collect();
    let mut workspace = CliWorkspace::load(&args.session)?;
    if args.overwrite_existing {
        workspace.app.transcript_ai_cfg.overwrite_existing_srt = true;
    }
    workspace.app.run_transcript_ai_for_selected(paths.clone());
    if workspace.app.transcript_ai_state.is_some() {
        workspace.wait_for_transcript_ai()?;
    } else if let Some(err) = workspace.app.transcript_ai_last_error.clone() {
        bail!(err);
    }
    workspace.save()?;
    let mut completed_paths = Vec::new();
    let mut skipped_paths = Vec::new();
    let mut failed_paths = Vec::new();
    let mut output_srt_paths = Vec::new();
    for path in paths {
        let Some(srt_path) = super::transcript::srt_path_for_audio(&path) else {
            failed_paths.push(json!({"path": pathbuf_to_string(&path), "error": "not an audio path"}));
            continue;
        };
        if srt_path.is_file() {
            output_srt_paths.push(pathbuf_to_string(&srt_path));
            if !args.overwrite_existing && existed_before.contains(&path_key(&srt_path)) {
                skipped_paths.push(pathbuf_to_string(&path));
            } else {
                completed_paths.push(pathbuf_to_string(&path));
            }
        } else {
            failed_paths.push(json!({"path": pathbuf_to_string(&path), "error": "SRT was not produced"}));
        }
    }
    Ok(CliCommandOutput {
        result: json!({
            "completed_paths": completed_paths,
            "skipped_paths": skipped_paths,
            "failed_paths": failed_paths,
            "output_srt_paths": output_srt_paths,
        }),
        warnings: Vec::new(),
    })
}

fn transcript_export_srt(args: TranscriptExportSrtArgs) -> Result<CliCommandOutput> {
    let (audio_path, _) = resolve_transcript_export_target(&args)?;
    let srt_path = super::transcript::srt_path_for_audio(&audio_path)
        .context("transcript export requires an audio file path")?;
    let transcript = super::transcript::load_srt(&srt_path)
        .with_context(|| format!("transcript not found: {}", srt_path.display()))?;
    let output = absolute_output_path(&args.output)?;
    ensure_parent_dir(&output)?;
    super::transcript::write_srt(&output, &transcript)
        .with_context(|| format!("write srt: {}", output.display()))?;
    Ok(CliCommandOutput {
        result: json!({
            "audio_path": pathbuf_to_string(&audio_path),
            "source_srt_path": pathbuf_to_string(&srt_path),
            "output_srt_path": absolute_string(&output)?,
            "segment_count": transcript.segments.len(),
        }),
        warnings: Vec::new(),
    })
}

fn music_ai_inspect(args: MusicAiInspectArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(
        args.source
            .session
            .as_deref()
            .context("music-ai inspect requires --session")?,
    )?;
    let tab_idx = workspace.ensure_target_tab_loaded(args.source.path.as_deref())?;
    let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
    Ok(CliCommandOutput {
        result: json!({
            "path": pathbuf_to_string(&tab.path),
            "analysis": music_analysis_json(&tab.music_analysis_draft),
        }),
        warnings: Vec::new(),
    })
}

fn music_ai_model_status(_: MusicAiModelStatusArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    app.refresh_music_ai_status();
    Ok(CliCommandOutput {
        result: music_model_status_json(&app),
        warnings: Vec::new(),
    })
}

fn music_ai_model_download(_: MusicAiModelDownloadArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    app.refresh_music_ai_status();
    app.queue_music_model_download();
    wait_for_music_model_download_app(&mut app)?;
    if let Some(err) = app.music_ai_last_error.clone() {
        bail!(err);
    }
    Ok(CliCommandOutput {
        result: music_model_status_json(&app),
        warnings: Vec::new(),
    })
}

fn music_ai_model_uninstall(_: MusicAiModelUninstallArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    app.uninstall_music_model_cache();
    if let Some(err) = app.music_ai_last_error.clone() {
        bail!(err);
    }
    Ok(CliCommandOutput {
        result: music_model_status_json(&app),
        warnings: Vec::new(),
    })
}

fn music_ai_analyze(args: MusicAiAnalyzeArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let tab_idx = workspace.ensure_target_tab_loaded(args.path.as_deref())?;
    let before = {
        let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
        music_analysis_json(&tab.music_analysis_draft)
    };
    let prefer_demucs_override = if args.prefer_demucs && args.stems_dir.is_none() {
        Some(
            workspace
                .app
                .tabs
                .get(tab_idx)
                .context("missing target tab")?
                .path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("__neowaves_force_demucs__"),
        )
    } else {
        None
    };
    if let Some(tab) = workspace.app.tabs.get_mut(tab_idx) {
        tab.music_analysis_draft.stems_dir_override = match args.stems_dir.as_ref() {
            Some(path) => Some(absolute_output_path(path)?),
            None => prefer_demucs_override.clone(),
        };
        tab.music_analysis_draft.last_error = None;
    }
    workspace.app.start_music_analysis_for_tab(tab_idx);
    if workspace.app.music_ai_state.is_some() {
        workspace.wait_for_music_analysis()?;
    }
    if let Some(tab) = workspace.app.tabs.get_mut(tab_idx) {
        if prefer_demucs_override.is_some() && args.stems_dir.is_none() {
            tab.music_analysis_draft.stems_dir_override = None;
        }
    }
    let after = {
        let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
        if let Some(err) = tab.music_analysis_draft.last_error.clone() {
            if tab.music_analysis_draft.result.is_none() {
                bail!(err);
            }
        }
        music_analysis_json(&tab.music_analysis_draft)
    };
    if let Some(report) = args.report.as_ref() {
        write_music_analysis_report(report, &after)?;
    }
    workspace.save()?;
    let path = pathbuf_to_string(
        &workspace
            .app
            .tabs
            .get(tab_idx)
            .context("missing target tab")?
            .path,
    );
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": after,
            "mutated_paths": [path],
        }),
        warnings: Vec::new(),
    })
}

fn music_ai_apply_markers(args: MusicAiApplyMarkersArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let tab_idx = workspace.ensure_target_tab_loaded(args.path.as_deref())?;
    let before = {
        let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
        json!({
            "markers": tab.markers.iter().map(marker_json).collect::<Vec<_>>(),
            "analysis": music_analysis_json(&tab.music_analysis_draft),
        })
    };
    let use_any = args.beats || args.downbeats || args.sections;
    if let Some(tab) = workspace.app.tabs.get_mut(tab_idx) {
        if use_any {
            tab.music_analysis_draft.show_beat = args.beats;
            tab.music_analysis_draft.show_downbeat = args.downbeats;
            tab.music_analysis_draft.show_section = args.sections;
        }
    }
    workspace.app.rebuild_music_provisional_markers_for_tab(tab_idx);
    if args.replace {
        if let Some(tab) = workspace.app.tabs.get_mut(tab_idx) {
            tab.markers.clear();
            tab.markers_committed.clear();
            tab.markers_applied.clear();
        }
    }
    workspace.app.apply_music_analysis_markers_to_tab(tab_idx);
    workspace.save()?;
    let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": {
                "markers": tab.markers.iter().map(marker_json).collect::<Vec<_>>(),
                "analysis": music_analysis_json(&tab.music_analysis_draft),
            },
            "generated_markers": tab.music_analysis_draft.provisional_markers.iter().map(marker_json).collect::<Vec<_>>(),
            "mutated_paths": [pathbuf_to_string(&tab.path)],
        }),
        warnings: Vec::new(),
    })
}

fn music_ai_export_stems(args: MusicAiExportStemsArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let tab_idx = workspace.ensure_target_tab_loaded(args.path.as_deref())?;
    let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
    let stem_paths = super::music_onnx::resolve_stem_paths(
        tab.path.as_path(),
        tab.music_analysis_draft.stems_dir_override.as_deref(),
    );
    if !stem_paths.is_ready() {
        bail!(
            "stems are not ready: missing={} searched={}",
            stem_paths.missing.join(", "),
            stem_paths
                .searched_roots
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
    let output_dir = absolute_output_path(&args.output_dir)?;
    std::fs::create_dir_all(&output_dir)?;
    let source_name = tab
        .path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("audio");
    let naming_template = args
        .naming_template
        .unwrap_or_else(|| "{name}_{stem}.wav".to_string());
    let mut exported_files = Vec::new();
    for (stem, source) in [
        ("bass", stem_paths.bass.clone()),
        ("drums", stem_paths.drums.clone()),
        ("other", stem_paths.other.clone()),
        ("vocals", stem_paths.vocals.clone()),
    ] {
        let mut file_name = naming_template
            .replace("{name}", source_name)
            .replace("{stem}", stem);
        if Path::new(&file_name).extension().is_none() {
            file_name.push_str(".wav");
        }
        let dst = output_dir.join(file_name);
        std::fs::copy(&source, &dst)
            .with_context(|| format!("copy stem {} -> {}", source.display(), dst.display()))?;
        exported_files.push(pathbuf_to_string(&dst));
    }
    Ok(CliCommandOutput {
        result: json!({
            "source": pathbuf_to_string(&tab.path),
            "output_dir": absolute_string(&output_dir)?,
            "exported_files": exported_files,
        }),
        warnings: Vec::new(),
    })
}

fn resolve_transcript_target_and_language(
    args: &TranscriptInspectArgs,
) -> Result<(PathBuf, Option<String>)> {
    match (args.input.as_deref(), args.session.as_deref()) {
        (Some(input), None) => Ok((absolute_existing_path(input)?, None)),
        (None, Some(session_path)) => {
            let session = load_session(session_path)?;
            let audio_path = if let Some(path) = args.path.as_deref() {
                absolute_output_path(path)?
            } else if let Some(idx) = session.project.active_tab {
                session
                    .project
                    .tabs
                    .get(idx)
                    .map(|tab| project::resolve_path(&tab.path, &session.base_dir))
                    .or_else(|| session_list_entries(&session).first().map(|entry| entry.path.clone()))
                    .context("session does not contain any target audio")?
            } else if let Some(tab) = session.project.tabs.first() {
                project::resolve_path(&tab.path, &session.base_dir)
            } else if let Some(entry) = session_list_entries(&session).first() {
                entry.path.clone()
            } else {
                bail!("session does not contain any target audio");
            };
            let audio_key = path_key(&audio_path);
            let language = session
                .project
                .list
                .transcript_languages
                .iter()
                .find(|entry| {
                    path_key(&project::resolve_path(&entry.path, &session.base_dir)) == audio_key
                })
                .map(|entry| entry.language.clone());
            Ok((audio_path, language))
        }
        _ => bail!("transcript inspect requires exactly one of --input or --session"),
    }
}

fn resolve_transcript_export_target(
    args: &TranscriptExportSrtArgs,
) -> Result<(PathBuf, Option<String>)> {
    let inspect_args = TranscriptInspectArgs {
        input: args.input.clone(),
        session: args.session.clone(),
        path: args.path.clone(),
    };
    resolve_transcript_target_and_language(&inspect_args)
}

fn external_key_rule_string(rule: super::types::ExternalKeyRule) -> &'static str {
    match rule {
        super::types::ExternalKeyRule::FileName => "file",
        super::types::ExternalKeyRule::Stem => "stem",
        super::types::ExternalKeyRule::Regex => "regex",
    }
}

fn external_regex_input_string(input: super::types::ExternalRegexInput) -> &'static str {
    match input {
        super::types::ExternalRegexInput::FileName => "file",
        super::types::ExternalRegexInput::Stem => "stem",
        super::types::ExternalRegexInput::Path => "path",
        super::types::ExternalRegexInput::Dir => "dir",
    }
}

fn external_source_json(index: usize, source: &super::types::ExternalSource) -> Value {
    json!({
        "index": index,
        "path": pathbuf_to_string(&source.path),
        "sheet_name": source.sheet_name,
        "sheet_names": source.sheet_names,
        "has_header": source.has_header,
        "header_row": source.header_row.map(|row| row + 1),
        "data_row": source.data_row.map(|row| row + 1),
        "row_count": source.rows.len(),
        "headers": source.headers,
    })
}

fn external_config_json(app: &WavesPreviewer) -> Value {
    let key_column = app
        .external_key_index
        .and_then(|idx| app.external_headers.get(idx))
        .cloned();
    json!({
        "active_source": app.external_active_source,
        "key_column": key_column,
        "key_rule": external_key_rule_string(app.external_key_rule),
        "regex_input": external_regex_input_string(app.external_match_input),
        "regex": app.external_match_regex,
        "replace": app.external_match_replace,
        "scope_regex": app.external_scope_regex,
        "visible_columns": app.external_visible_columns,
        "show_unmatched": app.external_show_unmatched,
        "match_count": app.external_match_count,
        "unmatched_count": app.external_unmatched_count,
    })
}

fn external_state_json(app: &WavesPreviewer) -> Value {
    json!({
        "sources": app
            .external_sources
            .iter()
            .enumerate()
            .map(|(index, source)| external_source_json(index, source))
            .collect::<Vec<_>>(),
        "active_source": app.external_active_source,
        "headers": app.external_headers,
        "visible_columns": app.external_visible_columns,
        "merged_row_count": app.external_rows.len(),
        "match_count": app.external_match_count,
        "unmatched_count": app.external_unmatched_count,
        "unmatched_rows": app.external_unmatched_rows,
        "load_error": app.external_load_error,
        "config": external_config_json(app),
    })
}

fn external_row_json(
    headers: &[String],
    row: &[String],
    unmatched: bool,
    row_index: usize,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("row_index".to_string(), json!(row_index));
    map.insert("unmatched".to_string(), json!(unmatched));
    let mut values = Map::new();
    for (idx, header) in headers.iter().enumerate() {
        values.insert(
            header.clone(),
            Value::String(row.get(idx).cloned().unwrap_or_default()),
        );
    }
    map.insert("values".to_string(), Value::Object(values));
    map
}

fn external_resolved_row_json(path: &Path, row: &HashMap<String, String>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("row_id".to_string(), json!(stable_row_id_for_path(path)));
    map.insert("path".to_string(), json!(pathbuf_to_string(path)));
    let mut values = Map::new();
    let mut keys: Vec<_> = row.keys().cloned().collect();
    keys.sort();
    for key in keys {
        values.insert(
            key.clone(),
            Value::String(row.get(&key).cloned().unwrap_or_default()),
        );
    }
    map.insert("values".to_string(), Value::Object(values));
    map
}

fn transcript_segment_json(segment: &super::types::TranscriptSegment) -> Value {
    json!({
        "start_ms": segment.start_ms,
        "end_ms": segment.end_ms,
        "text": segment.text,
    })
}

fn transcript_perf_mode_string(mode: super::types::TranscriptPerfMode) -> &'static str {
    match mode {
        super::types::TranscriptPerfMode::Stable => "stable",
        super::types::TranscriptPerfMode::Balanced => "balanced",
        super::types::TranscriptPerfMode::Boost => "boost",
    }
}

fn transcript_model_variant_string(
    variant: super::types::TranscriptModelVariant,
) -> &'static str {
    match variant {
        super::types::TranscriptModelVariant::Auto => "auto",
        super::types::TranscriptModelVariant::Fp16 => "fp16",
        super::types::TranscriptModelVariant::Quantized => "quantized",
    }
}

fn transcript_compute_target_string(
    target: super::types::TranscriptComputeTarget,
) -> &'static str {
    match target {
        super::types::TranscriptComputeTarget::Auto => "auto",
        super::types::TranscriptComputeTarget::Cpu => "cpu",
        super::types::TranscriptComputeTarget::Gpu => "gpu",
        super::types::TranscriptComputeTarget::Npu => "npu",
    }
}

fn transcript_config_json(cfg: &super::types::TranscriptAiConfig) -> Value {
    json!({
        "language": cfg.language,
        "task": cfg.task,
        "max_new_tokens": cfg.max_new_tokens,
        "overwrite_existing_srt": cfg.overwrite_existing_srt,
        "perf_mode": transcript_perf_mode_string(cfg.perf_mode),
        "model_variant": transcript_model_variant_string(cfg.model_variant),
        "omit_language_token": cfg.omit_language_token,
        "omit_notimestamps_token": cfg.omit_notimestamps_token,
        "vad_enabled": cfg.vad_enabled,
        "vad_model_path": cfg.vad_model_path.as_ref().map(|path| pathbuf_to_string(path)),
        "vad_threshold": cfg.vad_threshold,
        "vad_min_speech_ms": cfg.vad_min_speech_ms,
        "vad_min_silence_ms": cfg.vad_min_silence_ms,
        "vad_speech_pad_ms": cfg.vad_speech_pad_ms,
        "max_window_ms": cfg.max_window_ms,
        "no_speech_threshold": cfg.no_speech_threshold,
        "logprob_threshold": cfg.logprob_threshold,
        "compute_target": transcript_compute_target_string(cfg.compute_target),
        "dml_device_id": cfg.dml_device_id,
        "cpu_intra_threads": cfg.cpu_intra_threads,
    })
}

fn transcript_model_status_json(app: &WavesPreviewer) -> Value {
    json!({
        "model_dir": app.transcript_ai_model_dir.as_ref().map(|path| pathbuf_to_string(path)),
        "available": app.transcript_ai_available,
        "can_uninstall": app.transcript_ai_can_uninstall(),
        "supported_languages": app.transcript_supported_languages,
        "supported_tasks": app.transcript_supported_tasks,
        "effective_vad_model_path": app.transcript_ai_effective_vad_model_path().as_ref().map(|path| pathbuf_to_string(path)),
        "estimated_parallel_workers": app.transcript_estimated_parallel_workers(),
        "config": transcript_config_json(&app.transcript_ai_cfg),
        "last_error": app.transcript_ai_last_error,
    })
}

fn music_analysis_source_kind_string(kind: super::types::MusicAnalysisSourceKind) -> &'static str {
    match kind {
        super::types::MusicAnalysisSourceKind::StemsDir => "stems_dir",
        super::types::MusicAnalysisSourceKind::AutoDemucs => "auto_demucs",
    }
}

fn music_analysis_json(draft: &super::types::MusicAnalysisDraft) -> Value {
    json!({
        "has_result": draft.result.is_some(),
        "beats": draft.result.as_ref().map(|result| result.beats.clone()).unwrap_or_default(),
        "downbeats": draft.result.as_ref().map(|result| result.downbeats.clone()).unwrap_or_default(),
        "sections": draft.result.as_ref().map(|result| {
            result
                .sections
                .iter()
                .map(|(sample, label)| json!({"sample": sample, "label": label}))
                .collect::<Vec<_>>()
        }).unwrap_or_default(),
        "estimated_bpm": draft.result.as_ref().and_then(|result| result.estimated_bpm),
        "show_beat": draft.show_beat,
        "show_downbeat": draft.show_downbeat,
        "show_section": draft.show_section,
        "analysis_inflight": draft.analysis_inflight,
        "stems_dir_override": draft.stems_dir_override.as_ref().map(|path| pathbuf_to_string(path)),
        "last_error": draft.last_error,
        "analysis_source_len": draft.analysis_source_len,
        "analysis_source_kind": music_analysis_source_kind_string(draft.analysis_source_kind),
        "provisional_markers": draft.provisional_markers.iter().map(marker_json).collect::<Vec<_>>(),
        "preview_active": draft.preview_active,
        "preview_inflight": draft.preview_inflight,
        "preview_error": draft.preview_error,
        "preview_peak_abs": draft.preview_peak_abs,
        "preview_clip_applied": draft.preview_clip_applied,
        "analysis_process_message": draft.analysis_process_message,
    })
}

fn music_model_status_json(app: &WavesPreviewer) -> Value {
    json!({
        "model_dir": app.music_ai_model_dir.as_ref().map(|path| pathbuf_to_string(path)),
        "available": app.music_ai_available,
        "demucs_model_path": app.music_ai_demucs_model_path.as_ref().map(|path| pathbuf_to_string(path)),
        "demucs_available": app.music_ai_has_demucs_model(),
        "can_uninstall": app.music_ai_can_uninstall(),
        "last_error": app.music_ai_last_error,
    })
}

fn wait_for_transcript_model_download_app(app: &mut WavesPreviewer) -> Result<()> {
    let ctx = egui::Context::default();
    let started = Instant::now();
    while app.transcript_model_download_state.is_some() {
        app.drain_transcript_model_download_results(&ctx);
        if started.elapsed() > Duration::from_secs(120) {
            bail!("transcript model download timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn wait_for_music_model_download_app(app: &mut WavesPreviewer) -> Result<()> {
    let ctx = egui::Context::default();
    let started = Instant::now();
    while app.music_model_download_state.is_some() {
        app.drain_music_model_download_results(&ctx);
        if started.elapsed() > Duration::from_secs(120) {
            bail!("music model download timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn wait_for_plugin_scan_app(app: &mut WavesPreviewer) -> Result<()> {
    let ctx = egui::Context::default();
    let started = Instant::now();
    while app.plugin_scan_state.is_some() {
        app.drain_plugin_jobs(&ctx);
        if started.elapsed() > Duration::from_secs(120) {
            bail!("plugin scan timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn write_music_analysis_report(path: &Path, after: &Value) -> Result<()> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("md")
        .to_ascii_lowercase();
    ensure_parent_dir(path)?;
    match ext.as_str() {
        "json" => std::fs::write(path, serde_json::to_string_pretty(after)?)
            .with_context(|| format!("write report: {}", path.display()))?,
        "txt" => {
            let mut text = String::new();
            text.push_str("NeoWaves Music Analysis\n\n");
            text.push_str(&format!(
                "Estimated BPM: {:?}\n",
                after.get("estimated_bpm").cloned().unwrap_or(Value::Null)
            ));
            text.push_str(&format!(
                "Beats: {}\nDownbeats: {}\nSections: {}\n",
                after.get("beats").and_then(Value::as_array).map(|v| v.len()).unwrap_or(0),
                after.get("downbeats").and_then(Value::as_array).map(|v| v.len()).unwrap_or(0),
                after.get("sections").and_then(Value::as_array).map(|v| v.len()).unwrap_or(0),
            ));
            std::fs::write(path, text)
                .with_context(|| format!("write report: {}", path.display()))?;
        }
        _ => {
            let mut text = String::new();
            text.push_str("# NeoWaves Music Analysis\n\n");
            text.push_str(&format!(
                "- Estimated BPM: `{}`\n",
                after
                    .get("estimated_bpm")
                    .map(Value::to_string)
                    .unwrap_or_else(|| "null".to_string())
            ));
            text.push_str(&format!(
                "- Beats: `{}`\n- Downbeats: `{}`\n- Sections: `{}`\n",
                after.get("beats").and_then(Value::as_array).map(|v| v.len()).unwrap_or(0),
                after.get("downbeats").and_then(Value::as_array).map(|v| v.len()).unwrap_or(0),
                after.get("sections").and_then(Value::as_array).map(|v| v.len()).unwrap_or(0),
            ));
            std::fs::write(path, text)
                .with_context(|| format!("write report: {}", path.display()))?;
        }
    }
    Ok(())
}

fn draw_external_rows_image(
    headers: &[String],
    visible_columns: &[String],
    rows: &[Vec<String>],
    unmatched_rows: &HashSet<usize>,
    width: u32,
    height: u32,
) -> RgbaImage {
    let mut image = RgbaImage::from_pixel(width, height, Rgba([18, 20, 24, 255]));
    let mut draw_rect = |x: u32, y: u32, w: u32, h: u32, color: [u8; 4]| {
        let max_x = (x.saturating_add(w)).min(width);
        let max_y = (y.saturating_add(h)).min(height);
        for yy in y..max_y {
            for xx in x..max_x {
                image.put_pixel(xx, yy, Rgba(color));
            }
        }
    };
    let mut columns = Vec::new();
    if let Some(first) = headers.first() {
        columns.push(first.clone());
    }
    for column in visible_columns {
        if headers.iter().any(|header| header == column) && !columns.contains(column) {
            columns.push(column.clone());
        }
    }
    if columns.is_empty() {
        columns.extend(headers.iter().take(4).cloned());
    }
    if columns.is_empty() {
        return image;
    }
    let column_indices: Vec<usize> = columns
        .iter()
        .filter_map(|column| headers.iter().position(|header| header == column))
        .collect();
    let header_h = 32u32;
    let row_h = 20u32;
    let margin = 8u32;
    let usable_w = width.saturating_sub(margin * 2).max(1);
    let col_w = (usable_w / column_indices.len().max(1) as u32).max(1);
    let rows_fit = height
        .saturating_sub(header_h + margin * 2)
        .checked_div(row_h)
        .unwrap_or(0) as usize;
    for (col_idx, _header_idx) in column_indices.iter().enumerate() {
        let x = margin + col_idx as u32 * col_w;
        let hue = (col_idx as u8).wrapping_mul(37);
        draw_rect(
            x,
            margin,
            col_w.saturating_sub(2),
            header_h.saturating_sub(6),
            [40 + hue / 3, 70 + hue / 4, 110 + hue / 5, 255],
        );
    }
    for (row_idx, row) in rows.iter().take(rows_fit).enumerate() {
        let y = margin + header_h + row_idx as u32 * row_h;
        let row_bg = if unmatched_rows.contains(&row_idx) {
            [48, 22, 24, 255]
        } else if row_idx % 2 == 0 {
            [24, 26, 30, 255]
        } else {
            [20, 22, 26, 255]
        };
        draw_rect(margin, y, usable_w, row_h.saturating_sub(2), row_bg);
        for (col_pos, header_idx) in column_indices.iter().enumerate() {
            let x = margin + col_pos as u32 * col_w;
            let cell_color = if row
                .get(*header_idx)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                [96, 188, 168, 220]
            } else {
                [44, 48, 56, 200]
            };
            draw_rect(x + 3, y + 4, col_w.saturating_sub(8), row_h.saturating_sub(10), cell_color);
        }
    }
    image
}

fn plugin_format_string(format: crate::plugin::PluginFormat) -> &'static str {
    match format {
        crate::plugin::PluginFormat::Vst3 => "vst3",
        crate::plugin::PluginFormat::Clap => "clap",
    }
}

fn plugin_backend_string(backend: crate::plugin::PluginHostBackend) -> &'static str {
    match backend {
        crate::plugin::PluginHostBackend::Generic => "generic",
        crate::plugin::PluginHostBackend::NativeVst3 => "native_vst3",
        crate::plugin::PluginHostBackend::NativeClap => "native_clap",
    }
}

fn plugin_gui_status_string(status: crate::plugin::GuiSessionStatus) -> &'static str {
    match status {
        crate::plugin::GuiSessionStatus::Closed => "closed",
        crate::plugin::GuiSessionStatus::Opening => "opening",
        crate::plugin::GuiSessionStatus::Live => "live",
        crate::plugin::GuiSessionStatus::Error => "error",
    }
}

fn plugin_capabilities_json(capabilities: &crate::plugin::GuiCapabilities) -> Value {
    json!({
        "supports_native_gui": capabilities.supports_native_gui,
        "supports_param_feedback": capabilities.supports_param_feedback,
        "supports_state_sync": capabilities.supports_state_sync,
    })
}

fn plugin_catalog_entry_json(entry: &super::types::PluginCatalogEntry) -> Value {
    json!({
        "key": entry.key,
        "name": entry.name,
        "path": pathbuf_to_string(&entry.path),
        "format": plugin_format_string(entry.format),
    })
}

fn plugin_draft_json(draft: &super::types::PluginFxDraft) -> Value {
    json!({
        "plugin_key": draft.plugin_key,
        "plugin_name": draft.plugin_name,
        "backend": draft.backend.map(plugin_backend_string),
        "gui_capabilities": plugin_capabilities_json(&draft.gui_capabilities),
        "gui_status": plugin_gui_status_string(draft.gui_status),
        "enabled": draft.enabled,
        "bypass": draft.bypass,
        "filter": draft.filter,
        "params": draft.params.iter().map(|param| {
            json!({
                "id": param.id,
                "name": param.name,
                "normalized": param.normalized,
                "default_normalized": param.default_normalized,
                "min": param.min,
                "max": param.max,
                "unit": param.unit,
            })
        }).collect::<Vec<_>>(),
        "state_blob_b64": draft.state_blob.as_ref().map(|bytes| {
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes)
        }),
        "last_error": draft.last_error,
        "last_backend_log": draft.last_backend_log,
    })
}

fn resolve_plugin_probe_path(app: &mut WavesPreviewer, plugin: &str) -> Result<PathBuf> {
    let plugin_path = Path::new(plugin);
    if plugin_path.components().count() > 1 || plugin_path.is_absolute() {
        let abs = absolute_output_path(plugin_path)?;
        if abs.exists() {
            return Ok(abs);
        }
    }
    if app.plugin_catalog.is_empty() && app.plugin_scan_state.is_none() {
        app.spawn_plugin_scan();
        wait_for_plugin_scan_app(app)?;
    }
    let plugin_lower = plugin.to_ascii_lowercase();
    if let Some(entry) = app.plugin_catalog.iter().find(|entry| {
        entry.key.eq_ignore_ascii_case(plugin)
            || entry.name.eq_ignore_ascii_case(plugin)
            || entry
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.eq_ignore_ascii_case(plugin))
                .unwrap_or(false)
            || entry.path.to_string_lossy().to_ascii_lowercase() == plugin_lower
    }) {
        return Ok(entry.path.clone());
    }
    if let Ok(abs) = absolute_output_path(plugin_path) {
        if abs.exists() {
            return Ok(abs);
        }
    }
    if let Some(err) = app.plugin_scan_error.as_ref() {
        bail!("plugin not found: {plugin} ({err})");
    }
    bail!("plugin not found: {plugin}")
}

fn probe_plugin_path(
    plugin_path: &Path,
) -> Result<(
    String,
    String,
    Vec<super::types::PluginParamUiState>,
    Option<Vec<u8>>,
    crate::plugin::PluginHostBackend,
    crate::plugin::GuiCapabilities,
    Option<String>,
)> {
    let req = WorkerRequest::Probe {
        plugin_path: pathbuf_to_string(plugin_path),
    };
    match crate::plugin::client::run_request(&req) {
        Ok(WorkerResponse::ProbeResult {
            plugin,
            params,
            state_blob_b64,
            backend,
            capabilities,
            backend_note,
        }) => {
            let ui_params = params
                .into_iter()
                .map(|param| super::types::PluginParamUiState {
                    id: param.id,
                    name: param.name,
                    normalized: param.normalized.clamp(0.0, 1.0),
                    default_normalized: param.default_normalized.clamp(0.0, 1.0),
                    min: param.min,
                    max: param.max,
                    unit: param.unit,
                })
                .collect::<Vec<_>>();
            let state_blob = state_blob_b64.and_then(|raw| {
                base64::engine::general_purpose::STANDARD_NO_PAD
                    .decode(raw.as_bytes())
                    .ok()
            });
            Ok((
                plugin.key,
                plugin.name,
                ui_params,
                state_blob,
                backend,
                capabilities,
                backend_note,
            ))
        }
        Ok(WorkerResponse::Error { message }) => bail!(message),
        Ok(other) => bail!("plugin probe: unexpected worker response: {other:?}"),
        Err(err) => bail!(err),
    }
}

fn apply_plugin_param_overrides(
    draft: &mut super::types::PluginFxDraft,
    params: &[String],
) -> Result<()> {
    for raw in params {
        let mut parts = raw.splitn(2, '=');
        let id = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("plugin param override requires id=value")?;
        let value = parts
            .next()
            .context("plugin param override requires id=value")?
            .trim()
            .parse::<f32>()
            .with_context(|| format!("parse plugin param value: {raw}"))?
            .clamp(0.0, 1.0);
        let param = draft
            .params
            .iter_mut()
            .find(|param| param.id == id)
            .with_context(|| format!("plugin param not found: {id}"))?;
        param.normalized = value;
    }
    Ok(())
}

fn plugin_search_path_list(_: PluginSearchPathListArgs) -> Result<CliCommandOutput> {
    let app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    Ok(CliCommandOutput {
        result: json!({
            "search_paths": app
                .plugin_search_paths
                .iter()
                .enumerate()
                .map(|(index, path)| json!({"index": index, "path": pathbuf_to_string(path)}))
                .collect::<Vec<_>>(),
        }),
        warnings: Vec::new(),
    })
}

fn plugin_search_path_add(args: PluginSearchPathAddArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    let before = app
        .plugin_search_paths
        .iter()
        .map(|path| pathbuf_to_string(path))
        .collect::<Vec<_>>();
    let path = absolute_existing_path(&args.path)?;
    let _changed = app.add_plugin_search_path(path);
    app.save_prefs();
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": app.plugin_search_paths.iter().map(|path| pathbuf_to_string(path)).collect::<Vec<_>>(),
        }),
        warnings: Vec::new(),
    })
}

fn plugin_search_path_remove(args: PluginSearchPathRemoveArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    let before = app
        .plugin_search_paths
        .iter()
        .map(|path| pathbuf_to_string(path))
        .collect::<Vec<_>>();
    let removed = match (args.index, args.path.as_deref()) {
        (Some(index), None) => app.remove_plugin_search_path_at(index),
        (None, Some(path)) => {
            let target = absolute_output_path(path)?;
            let target_key = path_key(&target);
            let index = app
                .plugin_search_paths
                .iter()
                .position(|candidate| path_key(candidate) == target_key)
                .context("plugin search path not found")?;
            app.remove_plugin_search_path_at(index)
        }
        _ => bail!("plugin search-path remove requires exactly one of --index or --path"),
    };
    if !removed {
        bail!("plugin search path remove failed");
    }
    app.save_prefs();
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": app.plugin_search_paths.iter().map(|path| pathbuf_to_string(path)).collect::<Vec<_>>(),
        }),
        warnings: Vec::new(),
    })
}

fn plugin_search_path_reset(_: PluginSearchPathResetArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    let before = app
        .plugin_search_paths
        .iter()
        .map(|path| pathbuf_to_string(path))
        .collect::<Vec<_>>();
    app.reset_plugin_search_paths_to_default();
    app.save_prefs();
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": app.plugin_search_paths.iter().map(|path| pathbuf_to_string(path)).collect::<Vec<_>>(),
        }),
        warnings: Vec::new(),
    })
}

fn plugin_scan(_: PluginScanArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    app.spawn_plugin_scan();
    wait_for_plugin_scan_app(&mut app)?;
    let warnings = app.plugin_scan_error.clone().into_iter().collect::<Vec<_>>();
    Ok(CliCommandOutput {
        result: json!({
            "search_paths": app.plugin_search_paths.iter().map(|path| pathbuf_to_string(path)).collect::<Vec<_>>(),
            "plugins": app.plugin_catalog.iter().map(plugin_catalog_entry_json).collect::<Vec<_>>(),
            "scan_error": app.plugin_scan_error,
        }),
        warnings,
    })
}

fn plugin_list(args: PluginListArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    app.spawn_plugin_scan();
    wait_for_plugin_scan_app(&mut app)?;
    let filter = args.filter.as_deref().map(|value| value.to_ascii_lowercase());
    let plugins = app
        .plugin_catalog
        .iter()
        .filter(|entry| {
            filter.as_ref().map_or(true, |needle| {
                entry.key.to_ascii_lowercase().contains(needle)
                    || entry.name.to_ascii_lowercase().contains(needle)
                    || entry.path.to_string_lossy().to_ascii_lowercase().contains(needle)
            })
        })
        .map(plugin_catalog_entry_json)
        .collect::<Vec<_>>();
    let warnings = app.plugin_scan_error.clone().into_iter().collect::<Vec<_>>();
    Ok(CliCommandOutput {
        result: json!({
            "plugins": plugins,
            "scan_error": app.plugin_scan_error,
        }),
        warnings,
    })
}

fn plugin_probe(args: PluginProbeArgs) -> Result<CliCommandOutput> {
    let mut app = WavesPreviewer::new_headless(super::StartupConfig::default())?;
    let plugin_path = resolve_plugin_probe_path(&mut app, &args.plugin)?;
    let (plugin_key, plugin_name, params, state_blob, backend, capabilities, backend_note) =
        probe_plugin_path(&plugin_path)?;
    Ok(CliCommandOutput {
        result: json!({
            "plugin": {
                "key": plugin_key,
                "name": plugin_name,
                "path": pathbuf_to_string(&plugin_path),
            },
            "backend": plugin_backend_string(backend),
            "capabilities": plugin_capabilities_json(&capabilities),
            "backend_note": backend_note,
            "state_blob_b64": state_blob.as_ref().map(|bytes| base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes)),
            "params": params.iter().map(|param| {
                json!({
                    "id": param.id,
                    "name": param.name,
                    "normalized": param.normalized,
                    "default_normalized": param.default_normalized,
                    "min": param.min,
                    "max": param.max,
                    "unit": param.unit,
                })
            }).collect::<Vec<_>>(),
        }),
        warnings: Vec::new(),
    })
}

fn plugin_session_inspect(args: PluginSessionInspectArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let tab_idx = workspace.ensure_target_tab_loaded(args.path.as_deref())?;
    let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
    Ok(CliCommandOutput {
        result: json!({
            "path": pathbuf_to_string(&tab.path),
            "draft": plugin_draft_json(&tab.plugin_fx_draft),
        }),
        warnings: Vec::new(),
    })
}

fn plugin_session_set(args: PluginSessionSetArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let tab_idx = workspace.ensure_target_tab_loaded(args.path.as_deref())?;
    let before = {
        let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
        plugin_draft_json(&tab.plugin_fx_draft)
    };
    if let Some(plugin) = args.plugin.as_deref() {
        let plugin_path = resolve_plugin_probe_path(&mut workspace.app, plugin)?;
        let (plugin_key, plugin_name, params, state_blob, backend, capabilities, backend_note) =
            probe_plugin_path(&plugin_path)?;
        let tab = workspace
            .app
            .tabs
            .get_mut(tab_idx)
            .context("missing target tab")?;
        tab.plugin_fx_draft.plugin_key = Some(plugin_key);
        tab.plugin_fx_draft.plugin_name = plugin_name;
        tab.plugin_fx_draft.params = params;
        tab.plugin_fx_draft.state_blob = state_blob;
        tab.plugin_fx_draft.backend = Some(backend);
        tab.plugin_fx_draft.gui_capabilities = capabilities;
        tab.plugin_fx_draft.gui_status = crate::plugin::GuiSessionStatus::Closed;
        tab.plugin_fx_draft.enabled = true;
        tab.plugin_fx_draft.bypass = false;
        tab.plugin_fx_draft.last_error = None;
        tab.plugin_fx_draft.last_backend_log = backend_note;
    }
    {
        let tab = workspace
            .app
            .tabs
            .get_mut(tab_idx)
            .context("missing target tab")?;
        if let Some(enabled) = args.enabled {
            tab.plugin_fx_draft.enabled = enabled.into_bool();
        }
        if let Some(bypass) = args.bypass {
            tab.plugin_fx_draft.bypass = bypass.into_bool();
        }
        if !args.params.is_empty() {
            apply_plugin_param_overrides(&mut tab.plugin_fx_draft, &args.params)?;
        }
        if let Some(state_blob_b64) = args.state_blob_b64.as_deref() {
            tab.plugin_fx_draft.state_blob = Some(
                base64::engine::general_purpose::STANDARD_NO_PAD
                    .decode(state_blob_b64.as_bytes())
                    .with_context(|| "decode plugin state blob")?,
            );
        }
    }
    workspace.save()?;
    let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": plugin_draft_json(&tab.plugin_fx_draft),
            "mutated_paths": [pathbuf_to_string(&tab.path)],
        }),
        warnings: Vec::new(),
    })
}

fn plugin_session_preview(args: PluginSessionPreviewArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let tab_idx = workspace.ensure_target_tab_loaded(args.path.as_deref())?;
    let before = {
        let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
        plugin_draft_json(&tab.plugin_fx_draft)
    };
    if workspace.app.plugin_catalog.is_empty() {
        workspace.app.spawn_plugin_scan();
        let _ = workspace.wait_for_plugin_scan();
    }
    workspace.app.spawn_plugin_preview_for_tab(tab_idx);
    workspace.wait_for_plugin_process()?;
    workspace.save()?;
    let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
    if let Some(err) = tab.plugin_fx_draft.last_error.clone() {
        if tab.preview_overlay.is_none() {
            bail!(err);
        }
    }
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": plugin_draft_json(&tab.plugin_fx_draft),
            "preview_overlay_ready": tab.preview_overlay.is_some(),
            "preview_audio_tool": tab.preview_audio_tool.map(|tool| format!("{:?}", tool)),
            "mutated_paths": [pathbuf_to_string(&tab.path)],
        }),
        warnings: Vec::new(),
    })
}

fn plugin_session_apply(args: PluginSessionApplyArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let tab_idx = workspace.ensure_target_tab_loaded(args.path.as_deref())?;
    let before = {
        let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
        json!({
            "draft": plugin_draft_json(&tab.plugin_fx_draft),
            "dirty": tab.dirty,
            "samples_len": tab.samples_len,
            "channels": tab.ch_samples.len(),
        })
    };
    if workspace.app.plugin_catalog.is_empty() {
        workspace.app.spawn_plugin_scan();
        let _ = workspace.wait_for_plugin_scan();
    }
    workspace.app.spawn_plugin_apply_for_tab(tab_idx);
    workspace.wait_for_plugin_process()?;
    workspace.save()?;
    let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
    if let Some(err) = tab.plugin_fx_draft.last_error.clone() {
        bail!(err);
    }
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": {
                "draft": plugin_draft_json(&tab.plugin_fx_draft),
                "dirty": tab.dirty,
                "samples_len": tab.samples_len,
                "channels": tab.ch_samples.len(),
            },
            "mutated_paths": [pathbuf_to_string(&tab.path)],
        }),
        warnings: Vec::new(),
    })
}

fn plugin_session_clear(args: PluginSessionClearArgs) -> Result<CliCommandOutput> {
    let mut workspace = CliWorkspace::load(&args.session)?;
    let tab_idx = workspace.ensure_target_tab_loaded(args.path.as_deref())?;
    let before = {
        let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
        plugin_draft_json(&tab.plugin_fx_draft)
    };
    let tab = workspace
        .app
        .tabs
        .get_mut(tab_idx)
        .context("missing target tab")?;
    tab.plugin_fx_draft = super::types::PluginFxDraft::default();
    tab.preview_audio_tool = None;
    tab.preview_overlay = None;
    workspace.save()?;
    let tab = workspace.app.tabs.get(tab_idx).context("missing target tab")?;
    Ok(CliCommandOutput {
        result: json!({
            "before": before,
            "after": plugin_draft_json(&tab.plugin_fx_draft),
            "mutated_paths": [pathbuf_to_string(&tab.path)],
        }),
        warnings: Vec::new(),
    })
}
