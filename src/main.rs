#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;

use neowaves::{app, ipc};

fn main() -> eframe::Result<()> {
    let mut startup = cli::parse_startup_config();
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
