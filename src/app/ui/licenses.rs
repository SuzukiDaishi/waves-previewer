use crate::app::licenses::{self, Component, Flag, FlaggedTopic};

impl crate::app::WavesPreviewer {
    /// Help -> Licenses: the third-party notices this build is obliged to show,
    /// plus the handful of entries that need a decision before the binary is
    /// sold rather than given away.
    ///
    /// The obligations come first and the six hundred uneventful MIT crates
    /// come last, because a notices screen nobody can navigate satisfies the
    /// letter of the licences and nothing else.
    pub(crate) fn ui_licenses_window(&mut self, ctx: &egui::Context) {
        if !self.show_licenses_window {
            return;
        }
        let mut open = true;
        let scroll_target = self.begin_floating_scroll_surface("licenses_window");
        let scroll_guard = self.pointer_scroll_input_guard(scroll_target, ctx);
        let manifest = licenses::manifest();
        let filter = self.licenses_filter.trim().to_lowercase();

        let shown = egui::Window::new("Licenses")
            .open(&mut open)
            .default_width(720.0)
            .default_height(620.0)
            .vscroll(true)
            .show(ctx, |ui| {
                ui.heading("NeoWaves");
                ui.label(format!(
                    "MIT License. Snapshot of {} third-party components, generated {}.",
                    manifest.components.len(),
                    manifest.generated_at
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button("Copy all")
                        .on_hover_text(
                            "Copy every notice and licence text, ready to paste into a \
                             NOTICES file that ships with a build",
                        )
                        .clicked()
                    {
                        ctx.copy_text(licenses::plain_text());
                    }
                    ui.label(
                        egui::RichText::new(
                            "Regenerate with commands/generate_licenses.ps1 after a dependency change.",
                        )
                        .small()
                        .weak(),
                    );
                });

                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Licences in this build")
                        .strong()
                        .color(ui.visuals().weak_text_color()),
                );
                // Click a licence to filter the table by it -- the fastest way
                // to answer "what is the MPL-2.0 in here?".
                ui.horizontal_wrapped(|ui| {
                    for (id, count) in manifest.license_id_counts() {
                        if ui
                            .small_button(format!("{id} ×{count}"))
                            .on_hover_text("Filter the list by this licence")
                            .clicked()
                        {
                            self.licenses_filter = id.to_string();
                        }
                    }
                });

                ui.add_space(10.0);
                ui.separator();

                let flagged = manifest.flagged_topics();
                if !flagged.is_empty() {
                    ui.heading("Commercial distribution notes");
                    ui.label(
                        egui::RichText::new(format!(
                            "Everything below is fine to use. {} of the {} components carry an \
                             obligation beyond attribution — grouped here into {} points worth \
                             reading before shipping a build commercially.",
                            manifest.flagged_count(),
                            manifest.components.len(),
                            flagged.len(),
                        ))
                        .small()
                        .weak(),
                    );
                    ui.add_space(6.0);
                    for topic in flagged {
                        self.ui_license_flag_row(ui, &topic);
                    }
                    ui.add_space(10.0);
                    ui.separator();
                }

                ui.heading("All components");
                ui.horizontal(|ui| {
                    ui.label("Search:");
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.licenses_filter)
                            .hint_text("name, licence, author")
                            .desired_width(240.0),
                    );
                    if response.changed() {
                        // Nothing to recompute -- the filter is applied below --
                        // but a repaint keeps the list in step with the caret.
                        ctx.request_repaint();
                    }
                    if ui.button("Clear").clicked() {
                        self.licenses_filter.clear();
                    }
                });
                ui.add_space(6.0);

                let mut shown_rows = 0usize;
                for kind in manifest.kinds() {
                    let rows: Vec<&Component> = manifest
                        .components
                        .iter()
                        .filter(|c| c.kind == kind && c.matches(&filter))
                        .collect();
                    if rows.is_empty() {
                        continue;
                    }
                    shown_rows += rows.len();
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} ({})",
                            licenses::kind_title(kind),
                            rows.len()
                        ))
                        .strong()
                        .color(ui.visuals().weak_text_color()),
                    );
                    for component in rows {
                        Self::ui_license_component(ui, component);
                    }
                }

                if shown_rows == 0 {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!("Nothing matches \"{}\".", self.licenses_filter))
                            .weak(),
                    );
                }
            });

        drop(scroll_guard);
        if let Some(shown) = shown.as_ref() {
            self.register_scroll_surface(scroll_target, &shown.response);
        }
        self.show_licenses_window = open;
    }

    /// One issue in the summary at the top: the flag, what it is, why it is
    /// listed, and which other components it covers. The full licence text
    /// stays in the table below.
    fn ui_license_flag_row(&self, ui: &mut egui::Ui, topic: &FlaggedTopic<'_>) {
        let component = topic.primary;
        let flag = match component.flag {
            Some(flag) => flag,
            None => return,
        };
        let colour = match flag {
            Flag::Caution => ui.visuals().error_fg_color,
            Flag::Notice => ui.visuals().warn_fg_color,
        };
        ui.horizontal_top(|ui| {
            ui.label(
                egui::RichText::new(flag.label())
                    .small()
                    .strong()
                    .color(colour),
            );
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} — {}", component.name, component.license_expr))
                        .strong(),
                );
                if let Some(note) = &component.note {
                    ui.label(egui::RichText::new(note).small());
                }
                if !topic.also.is_empty() {
                    let names: Vec<&str> = topic.also.iter().map(|c| c.name.as_str()).collect();
                    ui.label(
                        egui::RichText::new(format!("Also covers: {}", names.join(", ")))
                            .small()
                            .weak(),
                    );
                }
            });
        });
        ui.add_space(6.0);
    }

    /// A collapsing row per component: the header carries name, version and
    /// licence, and expanding it reveals the attribution and the full text of
    /// every licence that component is covered by.
    fn ui_license_component(ui: &mut egui::Ui, component: &Component) {
        let manifest = licenses::manifest();
        let mut header = egui::RichText::new(format!(
            "{} {} — {}",
            component.name, component.version, component.license_expr
        ));
        if let Some(flag) = component.flag {
            header = header.color(match flag {
                Flag::Caution => ui.visuals().error_fg_color,
                Flag::Notice => ui.visuals().warn_fg_color,
            });
        }

        egui::CollapsingHeader::new(header)
            .id_salt(("license_component", &component.name, &component.version))
            .show(ui, |ui| {
                if !component.authors.is_empty() {
                    ui.label(egui::RichText::new(&component.authors).small().weak());
                }
                if !component.repository.is_empty() {
                    ui.hyperlink(&component.repository);
                }
                if let Some(note) = &component.note {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(note).small());
                }
                if component.license_keys.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("No licence text recorded for this component.")
                            .small()
                            .color(ui.visuals().warn_fg_color),
                    );
                }
                for key in &component.license_keys {
                    let Some(license) = manifest.license(key) else {
                        continue;
                    };
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!("{} [{}]", license.name, license.id))
                            .small()
                            .strong(),
                    );
                    // Horizontal only: licence texts are hard wrapped at 80
                    // columns and reflowing them mangles the section headings.
                    // Vertical scrolling stays with the window -- a nested
                    // vertical scroll area here would fight it for the wheel.
                    egui::ScrollArea::horizontal()
                        .id_salt(("license_text", &component.name, key))
                        .show(ui, |ui| {
                            ui.monospace(&license.text);
                        });
                }
            });
    }
}
