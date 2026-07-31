use crate::app::types::{ColumnId, SortKey};
use egui::{Align, Color32, RichText};

#[derive(Clone, Copy, Debug)]
struct BuiltinColumnDrag(ColumnId);

#[derive(Clone, Debug)]
struct MetadataColumnDrag(String);

fn move_vec_item<T>(items: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from >= items.len() || to >= items.len() || from == to {
        return false;
    }
    let item = items.remove(from);
    items.insert(to, item);
    true
}

fn set_button_accessible_label(response: &egui::Response, label: String) {
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), label.clone())
    });
}

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_list_columns_window(&mut self, ctx: &egui::Context) {
        if !self.show_list_columns_window {
            return;
        }

        let viewport = ctx.content_rect();
        let window_max = egui::vec2(
            (viewport.width() - 32.0).max(1.0),
            (viewport.height() - 32.0).max(1.0),
        );
        let window_min = egui::vec2(420.0_f32.min(window_max.x), 320.0_f32.min(window_max.y));
        let window_default = egui::vec2(560.0_f32.min(window_max.x), 720.0_f32.min(window_max.y));
        let content_max_height = (window_max.y - 44.0).max(120.0);
        let mut open = self.show_list_columns_window;

        egui::Window::new("List Columns")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .fade_in(false)
            .fade_out(false)
            .default_size(window_default)
            .min_size(window_min)
            .max_size(window_max)
            .constrain_to(viewport)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("Arrange the columns shown in List")
                            .strong()
                            .size(16.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Default visibility").clicked() {
                            self.list_columns = crate::app::types::ListColumnConfig::default();
                            self.ensure_sort_key_visible();
                            self.request_sort();
                        }
                        if ui.button("Reset order").clicked() {
                            self.list_column_order = ColumnId::ALL.to_vec();
                            self.save_prefs();
                        }
                    });
                });
                ui.label(
                    RichText::new(
                        "Checkboxes show or hide columns. Drag the ≡ handle, or use the arrows, to \
                         set the left-to-right order. Hidden columns keep their position.",
                    )
                    .weak(),
                );
                ui.add_space(4.0);

                let scroll_height = ui.available_height().max(120.0).min(content_max_height);
                egui::ScrollArea::vertical()
                    .id_salt("list_columns_window_scroll")
                    .auto_shrink([false, false])
                    .max_height(scroll_height)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("LIST COLUMNS").small().strong());
                            let visible = self
                                .list_column_order
                                .iter()
                                .filter(|column| column.enabled(&self.list_columns))
                                .count();
                            ui.label(
                                RichText::new(format!(
                                    "{visible} visible · {} total",
                                    self.list_column_order.len()
                                ))
                                .small()
                                .weak(),
                            );
                        });
                        ui.add_space(3.0);

                        let order = self.list_column_order.clone();
                        let mut next_columns = self.list_columns;
                        let external_available = !self.external_visible_columns.is_empty();
                        let mut visibility_changed = false;
                        let mut reorder_request: Option<(usize, usize)> = None;

                        for (index, column) in order.iter().copied().enumerate() {
                            let enabled = column.enabled(&next_columns);
                            let available = column != ColumnId::External || external_available;
                            let base_fill = if enabled && available {
                                ui.visuals().faint_bg_color
                            } else {
                                ui.visuals().extreme_bg_color.gamma_multiply(0.45)
                            };
                            let frame = egui::Frame::NONE
                                .fill(base_fill)
                                .corner_radius(5.0)
                                .inner_margin(egui::Margin::symmetric(7, 4));
                            let (drop_zone, dropped) =
                                ui.dnd_drop_zone::<BuiltinColumnDrag, _>(frame, |ui| {
                                    ui.push_id(("builtin_list_column", column.name()), |ui| {
                                        ui.set_min_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            let handle = ui
                                                .add(
                                                    egui::Label::new(
                                                        RichText::new("≡")
                                                            .color(Color32::from_gray(130)),
                                                    )
                                                    .sense(egui::Sense::drag()),
                                                )
                                                .on_hover_text("Drag to reorder");
                                            handle.dnd_set_drag_payload(BuiltinColumnDrag(column));
                                            handle.widget_info(|| {
                                                egui::WidgetInfo::labeled(
                                                    egui::WidgetType::Other,
                                                    true,
                                                    format!("Drag {} column", column.label()),
                                                )
                                            });
                                            ui.label(
                                                RichText::new(format!("{:02}", index + 1))
                                                    .monospace()
                                                    .weak(),
                                            );

                                            let up = ui
                                                .add_enabled(
                                                    index > 0,
                                                    egui::Button::new("↑").small(),
                                                )
                                                .on_hover_text("Move left in List");
                                            set_button_accessible_label(
                                                &up,
                                                format!("Move {} earlier", column.label()),
                                            );
                                            if up.clicked() {
                                                reorder_request =
                                                    Some((index, index.saturating_sub(1)));
                                            }

                                            let down = ui
                                                .add_enabled(
                                                    index + 1 < order.len(),
                                                    egui::Button::new("↓").small(),
                                                )
                                                .on_hover_text("Move right in List");
                                            set_button_accessible_label(
                                                &down,
                                                format!("Move {} later", column.label()),
                                            );
                                            if down.clicked() {
                                                reorder_request = Some((index, index + 1));
                                            }

                                            let mut next_enabled = enabled;
                                            let checkbox = ui.add_enabled(
                                                available,
                                                egui::Checkbox::without_text(&mut next_enabled),
                                            );
                                            checkbox.widget_info(|| {
                                                egui::WidgetInfo::selected(
                                                    egui::WidgetType::Checkbox,
                                                    available,
                                                    next_enabled,
                                                    column.label(),
                                                )
                                            });
                                            if checkbox.changed() {
                                                column.set_enabled(&mut next_columns, next_enabled);
                                                visibility_changed = true;
                                            }

                                            let label = if available {
                                                RichText::new(column.label())
                                            } else {
                                                RichText::new(column.label()).weak()
                                            };
                                            ui.add(
                                                egui::Label::new(label)
                                                    .truncate()
                                                    .sense(egui::Sense::hover()),
                                            )
                                            .on_hover_text(if available {
                                                column.label().to_string()
                                            } else {
                                                "Load External Data before enabling this column"
                                                    .to_string()
                                            });
                                        });
                                    });
                                });
                            if let Some(payload) = dropped {
                                if let Some(from) = order.iter().position(|item| *item == payload.0)
                                {
                                    reorder_request = Some((from, index));
                                }
                            }
                            drop_zone
                                .response
                                .on_hover_text("Drop here to change the List column order");
                            ui.add_space(3.0);
                        }

                        let any_builtin_visible = ColumnId::ALL.iter().copied().any(|column| {
                            column.enabled(&next_columns)
                                && (column != ColumnId::External || external_available)
                        });
                        if !any_builtin_visible {
                            ColumnId::File.set_enabled(&mut next_columns, true);
                            visibility_changed = true;
                        }
                        if visibility_changed && next_columns != self.list_columns {
                            self.list_columns = next_columns;
                            self.ensure_sort_key_visible();
                            self.request_sort();
                        }
                        if let Some((from, to)) = reorder_request {
                            if move_vec_item(&mut self.list_column_order, from, to) {
                                self.save_prefs();
                            }
                        }

                        if !self.metadata_list_columns.is_empty() {
                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("METADATA COLUMNS").small().strong());
                                let visible = self
                                    .metadata_list_columns
                                    .iter()
                                    .filter(|column| column.visible)
                                    .count();
                                ui.label(
                                    RichText::new(format!(
                                        "{visible} visible · {} registered",
                                        self.metadata_list_columns.len()
                                    ))
                                    .small()
                                    .weak(),
                                );
                            });
                            ui.label(
                                RichText::new(
                                    "Metadata columns appear after the built-in columns and can be \
                                     reordered within this section.",
                                )
                                .small()
                                .weak(),
                            );
                            ui.add_space(3.0);

                            let metadata = self.metadata_list_columns.clone();
                            let sorted_metadata_key =
                                if let SortKey::Metadata(index) = self.sort_key {
                                    self.metadata_list_columns
                                        .get(index)
                                        .map(|column| column.key.clone())
                                } else {
                                    None
                                };
                            let mut metadata_visibility_changed = false;
                            let mut metadata_reorder: Option<(usize, usize)> = None;

                            for (index, column) in metadata.iter().enumerate() {
                                let base_fill = if column.visible {
                                    ui.visuals().faint_bg_color
                                } else {
                                    ui.visuals().extreme_bg_color.gamma_multiply(0.45)
                                };
                                let frame = egui::Frame::NONE
                                    .fill(base_fill)
                                    .corner_radius(5.0)
                                    .inner_margin(egui::Margin::symmetric(7, 4));
                                let serialized_key = column.key.serialized_name();
                                let (drop_zone, dropped) = ui
                                    .dnd_drop_zone::<MetadataColumnDrag, _>(frame, |ui| {
                                        ui.push_id(
                                            ("metadata_list_column", &serialized_key),
                                            |ui| {
                                                ui.set_min_width(ui.available_width());
                                                ui.horizontal(|ui| {
                                                    let handle = ui
                                                        .add(
                                                            egui::Label::new(
                                                                RichText::new("≡")
                                                                    .color(Color32::from_gray(130)),
                                                            )
                                                            .sense(egui::Sense::drag()),
                                                        )
                                                        .on_hover_text("Drag to reorder");
                                                    handle.dnd_set_drag_payload(
                                                        MetadataColumnDrag(serialized_key.clone()),
                                                    );
                                                    handle.widget_info(|| {
                                                        egui::WidgetInfo::labeled(
                                                            egui::WidgetType::Other,
                                                            true,
                                                            format!(
                                                                "Drag metadata {} column",
                                                                column.label
                                                            ),
                                                        )
                                                    });
                                                    ui.label(
                                                        RichText::new(format!("{:02}", index + 1))
                                                            .monospace()
                                                            .weak(),
                                                    );

                                                    let up = ui
                                                        .add_enabled(
                                                            index > 0,
                                                            egui::Button::new("↑").small(),
                                                        )
                                                        .on_hover_text(
                                                            "Move left among metadata columns",
                                                        );
                                                    set_button_accessible_label(
                                                        &up,
                                                        format!(
                                                            "Move metadata {} earlier",
                                                            column.label
                                                        ),
                                                    );
                                                    if up.clicked() {
                                                        metadata_reorder =
                                                            Some((index, index.saturating_sub(1)));
                                                    }

                                                    let down = ui
                                                        .add_enabled(
                                                            index + 1 < metadata.len(),
                                                            egui::Button::new("↓").small(),
                                                        )
                                                        .on_hover_text(
                                                            "Move right among metadata columns",
                                                        );
                                                    set_button_accessible_label(
                                                        &down,
                                                        format!(
                                                            "Move metadata {} later",
                                                            column.label
                                                        ),
                                                    );
                                                    if down.clicked() {
                                                        metadata_reorder = Some((index, index + 1));
                                                    }

                                                    let mut visible = column.visible;
                                                    let checkbox = ui.add(
                                                        egui::Checkbox::without_text(&mut visible),
                                                    );
                                                    checkbox.widget_info(|| {
                                                        egui::WidgetInfo::selected(
                                                            egui::WidgetType::Checkbox,
                                                            true,
                                                            visible,
                                                            column.label.clone(),
                                                        )
                                                    });
                                                    if checkbox.changed() {
                                                        if let Some(target) = self
                                                            .metadata_list_columns
                                                            .get_mut(index)
                                                        {
                                                            target.visible = visible;
                                                            metadata_visibility_changed = true;
                                                        }
                                                    }
                                                    ui.add(
                                                        egui::Label::new(&column.label)
                                                            .truncate()
                                                            .sense(egui::Sense::hover()),
                                                    )
                                                    .on_hover_text(format!(
                                                        "{}\n{}",
                                                        column.label, serialized_key
                                                    ));
                                                });
                                            },
                                        );
                                    });
                                if let Some(payload) = dropped {
                                    if let Some(from) = metadata
                                        .iter()
                                        .position(|item| item.key.serialized_name() == payload.0)
                                    {
                                        metadata_reorder = Some((from, index));
                                    }
                                }
                                drop_zone
                                    .response
                                    .on_hover_text("Drop here to change the metadata column order");
                                ui.add_space(3.0);
                            }

                            if let Some((from, to)) = metadata_reorder {
                                if move_vec_item(&mut self.metadata_list_columns, from, to) {
                                    if let Some(key) = sorted_metadata_key.as_ref() {
                                        if let Some(index) = self
                                            .metadata_list_columns
                                            .iter()
                                            .position(|column| &column.key == key)
                                        {
                                            self.sort_key = SortKey::Metadata(index);
                                        }
                                    }
                                    self.save_metadata_column_registry();
                                    self.metadata_summary_generation =
                                        self.metadata_summary_generation.wrapping_add(1);
                                    self.request_sort();
                                }
                            }
                            if metadata_visibility_changed {
                                self.metadata_summary_generation =
                                    self.metadata_summary_generation.wrapping_add(1);
                                self.ensure_sort_key_visible();
                                self.request_sort();
                            }
                        }
                    });
            });

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
        assert!(!move_vec_item(&mut values, 9, 0));
    }
}
