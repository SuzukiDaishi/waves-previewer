mod art;
mod badges;
mod comment_cell;
mod label_cell;
mod navigation;
mod row_menu;
mod table;
mod wave_cell;

use egui::{Color32, RichText, Sense};
use std::path::PathBuf;
pub(super) struct ListInteractionState {
    pub(super) key_moved: bool,
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
    /// `header_h` plus the one item spacing the header strip adds below
    /// itself. The body starts here, not at `header_h`.
    pub(super) header_pitch: f32,
    pub(super) list_rect: egui::Rect,
    pub(super) pointer_over_list: bool,
    pub(super) row_count: usize,
    pub(super) row_h: f32,
    /// Vertical distance between consecutive row tops, i.e. what
    /// `egui_extras::TableBody::rows` actually advances by. Row *height*
    /// alone is not it: the table adds `item_spacing.y` between rows.
    pub(super) row_pitch: f32,
    pub(super) text_height: f32,
    pub(super) visible_rows: usize,
}

impl crate::app::WavesPreviewer {
    /// First-run empty state: no folder open and nothing loaded. Shows a
    /// centered onboarding panel instead of an empty table. Returns true
    /// when the panel was rendered (the table is skipped).
    fn ui_list_empty_state(&mut self, ui: &mut egui::Ui) -> bool {
        if self.root.is_some() || !self.items.is_empty() {
            return false;
        }
        let mut open_folder = false;
        let mut open_session: Option<PathBuf> = None;
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading("NeoWaves");
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Open a folder of audio files, or drop files / folders anywhere in this window.",
                )
                .weak(),
            );
            ui.add_space(12.0);
            if ui.button("Open Folder...").clicked() {
                open_folder = true;
            }
            let recents = self.recent_session_paths_for_menu();
            if !recents.is_empty() {
                ui.add_space(16.0);
                ui.label(egui::RichText::new("Recent sessions").strong());
                for path in recents.iter().take(5) {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("session.nwsess");
                    if ui
                        .link(name)
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        open_session = Some(path.clone());
                    }
                }
            }
        });
        if open_folder {
            if let Some(dir) = self.pick_folder_dialog() {
                self.root = Some(dir);
                self.rescan();
            }
        }
        if let Some(path) = open_session {
            self.queue_project_open(path);
        }
        true
    }

    pub(in crate::app) fn ui_list_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use crate::app::helpers::{
            db_to_color, format_duration_scaled, format_system_time_local,
            highlight_text_job_with_regex,
        };
        use crate::app::list_state_ops::QaStatus;
        self.list_last_fully_visible_row = None;
        if self.ui_list_empty_state(ui) {
            return;
        }
        let cols = self.list_columns;
        // Hoisted out of the row loop: read once per frame, and needed inside
        // the closure that borrows self mutably.
        let uses_hours = self.list_length_uses_hours();
        // One decision per frame, not per row: it cannot vary between rows, and
        // reading `files.len()` forty times a frame buys nothing.
        let meta_detail = self.list_meta_detail_now();
        // Resolved once: only the sounding row draws a playhead, so the rest of
        // the loop pays one path comparison.
        let playhead_frame = self.resolve_list_playhead_frame();
        let meta_wants_thumb = meta_detail == crate::app::meta_ops::ListMetaDetail::Thumb;
        let blank_threshold_dbfs = self.blank_threshold_dbfs;
        // Compile the search highlight regex once per frame instead of per row.
        let highlight_re = self.cached_highlight_regex();
        let metrics = self.list_view_metrics(ui);
        let text_height = metrics.text_height;
        let row_h = metrics.row_h;
        let row_count = metrics.row_count;
        let external_cols = &metrics.external_cols;
        let mut interaction = self.handle_list_focus_and_keyboard(ui, ctx, &metrics);
        let mut list_has_focus = interaction.list_has_focus;
        let key_moved = interaction.key_moved;

        let mut sort_changed = false;
        let mut missing_paths: Vec<PathBuf> = Vec::new();
        let mut to_open: Option<PathBuf> = None;
        let mut visible_first_row: Option<usize> = None;
        let mut visible_last_row: Option<usize> = None;
        // Applied after the table closes: `select_and_load` can remove a
        // missing row, which would mutate `items` mid-iteration.
        let mut wave_seek_request: Option<crate::app::list_seek_ops::ListSeekRequest> = None;
        // Rows the table lays out past the bottom of the viewport are clipped,
        // not scrollable into view (the table is built with `vscroll(false)`).
        // Tracking the last row that actually fitted lets the scroll clamp be
        // verified against pixels instead of index arithmetic.
        let list_bottom = metrics.list_rect.bottom();
        let mut last_fully_visible_row: Option<usize> = None;
        let mut end_row_fully_visible = false;
        let end_marker = table::format_list_end_marker(
            self.files.len(),
            self.items.len(),
            !self.search_query.trim().is_empty(),
        );
        let mut end_row_rect: Option<egui::Rect> = None;
        let allow_auto_scroll = self.list_allow_auto_scroll(ctx, &metrics, key_moved);
        self.update_list_scroll_state(ctx, &metrics, allow_auto_scroll);
        let (table, filler_cols, header_dirty) = self.build_list_table(ui, &metrics);
        // One copy per frame; the per-row closure below borrows self mutably,
        // and the order cannot change mid-frame.
        let column_layout = self.list_column_layout.clone();
        let metadata_columns = self
            .visible_metadata_columns()
            .map(|(index, column)| (index, column.clone()))
            .collect::<Vec<_>>();

        table
            .header(metrics.header_h, |mut header| {
                self.render_list_header(&mut header, &metrics, header_dirty, &mut sort_changed);
            })
            .body(|body| {
                body.rows(row_h, row_count, |mut row| {
                    // The table only ever renders the visible window; map the
                    // window-local index back to the absolute row.
                    let row_idx = self.list_scroll_row + row.index();
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
                        if !is_virtual && !self.path_is_file_cached(&path_owned) {
                            missing_paths.push(path_owned.clone());
                            return;
                        }
                        let near_selected = self
                            .selected
                            .map(|sel| sel.abs_diff(row_idx) <= 2)
                            .unwrap_or(false);
                        if !is_virtual {
                            let priority = meta_wants_thumb || near_selected;
                            self.queue_list_meta_for_path(&path_owned, priority);
                            if !self.transcript_ai_inflight.contains(&path_owned) {
                                self.queue_transcript_for_path(&path_owned, priority);
                            }
                            if !metadata_columns.is_empty() {
                                self.queue_metadata_summary_for_path(
                                    &path_owned,
                                    if near_selected {
                                        crate::metadata::cache::SummaryPriority::Selected
                                    } else {
                                        crate::metadata::cache::SummaryPriority::Visible
                                    },
                                );
                            }
                        }
                        // Borrow the item once and extract only what the row
                        // needs. Cloning the whole MediaItem (strings, external
                        // map, inline FileMeta with a ~1 KB thumb) for every
                        // visible row every frame was a steady allocator load.
                        let (
                            needs_bg_full,
                            needs_wave_meta,
                            needs_lufs_meta,
                            cover_art,
                            badge,
                            row_transcript,
                            row_transcript_language,
                        ) = {
                            let Some(item) = self.item_for_id(id) else {
                                return;
                            };
                            let needs_bg_full = match self.item_bg_mode {
                                crate::app::types::ItemBgMode::Standard => false,
                                crate::app::types::ItemBgMode::Dbfs => {
                                    item.meta.as_ref().and_then(|m| m.peak_db).is_none()
                                }
                                crate::app::types::ItemBgMode::Lufs => {
                                    if self.lufs_override.contains_key(&path_owned) {
                                        false
                                    } else {
                                        item.meta.as_ref().and_then(|m| m.lufs_i).is_none()
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
                                && item.meta.as_ref().and_then(|m| m.lufs_i).is_none();
                            let decode_ok = item
                                .meta
                                .as_ref()
                                .map(|m| m.decode_error.is_none())
                                .unwrap_or(true);
                            let needs_loudness_extra = decode_ok
                                && ((cols.dbtp
                                && item.meta.as_ref().and_then(|m| m.true_peak_db).is_none())
                                || (cols.lufs_s
                                    && item.meta.as_ref().and_then(|m| m.lufs_s_max).is_none())
                                || (cols.lufs_m
                                    && item
                                        .meta
                                        .as_ref()
                                        .and_then(|m| m.lufs_m_max)
                                        .is_none())
                                || ((cols.silence_lead || cols.silence_tail)
                                    && item
                                        .meta
                                        .as_ref()
                                        .and_then(|m| m.silence_lead_ms)
                                        .is_none())
                                || (cols.edge_zero
                                    && item.meta.as_ref().and_then(|m| m.edge_abs).is_none())
                                // The header pass only estimates the peak off a
                                // 0.25 s prefix, which is not enough to call a
                                // file over 0 dBFS either way.
                                || (cols.over_peak
                                    && item.meta.as_ref().map_or(true, |m| m.peak_db_estimate))
                                // A measurement taken at a threshold the user
                                // has since changed is stale, not missing.
                                || (cols.blank_pad
                                    && item.meta.as_ref().map_or(true, |m| {
                                        m.blank_pad.map_or(true, |b| {
                                            b.threshold_dbfs != blank_threshold_dbfs
                                        })
                                    })));
                            (
                                needs_bg_full,
                                needs_wave_meta,
                                needs_lufs_meta || needs_loudness_extra,
                                item.meta.as_ref().and_then(|m| m.cover_art.clone()),
                                Self::list_type_badge_for_item(item),
                                item.transcript.clone(),
                                item.transcript_language.clone(),
                            )
                        };
                        // A full decode is what produces the row waveform, the
                        // dBFS/LUFS row tint and the loudness columns -- and it
                        // is the most expensive thing a visible row can ask for.
                        // It waits until the scan has finished listing every
                        // file; the thumbs then fill in for the rows on screen,
                        // because `queue_full_meta_for_path` (unlike
                        // `queue_meta_for_path`) re-queues a row that already
                        // has header-only metadata.
                        if !is_virtual
                            && meta_wants_thumb
                            && (needs_bg_full || needs_wave_meta || needs_lufs_meta)
                        {
                            self.queue_full_meta_for_path(&path_owned, near_selected);
                        }
                        let is_selected = self.selected_multi.contains(&row_idx);
                        row.set_selected(is_selected);
                        let row_base_bg = ctx.global_style().visuals.faint_bg_color;
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
                        let mut interacted_with_control = false;
                        let mut control_focus_id = None;
                        let mut clicked_to_select = false;
                        let is_dirty = self.has_edits_for_path(&path_owned);
                        for column_key in &column_layout {
                        use crate::app::types::{ColumnId as C, ColumnKey};
                        let ColumnKey::Builtin(sorted_col) = column_key else {
                            if let Some((_, column)) = metadata_columns
                                .iter()
                                .find(|(_, column)| &column.key == column_key)
                            {
                                let cell = self.metadata_cell_for_path(&path_owned, &column.key);
                                let error = self.metadata_summary_errors.get(&path_owned).cloned();
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let text = cell.as_ref().map(|cell| cell.text.as_str()).unwrap_or_else(|| if error.is_some() { "!" } else { "..." });
                                    let mut rich = RichText::new(text).monospace();
                                    if cell.as_ref().is_some_and(|cell| cell.conflict) {
                                        rich = rich.color(self.palette().warning_text);
                                    } else if cell.as_ref().is_some_and(|cell| cell.partial) {
                                        rich = rich.weak();
                                    }
                                    let response = ui
                                        .add(egui::Label::new(rich).sense(Sense::click()).truncate())
                                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                                    let response = if let Some(cell) = &cell {
                                        if cell.tooltip.is_empty() { response } else { response.on_hover_text(&cell.tooltip) }
                                    } else if let Some(error) = &error {
                                        response.on_hover_text(error)
                                    } else {
                                        response
                                    };
                                    let response = self.attach_row_context_menu(response, row_idx, ctx);
                                    if response.clicked_by(egui::PointerButton::Primary) {
                                        clicked_to_load = true;
                                    }
                                });
                            }
                            continue;
                        };
                        let sorted_col = *sorted_col;
                        if !sorted_col.enabled(&cols) {
                            continue;
                        }
                        match sorted_col {
                            C::Edited => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    if is_dirty {
                                        ui.label(
                                            RichText::new("\u{25CF}")
                                                .color(self.palette().warning_text)
                                                .size(text_height * 1.05),
                                        );
                                    }
                                });
                            }
                            C::CoverArt => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let art = cover_art.clone();
                                    let (label, tooltip, fill, stroke) = badge.clone();
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
                                        .on_hover_text(if cover_art.is_some() {
                                            "Embedded artwork".to_string()
                                        } else {
                                            tooltip
                                        });
                                    if resp2.double_clicked() && cover_art.is_some() {
                                        self.open_list_art_window(ctx, &path_owned);
                                    } else if resp2.clicked_by(egui::PointerButton::Primary) {
                                        clicked_to_load = true;
                                    }
                                });
                            }
                            C::File => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    if self.inline_rename_path.as_deref()
                                        == Some(path_owned.as_path())
                                    {
                                        self.ui_inline_rename_cell(ui, &path_owned);
                                        return;
                                    }
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
                            C::Folder => {
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
                                                        RichText::new(parent.as_ref())
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
                                                label_resp.on_hover_text(parent.as_ref());
                                            }
                                        },
                                    );
                                });
                            }
                            C::Transcript => {
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
                                    let transcript_text = row_transcript
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
                                    let label = if let Some(job) = highlight_re
                                        .as_ref()
                                        .and_then(|re| highlight_text_job_with_regex(display, re, ui.style()))
                                    {
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
                            C::TranscriptLanguage => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let lang = row_transcript_language
                                        .as_deref()
                                        .filter(|v: &&str| !v.is_empty())
                                        .unwrap_or("-");
                                    ui.label(
                                        RichText::new(lang)
                                            .monospace()
                                            .size(text_height * 0.98),
                                    );
                                });
                            }
                            C::External => {
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
                                        // Build the label inside a short borrow so
                                        // no per-frame String is allocated here;
                                        // egui copies the text into the widget once
                                        // (unavoidable), and the hover tooltip
                                        // re-reads the value only while hovered.
                                        let label_widget = {
                                            let value = self
                                                .item_for_id(id)
                                                .and_then(|it| it.external_value(name))
                                                .map(|v| v.as_str())
                                                .unwrap_or("");
                                            egui::Label::new(
                                                RichText::new(value).size(text_height * 0.95),
                                            )
                                            .sense(Sense::click())
                                            .truncate()
                                            .show_tooltip_when_elided(false)
                                        };
                                        let label_resp = ui
                                            .add(label_widget)
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
                                        if label_resp.hovered() {
                                            let hover_value = self
                                                .item_for_id(id)
                                                .and_then(|it| it.external_value(name))
                                                .filter(|v| !v.is_empty())
                                                .cloned();
                                            if let Some(hover_value) = hover_value {
                                                label_resp.on_hover_text(hover_value);
                                            }
                                        }
                                    });
                                }
                            }
                            C::TypeBadge => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let (label, tooltip, fill, stroke) = badge.clone();
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
                            C::Length => {
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
                                        format_duration_scaled(secs, uses_hours)
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
                            C::Channels => {
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
                            C::SampleRate => {
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
                            C::Bits => {
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
                            C::BitRate => {
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
                            C::Peak => {
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
                                    let (orig, is_estimate) = self
                                        .meta_for_path(&path_owned)
                                        .map(|m| (m.peak_db, m.peak_db_estimate))
                                        .unwrap_or((None, false));
                                    let adj = orig.map(|db| db + gain_db);
                                    if let Some(db) = adj {
                                        ui.painter().rect_filled(rect2, 4.0, db_to_color(db));
                                    }
                                    let text = adj
                                        .map(|db| {
                                            if is_estimate {
                                                format!("~{:.1}", db)
                                            } else {
                                                format!("{:.1}", db)
                                            }
                                        })
                                        .unwrap_or_else(|| "...".into());
                                    let fid = egui::TextStyle::Monospace.resolve(ui.style());
                                    ui.painter().text(
                                        rect2.center(),
                                        egui::Align2::CENTER_CENTER,
                                        text,
                                        fid,
                                        egui::Color32::WHITE,
                                    );
                                    let resp2 = if is_estimate && adj.is_some() {
                                        resp2.on_hover_text("Estimated from the first 0.25 s")
                                    } else {
                                        resp2
                                    };
                                    let resp2 = self.attach_row_context_menu(resp2, row_idx, ctx);
                                    if resp2.clicked_by(egui::PointerButton::Primary) {
                                        clicked_to_load = true;
                                    }
                                });
                            }
                            C::Lufs => {
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
                            C::Dbtp => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let gain_db = self.pending_gain_db_for_path(&path_owned);
                                    let eff = self
                                        .meta_for_path(&path_owned)
                                        .and_then(|m| m.true_peak_db)
                                        .map(|v| v + gain_db);
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
                            C::LufsS => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let gain_db = self.pending_gain_db_for_path(&path_owned);
                                    let eff = self
                                        .meta_for_path(&path_owned)
                                        .and_then(|m| m.lufs_s_max)
                                        .map(|v| v + gain_db);
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
                            C::LufsM => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let gain_db = self.pending_gain_db_for_path(&path_owned);
                                    let eff = self
                                        .meta_for_path(&path_owned)
                                        .and_then(|m| m.lufs_m_max)
                                        .map(|v| v + gain_db);
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
                            C::SilenceLead | C::SilenceTail => {
                                let lead = sorted_col == C::SilenceLead;
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let ms = self.meta_for_path(&path_owned).and_then(|m| {
                                        if lead {
                                            m.silence_lead_ms
                                        } else {
                                            m.silence_tail_ms
                                        }
                                    });
                                    let resp = ui.add(
                                        egui::Label::new(
                                            RichText::new(
                                                ms.map(|v| format!("{:.0} ms", v))
                                                    .unwrap_or_else(|| "...".into()),
                                            )
                                            .monospace(),
                                        )
                                        .sense(Sense::click()),
                                    );
                                    let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                    if resp.clicked_by(egui::PointerButton::Primary) {
                                        clicked_to_load = true;
                                    }
                                });
                            }
                            // QA columns share one renderer: a passing file gets
                            // an empty cell so only the problems draw the eye.
                            C::EdgeZero | C::OverPeak | C::BlankPad => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    // Copy the verdict out before touching
                                    // &mut self below.
                                    let status =
                                        self.qa_status_for_column(sorted_col, &path_owned);
                                    let ng_fill = self.palette().error_text;
                                    let weak = ui.visuals().weak_text_color();
                                    let (rect2, resp2) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), row_h * 0.9),
                                        Sense::click(),
                                    );
                                    let fid = egui::TextStyle::Monospace.resolve(ui.style());
                                    let resp2 = match status {
                                        QaStatus::Pass => resp2,
                                        QaStatus::Unknown => {
                                            ui.painter().text(
                                                rect2.center(),
                                                egui::Align2::CENTER_CENTER,
                                                "...",
                                                fid,
                                                weak,
                                            );
                                            resp2
                                        }
                                        QaStatus::Fail(reason) => {
                                            ui.painter().rect_filled(rect2, 4.0, ng_fill);
                                            ui.painter().text(
                                                rect2.center(),
                                                egui::Align2::CENTER_CENTER,
                                                "NG",
                                                fid,
                                                egui::Color32::WHITE,
                                            );
                                            resp2.on_hover_text(reason)
                                        }
                                    };
                                    let resp2 = self.attach_row_context_menu(resp2, row_idx, ctx);
                                    if resp2.clicked_by(egui::PointerButton::Primary) {
                                        clicked_to_load = true;
                                    }
                                });
                            }
                            C::Bpm => {
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
                            C::CreatedAt => {
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
                            C::ModifiedAt => {
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
                            C::Gain => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let old = self.pending_gain_db_for_path(&path_owned);
                                    let mut g = old;
                                    // Table internals may change their auto-ID
                                    // sequence while async columns populate.
                                    // Keep the editor ID bound to the file so
                                    // click-to-text focus survives the next
                                    // frame.
                                    let resp = ui
                                        .push_id(("list_gain", &path_owned), |ui| {
                                            ui.add(
                                                egui::DragValue::new(&mut g)
                                                    .range(-24.0..=24.0)
                                                    .speed(0.1)
                                                    .fixed_decimals(1)
                                                    .suffix(" dB"),
                                            )
                                        })
                                        .inner;
                                    let resp = self.attach_row_context_menu(resp, row_idx, ctx);
                                    if resp.clicked_by(egui::PointerButton::Primary)
                                        || resp.has_focus()
                                    {
                                        interacted_with_control = true;
                                    }
                                    if resp.clicked_by(egui::PointerButton::Primary) {
                                        control_focus_id = Some(resp.id);
                                    }
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
                                            // Unified gain framework: with an open
                                            // editor tab the change is a destructive
                                            // editor edit (delta), else pending gain.
                                            let routed_to_editor =
                                                self.apply_file_gain_delta_unified(&path_owned, delta);
                                            if !routed_to_editor
                                                && self.playing_path.as_ref() == Some(&path_owned)
                                            {
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
                            C::Status | C::Tags => {
                                let is_tags = sorted_col == C::Tags;
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let outcome = if is_tags {
                                        self.ui_list_tags_cell(ui, &path_owned, row_h, text_height)
                                    } else {
                                        self.ui_list_status_cell(
                                            ui,
                                            &path_owned,
                                            row_h,
                                            text_height,
                                        )
                                    };
                                    if outcome.interacted_with_control {
                                        interacted_with_control = true;
                                    }
                                    if let Some(on_tags) = outcome.open_manager {
                                        self.open_status_tags_window(on_tags);
                                        interacted_with_control = true;
                                    }
                                });
                            }
                            C::Comments => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    if self.ui_list_comment_cell(
                                        ui,
                                        &path_owned,
                                        row_h,
                                        text_height,
                                    ) {
                                        interacted_with_control = true;
                                    }
                                });
                            }
                            C::Note => {
                                row.col(|ui| {
                                    if let Some(bg) = row_bg {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                                    }
                                    ui.visuals_mut().override_text_color = row_fg;
                                    let original = self
                                        .item_for_id(id)
                                        .map(|item| item.note.clone())
                                        .unwrap_or_default();
                                    let mut note = original.clone();
                                    let response = ui
                                        .push_id(("list_note", &path_owned), |ui| {
                                            ui.add_sized(
                                                [ui.available_width(), row_h * 0.8],
                                                egui::TextEdit::singleline(&mut note)
                                                    .hint_text("Add note..."),
                                            )
                                        })
                                        .inner;
                                    let original_id = response.id.with("original");
                                    if response.gained_focus() {
                                        ui.ctx().data_mut(|data| {
                                            data.insert_temp(original_id, original.clone())
                                        });
                                    }
                                    let cancel = response.has_focus()
                                        && ui.input(|input| input.key_pressed(egui::Key::Escape));
                                    if cancel {
                                        note = ui
                                            .ctx()
                                            .data(|data| data.get_temp::<String>(original_id))
                                            .unwrap_or(original);
                                        response.surrender_focus();
                                    }
                                    if response.clicked() || response.has_focus() {
                                        interacted_with_control = true;
                                    }
                                    if response.clicked() {
                                        control_focus_id = Some(response.id);
                                    }
                                    if (response.changed() || cancel) && note != self.item_for_id(id).map(|item| item.note.as_str()).unwrap_or("") {
                                        if let Some(item) = self.item_for_id_mut(id) {
                                            item.note = note;
                                        }
                                    }
                                });
                            }
                            C::Wave => {
                                let outcome = self.ui_list_wave_cell(
                                    &mut row,
                                    ctx,
                                    wave_cell::ListWaveCellCtx {
                                        row_idx,
                                        path: &path_owned,
                                        row_h,
                                        text_height,
                                        row_bg,
                                        row_fg,
                                        playhead: playhead_frame
                                            .as_ref()
                                            .filter(|frame| frame.path == path_owned)
                                            .map(|frame| &frame.info),
                                    },
                                );
                                if outcome.clicked_to_load {
                                    clicked_to_load = true;
                                }
                                if outcome.interacted_with_control {
                                    interacted_with_control = true;
                                }
                                if outcome.focus_list {
                                    Self::focus_list_widget(ctx);
                                    list_has_focus = true;
                                    self.search_has_focus = false;
                                }
                                if let Some(req) = outcome.seek_request {
                                    wave_seek_request = Some(req);
                                }
                            }
                        }
                        }

                        row.col(|ui| {
                            if let Some(bg) = row_bg {
                                ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                            }
                        });
                        // row-level interaction (must call response() after at least one col())
                        let resp = self.attach_row_context_menu(row.response(), row_idx, ctx);
                        if resp.rect.bottom() <= list_bottom + 0.5 {
                            last_fully_visible_row = Some(
                                last_fully_visible_row.map_or(row_idx, |v: usize| v.max(row_idx)),
                            );
                        }
                        let drag_started =
                            resp.drag_started_by(egui::PointerButton::Primary);
                        // Alt holds the row inside the app instead of handing
                        // it to the shell. A plain drag is already spoken for
                        // -- it drops the file into Explorer or a DAW -- and
                        // taking that away to gain a comment reference would
                        // be a bad trade. Alt is bound to nothing else here.
                        if drag_started && !interacted_with_control && ctx.input(|i| i.modifiers.alt)
                        {
                            if let Some(path) = self.path_for_row(row_idx).cloned() {
                                egui::DragAndDrop::set_payload(
                                    ctx,
                                    crate::app::ui::comments::CommentRefDrag(path),
                                );
                            }
                        } else if drag_started
                            && !interacted_with_control
                            && self.queue_external_drag_for_row(row_idx)
                        {
                            Self::focus_list_widget(ctx);
                            list_has_focus = true;
                            self.search_has_focus = false;
                            return;
                        }
                        let clicked_any = (resp.clicked_by(egui::PointerButton::Primary)
                            && !resp.double_clicked()
                            && !interacted_with_control)
                            || clicked_to_load;
                        if clicked_to_select {
                            self.selected = Some(row_idx);
                            self.scroll_to_selected = false;
                            self.selected_multi.clear();
                            self.selected_multi.insert(row_idx);
                            self.select_anchor = Some(row_idx);
                            Self::focus_list_widget(ctx);
                            list_has_focus = true;
                            self.search_has_focus = false;
                        } else if clicked_any {
                            let mods = ctx.input(|i| i.modifiers);
                            self.update_selection_on_click(row_idx, mods);
                            if self.list_click_audition {
                                self.select_and_load(row_idx, false);
                                if self.auto_play_list_nav {
                                    self.request_list_autoplay();
                                }
                            }
                            Self::focus_list_widget(ctx);
                            list_has_focus = true;
                            self.search_has_focus = false;
                        }
                        if let Some(control_focus_id) = control_focus_id {
                            ctx.memory_mut(|memory| memory.request_focus(control_focus_id));
                            list_has_focus = false;
                        }
                    } else if row_idx == self.files.len() {
                        // The row that closes the list. Scrolling stops with
                        // this on screen, so reaching the end is something the
                        // user reads rather than infers from a half-drawn row.
                        for _ in 0..filler_cols {
                            row.col(|ui| {
                                let _ = ui.allocate_exact_size(
                                    egui::vec2(ui.available_width(), row_h * 0.9),
                                    Sense::hover(),
                                );
                                let rect = ui.max_rect();
                                end_row_rect = Some(end_row_rect.map_or(rect, |all| all.union(rect)));
                            });
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

        if let Some(rect) = end_row_rect {
            end_row_fully_visible = rect.bottom() <= list_bottom + 0.5;
            // Fixed-height columns are clipped so their contents cannot break
            // the custom row-index scroll math. Paint the closing label from
            // the parent UI after the table instead, giving it the full row
            // width rather than the narrow first column's clip rectangle.
            ui.painter().text(
                egui::pos2(rect.left() + 6.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &end_marker,
                egui::FontId::proportional(text_height * 0.9),
                ui.style().visuals.weak_text_color(),
            );
        }
        self.list_end_row_fully_visible = end_row_fully_visible;
        self.list_last_fully_visible_row = last_fully_visible_row;
        if let Some(req) = wave_seek_request {
            self.apply_list_seek_request(req);
        }
        // egui can miss `drag_stopped` when the pointer is released outside
        // the cell/window. Commit the last visible target on a normal outside
        // release; a focus loss cancels and leaves playback stopped.
        if self.has_active_list_seek_gesture() && !ctx.input(|input| input.pointer.primary_down()) {
            let focused = ctx.input(|input| input.raw.focused);
            self.finish_list_seek_gesture_from_pointer_state(focused);
        }
        self.ui_list_scrollbar(ui, &metrics);
        self.commit_list_col_widths(ctx);

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

    /// In-cell rename editor shown in the file column while
    /// `inline_rename_path` targets this row. Enter commits, Escape or
    /// clicking elsewhere cancels; a failed commit keeps editing.
    fn ui_inline_rename_cell(&mut self, ui: &mut egui::Ui, path: &std::path::PathBuf) {
        let edit_id = egui::Id::new("list_inline_rename_edit");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.inline_rename_buffer)
                .id(edit_id)
                .font(egui::TextStyle::Monospace)
                .desired_width(ui.available_width().max(60.0)),
        );
        if self.inline_rename_focus_next {
            self.inline_rename_focus_next = false;
            resp.request_focus();
        }
        if resp.lost_focus() {
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let new_name = self.inline_rename_buffer.clone();
                match self.rename_file_path(path, &new_name) {
                    Ok(_) => {
                        self.inline_rename_path = None;
                    }
                    Err(err) => {
                        self.push_toast(
                            crate::app::types::ToastSeverity::Error,
                            format!("Rename failed: {err}"),
                        );
                        // Keep the editor open so the name can be fixed.
                        self.inline_rename_focus_next = true;
                    }
                }
            } else {
                self.inline_rename_path = None;
            }
        }
    }
}
