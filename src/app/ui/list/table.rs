use egui::{Align, Color32, RichText};
use egui_extras::{TableBuilder, TableRow};

use crate::app::{helpers::sortable_header, types::SortKey, WavesPreviewer};

use super::{ListInteractionState, ListRenderState, ListViewMetrics};

impl WavesPreviewer {
    pub(super) fn list_view_metrics(&mut self, ui: &mut egui::Ui) -> ListViewMetrics {
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
        // Rows actually rendered: the visible window plus one partial row,
        // with a floor so tiny viewports still show a usable list.
        let row_count = (visible_rows + 1).max(12);
        let external_cols = if cols.external {
            self.external_visible_columns.clone()
        } else {
            Vec::new()
        };
        let list_rect = ui.available_rect_before_wrap();
        let pointer_over_list = ui
            .input(|i| i.pointer.hover_pos())
            .is_some_and(|p| list_rect.contains(p));
        if self.debug.cfg.enabled {
            self.debug.last_pointer_over_list = pointer_over_list;
        }
        ListViewMetrics {
            avail_h,
            external_cols,
            header_h,
            list_rect,
            pointer_over_list,
            row_count,
            row_h,
            text_height,
            visible_rows,
        }
    }

    pub(super) fn list_allow_auto_scroll(
        &mut self,
        ctx: &egui::Context,
        metrics: &ListViewMetrics,
        key_moved: bool,
    ) -> bool {
        let wheel_raw = ctx.input(|i| i.smooth_scroll_delta);
        if metrics.pointer_over_list && wheel_raw != egui::Vec2::ZERO {
            self.last_list_scroll_at = Some(std::time::Instant::now());
        }
        self.scroll_to_selected
            && (key_moved
                || self
                    .last_list_scroll_at
                    .is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(300)))
    }

    /// Update the row-window scroll state from wheel input, selection
    /// auto-scroll, and list length. Runs before the table is built so this
    /// frame renders the final window (no one-frame lag on jumps).
    pub(super) fn update_list_scroll_state(
        &mut self,
        ctx: &egui::Context,
        metrics: &ListViewMetrics,
        allow_auto_scroll: bool,
    ) {
        let total = self.files.len();
        let visible = metrics.visible_rows.max(1);
        let max_start = total.saturating_sub(visible);
        // Wheel scrolling accumulates fractional rows; the window itself
        // always starts on a whole row (index-based, precise at any size).
        if metrics.pointer_over_list {
            let dy = ctx.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 && total > visible {
                self.list_scroll_residual -= dy / metrics.row_h.max(1.0);
                let whole = self.list_scroll_residual.trunc();
                if whole != 0.0 {
                    self.list_scroll_residual -= whole;
                    let delta = whole as i64;
                    let cur = self.list_scroll_row as i64;
                    self.list_scroll_row =
                        (cur + delta).clamp(0, max_start as i64) as usize;
                }
            }
        }
        if allow_auto_scroll {
            if let Some(sel) = self.selected.filter(|&s| s < total) {
                // Keep the selected row centered, matching the old
                // scroll_to_row(sel, Align::Center) behavior.
                self.list_scroll_row = sel.saturating_sub(visible / 2).min(max_start);
                self.scroll_to_selected = false;
            }
        }
        self.list_scroll_row = self.list_scroll_row.min(max_start);
    }

    /// Custom index-based scrollbar for the list. The thumb maps directly to
    /// `list_scroll_row` in f64, so it stays pixel-accurate at 1M rows where
    /// egui's own f32 scroll offsets quantize.
    pub(super) fn ui_list_scrollbar(
        &mut self,
        ui: &mut egui::Ui,
        metrics: &ListViewMetrics,
    ) {
        let total = self.files.len();
        let visible = metrics.visible_rows.max(1);
        if total <= visible {
            return;
        }
        const BAR_W: f32 = 12.0;
        let list_rect = metrics.list_rect;
        let bar_rect = egui::Rect::from_min_max(
            egui::pos2(list_rect.right() - BAR_W, list_rect.top() + metrics.header_h),
            list_rect.right_bottom(),
        );
        let id = ui.id().with("list_vscroll_custom");
        let resp = ui.interact(bar_rect, id, egui::Sense::click_and_drag());
        let track_h = bar_rect.height().max(1.0);
        let thumb_h = ((visible as f64 / total as f64) * track_h as f64)
            .max(24.0_f64.min(track_h as f64 * 0.5)) as f32;
        let denom = (total - visible) as f64;
        if (resp.dragged() || resp.clicked()) && denom > 0.0 {
            if let Some(pos) = resp.interact_pointer_pos() {
                let frac = ((pos.y - bar_rect.top() - thumb_h * 0.5)
                    / (track_h - thumb_h).max(1.0)) as f64;
                let row = (frac.clamp(0.0, 1.0) * denom).round() as usize;
                self.list_scroll_row = row.min(total - visible);
                self.last_list_scroll_at = Some(std::time::Instant::now());
            }
        }
        let frac = if denom > 0.0 {
            (self.list_scroll_row as f64 / denom).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let thumb_top = bar_rect.top() + frac as f32 * (track_h - thumb_h).max(0.0);
        let visuals = ui.style().visuals.clone();
        ui.painter().rect_filled(
            bar_rect,
            0.0,
            visuals.extreme_bg_color.gamma_multiply(0.5),
        );
        let thumb_rect = egui::Rect::from_min_max(
            egui::pos2(bar_rect.left() + 2.0, thumb_top),
            egui::pos2(bar_rect.right() - 2.0, thumb_top + thumb_h),
        );
        let thumb_color = if resp.hovered() || resp.dragged() {
            visuals.widgets.hovered.bg_fill
        } else {
            visuals.widgets.inactive.bg_fill
        };
        ui.painter().rect_filled(thumb_rect, 4.0, thumb_color);
    }

    pub(super) fn build_list_table<'a>(
        &mut self,
        ui: &'a mut egui::Ui,
        metrics: &ListViewMetrics,
    ) -> (TableBuilder<'a>, usize, bool) {
        let cols = self.list_columns;
        let header_dirty = self.list_header_dirty();
        let mut filler_cols = 0usize;
        let mut table = TableBuilder::new(ui)
            .id_salt("list_table")
            .striped(true)
            .resizable(true)
            .auto_shrink([false, true])
            // Vertical scrolling is handled by the app: only the visible row
            // window is ever handed to the table, so egui never sees a huge
            // content height (f32 offsets quantize past ~16.7M px).
            .vscroll(false)
            .sense(egui::Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(Align::Center));
        if cols.edited {
            table = table.column(egui_extras::Column::initial(30.0).resizable(false));
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
            for _ in 0..metrics.external_cols.len() {
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
        if cols.dbtp {
            table = table.column(egui_extras::Column::initial(90.0).resizable(true));
            filler_cols += 1;
        }
        if cols.lufs_s {
            table = table.column(egui_extras::Column::initial(90.0).resizable(true));
            filler_cols += 1;
        }
        if cols.lufs_m {
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
            .min_scrolled_height((metrics.avail_h - metrics.header_h).max(0.0));
        filler_cols += 1;
        (table, filler_cols, header_dirty)
    }

    pub(super) fn render_list_header(
        &mut self,
        header: &mut TableRow<'_, '_>,
        metrics: &ListViewMetrics,
        header_dirty: bool,
        sort_changed: &mut bool,
    ) {
        let cols = self.list_columns;
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
            for (idx, name) in metrics.external_cols.iter().enumerate() {
                header.col(|ui| {
                    *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
                    ui,
                    "LUFS (I)",
                    &mut self.sort_key,
                    &mut self.sort_dir,
                    SortKey::Lufs,
                    false,
                );
            });
        }
        if cols.dbtp {
            header.col(|ui| {
                *sort_changed |= sortable_header(
                    ui,
                    "dBTP",
                    &mut self.sort_key,
                    &mut self.sort_dir,
                    SortKey::TruePeak,
                    false,
                );
            });
        }
        if cols.lufs_s {
            header.col(|ui| {
                *sort_changed |= sortable_header(
                    ui,
                    "LUFS-S",
                    &mut self.sort_key,
                    &mut self.sort_dir,
                    SortKey::LufsShort,
                    false,
                );
            });
        }
        if cols.lufs_m {
            header.col(|ui| {
                *sort_changed |= sortable_header(
                    ui,
                    "LUFS-M",
                    &mut self.sort_key,
                    &mut self.sort_dir,
                    SortKey::LufsMomentary,
                    false,
                );
            });
        }
        if cols.bpm {
            header.col(|ui| {
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
                *sort_changed |= sortable_header(
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
    }

    pub(super) fn finish_list_view(
        &mut self,
        render: ListRenderState,
        interaction: ListInteractionState,
    ) {
        if self.item_bg_mode != crate::app::types::ItemBgMode::Standard && !self.files.is_empty() {
            let start = render
                .visible_first_row
                .or(self.selected)
                .unwrap_or(0)
                .min(self.files.len() - 1);
            let end = render
                .visible_last_row
                .unwrap_or(start)
                .min(self.files.len() - 1);
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
        self.queue_list_preview_prefetch_for_rows(
            render.visible_first_row,
            render.visible_last_row,
        );
        if !render.missing_paths.is_empty() {
            for path in render.missing_paths {
                self.remove_missing_path(&path);
            }
        }
        if render.sort_changed {
            self.list_meta_prefetch_cursor = 0;
            self.prime_sort_metadata_prefetch();
            self.request_sort();
        }
        if let Some(path) = render.to_open.as_ref() {
            self.open_or_activate_tab(path);
        }
        self.list_has_focus = interaction.list_has_focus;
    }

    fn list_header_dirty(&mut self) -> bool {
        self.tabs
            .iter()
            .any(|t| t.dirty || t.loop_markers_dirty || t.markers_dirty)
            || self
                .edited_cache
                .values()
                .any(|c| c.dirty || c.loop_markers_dirty || c.markers_dirty)
            // Throttled cache: a raw scan over all items here runs every
            // frame and dominated the frame at 100k+ files.
            || self.pending_gain_count_throttled() > 0
            || !self.sample_rate_override.is_empty()
            || !self.bit_depth_override.is_empty()
    }
}
