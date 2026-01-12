#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use neowaves::app;

fn parse_startup_config() -> app::StartupConfig {
    let mut cfg = app::StartupConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--open-folder" => {
                if let Some(p) = args.next() {
                    cfg.open_folder = Some(std::path::PathBuf::from(p));
                }
            }
            "--open-file" => {
                if let Some(p) = args.next() {
                    cfg.open_files.push(std::path::PathBuf::from(p));
                }
            }
            "--open-first" => {
                cfg.open_first = true;
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
                    "Usage:\n  neowaves [options]\n\nOptions:\n  --open-folder <dir>\n  --open-file <audio> (repeatable)\n  --open-first\n  --screenshot <path.png>\n  --screenshot-delay <frames>\n  --exit-after-screenshot\n  --dummy-list <count>\n  --debug\n  --debug-log <path>\n  --auto-run\n  --auto-run-pitch-shift <semitones>\n  --auto-run-time-stretch <rate>\n  --auto-run-delay <frames>\n  --auto-run-no-exit\n  --debug-check-interval <frames>\n  --mcp-stdio\n  --mcp-http\n  --mcp-http-addr <addr>\n  --mcp-allow-path <path>\n  --mcp-allow-write\n  --mcp-allow-export\n  --mcp-readwrite\n  --help"
                );
                std::process::exit(0);
            }
            _ => {
                if arg.starts_with('-') {
                    continue;
                }
                let path = std::path::PathBuf::from(&arg);
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
    let startup = parse_startup_config();
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
