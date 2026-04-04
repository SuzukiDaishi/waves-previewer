use std::path::{Path, PathBuf};

use neowaves::app;

const HELP_TEXT: &str = "Usage:\n  neowaves [options]\n\nOptions:\n  --open-session <session.nwsess>\n  --open-project <project.nwproj> (legacy)\n  --open-folder <dir>\n  --open-file <audio> (repeatable)\n  --open-first\n  --open-view-mode <wave|spec|mel>\n  --waveform-overlay <on|off>\n  --screenshot <path.png>\n  --screenshot-delay <frames>\n  --exit-after-screenshot\n  --dummy-list <count>\n  --external-dialog\n  --debug-summary <path>\n  --debug-summary-delay <frames>\n  --external-file <path>\n  --external-dummy <rows>\n  --external-dummy-cols <count>\n  --external-dummy-path <path>\n  --external-dummy-merge\n  --external-sheet <name>\n  --external-has-header <on|off>\n  --external-header-row <n> (1-based, 0=auto)\n  --external-data-row <n> (1-based, 0=auto)\n  --external-key-rule <file|stem|regex>\n  --external-key-input <file|stem|path|dir>\n  --external-key-regex <pattern>\n  --external-key-replace <text>\n  --external-scope-regex <pattern>\n  --external-show-unmatched\n  --debug\n  --debug-log <path>\n  --debug-input-trace\n  --debug-event-trace\n  --debug-input-trace-console\n  --auto-run\n  --auto-run-editor\n  --auto-run-pitch-shift <semitones>\n  --auto-run-time-stretch <rate>\n  --auto-run-delay <frames>\n  --auto-run-no-exit\n  --debug-check-interval <frames>\n  --mcp-stdio\n  --mcp-http\n  --mcp-http-addr <addr>\n  --mcp-allow-path <path>\n  --mcp-allow-write\n  --mcp-allow-export\n  --mcp-readwrite\n  --help";

pub fn parse_startup_config() -> app::StartupConfig {
    let mut cfg = app::StartupConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if handle_named_arg(arg.as_str(), &mut args, &mut cfg) {
            continue;
        }
        handle_positional_arg(&arg, &mut cfg);
    }
    cfg
}

fn handle_named_arg(
    arg: &str,
    args: &mut impl Iterator<Item = String>,
    cfg: &mut app::StartupConfig,
) -> bool {
    handle_open_arg(arg, args, cfg)
        || handle_external_arg(arg, args, cfg)
        || handle_debug_arg(arg, args, cfg)
        || handle_mcp_arg(arg, args, cfg)
        || handle_help_arg(arg)
}

fn handle_open_arg(
    arg: &str,
    args: &mut impl Iterator<Item = String>,
    cfg: &mut app::StartupConfig,
) -> bool {
    match arg {
        "--open-project" | "--open-session" => {
            if let Some(path) = next_path(args) {
                cfg.open_project = Some(path);
            }
            true
        }
        "--open-folder" => {
            if let Some(path) = next_path(args) {
                cfg.open_folder = Some(path);
            }
            true
        }
        "--open-file" => {
            if let Some(path) = next_path(args) {
                push_open_file(cfg, path);
            }
            true
        }
        "--open-first" => {
            cfg.open_first = true;
            true
        }
        "--open-view-mode" => {
            if let Some(value) = args.next().and_then(|raw| parse_view_mode(&raw)) {
                cfg.open_view_mode = Some(value);
            }
            true
        }
        "--waveform-overlay" => {
            if let Some(value) = args.next().and_then(|raw| parse_toggle_flag(&raw)) {
                cfg.open_waveform_overlay = Some(value);
            }
            true
        }
        "--screenshot" => {
            if let Some(path) = next_path(args) {
                cfg.screenshot_path = Some(path);
            }
            true
        }
        "--screenshot-delay" => {
            if let Some(value) = next_parsed::<u32>(args) {
                cfg.screenshot_delay_frames = value;
            }
            true
        }
        "--exit-after-screenshot" => {
            cfg.exit_after_screenshot = true;
            true
        }
        "--dummy-list" => {
            if let Some(value) = next_parsed::<usize>(args) {
                cfg.dummy_list_count = Some(value);
            }
            true
        }
        _ => false,
    }
}

fn handle_external_arg(
    arg: &str,
    args: &mut impl Iterator<Item = String>,
    cfg: &mut app::StartupConfig,
) -> bool {
    match arg {
        "--external-dialog" => {
            cfg.external_show_dialog = true;
            true
        }
        "--debug-summary" => {
            if let Some(path) = next_path(args) {
                cfg.debug_summary_path = Some(path);
            }
            true
        }
        "--debug-summary-delay" => {
            if let Some(value) = next_parsed::<u32>(args) {
                cfg.debug_summary_delay_frames = value;
            }
            true
        }
        "--external-file" => {
            if let Some(path) = next_path(args) {
                cfg.external_path = Some(path);
            }
            true
        }
        "--external-dummy" => {
            if let Some(value) = next_parsed::<usize>(args) {
                cfg.external_dummy_rows = Some(value);
            }
            true
        }
        "--external-dummy-cols" => {
            if let Some(value) = next_parsed::<usize>(args) {
                cfg.external_dummy_cols = value.max(1);
            }
            true
        }
        "--external-dummy-path" => {
            if let Some(path) = next_path(args) {
                cfg.external_dummy_path = Some(path);
            }
            true
        }
        "--external-dummy-merge" => {
            cfg.external_dummy_merge = true;
            true
        }
        "--external-sheet" => {
            if let Some(value) = args.next() {
                cfg.external_sheet = Some(value);
            }
            true
        }
        "--external-has-header" => {
            if let Some(value) = args.next().and_then(|raw| parse_toggle_flag(&raw)) {
                cfg.external_has_header = Some(value);
            }
            true
        }
        "--external-header-row" => {
            if let Some(value) = next_parsed::<usize>(args) {
                cfg.external_header_row = if value == 0 { None } else { Some(value - 1) };
            }
            true
        }
        "--external-data-row" => {
            if let Some(value) = next_parsed::<usize>(args) {
                cfg.external_data_row = if value == 0 { None } else { Some(value - 1) };
            }
            true
        }
        "--external-key-rule" => {
            cfg.external_key_rule = args.next().and_then(|raw| parse_external_key_rule(&raw));
            true
        }
        "--external-key-input" => {
            cfg.external_key_input = args.next().and_then(|raw| parse_external_key_input(&raw));
            true
        }
        "--external-key-regex" => {
            cfg.external_key_regex = args.next();
            true
        }
        "--external-key-replace" => {
            cfg.external_key_replace = args.next();
            true
        }
        "--external-scope-regex" => {
            cfg.external_scope_regex = args.next();
            true
        }
        "--external-show-unmatched" => {
            cfg.external_show_unmatched = true;
            true
        }
        _ => false,
    }
}

fn handle_debug_arg(
    arg: &str,
    args: &mut impl Iterator<Item = String>,
    cfg: &mut app::StartupConfig,
) -> bool {
    match arg {
        "--debug" => {
            cfg.debug.enabled = true;
            true
        }
        "--debug-log" => {
            cfg.debug.enabled = true;
            cfg.debug.log_path = next_path(args);
            true
        }
        "--debug-input-trace" => {
            cfg.debug.enabled = true;
            cfg.debug.input_trace_enabled = true;
            true
        }
        "--debug-event-trace" => {
            cfg.debug.enabled = true;
            cfg.debug.event_trace_enabled = true;
            true
        }
        "--debug-input-trace-console" => {
            cfg.debug.enabled = true;
            cfg.debug.input_trace_enabled = true;
            cfg.debug.input_trace_to_console = true;
            true
        }
        "--auto-run" => {
            cfg.debug.enabled = true;
            cfg.debug.auto_run = true;
            true
        }
        "--auto-run-editor" => {
            cfg.debug.enabled = true;
            cfg.debug.auto_run = true;
            cfg.debug.auto_run_editor = true;
            true
        }
        "--auto-run-pitch-shift" => {
            if let Some(value) = next_parsed::<f32>(args) {
                cfg.debug.enabled = true;
                cfg.debug.auto_run = true;
                cfg.debug.auto_run_pitch_shift_semitones = Some(value);
            }
            true
        }
        "--auto-run-time-stretch" => {
            if let Some(value) = next_parsed::<f32>(args) {
                cfg.debug.enabled = true;
                cfg.debug.auto_run = true;
                cfg.debug.auto_run_time_stretch_rate = Some(value);
            }
            true
        }
        "--auto-run-delay" => {
            if let Some(value) = next_parsed::<u32>(args) {
                cfg.debug.auto_run_delay_frames = value;
            }
            true
        }
        "--auto-run-no-exit" => {
            cfg.debug.auto_run_exit = false;
            true
        }
        "--debug-check-interval" => {
            if let Some(value) = next_parsed::<u32>(args) {
                cfg.debug.check_interval_frames = value;
            }
            true
        }
        _ => false,
    }
}

fn handle_mcp_arg(
    arg: &str,
    args: &mut impl Iterator<Item = String>,
    cfg: &mut app::StartupConfig,
) -> bool {
    match arg {
        "--mcp-stdio" => {
            cfg.mcp_stdio = true;
            true
        }
        "--mcp-http" => {
            cfg.mcp_http_addr = Some(neowaves::mcp::DEFAULT_HTTP_ADDR.to_string());
            true
        }
        "--mcp-http-addr" => {
            cfg.mcp_http_addr = args.next();
            true
        }
        "--mcp-allow-path" => {
            if let Some(path) = next_path(args) {
                cfg.mcp_allow_paths.push(path);
            }
            true
        }
        "--mcp-allow-write" => {
            cfg.mcp_allow_write = true;
            cfg.mcp_read_only = false;
            true
        }
        "--mcp-allow-export" => {
            cfg.mcp_allow_export = true;
            true
        }
        "--mcp-readwrite" => {
            cfg.mcp_read_only = false;
            true
        }
        _ => false,
    }
}

fn handle_help_arg(arg: &str) -> bool {
    if matches!(arg, "--help" | "-h") {
        eprintln!("{HELP_TEXT}");
        std::process::exit(0);
    }
    false
}

fn handle_positional_arg(arg: &str, cfg: &mut app::StartupConfig) {
    if arg.starts_with('-') {
        return;
    }
    let path = PathBuf::from(arg);
    if is_session_path(&path) {
        cfg.open_project = Some(path);
        return;
    }
    if path.is_dir() {
        if cfg.open_files.is_empty() {
            cfg.open_folder = Some(path);
        }
    } else {
        cfg.open_files.push(path);
    }
}

fn push_open_file(cfg: &mut app::StartupConfig, path: PathBuf) {
    if is_session_path(&path) {
        cfg.open_project = Some(path);
    } else {
        cfg.open_files.push(path);
    }
}

fn next_path(args: &mut impl Iterator<Item = String>) -> Option<PathBuf> {
    args.next().map(PathBuf::from)
}

fn next_parsed<T: std::str::FromStr>(args: &mut impl Iterator<Item = String>) -> Option<T> {
    args.next()?.parse().ok()
}

fn parse_view_mode(raw: &str) -> Option<app::ViewMode> {
    match raw.to_ascii_lowercase().as_str() {
        "wave" | "waveform" => Some(app::ViewMode::Waveform),
        "spec" | "spectrogram" => Some(app::ViewMode::Spectrogram),
        "log" => Some(app::ViewMode::Log),
        "mel" => Some(app::ViewMode::Mel),
        "tempogram" => Some(app::ViewMode::Tempogram),
        "chromagram" => Some(app::ViewMode::Chromagram),
        "other" => Some(app::ViewMode::Tempogram),
        _ => None,
    }
}

fn parse_toggle_flag(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => Some(true),
        "off" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn parse_external_key_rule(raw: &str) -> Option<app::ExternalKeyRule> {
    match raw.to_ascii_lowercase().as_str() {
        "file" | "filename" => Some(app::ExternalKeyRule::FileName),
        "stem" => Some(app::ExternalKeyRule::Stem),
        "regex" => Some(app::ExternalKeyRule::Regex),
        _ => None,
    }
}

fn parse_external_key_input(raw: &str) -> Option<app::ExternalRegexInput> {
    match raw.to_ascii_lowercase().as_str() {
        "file" | "filename" => Some(app::ExternalRegexInput::FileName),
        "stem" => Some(app::ExternalRegexInput::Stem),
        "path" => Some(app::ExternalRegexInput::Path),
        "dir" | "directory" => Some(app::ExternalRegexInput::Dir),
        _ => None,
    }
}

fn is_session_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("nwsess") || ext.eq_ignore_ascii_case("nwproj"))
        .unwrap_or(false)
}
