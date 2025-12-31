mod app;
mod audio;
mod wave;

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
            "--help" | "-h" => {
                eprintln!(
                    "Usage:\n  waves-previewer [options]\n\nOptions:\n  --open-folder <dir>\n  --open-file <wav> (repeatable)\n  --open-first\n  --screenshot <path.png>\n  --screenshot-delay <frames>\n  --exit-after-screenshot\n  --dummy-list <count>\n  --debug\n  --debug-log <path>\n  --auto-run\n  --auto-run-delay <frames>\n  --auto-run-no-exit\n  --debug-check-interval <frames>\n  --help"
                );
                std::process::exit(0);
            }
            _ => {}
        }
    }
    cfg
}

fn main() -> eframe::Result<()> {
    let startup = parse_startup_config();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_min_inner_size([960.0, 600.0])
            .with_inner_size([1280.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "waves-previewer",
        native_options,
        Box::new(move |cc| {
            Box::new(
                app::WavesPreviewer::new(cc, startup.clone()).expect("failed to init app"),
            )
        }),
    )
}
