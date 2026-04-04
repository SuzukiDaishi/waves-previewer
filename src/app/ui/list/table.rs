use egui::{Align, Color32, RichText};
use egui_extras::{TableBuilder, TableRow};

use crate::app::{
    helpers::sortable_header,
    types::SortKey,
    WavesPreviewer,
};

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
        let row_count = self.files.len().max(12);
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
        let wheel_raw = ctx.input(|i| i.raw_scroll_delta);
        if metrics.pointer_over_list && wheel_raw != egui::Vec2::ZERO {
            self.last_list_scroll_at = Some(std::time::Instant::now());
        }
        self.scroll_to_selected
            && (key_moved
                || self
                    .last_list_scroll_at
                    .is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(300)))
    }

    pub(super) fn build_list_table<'a>(
        &mut self,
        ui: &'a mut egui::Ui,
        metrics: &ListViewMetrics,
        allow_auto_scroll: bool,
    ) -> (TableBuilder<'a>, usize, bool) {
        let cols = self.list_columns;
        let header_dirty = self.list_header_dirty();
        let mut filler_cols = 0usize;
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .auto_shrink([false, true])
            .sense(egui::Sense::click())
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
        if allow_auto_scroll {
            if let Some(sel) = self.selected {
                if sel < metrics.row_count {
                    table = table.scroll_to_row(sel, Some(Align::Center));
                    self.scroll_to_selected = false;
                }
            }
        }
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
            let end = render.visible_last_row.unwrap_or(start).min(self.files.len() - 1);
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
        self.queue_list_preview_prefetch_for_rows(render.visible_first_row, render.visible_last_row);
        if !render.missing_paths.is_empty() {
            for path in render.missing_paths {
                self.remove_missing_path(&path);
            }
        }
        if render.sort_changed {
            self.list_meta_prefetch_cursor = 0;
            self.prime_sort_metadata_prefetch();
            self.apply_sort();
        }
        if let Some(path) = render.to_open.as_ref() {
            self.open_or_activate_tab(path);
        }
        self.list_has_focus = interaction.list_has_focus;
    }

    fn list_header_dirty(&self) -> bool {
        self.tabs
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
            || !self.bit_depth_override.is_empty()
    }
}
