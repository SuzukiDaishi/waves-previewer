use egui::RichText;

impl crate::app::WavesPreviewer {
    /// Popup for the transient harmonic action (Ctrl+click in Spec/Log):
    /// adjust the fundamental / harmonic count, then Mute or Attenuate all
    /// bands in one multi-band STFT pass over the time selection (whole
    /// file without one).
    pub(crate) fn ui_harmonic_window(&mut self, ctx: &egui::Context) {
        let Some(mut action) = self.harmonic_action else {
            return;
        };
        let tab_matches = self
            .active_tab
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.tab_id == action.tab_id)
            .unwrap_or(false);
        if !tab_matches {
            self.harmonic_action = None;
            return;
        }
        let mut open = true;
        let mut do_apply: Option<Option<f32>> = None;
        egui::Window::new("Harmonics")
            .open(&mut open)
            .default_width(280.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Bands at k x f0 (+/-3%) are highlighted in the spectral view. Applies to the time selection (whole file without one).",
                    )
                    .weak(),
                );
                ui.horizontal(|ui| {
                    ui.label("f0");
                    ui.add(
                        egui::DragValue::new(&mut action.f0)
                            .range(20.0..=8_000.0)
                            .speed(0.5)
                            .suffix(" Hz"),
                    );
                });
                ui.label("Harmonics");
                let mut harmonics = action.harmonics.clamp(1, 16);
                if ui.add(egui::Slider::new(&mut harmonics, 1..=16)).changed() {
                    action.harmonics = harmonics;
                }
                ui.label("Attenuate by");
                ui.add(
                    egui::Slider::new(&mut action.atten_db, 3.0..=60.0)
                        .suffix(" dB")
                        .fixed_decimals(0),
                );
                ui.horizontal(|ui| {
                    if ui.button("Mute").clicked() {
                        do_apply = Some(None);
                    }
                    if ui.button("Attenuate").clicked() {
                        do_apply = Some(Some(action.atten_db));
                    }
                    if ui.button("Cancel").clicked() {
                        self.harmonic_action = None;
                    }
                });
            });
        if self.harmonic_action.is_some() {
            self.harmonic_action = Some(action);
        }
        if !open {
            self.harmonic_action = None;
        }
        if let Some(atten) = do_apply {
            if let Some(tab_idx) = self.active_tab {
                self.editor_apply_harmonic_action(tab_idx, atten);
            }
        }
    }
}
