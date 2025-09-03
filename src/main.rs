mod audio;
mod wave;
mod app;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "waves-previewer",
        native_options,
        Box::new(|cc| Box::new(app::WavesPreviewer::new(cc).expect("failed to init app"))),
    )
}

