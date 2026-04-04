mod menus;
mod status;
mod transport;

impl crate::app::WavesPreviewer {
    pub(in crate::app) fn ui_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.vertical(|ui| {
                self.ui_topbar_menu_row(ui, ctx);
                self.ui_topbar_status_row(ui, ctx);
                self.ui_topbar_transport_row(ui, ctx);
            });
        });
    }

    fn ui_topbar_release_focus_to_list(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        delta: Option<isize>,
    ) {
        self.suppress_list_enter = true;
        response.surrender_focus();
        ctx.memory_mut(|m| m.stop_text_input());
        if let Some(delta) = delta {
            self.request_list_focus(ctx);
            self.nudge_list_selection(delta, true);
        }
    }

    fn ui_topbar_apply_search_now(&mut self) {
        self.apply_filter_from_search();
        if self.sort_dir != crate::app::types::SortDir::None {
            self.apply_sort();
        }
        self.search_dirty = false;
        self.search_deadline = None;
    }
}
