use egui::Vec2;
use egui_kittest::Harness;

use crate::{StartupConfig, WavesPreviewer};

pub fn harness_with_startup(startup: StartupConfig) -> Harness<'static, WavesPreviewer> {
    harness_with_startup_size(startup, Vec2::new(1280.0, 720.0))
}

pub fn harness_with_startup_size(
    startup: StartupConfig,
    size: Vec2,
) -> Harness<'static, WavesPreviewer> {
    Harness::builder()
        .with_size(size)
        .with_os(egui::os::OperatingSystem::from_target_os())
        .build_eframe(|cc| WavesPreviewer::new_for_test(cc, startup).expect("init test app"))
}

/// A harness on a display scaled by `pixels_per_point` (1.25 for 125 %):
/// the native scale, which the app's zoom factor of 1.0 multiplies. Setting
/// it on a running harness does not stick -- the app resets the zoom.
pub fn harness_with_startup_scaled(
    startup: StartupConfig,
    size: Vec2,
    pixels_per_point: f32,
) -> Harness<'static, WavesPreviewer> {
    Harness::builder()
        .with_size(size)
        .with_pixels_per_point(pixels_per_point)
        .with_os(egui::os::OperatingSystem::from_target_os())
        .build_eframe(|cc| WavesPreviewer::new_for_test(cc, startup).expect("init test app"))
}

pub fn harness_default() -> Harness<'static, WavesPreviewer> {
    harness_with_startup(StartupConfig::default())
}
