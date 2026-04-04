mod art;
mod badges;
mod navigation;
mod row_menu; mod table;
use egui::{Color32, RichText, Sense};
use std::path::PathBuf;
pub(super) struct ListInteractionState {
    pub(super) key_moved: bool,
    pub(super) list_focus_id: egui::Id,
    pub(super) list_has_focus: bool,
}
#[derive(Default)]
pub(super) struct ListRenderState {
    pub(super) missing_paths: Vec<PathBuf>,
    pub(super) sort_changed: bool,
    pub(super) to_open: Option<PathBuf>,
    pub(super) visible_first_row: Option<usize>,
    pub(super) visible_last_row: Option<usize>,
}
#[derive(Clone)]
pub(super) struct ListViewMetrics {
    pub(super) avail_h: f32,
    pub(super) external_cols: Vec<String>,
    pub(super) header_h: f32,
    pub(super) list_rect: egui::Rect,
    pub(super) pointer_over_list: bool,
    pub(super) row_count: usize,
    pub(super) row_h: f32,
    pub(super) text_height: f32,
    pub(super) visible_rows: usize,
}
impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_list_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use crate::app::helpers::{
            amp_to_color, db_to_amp, db_to_color, format_duration, format_system_time_local,
            highlight_text_job,
        };
        let cols = self.list_columns;
        let metrics = self.list_view_metrics(ui);
        let text_height = metrics.text_height;
        let row_h = metrics.row_h;
        let row_count = metrics.row_count;
        let external_cols = &metrics.external_cols;
        let mut interaction = self.handle_list_focus_and_keyboard(ui, ctx, &metrics);
        let list_focus_id = interaction.list_focus_id;
        let mut list_has_focus = interaction.list_has_focus;
        let key_moved = interaction.key_moved;

        let mut sort_changed = false;
        let mut missing_paths: Vec<PathBuf> = Vec::new();
        let mut to_open: Option<PathBuf> = None;
        let mut visible_first_row: Option<usize> = None;
        let mut visible_last_row: Option<usize> = None;
        let allow_auto_scroll = self.list_allow_auto_scroll(ctx, &metrics, key_moved);
        let (table, filler_cols, header_dirty) =
            self.build_list_table(ui, &metrics, allow_auto_scroll);

        table
            .header(metrics.header_h, |mut header| {
                self.render_list_header(&mut header, &metrics, header_dirty, &mut sort_changed);
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

        interaction.list_has_focus = list_has_focus;
        self.finish_list_view(
            ListRenderState {
                missing_paths,
                sort_changed,
                to_open,
                visible_first_row,
                visible_last_row,
            },
            interaction,
        );
    }
}
