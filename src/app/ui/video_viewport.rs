use crate::app::render::video_panel;
use crate::app::{PlaybackSourceKind, WavesPreviewer};

impl WavesPreviewer {
    pub(super) fn detached_video_viewport_id(tab_id: u64) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("detached_video", tab_id))
    }

    /// Select the single tab shown by the detached viewer. Switching targets
    /// releases the previous tab's large decode request immediately.
    pub(super) fn open_detached_video_for_tab(&mut self, tab_id: u64, ctx: &egui::Context) {
        if self.detached_video_tab_id != Some(tab_id) {
            if let Some(previous_id) = self.detached_video_tab_id {
                if let Some(panel) = self
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.tab_id == previous_id)
                    .and_then(|tab| tab.video_panel.as_mut())
                {
                    panel.detached_wanted_box_px = (0, 0);
                }
            }
            self.detached_video_tab_id = Some(tab_id);
        }
        ctx.send_viewport_cmd_to(
            Self::detached_video_viewport_id(tab_id),
            egui::ViewportCommand::Focus,
        );
        ctx.request_repaint();
    }

    /// Render the one native video-only viewport. Its frame ring, GPU texture,
    /// decoder worker and clock all belong to the original editor tab.
    pub(in crate::app) fn ui_detached_video_viewport(&mut self, ctx: &egui::Context) {
        let Some(tab_id) = self.detached_video_tab_id else {
            return;
        };
        let Some(tab_idx) = self.tabs.iter().position(|tab| tab.tab_id == tab_id) else {
            self.detached_video_tab_id = None;
            return;
        };
        let path = self.tabs[tab_idx].path.clone();
        let Some(frozen_secs) = self.tabs[tab_idx]
            .video_panel
            .as_ref()
            .map(|panel| panel.shown_pts.unwrap_or(0.0))
        else {
            self.detached_video_tab_id = None;
            return;
        };

        // A matching audio source is the only live clock. When another list
        // row or editor tab owns transport, this window keeps the last frame
        // that was synchronized to its own source and submits no decode work.
        let source_secs = self.playback_current_source_time_sec();
        let source_matches = match &self.playback_session.source {
            PlaybackSourceKind::ListPreview(source) | PlaybackSourceKind::EditorTab(source) => {
                source == &path
            }
            PlaybackSourceKind::None
            | PlaybackSourceKind::EffectGraph
            | PlaybackSourceKind::ToolPreview => false,
        } && source_secs.is_some();
        let video_secs = if source_matches {
            source_secs.unwrap_or(frozen_secs)
        } else {
            frozen_secs
        };
        let playing = source_matches && self.playback_is_playing_now();
        let title = format!(
            "NeoWaves Video — {}",
            path.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_else(|| path.as_os_str().to_string_lossy())
        );
        let viewport_id = Self::detached_video_viewport_id(tab_id);
        let builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([960.0, 540.0])
            .with_min_inner_size([320.0, 180.0])
            .with_resizable(true);
        let mut close_requested = false;

        {
            let panel = self.tabs[tab_idx]
                .video_panel
                .as_mut()
                .expect("video panel checked above");
            ctx.show_viewport_immediate(viewport_id, builder, |ui, _viewport_class| {
                if ui.ctx().input(|input| input.viewport().close_requested()) {
                    close_requested = true;
                    return;
                }
                let rect = ui.max_rect();
                ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
                panel.detached_wanted_box_px = video_panel::detached_frame_box_px(
                    rect.shrink(2.0),
                    panel.info.aspect(),
                    ui.ctx().pixels_per_point(),
                );
                Self::paint_video_surface(
                    ui,
                    tab_id,
                    panel,
                    rect,
                    video_secs,
                    false,
                    "detached_video_panel",
                );
            });
        }

        if close_requested {
            if let Some(panel) = self.tabs[tab_idx].video_panel.as_mut() {
                panel.detached_wanted_box_px = (0, 0);
            }
            self.detached_video_tab_id = None;
            return;
        }
        if source_matches {
            self.request_video_frame_for_tab(tab_idx, video_secs, playing);
        }
    }
}
