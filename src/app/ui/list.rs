use egui::{Align, Color32, RichText, Sense};
use egui_extras::TableBuilder;
use std::borrow::Cow;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn list_type_sort_key(item: &crate::app::types::MediaItem) -> Cow<'_, str> {
        match item.source {
            crate::app::types::MediaSource::Virtual => Cow::Borrowed("vir"),
            crate::app::types::MediaSource::External => Cow::Borrowed("ext"),
            crate::app::types::MediaSource::File => item
                .path
                .extension()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Borrowed("file")),
        }
    }

    fn list_type_badge_for_item(
        item: &crate::app::types::MediaItem,
    ) -> (String, String, Color32, Color32) {
        let (label, tooltip, fill, stroke) = match item.source {
            crate::app::types::MediaSource::Virtual => (
                "VIR".to_string(),
                "Virtual audio".to_string(),
                Color32::from_rgb(112, 78, 32),
                Color32::from_rgb(224, 178, 110),
            ),
            crate::app::types::MediaSource::External => (
                "EXT".to_string(),
                "External row".to_string(),
                Color32::from_rgb(68, 78, 98),
                Color32::from_rgb(158, 176, 205),
            ),
            crate::app::types::MediaSource::File => {
                let ext = item
                    .path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .unwrap_or_default();
                match ext.as_str() {
                    "wav" => (
                        "WAV".to_string(),
                        "WAV file".to_string(),
                        Color32::from_rgb(48, 96, 168),
                        Color32::from_rgb(120, 182, 255),
                    ),
                    "mp3" => (
                        "MP3".to_string(),
                        "MP3 file".to_string(),
                        Color32::from_rgb(146, 94, 34),
                        Color32::from_rgb(236, 178, 92),
                    ),
                    "m4a" => (
                        "M4A".to_string(),
                        "M4A file".to_string(),
                        Color32::from_rgb(40, 128, 92),
                        Color32::from_rgb(118, 226, 174),
                    ),
                    "ogg" => (
                        "OGG".to_string(),
                        "OGG file".to_string(),
                        Color32::from_rgb(88, 106, 128),
                        Color32::from_rgb(172, 198, 228),
                    ),
                    _ => {
                        let upper = if ext.is_empty() {
                            "FILE".to_string()
                        } else {
                            ext.to_ascii_uppercase().chars().take(4).collect()
                        };
                        (
                            upper,
                            if ext.is_empty() {
                                "File".to_string()
                            } else {
                                format!(".{ext} file")
                            },
                            Color32::from_rgb(84, 88, 98),
                            Color32::from_rgb(182, 188, 202),
                        )
                    }
                }
            }
        };

        let stroke = if matches!(item.status, crate::app::types::MediaStatus::DecodeFailed(_)) {
            Color32::from_rgb(220, 110, 110)
        } else {
            stroke
        };
        (label, tooltip, fill, stroke)
    }

    fn paint_list_type_badge(
        ui: &egui::Ui,
        rect: egui::Rect,
        text_height: f32,
        label: &str,
        fill: Color32,
        stroke: Color32,
    ) {
        ui.painter().rect_filled(rect, 5.0, fill);
        ui.painter().rect_stroke(
            rect,
            5.0,
            egui::Stroke::new(1.0, stroke),
            egui::StrokeKind::Outside,
        );
        let fid = egui::FontId::monospace((text_height * 0.88).max(9.0));
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            fid,
            Color32::WHITE,
        );
    }

    fn list_art_texture_for_path(
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

    fn open_list_art_window(&mut self, ctx: &egui::Context, path: &std::path::Path) {
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

    fn list_row_context_menu_contents(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected_paths();
        let has_selection = !selected.is_empty();
        if ui
            .add_enabled(has_selection, egui::Button::new("Copy to Clipboard"))
            .clicked()
        {
            self.copy_selected_to_clipboard();
            ui.close();
        }
        let can_paste = self
            .clipboard_payload
            .as_ref()
            .map(|p| !p.items.is_empty())
            .unwrap_or(false)
            || !self.get_clipboard_files().is_empty();
        if ui
            .add_enabled(can_paste, egui::Button::new("Paste"))
            .clicked()
        {
            self.paste_clipboard_to_list();
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Export Selected..."))
            .clicked()
        {
            self.trigger_save_selected();
            ui.close();
        }
        let effect_targets = selected.clone();
        ui.menu_button("Effect", |ui| {
            let entries = self.effect_graph.library.entries.clone();
            if entries.is_empty() {
                ui.label("No templates");
            }
            for entry in entries {
                let resp = ui.add_enabled(entry.valid, egui::Button::new(entry.name.clone()));
                if resp.clicked() {
                    if let Err(err) = self
                        .apply_effect_graph_template_to_paths(&entry.template_id, &effect_targets)
                    {
                        self.push_effect_graph_console(
                            crate::app::types::EffectGraphSeverity::Error,
                            "apply",
                            err,
                            None,
                        );
                    }
                    ui.close();
                }
            }
        });
        ui.menu_button("Effect Graph", |ui| {
            let can_open = has_selection;
            if ui
                .add_enabled(can_open, egui::Button::new("Open"))
                .clicked()
            {
                if let Some(path) = selected.first().cloned() {
                    self.open_effect_graph_workspace();
                    self.effect_graph.tester.target_path = Some(path.clone());
                    self.effect_graph.tester.target_path_input = path.display().to_string();
                    self.effect_graph.tester.last_input_bus = None;
                    self.effect_graph.tester.last_input_audio = None;
                    self.effect_graph.tester.last_output_bus = None;
                    self.effect_graph.tester.last_output_audio = None;
                    self.effect_graph.tester.playback_target = None;
                }
                ui.close();
            }
        });
        let transcript_targets: Vec<_> = selected
            .iter()
            .filter(|path| {
                self.item_for_path(path)
                    .map(|item| item.source == crate::app::types::MediaSource::File)
                    .unwrap_or(false)
                    && path.is_file()
                    && crate::audio_io::is_supported_audio_path(path)
            })
            .cloned()
            .collect();
        let transcript_running = self.transcript_ai_is_running();
        let transcript_ready = self.transcript_ai_menu_enabled();
        let has_transcript_targets = !transcript_targets.is_empty();
        let transcript_enabled = transcript_running || (transcript_ready && has_transcript_targets);
        let transcript_label = if transcript_running {
            "Transcript (AI) - Cancel"
        } else {
            "Transcript (AI)"
        };
        let transcript_resp =
            ui.add_enabled(transcript_enabled, egui::Button::new(transcript_label));
        if transcript_resp.clicked() {
            if transcript_running {
                self.cancel_transcript_ai_run();
            } else {
                self.run_transcript_ai_for_selected(transcript_targets);
            }
            ui.close();
        }
        if !transcript_enabled {
            let reason = if !has_transcript_targets {
                "Select at least one real audio file.".to_string()
            } else {
                self.transcript_ai_unavailable_reason()
                    .unwrap_or_else(|| "Transcript AI is unavailable.".to_string())
            };
            transcript_resp.on_hover_text(reason);
        }
        let renameable_selected = self.selected_renameable_paths();
        if renameable_selected.len() == 1 {
            if ui.button("Rename...").clicked() {
                self.open_rename_dialog(renameable_selected[0].clone());
                ui.close();
            }
        }
        let can_convert_bits = !selected.is_empty()
            && selected.iter().all(|p| {
                let is_wav = p
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("wav"))
                    .unwrap_or(false);
                is_wav
                    && p.is_file()
                    && self
                        .item_for_path(p)
                        .map(|item| item.source == crate::app::types::MediaSource::File)
                        .unwrap_or(false)
            });
        let convert_targets = if can_convert_bits {
            selected.clone()
        } else {
            Vec::new()
        };
        ui.menu_button("Convert Bits", |ui| {
            if ui
                .add_enabled(can_convert_bits, egui::Button::new("16-bit PCM"))
                .clicked()
            {
                self.spawn_convert_bits_selected(
                    convert_targets.clone(),
                    crate::wave::WavBitDepth::Pcm16,
                );
                ui.close();
            }
            if ui
                .add_enabled(can_convert_bits, egui::Button::new("24-bit PCM"))
                .clicked()
            {
                self.spawn_convert_bits_selected(
                    convert_targets.clone(),
                    crate::wave::WavBitDepth::Pcm24,
                );
                ui.close();
            }
            if ui
                .add_enabled(can_convert_bits, egui::Button::new("32-bit float"))
                .clicked()
            {
                self.spawn_convert_bits_selected(
                    convert_targets.clone(),
                    crate::wave::WavBitDepth::Float32,
                );
                ui.close();
            }
        });
        ui.menu_button("Convert Format", |ui| {
            if ui
                .add_enabled(has_selection, egui::Button::new("To WAV"))
                .clicked()
            {
                self.spawn_convert_format_selected(selected.clone(), "wav");
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("To MP3"))
                .clicked()
            {
                self.spawn_convert_format_selected(selected.clone(), "mp3");
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("To M4A"))
                .clicked()
            {
                self.spawn_convert_format_selected(selected.clone(), "m4a");
                ui.close();
            }
            if ui
                .add_enabled(has_selection, egui::Button::new("To OGG"))
                .clicked()
            {
                self.spawn_convert_format_selected(selected.clone(), "ogg");
                ui.close();
            }
        });
        if ui
            .add_enabled(has_selection, egui::Button::new("Remove from List"))
            .clicked()
        {
            self.remove_paths_from_list_with_undo(&selected);
            ui.close();
        }
        let has_edits = self.has_edits_for_paths(&selected);
        if ui
            .add_enabled(has_edits, egui::Button::new("Clear Edits"))
            .clicked()
        {
            self.clear_edits_for_paths(&selected);
            ui.close();
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("Sample Rate Convert..."))
            .clicked()
        {
            self.open_resample_dialog(selected.clone());
            ui.close();
        }
    }

    fn attach_row_context_menu(
        &mut self,
        resp: egui::Response,
        row_idx: usize,
        ctx: &egui::Context,
    ) -> egui::Response {
        if resp.secondary_clicked() && !self.selected_multi.contains(&row_idx) {
            let mods = ctx.input(|i| i.modifiers);
            self.update_selection_on_click(row_idx, mods);
        }
        resp.context_menu(|ui| {
            self.list_row_context_menu_contents(ui);
        });
        resp
    }

    pub(in crate::app) fn ui_list_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use crate::app::helpers::{
            amp_to_color, db_to_amp, db_to_color, format_duration, format_system_time_local,
            highlight_text_job, sortable_header,
        };
        use crate::app::types::SortKey;
        use std::path::PathBuf;

        let mut to_open: Option<PathBuf> = None;
        let text_height = egui::TextStyle::Body.resolve(ui.style()).size;
        let header_h = text_height * 1.6;
        let cols = self.list_columns;
        let row_h = if cols.cover_art {
            self.wave_row_h.max(text_height * 2.8).max(48.0)
        } else {
            self.wave_row_h.max(text_height * 1.3)
        };
        let avail_h = ui.available_height();
        let visible_rows = ((avail_h - header_h) / row_h).floor().max(1.0) as usize;
        ui.set_min_width(ui.available_width());
        let row_count = self.files.len().max(12);
        let external_cols = if cols.external {
            self.external_visible_columns.clone()
        } else {
            Vec::new()
        };
        let list_rect = ui.available_rect_before_wrap();
        let pointer_over_list = ui
            .input(|i| i.pointer.hover_pos())
            .map_or(false, |p| list_rect.contains(p));
        if self.debug.cfg.enabled {
            self.debug.last_pointer_over_list = pointer_over_list;
        }
        let list_focus_id = crate::app::WavesPreviewer::list_focus_id();
        let list_focus_now = ctx.memory(|m| m.has_focus(list_focus_id));
        let focused_id = ctx.memory(|m| m.focused());
        let search_focused =
            ctx.memory(|m| m.has_focus(crate::app::WavesPreviewer::search_box_id()));
        let has_non_list_focus = focused_id.is_some() && focused_id != Some(list_focus_id);
        let rename_modal_open = self.show_rename_dialog
            || self.show_batch_rename_dialog
            || self.show_export_settings
            || self.show_transcription_settings
            || self.show_resample_dialog
            || self.show_leave_prompt
            || self.show_external_dialog
            || self.show_list_art_window;
        // Keep list focus reclaim from stealing keyboard focus from active text inputs
        // (DragValue text-edit mode, settings text fields, etc.).
        let allow_focus_reclaim = !rename_modal_open && !search_focused && !has_non_list_focus;
        let focus_resp = ui.interact(list_rect, list_focus_id, Sense::click());
        if self.list_has_focus && !list_focus_now && allow_focus_reclaim {
            ctx.memory_mut(|m| m.request_focus(list_focus_id));
        }
        // NOTE: Do not force list focus on generic panel clicks.
        // It breaks in-place numeric text editing (e.g. top-bar rate and gain cells).
        let _ = focus_resp;
        let mut list_has_focus = list_focus_now || self.list_has_focus;
        if !list_has_focus
            && self.is_list_workspace_active()
            && self.selected.is_some()
            && !self.search_has_focus
            && allow_focus_reclaim
        {
            ctx.memory_mut(|m| m.request_focus(list_focus_id));
            list_has_focus = true;
            self.list_has_focus = true;
        }
        let mut key_moved = false;
        // Keyboard navigation & per-file gain adjust in list view
        // Do not gate on non-list text focus here: Up/Down must always recover list navigation
        // in list mode (e.g. after DragValue text-entry focus in topbar/list cells).
        let allow_list_keys = self.is_list_workspace_active()
            && !self.files.is_empty()
            && !search_focused
            && !rename_modal_open;
        if self.debug.cfg.enabled && self.is_list_workspace_active() && !self.files.is_empty() {
            let nav_key_pressed = ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowDown)
                    || i.key_pressed(egui::Key::ArrowUp)
                    || i.key_pressed(egui::Key::PageDown)
                    || i.key_pressed(egui::Key::PageUp)
                    || i.key_pressed(egui::Key::Home)
                    || i.key_pressed(egui::Key::End)
            });
            if nav_key_pressed && !allow_list_keys {
                self.debug_trace_input(&format!(
                    "list nav blocked (search_focused={search_focused}, has_non_list_focus={has_non_list_focus}, rename_modal_open={rename_modal_open})"
                ));
            }
        }
        let list_key_intent = if allow_list_keys {
            ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowDown)
                    || i.key_pressed(egui::Key::ArrowUp)
                    || i.key_pressed(egui::Key::Enter)
                    || i.key_pressed(egui::Key::ArrowLeft)
                    || i.key_pressed(egui::Key::ArrowRight)
                    || i.key_pressed(egui::Key::PageDown)
                    || i.key_pressed(egui::Key::PageUp)
                    || i.key_pressed(egui::Key::Home)
                    || i.key_pressed(egui::Key::End)
                    || i.key_pressed(egui::Key::Delete)
                    || ((i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::A))
            })
        } else {
            false
        };
        if allow_list_keys && list_key_intent && !rename_modal_open {
            ctx.memory_mut(|m| m.request_focus(list_focus_id));
            list_has_focus = true;
            self.list_has_focus = true;
        }
        if list_has_focus {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    list_focus_id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        tab: true,
                        ..Default::default()
                    },
                );
            });
        }
        let mut pressed_down = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
        } else {
            false
        };
        let mut pressed_up = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
        } else {
            false
        };
        // Some focused widgets (e.g. DragValue text-edit) may consume arrow keys before list handlers.
        // Use raw events as a fallback so list navigation never gets stuck in list mode.
        if allow_list_keys && (!pressed_down || !pressed_up) {
            let raw_arrow = ctx.input(|i| {
                let mut down = false;
                let mut up = false;
                for ev in &i.raw.events {
                    if let egui::Event::Key {
                        key, pressed: true, ..
                    } = ev
                    {
                        if *key == egui::Key::ArrowDown {
                            down = true;
                        } else if *key == egui::Key::ArrowUp {
                            up = true;
                        }
                    }
                }
                (down, up)
            });
            pressed_down |= raw_arrow.0;
            pressed_up |= raw_arrow.1;
        }
        let pressed_enter = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        } else {
            false
        };
        let pressed_ctrl_a = if allow_list_keys {
            ctx.input(|i| (i.modifiers.ctrl || i.modifiers.command) && i.key_pressed(egui::Key::A))
        } else {
            false
        };
        let pressed_left = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft))
        } else {
            false
        };
        let pressed_right = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight))
        } else {
            false
        };
        let pressed_pgdown = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown))
        } else {
            false
        };
        let pressed_pgup = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp))
        } else {
            false
        };
        let pressed_home = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Home))
        } else {
            false
        };
        let pressed_end = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::End))
        } else {
            false
        };
        let pressed_delete = if allow_list_keys {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Delete))
        } else {
            false
        };
        if self.is_list_workspace_active() && !self.files.is_empty() && allow_list_keys {
            if pressed_ctrl_a
                || pressed_home
                || pressed_end
                || pressed_pgdown
                || pressed_pgup
                || pressed_down
                || pressed_up
                || pressed_enter
                || pressed_delete
                || pressed_left
                || pressed_right
            {
                ctx.memory_mut(|m| m.request_focus(list_focus_id));
                list_has_focus = true;
                self.search_has_focus = false;
            }
            if pressed_ctrl_a {
                self.selected_multi.clear();
                for i in 0..self.files.len() {
                    self.selected_multi.insert(i);
                }
                if self.selected.is_none() {
                    self.selected = Some(0);
                }
            }
            if pressed_home || pressed_end {
                let len = self.files.len();
                let target = if pressed_home {
                    0
                } else {
                    len.saturating_sub(1)
                };
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            } else if pressed_pgdown || pressed_pgup {
                let len = self.files.len();
                let cur = self.selected.unwrap_or(0);
                let target = if pressed_pgdown {
                    (cur + visible_rows).min(len.saturating_sub(1))
                } else {
                    cur.saturating_sub(visible_rows)
                };
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            } else if pressed_down || pressed_up {
                let len = self.files.len();
                let cur = self.selected.unwrap_or(0);
                let target = if pressed_down {
                    (cur + 1).min(len.saturating_sub(1))
                } else {
                    cur.saturating_sub(1)
                };
                let mods = ctx.input(|i| i.modifiers);
                self.update_selection_on_click(target, mods);
                self.select_and_load(target, true);
                key_moved = true;
            }
            if pressed_enter && !self.suppress_list_enter {
                let selected = self.selected_paths();
                if !selected.is_empty() {
                    self.open_paths_in_tabs(&selected);
                }
            }
            if pressed_delete {
                let selected = self.selected_paths();
                if !selected.is_empty() {
                    self.remove_paths_from_list_with_undo(&selected);
                }
            }
            if key_moved && self.auto_play_list_nav {
                self.request_list_autoplay();
            }

            // Per-file Gain(dB) adjust: Left/Right arrows
            if pressed_left || pressed_right {
                let mods = ctx.input(|i| i.modifiers);
                let step = if mods.shift { 0.1 } else { 1.0 };
                let delta = if pressed_left { -step } else { step };
                let mut indices = self.selected_multi.clone();
                if indices.is_empty() {
                    if let Some(i) = self.selected {
                        indices.insert(i);
                    }
                }
                if !indices.is_empty() {
                    self.adjust_gain_for_indices(&indices, delta);
                }
            }
        }

        let mut sort_changed = false;
        let mut missing_paths: Vec<PathBuf> = Vec::new();
        let mut visible_first_row: Option<usize> = None;
        let mut visible_last_row: Option<usize> = None;
        let wheel_raw = ctx.input(|i| i.raw_scroll_delta);
        if pointer_over_list && wheel_raw != egui::Vec2::ZERO {
            self.last_list_scroll_at = Some(std::time::Instant::now());
        }
        let allow_auto_scroll = self.scroll_to_selected
            && (key_moved
                || self.last_list_scroll_at.map_or(true, |t| {
                    t.elapsed() > std::time::Duration::from_millis(300)
                }));
        let header_dirty = self
            .tabs
            .iter()
            .any(|t| t.dirty || t.loop_markers_dirty || t.markers_dirty)
            || self
                .edited_cache
                .values()
                .any(|c| c.dirty || c.loop_markers_dirty || c.markers_dirty)
            || self
                .items
                .iter()
                .any(|item| item.pending_gain_db.abs() > 0.0001)
            || !self.sample_rate_override.is_empty()
            || !self.bit_depth_override.is_empty();
        let mut filler_cols = 0usize;
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .auto_shrink([false, true])
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(Align::Center));
        if cols.edited {
            table = table.column(egui_extras::Column::initial(30.0).resizable(false)); // Status column
            filler_cols += 1;
        }
        if cols.cover_art {
            table = table.column(egui_extras::Column::initial(76.0).resizable(false));
            filler_cols += 1;
        }
        if cols.file {
            table = table.column(egui_extras::Column::initial(200.0).resizable(true));
            filler_cols += 1;
        }
        if cols.folder {
            table = table.column(egui_extras::Column::initial(250.0).resizable(true));
            filler_cols += 1;
        }
        if cols.transcript {
            table = table.column(egui_extras::Column::initial(280.0).resizable(true));
            filler_cols += 1;
        }
        if cols.transcript_language {
            table = table.column(egui_extras::Column::initial(56.0).resizable(true));
            filler_cols += 1;
        }
        if cols.external {
            for _ in 0..external_cols.len() {
                table = table.column(egui_extras::Column::initial(140.0).resizable(true));
                filler_cols += 1;
            }
        }
        if cols.type_badge {
            table = table.column(egui_extras::Column::initial(58.0).resizable(true));
            filler_cols += 1;
        }
        if cols.length {
            table = table.column(egui_extras::Column::initial(60.0).resizable(true));
            filler_cols += 1;
        }
        if cols.channels {
            table = table.column(egui_extras::Column::initial(40.0).resizable(true));
            filler_cols += 1;
        }
        if cols.sample_rate {
            table = table.column(egui_extras::Column::initial(70.0).resizable(true));
            filler_cols += 1;
        }
        if cols.bits {
            table = table.column(egui_extras::Column::initial(50.0).resizable(true));
            filler_cols += 1;
        }
        if cols.bit_rate {
            table = table.column(egui_extras::Column::initial(70.0).resizable(true));
            filler_cols += 1;
        }
        if cols.peak {
            table = table.column(egui_extras::Column::initial(90.0).resizable(true));
            filler_cols += 1;
        }
        if cols.lufs {
            table = table.column(egui_extras::Column::initial(90.0).resizable(true));
            filler_cols += 1;
        }
        if cols.bpm {
            table = table.column(egui_extras::Column::initial(70.0).resizable(true));
            filler_cols += 1;
        }
        if cols.created_at {
            table = table.column(egui_extras::Column::initial(120.0).resizable(true));
            filler_cols += 1;
        }
        if cols.modified_at {
            table = table.column(egui_extras::Column::initial(120.0).resizable(true));
            filler_cols += 1;
        }
        if cols.gain {
            table = table.column(egui_extras::Column::initial(80.0).resizable(true));
            filler_cols += 1;
        }
        if cols.wave {
            table = table.column(egui_extras::Column::initial(150.0).resizable(true));
            filler_cols += 1;
        }
        table = table
            .column(egui_extras::Column::remainder())
            .min_scrolled_height((avail_h - header_h).max(0.0));
        filler_cols += 1;
        if allow_auto_scroll {
            if let Some(sel) = self.selected {
                if sel < row_count {
                    table = table.scroll_to_row(sel, Some(Align::Center));
                    self.scroll_to_selected = false;
                }
            }
        }

        table
            .header(header_h, |mut header| {
                if cols.edited {
                    header.col(|ui| {
                        let mut dot = RichText::new("\u{25CF}");
                        if header_dirty {
                            dot = dot.color(Color32::from_rgb(255, 180, 60));
                        } else {
                            dot = dot.weak();
                        }
                        ui.label(dot);
                    });
                }

                if cols.cover_art {
                    header.col(|ui| {
                        ui.label(RichText::new("Art").strong());
                    });
                }
                if cols.file {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "File",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::File,
                            true,
                        );
                    });
                }
                if cols.folder {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Folder",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Folder,
                            true,
                        );
                    });
                }
                if cols.transcript {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Transcript",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Transcript,
                            true,
                        );
                    });
                }
                if cols.transcript_language {
                    header.col(|ui| {
                        ui.label(RichText::new("Lang").strong());
                    });
                }
                if cols.external {
                    for (idx, name) in external_cols.iter().enumerate() {
                        header.col(|ui| {
                            sort_changed |= sortable_header(
                                ui,
                                name,
                                &mut self.sort_key,
                                &mut self.sort_dir,
                                SortKey::External(idx),
                                true,
                            );
                        });
                    }
                }
                if cols.type_badge {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Type",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Type,
                            true,
                        );
                    });
                }
                if cols.length {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Length",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Length,
                            true,
                        );
                    });
                }
                if cols.channels {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Ch",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Channels,
                            true,
                        );
                    });
                }
                if cols.sample_rate {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "SR",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::SampleRate,
                            true,
                        );
                    });
                }
                if cols.bits {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Bits",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Bits,
                            true,
                        );
                    });
                }
                if cols.bit_rate {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Bitrate",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::BitRate,
                            true,
                        );
                    });
                }
                if cols.peak {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "dBFS (Peak)",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Level,
                            false,
                        );
                    });
                }
                if cols.lufs {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "LUFS (I)",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Lufs,
                            false,
                        );
                    });
                }
                if cols.bpm {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "BPM",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::Bpm,
                            false,
                        );
                    });
                }
                if cols.created_at {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Created",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::CreatedAt,
                            true,
                        );
                    });
                }
                if cols.modified_at {
                    header.col(|ui| {
                        sort_changed |= sortable_header(
                            ui,
                            "Modified",
                            &mut self.sort_key,
                            &mut self.sort_dir,
                            SortKey::ModifiedAt,
                            true,
                        );
                    });
                }
                if cols.gain {
                    header.col(|ui| {
                        ui.label(RichText::new("Gain (dB)").strong());
                    });
                }
                if cols.wave {
                    header.col(|ui| {
                        ui.label(RichText::new("Wave").strong());
                    });
                }
                header.col(|_ui| {});
            })
            .body(|body| {
                body.rows(row_h, row_count, |mut row| {
                    let row_idx = row.index();
                    if row_idx < self.files.len() {
                        visible_first_row = Some(visible_first_row.map_or(row_idx, |v| v.min(row_idx)));
                        visible_last_row = Some(visible_last_row.map_or(row_idx, |v| v.max(row_idx)));
                        let id = self.files[row_idx];
                        let (path_owned, file_name, parent, is_virtual) = match self.item_for_id(id) {
                            Some(item) => (
                                item.path.clone(),
                                item.display_name.clone(),
                                item.display_folder.clone(),
                                item.source == crate::app::types::MediaSource::Virtual,
                            ),
                            None => return,
                        };
                        if !is_virtual && !path_owned.is_file() {
                            missing_paths.push(path_owned.clone());
                            return;
                        }
                        let large_bg_list =
                            self.item_bg_mode != crate::app::types::ItemBgMode::Standard
                                && self.files.len() >= crate::app::LIST_BG_META_LARGE_THRESHOLD;
                        let near_selected = self
                            .selected
                            .map(|sel| sel.abs_diff(row_idx) <= 2)
                            .unwrap_or(false);
                        if !is_virtual {
                            if large_bg_list {
                                self.queue_header_meta_for_path(&path_owned, near_selected);
                                if !self.transcript_ai_inflight.contains(&path_owned) {
                                    self.queue_transcript_for_path(&path_owned, near_selected);
                                }
                            } else {
                                self.queue_meta_for_path(&path_owned, true);
                                if !self.transcript_ai_inflight.contains(&path_owned) {
                                    self.queue_transcript_for_path(&path_owned, true);
                                }
                            }
                        }
                        let Some(item) = self.item_for_id(id).cloned() else {
                            return;
                        };
                        if !is_virtual {
                            let needs_bg_full = match self.item_bg_mode {
                                crate::app::types::ItemBgMode::Standard => false,
                                crate::app::types::ItemBgMode::Dbfs => item
                                    .meta
                                    .as_ref()
                                    .and_then(|m| m.peak_db)
                                    .is_none(),
                                crate::app::types::ItemBgMode::Lufs => {
                                    if self.lufs_override.contains_key(&path_owned) {
                                        false
                                    } else {
                                        item.meta
                                            .as_ref()
                                            .and_then(|m| m.lufs_i)
                                            .is_none()
                                    }
                                }
                            };
                            let needs_wave_meta = cols.wave
                                && item
                                    .meta
                                    .as_ref()
                                    .map(|m| m.thumb.is_empty() && m.decode_error.is_none())
                                    .unwrap_or(true);
                            let needs_lufs_meta = cols.lufs
                                && !self.lufs_override.contains_key(&path_owned)
                                && item
                                    .meta
                                    .as_ref()
                                    .and_then(|m| m.lufs_i)
                                    .is_none();
                            if needs_bg_full || needs_wave_meta || needs_lufs_meta {
                                self.queue_full_meta_for_path(&path_owned, near_selected);
                            }
                        }
                        let is_selected = self.selected_multi.contains(&row_idx);
                        row.set_selected(is_selected);
                        let row_base_bg = ctx.style().visuals.faint_bg_color;
                        let row_bg = if is_selected {
                            None
                        } else {
                            match self.item_bg_mode {
                                crate::app::types::ItemBgMode::Standard => None,
                                crate::app::types::ItemBgMode::Dbfs => {
                                    let gain_db = self.pending_gain_db_for_path(&path_owned);
                                    self.meta_for_path(&path_owned)
                                        .and_then(|m| m.peak_db)
                                        .map(|v| db_to_color(v + gain_db))
                                }
                                crate::app::types::ItemBgMode::Lufs => {
                                    let base =
                                        self.meta_for_path(&path_owned).and_then(|m| m.lufs_i);
                                    let gain_db = self.pending_gain_db_for_path(&path_owned);
                                    let eff = if let Some(v) = self.lufs_override.get(&path_owned) {
                                        Some(*v)
                                    } else {
                                        base.map(|v| v + gain_db)
                                    };
                                    eff.map(db_to_color)
                                }
                            }
                            .map(|c| crate::app::helpers::lerp_color(row_base_bg, c, 0.16))
                        };
                        let row_fg = row_bg.map(|bg| {
                            let luma = (0.2126 * bg.r() as f32
                                + 0.7152 * bg.g() as f32
                                + 0.0722 * bg.b() as f32)
                                / 255.0;
                            if luma > 0.62 {
                                Color32::from_rgb(18, 22, 28)
                            } else {
                                Color32::from_rgb(230, 235, 242)
                            }
                        });
                        let mut clicked_to_load = false;
                        let mut clicked_to_select = false;
                        let is_dirty = self.has_edits_for_path(&path_owned);
                        if cols.edited {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                if is_dirty {
                                    ui.label(
                                        RichText::new("\u{25CF}")
                                            .color(Color32::from_rgb(255, 180, 60))
                                            .size(text_height * 1.05),
                                    );
                                }
                            });
                        }
                        if cols.cover_art {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let art = item.meta.as_ref().and_then(|meta| meta.cover_art.clone());
                                let (label, tooltip, fill, stroke) = Self::list_type_badge_for_item(&item);
                                let (rect2, resp2) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::click(),
                                );
                                let tile_side = (rect2.height() - 4.0).clamp(28.0, 56.0);
                                let tile_rect = egui::Rect::from_center_size(
                                    rect2.center(),
                                    egui::vec2(tile_side, tile_side),
                                );
                                if let Some(art) = art {
                                    let texture =
                                        self.list_art_texture_for_path(ctx, &path_owned, art);
                                    let mut tex_size = texture.size_vec2();
                                    tex_size.x = tex_size.x.max(1.0);
                                    tex_size.y = tex_size.y.max(1.0);
                                    let scale =
                                        (tile_rect.width() / tex_size.x).min(tile_rect.height() / tex_size.y);
                                    let draw_rect = egui::Rect::from_center_size(
                                        tile_rect.center(),
                                        tex_size * scale,
                                    );
                                    ui.painter().image(
                                        texture.id(),
                                        draw_rect,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        Color32::WHITE,
                                    );
                                } else {
                                    let badge_rect = egui::Rect::from_center_size(
                                        rect2.center(),
                                        egui::vec2(
                                            (rect2.width() - 8.0).clamp(28.0, 50.0),
                                            (rect2.height() - 6.0).clamp(16.0, 24.0),
                                        ),
                                    );
                                    Self::paint_list_type_badge(
                                        ui,
                                        badge_rect,
                                        text_height,
                                        &label,
                                        fill,
                                        stroke,
                                    );
                                }
                                let resp2 = self
                                    .attach_row_context_menu(resp2, row_idx, ctx)
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text(if item.meta.as_ref().and_then(|m| m.cover_art.as_ref()).is_some() {
                                        "Embedded artwork".to_string()
                                    } else {
                                        tooltip
                                    });
                                if resp2.double_clicked()
                                    && item
                                        .meta
                                        .as_ref()
                                        .and_then(|m| m.cover_art.as_ref())
                                        .is_some()
                                {
                                    self.open_list_art_window(ctx, &path_owned);
                                } else if resp2.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.file {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let cell_resp = self.attach_row_context_menu(
                                    ui.interact(
                                        ui.max_rect(),
                                        ui.id().with(("list_cell_file", row_idx)),
                                        Sense::click(),
                                    ),
                                    row_idx,
                                    ctx,
                                );
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        let display = file_name.clone();
                                        let label_resp = ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(display)
                                                        .monospace()
                                                        .size(text_height * 1.0),
                                                )
                                                .sense(Sense::click())
                                                .truncate()
                                                .show_tooltip_when_elided(false),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                                        let label_resp =
                                            self.attach_row_context_menu(label_resp, row_idx, ctx);
                                        if (cell_resp.clicked_by(egui::PointerButton::Primary)
                                            || label_resp.clicked_by(egui::PointerButton::Primary))
                                            && !(cell_resp.double_clicked()
                                                || label_resp.double_clicked())
                                        {
                                            clicked_to_load = true;
                                        }
                                        if cell_resp.double_clicked() || label_resp.double_clicked() {
                                            clicked_to_select = true;
                                            to_open = Some(path_owned.clone());
                                        }
                                        if label_resp.hovered() {
                                            label_resp.on_hover_text(&file_name);
                                        }
                                    },
                                );
                            });
                        }
                        if cols.folder {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let cell_resp = self.attach_row_context_menu(
                                    ui.interact(
                                        ui.max_rect(),
                                        ui.id().with(("list_cell_folder", row_idx)),
                                        Sense::click(),
                                    ),
                                    row_idx,
                                    ctx,
                                );
                                ui.with_layout(
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        let label_resp = ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(parent.as_str())
                                                        .monospace()
                                                        .size(text_height * 1.0),
                                                )
                                                .sense(Sense::click())
                                                .truncate()
                                                .show_tooltip_when_elided(false),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                                        let label_resp =
                                            self.attach_row_context_menu(label_resp, row_idx, ctx);
                                        if (cell_resp.clicked_by(egui::PointerButton::Primary)
                                            || label_resp.clicked_by(egui::PointerButton::Primary))
                                            && !(cell_resp.double_clicked()
                                                || label_resp.double_clicked())
                                        {
                                            clicked_to_load = true;
                                        }
                                        if cell_resp.double_clicked() || label_resp.double_clicked() {
                                            clicked_to_select = true;
                                            if !is_virtual {
                                                let _ = crate::app::helpers::open_folder_with_file_selected(
                                                    &path_owned,
                                                );
                                            }
                                        }
                                        if label_resp.hovered() {
                                            label_resp.on_hover_text(&parent);
                                        }
                                    },
                                );
                            });
                        }
                        if cols.transcript {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let cell_resp = self.attach_row_context_menu(
                                    ui.interact(
                                        ui.max_rect(),
                                        ui.id().with(("list_cell_transcript", row_idx)),
                                        Sense::click(),
                                    ),
                                    row_idx,
                                    ctx,
                                );
                                let transcript_text = item
                                    .transcript
                                    .as_ref()
                                    .map(|t| t.full_text.as_str())
                                    .unwrap_or("");
                                let inflight = self.transcript_ai_inflight.contains(&path_owned);
                                let queued = self
                                    .transcript_ai_state
                                    .as_ref()
                                    .map(|s| s.pending.contains(&path_owned))
                                    .unwrap_or(false);
                                let display = if transcript_text.is_empty() {
                                    if inflight {
                                        "[Transcribing...]"
                                    } else if queued {
                                        "[Queued...]"
                                    } else {
                                        ""
                                    }
                                } else {
                                    transcript_text
                                };
                                let label = if let Some(job) = highlight_text_job(
                                    display,
                                    &self.search_query,
                                    self.search_use_regex,
                                    ui.style(),
                                ) {
                                    egui::Label::new(job).sense(Sense::click()).truncate()
                                } else {
                                    egui::Label::new(
                                        RichText::new(display).size(text_height * 0.95),
                                    )
                                    .sense(Sense::click())
                                    .truncate()
                                };
                                let label_resp = ui
                                    .add(label.show_tooltip_when_elided(false))
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                let label_resp =
                                    self.attach_row_context_menu(label_resp, row_idx, ctx);
                                if (cell_resp.clicked_by(egui::PointerButton::Primary)
                                    || label_resp.clicked_by(egui::PointerButton::Primary))
                                    && !(cell_resp.double_clicked()
                                        || label_resp.double_clicked())
                                {
                                    clicked_to_load = true;
                                }
                                if label_resp.hovered() && !transcript_text.is_empty() {
                                    label_resp.on_hover_text(transcript_text);
                                }
                            });
                        }
                        if cols.transcript_language {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let lang = item
                                    .transcript_language
                                    .as_deref()
                                    .filter(|v| !v.is_empty())
                                    .unwrap_or("-");
                                ui.label(
                                    RichText::new(lang)
                                        .monospace()
                                        .size(text_height * 0.98),
                                );
                            });
                        }
                        if cols.external {
                            for name in external_cols.iter() {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let cell_resp = self.attach_row_context_menu(
                                        ui.interact(
                                            ui.max_rect(),
                                            ui.id().with(("list_cell_external", row_idx, name)),
                                            Sense::click(),
                                        ),
                                        row_idx,
                                        ctx,
                                    );
                                    let value = item
                                        .external
                                        .get(name)
                                        .map(|v| v.as_str())
                                        .unwrap_or("");
                                    let label_resp = ui
                                        .add(
                                            egui::Label::new(
                                                RichText::new(value).size(text_height * 0.95),
                                            )
                                            .sense(Sense::click())
                                            .truncate()
                                            .show_tooltip_when_elided(false),
                                        )
                                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                                    let label_resp =
                                        self.attach_row_context_menu(label_resp, row_idx, ctx);
                                    if (cell_resp.clicked_by(egui::PointerButton::Primary)
                                        || label_resp.clicked_by(egui::PointerButton::Primary))
                                        && !(cell_resp.double_clicked()
                                            || label_resp.double_clicked())
                                    {
                                        clicked_to_load = true;
                                    }
                                    if label_resp.hovered() && !value.is_empty() {
                                        label_resp.on_hover_text(value);
                                    }
                                });
                            }
                        }
                        if cols.type_badge {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let (label, tooltip, fill, stroke) =
                                    Self::list_type_badge_for_item(&item);
                                let (rect2, resp2) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::click(),
                                );
                                let badge_rect = egui::Rect::from_center_size(
                                    rect2.center(),
                                    egui::vec2(
                                        (rect2.width() - 8.0).clamp(28.0, 50.0),
                                        (rect2.height() - 6.0).clamp(16.0, 24.0),
                                    ),
                                );
                                Self::paint_list_type_badge(
                                    ui,
                                    badge_rect,
                                    text_height,
                                    &label,
                                    fill,
                                    stroke,
                                );
                                let resp2 = self
                                    .attach_row_context_menu(resp2, row_idx, ctx)
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text(tooltip);
                                if resp2.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.length {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let secs = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.duration_secs)
                                    .unwrap_or(f32::NAN);
                                let text = if secs.is_finite() {
                                    format_duration(secs)
                                } else {
                                    "...".into()
                                };
                                let resp = ui
                                    .add(
                                        egui::Label::new(RichText::new(text).monospace())
                                            .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.channels {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let ch = self
                                    .meta_for_path(&path_owned)
                                    .map(|m| m.channels)
                                    .filter(|v| *v > 0);
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(
                                                ch.map(|v| format!("{v}"))
                                                    .unwrap_or_else(|| "-".into()),
                                            )
                                            .monospace(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.sample_rate {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let sr = self.effective_sample_rate_for_path(&path_owned);
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(
                                                sr.map(|v| format!("{v}"))
                                                    .unwrap_or_else(|| "-".into()),
                                            )
                                            .monospace(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.bits {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let bits = self.effective_bits_label_for_path(&path_owned);
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(
                                                bits
                                                    .unwrap_or_else(|| "-".into()),
                                            )
                                            .monospace(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.bit_rate {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let br = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.bit_rate_bps)
                                    .filter(|v| *v > 0);
                                let text = br
                                    .map(|v| format!("{:.0}k", (v as f32) / 1000.0))
                                    .unwrap_or_else(|| "-".into());
                                let resp = ui
                                    .add(
                                        egui::Label::new(RichText::new(text).monospace())
                                            .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.peak {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let (rect2, resp2) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::click(),
                                );
                                let gain_db = self.pending_gain_db_for_path(&path_owned);
                                let orig = self.meta_for_path(&path_owned).and_then(|m| m.peak_db);
                                let adj = orig.map(|db| db + gain_db);
                                if let Some(db) = adj {
                                    ui.painter().rect_filled(rect2, 4.0, db_to_color(db));
                                }
                                let text = adj
                                    .map(|db| format!("{:.1}", db))
                                    .unwrap_or_else(|| "...".into());
                                let fid = egui::TextStyle::Monospace.resolve(ui.style());
                                ui.painter().text(
                                    rect2.center(),
                                    egui::Align2::CENTER_CENTER,
                                    text,
                                    fid,
                                    egui::Color32::WHITE,
                                );
                                let resp2 = self.attach_row_context_menu(resp2, row_idx, ctx);
                                if resp2.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.lufs {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let base = self.meta_for_path(&path_owned).and_then(|m| m.lufs_i);
                                let gain_db = self.pending_gain_db_for_path(&path_owned);
                                let eff = if let Some(v) = self.lufs_override.get(&path_owned) {
                                    Some(*v)
                                } else {
                                    base.map(|v| v + gain_db)
                                };
                                let (rect2, resp2) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::click(),
                                );
                                if let Some(db) = eff {
                                    ui.painter().rect_filled(rect2, 4.0, db_to_color(db));
                                }
                                let text = eff
                                    .map(|v| format!("{:.1}", v))
                                    .unwrap_or_else(|| "...".into());
                                let fid = egui::TextStyle::Monospace.resolve(ui.style());
                                ui.painter().text(
                                    rect2.center(),
                                    egui::Align2::CENTER_CENTER,
                                    text,
                                    fid,
                                    egui::Color32::WHITE,
                                );
                                let resp2 = self.attach_row_context_menu(resp2, row_idx, ctx);
                                if resp2.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.bpm {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let bpm = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.bpm)
                                    .filter(|v| v.is_finite() && *v > 0.0);
                                let resp = ui
                                    .add(
                                        egui::Label::new(
                                            RichText::new(
                                                bpm.map(|v| format!("{:.2}", v))
                                                    .unwrap_or_else(|| "-".into()),
                                            )
                                            .monospace(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.created_at {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let text = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.created_at)
                                    .map(format_system_time_local)
                                    .unwrap_or_else(|| "-".into());
                                let resp = ui
                                    .add(
                                        egui::Label::new(RichText::new(text).monospace())
                                            .sense(Sense::click())
                                            .truncate(),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.modified_at {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let text = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.modified_at)
                                    .map(format_system_time_local)
                                    .unwrap_or_else(|| "-".into());
                                let resp = ui
                                    .add(
                                        egui::Label::new(RichText::new(text).monospace())
                                            .sense(Sense::click())
                                            .truncate(),
                                    )
                                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                                let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                if resp.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        if cols.gain {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let old = self.pending_gain_db_for_path(&path_owned);
                                let mut g = old;
                                let resp = ui.add(
                                    egui::DragValue::new(&mut g)
                                        .range(-24.0..=24.0)
                                        .speed(0.1)
                                        .fixed_decimals(1)
                                        .suffix(" dB"),
                                );
                                let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                if resp.changed() {
                                    let new = crate::app::WavesPreviewer::clamp_gain_db(g);
                                    let delta = new - old;
                                    if self.selected_multi.len() > 1
                                        && self.selected_multi.contains(&row_idx)
                                    {
                                        let indices = self.selected_multi.clone();
                                        self.adjust_gain_for_indices(&indices, delta);
                                    } else {
                                        let path_list = vec![path_owned.clone()];
                                        let before = self.capture_list_selection_snapshot();
                                        let before_items =
                                            self.capture_list_undo_items_by_paths(&path_list);
                                        self.set_pending_gain_db_for_path(&path_owned, new);
                                        if self.playing_path.as_ref() == Some(&path_owned) {
                                            self.apply_effective_volume();
                                        }
                                        self.schedule_lufs_for_path(path_owned.clone());
                                        self.record_list_update_from_paths(
                                            &path_list,
                                            before_items,
                                            before,
                                        );
                                    }
                                }
                            });
                        }
                        if cols.wave {
                            row.col(|ui| {
                                if let Some(bg) = row_bg {
                                    ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                }
                                ui.visuals_mut().override_text_color = row_fg;
                                let (rect2, resp2) = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::click(),
                                );
                                let error_text = self
                                    .meta_for_path(&path_owned)
                                    .and_then(|m| m.decode_error.as_deref());
                                let (wave_rect, error_rect) = if error_text.is_some() {
                                    let err_max = (rect2.height() * 0.45).max(8.0);
                                    let mut err_h = (row_h * 0.36).max(8.0);
                                    if err_h > err_max {
                                        err_h = err_max;
                                    }
                                    let wave_h = (rect2.height() - err_h).max(1.0);
                                    let wave_rect = egui::Rect::from_min_size(
                                        rect2.min,
                                        egui::vec2(rect2.width(), wave_h),
                                    );
                                    let error_rect = egui::Rect::from_min_size(
                                        egui::pos2(rect2.min.x, rect2.max.y - err_h),
                                        egui::vec2(rect2.width(), err_h),
                                    );
                                    (wave_rect, Some(error_rect))
                                } else {
                                    (rect2, None)
                                };
                                if let Some(m) = self.meta_for_path(&path_owned) {
                                    let w = wave_rect.width();
                                    let h = wave_rect.height();
                                    let n = m.thumb.len().max(1) as f32;
                                    let gain_db = self.pending_gain_db_for_path(&path_owned);
                                    let scale = db_to_amp(gain_db);
                                    for (idx, &(mn0, mx0)) in m.thumb.iter().enumerate() {
                                        let mn = (mn0 * scale).clamp(-1.0, 1.0);
                                        let mx = (mx0 * scale).clamp(-1.0, 1.0);
                                        let x = wave_rect.left() + (idx as f32 / n) * w;
                                        let y0 = wave_rect.center().y - mx * (h * 0.45);
                                        let y1 = wave_rect.center().y - mn * (h * 0.45);
                                        let a = (mn.abs().max(mx.abs())).clamp(0.0, 1.0);
                                        let col = amp_to_color(a);
                                        ui.painter().line_segment(
                                            [egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))],
                                            egui::Stroke::new(1.0, col),
                                        );
                                    }
                                }
                                if let (Some(text), Some(err_rect)) = (error_text, error_rect) {
                                    let text_pos =
                                        egui::pos2(err_rect.left() + 4.0, err_rect.center().y);
                                    let mut font_size = text_height * 0.85;
                                    if font_size < 10.0 {
                                        font_size = 10.0;
                                    }
                                    if font_size > err_rect.height() {
                                        font_size = err_rect.height();
                                    }
                                    let font = egui::FontId::proportional(font_size);
                                    ui.painter().text(
                                        text_pos,
                                        egui::Align2::LEFT_CENTER,
                                        text,
                                        font,
                                        egui::Color32::from_rgb(220, 90, 90),
                                    );
                                }
                                let resp2 = self.attach_row_context_menu(resp2, row_idx, ctx);
                                if resp2.clicked_by(egui::PointerButton::Primary) {
                                    clicked_to_load = true;
                                }
                            });
                        }
                        row.col(|ui| {
                            if let Some(bg) = row_bg {
                                ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                            }
                        });
                        // row-level interaction (must call response() after at least one col())
                        let resp = self.attach_row_context_menu(row.response(), row_idx, ctx);
                        let clicked_any = (resp.clicked_by(egui::PointerButton::Primary)
                            && !resp.double_clicked())
                            || clicked_to_load;
                        if clicked_to_select {
                            self.selected = Some(row_idx);
                            self.scroll_to_selected = false;
                            self.selected_multi.clear();
                            self.selected_multi.insert(row_idx);
                            self.select_anchor = Some(row_idx);
                            ctx.memory_mut(|m| m.request_focus(list_focus_id));
                            list_has_focus = true;
                            self.search_has_focus = false;
                        } else if clicked_any {
                            let mods = ctx.input(|i| i.modifiers);
                            self.update_selection_on_click(row_idx, mods);
                            self.select_and_load(row_idx, false);
                            if self.auto_play_list_nav {
                                self.request_list_autoplay();
                            }
                            ctx.memory_mut(|m| m.request_focus(list_focus_id));
                            list_has_focus = true;
                            self.search_has_focus = false;
                        }
                    } else {
                        // filler
                        for _ in 0..filler_cols {
                            row.col(|ui| {
                                let _ = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::hover(),
                                );
                            });
                        }
                    }
                });
            });

        if self.item_bg_mode != crate::app::types::ItemBgMode::Standard && !self.files.is_empty() {
            let start = visible_first_row
                .or(self.selected)
                .unwrap_or(0)
                .min(self.files.len() - 1);
            let end = visible_last_row.unwrap_or(start).min(self.files.len() - 1);
            // Keep UI pass light; broad prefetch is handled by pump_list_meta_prefetch().
            let look_back = 8usize;
            let look_ahead = if self.files.len() >= crate::app::LIST_BG_META_LARGE_THRESHOLD {
                16usize
            } else {
                48usize
            };
            let prefetch_start = start.saturating_sub(look_back);
            let prefetch_end = (end + look_ahead).min(self.files.len() - 1);
            for idx in prefetch_start..=prefetch_end {
                let Some(path) = self.path_for_row(idx).cloned() else {
                    continue;
                };
                if self.is_virtual_path(&path) {
                    continue;
                }
                if self.files.len() >= crate::app::LIST_BG_META_LARGE_THRESHOLD {
                    self.queue_header_meta_for_path(&path, false);
                } else {
                    self.queue_meta_for_path(&path, false);
                }
            }
        }
        self.queue_list_preview_prefetch_for_rows(visible_first_row, visible_last_row);

        if !missing_paths.is_empty() {
            for p in missing_paths {
                self.remove_missing_path(&p);
            }
        }
        if sort_changed {
            self.list_meta_prefetch_cursor = 0;
            self.prime_sort_metadata_prefetch();
            self.apply_sort();
        }
        if let Some(p) = to_open.as_ref() {
            self.open_or_activate_tab(p);
        }
        self.list_has_focus = list_has_focus;

        // keyboard handling moved above table to allow same-frame auto-scroll
    }
}
