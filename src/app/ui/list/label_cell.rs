//! The list's Status and Tags columns.
//!
//! Both cells are a badge the user clicks to open a picker, so they share the
//! painting and the "commit as one undoable edit" path here rather than
//! growing two more arms of `ui_list_view`'s column match.

use std::path::Path;

use egui::{Color32, Sense};

/// What a cell wants the caller to do once the row loop can act on it.
#[derive(Default)]
pub(crate) struct ListLabelCellOutcome {
    /// The cell owned the pointer, so the row must not treat this as a click
    /// on the row itself.
    pub(crate) interacted_with_control: bool,
    /// Open the manager window on the Statuses (false) or Tags (true) tab.
    pub(crate) open_manager: Option<bool>,
}

impl crate::app::WavesPreviewer {
    /// The paths a label edit started from this row should apply to: the whole
    /// selection when the row is part of it, otherwise just this row. Editing
    /// one row of a selection without this would silently ignore the rest.
    fn label_edit_targets(&self, path: &Path) -> Vec<std::path::PathBuf> {
        let selected = self.selected_paths();
        if selected.iter().any(|candidate| candidate == path) {
            selected
        } else {
            vec![path.to_path_buf()]
        }
    }

    pub(super) fn ui_list_status_cell(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        row_h: f32,
        text_height: f32,
    ) -> ListLabelCellOutcome {
        let mut outcome = ListLabelCellOutcome::default();
        let current = self
            .item_for_path(path)
            .and_then(|item| item.status_id.as_deref().map(str::to_string));
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), row_h * 0.9),
            Sense::click(),
        );
        match current.as_deref() {
            Some(id) => {
                let label = self.status_palette.label_for(id);
                let fill = self.status_palette.color_for(id);
                // Sized to the label and left-aligned, so a Status badge and
                // a Tags badge read as the same kind of thing rather than the
                // status filling the column as a solid block.
                let height = (rect.height() - 6.0).clamp(16.0, 24.0);
                let width = Self::list_badge_width(ui, text_height, &label)
                    .min((rect.width() - 8.0).max(1.0));
                let badge = egui::Rect::from_min_size(
                    egui::pos2(rect.left() + 4.0, rect.center().y - height * 0.5),
                    egui::vec2(width, height),
                );
                Self::paint_list_badge(
                    ui,
                    badge,
                    text_height,
                    &label,
                    Color32::from_rgb(fill[0], fill[1], fill[2]),
                    Color32::from_rgb(fill[0], fill[1], fill[2]),
                    crate::app::status_tags::text_color_on(fill),
                    false,
                );
            }
            None => {
                ui.painter().text(
                    rect.left_center() + egui::vec2(6.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    "—",
                    egui::FontId::proportional((text_height * 0.9).max(9.0)),
                    ui.visuals().weak_text_color(),
                );
            }
        }
        if response.hovered() {
            ui.painter().rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(1.0, ui.visuals().widgets.hovered.bg_stroke.color),
                egui::StrokeKind::Inside,
            );
        }
        if response.clicked() {
            outcome.interacted_with_control = true;
        }
        // Both of these are per-visible-row, per-frame work, so they wait
        // until this row's menu is actually open: `defs` allocates a String
        // per entry, and `selected_paths` clones the whole selection, which
        // on a large list is unbounded.
        // `clicked()` covers the frame the menu opens on: the popup's own
        // open flag is not set until `show` runs, just below.
        let open = response.clicked()
            || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
        let defs = if open {
            self.status_palette.defs.clone()
        } else {
            Vec::new()
        };
        let mut picked: Option<Option<String>> = None;
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(150.0);
            if ui.selectable_label(current.is_none(), "— (none)").clicked() {
                picked = Some(None);
                ui.close();
            }
            for def in &defs {
                let selected = current.as_deref() == Some(&*def.id);
                if Self::label_menu_entry(ui, def, selected).clicked() {
                    picked = Some(Some(def.id.to_string()));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Edit Statuses...").clicked() {
                outcome.open_manager = Some(false);
                ui.close();
            }
        });
        if let Some(choice) = picked {
            let targets = self.label_edit_targets(path);
            self.set_status_for_paths(&targets, choice.as_deref());
            outcome.interacted_with_control = true;
        }
        outcome
    }

    pub(super) fn ui_list_tags_cell(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        row_h: f32,
        text_height: f32,
    ) -> ListLabelCellOutcome {
        let mut outcome = ListLabelCellOutcome::default();
        let current: Vec<String> = self
            .item_for_path(path)
            .map(|item| item.tags().iter().map(|tag| tag.to_string()).collect())
            .unwrap_or_default();
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), row_h * 0.9),
            Sense::click(),
        );
        if current.is_empty() {
            ui.painter().text(
                rect.left_center() + egui::vec2(6.0, 0.0),
                egui::Align2::LEFT_CENTER,
                "—",
                egui::FontId::proportional((text_height * 0.9).max(9.0)),
                ui.visuals().weak_text_color(),
            );
        } else {
            // Lay the badges left to right and stop at the edge, reserving
            // room for a "+N" so the count of what is hidden stays visible.
            let height = (rect.height() - 6.0).clamp(16.0, 24.0);
            let mut x = rect.left() + 4.0;
            let mut shown = 0usize;
            for id in &current {
                let label = self.tag_palette.label_for(id);
                let width = Self::list_badge_width(ui, text_height, &label);
                let remaining = current.len() - shown;
                let reserve = if remaining > 1 { 26.0 } else { 0.0 };
                if x + width + reserve > rect.right() - 4.0 && shown > 0 {
                    break;
                }
                let fill = self.tag_palette.color_for(id);
                Self::paint_list_badge(
                    ui,
                    egui::Rect::from_min_size(
                        egui::pos2(x, rect.center().y - height * 0.5),
                        egui::vec2(width.min(rect.right() - 4.0 - x).max(1.0), height),
                    ),
                    text_height,
                    &label,
                    Color32::from_rgb(fill[0], fill[1], fill[2]),
                    Color32::from_rgb(fill[0], fill[1], fill[2]),
                    crate::app::status_tags::text_color_on(fill),
                    false,
                );
                x += width + 3.0;
                shown += 1;
            }
            if shown < current.len() {
                ui.painter().text(
                    egui::pos2(x + 2.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("+{}", current.len() - shown),
                    egui::FontId::proportional((text_height * 0.85).max(9.0)),
                    ui.visuals().weak_text_color(),
                );
            }
        }
        if response.hovered() {
            ui.painter().rect_stroke(
                rect,
                4.0,
                egui::Stroke::new(1.0, ui.visuals().widgets.hovered.bg_stroke.color),
                egui::StrokeKind::Inside,
            );
        }
        if response.clicked() {
            outcome.interacted_with_control = true;
        }
        // `clicked()` covers the frame the menu opens on: the popup's own
        // open flag is not set until `show` runs, just below.
        let open = response.clicked()
            || egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
        let defs = if open {
            self.tag_palette.defs.clone()
        } else {
            Vec::new()
        };
        let mut toggled: Option<(String, bool)> = None;
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(160.0);
            if defs.is_empty() {
                ui.label(egui::RichText::new("No tags defined yet").weak());
            }
            for def in &defs {
                let on = current.iter().any(|id| id == &*def.id);
                if Self::label_menu_entry(ui, def, on).clicked() {
                    toggled = Some((def.id.to_string(), !on));
                    // Deliberately left open: tagging is usually more than one
                    // tag at a time, and reopening the menu for each is worse.
                }
            }
            ui.separator();
            if ui.button("Edit Tags...").clicked() {
                outcome.open_manager = Some(true);
                ui.close();
            }
        });
        if let Some((id, on)) = toggled {
            let targets = self.label_edit_targets(path);
            self.set_tag_for_paths(&targets, &id, on);
            outcome.interacted_with_control = true;
        }
        outcome
    }

    /// One picker row: a color chip, the label, and a check when it is set.
    pub(in crate::app) fn label_menu_entry(
        ui: &mut egui::Ui,
        def: &crate::app::status_tags::LabelDef,
        selected: bool,
    ) -> egui::Response {
        let text = format!("{} {}", if selected { "\u{2713}" } else { " " }, def.label);
        let response = ui.selectable_label(selected, text);
        let chip = egui::Rect::from_center_size(
            egui::pos2(response.rect.right() - 10.0, response.rect.center().y),
            egui::vec2(10.0, 10.0),
        );
        ui.painter().rect_filled(chip, 2.0, def.color32());
        response
    }
}
