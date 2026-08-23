//! The list's Wave column: the row waveform, its marker/loop overlay, and the
//! transport surface layered on top of it.
//!
//! Split out of `ui_list_view`'s column match, which had this inlined amongst
//! twenty other cells.

use std::path::Path;

use egui::{Color32, Sense};

use crate::app::helpers::{amp_to_color, db_to_amp};
use crate::app::list_seek_ops::{ListSeekPhase, ListSeekRequest, ListWavePlayheadInfo};

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ListWaveOverlayInfo {
    pub(crate) marker_fracs: Vec<f32>,
    pub(crate) loop_frac: Option<(f32, f32)>,
    pub(crate) dirty: bool,
}

impl crate::app::WavesPreviewer {
    fn normalized_list_wave_marker_fracs(marker_fracs: &[f32]) -> Vec<f32> {
        let mut fracs: Vec<f32> = marker_fracs
            .iter()
            .copied()
            .filter(|frac| frac.is_finite())
            .map(|frac| frac.clamp(0.0, 1.0))
            .collect();
        fracs.sort_by(|a, b| a.total_cmp(b));
        fracs.dedup_by(|a, b| (*a - *b).abs() <= f32::EPSILON);
        fracs
    }

    fn normalized_list_wave_loop_frac(loop_frac: Option<(f32, f32)>) -> Option<(f32, f32)> {
        let (a, b) = loop_frac?;
        if !a.is_finite() || !b.is_finite() {
            return None;
        }
        Some(if a <= b {
            (a.clamp(0.0, 1.0), b.clamp(0.0, 1.0))
        } else {
            (b.clamp(0.0, 1.0), a.clamp(0.0, 1.0))
        })
    }

    fn build_list_wave_overlay_info(
        marker_fracs: Vec<f32>,
        loop_frac: Option<(f32, f32)>,
        dirty: bool,
    ) -> ListWaveOverlayInfo {
        ListWaveOverlayInfo {
            marker_fracs: Self::normalized_list_wave_marker_fracs(&marker_fracs),
            loop_frac: Self::normalized_list_wave_loop_frac(loop_frac),
            dirty,
        }
    }

    fn build_list_wave_overlay_from_live_state(
        markers: &[crate::markers::MarkerEntry],
        loop_region: Option<(usize, usize)>,
        samples_len: usize,
        dirty: bool,
    ) -> ListWaveOverlayInfo {
        if samples_len == 0 {
            return ListWaveOverlayInfo {
                dirty,
                ..ListWaveOverlayInfo::default()
            };
        }
        let denom = samples_len as f32;
        let marker_fracs = markers
            .iter()
            .map(|marker| (marker.sample as f32 / denom).clamp(0.0, 1.0))
            .collect();
        let loop_frac = loop_region.map(|(a, b)| {
            (
                (a as f32 / denom).clamp(0.0, 1.0),
                (b as f32 / denom).clamp(0.0, 1.0),
            )
        });
        Self::build_list_wave_overlay_info(marker_fracs, loop_frac, dirty)
    }

    pub(crate) fn resolve_list_wave_overlay_info(
        &self,
        path: &Path,
    ) -> Option<ListWaveOverlayInfo> {
        if let Some(tab) = self.tabs.iter().find(|tab| tab.path.as_path() == path) {
            return Some(Self::build_list_wave_overlay_from_live_state(
                &tab.markers,
                tab.loop_region,
                tab.samples_len,
                tab.markers_dirty || tab.loop_markers_dirty,
            ));
        }
        if let Some(cached) = self.edited_cache.get(path) {
            return Some(Self::build_list_wave_overlay_from_live_state(
                &cached.markers,
                cached.loop_region,
                cached.samples_len,
                cached.markers_dirty || cached.loop_markers_dirty,
            ));
        }
        self.item_for_path(path).and_then(|item| {
            item.meta.as_ref().map(|meta| {
                Self::build_list_wave_overlay_info(meta.marker_fracs.clone(), meta.loop_frac, false)
            })
        })
    }

    pub(crate) fn coalesce_list_wave_marker_fracs(marker_fracs: &[f32], width_px: f32) -> Vec<f32> {
        if marker_fracs.is_empty() || !width_px.is_finite() || width_px <= 0.0 {
            return Vec::new();
        }
        let columns = width_px.floor().max(1.0) as usize;
        let mut out = Vec::new();
        let mut last_col: Option<usize> = None;
        for frac in Self::normalized_list_wave_marker_fracs(marker_fracs) {
            let col = if columns <= 1 {
                0
            } else {
                (frac * (columns.saturating_sub(1) as f32)).round() as usize
            };
            if Some(col) == last_col {
                continue;
            }
            last_col = Some(col);
            out.push(frac);
        }
        out
    }

    fn paint_list_wave_overlay(
        &self,
        ui: &mut egui::Ui,
        wave_rect: egui::Rect,
        overlay: &ListWaveOverlayInfo,
    ) {
        if let Some((start_frac, end_frac)) = overlay.loop_frac {
            let start_x = wave_rect.left() + start_frac.clamp(0.0, 1.0) * wave_rect.width();
            let end_x = wave_rect.left() + end_frac.clamp(0.0, 1.0) * wave_rect.width();
            let palette = self.palette();
            let band_fill = if overlay.dirty {
                palette.selection_fill
            } else {
                palette.selection_fill_weak
            };
            let band_line = if overlay.dirty {
                palette.selection_stroke
            } else {
                palette.selection_stroke_weak
            };
            if start_x != end_x {
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(start_x.min(end_x), wave_rect.top()),
                        egui::pos2(start_x.max(end_x), wave_rect.bottom()),
                    ),
                    0.0,
                    band_fill,
                );
            }
            ui.painter().line_segment(
                [
                    egui::pos2(start_x, wave_rect.top()),
                    egui::pos2(start_x, wave_rect.bottom()),
                ],
                egui::Stroke::new(1.0, band_line),
            );
            ui.painter().line_segment(
                [
                    egui::pos2(end_x, wave_rect.top()),
                    egui::pos2(end_x, wave_rect.bottom()),
                ],
                egui::Stroke::new(1.0, band_line),
            );
        }
        let marker_fracs =
            Self::coalesce_list_wave_marker_fracs(&overlay.marker_fracs, wave_rect.width());
        let marker_color = if overlay.dirty {
            self.palette().attention_fill
        } else {
            self.palette().attention_fill_weak
        };
        for frac in marker_fracs {
            let x = wave_rect.left() + frac.clamp(0.0, 1.0) * wave_rect.width();
            ui.painter().line_segment(
                [
                    egui::pos2(x, wave_rect.top()),
                    egui::pos2(x, wave_rect.bottom()),
                ],
                egui::Stroke::new(1.0, marker_color),
            );
        }
    }

    /// Playback chrome layered over the row waveform: how far playback has got,
    /// how far the decode reaches, and where a parked seek is aiming.
    ///
    /// Separate from `paint_list_wave_overlay` on purpose. That one draws
    /// marker/loop geometry from a `PartialEq` struct used for change
    /// detection; this changes every frame while playing.
    fn paint_list_wave_playhead(
        &self,
        ui: &mut egui::Ui,
        wave_rect: egui::Rect,
        info: &ListWavePlayheadInfo,
        hover_frac: Option<f32>,
    ) {
        let x_at = |frac: f32| wave_rect.left() + frac.clamp(0.0, 1.0) * wave_rect.width();
        let palette = self.palette();

        // What has been played, so the cell reads as a progress bar at a glance
        // instead of relying on a 1px playhead being spotted in a 26px row.
        if let Some(frac) = info.play_frac {
            let end_x = x_at(frac);
            if end_x > wave_rect.left() {
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        wave_rect.left_top(),
                        egui::pos2(end_x, wave_rect.bottom()),
                    ),
                    0.0,
                    palette.playing_text.gamma_multiply(0.14),
                );
            }
        }

        // The undecoded tail cannot be seeked into immediately; shading it
        // explains the wait instead of leaving the click feeling ignored.
        if info.decoded_frac < 0.999 {
            let start_x = x_at(info.decoded_frac);
            if start_x < wave_rect.right() {
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(start_x, wave_rect.top()),
                        wave_rect.right_bottom(),
                    ),
                    0.0,
                    ui.style().visuals.extreme_bg_color.gamma_multiply(0.35),
                );
            }
        }

        if let Some(frac) = hover_frac {
            let x = x_at(frac);
            ui.painter().line_segment(
                [
                    egui::pos2(x, wave_rect.top()),
                    egui::pos2(x, wave_rect.bottom()),
                ],
                egui::Stroke::new(
                    1.0,
                    ui.style()
                        .visuals
                        .widgets
                        .hovered
                        .fg_stroke
                        .color
                        .gamma_multiply(0.5),
                ),
            );
        }

        // Where a seek is aiming while the decode catches up.
        if let Some(frac) = info.pending_frac {
            let x = x_at(frac);
            ui.painter().line_segment(
                [
                    egui::pos2(x, wave_rect.top()),
                    egui::pos2(x, wave_rect.bottom()),
                ],
                egui::Stroke::new(1.0, palette.attention_fill_weak),
            );
        }

        if let Some(frac) = info.play_frac {
            let x = x_at(frac);
            ui.painter().line_segment(
                [
                    egui::pos2(x, wave_rect.top()),
                    egui::pos2(x, wave_rect.bottom()),
                ],
                egui::Stroke::new(if info.playing { 2.0 } else { 1.0 }, palette.playing_text),
            );
        }
    }
}

/// Everything the Wave cell needs from the row loop. Passed as a struct
/// because the row already owns these and the alternative is a seven-argument
/// method.
pub(super) struct ListWaveCellCtx<'a> {
    pub(super) row_idx: usize,
    pub(super) path: &'a Path,
    pub(super) row_h: f32,
    pub(super) text_height: f32,
    pub(super) row_bg: Option<Color32>,
    pub(super) row_fg: Option<Color32>,
    /// Resolved once per frame for whichever row is sounding; `Some` only for
    /// that row, so the rest of the list costs one path comparison.
    pub(super) playhead: Option<&'a ListWavePlayheadInfo>,
}

/// What the cell decided, applied by the row loop once its borrows are done.
#[derive(Default)]
pub(super) struct ListWaveCellOutcome {
    pub(super) clicked_to_load: bool,
    /// Set while the pointer is pressed or dragged inside the waveform, so the
    /// row loop does not read the same drag as a file drag-out to the OS. Never
    /// set on hover -- that would disable drag-out permanently.
    pub(super) interacted_with_control: bool,
    /// A seek the row loop applies once the table's borrows are released.
    pub(super) seek_request: Option<ListSeekRequest>,
    /// Give the list keyboard focus, as a click on any other cell does. The
    /// row's own click handling is skipped once `interacted_with_control` is
    /// set, and without this a waveform click would leave Space inert -- which
    /// is how playback starts when Auto Play is off.
    pub(super) focus_list: bool,
}

impl crate::app::WavesPreviewer {
    pub(super) fn ui_list_wave_cell(
        &mut self,
        row: &mut egui_extras::TableRow<'_, '_>,
        ctx: &egui::Context,
        cell: ListWaveCellCtx<'_>,
    ) -> ListWaveCellOutcome {
        let mut outcome = ListWaveCellOutcome::default();
        row.col(|ui| {
            if let Some(bg) = cell.row_bg {
                ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
            }
            ui.visuals_mut().override_text_color = cell.row_fg;
            let (rect2, resp2) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), cell.row_h * 0.9),
                Sense::click_and_drag(),
            );
            resp2.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Slider,
                    ui.is_enabled(),
                    format!("List seek row {}", cell.row_idx),
                )
            });
            let status_text = self.meta_for_path(cell.path).and_then(|meta| {
                meta.decode_error
                    .as_deref()
                    .map(|text| (text, true))
                    .or_else(|| meta.audio_track_absent.then_some(("NO AUDIO", false)))
            });
            let (wave_rect, error_rect) = if status_text.is_some() {
                let err_max = (rect2.height() * 0.45).max(8.0);
                let mut err_h = (cell.row_h * 0.36).max(8.0);
                if err_h > err_max {
                    err_h = err_max;
                }
                let wave_h = (rect2.height() - err_h).max(1.0);
                let wave_rect =
                    egui::Rect::from_min_size(rect2.min, egui::vec2(rect2.width(), wave_h));
                let error_rect = egui::Rect::from_min_size(
                    egui::pos2(rect2.min.x, rect2.max.y - err_h),
                    egui::vec2(rect2.width(), err_h),
                );
                (wave_rect, Some(error_rect))
            } else {
                (rect2, None)
            };
            if let Some(m) = self.meta_for_path(cell.path) {
                let w = wave_rect.width();
                let h = wave_rect.height();
                let n = m.thumb.len().max(1) as f32;
                let gain_db = self.pending_gain_db_for_path(cell.path);
                let scale = db_to_amp(gain_db);
                for (idx, &(mn0, mx0)) in m.thumb.iter().enumerate() {
                    let mn = (mn0 * scale).clamp(-1.0, 1.0);
                    let mx = (mx0 * scale).clamp(-1.0, 1.0);
                    let x = wave_rect.left() + (idx as f32 / n) * w;
                    let y0 = wave_rect.center().y - mx * (h * 0.45);
                    let y1 = wave_rect.center().y - mn * (h * 0.45);
                    let a = (mn.abs().max(mx.abs())).clamp(0.0, 1.0);
                    let col = amp_to_color(a);
                    ui.painter().line_segment(
                        [egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))],
                        egui::Stroke::new(1.0, col),
                    );
                }
            }
            if let Some(overlay) = self.resolve_list_wave_overlay_info(cell.path) {
                self.paint_list_wave_overlay(ui, wave_rect, &overlay);
            }
            if let (Some((text, is_error)), Some(err_rect)) = (status_text, error_rect) {
                let text_pos = egui::pos2(err_rect.left() + 4.0, err_rect.center().y);
                let mut font_size = cell.text_height * 0.85;
                if font_size < 10.0 {
                    font_size = 10.0;
                }
                if font_size > err_rect.height() {
                    font_size = err_rect.height();
                }
                let font = egui::FontId::proportional(font_size);
                ui.painter().text(
                    text_pos,
                    egui::Align2::LEFT_CENTER,
                    text,
                    font,
                    if is_error {
                        self.palette().error_text
                    } else {
                        Color32::from_rgb(220, 180, 110)
                    },
                );
            }
            // A fraction can only become a time once the duration is known,
            // which the header pass resolves before any decode. Until then the
            // cell keeps its old select-and-load behaviour.
            let seekable = self.list_row_duration_secs(cell.path).is_some();
            let hover_frac = (seekable && resp2.hovered())
                .then(|| ui.input(|i| i.pointer.latest_pos()))
                .flatten()
                .map(|pos| {
                    ((pos.x - wave_rect.left()) / wave_rect.width().max(1.0)).clamp(0.0, 1.0)
                });
            self.paint_list_wave_playhead(
                ui,
                wave_rect,
                cell.playhead.unwrap_or(&ListWavePlayheadInfo::default()),
                hover_frac,
            );

            let resp2 = self.attach_row_context_menu(resp2, cell.row_idx, ctx);
            let active_for_row = self.list_seek_gesture.as_ref().is_some_and(|gesture| {
                gesture.row == cell.row_idx && gesture.path.as_path() == cell.path
            });
            let released = resp2.drag_stopped_by(egui::PointerButton::Primary)
                || resp2.clicked_by(egui::PointerButton::Primary);
            let pressed = resp2.is_pointer_button_down_on()
                || resp2.dragged_by(egui::PointerButton::Primary)
                || resp2.drag_started_by(egui::PointerButton::Primary);
            let seek_phase = if released {
                Some(ListSeekPhase::Commit)
            } else if pressed && active_for_row {
                Some(ListSeekPhase::Update)
            } else if pressed {
                Some(ListSeekPhase::Begin)
            } else {
                None
            };
            if seekable && seek_phase.is_some() {
                // Claim the drag so the row does not start a file drag-out.
                outcome.interacted_with_control = true;
                outcome.focus_list = true;
                if let Some(pos) = resp2.interact_pointer_pos() {
                    let frac =
                        ((pos.x - wave_rect.left()) / wave_rect.width().max(1.0)).clamp(0.0, 1.0);
                    outcome.seek_request = Some(ListSeekRequest {
                        row: cell.row_idx,
                        frac,
                        phase: seek_phase.expect("checked above"),
                    });
                }
            } else if resp2.clicked_by(egui::PointerButton::Primary) {
                outcome.clicked_to_load = true;
            }
            if seekable && resp2.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        });
        outcome
    }
}
