use std::path::Path;

use anyhow::{Context, Result};

pub fn save_color_image_png(path: &Path, image: &egui::ColorImage) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create screenshot dir {}", parent.display()))?;
    }
    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for px in &image.pixels {
        rgba.push(px.r());
        rgba.push(px.g());
        rgba.push(px.b());
        rgba.push(px.a());
    }
    let width = image.size[0] as u32;
    let height = image.size[1] as u32;
    let img = image::RgbaImage::from_raw(width, height, rgba)
        .context("convert screenshot buffer")?;
    img.save(path)
        .with_context(|| format!("save screenshot {}", path.display()))?;
    Ok(())
}
