use crate::app::types::{ColumnId, ColumnKey, ListColumnConfig};
use egui::{Align, Color32, RichText};

#[derive(Clone, Debug)]
struct ColumnDrag(ColumnKey);

fn move_vec_item<T>(items: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from >= items.len() || to >= items.len() || from == to {
        return false;
    }
    let item = items.remove(from);
    items.insert(to, item);
    true
}

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_list_columns_window(&mut self, ctx: &egui::Context) {
        if !self.show_list_columns_window {
            return;
        }
        self.sanitize_list_column_layout();

        let viewport = ctx.content_rect();
        let max_size = egui::vec2(
            (viewport.width() - 32.0).max(1.0),
            (viewport.height() - 32.0).max(1.0),
        );
        let default_size = egui::vec2(520.0_f32.min(max_size.x), 680.0_f32.min(max_size.y));
        let centered_pos = viewport.center() - default_size * 0.5;
        let requested_pos = self
            .list_columns_window_pos
            .filter(|pos| pos.x.is_finite() && pos.y.is_finite())
            .unwrap_or(centered_pos);
        let mut open = self.show_list_columns_window;
        // The one dialog that was never registered as an input surface, so it
        // neither owned the wheel nor stood the list's keys down.
        let scroll_target = self.begin_floating_scroll_surface("list_columns_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let shown = egui::Window::new("List Columns")
            .open(&mut open)
            .collapsible(false)
            .movable(true)
            .resizable(true)
            .current_pos(requested_pos)
            .default_size(default_size)
            .min_size(egui::vec2(390.0, 320.0))
            .max_size(max_size)
            .constrain_to(viewport)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Columns shown in List").strong().size(16.0));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Reset").clicked() {
                            self.list_columns = ListColumnConfig::default();
                            for column in &mut self.metadata_list_columns {
                                column.visible = false;
                            }
                            self.list_column_layout = ColumnId::ALL
                                .iter()
                                .copied()
                                .filter(|column| *column != ColumnId::Note)
                                .map(ColumnKey::Builtin)
                                .chain(
                                    self.metadata_list_columns
                                        .iter()
                                        .map(|column| column.key.clone()),
                                )
                                .chain(std::iter::once(ColumnKey::Builtin(ColumnId::Note)))
                                .collect();
                            self.list_table_layout_revision =
                                self.list_table_layout_revision.wrapping_add(1);
                            self.sanitize_list_column_layout();
                            self.save_metadata_column_registry();
                            self.save_prefs();
                            self.ensure_sort_key_visible();
                            self.request_sort();
                        }
                    });
                });
                ui.label(
                    RichText::new(
                        "Drag rows vertically to set the exact left-to-right order. Use the checkbox to show or hide a column.",
                    )
                    .weak(),
                );
                ui.add_space(4.0);

                let visible_count = self
                    .list_column_layout
                    .iter()
                    .filter(|key| match key {
                        ColumnKey::Builtin(column) => column.enabled(&self.list_columns),
                        _ => self
                            .metadata_list_column_for_key(key)
                            .is_some_and(|(_, column)| column.visible),
                    })
                    .count();
                ui.label(
                    RichText::new(format!(
                        "{visible_count} visible · {} total",
                        self.list_column_layout.len()
                    ))
                    .small()
                    .weak(),
                );

                let order = self.list_column_layout.clone();
                let mut next_columns = self.list_columns;
                let mut metadata_visibility: Option<(ColumnKey, bool)> = None;
                let mut reorder: Option<(usize, usize)> = None;
                let external_available = !self.external_visible_columns.is_empty();

                egui::ScrollArea::vertical()
                    .id_salt("unified_list_columns")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        for (index, key) in order.iter().cloned().enumerate() {
                            let (label, kind, mut visible, available) = match &key {
                                ColumnKey::Builtin(column) => (
                                    column.label().to_string(),
                                    "BUILT-IN",
                                    column.enabled(&next_columns),
                                    *column != ColumnId::External || external_available,
                                ),
                                _ => {
                                    let column = self.metadata_list_column_for_key(&key);
                                    (
                                        column
                                            .map(|(_, column)| column.label.clone())
                                            .unwrap_or_else(|| key.serialized_name()),
                                        "METADATA",
                                        column.is_some_and(|(_, column)| column.visible),
                                        true,
                                    )
                                }
                            };
                            let fill = if visible && available {
                                ui.visuals().faint_bg_color
                            } else {
                                ui.visuals().extreme_bg_color.gamma_multiply(0.5)
                            };
                            let frame = egui::Frame::NONE
                                .fill(fill)
                                .corner_radius(6.0)
                                .inner_margin(egui::Margin::symmetric(8, 6));
                            let (zone, dropped) = ui.dnd_drop_zone::<ColumnDrag, _>(frame, |ui| {
                                ui.push_id(("list_column", key.serialized_name()), |ui| {
                                    ui.horizontal(|ui| {
                                        let response = ui.add_enabled(
                                            available,
                                            egui::Checkbox::without_text(&mut visible),
                                        );
                                        response.widget_info(|| {
                                            egui::WidgetInfo::labeled(
                                                egui::WidgetType::Checkbox,
                                                available,
                                                format!("Show {label} column"),
                                            )
                                        });
                                        if response.changed() {
                                            match &key {
                                                ColumnKey::Builtin(column) => {
                                                    column.set_enabled(&mut next_columns, visible)
                                                }
                                                _ => {
                                                    metadata_visibility = Some((key.clone(), visible))
                                                }
                                            }
                                        }

                                        let drag_id = ui.make_persistent_id((
                                            "list_column_drag",
                                            key.serialized_name(),
                                        ));
                                        let is_dragging = ui.ctx().is_being_dragged(drag_id);
                                        let drag_source = ui.dnd_drag_source(
                                            drag_id,
                                            ColumnDrag(key.clone()),
                                            |ui| {
                                                let drag_frame = if is_dragging {
                                                    egui::Frame::NONE
                                                        .fill(ui.visuals().window_fill())
                                                        .stroke(ui.visuals().selection.stroke)
                                                        .corner_radius(6.0)
                                                        .inner_margin(egui::Margin::symmetric(8, 6))
                                                } else {
                                                    egui::Frame::NONE
                                                };
                                                drag_frame.show(ui, |ui| {
                                                    ui.set_min_width(ui.available_width());
                                                    ui.horizontal(|ui| {
                                                        ui.label(
                                                            RichText::new("≡")
                                                                .size(18.0)
                                                                .color(Color32::from_gray(145)),
                                                        );
                                                        ui.label(
                                                            RichText::new(format!(
                                                                "{:02}",
                                                                index + 1
                                                            ))
                                                            .monospace()
                                                            .weak(),
                                                        );
                                                        ui.add(
                                                            egui::Label::new(if available {
                                                                RichText::new(&label).strong()
                                                            } else {
                                                                RichText::new(&label).weak()
                                                            })
                                                            .truncate(),
                                                        );
                                                        ui.with_layout(
                                                            egui::Layout::right_to_left(
                                                                Align::Center,
                                                            ),
                                                            |ui| {
                                                                ui.label(
                                                                    RichText::new(kind)
                                                                        .small()
                                                                        .weak(),
                                                                );
                                                            },
                                                        );
                                                    })
                                                })
                                            },
                                        );
                                        drag_source.response.widget_info(|| {
                                            egui::WidgetInfo::labeled(
                                                egui::WidgetType::Button,
                                                true,
                                                format!("Drag {label} column"),
                                            )
                                        });
                                        drag_source
                                            .response
                                            .on_hover_text("Drag up or down to reorder");
                                    });
                                });
                            });
                            if let Some(payload) = dropped {
                                if let Some(from) = order.iter().position(|entry| *entry == payload.0)
                                {
                                    reorder = Some((from, index));
                                }
                            }
                            if egui::DragAndDrop::has_payload_of_type::<ColumnDrag>(ui.ctx())
                                && zone.response.contains_pointer()
                            {
                                ui.painter().rect_stroke(
                                    zone.response.rect,
                                    6.0,
                                    egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            zone.response.on_hover_text("Drop to place the column here");
                            ui.add_space(3.0);
                        }
                    });

                if next_columns != self.list_columns {
                    self.list_columns = next_columns;
                    self.ensure_sort_key_visible();
                    self.request_sort();
                }
                if let Some((key, visible)) = metadata_visibility {
                    if let Some(index) = self
                        .metadata_list_columns
                        .iter()
                        .position(|column| column.key == key)
                    {
                        self.metadata_list_columns[index].visible = visible;
                        self.save_metadata_column_registry();
                        self.ensure_sort_key_visible();
                        self.request_sort();
                    }
                }
                let any_visible = self.list_column_layout.iter().any(|key| match key {
                    ColumnKey::Builtin(column) => column.enabled(&self.list_columns),
                    _ => self
                        .metadata_list_column_for_key(key)
                        .is_some_and(|(_, column)| column.visible),
                });
                if !any_visible {
                    ColumnId::File.set_enabled(&mut self.list_columns, true);
                }
                if let Some((from, to)) = reorder {
                    if move_vec_item(&mut self.list_column_layout, from, to) {
                        self.list_table_layout_revision =
                            self.list_table_layout_revision.wrapping_add(1);
                        self.sanitize_list_column_layout();
                        self.save_prefs();
                    }
                }
            });

        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
            let position = shown.response.rect.min;
            self.list_columns_window_pos = Some(position);
            let global_position_changed = self.project_path.is_none()
                && self.list_columns_window_global_pos.map_or(true, |saved| {
                    (saved.x - position.x).abs() > 0.5 || (saved.y - position.y).abs() > 0.5
                });
            if global_position_changed && ctx.input(|input| input.pointer.any_released()) {
                self.list_columns_window_global_pos = Some(position);
                self.save_prefs();
            }
        } else {
            open = false;
        }
        self.show_list_columns_window = open;
    }
}

#[cfg(test)]
mod tests {
    use super::move_vec_item;

    #[test]
    fn move_vec_item_supports_both_directions_and_rejects_invalid_moves() {
        let mut values = vec!["a", "b", "c", "d"];
        assert!(move_vec_item(&mut values, 0, 2));
        assert_eq!(values, ["b", "c", "a", "d"]);
        assert!(move_vec_item(&mut values, 3, 1));
        assert_eq!(values, ["b", "d", "c", "a"]);
        assert!(!move_vec_item(&mut values, 1, 1));
    }
}
