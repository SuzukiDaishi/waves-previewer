use egui::Color32;

use crate::app::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn list_art_texture_for_path(
        &mut self,
        ctx: &egui::Context,
        path: &std::path::Path,
        art: std::sync::Arc<egui::ColorImage>,
    ) -> egui::TextureHandle {
        if let Some(texture) = self.list_art_textures.get(path) {
            return texture.clone();
        }
        let texture = ctx.load_texture(
            format!("list-cover-art:{}", path.display()),
            (*art).clone(),
            egui::TextureOptions::LINEAR,
        );
        self.list_art_textures
            .insert(path.to_path_buf(), texture.clone());
        texture
    }

    pub(super) fn open_list_art_window(&mut self, ctx: &egui::Context, path: &std::path::Path) {
        const MODAL_MAX_DIM: u32 = 1400;
        const MAX_ARTWORK_BYTES: usize = 24 * 1024 * 1024;

        self.show_list_art_window = true;
        self.list_art_window_path = Some(path.to_path_buf());
        self.list_art_window_error = None;

        let Some(bytes) = crate::audio_io::read_embedded_artwork(path) else {
            self.list_art_window_texture = None;
            self.list_art_window_error = Some("No embedded artwork.".to_string());
            return;
        };
        if bytes.is_empty() || bytes.len() > MAX_ARTWORK_BYTES {
            self.list_art_window_texture = None;
            self.list_art_window_error = Some("Artwork is unavailable or too large.".to_string());
            return;
        }

        let image = match image::load_from_memory(&bytes) {
            Ok(image) => image,
            Err(err) => {
                self.list_art_window_texture = None;
                self.list_art_window_error = Some(format!("Failed to decode artwork: {err}"));
                return;
            }
        };
        let image = if image.width().max(image.height()) > MODAL_MAX_DIM {
            image.resize(
                MODAL_MAX_DIM,
                MODAL_MAX_DIM,
                image::imageops::FilterType::Lanczos3,
            )
        } else {
            image
        };
        let rgba = image.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        let color_image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
        let texture = ctx.load_texture(
            format!("list-art-modal:{}", path.display()),
            color_image,
            egui::TextureOptions::LINEAR,
        );
        self.list_art_window_texture = Some(texture);
    }

    pub(in crate::app) fn ui_list_art_window(&mut self, ctx: &egui::Context) {
        if !self.show_list_art_window {
            return;
        }
        let viewport = ctx.content_rect();
        let window_max = egui::vec2(
            (viewport.width() * 0.82).max(320.0),
            (viewport.height() * 0.86).max(320.0),
        );
        let texture_size = self
            .list_art_window_texture
            .as_ref()
            .map(|texture| texture.size_vec2())
            .unwrap_or(egui::vec2(480.0, 480.0));
        let content_target = egui::vec2(
            texture_size.x.min(window_max.x - 36.0).max(220.0),
            texture_size.y.min(window_max.y - 110.0).max(180.0),
        );
        let window_default = egui::vec2(
            (content_target.x + 36.0).clamp(320.0, window_max.x),
            (content_target.y + 110.0).clamp(260.0, window_max.y),
        );
        let mut open = self.show_list_art_window;
        let mut close_clicked = false;
        egui::Window::new("Artwork")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(window_default)
            .min_size(egui::vec2(320.0, 260.0))
            .max_size(window_max)
            .constrain_to(viewport)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                if let Some(path) = self.list_art_window_path.as_ref() {
                    ui.label(path.display().to_string());
                    ui.separator();
                }
                if let Some(error) = self.list_art_window_error.as_ref() {
                    ui.colored_label(Color32::LIGHT_RED, error);
                    return;
                }
                let Some(texture) = self.list_art_window_texture.as_ref() else {
                    ui.label("No artwork.");
                    return;
                };
                let image_max = egui::vec2(
                    ui.available_width().max(1.0),
                    (viewport.height() * 0.72).max(180.0),
                );
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .max_width(image_max.x)
                    .max_height(image_max.y)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Image::from_texture(texture)
                                .shrink_to_fit()
                                .max_size(image_max),
                        );
                    });
                ui.separator();
                if ui.button("Close").clicked() {
                    close_clicked = true;
                }
            });
        if close_clicked {
            open = false;
        }
        if !open {
            self.show_list_art_window = false;
            self.list_art_window_path = None;
            self.list_art_window_texture = None;
            self.list_art_window_error = None;
        }
    }
}
