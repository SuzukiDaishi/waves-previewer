use egui::Vec2;
use egui_kittest::Harness;

use crate::{StartupConfig, WavesPreviewer};

pub fn harness_with_startup(startup: StartupConfig) -> Harness<'static, WavesPreviewer> {
    Harness::builder()
        .with_size(Vec2::new(1280.0, 720.0))
        .with_os(egui::os::OperatingSystem::from_target_os())
        .build_eframe(|cc| WavesPreviewer::new_for_test(cc, startup).expect("init test app"))
}

pub fn harness_default() -> Harness<'static, WavesPreviewer> {
    harness_with_startup(StartupConfig::default())
}
