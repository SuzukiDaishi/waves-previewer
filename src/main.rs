#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use neowaves::{app, cli, ipc};

fn main() -> eframe::Result<()> {
    let mut startup = match cli::parse_runtime_mode() {
        cli::ParseOutcome::Run(cli::RuntimeMode::Gui(startup)) => startup,
        cli::ParseOutcome::Run(cli::RuntimeMode::Cli(command)) => {
            if app::run_cli(command).is_err() {
                std::process::exit(1);
            }
            return Ok(());
        }
        cli::ParseOutcome::Exit(code) => {
            std::process::exit(code);
        }
    };
    let mut request = ipc::IpcRequest::empty();
    request.project = startup.open_project.clone();
    request.files = startup.open_files.clone();
    if !startup.no_ipc_forward && request.has_payload() && ipc::try_send_request(&request) {
        return Ok(());
    }
    if !startup.no_ipc_forward {
        if let Ok(rx) = ipc::start_listener() {
            startup.ipc_rx = Some(std::sync::Arc::new(std::sync::Mutex::new(rx)));
        }
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
    let app_title = format!("NeoWaves Audio List Editor v{}", env!("CARGO_PKG_VERSION"));
    eframe::run_native(
        &app_title,
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
