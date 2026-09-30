//! Multi Edits picture windows: one native window per video track whose
//! "Video" toggle is on, showing that track's clip at the playhead. The same
//! shape as the editor's detached viewer (`video_viewport.rs`) -- a separate
//! OS window, the editor's frame ring and decode worker -- one per track.

use crate::app::render::video_panel;
use crate::app::WavesPreviewer;

/// A picture window's first size and the least it shrinks to (16:9).
const VIDEO_WINDOW_SIZE: [f32; 2] = [640.0, 360.0];
const VIDEO_WINDOW_MIN_SIZE: [f32; 2] = [320.0, 180.0];

impl WavesPreviewer {
    pub(super) fn multi_edit_video_viewport_id(doc_id: &str, track_id: &str) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("multi_edit_video", doc_id, track_id))
    }

    /// Draw every open picture window of the active timeline. Closing one
    /// turns its track's toggle off.
    pub(in crate::app) fn ui_multi_edit_video_windows(&mut self, ctx: &egui::Context) {
        if !self.is_multi_edit_workspace_active() {
            return;
        }
        let Some(doc_id) = self.multi_edit.active.clone() else {
            return;
        };
        let windows = self.multi_edit_video_windows();
        // Panels of tracks no longer shown go, and their workers with them.
        self.multi_edit
            .video_panels
            .retain(|track_id, _| windows.iter().any(|(id, _, _)| id == track_id));
        let playing = self.multi_edit_is_playing(&doc_id);
        for (track_id, title, target) in windows {
            let viewport_id = Self::multi_edit_video_viewport_id(&doc_id, &track_id);
            let builder = egui::ViewportBuilder::default()
                .with_title(title.clone())
                .with_inner_size(VIDEO_WINDOW_SIZE)
                .with_min_inner_size(VIDEO_WINDOW_MIN_SIZE)
                .with_resizable(true);
            let mut close_requested = false;
            match target.as_ref() {
                Some((path, secs)) => {
                    let secs = *secs;
                    let video = self.multi_edit_video_panel(&track_id, path);
                    let id = video.id;
                    let panel = &mut video.panel;
                    ctx.show_viewport_immediate(viewport_id, builder, |ui, _class| {
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
                            id,
                            panel,
                            rect,
                            secs,
                            false,
                            "multi_edit_video",
                        );
                    });
                    if !close_requested {
                        self.multi_edit_request_video(&track_id, secs, playing);
                    }
                }
                None => {
                    ctx.show_viewport_immediate(viewport_id, builder, |ui, _class| {
                        if ui.ctx().input(|input| input.viewport().close_requested()) {
                            close_requested = true;
                            return;
                        }
                        let rect = ui.max_rect();
                        ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "No video on this track at the playhead",
                            egui::FontId::proportional(13.0),
                            egui::Color32::from_gray(140),
                        );
                    });
                }
            }
            // In front of the app, as the editor's viewer is.
            self.window_owner.keep_in_front(viewport_id, &title);
            if close_requested {
                self.multi_edit_set_show_video(&track_id, false);
            }
        }
    }
}
