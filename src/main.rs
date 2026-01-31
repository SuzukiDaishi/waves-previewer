#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use neowaves::{app, ipc};

fn parse_startup_config() -> app::StartupConfig {
    let mut cfg = app::StartupConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--open-project" => {
                if let Some(p) = args.next() {
                    cfg.open_project = Some(std::path::PathBuf::from(p));
                }
            }
            "--open-folder" => {
                if let Some(p) = args.next() {
                    cfg.open_folder = Some(std::path::PathBuf::from(p));
                }
            }
            "--open-file" => {
                if let Some(p) = args.next() {
                    let path = std::path::PathBuf::from(p);
                    if path
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.eq_ignore_ascii_case("nwproj"))
                        .unwrap_or(false)
                    {
                        cfg.open_project = Some(path);
                    } else {
                        cfg.open_files.push(path);
                    }
                }
            }
            "--open-first" => {
                cfg.open_first = true;
            }
            "--open-view-mode" => {
                if let Some(v) = args.next() {
                    let mode = match v.to_lowercase().as_str() {
                        "wave" | "waveform" => Some(app::ViewMode::Waveform),
                        "spec" | "spectrogram" => Some(app::ViewMode::Spectrogram),
                        "mel" => Some(app::ViewMode::Mel),
                        _ => None,
                    };
                    if let Some(mode) = mode {
                        cfg.open_view_mode = Some(mode);
                    }
                }
            }
            "--waveform-overlay" => {
                if let Some(v) = args.next() {
                    let flag = match v.to_lowercase().as_str() {
                        "on" | "true" | "1" => Some(true),
                        "off" | "false" | "0" => Some(false),
                        _ => None,
                    };
                    if let Some(flag) = flag {
                        cfg.open_waveform_overlay = Some(flag);
                    }
                }
            }
            "--screenshot" => {
                if let Some(p) = args.next() {
                    cfg.screenshot_path = Some(std::path::PathBuf::from(p));
                }
            }
            "--screenshot-delay" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse::<u32>() {
                        cfg.screenshot_delay_frames = n;
                    }
                }
            }
            "--exit-after-screenshot" => {
                cfg.exit_after_screenshot = true;
            }
            "--dummy-list" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse::<usize>() {
                        cfg.dummy_list_count = Some(n);
                    }
                }
            }
            "--external-dialog" => {
                cfg.external_show_dialog = true;
            }
            "--debug-summary" => {
                if let Some(p) = args.next() {
                    cfg.debug_summary_path = Some(std::path::PathBuf::from(p));
                }
            }
            "--debug-summary-delay" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse::<u32>() {
                        cfg.debug_summary_delay_frames = n;
                    }
                }
            }
            "--external-file" => {
                if let Some(p) = args.next() {
                    cfg.external_path = Some(std::path::PathBuf::from(p));
                }
            }
            "--external-dummy" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse::<usize>() {
                        cfg.external_dummy_rows = Some(n);
                    }
                }
            }
            "--external-dummy-cols" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse::<usize>() {
                        cfg.external_dummy_cols = n.max(1);
                    }
                }
            }
            "--external-dummy-path" => {
                if let Some(p) = args.next() {
                    cfg.external_dummy_path = Some(std::path::PathBuf::from(p));
                }
            }
            "--external-dummy-merge" => {
                cfg.external_dummy_merge = true;
            }
            "--external-sheet" => {
                if let Some(v) = args.next() {
                    cfg.external_sheet = Some(v);
                }
            }
            "--external-has-header" => {
                if let Some(v) = args.next() {
                    let flag = match v.to_lowercase().as_str() {
                        "on" | "true" | "1" => Some(true),
                        "off" | "false" | "0" => Some(false),
                        _ => None,
                    };
                    if let Some(flag) = flag {
                        cfg.external_has_header = Some(flag);
                    }
                }
            }
            "--external-header-row" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse::<usize>() {
                        cfg.external_header_row = if n == 0 { None } else { Some(n - 1) };
                    }
                }
            }
            "--external-data-row" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse::<usize>() {
                        cfg.external_data_row = if n == 0 { None } else { Some(n - 1) };
                    }
                }
            }
            "--external-key-rule" => {
                if let Some(v) = args.next() {
                    cfg.external_key_rule = match v.to_lowercase().as_str() {
                        "file" | "filename" => Some(app::ExternalKeyRule::FileName),
                        "stem" => Some(app::ExternalKeyRule::Stem),
                        "regex" => Some(app::ExternalKeyRule::Regex),
                        _ => None,
                    };
                }
            }
            "--external-key-input" => {
                if let Some(v) = args.next() {
                    cfg.external_key_input = match v.to_lowercase().as_str() {
                        "file" | "filename" => Some(app::ExternalRegexInput::FileName),
                        "stem" => Some(app::ExternalRegexInput::Stem),
                        "path" => Some(app::ExternalRegexInput::Path),
                        "dir" | "directory" => Some(app::ExternalRegexInput::Dir),
                        _ => None,
                    };
                }
            }
            "--external-key-regex" => {
                if let Some(v) = args.next() {
                    cfg.external_key_regex = Some(v);
                }
            }
            "--external-key-replace" => {
                if let Some(v) = args.next() {
                    cfg.external_key_replace = Some(v);
                }
            }
            "--external-scope-regex" => {
                if let Some(v) = args.next() {
                    cfg.external_scope_regex = Some(v);
                }
            }
            "--external-show-unmatched" => {
                cfg.external_show_unmatched = true;
            }
            "--debug" => {
                cfg.debug.enabled = true;
            }
            "--debug-log" => {
                if let Some(p) = args.next() {
                    cfg.debug.enabled = true;
                    cfg.debug.log_path = Some(std::path::PathBuf::from(p));
                }
            }
            "--auto-run" => {
                cfg.debug.enabled = true;
                cfg.debug.auto_run = true;
            }
            "--auto-run-editor" => {
                cfg.debug.enabled = true;
                cfg.debug.auto_run = true;
                cfg.debug.auto_run_editor = true;
            }
            "--auto-run-pitch-shift" => {
                if let Some(v) = args.next() {
                    if let Ok(semi) = v.parse::<f32>() {
                        cfg.debug.enabled = true;
                        cfg.debug.auto_run = true;
                        cfg.debug.auto_run_pitch_shift_semitones = Some(semi);
                    }
                }
            }
            "--auto-run-time-stretch" => {
                if let Some(v) = args.next() {
                    if let Ok(rate) = v.parse::<f32>() {
                        cfg.debug.enabled = true;
                        cfg.debug.auto_run = true;
                        cfg.debug.auto_run_time_stretch_rate = Some(rate);
                    }
                }
            }
            "--auto-run-delay" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse::<u32>() {
                        cfg.debug.auto_run_delay_frames = n;
                    }
                }
            }
            "--auto-run-no-exit" => {
                cfg.debug.auto_run_exit = false;
            }
            "--debug-check-interval" => {
                if let Some(v) = args.next() {
                    if let Ok(n) = v.parse::<u32>() {
                        cfg.debug.check_interval_frames = n;
                    }
                }
            }
            "--mcp-stdio" => {
                cfg.mcp_stdio = true;
            }
            "--mcp-http" => {
                cfg.mcp_http_addr = Some(neowaves::mcp::DEFAULT_HTTP_ADDR.to_string());
            }
            "--mcp-http-addr" => {
                if let Some(p) = args.next() {
                    cfg.mcp_http_addr = Some(p);
                }
            }
            "--mcp-allow-path" => {
                if let Some(p) = args.next() {
                    cfg.mcp_allow_paths.push(std::path::PathBuf::from(p));
                }
            }
            "--mcp-allow-write" => {
                cfg.mcp_allow_write = true;
                cfg.mcp_read_only = false;
            }
            "--mcp-allow-export" => {
                cfg.mcp_allow_export = true;
            }
            "--mcp-readwrite" => {
                cfg.mcp_read_only = false;
            }
            "--help" | "-h" => {
                eprintln!(
                    "Usage:\n  neowaves [options]\n\nOptions:\n  --open-project <project.nwproj>\n  --open-folder <dir>\n  --open-file <audio> (repeatable)\n  --open-first\n  --open-view-mode <wave|spec|mel>\n  --waveform-overlay <on|off>\n  --screenshot <path.png>\n  --screenshot-delay <frames>\n  --exit-after-screenshot\n  --dummy-list <count>\n  --external-dialog\n  --debug-summary <path>\n  --debug-summary-delay <frames>\n  --external-file <path>\n  --external-dummy <rows>\n  --external-dummy-cols <count>\n  --external-dummy-path <path>\n  --external-dummy-merge\n  --external-sheet <name>\n  --external-has-header <on|off>\n  --external-header-row <n> (1-based, 0=auto)\n  --external-data-row <n> (1-based, 0=auto)\n  --external-key-rule <file|stem|regex>\n  --external-key-input <file|stem|path|dir>\n  --external-key-regex <pattern>\n  --external-key-replace <text>\n  --external-scope-regex <pattern>\n  --external-show-unmatched\n  --debug\n  --debug-log <path>\n  --auto-run\n  --auto-run-editor\n  --auto-run-pitch-shift <semitones>\n  --auto-run-time-stretch <rate>\n  --auto-run-delay <frames>\n  --auto-run-no-exit\n  --debug-check-interval <frames>\n  --mcp-stdio\n  --mcp-http\n  --mcp-http-addr <addr>\n  --mcp-allow-path <path>\n  --mcp-allow-write\n  --mcp-allow-export\n  --mcp-readwrite\n  --help"
                );
                std::process::exit(0);
            }
            _ => {
                if arg.starts_with('-') {
                    continue;
                }
                let path = std::path::PathBuf::from(&arg);
                if path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("nwproj"))
                    .unwrap_or(false)
                {
                    cfg.open_project = Some(path);
                    continue;
                }
                if path.is_dir() {
                    if cfg.open_files.is_empty() {
                        cfg.open_folder = Some(path);
                    }
                } else {
                    cfg.open_files.push(path);
                }
            }
        }
    }
    cfg
}

fn main() -> eframe::Result<()> {
    let mut startup = parse_startup_config();
    let mut request = ipc::IpcRequest::empty();
    request.project = startup.open_project.clone();
    request.files = startup.open_files.clone();
    if request.has_payload() && ipc::try_send_request(&request) {
        return Ok(());
    }
    if let Ok(rx) = ipc::start_listener() {
        startup.ipc_rx = Some(std::sync::Arc::new(std::sync::Mutex::new(rx)));
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_min_inner_size([960.0, 600.0])
        .with_inner_size([1280.0, 720.0]);
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "NeoWaves Audio List Editor",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(
                app::WavesPreviewer::new(cc, startup.clone()).expect("failed to init app"),
            ))
        }),
    )
}

fn load_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../icons/icon.png");
    let image = image::load_from_memory(bytes).ok()?;
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}
