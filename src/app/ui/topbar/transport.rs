use crate::app::types::RateMode;
use crate::app::WavesPreviewer;

impl WavesPreviewer {
    pub(super) fn ui_topbar_transport_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            self.ui_topbar_playback_mode_controls(ui, ctx);
            ui.separator();
            self.ui_topbar_playback_controls(ui);
            ui.separator();
            self.ui_topbar_search_controls(ui, ctx);
        });
    }

    fn ui_topbar_playback_mode_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.scope(|ui| {
            let s = ui.style_mut();
            s.spacing.item_spacing.x = 6.0;
            s.spacing.button_padding = egui::vec2(4.0, 2.0);
            ui.label("Mode");
            let prev_mode = self.mode;
            let prev_rate = self.playback_rate;
            for (mode, label) in [
                (RateMode::Speed, "Speed"),
                (RateMode::PitchShift, "Pitch"),
                (RateMode::TimeStretch, "Stretch"),
            ] {
                if ui.selectable_label(self.mode == mode, label).clicked() {
                    self.mode = mode;
                }
            }
            if self.mode != prev_mode {
                self.refresh_playback_mode_for_current_source(prev_mode, prev_rate);
            }
            match self.mode {
                RateMode::Speed => {
                    let prev_rate = self.playback_rate;
                    let resp = ui.add(
                        egui::DragValue::new(&mut self.playback_rate)
                            .range(0.25..=4.0)
                            .speed(0.05)
                            .fixed_decimals(2)
                            .suffix(" x"),
                    );
                    if resp.changed() {
                        self.refresh_playback_mode_for_current_source(RateMode::Speed, prev_rate);
                    }
                    self.ui_topbar_handle_numeric_drag_response(ctx, &resp);
                }
                RateMode::PitchShift => {
                    let prev_rate = self.playback_rate;
                    let resp = ui.add(
                        egui::DragValue::new(&mut self.pitch_semitones)
                            .range(-12.0..=12.0)
                            .speed(0.1)
                            .fixed_decimals(1)
                            .suffix(" st"),
                    );
                    if resp.changed() {
                        self.refresh_playback_mode_for_current_source(
                            RateMode::PitchShift,
                            prev_rate,
                        );
                    }
                    self.ui_topbar_handle_numeric_drag_response(ctx, &resp);
                }
                RateMode::TimeStretch => {
                    let prev_rate = self.playback_rate;
                    let resp = ui.add(
                        egui::DragValue::new(&mut self.playback_rate)
                            .range(0.25..=4.0)
                            .speed(0.05)
                            .fixed_decimals(2)
                            .suffix(" x"),
                    );
                    if resp.changed() {
                        self.refresh_playback_mode_for_current_source(
                            RateMode::TimeStretch,
                            prev_rate,
                        );
                    }
                    self.ui_topbar_handle_numeric_drag_response(ctx, &resp);
                }
            }
        });
    }

    fn ui_topbar_handle_numeric_drag_response(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
    ) {
        let nav_up = if response.has_focus() && self.is_list_workspace_active() {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
        } else {
            false
        };
        let nav_down = if response.has_focus() && self.is_list_workspace_active() {
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
        } else {
            false
        };
        if nav_up || nav_down {
            let delta = if nav_down { 1 } else { -1 };
            self.ui_topbar_release_focus_to_list(ctx, response, Some(delta));
        }
        if response.has_focus()
            && ctx.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape))
        {
            self.ui_topbar_release_focus_to_list(ctx, response, None);
        }
    }

    fn ui_topbar_playback_controls(&mut self, ui: &mut egui::Ui) {
        let playing = self
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        let play_text = if playing {
            "Pause (Space)"
        } else {
            "Play (Space)"
        };
        let play_enabled =
            !self.is_editor_workspace_active() || self.active_editor_exact_audio_ready() || playing;
        if ui
            .add_enabled(
                play_enabled,
                egui::Button::new(play_text).min_size(egui::vec2(110.0, 22.0)),
            )
            .clicked()
        {
            self.request_workspace_play_toggle();
        }
        ui.checkbox(&mut self.auto_play_list_nav, "Auto Play");
    }

    fn ui_topbar_search_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let regex_changed = ui.checkbox(&mut self.search_use_regex, "Regex").changed();
        let te = egui::TextEdit::singleline(&mut self.search_query)
            .hint_text("Search...")
            .id(crate::app::WavesPreviewer::search_box_id());
        let resp = ui.add(te);
        let search_focused = resp.has_focus();
        let search_lost = resp.lost_focus();
        self.search_has_focus = search_focused;
        if search_focused {
            self.suppress_list_enter = true;
            self.list_has_focus = false;
        }
        if search_lost
            && ctx.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape))
        {
            self.suppress_list_enter = true;
        }
        if resp.has_focus()
            && ctx.input(|i| {
                i.key_pressed(egui::Key::ArrowUp)
                    || i.key_pressed(egui::Key::ArrowDown)
                    || i.key_pressed(egui::Key::Escape)
            })
        {
            resp.surrender_focus();
            if self.is_list_workspace_active() {
                ctx.memory_mut(|m| m.request_focus(crate::app::WavesPreviewer::list_focus_id()));
                self.list_has_focus = true;
            }
            self.search_has_focus = false;
            if self.debug.cfg.enabled {
                self.debug_trace_input("search focus released via arrow/escape");
            }
        }
        if resp.changed() {
            self.schedule_search_refresh();
        }
        if regex_changed {
            self.ui_topbar_apply_search_now();
        }
        if resp.has_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.ui_topbar_apply_search_now();
        }
        if !self.search_query.is_empty() && ui.button("x").on_hover_text("Clear").clicked() {
            self.search_query.clear();
            self.ui_topbar_apply_search_now();
        }
    }
}
