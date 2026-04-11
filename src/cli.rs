use std::path::{Path, PathBuf};

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::app;

const ROOT_AFTER_HELP: &str = r#"Examples:
  neowaves
  neowaves --open-file .\demo.wav
  neowaves --open-session .\work.nwsess
  neowaves --cli list query --folder .\assets\audio
  neowaves --cli editor inspect --input .\demo.wav
  neowaves --cli render waveform --input .\demo.wav --output .\out\wave.png

See docs/CLI_MASTER_PLAN.md and docs/CLI_COMMAND_REFERENCE.md for the full CLI contract."#;

const CLI_AFTER_HELP: &str = r#"Examples:
  neowaves --cli session inspect --session .\work.nwsess
  neowaves --cli list query --folder .\assets\audio --columns file,length,sample_rate
  neowaves --cli render list --folder .\assets\audio --output .\out\list.png
  neowaves --cli editor inspect --input .\demo.wav
  neowaves --cli editor playback play --session .\work.nwsess --selection
  neowaves --cli render spectrum --input .\demo.wav --view-mode mel

stdout is JSON. Human-readable diagnostics are written to stderr."#;

const SESSION_NEW_AFTER_HELP: &str = r#"Examples:
  neowaves --cli session new --folder .\assets\audio --output .\work.nwsess
  neowaves --cli session new --input .\a.wav --input .\b.wav --output .\work.nwsess"#;

const SESSION_INSPECT_AFTER_HELP: &str = r#"Examples:
  neowaves --cli session inspect --session .\work.nwsess"#;

const LIST_QUERY_AFTER_HELP: &str = r#"Examples:
  neowaves --cli list query --folder .\assets\audio
  neowaves --cli list query --session .\work.nwsess --query battle --sort-key file
  neowaves --cli list query --folder .\assets\audio --include-overlays"#;

const EDITOR_INSPECT_AFTER_HELP: &str = r#"Examples:
  neowaves --cli editor inspect --input .\demo.wav
  neowaves --cli editor inspect --session .\work.nwsess --path .\demo.wav"#;

const EDITOR_PLAYBACK_PLAY_AFTER_HELP: &str = r#"Examples:
  neowaves --cli editor playback play --input .\demo.wav
  neowaves --cli editor playback play --session .\work.nwsess --selection
  neowaves --cli editor playback play --session .\work.nwsess --loop --rate 0.8"#;

const EDITOR_TOOL_SET_AFTER_HELP: &str = r#"Examples:
  neowaves --cli editor tool set --session .\work.nwsess --tool gain --gain-db -3.0
  neowaves --cli editor tool set --session .\work.nwsess --tool pitch --pitch-semitones 2.5
  neowaves --cli editor tool set --session .\work.nwsess --tool fade --fade-in-ms 250"#;

const RENDER_WAVEFORM_AFTER_HELP: &str = r#"Examples:
  neowaves --cli render waveform --input .\demo.wav --output .\out\wave.png
  neowaves --cli render waveform --input .\demo.wav --mixdown"#;

const RENDER_SPECTRUM_AFTER_HELP: &str = r#"Examples:
  neowaves --cli render spectrum --input .\demo.wav --output .\out\spec.png
  neowaves --cli render spectrum --input .\demo.wav --view-mode mel"#;

const RENDER_EDITOR_AFTER_HELP: &str = r#"Examples:
  neowaves --cli render editor --input .\demo.wav --output .\out\editor.png
  neowaves --cli render editor --session .\work.nwsess --view-mode spec"#;

const RENDER_LIST_AFTER_HELP: &str = r#"Examples:
  neowaves --cli render list --folder .\assets\audio --output .\out\list.png
  neowaves --cli render list --session .\work.nwsess --columns file,length,wave"#;

const EXPORT_FILE_AFTER_HELP: &str = r#"Examples:
  neowaves --cli export file --input .\demo.wav --output .\demo_copy.wav
  neowaves --cli export file --input .\demo.wav --output .\demo_gain.wav --gain-db -3.0
  neowaves --cli export file --session .\work.nwsess --overwrite"#;

pub enum RuntimeMode {
    Gui(app::StartupConfig),
    Cli(CliRoot),
}

pub enum ParseOutcome {
    Run(RuntimeMode),
    Exit(i32),
}

pub fn parse_runtime_mode() -> ParseOutcome {
    let args: Vec<String> = std::env::args().collect();
    let Some(exe) = args.first().cloned() else {
        return ParseOutcome::Exit(2);
    };
    if let Some(cli_idx) = args.iter().position(|arg| arg == "--cli") {
        let mut cli_args = vec![exe];
        cli_args.extend(args.iter().skip(cli_idx + 1).cloned());
        return match CliRoot::try_parse_from(cli_args) {
            Ok(cli) => ParseOutcome::Run(RuntimeMode::Cli(cli)),
            Err(err) => {
                let code = if err.use_stderr() { 2 } else { 0 };
                let _ = err.print();
                ParseOutcome::Exit(code)
            }
        };
    }
    match GuiArgs::try_parse_from(args) {
        Ok(gui) => ParseOutcome::Run(RuntimeMode::Gui(gui.into_startup_config())),
        Err(err) => {
            let code = if err.use_stderr() { 2 } else { 0 };
            let _ = err.print();
            ParseOutcome::Exit(code)
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "neowaves",
    about = "NeoWaves Audio List Editor",
    long_about = "NeoWaves starts the GUI by default. Use --cli to run headless automation commands.",
    disable_help_subcommand = true,
    after_help = ROOT_AFTER_HELP
)]
struct GuiArgs {
    #[arg(long, help = "Run headless CLI mode instead of opening the GUI", action = ArgAction::SetTrue)]
    cli: bool,
    #[arg(long = "open-session", value_name = "SESSION")]
    open_session: Option<PathBuf>,
    #[arg(long = "open-project", value_name = "PROJECT", hide = true)]
    open_project: Option<PathBuf>,
    #[arg(long = "open-folder", value_name = "DIR")]
    open_folder: Option<PathBuf>,
    #[arg(long = "open-file", value_name = "AUDIO")]
    open_file: Vec<PathBuf>,
    #[arg(long = "open-first", action = ArgAction::SetTrue)]
    open_first: bool,
    #[arg(long = "open-view-mode", value_enum)]
    open_view_mode: Option<CliViewMode>,
    #[arg(long = "waveform-overlay", value_enum)]
    open_waveform_overlay: Option<CliToggle>,
    #[arg(long = "screenshot", value_name = "PNG")]
    screenshot_path: Option<PathBuf>,
    #[arg(long = "screenshot-delay", default_value_t = 5)]
    screenshot_delay_frames: u32,
    #[arg(long = "exit-after-screenshot", action = ArgAction::SetTrue)]
    exit_after_screenshot: bool,
    #[arg(long = "dummy-list")]
    dummy_list_count: Option<usize>,
    #[arg(long = "external-dialog", action = ArgAction::SetTrue)]
    external_show_dialog: bool,
    #[arg(long = "debug-summary", value_name = "TXT")]
    debug_summary_path: Option<PathBuf>,
    #[arg(long = "debug-summary-delay", default_value_t = 10)]
    debug_summary_delay_frames: u32,
    #[arg(long = "external-file", value_name = "PATH")]
    external_path: Option<PathBuf>,
    #[arg(long = "external-dummy")]
    external_dummy_rows: Option<usize>,
    #[arg(long = "external-dummy-cols", default_value_t = 6)]
    external_dummy_cols: usize,
    #[arg(long = "external-dummy-path", value_name = "PATH")]
    external_dummy_path: Option<PathBuf>,
    #[arg(long = "external-dummy-merge", action = ArgAction::SetTrue)]
    external_dummy_merge: bool,
    #[arg(long = "external-sheet")]
    external_sheet: Option<String>,
    #[arg(long = "external-has-header", value_enum)]
    external_has_header: Option<CliToggle>,
    #[arg(long = "external-header-row")]
    external_header_row: Option<usize>,
    #[arg(long = "external-data-row")]
    external_data_row: Option<usize>,
    #[arg(long = "external-key-rule", value_enum)]
    external_key_rule: Option<CliExternalKeyRule>,
    #[arg(long = "external-key-input", value_enum)]
    external_key_input: Option<CliExternalRegexInput>,
    #[arg(long = "external-key-regex")]
    external_key_regex: Option<String>,
    #[arg(long = "external-key-replace")]
    external_key_replace: Option<String>,
    #[arg(long = "external-scope-regex")]
    external_scope_regex: Option<String>,
    #[arg(long = "external-show-unmatched", action = ArgAction::SetTrue)]
    external_show_unmatched: bool,
    #[arg(long = "debug", action = ArgAction::SetTrue)]
    debug_enabled: bool,
    #[arg(long = "debug-log", value_name = "PATH")]
    debug_log_path: Option<PathBuf>,
    #[arg(long = "debug-input-trace", action = ArgAction::SetTrue)]
    debug_input_trace: bool,
    #[arg(long = "debug-event-trace", action = ArgAction::SetTrue)]
    debug_event_trace: bool,
    #[arg(long = "debug-input-trace-console", action = ArgAction::SetTrue)]
    debug_input_trace_console: bool,
    #[arg(long = "auto-run", action = ArgAction::SetTrue)]
    auto_run: bool,
    #[arg(long = "auto-run-editor", action = ArgAction::SetTrue)]
    auto_run_editor: bool,
    #[arg(long = "auto-run-pitch-shift")]
    auto_run_pitch_shift: Option<f32>,
    #[arg(long = "auto-run-time-stretch")]
    auto_run_time_stretch: Option<f32>,
    #[arg(long = "auto-run-delay", default_value_t = 8)]
    auto_run_delay_frames: u32,
    #[arg(long = "auto-run-no-exit", action = ArgAction::SetTrue)]
    auto_run_no_exit: bool,
    #[arg(long = "debug-check-interval", default_value_t = 30)]
    debug_check_interval: u32,
    #[arg(long = "no-ipc-forward", hide = true, action = ArgAction::SetTrue)]
    no_ipc_forward: bool,
    #[arg(value_name = "PATH")]
    inputs: Vec<PathBuf>,
}

impl GuiArgs {
    fn into_startup_config(self) -> app::StartupConfig {
        let mut cfg = app::StartupConfig::default();
        let open_project = self.open_session.or(self.open_project);
        cfg.open_project = open_project;
        cfg.open_folder = self.open_folder;
        cfg.open_files = self.open_file;
        cfg.open_first = self.open_first;
        cfg.open_view_mode = self.open_view_mode.map(|mode| mode.into());
        cfg.open_waveform_overlay = self.open_waveform_overlay.map(|flag| flag.into_bool());
        cfg.screenshot_path = self.screenshot_path;
        cfg.screenshot_delay_frames = self.screenshot_delay_frames;
        cfg.exit_after_screenshot = self.exit_after_screenshot;
        cfg.dummy_list_count = self.dummy_list_count;
        cfg.external_path = self.external_path;
        cfg.external_dummy_rows = self.external_dummy_rows;
        cfg.external_dummy_cols = self.external_dummy_cols.max(1);
        cfg.external_dummy_path = self.external_dummy_path;
        cfg.external_dummy_merge = self.external_dummy_merge;
        cfg.external_sheet = self.external_sheet;
        cfg.external_has_header = self.external_has_header.map(|flag| flag.into_bool());
        cfg.external_header_row = self.external_header_row.and_then(|row| row.checked_sub(1));
        cfg.external_data_row = self.external_data_row.and_then(|row| row.checked_sub(1));
        cfg.external_key_rule = self.external_key_rule.map(Into::into);
        cfg.external_key_input = self.external_key_input.map(Into::into);
        cfg.external_key_regex = self.external_key_regex;
        cfg.external_key_replace = self.external_key_replace;
        cfg.external_scope_regex = self.external_scope_regex;
        cfg.external_show_unmatched = self.external_show_unmatched;
        cfg.external_show_dialog = self.external_show_dialog;
        cfg.debug_summary_path = self.debug_summary_path;
        cfg.debug_summary_delay_frames = self.debug_summary_delay_frames;
        cfg.debug.enabled = self.debug_enabled
            || self.debug_log_path.is_some()
            || self.debug_input_trace
            || self.debug_event_trace
            || self.debug_input_trace_console
            || self.auto_run
            || self.auto_run_editor
            || self.auto_run_pitch_shift.is_some()
            || self.auto_run_time_stretch.is_some();
        cfg.debug.log_path = self.debug_log_path;
        cfg.debug.input_trace_to_console = self.debug_input_trace_console;
        cfg.debug.input_trace_enabled = self.debug_input_trace || self.debug_input_trace_console;
        cfg.debug.event_trace_enabled = self.debug_event_trace;
        cfg.debug.auto_run = self.auto_run
            || self.auto_run_editor
            || self.auto_run_pitch_shift.is_some()
            || self.auto_run_time_stretch.is_some();
        cfg.debug.auto_run_editor = self.auto_run_editor;
        cfg.debug.auto_run_pitch_shift_semitones = self.auto_run_pitch_shift;
        cfg.debug.auto_run_time_stretch_rate = self.auto_run_time_stretch;
        cfg.debug.auto_run_delay_frames = self.auto_run_delay_frames;
        cfg.debug.auto_run_exit = !self.auto_run_no_exit;
        cfg.debug.check_interval_frames = self.debug_check_interval.max(1);
        cfg.no_ipc_forward = self.no_ipc_forward;
        for path in self.inputs {
            push_input_path(&mut cfg, path);
        }
        cfg
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "neowaves",
    about = "NeoWaves headless CLI",
    long_about = "Headless automation mode for NeoWaves. JSON is written to stdout. Use `neowaves --help` for default GUI startup.",
    disable_help_subcommand = true,
    arg_required_else_help = true,
    after_help = CLI_AFTER_HELP
)]
pub struct CliRoot {
    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Debug, Subcommand)]
pub enum CliCommand {
    #[command(subcommand)]
    Session(SessionCommand),
    #[command(subcommand)]
    Item(ItemCommand),
    #[command(subcommand)]
    List(ListCommand),
    #[command(subcommand)]
    Editor(EditorCommand),
    #[command(subcommand)]
    Render(RenderCommand),
    #[command(subcommand)]
    Export(ExportCommand),
    #[command(subcommand)]
    Debug(DebugCommand),
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    New(SessionNewArgs),
    Inspect(SessionInspectArgs),
}

#[derive(Debug, Args)]
#[command(after_help = SESSION_NEW_AFTER_HELP)]
pub struct SessionNewArgs {
    #[arg(long, value_name = "DIR")]
    pub folder: Option<PathBuf>,
    #[arg(long, value_name = "AUDIO")]
    pub input: Vec<PathBuf>,
    #[arg(long, value_name = "SESSION")]
    pub output: Option<PathBuf>,
    #[arg(long, action = ArgAction::SetTrue)]
    pub open_first: bool,
}

#[derive(Debug, Args)]
#[command(after_help = SESSION_INSPECT_AFTER_HELP)]
pub struct SessionInspectArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum ItemCommand {
    Inspect(ItemInspectArgs),
    Meta(ItemMetaArgs),
    Artwork(ItemArtworkArgs),
}

#[derive(Debug, Args)]
pub struct ItemInspectArgs {
    #[arg(long, value_name = "AUDIO")]
    pub input: PathBuf,
}

#[derive(Debug, Args)]
pub struct ItemMetaArgs {
    #[arg(long, value_name = "AUDIO")]
    pub input: PathBuf,
}

#[derive(Debug, Args)]
pub struct ItemArtworkArgs {
    #[arg(long, value_name = "AUDIO")]
    pub input: PathBuf,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum ListCommand {
    Columns(ListColumnsArgs),
    Query(ListQueryArgs),
    Render(ListRenderArgs),
}

#[derive(Debug, Args, Default)]
pub struct ListColumnsArgs {}

#[derive(Debug, Args)]
pub struct ListSourceArgs {
    #[arg(long, value_name = "DIR", conflicts_with = "session")]
    pub folder: Option<PathBuf>,
    #[arg(long, value_name = "SESSION", conflicts_with = "folder")]
    pub session: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(after_help = LIST_QUERY_AFTER_HELP)]
pub struct ListQueryArgs {
    #[command(flatten)]
    pub source: ListSourceArgs,
    #[arg(long, default_value = "file,folder,length,channels,sample_rate,bits,peak,lufs,gain,wave")]
    pub columns: String,
    #[arg(long)]
    pub query: Option<String>,
    #[arg(long = "sort-key")]
    pub sort_key: Option<String>,
    #[arg(long = "sort-dir")]
    pub sort_dir: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long, action = ArgAction::SetTrue)]
    pub include_overlays: bool,
}

#[derive(Debug, Args)]
pub struct ListRenderArgs {
    #[command(flatten)]
    pub source: ListSourceArgs,
    #[arg(long, default_value = "file,folder,length,channels,sample_rate,bits,peak,lufs,gain,wave")]
    pub columns: String,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
    #[arg(long, action = ArgAction::SetTrue)]
    pub show_markers: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    pub show_loop: bool,
}

#[derive(Debug, Subcommand)]
pub enum EditorCommand {
    Inspect(EditorInspectArgs),
    #[command(subcommand)]
    View(EditorViewCommand),
    #[command(subcommand)]
    Selection(EditorSelectionCommand),
    #[command(subcommand)]
    Playback(EditorPlaybackCommand),
    #[command(subcommand)]
    Tool(EditorToolCommand),
    #[command(subcommand)]
    Markers(EditorMarkersCommand),
    #[command(subcommand)]
    Loop(EditorLoopCommand),
}

#[derive(Debug, Args, Clone)]
pub struct EditorSourceArgs {
    #[arg(long, value_name = "AUDIO", conflicts_with = "session")]
    pub input: Option<PathBuf>,
    #[arg(long, value_name = "SESSION", conflicts_with = "input")]
    pub session: Option<PathBuf>,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(after_help = EDITOR_INSPECT_AFTER_HELP)]
pub struct EditorInspectArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Subcommand)]
pub enum EditorViewCommand {
    Get(EditorViewGetArgs),
    Set(EditorViewSetArgs),
}

#[derive(Debug, Args)]
pub struct EditorViewGetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
pub struct EditorViewSetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long)]
    pub view_mode: Option<CliViewMode>,
    #[arg(long = "waveform-overlay")]
    pub waveform_overlay: Option<CliToggle>,
    #[arg(long)]
    pub samples_per_px: Option<f32>,
    #[arg(long)]
    pub view_offset: Option<usize>,
    #[arg(long)]
    pub vertical_zoom: Option<f32>,
    #[arg(long = "vertical-center")]
    pub vertical_center: Option<f32>,
}

#[derive(Debug, Subcommand)]
pub enum EditorSelectionCommand {
    Get(EditorSelectionGetArgs),
    Set(EditorSelectionSetArgs),
    Clear(EditorSelectionClearArgs),
}

#[derive(Debug, Args)]
pub struct EditorSelectionGetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
pub struct EditorSelectionSetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long)]
    pub start_sample: Option<usize>,
    #[arg(long)]
    pub end_sample: Option<usize>,
    #[arg(long)]
    pub start_frac: Option<f32>,
    #[arg(long)]
    pub end_frac: Option<f32>,
}

#[derive(Debug, Args)]
pub struct EditorSelectionClearArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Subcommand)]
pub enum EditorPlaybackCommand {
    Play(EditorPlaybackPlayArgs),
}

#[derive(Debug, Args)]
#[command(after_help = EDITOR_PLAYBACK_PLAY_AFTER_HELP)]
pub struct EditorPlaybackPlayArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long, action = ArgAction::SetTrue)]
    pub selection: bool,
    #[arg(long = "loop", action = ArgAction::SetTrue)]
    pub loop_range: bool,
    #[arg(long)]
    pub start_sample: Option<usize>,
    #[arg(long)]
    pub end_sample: Option<usize>,
    #[arg(long)]
    pub start_frac: Option<f32>,
    #[arg(long)]
    pub end_frac: Option<f32>,
    #[arg(long = "volume-db", default_value_t = -12.0, allow_hyphen_values = true)]
    pub volume_db: f32,
    #[arg(long, default_value_t = 1.0)]
    pub rate: f32,
    #[arg(long = "output-device")]
    pub output_device: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum EditorToolCommand {
    Get(EditorToolGetArgs),
    Set(EditorToolSetArgs),
    Apply(EditorToolApplyArgs),
}

#[derive(Debug, Args)]
pub struct EditorToolGetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
#[command(after_help = EDITOR_TOOL_SET_AFTER_HELP)]
pub struct EditorToolSetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long)]
    pub tool: Option<CliEditorTool>,
    #[arg(long = "fade-in-ms")]
    pub fade_in_ms: Option<f32>,
    #[arg(long = "fade-out-ms")]
    pub fade_out_ms: Option<f32>,
    #[arg(long = "gain-db", allow_hyphen_values = true)]
    pub gain_db: Option<f32>,
    #[arg(long = "normalize-target-db", allow_hyphen_values = true)]
    pub normalize_target_db: Option<f32>,
    #[arg(long = "loudness-target-lufs", allow_hyphen_values = true)]
    pub loudness_target_lufs: Option<f32>,
    #[arg(long = "pitch-semitones", allow_hyphen_values = true)]
    pub pitch_semitones: Option<f32>,
    #[arg(long = "stretch-rate")]
    pub stretch_rate: Option<f32>,
    #[arg(long = "loop-repeat")]
    pub loop_repeat: Option<u32>,
}

#[derive(Debug, Args)]
pub struct EditorToolApplyArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Subcommand)]
pub enum EditorMarkersCommand {
    List(EditorMarkersListArgs),
    Add(EditorMarkersAddArgs),
    Set(EditorMarkersSetArgs),
    Remove(EditorMarkersRemoveArgs),
    Clear(EditorMarkersClearArgs),
    Apply(EditorMarkersApplyArgs),
}

#[derive(Debug, Args)]
pub struct EditorMarkersListArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
pub struct EditorMarkersAddArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long)]
    pub sample: usize,
    #[arg(long)]
    pub label: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditorMarkersSetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long = "marker")]
    pub markers: Vec<String>,
}

#[derive(Debug, Args)]
pub struct EditorMarkersRemoveArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long)]
    pub index: usize,
}

#[derive(Debug, Args)]
pub struct EditorMarkersClearArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
pub struct EditorMarkersApplyArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Subcommand)]
pub enum EditorLoopCommand {
    Get(EditorLoopGetArgs),
    Set(EditorLoopSetArgs),
    Clear(EditorLoopClearArgs),
    Apply(EditorLoopApplyArgs),
    Mode(EditorLoopModeArgs),
    Xfade(EditorLoopXfadeArgs),
    Repeat(EditorLoopRepeatArgs),
}

#[derive(Debug, Args)]
pub struct EditorLoopGetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
pub struct EditorLoopSetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long)]
    pub start_sample: Option<usize>,
    #[arg(long)]
    pub end_sample: Option<usize>,
    #[arg(long)]
    pub start_frac: Option<f32>,
    #[arg(long)]
    pub end_frac: Option<f32>,
}

#[derive(Debug, Args)]
pub struct EditorLoopClearArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
pub struct EditorLoopApplyArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
pub struct EditorLoopModeArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long)]
    pub mode: CliLoopMode,
}

#[derive(Debug, Args)]
pub struct EditorLoopXfadeArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long)]
    pub samples: usize,
    #[arg(long)]
    pub shape: CliLoopXfadeShape,
}

#[derive(Debug, Args)]
pub struct EditorLoopRepeatArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long = "count")]
    pub count: u32,
}

#[derive(Debug, Subcommand)]
pub enum RenderCommand {
    Waveform(RenderWaveformArgs),
    Spectrum(RenderSpectrumArgs),
    Editor(RenderEditorArgs),
    List(RenderListArgs),
}

#[derive(Debug, Args)]
#[command(after_help = RENDER_WAVEFORM_AFTER_HELP)]
pub struct RenderWaveformArgs {
    #[arg(long, value_name = "AUDIO")]
    pub input: PathBuf,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
    #[arg(long, default_value_t = 1280)]
    pub width: u32,
    #[arg(long, default_value_t = 360)]
    pub height: u32,
    #[arg(long, action = ArgAction::SetTrue)]
    pub mixdown: bool,
}

#[derive(Debug, Args)]
#[command(after_help = RENDER_SPECTRUM_AFTER_HELP)]
pub struct RenderSpectrumArgs {
    #[arg(long, value_name = "AUDIO")]
    pub input: PathBuf,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
    #[arg(long, default_value_t = 1280)]
    pub width: u32,
    #[arg(long, default_value_t = 360)]
    pub height: u32,
    #[arg(long = "view-mode", default_value = "spec")]
    pub view_mode: CliSpectralViewMode,
}

#[derive(Debug, Args)]
#[command(after_help = RENDER_EDITOR_AFTER_HELP)]
pub struct RenderEditorArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
    #[arg(long = "view-mode")]
    pub view_mode: Option<CliViewMode>,
    #[arg(long = "waveform-overlay")]
    pub waveform_overlay: Option<CliToggle>,
    #[arg(long, action = ArgAction::SetTrue)]
    pub include_inspector: bool,
}

#[derive(Debug, Args)]
#[command(after_help = RENDER_LIST_AFTER_HELP)]
pub struct RenderListArgs {
    #[command(flatten)]
    pub source: ListSourceArgs,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
    #[arg(long, default_value = "file,folder,length,channels,sample_rate,bits,peak,lufs,gain,wave")]
    pub columns: String,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    #[arg(long)]
    pub limit: Option<usize>,
}

#[derive(Debug, Subcommand)]
pub enum ExportCommand {
    File(ExportFileArgs),
}

#[derive(Debug, Args)]
#[command(after_help = EXPORT_FILE_AFTER_HELP)]
pub struct ExportFileArgs {
    #[arg(long, value_name = "AUDIO", conflicts_with = "session")]
    pub input: Option<PathBuf>,
    #[arg(long, value_name = "SESSION", conflicts_with = "input")]
    pub session: Option<PathBuf>,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
    #[arg(long, value_name = "AUDIO")]
    pub output: Option<PathBuf>,
    #[arg(long, action = ArgAction::SetTrue)]
    pub overwrite: bool,
    #[arg(long)]
    pub format: Option<String>,
    #[arg(long = "gain-db", allow_hyphen_values = true)]
    pub gain_db: Option<f32>,
    #[arg(long = "loop-start-sample")]
    pub loop_start_sample: Option<usize>,
    #[arg(long = "loop-end-sample")]
    pub loop_end_sample: Option<usize>,
    #[arg(long = "marker")]
    pub markers: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    Summary(DebugSummaryArgs),
    Screenshot(DebugScreenshotArgs),
}

#[derive(Debug, Args)]
pub struct DebugSummaryArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
pub struct DebugScreenshotArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
    #[arg(long = "view-mode")]
    pub view_mode: Option<CliViewMode>,
    #[arg(long = "waveform-overlay")]
    pub waveform_overlay: Option<CliToggle>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliViewMode {
    Wave,
    Spec,
    Log,
    Mel,
    Tempogram,
    Chromagram,
}

impl From<CliViewMode> for app::ViewMode {
    fn from(value: CliViewMode) -> Self {
        match value {
            CliViewMode::Wave => app::ViewMode::Waveform,
            CliViewMode::Spec => app::ViewMode::Spectrogram,
            CliViewMode::Log => app::ViewMode::Log,
            CliViewMode::Mel => app::ViewMode::Mel,
            CliViewMode::Tempogram => app::ViewMode::Tempogram,
            CliViewMode::Chromagram => app::ViewMode::Chromagram,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliSpectralViewMode {
    Spec,
    Log,
    Mel,
}

impl From<CliSpectralViewMode> for app::ViewMode {
    fn from(value: CliSpectralViewMode) -> Self {
        match value {
            CliSpectralViewMode::Spec => app::ViewMode::Spectrogram,
            CliSpectralViewMode::Log => app::ViewMode::Log,
            CliSpectralViewMode::Mel => app::ViewMode::Mel,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliEditorTool {
    Trim,
    Fade,
    Pitch,
    Stretch,
    Gain,
    Normalize,
    Loudness,
    Reverse,
}

impl From<CliEditorTool> for app::ToolKind {
    fn from(value: CliEditorTool) -> Self {
        match value {
            CliEditorTool::Trim => app::ToolKind::Trim,
            CliEditorTool::Fade => app::ToolKind::Fade,
            CliEditorTool::Pitch => app::ToolKind::PitchShift,
            CliEditorTool::Stretch => app::ToolKind::TimeStretch,
            CliEditorTool::Gain => app::ToolKind::Gain,
            CliEditorTool::Normalize => app::ToolKind::Normalize,
            CliEditorTool::Loudness => app::ToolKind::Loudness,
            CliEditorTool::Reverse => app::ToolKind::Reverse,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliLoopMode {
    Off,
    Whole,
    Marker,
}

impl From<CliLoopMode> for app::LoopMode {
    fn from(value: CliLoopMode) -> Self {
        match value {
            CliLoopMode::Off => app::LoopMode::Off,
            CliLoopMode::Whole => app::LoopMode::OnWhole,
            CliLoopMode::Marker => app::LoopMode::Marker,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliLoopXfadeShape {
    Linear,
    Equal,
    #[value(name = "linear-dip")]
    LinearDip,
    #[value(name = "equal-dip")]
    EqualDip,
}

impl From<CliLoopXfadeShape> for app::LoopXfadeShape {
    fn from(value: CliLoopXfadeShape) -> Self {
        match value {
            CliLoopXfadeShape::Linear => app::LoopXfadeShape::Linear,
            CliLoopXfadeShape::Equal => app::LoopXfadeShape::EqualPower,
            CliLoopXfadeShape::LinearDip => app::LoopXfadeShape::LinearDip,
            CliLoopXfadeShape::EqualDip => app::LoopXfadeShape::EqualPowerDip,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliToggle {
    On,
    Off,
}

impl CliToggle {
    pub fn into_bool(self) -> bool {
        matches!(self, Self::On)
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliExternalKeyRule {
    File,
    Stem,
    Regex,
}

impl From<CliExternalKeyRule> for app::ExternalKeyRule {
    fn from(value: CliExternalKeyRule) -> Self {
        match value {
            CliExternalKeyRule::File => app::ExternalKeyRule::FileName,
            CliExternalKeyRule::Stem => app::ExternalKeyRule::Stem,
            CliExternalKeyRule::Regex => app::ExternalKeyRule::Regex,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliExternalRegexInput {
    File,
    Stem,
    Path,
    Dir,
}

impl From<CliExternalRegexInput> for app::ExternalRegexInput {
    fn from(value: CliExternalRegexInput) -> Self {
        match value {
            CliExternalRegexInput::File => app::ExternalRegexInput::FileName,
            CliExternalRegexInput::Stem => app::ExternalRegexInput::Stem,
            CliExternalRegexInput::Path => app::ExternalRegexInput::Path,
            CliExternalRegexInput::Dir => app::ExternalRegexInput::Dir,
        }
    }
}

fn is_session_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("nwsess") || ext.eq_ignore_ascii_case("nwproj"))
        .unwrap_or(false)
}

fn push_input_path(cfg: &mut app::StartupConfig, path: PathBuf) {
    if is_session_path(&path) {
        cfg.open_project = Some(path);
    } else if path.is_dir() {
        if cfg.open_files.is_empty() {
            cfg.open_folder = Some(path);
        }
    } else {
        cfg.open_files.push(path);
    }
}

pub fn root_help_text() -> String {
    let mut cmd = GuiArgs::command();
    let mut bytes = Vec::new();
    cmd.write_long_help(&mut bytes).expect("root help");
    String::from_utf8(bytes).expect("utf8 help")
}

pub fn cli_help_text() -> String {
    let mut cmd = CliRoot::command();
    let mut bytes = Vec::new();
    cmd.write_long_help(&mut bytes).expect("cli help");
    String::from_utf8(bytes).expect("utf8 help")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_help_mentions_cli_mode() {
        let help = root_help_text();
        assert!(help.contains("--cli"));
        assert!(help.contains("Run headless CLI mode"));
    }

    #[test]
    fn cli_help_mentions_phase_1b_commands() {
        let help = cli_help_text();
        assert!(help.contains("editor"));
        assert!(help.contains("editor playback play"));
        assert!(help.contains("render spectrum"));
    }

    #[test]
    fn parses_editor_playback_play() {
        let cli = CliRoot::try_parse_from([
            "neowaves",
            "editor",
            "playback",
            "play",
            "--session",
            "work.nwsess",
            "--selection",
            "--rate",
            "0.75",
        ])
        .expect("parse playback play");
        match cli.command {
            CliCommand::Editor(EditorCommand::Playback(EditorPlaybackCommand::Play(args))) => {
                assert_eq!(
                    args.source.session.as_deref(),
                    Some(std::path::Path::new("work.nwsess"))
                );
                assert!(args.selection);
                assert!((args.rate - 0.75).abs() < f32::EPSILON);
            }
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn parses_editor_tool_set() {
        let cli = CliRoot::try_parse_from([
            "neowaves",
            "editor",
            "tool",
            "set",
            "--session",
            "work.nwsess",
            "--tool",
            "gain",
            "--gain-db",
            "-3",
        ])
        .expect("parse tool set");
        match cli.command {
            CliCommand::Editor(EditorCommand::Tool(EditorToolCommand::Set(args))) => {
                assert_eq!(
                    args.source.session.as_deref(),
                    Some(std::path::Path::new("work.nwsess"))
                );
                assert!(matches!(args.tool, Some(CliEditorTool::Gain)));
                assert_eq!(args.gain_db, Some(-3.0));
            }
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn parses_session_backed_export_overwrite() {
        let cli = CliRoot::try_parse_from([
            "neowaves",
            "export",
            "file",
            "--session",
            "work.nwsess",
            "--overwrite",
        ])
        .expect("parse export overwrite");
        match cli.command {
            CliCommand::Export(ExportCommand::File(args)) => {
                assert_eq!(args.session.as_deref(), Some(std::path::Path::new("work.nwsess")));
                assert!(args.overwrite);
                assert!(args.output.is_none());
            }
            _ => panic!("unexpected command"),
        }
    }
}
