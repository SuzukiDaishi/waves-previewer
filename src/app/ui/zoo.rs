use std::time::Instant;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_zoo_menu(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.menu_button("Zoo", |ui| {
            let mut prefs_dirty = false;
            if ui.checkbox(&mut self.zoo_enabled, "Enable Zoo").changed() {
                prefs_dirty = true;
            }
            if ui.checkbox(&mut self.zoo_walk_enabled, "Walk in Editor").changed() {
                prefs_dirty = true;
            }
            if ui
                .checkbox(&mut self.zoo_voice_enabled, "Voice on Touch")
                .changed()
            {
                prefs_dirty = true;
            }
            if ui.checkbox(&mut self.zoo_use_bpm, "Follow BPM").changed() {
                prefs_dirty = true;
            }
            if ui.checkbox(&mut self.zoo_flip_manual, "Flip Image").changed() {
                prefs_dirty = true;
            }
            ui.separator();
            ui.label("GIF/Image");
            let gif_label = self
                .zoo_gif_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".to_string());
            ui.add(
                egui::Label::new(egui::RichText::new(gif_label).monospace())
                    .truncate()
                    .show_tooltip_when_elided(true),
            );
            ui.horizontal(|ui| {
                if ui.button("Select GIF...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("image", &["gif", "png", "webp"])
                        .pick_file()
                    {
                        self.set_zoo_gif_path(Some(path));
                        prefs_dirty = true;
                    }
                }
                if ui.button("Clear").clicked() {
                    self.set_zoo_gif_path(None);
                    prefs_dirty = true;
                }
            });
            ui.separator();
            ui.label("Voice (optional)");
            let voice_label = self
                .zoo_voice_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".to_string());
            ui.add(
                egui::Label::new(egui::RichText::new(voice_label).monospace())
                    .truncate()
                    .show_tooltip_when_elided(true),
            );
            ui.horizontal(|ui| {
                if ui.button("Select Voice...").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("audio", &["wav", "mp3", "m4a", "ogg"])
                        .pick_file()
                    {
                        self.set_zoo_voice_path(Some(path));
                        prefs_dirty = true;
                    }
                }
                if ui.button("Clear").clicked() {
                    self.set_zoo_voice_path(None);
                    prefs_dirty = true;
                }
                if ui.button("Test").clicked() {
                    self.play_zoo_voice();
                }
            });
            ui.separator();
            if ui
                .add(egui::Slider::new(&mut self.zoo_speed, 40.0..=360.0).text("Walk Speed"))
                .changed()
            {
                prefs_dirty = true;
            }
            if ui
                .add(egui::Slider::new(&mut self.zoo_scale, 0.25..=2.5).text("Size"))
                .changed()
            {
                prefs_dirty = true;
            }
            if ui
                .add(egui::Slider::new(&mut self.zoo_opacity, 0.3..=1.0).text("Opacity"))
                .changed()
            {
                prefs_dirty = true;
            }
            if let Some(err) = self.zoo_last_error.as_deref() {
                ui.separator();
                ui.label(egui::RichText::new(err).color(egui::Color32::LIGHT_RED));
            }
            if prefs_dirty {
                self.save_prefs();
            }
        });
    }

    pub(in crate::app) fn ui_editor_zoo_overlay(
        &mut self,
        ctx: &egui::Context,
        tab_idx: Option<usize>,
        editor_rect: egui::Rect,
    ) {
        if !self.zoo_enabled {
            return;
        }
        if self.zoo_frames_raw.is_empty() && self.zoo_gif_path.is_some() {
            self.reload_zoo_gif_frames();
        }
        self.ensure_zoo_textures(ctx);
        if self.zoo_frames_tex.is_empty() {
            return;
        }
        let now = Instant::now();
        let dt = (now - self.zoo_last_tick).as_secs_f32().clamp(0.0, 0.1);
        self.zoo_last_tick = now;

        let mut bpm_mul = 1.0f32;
        if self.zoo_use_bpm {
            if let Some(tab) = tab_idx.and_then(|i| self.tabs.get(i)) {
                if tab.bpm_enabled && tab.bpm_value.is_finite() && tab.bpm_value > 1.0 {
                    bpm_mul = (tab.bpm_value / 120.0).clamp(0.5, 2.5);
                }
            }
        }
        let energy = self.zoo_energy_level();
        let playing = self
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        let walk_mul = if playing { 0.75 + energy * 0.9 } else { 0.45 };
        let anim_mul = if playing { 0.8 + energy * 1.8 } else { 0.5 };

        let frame_count = self.zoo_frames_tex.len();
        let total_anim_s: f32 = self.zoo_frames_tex.iter().map(|f| f.delay_s).sum::<f32>().max(0.016);
        self.zoo_anim_clock =
            (self.zoo_anim_clock + dt * anim_mul.max(bpm_mul)).rem_euclid(total_anim_s);
        let mut acc = 0.0f32;
        let mut frame_idx = 0usize;
        for (idx, frame) in self.zoo_frames_tex.iter().enumerate() {
            acc += frame.delay_s;
            if self.zoo_anim_clock <= acc || idx + 1 == frame_count {
                frame_idx = idx;
                break;
            }
        }
        let frame = self.zoo_frames_tex[frame_idx].clone();
        let source_size = frame.texture.size_vec2();
        if source_size.x <= 1.0 || source_size.y <= 1.0 {
            return;
        }
        let scale = self.zoo_scale.clamp(0.25, 2.5);
        let base_w = source_size.x * scale;
        let base_h = source_size.y * scale;
        let min_x = editor_rect.min.x + 8.0;
        let max_x = (editor_rect.max.x - base_w - 8.0).max(min_x);
        if self.zoo_pos_x < min_x || self.zoo_pos_x > max_x {
            self.zoo_pos_x = min_x;
        }
        if self.zoo_walk_enabled {
            let move_px = self.zoo_speed.max(40.0) * walk_mul * bpm_mul * dt;
            self.zoo_pos_x += self.zoo_dir * move_px;
            if self.zoo_pos_x <= min_x {
                self.zoo_pos_x = min_x;
                self.zoo_dir = 1.0;
            } else if self.zoo_pos_x >= max_x {
                self.zoo_pos_x = max_x;
                self.zoo_dir = -1.0;
            }
        }
        let mut draw_w = base_w;
        let mut draw_h = base_h;
        if self
            .zoo_squish_until
            .map(|until| until > now)
            .unwrap_or(false)
        {
            draw_w *= 1.1;
            draw_h *= 0.86;
        }
        let draw_x = self.zoo_pos_x - (draw_w - base_w) * 0.5;
        let draw_y = (editor_rect.max.y - draw_h - 8.0).max(editor_rect.min.y + 8.0);
        egui::Area::new(egui::Id::new(("zoo_overlay", tab_idx)))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(draw_x, draw_y))
            .show(ctx, |ui| {
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(draw_w, draw_h), egui::Sense::click());
                if resp.hovered() {
                    ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    self.zoo_squish_until =
                        Some(now + std::time::Duration::from_millis(150));
                    self.play_zoo_voice();
                }
                let alpha = (self.zoo_opacity.clamp(0.3, 1.0) * 255.0).round() as u8;
                let auto_flip = self.zoo_walk_enabled && self.zoo_dir < 0.0;
                let flip_x = self.zoo_flip_manual ^ auto_flip;
                let uv = if flip_x {
                    egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
                } else {
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
                };
                ui.painter().image(
                    frame.texture.id(),
                    rect,
                    uv,
                    egui::Color32::from_white_alpha(alpha),
                );
            });
    }
}
