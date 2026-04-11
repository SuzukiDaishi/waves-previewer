use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use image::{ColorType, ImageBuffer, ImageEncoder, Rgba, RgbaImage};
use serde::Serialize;
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
    EditorPrimaryView, EditorSpecSubView, ListColumnConfig, LoopMode, LoopXfadeShape,
    SpectrogramConfig, SpectrogramData, ToolKind, ToolState, ViewMode,
};
use super::WavesPreviewer;
use crate::audio_io::{
    build_wav_proxy_preview, decode_audio_mono, decode_audio_multi, is_supported_audio_path,
    read_audio_info, read_embedded_artwork, AudioInfo, AudioProxyPreview,
};
use crate::cli::{
    CliCommand, CliLoopXfadeShape, CliRoot, CliSpectralViewMode, CliToggle, DebugCommand,
    DebugScreenshotArgs, DebugSummaryArgs, EditorCommand,
    EditorInspectArgs, EditorLoopApplyArgs, EditorLoopClearArgs, EditorLoopCommand,
    EditorLoopGetArgs, EditorLoopModeArgs, EditorLoopRepeatArgs, EditorLoopSetArgs,
    EditorLoopXfadeArgs, EditorMarkersAddArgs, EditorMarkersApplyArgs, EditorMarkersClearArgs,
    EditorMarkersCommand, EditorMarkersListArgs, EditorMarkersRemoveArgs, EditorMarkersSetArgs,
    EditorPlaybackCommand, EditorPlaybackPlayArgs, EditorSelectionClearArgs,
    EditorSelectionCommand, EditorSelectionGetArgs, EditorSelectionSetArgs, EditorSourceArgs,
    EditorToolApplyArgs, EditorToolCommand, EditorToolGetArgs, EditorToolSetArgs,
    EditorViewCommand, EditorViewGetArgs, EditorViewSetArgs, ExportCommand, ExportFileArgs,
    ItemArtworkArgs, ItemCommand, ItemInspectArgs, ItemMetaArgs, ListColumnsArgs, ListCommand,
    ListQueryArgs, ListRenderArgs, ListSourceArgs, RenderCommand, RenderEditorArgs,
    RenderListArgs, RenderSpectrumArgs, RenderWaveformArgs, SessionCommand, SessionInspectArgs,
    SessionNewArgs,
};
use crate::loop_markers;
use crate::markers::{self, MarkerEntry};
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
        CliCommand::Editor(cmd) => dispatch_editor(cmd),
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
        CliCommand::Render(RenderCommand::Waveform(_)) => "render.waveform",
        CliCommand::Render(RenderCommand::Spectrum(_)) => "render.spectrum",
        CliCommand::Render(RenderCommand::Editor(_)) => "render.editor",
        CliCommand::Render(RenderCommand::List(_)) => "render.list",
        CliCommand::Export(ExportCommand::File(_)) => "export.file",
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
        ListCommand::Render(args) => list_render(args),
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
    let columns = parse_list_column_keys(&args.columns)?;
    let source = load_list_source(&args.source)?;
    let mut rows = list_rows_from_source(&source, args.include_overlays)?;
    apply_list_query_filter_sort(
        &mut rows,
        args.query.as_deref(),
        args.sort_key.as_deref(),
        args.sort_dir.as_deref(),
    );
    let total = rows.len();
    let rows = slice_rows(rows, args.offset, args.limit);
    Ok(CliCommandOutput {
        result: json!({
            "total": total,
            "offset": args.offset,
            "limit": args.limit,
            "columns": columns,
            "rows": rows.into_iter().map(Value::Object).collect::<Vec<_>>(),
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
    let session_path = args
        .source
        .session
        .as_deref()
        .context("loop apply requires --session")?;
    let mut workspace = CliWorkspace::load(session_path)?;
    workspace.apply_loop_for_target(args.source.path.as_deref())?;
    workspace.save()?;
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
    let input = absolute_existing_path(&args.input)?;
    let info = read_audio_info(&input)?;
    let channels = load_waveform_channels(&input, args.width as usize, args.mixdown)?;
    let image = draw_waveform_image(
        &channels,
        infer_total_frames(&info).unwrap_or(0),
        args.width.max(16),
        args.height.max(16),
        normalized_markers(&input, &info)?,
        normalized_loop(&input, &info),
    );
    let output = prepare_output_path(args.output, "waveform", "png")?;
    save_rgba_image(&image, &output)?;
    Ok(CliCommandOutput {
        result: json!({
            "path": absolute_string(&output)?,
            "width": image.width(),
            "height": image.height(),
            "source": pathbuf_to_string(&input),
            "view_params": {
                "mixdown": args.mixdown,
                "channels_rendered": channels.len(),
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
    Ok(CliCommandOutput {
        result: json!({
            "source": pathbuf_to_string(&input),
            "destination": absolute_string(&output)?,
            "mode": "new_file",
            "saved_markers": markers.iter().map(marker_json).collect::<Vec<_>>(),
            "saved_loop": loop_region,
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
    Ok(CliCommandOutput {
        result: json!({
            "source": pathbuf_to_string(&src),
            "destination": pathbuf_to_string(&dst),
            "mode": if args.overwrite { "overwrite" } else { "new_file" },
            "saved_markers": markers.iter().map(marker_json).collect::<Vec<_>>(),
            "saved_loop": loop_region,
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

fn load_waveform_channels(path: &Path, width: usize, mixdown: bool) -> Result<Vec<Vec<f32>>> {
    if let Some(proxy) = build_wav_proxy_preview(path, width.saturating_mul(8).max(2048))? {
        return Ok(proxy_channels(proxy, mixdown));
    }
    let (mut channels, _) = decode_audio_multi(path)?;
    if mixdown {
        channels = vec![mixdown_channels(&channels)];
    }
    Ok(channels)
}

fn proxy_channels(proxy: AudioProxyPreview, mixdown: bool) -> Vec<Vec<f32>> {
    if mixdown {
        vec![mixdown_channels(&proxy.channels)]
    } else {
        proxy.channels
    }
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
