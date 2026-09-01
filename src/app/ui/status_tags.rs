//! The Statuses & Tags manager: where the two palettes are edited.
//!
//! Modelled on `ui/list_columns.rs`, including the floating-scroll-surface
//! registration that keeps the wheel and the list's keys from fighting over
//! an open dialog.

use egui::{Align, Color32, RichText};

use crate::app::status_tags::LabelPalette;

/// A palette edit the row loop asks for, applied after the loop so it is not
/// mutating the palette it is iterating.
enum PaletteEdit {
    Rename(usize, String),
    Color(usize, [u8; 3]),
    Move(usize, usize),
    MakeDefault(Option<usize>),
    Delete(usize),
}

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn open_status_tags_window(&mut self, on_tags: bool) {
        self.show_status_tags_window = true;
        self.status_tags_window_on_tags = on_tags;
    }

    pub(in crate::app) fn ui_status_tags_window(&mut self, ctx: &egui::Context) {
        if !self.show_status_tags_window {
            return;
        }
        let viewport = ctx.content_rect();
        let max_size = egui::vec2(
            (viewport.width() - 32.0).max(1.0),
            (viewport.height() - 32.0).max(1.0),
        );
        let default_size = egui::vec2(560.0_f32.min(max_size.x), 560.0_f32.min(max_size.y));
        let centered_pos = viewport.center() - default_size * 0.5;
        let requested_pos = self
            .status_tags_window_pos
            .filter(|pos| pos.x.is_finite() && pos.y.is_finite())
            .unwrap_or(centered_pos);
        let mut open = self.show_status_tags_window;
        let scroll_target = self.begin_floating_scroll_surface("status_tags_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let shown = egui::Window::new("Statuses & Tags")
            .open(&mut open)
            .collapsible(false)
            .movable(true)
            .resizable(true)
            .current_pos(requested_pos)
            .default_size(default_size)
            .min_size(egui::vec2(420.0, 300.0))
            .max_size(max_size)
            .constrain_to(viewport)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let mut on_tags = self.status_tags_window_on_tags;
                    ui.selectable_value(&mut on_tags, false, "Statuses");
                    ui.selectable_value(&mut on_tags, true, "Tags");
                    self.status_tags_window_on_tags = on_tags;
                });
                ui.separator();
                let on_tags = self.status_tags_window_on_tags;
                ui.label(
                    RichText::new(if on_tags {
                        "Tags are free-form labels; a row can carry any number of them."
                    } else {
                        "A row carries one status. The default is stamped on rows as they are \
                         added to the list."
                    })
                    .weak(),
                );
                ui.add_space(4.0);
                self.ui_label_palette_editor(ui, on_tags);
                ui.add_space(6.0);
                ui.separator();
                self.ui_label_palette_footer(ui, on_tags);
            });
        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
            let position = shown.response.rect.min;
            self.status_tags_window_pos = Some(position);
            let global_position_changed = self.project_path.is_none()
                && self.status_tags_window_global_pos.is_none_or(|saved| {
                    (saved.x - position.x).abs() > 0.5 || (saved.y - position.y).abs() > 0.5
                });
            if global_position_changed && ctx.input(|input| input.pointer.any_released()) {
                self.status_tags_window_global_pos = Some(position);
                self.save_prefs();
            }
        } else {
            open = false;
        }
        self.show_status_tags_window = open;
        self.ui_status_tags_delete_confirm(ctx);
    }

    fn ui_label_palette_editor(&mut self, ui: &mut egui::Ui, on_tags: bool) {
        let defs = self.label_palette(on_tags).defs.clone();
        let default_id = self.default_status.as_deref().map(str::to_string);
        // One pass over the list for the whole palette, not one per row below.
        let usage: std::collections::HashMap<String, usize> = self
            .label_usage_counts(on_tags)
            .into_iter()
            .map(|(id, count)| (id.to_string(), count))
            .collect();
        let last = defs.len().saturating_sub(1);
        let mut edit: Option<PaletteEdit> = None;

        egui::ScrollArea::vertical()
            .id_salt(("status_tags_editor", on_tags))
            .auto_shrink([false, false])
            .max_height(ui.available_height() - 96.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if defs.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(if on_tags {
                            "No tags yet. Add one below."
                        } else {
                            "No statuses yet. Add one below."
                        })
                        .weak(),
                    );
                }
                for (index, def) in defs.iter().enumerate() {
                    let usage = usage.get(&*def.id).copied().unwrap_or(0);
                    egui::Frame::NONE
                        .fill(ui.visuals().faint_bg_color)
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(8, 6))
                        .show(ui, |ui| {
                            ui.push_id(("label_def", on_tags, def.id.to_string()), |ui| {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(index > 0, egui::Button::new("▲"))
                                        .on_hover_text("Move up")
                                        .clicked()
                                    {
                                        edit = Some(PaletteEdit::Move(index, index - 1));
                                    }
                                    if ui
                                        .add_enabled(index < last, egui::Button::new("▼"))
                                        .on_hover_text("Move down")
                                        .clicked()
                                    {
                                        edit = Some(PaletteEdit::Move(index, index + 1));
                                    }

                                    let mut color = def.color32();
                                    if ui.color_edit_button_srgba(&mut color).changed() {
                                        edit = Some(PaletteEdit::Color(
                                            index,
                                            [color.r(), color.g(), color.b()],
                                        ));
                                    }

                                    let mut label = def.label.clone();
                                    let width = (ui.available_width() - 210.0).max(90.0);
                                    if ui
                                        .add_sized(
                                            [width, 22.0],
                                            egui::TextEdit::singleline(&mut label),
                                        )
                                        .changed()
                                    {
                                        edit = Some(PaletteEdit::Rename(index, label));
                                    }

                                    ui.with_layout(
                                        egui::Layout::right_to_left(Align::Center),
                                        |ui| {
                                            if ui
                                                .button("🗑")
                                                .on_hover_text("Delete this label")
                                                .clicked()
                                            {
                                                edit = Some(PaletteEdit::Delete(index));
                                            }
                                            ui.label(
                                                RichText::new(match usage {
                                                    0 => "unused".to_string(),
                                                    1 => "1 row".to_string(),
                                                    n => format!("{n} rows"),
                                                })
                                                .small()
                                                .weak(),
                                            );
                                            if !on_tags {
                                                let is_default =
                                                    default_id.as_deref() == Some(&*def.id);
                                                if ui
                                                    .radio(is_default, "Default")
                                                    .on_hover_text(
                                                        "Stamp this status on rows as they are \
                                                         added to the list",
                                                    )
                                                    .clicked()
                                                {
                                                    edit = Some(PaletteEdit::MakeDefault(
                                                        (!is_default).then_some(index),
                                                    ));
                                                }
                                            }
                                        },
                                    );
                                });
                                // The id is what the session stores, so it is
                                // worth seeing when comparing two machines.
                                ui.label(RichText::new(format!("id: {}", def.id)).small().weak());
                            });
                        });
                    ui.add_space(4.0);
                }
            });

        if let Some(edit) = edit {
            self.apply_palette_edit(on_tags, edit, &defs);
        }
    }

    fn apply_palette_edit(
        &mut self,
        on_tags: bool,
        edit: PaletteEdit,
        defs: &[crate::app::status_tags::LabelDef],
    ) {
        match edit {
            PaletteEdit::Rename(index, label) => {
                if let Some(def) = defs.get(index) {
                    let id = def.id.to_string();
                    self.label_palette_mut(on_tags).rename(&id, &label);
                }
            }
            PaletteEdit::Color(index, color) => {
                if let Some(def) = defs.get(index) {
                    let id = def.id.to_string();
                    self.label_palette_mut(on_tags).set_color(&id, color);
                }
            }
            PaletteEdit::Move(from, to) => {
                self.label_palette_mut(on_tags).move_def(from, to);
            }
            PaletteEdit::MakeDefault(index) => {
                self.default_status = index
                    .and_then(|index| defs.get(index))
                    .map(|def| std::sync::Arc::clone(&def.id));
            }
            PaletteEdit::Delete(index) => {
                if let Some(def) = defs.get(index) {
                    let usage = self.label_usage_count(on_tags, &def.id);
                    if usage == 0 {
                        // Nothing to lose, so no confirmation to sit through.
                        self.remove_label_def(on_tags, def.id.as_ref());
                        return;
                    }
                    self.status_tags_pending_delete =
                        Some((on_tags, std::sync::Arc::clone(&def.id), usage));
                    return;
                }
            }
        }
        self.save_label_palette_prefs();
    }

    fn ui_label_palette_footer(&mut self, ui: &mut egui::Ui, on_tags: bool) {
        ui.horizontal(|ui| {
            if ui
                .button(if on_tags { "+ Add Tag" } else { "+ Add Status" })
                .clicked()
            {
                let n = self.label_palette(on_tags).len();
                let label = format!("{} {}", if on_tags { "Tag" } else { "Status" }, n + 1);
                let color = next_palette_color(self.label_palette(on_tags));
                self.label_palette_mut(on_tags).add(&label, color);
                self.save_label_palette_prefs();
            }
            if !on_tags
                && ui
                    .add_enabled(
                        self.default_status.is_some(),
                        egui::Button::new("No default"),
                    )
                    .on_hover_text("Stop stamping a status on newly added rows")
                    .clicked()
            {
                self.default_status = None;
                self.save_label_palette_prefs();
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            // The palette lives in two places on purpose: the session carries
            // the one it was authored against so a shared `.nwsess` reads
            // correctly on someone else's machine, and prefs holds the set
            // this machine starts new work from. These two buttons are how
            // one becomes the other.
            if ui
                .button("Save as global default")
                .on_hover_text("Copy this palette into your app preferences")
                .clicked()
            {
                self.save_prefs();
                self.push_toast(
                    crate::app::types::ToastSeverity::Info,
                    "Saved the current statuses and tags as your global default".to_string(),
                );
            }
            if ui
                .button("Load global default")
                .on_hover_text("Replace this session's palette with your saved one")
                .clicked()
            {
                self.load_label_palettes_from_prefs();
            }
        });
        ui.add_space(2.0);
        ui.label(
            RichText::new(
                "Saved with the session, so a shared .nwsess keeps these labels and colors.",
            )
            .small()
            .weak(),
        );
    }

    fn ui_status_tags_delete_confirm(&mut self, ctx: &egui::Context) {
        let Some((on_tags, id, usage)) = self.status_tags_pending_delete.clone() else {
            return;
        };
        let label = self.label_palette(on_tags).label_for(&id);
        let mut decision: Option<bool> = None;
        egui::Modal::new(egui::Id::new("status_tags_delete_confirm")).show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.heading(if on_tags {
                "Delete tag"
            } else {
                "Delete status"
            });
            ui.add_space(6.0);
            ui.label(format!(
                "\"{label}\" is used by {usage} {}. Deleting it removes it from {} as well.",
                if usage == 1 { "row" } else { "rows" },
                if usage == 1 { "that row" } else { "those rows" },
            ));
            ui.label(
                RichText::new("This can be undone with Ctrl+Z.")
                    .small()
                    .weak(),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
                if ui
                    .button(RichText::new("Delete").color(Color32::from_rgb(230, 120, 120)))
                    .clicked()
                {
                    decision = Some(true);
                }
            });
        });
        match decision {
            Some(true) => {
                self.remove_label_def(on_tags, &id);
                self.status_tags_pending_delete = None;
            }
            Some(false) => self.status_tags_pending_delete = None,
            None => {}
        }
    }
}

/// Colors handed out to new labels, cycling so a fresh palette does not come
/// out all one shade.
fn next_palette_color(palette: &LabelPalette) -> [u8; 3] {
    const WHEEL: [[u8; 3]; 8] = [
        [78, 132, 210],
        [76, 160, 96],
        [212, 152, 56],
        [196, 74, 74],
        [156, 106, 200],
        [72, 164, 168],
        [204, 116, 160],
        [110, 116, 128],
    ];
    WHEEL[palette.len() % WHEEL.len()]
}
