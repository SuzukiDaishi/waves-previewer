use std::path::{Path, PathBuf};

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};

use crate::app;

const ROOT_AFTER_HELP: &str = r#"Examples:
  neowaves
  neowaves --open-file .\demo.wav
  neowaves --open-session .\work.nwsess
  neowaves --cli list query --folder .\assets\audio
  neowaves --cli batch loudness plan --session .\work.nwsess --query _BGM --target-lufs -24
  neowaves --cli editor inspect --input .\demo.wav
  neowaves --cli effect-graph list
  neowaves --cli external inspect --session .\work.nwsess
  neowaves --cli transcript generate --session .\work.nwsess --write-srt
  neowaves --cli music-ai analyze --session .\work.nwsess --report .\music_analysis.md
  neowaves --cli plugin scan

See docs/CLI_MASTER_PLAN.md and docs/CLI_COMMAND_REFERENCE.md for the full CLI contract."#;

const CLI_AFTER_HELP: &str = r#"Examples:
  neowaves --cli session inspect --session .\work.nwsess
  neowaves --cli list query --folder .\assets\audio --columns file,length,sample_rate
  neowaves --cli batch loudness plan --session .\work.nwsess --query _BGM --target-lufs -24
  neowaves --cli editor playback play --session .\work.nwsess --selection
  neowaves --cli export verify-loop-tags --input .\out\music_loop.mp3
  neowaves --cli effect-graph test --graph convert_2ch_to_5ch --input .\stereo.wav
  neowaves --cli external source add --session .\work.nwsess --input .\table.xlsx
  neowaves --cli transcript generate --session .\work.nwsess --write-srt --overwrite-existing
  neowaves --cli music-ai apply-markers --session .\work.nwsess --beats --downbeats --sections --replace
  neowaves --cli plugin session apply --session .\work.nwsess

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

const EDITOR_CURSOR_AFTER_HELP: &str = r#"Examples:
  neowaves --cli editor cursor get --session .\work.nwsess
  neowaves --cli editor cursor set --session .\work.nwsess --sample 44100
  neowaves --cli editor cursor nudge --session .\work.nwsess --samples 512 --snap zero-cross"#;

const RENDER_WAVEFORM_AFTER_HELP: &str = r#"Examples:
  neowaves --cli render waveform --input .\demo.wav --output .\out\wave.png
  neowaves --cli render waveform --input .\demo.wav --mixdown
  neowaves --cli render waveform --session .\work.nwsess --path .\demo.wav --loop"#;

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
  neowaves --cli export file --session .\work.nwsess --overwrite
  neowaves --cli export file --session .\work.nwsess --output .\music_loop.mp3 --format mp3"#;

const EXPORT_VERIFY_LOOP_TAGS_AFTER_HELP: &str = r#"Examples:
  neowaves --cli export verify-loop-tags --input .\music_loop.mp3
  neowaves --cli export verify-loop-tags --input .\battle_loop.wav"#;

const BATCH_LOUDNESS_PLAN_AFTER_HELP: &str = r#"Examples:
  neowaves --cli batch loudness plan --session .\work.nwsess --query _BGM --target-lufs -24
  neowaves --cli batch loudness plan --session .\work.nwsess --query-id <id> --target-lufs -24 --report .\bgm_plan.md"#;

const BATCH_LOUDNESS_APPLY_AFTER_HELP: &str = r#"Examples:
  neowaves --cli batch loudness apply --session .\work.nwsess --query _BGM --target-lufs -24
  neowaves --cli batch loudness apply --session .\work.nwsess --query-id <id> --target-lufs -24"#;

const BATCH_EXPORT_AFTER_HELP: &str = r#"Examples:
  neowaves --cli batch export --session .\work.nwsess --query _BGM --overwrite
  neowaves --cli batch export --session .\work.nwsess --query-id <id> --output-dir .\out --report .\export.md"#;

const EFFECT_GRAPH_AFTER_HELP: &str = r#"Examples:
  neowaves --cli effect-graph list
  neowaves --cli effect-graph new --name convert_2ch_to_5ch
  neowaves --cli effect-graph validate --graph convert_2ch_to_5ch
  neowaves --cli effect-graph test --graph convert_2ch_to_5ch --input .\stereo.wav"#;

const EXTERNAL_AFTER_HELP: &str = r#"Examples:
  neowaves --cli external inspect --session .\work.nwsess
  neowaves --cli external source add --session .\work.nwsess --input .\table.xlsx --sheet Sheet1
  neowaves --cli external config set --session .\work.nwsess --key-rule regex --regex-input stem --regex "(.*)_BGM" --replace "$1"
  neowaves --cli external rows --session .\work.nwsess"#;

const TRANSCRIPT_AFTER_HELP: &str = r#"Examples:
  neowaves --cli transcript inspect --input .\voice.wav
  neowaves --cli transcript config set --session .\work.nwsess --language ja --task transcribe
  neowaves --cli transcript generate --session .\work.nwsess --path .\voice.wav --overwrite-existing
  neowaves --cli transcript batch generate --session .\work.nwsess --query _VO"#;

const TRANSCRIPT_GENERATE_AFTER_HELP: &str = r#"Examples:
  neowaves --cli transcript generate --session .\work.nwsess --write-srt
  neowaves --cli transcript generate --session .\work.nwsess --path .\voice.wav --write-srt --overwrite-existing
  neowaves --cli transcript batch generate --session .\work.nwsess --query _VO --write-srt"#;

const MUSIC_AI_AFTER_HELP: &str = r#"Examples:
  neowaves --cli music-ai inspect --session .\work.nwsess --path .\music.wav
  neowaves --cli music-ai analyze --session .\work.nwsess --path .\music.wav --stems-dir .\stems
  neowaves --cli music-ai apply-markers --session .\work.nwsess --beats --downbeats --replace
  neowaves --cli music-ai export-stems --session .\work.nwsess --output-dir .\out\stems"#;

const MUSIC_AI_ANALYZE_AFTER_HELP: &str = r#"Examples:
  neowaves --cli music-ai analyze --session .\work.nwsess --report .\music_analysis.md
  neowaves --cli music-ai analyze --session .\work.nwsess --path .\music.wav --stems-dir .\stems --prefer-demucs"#;

const PLUGIN_AFTER_HELP: &str = r#"Examples:
  neowaves --cli plugin search-path list
  neowaves --cli plugin scan
  neowaves --cli plugin probe --plugin my_fx
  neowaves --cli plugin session set --session .\work.nwsess --plugin my_fx --param mix=0.5
  neowaves --cli plugin session apply --session .\work.nwsess"#;

const PLUGIN_SESSION_APPLY_AFTER_HELP: &str = r#"Examples:
  neowaves --cli plugin session apply --session .\work.nwsess
  neowaves --cli plugin session set --session .\work.nwsess --plugin my_fx --param mix=0.5
  neowaves --cli plugin session preview --session .\work.nwsess

This command mutates the current session target and requires --session."#;

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
    Batch(BatchCommand),
    #[command(subcommand)]
    Editor(EditorCommand),
    #[command(subcommand, after_help = EXTERNAL_AFTER_HELP)]
    External(ExternalCommand),
    #[command(subcommand, after_help = TRANSCRIPT_AFTER_HELP)]
    Transcript(TranscriptCommand),
    #[command(subcommand, name = "music-ai", after_help = MUSIC_AI_AFTER_HELP)]
    MusicAi(MusicAiCommand),
    #[command(subcommand, after_help = PLUGIN_AFTER_HELP)]
    Plugin(PluginCommand),
    #[command(subcommand, name = "effect-graph", after_help = EFFECT_GRAPH_AFTER_HELP)]
    EffectGraph(EffectGraphCommand),
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
    Sort(ListSortArgs),
    Search(ListSearchArgs),
    Select(ListSelectArgs),
    #[command(name = "save-query")]
    SaveQuery(ListSaveQueryArgs),
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

#[derive(Debug, Args, Clone, Default)]
pub struct CliQueryFilterArgs {
    #[arg(long)]
    pub query: Option<String>,
    #[arg(long = "sort-key")]
    pub sort_key: Option<String>,
    #[arg(long = "sort-dir")]
    pub sort_dir: Option<String>,
    #[arg(long = "query-id")]
    pub query_id: Option<String>,
}

#[derive(Debug, Args)]
#[command(after_help = LIST_QUERY_AFTER_HELP)]
pub struct ListQueryArgs {
    #[command(flatten)]
    pub source: ListSourceArgs,
    #[arg(long, default_value = "file,folder,length,channels,sample_rate,bits,peak,lufs,gain,wave")]
    pub columns: String,
    #[command(flatten)]
    pub filter: CliQueryFilterArgs,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long, action = ArgAction::SetTrue)]
    pub include_overlays: bool,
}

#[derive(Debug, Args)]
pub struct ListSortArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long = "sort-key")]
    pub sort_key: String,
    #[arg(long = "sort-dir", default_value = "asc")]
    pub sort_dir: String,
    #[arg(long, default_value = "file,folder,length,channels,sample_rate,bits,peak,lufs,gain,wave")]
    pub columns: String,
    #[arg(long)]
    pub query: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long, action = ArgAction::SetTrue)]
    pub include_overlays: bool,
}

#[derive(Debug, Args)]
pub struct ListSearchArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long)]
    pub query: String,
    #[arg(long, default_value = "file,folder,length,channels,sample_rate,bits,peak,lufs,gain,wave")]
    pub columns: String,
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
pub struct ListSelectArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO", conflicts_with = "query")]
    pub path: Option<PathBuf>,
    #[arg(long, conflicts_with = "path")]
    pub query: Option<String>,
    #[arg(long, default_value_t = 0)]
    pub index: usize,
}

#[derive(Debug, Args)]
pub struct ListSaveQueryArgs {
    #[command(flatten)]
    pub source: ListSourceArgs,
    #[command(flatten)]
    pub filter: CliQueryFilterArgs,
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
pub enum ExternalCommand {
    Inspect(ExternalInspectArgs),
    Render(ExternalRenderArgs),
    Rows(ExternalRowsArgs),
    #[command(subcommand)]
    Source(ExternalSourceCommand),
    #[command(subcommand)]
    Config(ExternalConfigCommand),
}

#[derive(Debug, Args)]
pub struct ExternalInspectArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
}

#[derive(Debug, Args)]
pub struct ExternalRenderArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
    #[arg(long, default_value_t = 1280)]
    pub width: u32,
    #[arg(long, default_value_t = 720)]
    pub height: u32,
}

#[derive(Debug, Args)]
pub struct ExternalRowsArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    #[arg(long)]
    pub limit: Option<usize>,
    #[arg(long, action = ArgAction::SetTrue)]
    pub include_unmatched: bool,
}

#[derive(Debug, Subcommand)]
pub enum ExternalSourceCommand {
    List(ExternalSourceListArgs),
    Add(ExternalSourceAddArgs),
    Reload(ExternalSourceReloadArgs),
    Remove(ExternalSourceRemoveArgs),
    Clear(ExternalSourceClearArgs),
}

#[derive(Debug, Args)]
pub struct ExternalSourceListArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
}

#[derive(Debug, Args)]
pub struct ExternalSourceAddArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long = "input", value_name = "PATH")]
    pub input: PathBuf,
    #[arg(long = "sheet")]
    pub sheet_name: Option<String>,
    #[arg(long = "has-header")]
    pub has_header: Option<CliToggle>,
    #[arg(long = "header-row")]
    pub header_row: Option<usize>,
    #[arg(long = "data-row")]
    pub data_row: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ExternalSourceReloadArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long)]
    pub index: Option<usize>,
}

#[derive(Debug, Args)]
pub struct ExternalSourceRemoveArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long)]
    pub index: usize,
}

#[derive(Debug, Args)]
pub struct ExternalSourceClearArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum ExternalConfigCommand {
    Get(ExternalConfigGetArgs),
    Set(ExternalConfigSetArgs),
}

#[derive(Debug, Args)]
pub struct ExternalConfigGetArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
}

#[derive(Debug, Args)]
pub struct ExternalConfigSetArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long = "active-source")]
    pub active_source: Option<usize>,
    #[arg(long = "key-column")]
    pub key_column: Option<String>,
    #[arg(long = "key-rule")]
    pub key_rule: Option<CliExternalKeyRule>,
    #[arg(long = "regex-input")]
    pub regex_input: Option<CliExternalRegexInput>,
    #[arg(long = "regex")]
    pub regex: Option<String>,
    #[arg(long = "replace")]
    pub replace: Option<String>,
    #[arg(long = "scope-regex")]
    pub scope_regex: Option<String>,
    #[arg(long = "visible-column")]
    pub visible_columns: Vec<String>,
    #[arg(long = "show-unmatched")]
    pub show_unmatched: Option<CliToggle>,
}

#[derive(Debug, Subcommand)]
pub enum TranscriptCommand {
    Inspect(TranscriptInspectArgs),
    #[command(subcommand)]
    Model(TranscriptModelCommand),
    #[command(subcommand)]
    Config(TranscriptConfigCommand),
    Generate(TranscriptGenerateArgs),
    #[command(subcommand)]
    Batch(TranscriptBatchCommand),
    #[command(name = "export-srt")]
    ExportSrt(TranscriptExportSrtArgs),
}

#[derive(Debug, Args)]
pub struct TranscriptInspectArgs {
    #[arg(long, value_name = "AUDIO", conflicts_with = "session")]
    pub input: Option<PathBuf>,
    #[arg(long, value_name = "SESSION", conflicts_with = "input")]
    pub session: Option<PathBuf>,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum TranscriptModelCommand {
    Status(TranscriptModelStatusArgs),
    Download(TranscriptModelDownloadArgs),
    Uninstall(TranscriptModelUninstallArgs),
}

#[derive(Debug, Args, Default)]
pub struct TranscriptModelStatusArgs {}

#[derive(Debug, Args, Default)]
pub struct TranscriptModelDownloadArgs {}

#[derive(Debug, Args, Default)]
pub struct TranscriptModelUninstallArgs {}

#[derive(Debug, Subcommand)]
pub enum TranscriptConfigCommand {
    Get(TranscriptConfigGetArgs),
    Set(TranscriptConfigSetArgs),
}

#[derive(Debug, Args)]
pub struct TranscriptConfigGetArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
}

#[derive(Debug, Args)]
pub struct TranscriptConfigSetArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long)]
    pub language: Option<String>,
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long = "max-new-tokens")]
    pub max_new_tokens: Option<usize>,
    #[arg(long = "overwrite-existing-srt")]
    pub overwrite_existing_srt: Option<CliToggle>,
    #[arg(long = "perf-mode")]
    pub perf_mode: Option<CliTranscriptPerfMode>,
    #[arg(long = "model-variant")]
    pub model_variant: Option<CliTranscriptModelVariant>,
    #[arg(long = "omit-language-token")]
    pub omit_language_token: Option<CliToggle>,
    #[arg(long = "omit-notimestamps-token")]
    pub omit_notimestamps_token: Option<CliToggle>,
    #[arg(long = "vad-enabled")]
    pub vad_enabled: Option<CliToggle>,
    #[arg(long = "vad-model-path")]
    pub vad_model_path: Option<PathBuf>,
    #[arg(long = "clear-vad-model-path", action = ArgAction::SetTrue)]
    pub clear_vad_model_path: bool,
    #[arg(long = "vad-threshold")]
    pub vad_threshold: Option<f32>,
    #[arg(long = "vad-min-speech-ms")]
    pub vad_min_speech_ms: Option<usize>,
    #[arg(long = "vad-min-silence-ms")]
    pub vad_min_silence_ms: Option<usize>,
    #[arg(long = "vad-speech-pad-ms")]
    pub vad_speech_pad_ms: Option<usize>,
    #[arg(long = "max-window-ms")]
    pub max_window_ms: Option<usize>,
    #[arg(long = "no-speech-threshold")]
    pub no_speech_threshold: Option<f32>,
    #[arg(long = "clear-no-speech-threshold", action = ArgAction::SetTrue)]
    pub clear_no_speech_threshold: bool,
    #[arg(long = "logprob-threshold")]
    pub logprob_threshold: Option<f32>,
    #[arg(long = "clear-logprob-threshold", action = ArgAction::SetTrue)]
    pub clear_logprob_threshold: bool,
    #[arg(long = "compute-target")]
    pub compute_target: Option<CliTranscriptComputeTarget>,
    #[arg(long = "dml-device-id")]
    pub dml_device_id: Option<i32>,
    #[arg(long = "cpu-intra-threads")]
    pub cpu_intra_threads: Option<usize>,
}

#[derive(Debug, Args)]
#[command(
    after_help = TRANSCRIPT_GENERATE_AFTER_HELP,
    after_long_help = TRANSCRIPT_GENERATE_AFTER_HELP
)]
pub struct TranscriptGenerateArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
    #[arg(long = "write-srt", action = ArgAction::SetTrue)]
    pub write_srt: bool,
    #[arg(long = "overwrite-existing", action = ArgAction::SetTrue)]
    pub overwrite_existing: bool,
}

#[derive(Debug, Subcommand)]
pub enum TranscriptBatchCommand {
    Generate(TranscriptBatchGenerateArgs),
}

#[derive(Debug, Args)]
#[command(
    after_help = TRANSCRIPT_GENERATE_AFTER_HELP,
    after_long_help = TRANSCRIPT_GENERATE_AFTER_HELP
)]
pub struct TranscriptBatchGenerateArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[command(flatten)]
    pub filter: CliQueryFilterArgs,
    #[arg(long = "write-srt", action = ArgAction::SetTrue)]
    pub write_srt: bool,
    #[arg(long = "overwrite-existing", action = ArgAction::SetTrue)]
    pub overwrite_existing: bool,
}

#[derive(Debug, Args)]
pub struct TranscriptExportSrtArgs {
    #[arg(long, value_name = "AUDIO", conflicts_with = "session")]
    pub input: Option<PathBuf>,
    #[arg(long, value_name = "SESSION", conflicts_with = "input")]
    pub session: Option<PathBuf>,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
    #[arg(long, value_name = "SRT")]
    pub output: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum MusicAiCommand {
    Inspect(MusicAiInspectArgs),
    #[command(subcommand)]
    Model(MusicAiModelCommand),
    Analyze(MusicAiAnalyzeArgs),
    #[command(name = "apply-markers")]
    ApplyMarkers(MusicAiApplyMarkersArgs),
    #[command(name = "export-stems")]
    ExportStems(MusicAiExportStemsArgs),
}

#[derive(Debug, Args)]
pub struct MusicAiInspectArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Subcommand)]
pub enum MusicAiModelCommand {
    Status(MusicAiModelStatusArgs),
    Download(MusicAiModelDownloadArgs),
    Uninstall(MusicAiModelUninstallArgs),
}

#[derive(Debug, Args, Default)]
pub struct MusicAiModelStatusArgs {}

#[derive(Debug, Args, Default)]
pub struct MusicAiModelDownloadArgs {}

#[derive(Debug, Args, Default)]
pub struct MusicAiModelUninstallArgs {}

#[derive(Debug, Args)]
#[command(
    after_help = MUSIC_AI_ANALYZE_AFTER_HELP,
    after_long_help = MUSIC_AI_ANALYZE_AFTER_HELP
)]
pub struct MusicAiAnalyzeArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
    #[arg(long = "stems-dir")]
    pub stems_dir: Option<PathBuf>,
    #[arg(long = "prefer-demucs", action = ArgAction::SetTrue)]
    pub prefer_demucs: bool,
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct MusicAiApplyMarkersArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
    #[arg(long, action = ArgAction::SetTrue)]
    pub beats: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    pub downbeats: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    pub sections: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    pub replace: bool,
}

#[derive(Debug, Args)]
pub struct MusicAiExportStemsArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: PathBuf,
    #[arg(long = "naming-template")]
    pub naming_template: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    #[command(subcommand, name = "search-path")]
    SearchPath(PluginSearchPathCommand),
    Scan(PluginScanArgs),
    List(PluginListArgs),
    Probe(PluginProbeArgs),
    #[command(subcommand)]
    Session(PluginSessionCommand),
}

#[derive(Debug, Subcommand)]
pub enum PluginSearchPathCommand {
    List(PluginSearchPathListArgs),
    Add(PluginSearchPathAddArgs),
    Remove(PluginSearchPathRemoveArgs),
    Reset(PluginSearchPathResetArgs),
}

#[derive(Debug, Args, Default)]
pub struct PluginSearchPathListArgs {}

#[derive(Debug, Args)]
pub struct PluginSearchPathAddArgs {
    #[arg(long, value_name = "DIR")]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
pub struct PluginSearchPathRemoveArgs {
    #[arg(long)]
    pub index: Option<usize>,
    #[arg(long, value_name = "DIR")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Args, Default)]
pub struct PluginSearchPathResetArgs {}

#[derive(Debug, Args, Default)]
pub struct PluginScanArgs {}

#[derive(Debug, Args)]
pub struct PluginListArgs {
    #[arg(long)]
    pub filter: Option<String>,
}

#[derive(Debug, Args)]
pub struct PluginProbeArgs {
    #[arg(long = "plugin")]
    pub plugin: String,
}

#[derive(Debug, Subcommand)]
pub enum PluginSessionCommand {
    Inspect(PluginSessionInspectArgs),
    Set(PluginSessionSetArgs),
    Preview(PluginSessionPreviewArgs),
    Apply(PluginSessionApplyArgs),
    Clear(PluginSessionClearArgs),
}

#[derive(Debug, Args)]
pub struct PluginSessionInspectArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PluginSessionSetArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
    #[arg(long = "plugin")]
    pub plugin: Option<String>,
    #[arg(long)]
    pub enabled: Option<CliToggle>,
    #[arg(long)]
    pub bypass: Option<CliToggle>,
    #[arg(long = "param")]
    pub params: Vec<String>,
    #[arg(long = "state-blob-b64")]
    pub state_blob_b64: Option<String>,
}

#[derive(Debug, Args)]
pub struct PluginSessionPreviewArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(
    after_help = PLUGIN_SESSION_APPLY_AFTER_HELP,
    after_long_help = PLUGIN_SESSION_APPLY_AFTER_HELP
)]
pub struct PluginSessionApplyArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct PluginSessionClearArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum EditorCommand {
    Inspect(EditorInspectArgs),
    #[command(subcommand)]
    View(EditorViewCommand),
    #[command(subcommand)]
    Selection(EditorSelectionCommand),
    #[command(subcommand)]
    Cursor(EditorCursorCommand),
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

#[derive(Debug, Subcommand)]
pub enum EditorCursorCommand {
    Get(EditorCursorGetArgs),
    Set(EditorCursorSetArgs),
    Nudge(EditorCursorNudgeArgs),
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

#[derive(Debug, Args)]
#[command(after_help = EDITOR_CURSOR_AFTER_HELP)]
pub struct EditorCursorGetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
}

#[derive(Debug, Args)]
pub struct EditorCursorSetArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long)]
    pub sample: Option<usize>,
    #[arg(long)]
    pub frac: Option<f32>,
    #[arg(long, default_value = "none")]
    pub snap: CliCursorSnap,
}

#[derive(Debug, Args)]
pub struct EditorCursorNudgeArgs {
    #[command(flatten)]
    pub source: EditorSourceArgs,
    #[arg(long = "samples", allow_hyphen_values = true)]
    pub samples: i64,
    #[arg(long, default_value = "none")]
    pub snap: CliCursorSnap,
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
    #[arg(long, value_name = "AUDIO", conflicts_with = "session")]
    pub input: Option<PathBuf>,
    #[arg(long, value_name = "SESSION", conflicts_with = "input")]
    pub session: Option<PathBuf>,
    #[arg(long, value_name = "AUDIO")]
    pub path: Option<PathBuf>,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
    #[arg(long, default_value_t = 1280)]
    pub width: u32,
    #[arg(long, default_value_t = 360)]
    pub height: u32,
    #[arg(long, action = ArgAction::SetTrue)]
    pub mixdown: bool,
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
    #[arg(long, action = ArgAction::SetTrue, default_value_t = true)]
    pub show_markers: bool,
    #[arg(long, action = ArgAction::SetTrue, default_value_t = true)]
    pub show_loop: bool,
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
    #[command(name = "verify-loop-tags")]
    VerifyLoopTags(ExportVerifyLoopTagsArgs),
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

#[derive(Debug, Args)]
#[command(after_help = EXPORT_VERIFY_LOOP_TAGS_AFTER_HELP)]
pub struct ExportVerifyLoopTagsArgs {
    #[arg(long, value_name = "AUDIO")]
    pub input: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum BatchCommand {
    #[command(subcommand)]
    Loudness(BatchLoudnessCommand),
    Export(BatchExportArgs),
}

#[derive(Debug, Subcommand)]
pub enum BatchLoudnessCommand {
    Plan(BatchLoudnessPlanArgs),
    Apply(BatchLoudnessApplyArgs),
}

#[derive(Debug, Args)]
#[command(after_help = BATCH_LOUDNESS_PLAN_AFTER_HELP)]
pub struct BatchLoudnessPlanArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[command(flatten)]
    pub filter: CliQueryFilterArgs,
    #[arg(long = "target-lufs", allow_hyphen_values = true)]
    pub target_lufs: f32,
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(after_help = BATCH_LOUDNESS_APPLY_AFTER_HELP)]
pub struct BatchLoudnessApplyArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[command(flatten)]
    pub filter: CliQueryFilterArgs,
    #[arg(long = "target-lufs", allow_hyphen_values = true)]
    pub target_lufs: f32,
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(after_help = BATCH_EXPORT_AFTER_HELP)]
pub struct BatchExportArgs {
    #[arg(long, value_name = "SESSION")]
    pub session: PathBuf,
    #[command(flatten)]
    pub filter: CliQueryFilterArgs,
    #[arg(long, action = ArgAction::SetTrue)]
    pub overwrite: bool,
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum EffectGraphCommand {
    List(EffectGraphListArgs),
    New(EffectGraphNewArgs),
    Inspect(EffectGraphInspectArgs),
    Render(EffectGraphRenderArgs),
    Validate(EffectGraphValidateArgs),
    Test(EffectGraphTestArgs),
    Save(EffectGraphSaveArgs),
    Import(EffectGraphImportArgs),
    Export(EffectGraphExportArgs),
    #[command(subcommand)]
    Node(EffectGraphNodeCommand),
    #[command(subcommand)]
    Edge(EffectGraphEdgeCommand),
}

#[derive(Debug, Args, Default)]
pub struct EffectGraphListArgs {}

#[derive(Debug, Args, Clone)]
pub struct EffectGraphRefArgs {
    #[arg(long = "graph", value_name = "GRAPH")]
    pub graph: String,
}

#[derive(Debug, Args)]
pub struct EffectGraphNewArgs {
    #[arg(long)]
    pub name: String,
    #[arg(long = "output", value_name = "JSON")]
    pub output: Option<PathBuf>,
    #[arg(long = "template", value_name = "GRAPH")]
    pub template: Option<String>,
}

#[derive(Debug, Args)]
pub struct EffectGraphInspectArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
}

#[derive(Debug, Args)]
pub struct EffectGraphRenderArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct EffectGraphValidateArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct EffectGraphTestArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
    #[arg(long, value_name = "AUDIO")]
    pub input: Option<PathBuf>,
    #[arg(long, value_name = "PNG")]
    pub output: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct EffectGraphSaveArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
}

#[derive(Debug, Args)]
pub struct EffectGraphImportArgs {
    #[arg(long, value_name = "JSON")]
    pub input: PathBuf,
    #[arg(long, value_name = "JSON")]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct EffectGraphExportArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
    #[arg(long, value_name = "JSON")]
    pub output: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum EffectGraphNodeCommand {
    Add(EffectGraphNodeAddArgs),
    Remove(EffectGraphNodeRemoveArgs),
    Set(EffectGraphNodeSetArgs),
}

#[derive(Debug, Subcommand)]
pub enum EffectGraphEdgeCommand {
    Connect(EffectGraphEdgeConnectArgs),
    Disconnect(EffectGraphEdgeDisconnectArgs),
}

#[derive(Debug, Args)]
pub struct EffectGraphNodeAddArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
    #[arg(long)]
    pub kind: CliEffectGraphNodeKind,
    #[arg(long = "node-id")]
    pub node_id: Option<String>,
    #[arg(long, default_value_t = 160.0)]
    pub x: f32,
    #[arg(long, default_value_t = 160.0)]
    pub y: f32,
}

#[derive(Debug, Args)]
pub struct EffectGraphNodeRemoveArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
    #[arg(long = "node-id")]
    pub node_id: String,
}

#[derive(Debug, Args)]
pub struct EffectGraphNodeSetArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
    #[arg(long = "node-id")]
    pub node_id: String,
    #[arg(long)]
    pub x: Option<f32>,
    #[arg(long)]
    pub y: Option<f32>,
    #[arg(long)]
    pub width: Option<f32>,
    #[arg(long)]
    pub height: Option<f32>,
    #[arg(long = "gain-db", allow_hyphen_values = true)]
    pub gain_db: Option<f32>,
    #[arg(long = "target-lufs", allow_hyphen_values = true)]
    pub target_lufs: Option<f32>,
    #[arg(long = "rate")]
    pub rate: Option<f32>,
    #[arg(long = "semitones", allow_hyphen_values = true)]
    pub semitones: Option<f32>,
    #[arg(long = "spectrum-mode")]
    pub spectrum_mode: Option<CliEffectGraphSpectrumMode>,
    #[arg(long = "ignore-channel")]
    pub ignore_channels: Vec<usize>,
}

#[derive(Debug, Args)]
pub struct EffectGraphEdgeConnectArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
    #[arg(long = "from-node")]
    pub from_node: String,
    #[arg(long = "from-port", default_value = "out")]
    pub from_port: String,
    #[arg(long = "to-node")]
    pub to_node: String,
    #[arg(long = "to-port", default_value = "in")]
    pub to_port: String,
    #[arg(long = "edge-id")]
    pub edge_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct EffectGraphEdgeDisconnectArgs {
    #[command(flatten)]
    pub graph: EffectGraphRefArgs,
    #[arg(long = "edge-id")]
    pub edge_id: Option<String>,
    #[arg(long = "from-node")]
    pub from_node: Option<String>,
    #[arg(long = "from-port")]
    pub from_port: Option<String>,
    #[arg(long = "to-node")]
    pub to_node: Option<String>,
    #[arg(long = "to-port")]
    pub to_port: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    Summary(DebugSummaryArgs),
    Screenshot(DebugScreenshotArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliCursorSnap {
    None,
    #[value(name = "zero-cross")]
    ZeroCross,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliEffectGraphNodeKind {
    Input,
    Output,
    Gain,
    Loudness,
    #[value(name = "mono-mix")]
    MonoMix,
    Pitch,
    Stretch,
    Speed,
    #[value(name = "plugin-fx")]
    PluginFx,
    Duplicate,
    #[value(name = "split-channels")]
    SplitChannels,
    #[value(name = "combine-channels")]
    CombineChannels,
    #[value(name = "debug-waveform")]
    DebugWaveform,
    #[value(name = "debug-spectrum")]
    DebugSpectrum,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliEffectGraphSpectrumMode {
    Spec,
    Log,
    Mel,
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

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliTranscriptPerfMode {
    Stable,
    Balanced,
    Boost,
}

impl From<CliTranscriptPerfMode> for app::TranscriptPerfMode {
    fn from(value: CliTranscriptPerfMode) -> Self {
        match value {
            CliTranscriptPerfMode::Stable => app::TranscriptPerfMode::Stable,
            CliTranscriptPerfMode::Balanced => app::TranscriptPerfMode::Balanced,
            CliTranscriptPerfMode::Boost => app::TranscriptPerfMode::Boost,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliTranscriptModelVariant {
    Auto,
    Fp16,
    Quantized,
}

impl From<CliTranscriptModelVariant> for app::TranscriptModelVariant {
    fn from(value: CliTranscriptModelVariant) -> Self {
        match value {
            CliTranscriptModelVariant::Auto => app::TranscriptModelVariant::Auto,
            CliTranscriptModelVariant::Fp16 => app::TranscriptModelVariant::Fp16,
            CliTranscriptModelVariant::Quantized => app::TranscriptModelVariant::Quantized,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliTranscriptComputeTarget {
    Auto,
    Cpu,
    Gpu,
    Npu,
}

impl From<CliTranscriptComputeTarget> for app::TranscriptComputeTarget {
    fn from(value: CliTranscriptComputeTarget) -> Self {
        match value {
            CliTranscriptComputeTarget::Auto => app::TranscriptComputeTarget::Auto,
            CliTranscriptComputeTarget::Cpu => app::TranscriptComputeTarget::Cpu,
            CliTranscriptComputeTarget::Gpu => app::TranscriptComputeTarget::Gpu,
            CliTranscriptComputeTarget::Npu => app::TranscriptComputeTarget::Npu,
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
        assert!(help.contains("batch"));
        assert!(help.contains("effect-graph"));
        assert!(help.contains("verify-loop-tags"));
        assert!(help.contains("external"));
        assert!(help.contains("transcript"));
        assert!(help.contains("music-ai"));
        assert!(help.contains("plugin"));
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

    #[test]
    fn parses_batch_loudness_plan() {
        let cli = CliRoot::try_parse_from([
            "neowaves",
            "batch",
            "loudness",
            "plan",
            "--session",
            "work.nwsess",
            "--query",
            "_BGM",
            "--target-lufs",
            "-24",
        ])
        .expect("parse batch loudness plan");
        match cli.command {
            CliCommand::Batch(BatchCommand::Loudness(BatchLoudnessCommand::Plan(args))) => {
                assert_eq!(args.session, PathBuf::from("work.nwsess"));
                assert_eq!(args.filter.query.as_deref(), Some("_BGM"));
                assert_eq!(args.target_lufs, -24.0);
            }
            _ => panic!("unexpected command"),
        }
    }

    #[test]
    fn parses_effect_graph_node_add() {
        let cli = CliRoot::try_parse_from([
            "neowaves",
            "effect-graph",
            "node",
            "add",
            "--graph",
            "convert_2ch_to_5ch",
            "--kind",
            "gain",
            "--node-id",
            "gain_l",
        ])
        .expect("parse effect graph node add");
        match cli.command {
            CliCommand::EffectGraph(EffectGraphCommand::Node(EffectGraphNodeCommand::Add(args))) => {
                assert_eq!(args.graph.graph, "convert_2ch_to_5ch");
                assert!(matches!(args.kind, CliEffectGraphNodeKind::Gain));
                assert_eq!(args.node_id.as_deref(), Some("gain_l"));
            }
            _ => panic!("unexpected command"),
        }
    }
}
