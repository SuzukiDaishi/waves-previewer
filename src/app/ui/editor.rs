use std::path::PathBuf;

use crate::app::music_onnx;
use crate::app::render::overlay as ov;
use crate::app::render::waveform_pyramid as wf_cache;
use crate::app::{helpers::*, types::*, LIVE_PREVIEW_SAMPLE_LIMIT};
use crate::wave::build_minmax;
use egui::*;

struct LoopSeamPreview {
    raw_left: Vec<f32>,
    raw_right: Vec<f32>,
    blended_left: Option<Vec<f32>>,
    blended_right: Option<Vec<f32>>,
    sample_rate: u32,
    effective_xfade_samples: usize,
    uses_through_zero: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaveformRenderLod {
    Raw,
    VisibleMinMax,
    Pyramid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AmplitudeNavDragKind {
    MoveCenter,
    ResizeViewport,
}

#[derive(Clone, Copy, Debug)]
struct AmplitudeNavDragState {
    kind: AmplitudeNavDragKind,
    pointer_amp_offset: f32,
    fixed_center: f32,
}

#[derive(Clone, Copy, Debug)]
struct EditorDisplayGeometry {
    wave_left: f32,
    wave_w: f32,
    spp: f32,
    view_offset: usize,
    view_offset_exact: f64,
    display_samples_len: usize,
    visible_count: usize,
}

impl EditorDisplayGeometry {
    fn new(
        wave_left: f32,
        wave_w: f32,
        samples_per_px: f32,
        view_offset: usize,
        view_offset_exact: f64,
        display_samples_len: usize,
    ) -> Self {
        let wave_w = wave_w.max(1.0);
        let spp = samples_per_px.max(0.0001);
        let visible_count = ((wave_w * spp).ceil()).max(1.0) as usize;
        Self {
            wave_left,
            wave_w,
            spp,
            view_offset: view_offset.min(display_samples_len),
            view_offset_exact: view_offset_exact.clamp(
                0.0,
                display_samples_len.saturating_sub(visible_count) as f64,
            ),
            display_samples_len,
            visible_count,
        }
    }

    fn clamp_display_sample(&self, sample: usize) -> usize {
        if self.display_samples_len == 0 {
            0
        } else {
            sample.min(self.display_samples_len.saturating_sub(1))
        }
    }

    fn max_left(&self) -> usize {
        self.display_samples_len.saturating_sub(self.visible_count)
    }

    fn visible_start(&self) -> usize {
        self.view_offset.min(self.display_samples_len)
    }

    fn visible_end(&self) -> usize {
        self.visible_start()
            .saturating_add(self.visible_count)
            .min(self.display_samples_len)
    }

    fn sample_center_x_unclamped(&self, display_sample: usize) -> f32 {
        let rel =
            ((display_sample as f64 + 0.5) - self.view_offset_exact) / self.spp.max(0.0001) as f64;
        self.wave_left + rel as f32
    }

    fn sample_center_x(&self, display_sample: usize) -> f32 {
        self.sample_center_x_unclamped(display_sample)
            .clamp(self.wave_left, self.wave_left + self.wave_w)
    }

    fn sample_boundary_x(&self, display_sample: usize) -> f32 {
        let rel = ((display_sample as f64) - self.view_offset_exact) / self.spp.max(0.0001) as f64;
        (self.wave_left + rel as f32).clamp(self.wave_left, self.wave_left + self.wave_w)
    }

    fn x_to_display_sample(&self, x: f32) -> usize {
        if self.display_samples_len == 0 {
            return 0;
        }
        let clamped_x = x.clamp(self.wave_left, self.wave_left + self.wave_w);
        let raw =
            self.view_offset_exact + ((clamped_x - self.wave_left) as f64 * self.spp as f64) - 0.5;
        raw.round()
            .clamp(0.0, self.display_samples_len.saturating_sub(1) as f64) as usize
    }

    fn contains_sample_center(&self, display_sample: usize) -> bool {
        let x = self.sample_center_x_unclamped(display_sample);
        x >= self.wave_left && x <= self.wave_left + self.wave_w
    }
}

impl crate::app::WavesPreviewer {
    pub(crate) fn normalized_loop_range(range: Option<(usize, usize)>) -> Option<(usize, usize)> {
        range.map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
    }

    pub(crate) fn resolve_editor_loop_visual_ranges(
        tab: &EditorTab,
    ) -> (Option<(usize, usize)>, Option<(usize, usize)>) {
        let editing = Self::normalized_loop_range(tab.loop_region);
        let applied = Self::normalized_loop_range(tab.loop_region_applied)
            .filter(|applied| Some(*applied) != editing);
        (applied, editing)
    }

    fn build_loop_seam_preview(tab: &EditorTab, sample_rate: u32) -> Option<LoopSeamPreview> {
        if tab.active_tool != ToolKind::LoopEdit {
            return None;
        }
        let (a0, b0) = tab.loop_region?;
        let (start, end) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
        if end <= start || tab.ch_samples.is_empty() {
            return None;
        }
        let available_len = tab.ch_samples.iter().map(|ch| ch.len()).min().unwrap_or(0);
        if available_len == 0 || start >= available_len {
            return None;
        }
        let mut mono = vec![0.0f32; available_len];
        let inv_channels = 1.0 / tab.ch_samples.len().max(1) as f32;
        for ch in &tab.ch_samples {
            for (dst, &sample) in mono.iter_mut().zip(ch.iter().take(available_len)) {
                *dst += sample * inv_channels;
            }
        }
        let sr = sample_rate.max(1);
        let effective_xfade_samples =
            Self::effective_loop_xfade_samples(start, end, available_len, tab.loop_xfade_samples);
        let base_side_samples = ((sr as f32) * 0.12).round() as usize;
        let side_samples = base_side_samples
            .max(effective_xfade_samples.saturating_mul(2))
            .clamp(256, 16_384);
        let clamped_end = end.min(available_len);
        let left_start = clamped_end.saturating_sub(side_samples);
        let right_end = start.saturating_add(side_samples).min(available_len);
        let raw_left = mono[left_start..clamped_end].to_vec();
        let raw_right = mono[start..right_end].to_vec();
        if raw_left.is_empty() && raw_right.is_empty() {
            return None;
        }
        let xfade = effective_xfade_samples
            .min(raw_left.len())
            .min(raw_right.len());
        let (blended_left, blended_right) = if xfade > 0 {
            let mut left = raw_left.clone();
            let mut right = raw_right.clone();
            let denom = (xfade.saturating_sub(1)).max(1) as f32;
            let left_base = left.len().saturating_sub(xfade);
            let uses_dip = Self::loop_xfade_uses_through_zero(tab.loop_xfade_shape);
            for i in 0..xfade {
                let t = (i as f32) / denom;
                let (w_out, w_in) = Self::loop_xfade_weights(tab.loop_xfade_shape, t);
                if uses_dip {
                    left[left_base + i] *= w_out;
                    right[i] *= w_in;
                } else {
                    let mixed = left[left_base + i] * w_out + right[i] * w_in;
                    left[left_base + i] = mixed;
                    right[i] = mixed;
                }
            }
            (Some(left), Some(right))
        } else {
            (None, None)
        };
        Some(LoopSeamPreview {
            raw_left,
            raw_right,
            blended_left,
            blended_right,
            sample_rate: sr,
            effective_xfade_samples,
            uses_through_zero: Self::loop_xfade_uses_through_zero(tab.loop_xfade_shape),
        })
    }

    fn tempogram_axis_position(data: &TempogramData, bpm: f32) -> Option<f32> {
        if data.bpm_values.is_empty() {
            return None;
        }
        let mut best_idx = 0usize;
        let mut best_delta = f32::INFINITY;
        for (idx, candidate) in data.bpm_values.iter().copied().enumerate() {
            let delta = (candidate - bpm).abs();
            if delta < best_delta {
                best_delta = delta;
                best_idx = idx;
            }
        }
        Some(best_idx as f32 / data.bpm_values.len().saturating_sub(1).max(1) as f32)
    }

    fn chroma_label_for_bin(bin: usize) -> &'static str {
        const LABELS: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        LABELS[bin.min(LABELS.len() - 1)]
    }

    fn editor_view_label(view: ViewMode) -> &'static str {
        match view {
            ViewMode::Waveform => "Wave",
            ViewMode::Spectrogram => "Spec",
            ViewMode::Log => "Freq Log",
            ViewMode::Mel => "Mel",
            ViewMode::Tempogram => "Tempogram",
            ViewMode::Chromagram => "Chromagram",
        }
    }

    fn editor_set_view_offset(tab: &mut EditorTab, new_view: usize) {
        tab.view_offset = new_view;
        tab.view_offset_exact = new_view as f64;
    }

    fn editor_set_view_offset_exact(tab: &mut EditorTab, exact_view: f64, max_left: usize) {
        let clamped = exact_view.clamp(0.0, max_left as f64);
        tab.view_offset_exact = clamped;
        tab.view_offset = clamped.round() as usize;
    }

    fn editor_selection_anchor_or(tab: &EditorTab, fallback: usize) -> usize {
        tab.selection_anchor_sample
            .or_else(|| tab.selection.map(|(a, b)| a.min(b)))
            .unwrap_or(fallback)
    }

    fn editor_exact_view_for_anchor(
        anchor_sample: usize,
        anchor_ratio: f32,
        wave_w: f32,
        spp: f32,
    ) -> f64 {
        (anchor_sample as f64 + 0.5)
            - (anchor_ratio as f64 * wave_w.max(1.0) as f64 * spp.max(0.0001) as f64)
    }

    fn editor_set_selection_from_anchor(tab: &mut EditorTab, anchor: usize, target: usize) {
        let (start, end) = if target >= anchor {
            (anchor, target)
        } else {
            (target, anchor)
        };
        tab.selection_anchor_sample = Some(anchor);
        tab.selection = Some((start, end));
    }

    fn editor_zoom_anchor(
        mode: EditorHorizontalZoomAnchorMode,
        tab: &EditorTab,
        display_samples_len: usize,
        wave_left: f32,
        wave_w: f32,
        pointer_x: Option<f32>,
        playhead_display: usize,
    ) -> (usize, f32) {
        let geom = EditorDisplayGeometry::new(
            wave_left,
            wave_w,
            tab.samples_per_px,
            tab.view_offset,
            tab.view_offset_exact,
            display_samples_len,
        );
        let center_ratio = 0.5f32;
        let center_sample = geom.x_to_display_sample(wave_left + wave_w * center_ratio);
        let playhead_sample = if display_samples_len == 0 {
            0
        } else {
            playhead_display.min(display_samples_len.saturating_sub(1))
        };
        let playhead_ratio = if wave_w > 0.0 {
            ((geom.sample_center_x_unclamped(playhead_sample) - wave_left) / wave_w).clamp(0.0, 1.0)
        } else {
            center_ratio
        };
        let pointer = pointer_x.map(|x| {
            let clamped_x = x.clamp(wave_left, wave_left + wave_w);
            let ratio = ((clamped_x - wave_left) / wave_w).clamp(0.0, 1.0);
            let sample = geom.x_to_display_sample(clamped_x);
            (sample, ratio)
        });
        match mode {
            EditorHorizontalZoomAnchorMode::Pointer => pointer
                .or_else(|| Some((playhead_sample, playhead_ratio)))
                .unwrap_or((center_sample, center_ratio)),
            EditorHorizontalZoomAnchorMode::Playhead => {
                if display_samples_len > 0 {
                    (playhead_sample, playhead_ratio)
                } else {
                    (center_sample, center_ratio)
                }
            }
        }
    }

    #[cfg(feature = "kittest")]
    pub(crate) fn editor_display_sample_x_for_tab(
        tab: &EditorTab,
        wave_left: f32,
        wave_w: f32,
        display_samples_len: usize,
        display_sample: usize,
    ) -> f32 {
        EditorDisplayGeometry::new(
            wave_left,
            wave_w,
            tab.samples_per_px,
            tab.view_offset,
            tab.view_offset_exact,
            display_samples_len,
        )
        .sample_center_x(display_sample)
    }

    #[cfg(feature = "kittest")]
    pub(crate) fn editor_display_sample_at_x_for_tab(
        tab: &EditorTab,
        wave_left: f32,
        wave_w: f32,
        display_samples_len: usize,
        x: f32,
    ) -> usize {
        EditorDisplayGeometry::new(
            wave_left,
            wave_w,
            tab.samples_per_px,
            tab.view_offset,
            tab.view_offset_exact,
            display_samples_len,
        )
        .x_to_display_sample(x)
    }

    #[cfg(feature = "kittest")]
    pub(crate) fn editor_visible_display_range_for_tab(
        tab: &EditorTab,
        wave_left: f32,
        wave_w: f32,
        display_samples_len: usize,
    ) -> (usize, usize) {
        let geom = EditorDisplayGeometry::new(
            wave_left,
            wave_w,
            tab.samples_per_px,
            tab.view_offset,
            tab.view_offset_exact,
            display_samples_len,
        );
        (geom.visible_start(), geom.visible_end())
    }

    pub(crate) fn waveform_y_from_amp(
        lane_rect: egui::Rect,
        vertical_zoom: f32,
        vertical_view_center: f32,
        amp: f32,
    ) -> f32 {
        crate::app::render::overlay::waveform_y_from_amp(
            lane_rect,
            vertical_zoom,
            vertical_view_center,
            amp,
        )
    }

    pub(crate) fn waveform_center_y(
        lane_rect: egui::Rect,
        vertical_zoom: f32,
        vertical_view_center: f32,
    ) -> f32 {
        Self::waveform_y_from_amp(lane_rect, vertical_zoom, vertical_view_center, 0.0)
    }

    fn amplitude_nav_y_from_amp(rail_rect: egui::Rect, amp: f32) -> f32 {
        let clamped = amp.clamp(-1.0, 1.0);
        rail_rect.center().y - clamped * (rail_rect.height() * 0.5)
    }

    fn amplitude_nav_amp_from_y(rail_rect: egui::Rect, y: f32) -> f32 {
        let half_h = (rail_rect.height() * 0.5).max(1.0);
        ((rail_rect.center().y - y) / half_h).clamp(-1.0, 1.0)
    }

    fn amplitude_nav_viewport_fraction(vertical_zoom: f32) -> f32 {
        crate::app::render::overlay::visible_half_amplitude(vertical_zoom)
            .clamp(1.0 / crate::app::EDITOR_MAX_VERTICAL_ZOOM, 1.0)
    }

    fn amplitude_nav_zoom_from_fraction(viewport_fraction: f32) -> f32 {
        (1.0 / viewport_fraction.clamp(1.0 / crate::app::EDITOR_MAX_VERTICAL_ZOOM, 1.0)).clamp(
            crate::app::EDITOR_MIN_VERTICAL_ZOOM,
            crate::app::EDITOR_MAX_VERTICAL_ZOOM,
        )
    }

    fn amplitude_nav_viewport_rect(
        rail_rect: egui::Rect,
        vertical_zoom: f32,
        vertical_view_center: f32,
    ) -> egui::Rect {
        let viewport_frac = Self::amplitude_nav_viewport_fraction(vertical_zoom);
        let viewport_h = (rail_rect.height() * viewport_frac).clamp(18.0, rail_rect.height());
        let center_amp =
            Self::editor_clamped_vertical_view_center(vertical_zoom, vertical_view_center);
        let center_y = Self::amplitude_nav_y_from_amp(rail_rect, center_amp);
        egui::Rect::from_center_size(
            egui::pos2(rail_rect.center().x, center_y),
            egui::vec2(rail_rect.width(), viewport_h),
        )
    }

    fn draw_editor_time_navigator(
        ui: &mut egui::Ui,
        overview: &[(f32, f32)],
        display_samples_len: usize,
        sample_rate: u32,
        view_offset: usize,
        visible_samples: usize,
        left_pad: f32,
        desired_width: f32,
    ) -> Option<usize> {
        if display_samples_len == 0 {
            return None;
        }
        let mut next_view = None;
        let mut rect = egui::Rect::NOTHING;
        let mut resp = None;
        ui.horizontal(|ui| {
            if left_pad > 0.0 {
                ui.add_space(left_pad);
            }
            ui.vertical(|ui| {
                ui.label(RichText::new("Time").small().strong());
                let desired = egui::vec2(desired_width.max(120.0), 54.0);
                let (navigator_resp, painter) =
                    ui.allocate_painter(desired, Sense::click_and_drag());
                rect = navigator_resp.rect;
                resp = Some(navigator_resp);

                painter.rect_filled(rect, 6.0, Color32::from_rgb(14, 16, 20));
                painter.rect_stroke(
                    rect,
                    6.0,
                    Stroke::new(1.0, Color32::from_rgb(46, 54, 66)),
                    egui::StrokeKind::Outside,
                );
                let wave_rect = rect.shrink2(egui::vec2(8.0, 8.0));
                let wave_rect = egui::Rect::from_min_max(
                    wave_rect.min,
                    egui::pos2(wave_rect.max.x, wave_rect.max.y - 14.0),
                );
                let center_y = wave_rect.center().y;
                let amp = wave_rect.height() * 0.46;
                if !overview.is_empty() {
                    let step = overview.len().max(1) as f32;
                    for (idx, &(lo, hi)) in overview.iter().enumerate() {
                        let x = wave_rect.left() + (idx as f32 / step) * wave_rect.width();
                        let y0 = center_y - hi.clamp(-1.0, 1.0) * amp;
                        let y1 = center_y - lo.clamp(-1.0, 1.0) * amp;
                        painter.line_segment(
                            [egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))],
                            Stroke::new(1.0, Color32::from_rgb(120, 138, 162)),
                        );
                    }
                } else {
                    painter.line_segment(
                        [
                            egui::pos2(wave_rect.left(), center_y),
                            egui::pos2(wave_rect.right(), center_y),
                        ],
                        Stroke::new(1.0, Color32::from_rgb(84, 94, 110)),
                    );
                }

                let total = display_samples_len.max(1);
                let visible = visible_samples.clamp(1, total);
                let max_left = total.saturating_sub(visible);
                let start_frac = (view_offset.min(max_left) as f32 / total as f32).clamp(0.0, 1.0);
                let end_frac =
                    ((view_offset.min(max_left) + visible) as f32 / total as f32).clamp(0.0, 1.0);
                let viewport_rect = egui::Rect::from_min_max(
                    egui::pos2(
                        wave_rect.left() + start_frac * wave_rect.width(),
                        wave_rect.top(),
                    ),
                    egui::pos2(
                        wave_rect.left() + end_frac * wave_rect.width(),
                        wave_rect.bottom(),
                    ),
                );
                painter.rect_filled(
                    viewport_rect,
                    4.0,
                    Color32::from_rgba_unmultiplied(72, 160, 255, 34),
                );
                painter.rect_stroke(
                    viewport_rect,
                    4.0,
                    Stroke::new(1.5, Color32::from_rgb(92, 188, 255)),
                    egui::StrokeKind::Outside,
                );

                let zoom = total as f32 / visible.max(1) as f32;
                let visible_sec = visible as f32 / sample_rate.max(1) as f32;
                let total_sec = total as f32 / sample_rate.max(1) as f32;
                painter.text(
                    egui::pos2(rect.left() + 8.0, rect.bottom() - 5.0),
                    egui::Align2::LEFT_BOTTOM,
                    format!(
                        "Zoom x{zoom:.1}  |  Visible {} / {}",
                        crate::app::helpers::format_time_s(visible_sec),
                        crate::app::helpers::format_time_s(total_sec)
                    ),
                    TextStyle::Small.resolve(ui.style()),
                    Color32::from_rgb(176, 188, 205),
                );
            });
        });
        let max_left = display_samples_len
            .saturating_sub(visible_samples.clamp(1, display_samples_len.max(1)));
        if max_left == 0 {
            return None;
        }
        let navigator_resp = resp.as_ref().expect("navigator response");
        if (navigator_resp.clicked() || navigator_resp.dragged())
            && navigator_resp.interact_pointer_pos().is_some()
        {
            let wave_rect = rect.shrink2(egui::vec2(8.0, 8.0));
            let wave_rect = egui::Rect::from_min_max(
                wave_rect.min,
                egui::pos2(wave_rect.max.x, wave_rect.max.y - 14.0),
            );
            let pos = navigator_resp.interact_pointer_pos().unwrap();
            let ratio = ((pos.x - wave_rect.left()) / wave_rect.width()).clamp(0.0, 1.0);
            let total = display_samples_len.max(1);
            let visible = visible_samples.clamp(1, total);
            let center = (ratio * total as f32).round() as usize;
            next_view = Some(center.saturating_sub(visible / 2).min(max_left));
        }
        next_view
    }

    fn draw_editor_amplitude_navigator(
        ui: &mut egui::Ui,
        rect: egui::Rect,
        tab: &mut EditorTab,
    ) -> Option<(f32, f32)> {
        let current_zoom = tab.vertical_zoom.clamp(
            crate::app::EDITOR_MIN_VERTICAL_ZOOM,
            crate::app::EDITOR_MAX_VERTICAL_ZOOM,
        );
        let current_center =
            Self::editor_clamped_vertical_view_center(current_zoom, tab.vertical_view_center);
        let mut next_zoom = current_zoom;
        let mut next_center = current_center;
        let nav_resp = ui.interact(
            rect,
            egui::Id::new("editor_amplitude_nav"),
            Sense::click_and_drag(),
        );
        let nav_resp = nav_resp.on_hover_text(format!(
            "Zoom x{current_zoom:.1}\nCenter {current_center:+.2}"
        ));
        let painter = ui.painter_at(rect);
        let rail_rect = rect;
        let viewport_rect =
            Self::amplitude_nav_viewport_rect(rail_rect, current_zoom, current_center);
        let drag_id = nav_resp.id.with("drag_state");
        let active_drag_state = ui
            .ctx()
            .memory(|mem| mem.data.get_temp::<AmplitudeNavDragState>(drag_id));

        painter.rect_filled(rail_rect, 5.0, Color32::from_rgb(14, 16, 20));
        painter.rect_stroke(
            rail_rect,
            5.0,
            Stroke::new(1.0, Color32::from_rgb(46, 54, 66)),
            egui::StrokeKind::Outside,
        );
        let zero_y = Self::amplitude_nav_y_from_amp(rail_rect, 0.0);
        painter.line_segment(
            [
                egui::pos2(rail_rect.left(), zero_y),
                egui::pos2(rail_rect.right(), zero_y),
            ],
            Stroke::new(1.0, Color32::from_rgb(86, 98, 116)),
        );
        painter.rect_filled(
            viewport_rect,
            4.0,
            Color32::from_rgba_unmultiplied(72, 160, 255, 34),
        );
        painter.rect_stroke(
            viewport_rect,
            4.0,
            Stroke::new(1.5, Color32::from_rgb(92, 188, 255)),
            egui::StrokeKind::Outside,
        );
        for y in [viewport_rect.top(), viewport_rect.bottom()] {
            painter.line_segment(
                [
                    egui::pos2(viewport_rect.left() + 2.0, y),
                    egui::pos2(viewport_rect.right() - 2.0, y),
                ],
                Stroke::new(2.0, Color32::from_rgb(110, 210, 255)),
            );
        }

        if nav_resp.hovered() || active_drag_state.is_some() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::ResizeVertical);
        }

        let edge_zone = 9.0;
        let (pointer_pos, pointer_down, pointer_released, time_now, press_positions) =
            ui.input(|i| {
                let press_positions = i
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            ..
                        } if rail_rect.contains(*pos) => Some(*pos),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let pointer_released = i.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::PointerButton {
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            ..
                        }
                    )
                });
                (
                    i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()),
                    i.pointer.primary_down(),
                    pointer_released,
                    i.time,
                    press_positions,
                )
            });

        if let Some(pos) = press_positions.last().copied() {
            let is_double_click = if press_positions.len() >= 2 {
                let prev = press_positions[press_positions.len() - 2];
                prev.distance(pos) <= 6.0
            } else {
                tab.last_amplitude_nav_click_pos
                    .map(|prev_pos| {
                        (time_now - tab.last_amplitude_nav_click_at) <= 0.8
                            && prev_pos.distance(pos) <= 6.0
                    })
                    .unwrap_or(false)
            };
            tab.last_amplitude_nav_click_at = time_now;
            tab.last_amplitude_nav_click_pos = Some(pos);

            if is_double_click {
                next_zoom = 1.0;
                next_center = 0.0;
                ui.ctx().memory_mut(|mem| {
                    mem.data.remove::<AmplitudeNavDragState>(drag_id);
                });
            } else {
                let pointer_amp = Self::amplitude_nav_amp_from_y(rail_rect, pos.y);
                let expanded_viewport = viewport_rect.expand2(egui::vec2(7.0, edge_zone));
                let drag_state = if expanded_viewport.contains(pos)
                    && ((pos.y - viewport_rect.top()).abs() <= edge_zone
                        || (pos.y - viewport_rect.bottom()).abs() <= edge_zone)
                {
                    Some(AmplitudeNavDragState {
                        kind: AmplitudeNavDragKind::ResizeViewport,
                        pointer_amp_offset: 0.0,
                        fixed_center: current_center,
                    })
                } else {
                    let center = if viewport_rect.expand2(egui::vec2(4.0, 0.0)).contains(pos) {
                        current_center
                    } else {
                        Self::editor_clamped_vertical_view_center(current_zoom, pointer_amp)
                    };
                    next_center = center;
                    Some(AmplitudeNavDragState {
                        kind: AmplitudeNavDragKind::MoveCenter,
                        pointer_amp_offset: pointer_amp - center,
                        fixed_center: center,
                    })
                };
                if let Some(state) = drag_state {
                    ui.ctx().memory_mut(|mem| {
                        mem.data.insert_temp(drag_id, state);
                    });
                }
            }
        }

        if pointer_down {
            if let (Some(state), Some(pos)) = (active_drag_state, pointer_pos) {
                let pointer_y = pos.y.clamp(rail_rect.top(), rail_rect.bottom());
                let pointer_amp = Self::amplitude_nav_amp_from_y(rail_rect, pointer_y);
                match state.kind {
                    AmplitudeNavDragKind::MoveCenter => {
                        next_center = Self::editor_clamped_vertical_view_center(
                            current_zoom,
                            pointer_amp - state.pointer_amp_offset,
                        );
                    }
                    AmplitudeNavDragKind::ResizeViewport => {
                        let half = (pointer_amp - state.fixed_center)
                            .abs()
                            .clamp(1.0 / crate::app::EDITOR_MAX_VERTICAL_ZOOM, 1.0);
                        next_zoom = Self::amplitude_nav_zoom_from_fraction(half)
                            .clamp(1.0, crate::app::EDITOR_MAX_VERTICAL_ZOOM);
                        next_center = Self::editor_clamped_vertical_view_center(
                            next_zoom,
                            state.fixed_center,
                        );
                    }
                }
            }
        }

        if pointer_released {
            ui.ctx().memory_mut(|mem| {
                mem.data.remove::<AmplitudeNavDragState>(drag_id);
            });
        }

        let changed = (next_zoom - current_zoom).abs() > 0.0001
            || (next_center - current_center).abs() > 0.0001;
        changed.then_some((next_zoom, next_center))
    }

    fn draw_loop_seam_preview(
        ui: &mut egui::Ui,
        preview: &LoopSeamPreview,
        vertical_zoom: f32,
        vertical_view_center: f32,
    ) {
        let desired = egui::vec2(ui.available_width().max(120.0), 84.0);
        let (resp, painter) = ui.allocate_painter(desired, Sense::hover());
        let rect = resp.rect;
        painter.rect_filled(rect, 6.0, Color32::from_rgb(15, 18, 24));
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, Color32::from_rgb(52, 62, 78)),
            egui::StrokeKind::Outside,
        );
        let wave_rect = rect.shrink2(egui::vec2(10.0, 8.0));
        let wave_rect = egui::Rect::from_min_max(
            wave_rect.min,
            egui::pos2(wave_rect.max.x, wave_rect.max.y - 14.0),
        );
        let footer_y = rect.bottom() - 10.0;
        let seam_x = wave_rect.center().x;
        let half_gap = 6.0;
        let left_rect = egui::Rect::from_min_max(
            wave_rect.min,
            egui::pos2(seam_x - half_gap, wave_rect.max.y),
        );
        let right_rect = egui::Rect::from_min_max(
            egui::pos2(seam_x + half_gap, wave_rect.min.y),
            wave_rect.max,
        );
        painter.line_segment(
            [
                egui::pos2(
                    wave_rect.left(),
                    Self::waveform_center_y(wave_rect, vertical_zoom, vertical_view_center),
                ),
                egui::pos2(
                    wave_rect.right(),
                    Self::waveform_center_y(wave_rect, vertical_zoom, vertical_view_center),
                ),
            ],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 140, 170, 36)),
        );
        painter.line_segment(
            [
                egui::pos2(seam_x, wave_rect.top()),
                egui::pos2(seam_x, wave_rect.bottom()),
            ],
            Stroke::new(1.5, Color32::from_rgb(255, 196, 72)),
        );
        let draw_half = |painter: &egui::Painter,
                         samples: &[f32],
                         target_rect: egui::Rect,
                         stroke: Stroke,
                         stem_col: Color32| {
            let bins = target_rect.width().round().max(8.0) as usize;
            let mut tmp = Vec::new();
            build_minmax(&mut tmp, samples, bins);
            if tmp.is_empty() {
                return;
            }
            let denom = (tmp.len().saturating_sub(1)).max(1) as f32;
            let mut points = Vec::with_capacity(tmp.len());
            for (i, (mn, mx)) in tmp.iter().enumerate() {
                let x = egui::lerp(target_rect.x_range(), i as f32 / denom);
                let y_min =
                    Self::waveform_y_from_amp(wave_rect, vertical_zoom, vertical_view_center, *mn);
                let y_max =
                    Self::waveform_y_from_amp(wave_rect, vertical_zoom, vertical_view_center, *mx);
                painter.line_segment(
                    [egui::pos2(x, y_min), egui::pos2(x, y_max)],
                    Stroke::new(1.0, stem_col),
                );
                points.push(egui::pos2(
                    x,
                    Self::waveform_y_from_amp(
                        wave_rect,
                        vertical_zoom,
                        vertical_view_center,
                        (mn + mx) * 0.5,
                    ),
                ));
            }
            if points.len() >= 2 {
                painter.add(egui::Shape::line(points, stroke));
            }
        };
        let raw_stem = Color32::from_rgba_unmultiplied(112, 160, 255, 48);
        let raw_line = Stroke::new(1.2, Color32::from_rgb(120, 176, 255));
        draw_half(&painter, &preview.raw_left, left_rect, raw_line, raw_stem);
        draw_half(&painter, &preview.raw_right, right_rect, raw_line, raw_stem);
        if let (Some(left), Some(right)) = (&preview.blended_left, &preview.blended_right) {
            let blend_stem = Color32::from_rgba_unmultiplied(88, 255, 224, 32);
            let blend_line = Stroke::new(1.8, Color32::from_rgb(92, 255, 224));
            draw_half(&painter, left, left_rect, blend_line, blend_stem);
            draw_half(&painter, right, right_rect, blend_line, blend_stem);
        }
        let label_font = TextStyle::Small.resolve(ui.style());
        painter.text(
            left_rect.left_top(),
            egui::Align2::LEFT_TOP,
            "End",
            label_font.clone(),
            Color32::from_rgb(185, 190, 204),
        );
        painter.text(
            right_rect.right_top(),
            egui::Align2::RIGHT_TOP,
            "Start",
            label_font.clone(),
            Color32::from_rgb(185, 190, 204),
        );
        let window_ms = (preview.raw_left.len().max(preview.raw_right.len()) as f32
            / preview.sample_rate as f32)
            * 1000.0;
        let xfade_ms =
            (preview.effective_xfade_samples as f32 / preview.sample_rate as f32) * 1000.0;
        let mode_label = if preview.uses_through_zero {
            "fade to 0"
        } else {
            "crossfade"
        };
        painter.text(
            egui::pos2(rect.center().x, footer_y),
            egui::Align2::CENTER_CENTER,
            format!("{mode_label} / window {window_ms:.1} ms / xfade {xfade_ms:.1} ms"),
            label_font,
            Color32::from_rgb(150, 162, 184),
        );
    }

    fn draw_loop_window_preview(
        ui: &mut egui::Ui,
        samples: &[f32],
        sample_rate: u32,
        accent: Color32,
        vertical_zoom: f32,
        vertical_view_center: f32,
    ) {
        let desired = egui::vec2(ui.available_width().max(96.0), 84.0);
        let (resp, painter) = ui.allocate_painter(desired, Sense::hover());
        let rect = resp.rect;
        painter.rect_filled(rect, 6.0, Color32::from_rgb(15, 18, 24));
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, Color32::from_rgb(52, 62, 78)),
            egui::StrokeKind::Outside,
        );
        if samples.is_empty() {
            return;
        }
        let wave_rect = rect.shrink2(egui::vec2(10.0, 8.0));
        let wave_rect = egui::Rect::from_min_max(
            wave_rect.min,
            egui::pos2(wave_rect.max.x, wave_rect.max.y - 14.0),
        );
        painter.line_segment(
            [
                egui::pos2(
                    wave_rect.left(),
                    Self::waveform_center_y(wave_rect, vertical_zoom, vertical_view_center),
                ),
                egui::pos2(
                    wave_rect.right(),
                    Self::waveform_center_y(wave_rect, vertical_zoom, vertical_view_center),
                ),
            ],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 140, 170, 30)),
        );
        let bins = wave_rect.width().round().max(8.0) as usize;
        let mut tmp = Vec::new();
        build_minmax(&mut tmp, samples, bins);
        let denom = (tmp.len().saturating_sub(1)).max(1) as f32;
        for (idx, (mn, mx)) in tmp.iter().enumerate() {
            let x = egui::lerp(wave_rect.x_range(), idx as f32 / denom);
            let y0 = Self::waveform_y_from_amp(wave_rect, vertical_zoom, vertical_view_center, *mx);
            let y1 = Self::waveform_y_from_amp(wave_rect, vertical_zoom, vertical_view_center, *mn);
            painter.line_segment(
                [egui::pos2(x, y0), egui::pos2(x, y1)],
                Stroke::new(1.1, accent.gamma_multiply(0.45)),
            );
        }
        let ms = (samples.len() as f32 / sample_rate.max(1) as f32) * 1000.0;
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 10.0),
            egui::Align2::CENTER_CENTER,
            format!("{ms:.1} ms"),
            TextStyle::Small.resolve(ui.style()),
            Color32::from_rgb(150, 162, 184),
        );
    }

    fn find_zero_cross_display(&self, tab_idx: usize, cur: usize, dir: i32) -> usize {
        let Some(tab) = self.tabs.get(tab_idx) else {
            return cur;
        };
        let channel_count = tab.ch_samples.len();
        if channel_count == 0 {
            return cur;
        }
        let eps = self.zero_cross_epsilon.max(0.0);
        let mut visible = tab.channel_view.visible_indices(channel_count);
        let use_mixdown = tab.channel_view.mode == ChannelViewMode::Mixdown || visible.len() <= 1;
        let require_all = tab.channel_view.mode == ChannelViewMode::All;
        if require_all {
            visible = (0..channel_count).collect();
        }
        let min_len = tab.ch_samples.iter().map(|c| c.len()).min().unwrap_or(0);
        if min_len == 0 {
            return cur;
        }
        let cur = cur.min(min_len.saturating_sub(1));
        let is_cross = |prev: f32, cur: f32| -> bool {
            cur.abs() <= eps
                || prev.abs() <= eps
                || (prev > 0.0 && cur < 0.0)
                || (prev < 0.0 && cur > 0.0)
        };
        if use_mixdown {
            let mix_at = |idx: usize| -> f32 {
                let mut sum = 0.0f32;
                for ch in &tab.ch_samples {
                    if idx < ch.len() {
                        sum += ch[idx];
                    }
                }
                sum / channel_count as f32
            };
            if dir > 0 {
                if cur + 1 >= min_len {
                    return cur;
                }
                let mut prev = mix_at(cur);
                let mut i = cur + 1;
                while i < min_len {
                    let s = mix_at(i);
                    if is_cross(prev, s) {
                        return i;
                    }
                    prev = s;
                    i += 1;
                }
            } else if cur > 0 {
                let mut prev = mix_at(cur);
                let mut i = cur.saturating_sub(1);
                loop {
                    let s = mix_at(i);
                    if is_cross(prev, s) {
                        return i;
                    }
                    prev = s;
                    if i == 0 {
                        break;
                    }
                    i -= 1;
                }
            }
            return cur;
        }

        let mut prevs: Vec<f32> = Vec::with_capacity(visible.len());
        for &ch_idx in &visible {
            let ch = &tab.ch_samples[ch_idx];
            prevs.push(ch.get(cur).copied().unwrap_or(0.0));
        }
        if dir > 0 {
            if cur + 1 >= min_len {
                return cur;
            }
            let mut i = cur + 1;
            while i < min_len {
                let mut all_ok = true;
                for (slot, &ch_idx) in visible.iter().enumerate() {
                    let ch = &tab.ch_samples[ch_idx];
                    let s = ch.get(i).copied().unwrap_or(0.0);
                    if !is_cross(prevs[slot], s) {
                        all_ok = false;
                    }
                    prevs[slot] = s;
                }
                if all_ok {
                    return i;
                }
                i += 1;
            }
        } else if cur > 0 {
            let mut i = cur.saturating_sub(1);
            loop {
                let mut all_ok = true;
                for (slot, &ch_idx) in visible.iter().enumerate() {
                    let ch = &tab.ch_samples[ch_idx];
                    let s = ch.get(i).copied().unwrap_or(0.0);
                    if !is_cross(prevs[slot], s) {
                        all_ok = false;
                    }
                    prevs[slot] = s;
                }
                if all_ok {
                    return i;
                }
                if i == 0 {
                    break;
                }
                i -= 1;
            }
        }
        cur
    }

    pub(crate) fn push_peak_shapes(
        painter: &egui::Painter,
        peaks: &[wf_cache::Peak],
        lane_rect: egui::Rect,
        columns: &ov::WaveformDeviceColumns,
        scale: f32,
        vertical_zoom: f32,
        vertical_view_center: f32,
    ) {
        ov::draw_aggregated_waveform_columns(
            painter,
            lane_rect,
            columns,
            0,
            peaks.len(),
            vertical_zoom,
            vertical_view_center,
            |idx| {
                let peak = peaks.get(idx)?;
                let mn = (peak.min * scale).clamp(-1.0, 1.0);
                let mx = (peak.max * scale).clamp(-1.0, 1.0);
                if !mn.is_finite() || !mx.is_finite() {
                    return None;
                }
                let amp = (mn.abs().max(mx.abs())).clamp(0.0, 1.0);
                Some(ov::AggregatedWaveColumn {
                    min: mn,
                    max: mx,
                    color: amp_to_color(amp),
                    stroke: 1.0,
                })
            },
        );
    }

    fn render_loading_overview_waveform(
        overview: &[(f32, f32)],
        display_samples_len: usize,
        lane_rect: egui::Rect,
        waveform_columns: &ov::WaveformDeviceColumns,
        scale: f32,
        vertical_zoom: f32,
        vertical_view_center: f32,
        start: usize,
        end: usize,
        painter: &egui::Painter,
        scratch: &mut wf_cache::WaveformScratch,
    ) -> (WaveformRenderLod, f32, f32) {
        if overview.is_empty() || display_samples_len == 0 || end <= start {
            return (WaveformRenderLod::VisibleMinMax, 0.0, 0.0);
        }
        let peaks = &mut scratch.peaks;
        peaks.clear();
        let bins = waveform_columns.column_count().max(1);
        peaks.reserve(bins);
        let visible_len = end.saturating_sub(start).max(1);
        let query_started = std::time::Instant::now();
        for col in 0..bins {
            let s0 = start.saturating_add(
                ((visible_len as u128).saturating_mul(col as u128) / bins as u128) as usize,
            );
            let s1 = start.saturating_add(
                ((visible_len as u128).saturating_mul((col + 1) as u128) / bins as u128) as usize,
            );
            let mut i0 = ((s0 as u128).saturating_mul(overview.len() as u128)
                / display_samples_len.max(1) as u128) as usize;
            let mut i1 = (((s1.max(s0 + 1) as u128).saturating_mul(overview.len() as u128))
                .saturating_add(display_samples_len.max(1) as u128 - 1)
                / display_samples_len.max(1) as u128) as usize;
            i0 = i0.min(overview.len().saturating_sub(1));
            i1 = i1.clamp(i0 + 1, overview.len());
            let mut mn = 1.0f32;
            let mut mx = -1.0f32;
            for &(lo, hi) in &overview[i0..i1] {
                mn = mn.min(lo);
                mx = mx.max(hi);
            }
            peaks.push(wf_cache::Peak { min: mn, max: mx });
        }
        let query_ms = query_started.elapsed().as_secs_f32() * 1000.0;
        let draw_started = std::time::Instant::now();
        Self::push_peak_shapes(
            painter,
            peaks,
            lane_rect,
            waveform_columns,
            scale,
            vertical_zoom,
            vertical_view_center,
        );
        let draw_ms = draw_started.elapsed().as_secs_f32() * 1000.0;
        (WaveformRenderLod::VisibleMinMax, query_ms, draw_ms)
    }

    fn compute_overlay_bins_from_overview(
        overview: &[(f32, f32)],
        start: usize,
        visible_len: usize,
        base_total: usize,
        overlay_total: usize,
        bins: usize,
        is_time_stretch: bool,
    ) -> Vec<(f32, f32)> {
        if overview.is_empty()
            || visible_len == 0
            || bins == 0
            || base_total == 0
            || overlay_total == 0
        {
            return Vec::new();
        }
        let ratio = if is_time_stretch {
            1.0
        } else {
            overlay_total as f64 / base_total.max(1) as f64
        };
        let start_scaled = ((start as f64) * ratio).round() as usize;
        let mut visible_scaled = ((visible_len as f64) * ratio).ceil() as usize;
        if visible_scaled == 0 {
            visible_scaled = 1;
        }
        let overlay_window_end = start_scaled
            .saturating_add(visible_scaled)
            .min(overlay_total.max(start_scaled + 1));
        let overlay_window_len = overlay_window_end.saturating_sub(start_scaled).max(1);
        let mut out = Vec::with_capacity(bins);
        for col in 0..bins {
            let s0 = start_scaled.saturating_add(
                ((overlay_window_len as u128).saturating_mul(col as u128) / bins as u128) as usize,
            );
            let s1 = start_scaled.saturating_add(
                ((overlay_window_len as u128).saturating_mul((col + 1) as u128) / bins as u128)
                    as usize,
            );
            let mut i0 = ((s0 as u128).saturating_mul(overview.len() as u128)
                / overlay_total.max(1) as u128) as usize;
            let mut i1 = (((s1.max(s0 + 1) as u128).saturating_mul(overview.len() as u128))
                .saturating_add(overlay_total.max(1) as u128 - 1)
                / overlay_total.max(1) as u128) as usize;
            i0 = i0.min(overview.len().saturating_sub(1));
            i1 = i1.clamp(i0 + 1, overview.len());
            let mut mn = 1.0f32;
            let mut mx = -1.0f32;
            for &(lo, hi) in &overview[i0..i1] {
                mn = mn.min(lo);
                mx = mx.max(hi);
            }
            out.push((mn, mx));
        }
        out
    }

    fn render_editor_lane_waveform(
        tab: &EditorTab,
        use_mixdown: bool,
        channel_index: Option<usize>,
        lane_rect: egui::Rect,
        geom: EditorDisplayGeometry,
        waveform_columns: &ov::WaveformDeviceColumns,
        scale: f32,
        vertical_zoom: f32,
        vertical_view_center: f32,
        start: usize,
        end: usize,
        spp: f32,
        painter: &egui::Painter,
        scratch: &mut wf_cache::WaveformScratch,
    ) -> (WaveformRenderLod, f32, f32) {
        let visible_len = end.saturating_sub(start);
        if visible_len == 0 {
            return (WaveformRenderLod::VisibleMinMax, 0.0, 0.0);
        }
        let (peaks, mono, shapes, line_points) = (
            &mut scratch.peaks,
            &mut scratch.mono,
            &mut scratch.shapes,
            &mut scratch.line_points,
        );
        let bins = waveform_columns.column_count().max(1);
        let mut lod = if spp < 2.0 {
            WaveformRenderLod::Raw
        } else if spp < 32.0 {
            WaveformRenderLod::VisibleMinMax
        } else {
            WaveformRenderLod::Pyramid
        };

        let query_started = std::time::Instant::now();
        peaks.clear();
        mono.clear();
        line_points.clear();
        match lod {
            WaveformRenderLod::Raw => {
                if use_mixdown {
                    wf_cache::build_mixdown_visible(&tab.ch_samples, start, end, mono);
                } else if let Some(samples) = channel_index
                    .and_then(|idx| tab.ch_samples.get(idx))
                    .map(|ch| &ch[start..end])
                {
                    if samples.is_empty() {
                        lod = WaveformRenderLod::VisibleMinMax;
                    }
                }
            }
            WaveformRenderLod::VisibleMinMax => {
                if use_mixdown {
                    wf_cache::build_mixdown_minmax_visible(
                        &tab.ch_samples,
                        start,
                        end,
                        bins,
                        peaks,
                    );
                } else if let Some(samples) = channel_index
                    .and_then(|idx| tab.ch_samples.get(idx))
                    .map(|ch| &ch[start..end])
                {
                    wf_cache::build_visible_minmax(samples, bins, peaks);
                }
            }
            WaveformRenderLod::Pyramid => {
                let mut used_pyramid = false;
                if let Some(set) = tab.waveform_pyramid.as_ref() {
                    if use_mixdown {
                        set.mixdown.query_columns(start, end, bins, spp, peaks);
                        used_pyramid = !peaks.is_empty();
                    } else if let Some(channel_idx) = channel_index {
                        if let Some(pyramid) = set.channels.get(channel_idx) {
                            pyramid.query_columns(start, end, bins, spp, peaks);
                            used_pyramid = !peaks.is_empty();
                        }
                    }
                }
                if !used_pyramid {
                    lod = WaveformRenderLod::VisibleMinMax;
                    if use_mixdown {
                        wf_cache::build_mixdown_minmax_visible(
                            &tab.ch_samples,
                            start,
                            end,
                            bins,
                            peaks,
                        );
                    } else if let Some(samples) = channel_index
                        .and_then(|idx| tab.ch_samples.get(idx))
                        .map(|ch| &ch[start..end])
                    {
                        wf_cache::build_visible_minmax(samples, bins, peaks);
                    }
                }
            }
        }
        let query_ms = query_started.elapsed().as_secs_f32() * 1000.0;

        let draw_started = std::time::Instant::now();
        shapes.clear();
        match lod {
            WaveformRenderLod::Raw => {
                let base_y =
                    Self::waveform_center_y(lane_rect, vertical_zoom, vertical_view_center);
                if use_mixdown {
                    if mono.len() == 1 {
                        let sx = geom.sample_center_x(start);
                        let v = mono[0].mul_add(scale, 0.0).clamp(-1.0, 1.0);
                        let sy = Self::waveform_y_from_amp(
                            lane_rect,
                            vertical_zoom,
                            vertical_view_center,
                            v,
                        );
                        painter.circle_filled(
                            egui::pos2(sx, sy),
                            2.0,
                            amp_to_color(v.abs().clamp(0.0, 1.0)),
                        );
                    } else if !mono.is_empty() {
                        line_points.reserve(mono.len());
                        for (i, &sample) in mono.iter().enumerate() {
                            let v = (sample * scale).clamp(-1.0, 1.0);
                            let sample_idx = start.saturating_add(i);
                            let sx = geom.sample_center_x(sample_idx);
                            let sy = Self::waveform_y_from_amp(
                                lane_rect,
                                vertical_zoom,
                                vertical_view_center,
                                v,
                            );
                            line_points.push(egui::pos2(sx, sy));
                        }
                        for i in 1..line_points.len() {
                            let v = (mono[i - 1] * scale).clamp(-1.0, 1.0);
                            let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                            shapes.push(egui::Shape::line_segment(
                                [line_points[i - 1], line_points[i]],
                                egui::Stroke::new(1.0, col),
                            ));
                        }
                        let pps = 1.0 / spp.max(1.0e-6);
                        if pps >= 6.0 {
                            for (point, &sample) in line_points.iter().zip(mono.iter()) {
                                let v = (sample * scale).clamp(-1.0, 1.0);
                                let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                                shapes.push(egui::Shape::line_segment(
                                    [egui::pos2(point.x, base_y), *point],
                                    egui::Stroke::new(1.0, col),
                                ));
                            }
                        }
                    }
                } else if let Some(samples) = channel_index
                    .and_then(|idx| tab.ch_samples.get(idx))
                    .map(|ch| &ch[start..end])
                {
                    if samples.len() == 1 {
                        let sx = geom.sample_center_x(start);
                        let v = samples[0].mul_add(scale, 0.0).clamp(-1.0, 1.0);
                        let sy = Self::waveform_y_from_amp(
                            lane_rect,
                            vertical_zoom,
                            vertical_view_center,
                            v,
                        );
                        painter.circle_filled(
                            egui::pos2(sx, sy),
                            2.0,
                            amp_to_color(v.abs().clamp(0.0, 1.0)),
                        );
                    } else if !samples.is_empty() {
                        line_points.reserve(samples.len());
                        for (i, &sample) in samples.iter().enumerate() {
                            let v = (sample * scale).clamp(-1.0, 1.0);
                            let sample_idx = start.saturating_add(i);
                            let sx = geom.sample_center_x(sample_idx);
                            let sy = Self::waveform_y_from_amp(
                                lane_rect,
                                vertical_zoom,
                                vertical_view_center,
                                v,
                            );
                            line_points.push(egui::pos2(sx, sy));
                        }
                        for i in 1..line_points.len() {
                            let v = (samples[i - 1] * scale).clamp(-1.0, 1.0);
                            let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                            shapes.push(egui::Shape::line_segment(
                                [line_points[i - 1], line_points[i]],
                                egui::Stroke::new(1.0, col),
                            ));
                        }
                        let pps = 1.0 / spp.max(1.0e-6);
                        if pps >= 6.0 {
                            for (point, &sample) in line_points.iter().zip(samples.iter()) {
                                let v = (sample * scale).clamp(-1.0, 1.0);
                                let col = amp_to_color(v.abs().clamp(0.0, 1.0));
                                shapes.push(egui::Shape::line_segment(
                                    [egui::pos2(point.x, base_y), *point],
                                    egui::Stroke::new(1.0, col),
                                ));
                            }
                        }
                    }
                }
            }
            WaveformRenderLod::VisibleMinMax | WaveformRenderLod::Pyramid => {
                Self::push_peak_shapes(
                    painter,
                    peaks,
                    lane_rect,
                    waveform_columns,
                    scale,
                    vertical_zoom,
                    vertical_view_center,
                );
            }
        }
        if !shapes.is_empty() {
            painter.extend(shapes.drain(..));
        }
        let draw_ms = draw_started.elapsed().as_secs_f32() * 1000.0;
        (lod, query_ms, draw_ms)
    }

    pub(in crate::app) fn ui_editor_view(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        tab_idx: usize,
    ) {
        let editor_panel_rect = ui.max_rect();
        let mut apply_pending_loop = false;
        let mut do_commit_loop = false;
        let mut do_preview_unwrap: Option<u32> = None;
        let mut do_commit_markers = false;
        let mut pending_edit_undo: Option<EditorUndoState> = None;
        // Use one editor display timebase for playhead, seek, HUD, and time ruler.
        let sr_ctx = Self::editor_display_sample_rate(
            &self.tabs[tab_idx],
            self.audio.shared.out_sample_rate.max(1),
        ) as f32;
        let pos_audio_now = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        let tab_samples_len = Self::editor_display_samples_len(&self.tabs[tab_idx]);
        let audio_len = self.audio.current_source_len();
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let playback_source = self.playback_session.source.clone();
        let transport = self.playback_session.transport;
        let transport_sr = self.playback_session.transport_sr.max(1);
        let playback_rate = self.playback_session.applied_playback_rate;
        let mode = self.playback_session.applied_mode;
        let map_audio_to_display = |tab: &EditorTab, audio_pos: usize| -> usize {
            Self::map_audio_to_display_sample_with(
                tab,
                audio_pos,
                audio_len,
                &playback_source,
                transport,
                transport_sr,
                out_sr,
                mode,
                playback_rate,
            )
        };
        let map_display_to_audio = |tab: &EditorTab, display_pos: usize| -> usize {
            Self::map_display_to_audio_sample_with(
                tab,
                display_pos,
                audio_len,
                &playback_source,
                transport,
                transport_sr,
                out_sr,
                mode,
                playback_rate,
            )
        };
        let playhead_display_now = map_audio_to_display(&self.tabs[tab_idx], pos_audio_now);
        let mut request_seek: Option<usize> = None;
        let spec_path = self.tabs[tab_idx].path.clone();
        let current_view = self.tabs[tab_idx].leaf_view_mode();
        let spec_cache = self.spectro_cache.get(&spec_path).cloned();
        let spec_loading = self.spectro_inflight.contains(&spec_path);
        let feature_kind = match current_view {
            ViewMode::Tempogram => Some(EditorAnalysisKind::Tempogram),
            ViewMode::Chromagram => Some(EditorAnalysisKind::Chromagram),
            _ => None,
        };
        let feature_key = feature_kind.map(|kind| EditorAnalysisKey {
            path: spec_path.clone(),
            kind,
        });
        let feature_cache = feature_key
            .as_ref()
            .and_then(|key| self.editor_feature_cache.get(key).cloned());
        let feature_loading = feature_key
            .as_ref()
            .map(|key| self.editor_feature_inflight.contains(key))
            .unwrap_or(false);
        let feature_progress = feature_key
            .as_ref()
            .and_then(|key| self.editor_feature_progress.get(key))
            .map(|p| (p.done_units, p.total_units, p.started_at));
        let tempogram_data = match feature_cache.as_deref() {
            Some(EditorFeatureAnalysisData::Tempogram(data)) => Some(data.clone()),
            _ => None,
        };
        let chromagram_data = match feature_cache.as_deref() {
            Some(EditorFeatureAnalysisData::Chromagram(data)) => Some(data.clone()),
            _ => None,
        };
        let mut touch_spectro_cache = false;
        let mut pending_viewport_hint: Option<crate::app::editor_viewport::EditorViewportHint> =
            None;
        ui.horizontal(|ui| {
            let tab = &self.tabs[tab_idx];
            let base = if self.is_virtual_path(&tab.path) {
                format!("{} (virtual)", tab.display_name)
            } else {
                tab.path.display().to_string()
            };
            ui.add(
                egui::Label::new(RichText::new(base).monospace())
                    .truncate()
                    .show_tooltip_when_elided(true),
            );
        });
        let mut discard_preview_for_view_change = false;
        let mut request_preview_refresh = false;
        let mut requested_channel_view: Option<ChannelView> = None;
        let channel_count = self.tabs[tab_idx].ch_samples.len();
        ui.horizontal_wrapped(|ui| {
            let tab = &mut self.tabs[tab_idx];
            // Loop mode toggles (kept): Off / OnWhole / Marker
            ui.label("Loop:");
            for (m, label) in [
                (LoopMode::Off, "Off"),
                (LoopMode::OnWhole, "On"),
                (LoopMode::Marker, "Marker"),
            ] {
                if ui.selectable_label(tab.loop_mode == m, label).clicked() {
                    tab.loop_mode = m;
                    apply_pending_loop = true;
                }
            }
            ui.separator();
            let prev_view = tab.leaf_view_mode();
            let mut selected_view = prev_view;
            ui.horizontal_wrapped(|ui| {
                ui.label("View:");
                egui::ComboBox::from_id_salt(("editor_view_select", tab_idx))
                    .selected_text(Self::editor_view_label(selected_view))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut selected_view, ViewMode::Waveform, "Wave");
                        ui.separator();
                        ui.label(RichText::new("Spectral").weak());
                        ui.selectable_value(&mut selected_view, ViewMode::Spectrogram, "Spec");
                        ui.selectable_value(&mut selected_view, ViewMode::Log, "Freq Log");
                        ui.selectable_value(&mut selected_view, ViewMode::Mel, "Mel");
                        ui.separator();
                        ui.label(RichText::new("Other").weak());
                        ui.selectable_value(&mut selected_view, ViewMode::Tempogram, "Tempogram");
                        ui.selectable_value(&mut selected_view, ViewMode::Chromagram, "Chromagram");
                    });
            });
            if selected_view != prev_view {
                let prev_preview_supported =
                    Self::view_supports_wave_preview(prev_view, tab.show_waveform_overlay);
                tab.set_leaf_view_mode(selected_view);
                let next_preview_supported =
                    Self::view_supports_wave_preview(selected_view, tab.show_waveform_overlay);
                if prev_preview_supported && !next_preview_supported {
                    discard_preview_for_view_change = true;
                } else if !prev_preview_supported
                    && next_preview_supported
                    && Self::tool_supports_preview(tab.active_tool)
                {
                    request_preview_refresh = true;
                }
            }
            ui.separator();
            // Channel view toggles
            if channel_count > 0 {
                let mut view = tab.channel_view.clone();
                let mut view_changed = false;
                ui.label("Ch:");
                if ui
                    .selectable_label(view.mode == ChannelViewMode::Mixdown, "Mix")
                    .clicked()
                {
                    view.mode = ChannelViewMode::Mixdown;
                    view_changed = true;
                }
                if ui
                    .selectable_label(view.mode == ChannelViewMode::All, "All")
                    .clicked()
                {
                    view.mode = ChannelViewMode::All;
                    view_changed = true;
                }
                ui.menu_button("Select", |ui| {
                    let mut selection_changed = false;
                    for idx in 0..channel_count {
                        let label = format!("Ch {}", idx + 1);
                        let mut selected = view.selected.contains(&idx);
                        if ui.checkbox(&mut selected, label).changed() {
                            selection_changed = true;
                            if selected {
                                if !view.selected.contains(&idx) {
                                    view.selected.push(idx);
                                }
                            } else {
                                view.selected.retain(|&v| v != idx);
                            }
                        }
                    }
                    if ui.button("Clear").clicked() {
                        view.selected.clear();
                        selection_changed = true;
                    }
                    if selection_changed {
                        view.mode = if view.selected.is_empty() {
                            ChannelViewMode::Mixdown
                        } else {
                            ChannelViewMode::Custom
                        };
                        view_changed = true;
                    }
                });
                if view_changed {
                    view.selected.retain(|&idx| idx < channel_count);
                    requested_channel_view = Some(view);
                }
            }
            ui.separator();
            let mut bpm_enabled = tab.bpm_enabled;
            if ui.checkbox(&mut bpm_enabled, "BPM").changed() {
                tab.bpm_enabled = bpm_enabled;
            }
            let mut bpm_value = tab.bpm_value;
            let bpm_resp = ui.add(
                egui::DragValue::new(&mut bpm_value)
                    .range(0.0..=300.0)
                    .speed(0.1)
                    .fixed_decimals(2)
                    .suffix(" BPM"),
            );
            if bpm_resp.changed() {
                tab.bpm_value = bpm_value.max(0.0);
                tab.bpm_user_set = true;
            }
            ui.label("Offset:");
            let mut bpm_offset_sec = tab.bpm_offset_sec;
            if ui
                .add(
                    egui::DragValue::new(&mut bpm_offset_sec)
                        .range(-30.0..=30.0)
                        .speed(0.01)
                        .fixed_decimals(2)
                        .suffix(" s"),
                )
                .changed()
            {
                tab.bpm_offset_sec = bpm_offset_sec.clamp(-30.0, 30.0);
            }
            ui.separator();
            // Time HUD: play position (editable) / total length
            let sr = sr_ctx.max(1.0); // restore local sample-rate alias after removing top-level Loop block
            let mut pos_sec = playhead_display_now as f32 / sr;
            let mut len_sec = (tab_samples_len as f32 / sr).max(0.0);
            if !pos_sec.is_finite() {
                pos_sec = 0.0;
            }
            if !len_sec.is_finite() {
                len_sec = 0.0;
            }
            if pos_sec > len_sec {
                pos_sec = len_sec;
            }
            ui.label("Pos:");
            let pos_resp = ui.add(
                egui::DragValue::new(&mut pos_sec)
                    .range(0.0..=len_sec)
                    .speed(0.05)
                    .fixed_decimals(2),
            );
            if pos_resp.changed() {
                let display_samp = (pos_sec.max(0.0) * sr) as usize;
                let audio_samp = map_display_to_audio(tab, display_samp);
                request_seek = Some(audio_samp);
            }
            let pos_samples = (pos_sec.max(0.0) * sr).round() as usize;
            ui.label(
                RichText::new(format!(
                    " ({pos_samples} smp) / {} ({tab_samples_len} smp)",
                    crate::app::helpers::format_time_s(len_sec),
                ))
                .monospace(),
            );
        });
        if let Some(view) = requested_channel_view.take() {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.channel_view = view;
            }
            let path = self.tabs[tab_idx].path.clone();
            self.cancel_spectrogram_for_path(&path);
        }
        ui.separator();
        let _len_sec = if sr_ctx > 0.0 {
            (tab_samples_len as f32 / sr_ctx).max(0.0)
        } else {
            0.0
        };
        if !ctx.wants_keyboard_input() && tab_samples_len > 0 {
            let mods = ctx.input(|i| i.modifiers);
            let ctrl = mods.ctrl || mods.command;
            let shift = mods.shift;
            let alt = mods.alt;
            let pressed_left = ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft));
            let pressed_right = ctx.input(|i| i.key_pressed(egui::Key::ArrowRight));
            let left_down = ctx.input(|i| i.key_down(egui::Key::ArrowLeft));
            let right_down = ctx.input(|i| i.key_down(egui::Key::ArrowRight));
            let dir = if left_down ^ right_down {
                if right_down {
                    1
                } else {
                    -1
                }
            } else {
                0
            };
            let mut hold = self.tabs[tab_idx].seek_hold.take();
            if dir != 0 {
                let now = std::time::Instant::now();
                let pressed = if dir > 0 { pressed_right } else { pressed_left };
                let repeat_delay = std::time::Duration::from_millis(220);
                let repeat_fast = std::time::Duration::from_millis(35);
                let repeat_slow = std::time::Duration::from_millis(70);
                let mut should_step = pressed;
                let mut hold_state = match hold.take() {
                    Some(mut state) => {
                        if state.dir != dir {
                            state = SeekHoldState {
                                dir,
                                started_at: now,
                                last_step_at: now,
                            };
                            should_step = true;
                        } else if !pressed {
                            let elapsed = now.saturating_duration_since(state.started_at);
                            let since = now.saturating_duration_since(state.last_step_at);
                            let interval = if elapsed >= std::time::Duration::from_millis(650) {
                                repeat_fast
                            } else {
                                repeat_slow
                            };
                            if elapsed >= repeat_delay && since >= interval {
                                should_step = true;
                            }
                        }
                        state
                    }
                    None => {
                        should_step = true;
                        SeekHoldState {
                            dir,
                            started_at: now,
                            last_step_at: now,
                        }
                    }
                };
                if should_step {
                    let cur_display = playhead_display_now;
                    let spp = self.tabs[tab_idx].samples_per_px.max(0.0001);
                    let sr_u32 = self.audio.shared.out_sample_rate.max(1);
                    let sr = sr_u32 as f32;
                    let px_per_sec = (1.0 / spp) * sr;
                    let sample_step_sec = 1.0 / sr;
                    let sample_step = 1usize;
                    let time_grid_step = |min_px: f32| -> f32 {
                        let steps: [f32; 18] = [
                            0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0,
                            10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
                        ];
                        let mut step = steps[steps.len() - 1];
                        for s in steps {
                            if px_per_sec * s >= min_px {
                                step = s;
                                break;
                            }
                        }
                        step
                    };
                    let tab_bpm_enabled = self.tabs[tab_idx].bpm_enabled;
                    let bpm_value = self.tabs[tab_idx].bpm_value.max(1.0);
                    let use_bpm = tab_bpm_enabled && bpm_value >= 20.0;
                    let base_step_sec = if use_bpm {
                        let beat_sec = 60.0 / bpm_value;
                        let steps: [f32; 10] = [
                            1.0 / 64.0,
                            1.0 / 32.0,
                            1.0 / 16.0,
                            1.0 / 8.0,
                            1.0 / 4.0,
                            0.5,
                            1.0,
                            2.0,
                            4.0,
                            8.0,
                        ];
                        let px_per_beat = px_per_sec * beat_sec;
                        let mut step_beats = steps[steps.len() - 1];
                        for s in steps {
                            if px_per_beat * s >= 90.0 {
                                step_beats = s;
                                break;
                            }
                        }
                        (beat_sec * step_beats).max(sample_step_sec)
                    } else {
                        let mut base = time_grid_step(90.0);
                        if spp <= 1.0 {
                            base = sample_step_sec;
                        }
                        base.max(sample_step_sec)
                    };
                    let base_step_samples =
                        ((base_step_sec * sr).round() as usize).max(sample_step);
                    let raw_target = if alt && !ctrl {
                        // Alt: zero-cross move/range.
                        self.find_zero_cross_display(tab_idx, cur_display, dir)
                    } else if ctrl && alt {
                        // Ctrl+Alt: relative move (unsnapped grid step).
                        if dir > 0 {
                            cur_display.saturating_add(base_step_samples)
                        } else {
                            cur_display.saturating_sub(base_step_samples)
                        }
                    } else {
                        let step_samples = if ctrl { sample_step } else { base_step_samples };
                        if ctrl || shift {
                            if dir > 0 {
                                cur_display.saturating_add(step_samples)
                            } else {
                                cur_display.saturating_sub(step_samples)
                            }
                        } else if dir > 0 {
                            let target = cur_display.saturating_add(step_samples);
                            (target / step_samples) * step_samples
                        } else if cur_display == 0 {
                            0
                        } else {
                            let target = cur_display.saturating_sub(1);
                            (target / step_samples) * step_samples
                        }
                    };
                    let mut new_display = raw_target.min(tab_samples_len);
                    new_display = Self::stop_with_marker_if_needed(
                        &self.tabs[tab_idx],
                        cur_display,
                        new_display,
                        dir,
                    );
                    if shift {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            let anchor = if ctrl && alt {
                                if let Some((a0, b0)) = tab.selection {
                                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                    if dir > 0 {
                                        a
                                    } else {
                                        b
                                    }
                                } else {
                                    Self::editor_selection_anchor_or(tab, cur_display)
                                }
                            } else {
                                Self::editor_selection_anchor_or(tab, cur_display)
                            };
                            Self::editor_set_selection_from_anchor(tab, anchor, new_display);
                        }
                    }
                    if new_display != cur_display {
                        if let Some(tab) = self.tabs.get(tab_idx) {
                            request_seek = Some(map_display_to_audio(tab, new_display));
                        }
                    }
                    hold_state.last_step_at = now;
                }
                hold = Some(hold_state);
            }
            self.tabs[tab_idx].seek_hold = hold;
        } else if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.seek_hold = None;
        }
        if self
            .tabs
            .get(tab_idx)
            .map(|tab| tab.active_tool == ToolKind::PluginFx)
            .unwrap_or(false)
        {
            self.request_plugin_scan_if_needed();
        }

        let avail = ui.available_size();
        // pending actions to perform after UI borrows end
        let mut do_set_loop_from: Option<(usize, usize)> = None;
        let mut do_trim: Option<(usize, usize)> = None; // keep-only (optional)
        let mut do_trim_virtual: Option<(usize, usize)> = None;
        let do_fade: Option<((usize, usize), f32, f32)> = None; // legacy whole-file fade
        let mut do_gain: Option<((usize, usize), f32)> = None;
        let mut do_normalize: Option<((usize, usize), f32)> = None;
        let mut do_reverse: Option<(usize, usize)> = None;
        let mut do_mute: Option<(usize, usize)> = None;
        let mut do_cutjoin: Option<(usize, usize)> = None;
        // Loop/marker apply handled via commit flags below.
        let mut do_fade_in: Option<((usize, usize), crate::app::types::FadeShape)> = None;
        let mut do_fade_out: Option<((usize, usize), crate::app::types::FadeShape)> = None;
        let mut stop_playback = false;
        // Snapshot busy state and prepare deferred overlay job.
        // IMPORTANT: Do NOT call `self.*` (which takes &mut self) while holding `let tab = &mut self.tabs[...]`.
        // That pattern triggers borrow checker error E0499. Defer such calls to after the UI closures.
        let tab_path = self.tabs[tab_idx].path.clone();
        let plugin_catalog = self.plugin_catalog.clone();
        let plugin_search_paths = self.plugin_search_paths.clone();
        let mut plugin_search_path_input = self.plugin_search_path_input.clone();
        let plugin_scan_busy = self.plugin_scan_state.is_some();
        let plugin_scan_error = self.plugin_scan_error.clone();
        let plugin_probe_busy = self
            .plugin_probe_state
            .as_ref()
            .map(|s| s.tab_path == tab_path)
            .unwrap_or(false);
        let plugin_preview_busy = self
            .plugin_process_state
            .as_ref()
            .map(|s| s.tab_idx == tab_idx && !s.is_apply)
            .unwrap_or(false);
        let plugin_apply_busy = self
            .plugin_process_state
            .as_ref()
            .map(|s| s.tab_idx == tab_idx && s.is_apply)
            .unwrap_or(false);
        let overlay_busy =
            self.current_tab_preview_busy(tab_idx) || self.music_preview_state.is_some();
        let apply_busy = self.editor_apply_state.is_some() || plugin_apply_busy;
        let mut pending_overlay_job: Option<(ToolKind, f32)> = None;
        let mut pending_overlay_path: Option<(ToolKind, PathBuf, f32)> = None;
        let music_model_ready = self.music_ai_has_model();
        let music_demucs_ready = self.music_ai_has_demucs_model();
        let music_model_downloading = self.music_model_download_state.is_some();
        let music_model_dir_text = self
            .music_ai_model_dir
            .as_ref()
            .map(|p| p.display().to_string());
        let music_can_uninstall = self.music_ai_can_uninstall();
        let music_analyze_running = self.music_ai_state.is_some();
        let music_run_status = self.music_analysis_status_text();
        let music_run_process = self.music_analysis_process_text();
        let mut pending_music_model_download = false;
        let mut pending_music_model_uninstall = false;
        let mut pending_music_analyze_start = false;
        let mut pending_music_analyze_cancel = false;
        let mut pending_music_preview_cancel = false;
        let mut pending_music_rebuild_markers = false;
        let mut pending_music_preview_refresh = false;
        let mut pending_music_apply_markers = false;
        let mut pending_music_apply_preview = false;
        let mut request_undo = false;
        let mut request_redo = false;
        let gain_db = self
            .tabs
            .get(tab_idx)
            .map(|tab| self.pending_gain_db_for_path(&tab.path))
            .unwrap_or(0.0);
        let apply_msg = if let Some(state) = self.editor_apply_state.as_ref() {
            Some(state.msg.clone())
        } else if plugin_apply_busy {
            Some("Applying Plugin FX...".to_string())
        } else {
            None
        };
        let decode_status = self.editor_decode_ui_status(Some(tab_path.as_path()));
        let processing_msg = self
            .processing
            .as_ref()
            .filter(|p| p.path == tab_path)
            .map(|p| (p.msg.clone(), p.started_at));
        let preview_msg = if let Some(msg) = self.current_tab_preview_message(tab_idx) {
            Some(msg)
        } else if self.music_preview_state.is_some() {
            Some("Previewing Music Analyze...".to_string())
        } else if plugin_preview_busy {
            Some("Previewing Plugin FX...".to_string())
        } else {
            None
        };
        let spectro_loading = self.spectro_inflight.contains(&tab_path);
        let spectro_progress = self
            .spectro_progress
            .get(&tab_path)
            .map(|p| (p.done_tiles, p.total_tiles, p.started_at));
        let analysis_loading = match current_view {
            ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => spectro_loading,
            ViewMode::Tempogram | ViewMode::Chromagram => feature_loading,
            ViewMode::Waveform => false,
        };
        let analysis_progress = match current_view {
            ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => spectro_progress,
            ViewMode::Tempogram | ViewMode::Chromagram => feature_progress,
            ViewMode::Waveform => None,
        };
        let analysis_label = match current_view {
            ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => "Spectrogram",
            ViewMode::Tempogram => "Tempogram",
            ViewMode::Chromagram => "Chromagram",
            ViewMode::Waveform => "",
        };
        let mut cancel_apply = false;
        let mut cancel_decode = false;
        let mut cancel_processing = false;
        let mut cancel_preview = false;
        let mut cancel_spectro = false;
        let mut cancel_feature_analysis = false;
        let mut pending_tempogram_refresh = false;
        let mut pending_chromagram_refresh = false;
        let mut apply_estimated_bpm: Option<f32> = None;
        let mut perf_mixdown_ms: Option<f32> = None;
        let mut perf_wave_render_ms: Option<f32> = None;
        let mut waveform_render_started: Option<std::time::Instant> = None;
        let mut waveform_scratch = wf_cache::WaveformScratch::default();
        let mut waveform_query_ms_total = 0.0f32;
        let mut waveform_draw_ms_total = 0.0f32;
        let pixels_per_point = ctx.pixels_per_point();
        // Split canvas and inspector; keep inspector visible on narrow widths.
        let min_canvas_w = 160.0f32;
        let min_inspector_w = 220.0f32;
        let max_inspector_w = 360.0f32;
        let split_spacing = ui.spacing().item_spacing.x;
        let split_avail_w = (avail.x - split_spacing).max(0.0);
        let inspector_w = if split_avail_w <= min_inspector_w {
            split_avail_w
        } else {
            let available = (split_avail_w - min_canvas_w).max(min_inspector_w);
            available.min(max_inspector_w).min(split_avail_w)
        };
        let canvas_w = (split_avail_w - inspector_w).max(0.0);
        ui.horizontal_top(|ui| {
                let tab = &mut self.tabs[tab_idx];
                let preview_ok = tab.samples_len <= LIVE_PREVIEW_SAMPLE_LIMIT;
                let simplified_preview_note = if !preview_ok {
                    Some("Long clip: simplified waveform preview")
                } else {
                    None
                };
                let preview_disabled_reason = if apply_busy {
                    Some("Preview unavailable while Apply is running")
                } else if preview_msg.is_some() || plugin_preview_busy {
                    Some("Preview unavailable while another preview is running")
                } else {
                    None
                };
                let preview_button_enabled = preview_disabled_reason.is_none();
                // Canvas area
                let mut need_restore_preview = false;
                // Accumulate non-destructive preview audio to audition.
                // Carry the tool kind to keep preview state consistent.
                let mut pending_preview: Option<(ToolKind, Vec<f32>)> = None;
                let mut pending_heavy_preview: Option<(ToolKind, Vec<f32>, f32)> = None;
                let mut pending_heavy_preview_path: Option<(ToolKind, PathBuf, f32)> = None;
                let mut pending_pitch_apply: Option<f32> = None;
                let mut pending_stretch_apply: Option<f32> = None;
                let mut pending_loudness_apply: Option<f32> = None;
                let mut pending_plugin_scan = false;
                let mut pending_plugin_probe: Option<String> = None;
                let mut pending_plugin_preview = false;
                let mut pending_plugin_apply = false;
                let mut pending_plugin_gui_open = false;
                let mut pending_plugin_gui_sync = false;
                let mut pending_plugin_gui_close = false;
                let mut pending_plugin_add_path: Option<PathBuf> = None;
                let mut pending_plugin_remove_index: Option<usize> = None;
                let mut pending_plugin_reset_paths = false;
                let mut pending_plugin_pick_folder = false;
                if discard_preview_for_view_change {
                    need_restore_preview = true;
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(canvas_w, avail.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                    let canvas_h = (canvas_w * 0.35).clamp(180.0, avail.y);
                    let (resp, painter) = ui.allocate_painter(egui::vec2(canvas_w, canvas_h), Sense::click_and_drag());
                    let rect = resp.rect;
                    let w = rect.width().max(1.0); let h = rect.height().max(1.0);
                    let mut hover_cursor: Option<egui::CursorIcon> = None;
                    painter.rect_filled(rect, 0.0, Color32::from_rgb(16,16,18));
                    // Layout parameters
                    let view_mode = tab.leaf_view_mode();
                    let gutter_w = 44.0;
                    let show_amplitude_navigator = matches!(
                        view_mode,
                        ViewMode::Waveform | ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel
                    );
                    let amplitude_nav_gap = if show_amplitude_navigator { 6.0 } else { 0.0 };
                    let amplitude_nav_right_pad = if show_amplitude_navigator { 6.0 } else { 0.0 };
                    let amplitude_nav_strip_w = if show_amplitude_navigator { 18.0 } else { 0.0 };
                    let amplitude_nav_reserved_w =
                        amplitude_nav_gap + amplitude_nav_right_pad + amplitude_nav_strip_w;
                    let wave_left = rect.left() + gutter_w;
                    let wave_w = (w - gutter_w - amplitude_nav_reserved_w).max(1.0);
                    let amplitude_nav_rect = if show_amplitude_navigator {
                        Some(egui::Rect::from_min_max(
                            egui::pos2(rect.right() - amplitude_nav_right_pad - amplitude_nav_strip_w, rect.top() + 10.0),
                            egui::pos2(rect.right() - amplitude_nav_right_pad, rect.bottom() - 10.0),
                        ))
                    } else {
                        None
                    };
                    tab.last_amplitude_nav_rect = amplitude_nav_rect;
                    tab.last_amplitude_viewport_rect = amplitude_nav_rect.map(|nav_rect| {
                        Self::amplitude_nav_viewport_rect(
                            nav_rect,
                            tab.vertical_zoom,
                            tab.vertical_view_center,
                        )
                    });
                        let channel_count = tab.ch_samples.len().max(1);
                        let mut visible_channels = tab.channel_view.visible_indices(channel_count);
                        let force_feature_mixdown =
                            matches!(view_mode, ViewMode::Tempogram | ViewMode::Chromagram);
                        let use_mixdown = force_feature_mixdown
                            || tab.channel_view.mode == ChannelViewMode::Mixdown
                            || visible_channels.is_empty();
                        if use_mixdown {
                            visible_channels.clear();
                        }
                        let lane_count = if force_feature_mixdown || use_mixdown {
                            1
                        } else {
                            visible_channels.len().max(1)
                        };
                        let lane_h = h / lane_count as f32;

                    // Visual amplitude scale: assume Volume=0 dB for display; apply per-file Gain only
                    let scale = db_to_amp(gain_db);

                    // Initialize zoom to fit if unset (show whole file)
                    let display_samples_len = if tab.loading && tab.samples_len_visual > 0 {
                        tab.samples_len_visual
                    } else {
                        tab.samples_len
                    };
                    if display_samples_len > 0 && tab.samples_per_px <= 0.0 {
                        let fit_spp = (display_samples_len as f32 / wave_w.max(1.0))
                            .max(crate::app::EDITOR_MIN_SAMPLES_PER_PX);
                        tab.samples_per_px = fit_spp;
                        Self::editor_set_view_offset(tab, 0);
                    }
                    // Keep the same center sample anchored when the window width changes.
                    if display_samples_len > 0 {
                        let spp = tab.samples_per_px.max(0.0001);
                        let old_wave_w = tab.last_wave_w;
                        if old_wave_w > 0.0 && (old_wave_w - wave_w).abs() > 0.5 {
                            let old_geom = EditorDisplayGeometry::new(
                                wave_left,
                                old_wave_w,
                                spp,
                                tab.view_offset,
                                tab.view_offset_exact,
                                display_samples_len,
                            );
                            let new_geom = EditorDisplayGeometry::new(
                                wave_left,
                                wave_w,
                                spp,
                                tab.view_offset,
                                tab.view_offset_exact,
                                display_samples_len,
                            );
                            let old_vis = old_geom.visible_count;
                            let new_vis = new_geom.visible_count;
                            if old_vis > 0 && new_vis > 0 {
                                let anchor = old_geom
                                    .x_to_display_sample(wave_left + old_wave_w * 0.5)
                                    .min(display_samples_len.saturating_sub(1));
                                let next_exact = Self::editor_exact_view_for_anchor(
                                    anchor,
                                    0.5,
                                    wave_w,
                                    spp,
                                );
                                Self::editor_set_view_offset_exact(tab, next_exact, new_geom.max_left());
                            }
                        }
                        tab.last_wave_w = wave_w;
                    } else {
                        tab.last_wave_w = wave_w;
                    }

            let geom = EditorDisplayGeometry::new(
                wave_left,
                wave_w,
                tab.samples_per_px,
                tab.view_offset,
                tab.view_offset_exact,
                display_samples_len,
            );
            let spp = geom.spp;
            let start = geom.visible_start();
            let end = geom.visible_end();
            let wave_width_px =
                Self::editor_viewport_dimension_px(wave_w, pixels_per_point);
            let lane_height_px =
                Self::editor_viewport_dimension_px(lane_h, pixels_per_point);
            pending_viewport_hint = matches!(
                view_mode,
                ViewMode::Spectrogram
                    | ViewMode::Log
                    | ViewMode::Mel
                    | ViewMode::Tempogram
                    | ViewMode::Chromagram
            )
            .then_some(crate::app::editor_viewport::EditorViewportHint {
                view_mode,
                display_samples_len,
                start,
                end,
                wave_width_px,
                lane_height_px,
                lane_count,
                use_mixdown,
                visible_channels: visible_channels.clone(),
            });
            let current_feature_viewport_key = if matches!(
                view_mode,
                ViewMode::Spectrogram
                    | ViewMode::Log
                    | ViewMode::Mel
                    | ViewMode::Tempogram
                    | ViewMode::Chromagram
            ) {
                    Some(EditorViewportRenderKey {
                        kind: EditorViewportPayloadKind::Spectral,
                        view_mode,
                        source_generation: tab.viewport_source_generation,
                        display_samples_len,
                        start,
                        end,
                        lane_count: lane_count.max(1),
                        lane_height_px,
                        wave_width_px,
                        use_mixdown,
                        visible_channels: visible_channels.clone(),
                        samples_per_px_bits: tab.samples_per_px.to_bits(),
                        vertical_zoom_bits: tab.vertical_zoom.to_bits(),
                        vertical_view_center_bits: tab.vertical_view_center.to_bits(),
                        scale_bits: 0,
                        spectro_cfg_digest: Self::editor_spectro_cfg_digest(&self.spectro_cfg),
                    })
                } else {
                    None
                };

            // Time ruler (ticks + labels) across all lanes
            {
                if end > start {
                    let sr = sr_ctx.max(1.0);
                    let t0 = start as f32 / sr;
                    let t1 = end as f32 / sr;
                    let px_per_sec = (1.0 / spp) * sr;
                    let min_px = 90.0;
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    let grid_col = Color32::from_rgb(38,38,44);
                    let label_col = Color32::GRAY;
                    if tab.bpm_enabled && tab.bpm_value >= 20.0 {
                        let bpm = tab.bpm_value.max(1.0);
                        let beat_sec = 60.0 / bpm;
                        let offset_sec = tab.bpm_offset_sec;
                        let px_per_beat = px_per_sec * beat_sec;
                        let steps: [f32; 10] = [1.0/64.0, 1.0/32.0, 1.0/16.0, 1.0/8.0, 1.0/4.0, 0.5, 1.0, 2.0, 4.0, 8.0];
                        let mut step_beats = steps[steps.len() - 1];
                        for s in steps {
                            if px_per_beat * s >= min_px {
                                step_beats = s;
                                break;
                            }
                        }
                        let b0 = (t0 - offset_sec) / beat_sec;
                        let b1 = (t1 - offset_sec) / beat_sec;
                        let start_beat = (b0 / step_beats).floor() * step_beats;
                        let mut beat = start_beat;
                        let label_every = if step_beats < 0.25 {
                            1.0
                        } else if step_beats < 1.0 {
                            1.0
                        } else {
                            step_beats
                        };
                        while beat <= b1 + step_beats * 0.5 {
                            let t = offset_sec + beat * beat_sec;
                            if t < t0 || t > t1 {
                                beat += step_beats;
                                continue;
                            }
                            let s_idx = (t * sr).round() as isize;
                            let x = geom.sample_boundary_x(s_idx.max(0) as usize);
                            painter.line_segment(
                                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                                egui::Stroke::new(1.0, grid_col),
                            );
                            if px_per_beat * step_beats >= 70.0
                                && ((beat / label_every).round() * label_every - beat).abs() < 1e-6
                            {
                                let label = if label_every >= 1.0 {
                                    format!("{:.0}b", beat)
                                } else {
                                    format!("{:.2}b", beat)
                                };
                                painter.text(
                                    egui::pos2(x + 2.0, rect.top() + 2.0),
                                    egui::Align2::LEFT_TOP,
                                    label,
                                    fid.clone(),
                                    label_col,
                                );
                            }
                            beat += step_beats;
                        }
                    } else {
                        let steps: [f32; 15] = [0.01,0.02,0.05,0.1,0.2,0.5,1.0,2.0,5.0,10.0,15.0,30.0,60.0,120.0,300.0];
                        let mut step = steps[steps.len()-1];
                        for s in steps { if px_per_sec * s >= min_px { step = s; break; } }
                        let start_tick = (t0 / step).floor() * step;
                        let mut t = start_tick;
                        while t <= t1 + step*0.5 {
                            let s_idx = (t * sr).round() as isize;
                            let x = geom.sample_boundary_x(s_idx.max(0) as usize);
                            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0, grid_col));
                            // Label near top; avoid overcrowding by skipping when too dense
                            if px_per_sec * step >= 70.0 {
                                let label = crate::app::helpers::format_time_s(t);
                                painter.text(egui::pos2(x + 2.0, rect.top() + 2.0), egui::Align2::LEFT_TOP, label, fid.clone(), label_col);
                            }
                            t += step;
                        }
                    }
                }
            }

            if view_mode != ViewMode::Waveform {
                match view_mode {
                    ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => {
                        if let Some(specs) = spec_cache.as_ref() {
                            touch_spectro_cache = true;
                            let current_viewport_cache = current_feature_viewport_key
                                .as_ref()
                                .and_then(|key| Self::exact_editor_viewport_cache(tab, key));
                            if let Some(crate::app::types::EditorViewportRenderCache {
                                payload:
                                    crate::app::types::EditorViewportCachePayload::Image {
                                        texture: Some(texture),
                                        ..
                                    },
                                ..
                            }) = current_viewport_cache
                            {
                                painter.image(
                                    texture.id(),
                                    egui::Rect::from_min_size(
                                        egui::pos2(wave_left, rect.top()),
                                        egui::vec2(wave_w, lane_h * lane_count as f32),
                                    ),
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    Color32::WHITE,
                                );
                            } else {
                                let lane_spec_indices = if use_mixdown {
                                    vec![0usize; lane_count.max(1)]
                                } else if visible_channels.is_empty() {
                                    (0..lane_count.max(1)).collect::<Vec<_>>()
                                } else {
                                    visible_channels
                                        .iter()
                                        .copied()
                                        .take(lane_count.max(1))
                                        .collect::<Vec<_>>()
                                };
                                let fallback_image = Self::render_spectral_viewport_image(
                                    specs,
                                    &lane_spec_indices,
                                    wave_width_px,
                                    lane_height_px,
                                    lane_count.max(1),
                                    start,
                                    end,
                                    tab.vertical_zoom,
                                    tab.vertical_view_center,
                                    &self.spectro_cfg,
                                    view_mode,
                                    crate::app::types::EditorViewportRenderQuality::Coarse,
                                );
                                let fallback_texture = ui.ctx().load_texture(
                                    format!("editor_viewport_sync_fallback_{tab_idx}_{view_mode:?}"),
                                    fallback_image,
                                    egui::TextureOptions::LINEAR,
                                );
                                painter.image(
                                    fallback_texture.id(),
                                    egui::Rect::from_min_size(
                                        egui::pos2(wave_left, rect.top()),
                                        egui::vec2(wave_w, lane_h * lane_count as f32),
                                    ),
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    Color32::WHITE,
                                );
                                if spec_loading {
                                    let fid = TextStyle::Monospace.resolve(ui.style());
                                    painter.text(
                                        egui::pos2(wave_left + 6.0, rect.top() + 6.0),
                                        egui::Align2::LEFT_TOP,
                                        "Building spectrogram...",
                                        fid,
                                        Color32::GRAY,
                                    );
                                }
                            }
                        } else {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let msg = if spec_loading {
                                "Building spectrogram..."
                            } else {
                                "Spectrogram not ready"
                            };
                            painter.text(
                                egui::pos2(wave_left + 6.0, rect.top() + 6.0),
                                egui::Align2::LEFT_TOP,
                                msg,
                                fid,
                                Color32::GRAY,
                            );
                        }
                        if !tab.show_waveform_overlay {
                            let sr = spec_cache
                                .as_ref()
                                .and_then(|specs| specs.first())
                                .map(|spec| spec.sample_rate)
                                .unwrap_or(self.audio.shared.out_sample_rate);
                            let mut max_freq = (sr.max(1) as f32) * 0.5;
                            if self.spectro_cfg.max_freq_hz > 0.0 {
                                max_freq = self.spectro_cfg.max_freq_hz.min(max_freq).max(1.0);
                            }
                            let log_min = 20.0_f32.min(max_freq).max(1.0);
                            let ticks_hz: [f32; 10] = [
                                0.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0,
                                20000.0,
                            ];
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let tick_col = Color32::from_rgb(140, 150, 165);
                            let tick_stroke = egui::Stroke::new(1.0, tick_col);
                            let freq_to_note_label = |freq: f32| -> String {
                                if freq <= 0.0 {
                                    return String::new();
                                }
                                let note_f = 69.0 + 12.0 * (freq / 440.0).log2();
                                let note = note_f.round() as i32;
                                if !(0..=127).contains(&note) {
                                    return String::new();
                                }
                                let names = [
                                    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#",
                                    "B",
                                ];
                                let idx = ((note % 12) + 12) % 12;
                                let octave = (note / 12) - 1;
                                format!("{}{}", names[idx as usize], octave)
                            };
                            let format_freq = |freq: f32| -> String {
                                if freq >= 1000.0 {
                                    let k = freq / 1000.0;
                                    if (k - k.round()).abs() < 0.05 {
                                        format!("{:.0}k", k)
                                    } else {
                                        format!("{:.1}k", k)
                                    }
                                } else {
                                    format!("{:.0}", freq)
                                }
                            };
                            let mel_max = 2595.0 * (1.0 + max_freq / 700.0).log10();
                            let mel_min = 1.0_f32;
                            let (visible_min, visible_max) = Self::editor_vertical_range_for_view(
                                view_mode,
                                tab.vertical_zoom,
                                tab.vertical_view_center,
                                &self.spectro_cfg,
                            );
                            for ci in 0..lane_count {
                                let lane_top = rect.top() + lane_h * ci as f32;
                                let lane_rect = egui::Rect::from_min_size(
                                    egui::pos2(wave_left, lane_top),
                                    egui::vec2(wave_w, lane_h),
                                );
                                let mut last_y = f32::INFINITY;
                                for &freq in &ticks_hz {
                                    if freq <= 0.0 || freq > max_freq {
                                        if freq != 0.0 {
                                            continue;
                                        }
                                    }
                                    let frac = match view_mode {
                                        ViewMode::Spectrogram => (freq / max_freq).clamp(0.0, 1.0),
                                        ViewMode::Log => {
                                            if freq <= 0.0 || max_freq <= log_min {
                                                0.0
                                            } else {
                                                let f = freq.clamp(log_min, max_freq);
                                                (f / log_min).ln() / (max_freq / log_min).ln()
                                            }
                                        }
                                        ViewMode::Mel => match self.spectro_cfg.mel_scale {
                                            SpectrogramScale::Linear => {
                                                let mel = 2595.0 * (1.0 + (freq / 700.0)).log10();
                                                (mel / mel_max).clamp(0.0, 1.0)
                                            }
                                            SpectrogramScale::Log => {
                                                let mel = 2595.0 * (1.0 + (freq / 700.0)).log10();
                                                if mel_max <= mel_min {
                                                    (mel / mel_max.max(1.0)).clamp(0.0, 1.0)
                                                } else {
                                                    (mel / mel_min)
                                                        .ln()
                                                        .clamp(0.0, (mel_max / mel_min).ln())
                                                        / (mel_max / mel_min).ln()
                                                }
                                            }
                                        },
                                        _ => 0.0,
                                    };
                                    let visible_frac = if (visible_max - visible_min).abs()
                                        < f32::EPSILON
                                    {
                                        0.0
                                    } else {
                                        ((frac - visible_min) / (visible_max - visible_min))
                                            .clamp(0.0, 1.0)
                                    };
                                    let y = lane_rect.bottom() - visible_frac * lane_rect.height();
                                    if last_y.is_finite() && (last_y - y) < 12.0 {
                                        continue;
                                    }
                                    let label = if self.spectro_cfg.show_note_labels {
                                        let note = freq_to_note_label(freq);
                                        if note.is_empty() {
                                            format_freq(freq)
                                        } else {
                                            format!("{} {}", format_freq(freq), note)
                                        }
                                    } else {
                                        format_freq(freq)
                                    };
                                    painter.line_segment(
                                        [
                                            egui::pos2(wave_left - 6.0, y),
                                            egui::pos2(wave_left - 2.0, y),
                                        ],
                                        tick_stroke,
                                    );
                                    painter.text(
                                        egui::pos2(rect.left() + 2.0, y),
                                        egui::Align2::LEFT_CENTER,
                                        label,
                                        fid.clone(),
                                        tick_col,
                                    );
                                    last_y = y;
                                }
                            }
                        }
                    }
                    ViewMode::Tempogram => {
                        let lane_rect = egui::Rect::from_min_size(
                            egui::pos2(wave_left, rect.top()),
                            egui::vec2(wave_w, h),
                        );
                        let current_viewport_cache = current_feature_viewport_key
                            .as_ref()
                            .and_then(|key| Self::exact_editor_viewport_cache(tab, key));
                        if let Some(crate::app::types::EditorViewportRenderCache {
                            payload:
                                crate::app::types::EditorViewportCachePayload::Image {
                                    texture: Some(texture),
                                    ..
                                },
                            ..
                        }) = current_viewport_cache
                        {
                            painter.image(
                                texture.id(),
                                lane_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                Color32::WHITE,
                            );
                        } else if let Some(data) = tempogram_data.as_ref() {
                            let fallback_image = Self::render_tempogram_viewport_image(
                                data,
                                wave_width_px,
                                Self::editor_viewport_dimension_px(h, pixels_per_point),
                                1,
                                start,
                                end,
                                tab.vertical_zoom,
                                tab.vertical_view_center,
                                &self.spectro_cfg,
                                view_mode,
                                crate::app::types::EditorViewportRenderQuality::Coarse,
                            );
                            let fallback_texture = ui.ctx().load_texture(
                                format!("editor_viewport_sync_fallback_{tab_idx}_{view_mode:?}"),
                                fallback_image,
                                egui::TextureOptions::LINEAR,
                            );
                            painter.image(
                                fallback_texture.id(),
                                lane_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                Color32::WHITE,
                            );
                        } else {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let msg = if feature_loading {
                                "Building tempogram..."
                            } else {
                                "Tempogram not ready"
                            };
                            painter.text(
                                egui::pos2(wave_left + 6.0, rect.top() + 6.0),
                                egui::Align2::LEFT_TOP,
                                msg,
                                fid,
                                Color32::GRAY,
                            );
                        }
                        if !tab.show_waveform_overlay {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let tick_col = Color32::from_rgb(140, 150, 165);
                            let tick_stroke = egui::Stroke::new(1.0, tick_col);
                            let (visible_min, visible_max) = Self::editor_vertical_range_for_view(
                                view_mode,
                                tab.vertical_zoom,
                                tab.vertical_view_center,
                                &self.spectro_cfg,
                            );
                            for bpm in [30.0, 60.0, 90.0, 120.0, 150.0, 180.0, 240.0, 300.0] {
                                let frac = tempogram_data
                                    .as_ref()
                                    .and_then(|data| Self::tempogram_axis_position(data, bpm))
                                    .unwrap_or(((bpm - 30.0) / 270.0).clamp(0.0, 1.0));
                                let visible_frac = if (visible_max - visible_min).abs() < f32::EPSILON {
                                    0.0
                                } else {
                                    ((frac - visible_min) / (visible_max - visible_min)).clamp(0.0, 1.0)
                                };
                                let y = lane_rect.bottom() - visible_frac * lane_rect.height();
                                painter.line_segment(
                                    [
                                        egui::pos2(wave_left - 6.0, y),
                                        egui::pos2(wave_left - 2.0, y),
                                    ],
                                    tick_stroke,
                                );
                                painter.text(
                                    egui::pos2(rect.left() + 2.0, y),
                                    egui::Align2::LEFT_CENTER,
                                    format!("{bpm:.0}"),
                                    fid.clone(),
                                    tick_col,
                                );
                            }
                        }
                    }
                    ViewMode::Chromagram => {
                        let lane_rect = egui::Rect::from_min_size(
                            egui::pos2(wave_left, rect.top()),
                            egui::vec2(wave_w, h),
                        );
                        let current_viewport_cache = current_feature_viewport_key
                            .as_ref()
                            .and_then(|key| Self::exact_editor_viewport_cache(tab, key));
                        if let Some(crate::app::types::EditorViewportRenderCache {
                            payload:
                                crate::app::types::EditorViewportCachePayload::Image {
                                    texture: Some(texture),
                                    ..
                                },
                            ..
                        }) = current_viewport_cache
                        {
                            painter.image(
                                texture.id(),
                                lane_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                Color32::WHITE,
                            );
                        } else if let Some(data) = chromagram_data.as_ref() {
                            let fallback_image = Self::render_chromagram_viewport_image(
                                data,
                                wave_width_px,
                                Self::editor_viewport_dimension_px(h, pixels_per_point),
                                1,
                                start,
                                end,
                                tab.vertical_zoom,
                                tab.vertical_view_center,
                                &self.spectro_cfg,
                                view_mode,
                                crate::app::types::EditorViewportRenderQuality::Coarse,
                            );
                            let fallback_texture = ui.ctx().load_texture(
                                format!("editor_viewport_sync_fallback_{tab_idx}_{view_mode:?}"),
                                fallback_image,
                                egui::TextureOptions::LINEAR,
                            );
                            painter.image(
                                fallback_texture.id(),
                                lane_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                Color32::WHITE,
                            );
                        } else {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let msg = if feature_loading {
                                "Building chromagram..."
                            } else {
                                "Chromagram not ready"
                            };
                            painter.text(
                                egui::pos2(wave_left + 6.0, rect.top() + 6.0),
                                egui::Align2::LEFT_TOP,
                                msg,
                                fid,
                                Color32::GRAY,
                            );
                        }
                        if !tab.show_waveform_overlay {
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let tick_col = Color32::from_rgb(140, 150, 165);
                            let tick_stroke = egui::Stroke::new(1.0, tick_col);
                            let (visible_min, visible_max) = Self::editor_vertical_range_for_view(
                                view_mode,
                                tab.vertical_zoom,
                                tab.vertical_view_center,
                                &self.spectro_cfg,
                            );
                            for bin in 0..12 {
                                let frac = if 11 == 0 {
                                    0.0
                                } else {
                                    bin as f32 / 11.0
                                };
                                let visible_frac = if (visible_max - visible_min).abs() < f32::EPSILON {
                                    0.0
                                } else {
                                    ((frac - visible_min) / (visible_max - visible_min)).clamp(0.0, 1.0)
                                };
                                let y = lane_rect.bottom() - visible_frac * lane_rect.height();
                                painter.line_segment(
                                    [
                                        egui::pos2(wave_left - 6.0, y),
                                        egui::pos2(wave_left - 2.0, y),
                                    ],
                                    tick_stroke,
                                );
                                painter.text(
                                    egui::pos2(rect.left() + 2.0, y),
                                    egui::Align2::LEFT_CENTER,
                                    Self::chroma_label_for_bin(bin),
                                    fid.clone(),
                                    tick_col,
                                );
                            }
                        }
                    }
                    ViewMode::Waveform => {}
                }
            }

            if let Some(started) = waveform_render_started {
                perf_wave_render_ms = Some(started.elapsed().as_secs_f32() * 1000.0);
            }

            // Handle interactions (seek, zoom, pan, selection)
            if view_mode == ViewMode::Waveform
                && display_samples_len > 0
                && !ctx.wants_keyboard_input()
            {
                let zoom_in = ctx.input(|i| i.key_pressed(egui::Key::ArrowUp));
                let zoom_out = ctx.input(|i| i.key_pressed(egui::Key::ArrowDown));
                if zoom_in || zoom_out {
                    let factor = if zoom_in { 0.9 } else { 1.1 };
                    let old_spp = tab.samples_per_px.max(0.0001);
                    let (anchor, t) = Self::editor_zoom_anchor(
                        self.horizontal_zoom_anchor_mode,
                        tab,
                        display_samples_len,
                        wave_left,
                        wave_w,
                        None,
                        playhead_display_now,
                    );
                    let min_spp = crate::app::EDITOR_MIN_SAMPLES_PER_PX;
                    let max_spp_fit =
                        (display_samples_len as f32 / wave_w.max(1.0)).max(min_spp);
                    let new_spp = (old_spp * factor).clamp(min_spp, max_spp_fit);
                    tab.samples_per_px = new_spp;
                    let vis2 = EditorDisplayGeometry::new(
                        wave_left,
                        wave_w,
                        tab.samples_per_px,
                        tab.view_offset,
                        tab.view_offset_exact,
                        display_samples_len,
                    );
                    let next_exact =
                        Self::editor_exact_view_for_anchor(anchor, t, wave_w, tab.samples_per_px);
                    Self::editor_set_view_offset_exact(tab, next_exact, vis2.max_left());
                }
            }

            // Detect hover using pointer position against our canvas rect (robust across senses)
            let pointer_pos = ui.input(|i| i.pointer.hover_pos());
            let pointer_over_waveform = pointer_pos.map_or(false, |p| {
                rect.contains(p)
                    && amplitude_nav_rect
                        .map(|amp_rect| !amp_rect.contains(p))
                        .unwrap_or(true)
            });
            if pointer_over_waveform {
                // Zoom with wheel/pinch over this widget.
                // `zoom_delta` captures ctrl/cmd+wheel and pinch gestures robustly.
                let (wheel_raw, wheel_smooth, zoom_delta, modifiers) = ui.input(|i| {
                    (
                        i.raw_scroll_delta,
                        i.smooth_scroll_delta,
                        i.zoom_delta(),
                        i.modifiers,
                    )
                });
                let wheel = if wheel_raw != egui::Vec2::ZERO {
                    wheel_raw
                } else {
                    wheel_smooth
                };
                let scroll_y = wheel.y;
                let pointer_x = resp.hover_pos().map(|p| p.x).filter(|_| pointer_over_waveform);
                let wheel_has_scroll = wheel.x.abs() > 0.0 || scroll_y.abs() > 0.0;
                let zoom_factor_from_input = if !wheel_has_scroll
                    && zoom_delta.is_finite()
                    && (zoom_delta - 1.0).abs() > 0.10
                {
                    // egui zoom_delta > 1 means "zoom in". For samples-per-pixel we invert it.
                    Some((1.0 / zoom_delta.max(1e-3)).clamp(0.2, 5.0))
                } else {
                    None
                };
                // Debug trace (dev builds): log incoming deltas and modifiers when over canvas
                #[cfg(debug_assertions)]
                if wheel_raw != egui::Vec2::ZERO
                    || wheel_smooth != egui::Vec2::ZERO
                    || zoom_factor_from_input.is_some()
                {
                    eprintln!(
                        "wheel_raw=({:.2},{:.2}) wheel_smooth=({:.2},{:.2}) wheel_used=({:.2},{:.2}) ctrl={} shift={} zoom_delta={:.3}",
                        wheel_raw.x,
                        wheel_raw.y,
                        wheel_smooth.x,
                        wheel_smooth.y,
                        wheel.x,
                        wheel.y,
                        modifiers.ctrl,
                        modifiers.shift,
                        zoom_delta
                    );
                }
                // Zoom: plain wheel (unless Shift is held for pan) or gesture zoom.
                if (((scroll_y.abs() > 0.0) && !modifiers.shift)
                    || zoom_factor_from_input.is_some())
                    && display_samples_len > 0
                {
                    let factor = zoom_factor_from_input
                        .unwrap_or_else(|| {
                            let zoom_scroll_y = if self.invert_wave_zoom_wheel {
                                -scroll_y
                            } else {
                                scroll_y
                            };
                            if zoom_scroll_y > 0.0 {
                                0.9
                            } else {
                                1.1
                            }
                        })
                        .clamp(0.2, 5.0);
                    let old_spp = tab.samples_per_px.max(0.0001);
                    let (anchor, t) = Self::editor_zoom_anchor(
                        self.horizontal_zoom_anchor_mode,
                        tab,
                        display_samples_len,
                        wave_left,
                        wave_w,
                        pointer_x,
                        playhead_display_now,
                    );
                    let min_spp = crate::app::EDITOR_MIN_SAMPLES_PER_PX;
                    let max_spp_fit =
                        (display_samples_len as f32 / wave_w.max(1.0)).max(min_spp);
                    let new_spp = (old_spp * factor).clamp(min_spp, max_spp_fit);
                    tab.samples_per_px = new_spp;
                    let geom2 = EditorDisplayGeometry::new(
                        wave_left,
                        wave_w,
                        tab.samples_per_px,
                        tab.view_offset,
                        tab.view_offset_exact,
                        display_samples_len,
                    );
                    let next_exact =
                        Self::editor_exact_view_for_anchor(anchor, t, wave_w, tab.samples_per_px);
                    #[cfg(debug_assertions)]
                    {
                        let vis = (wave_w * old_spp).ceil() as usize;
                        let mode = if tab.samples_per_px >= 1.0 { "agg" } else { "line" };
                        let fit_whole = (new_spp - max_spp_fit).abs() < 1e-6;
                        eprintln!(
                            "ZOOM change: spp {:.5} -> {:.5} ({mode}) factor {:.3} vis={} -> {} anchor={} new_view_exact={:.3} wave_w={:.1} fit_whole={}",
                            old_spp, new_spp, factor, vis, geom2.visible_count, anchor, next_exact, wave_w, fit_whole
                        );
                    }
                    Self::editor_set_view_offset_exact(tab, next_exact, geom2.max_left());
                }
                // Pan with Shift + wheel (prefer horizontal wheel if available)
                let mut scroll_for_pan = if wheel.x.abs() > 0.0 { wheel.x } else { wheel.y };
                if self.invert_shift_wheel_pan {
                    scroll_for_pan = -scroll_for_pan;
                }
                if modifiers.shift && scroll_for_pan.abs() > 0.0 && display_samples_len > 0 {
                    let delta_px = -scroll_for_pan.signum() * 60.0; // a page step
                    let vis = (wave_w * tab.samples_per_px).ceil() as usize;
                    let max_left = display_samples_len.saturating_sub(vis);
                    let next_exact = tab.view_offset_exact + (delta_px * tab.samples_per_px) as f64;
                    Self::editor_set_view_offset_exact(tab, next_exact, max_left);
                }
                // Pan with Middle drag or Alt + Left drag (DAW-like).
                let (left_down, mid_down, alt_mod) = ui.input(|i| (
                    i.pointer.button_down(egui::PointerButton::Primary),
                    i.pointer.button_down(egui::PointerButton::Middle),
                    i.modifiers.alt,
                ));
                let alt_left_pan = alt_mod && left_down;
                if (mid_down || alt_left_pan) && display_samples_len > 0 {
                    let dx = ui.input(|i| i.pointer.delta().x);
                    if dx.abs() > 0.0 {
                        let vis = (wave_w * tab.samples_per_px).ceil() as usize;
                        let max_left = display_samples_len.saturating_sub(vis);
                        let next_exact = tab.view_offset_exact + (-dx * tab.samples_per_px) as f64;
                        Self::editor_set_view_offset_exact(tab, next_exact, max_left);
                    }
                }
            }
            // Drag markers for LoopEdit (primary button only)
            let mut suppress_seek = false;
            let alt_now = ui.input(|i| i.modifiers.alt);
            // Right drag is dedicated to seek/playhead movement.
            // Shift+Right drag switches to range selection with button-down anchor.
            if pointer_over_waveform
                && !alt_now
                && display_samples_len > 0
                && tab.dragging_marker.is_none()
            {
                let right_pressed = ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
                let right_drag_started = resp.drag_started_by(egui::PointerButton::Secondary);
                let right_dragging = resp.dragged_by(egui::PointerButton::Secondary);
                let right_drag_stopped = resp.drag_stopped_by(egui::PointerButton::Secondary);
                let shift_now = ui.input(|i| i.modifiers.shift);
                let to_display_sample = |x: f32| -> usize {
                    geom.x_to_display_sample(x)
                };

                if right_pressed || right_drag_started {
                    tab.right_drag_mode = Some(if shift_now {
                        RightDragMode::SelectRange
                    } else {
                        RightDragMode::Seek
                    });
                    if shift_now {
                        if let Some(pos) = resp
                            .interact_pointer_pos()
                            .or_else(|| ui.input(|i| i.pointer.hover_pos()))
                        {
                            let samp = to_display_sample(pos.x);
                            Self::editor_set_selection_from_anchor(tab, samp, samp);
                            suppress_seek = true;
                        }
                    }
                }
                if right_dragging {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let samp = to_display_sample(pos.x);
                        match tab.right_drag_mode.unwrap_or(if shift_now {
                            RightDragMode::SelectRange
                        } else {
                            RightDragMode::Seek
                        }) {
                            RightDragMode::Seek => {
                                request_seek = Some(map_display_to_audio(tab, samp));
                                suppress_seek = true;
                            }
                            RightDragMode::SelectRange => {
                                let anchor = Self::editor_selection_anchor_or(
                                    tab,
                                    playhead_display_now.min(display_samples_len),
                                );
                                Self::editor_set_selection_from_anchor(tab, anchor, samp);
                                suppress_seek = true;
                            }
                        }
                    }
                }
                if right_drag_stopped {
                    tab.right_drag_mode = None;
                }
            }
            if pointer_over_waveform
                && matches!(tab.active_tool, ToolKind::LoopEdit)
                && display_samples_len > 0
            {
                let pointer_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
                let pointer_released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
                if pointer_released || !pointer_down {
                    tab.dragging_marker = None;
                }
                if pointer_down {
                    let to_sample = |x: f32| geom.x_to_display_sample(x);
                    let to_x = |samp: usize| geom.sample_boundary_x(samp);
                    let hit_radius = 7.0;
                    if tab.dragging_marker.is_none() {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            let x = pos.x;
                            match tab.active_tool {
                                ToolKind::LoopEdit => {
                                    if let Some((a0, b0)) = tab.loop_region {
                                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                        let ax = to_x(a);
                                        let bx = to_x(b);
                                        if (x - ax).abs() <= hit_radius {
                                            if pending_edit_undo.is_none() {
                                                pending_edit_undo = Some(Self::capture_undo_state(tab));
                                            }
                                            tab.dragging_marker = Some(MarkerKind::A);
                                        } else if (x - bx).abs() <= hit_radius {
                                            if pending_edit_undo.is_none() {
                                                pending_edit_undo = Some(Self::capture_undo_state(tab));
                                            }
                                            tab.dragging_marker = Some(MarkerKind::B);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    if let Some(marker) = tab.dragging_marker {
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                            let samp = to_sample(pos.x);
                            match tab.active_tool {
                                ToolKind::LoopEdit => {
                                    if let Some((a0, b0)) = tab.loop_region {
                                        let (mut a, mut b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                        if pending_edit_undo.is_none() {
                                            pending_edit_undo = Some(Self::capture_undo_state(tab));
                                        }
                                        match marker {
                                            MarkerKind::A => a = samp.min(b),
                                            MarkerKind::B => b = samp.max(a),
                                        }
                                        tab.loop_region = Some((a, b));
                                        Self::update_loop_markers_dirty(tab);
                                        apply_pending_loop = true;
                                    }
                                }
                                _ => {}
                            }
                        }
                        suppress_seek = true;
                    }
                }
            }
            // Drag to select a range (independent of tool), unless we are dragging markers
            if pointer_over_waveform
                && !alt_now
                && display_samples_len > 0
                && tab.dragging_marker.is_none()
            {
                let drag_started = resp.drag_started_by(egui::PointerButton::Primary);
                let dragging = resp.dragged_by(egui::PointerButton::Primary);
                let drag_released = resp.drag_stopped_by(egui::PointerButton::Primary);
                let playhead_display = playhead_display_now.min(display_samples_len);
                let playhead_x = geom.sample_center_x(playhead_display);
                let snap_radius_px = 8.0f32;
                let to_display_sample_snapped = |x: f32| -> usize {
                    let x = x.clamp(wave_left, wave_left + wave_w);
                    if (x - playhead_x).abs() <= snap_radius_px {
                        return playhead_display;
                    }
                    geom.x_to_display_sample(x)
                };
                if drag_started {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let samp = to_display_sample_snapped(pos.x);
                        tab.selection_anchor_sample = Some(samp);
                    }
                }
                if dragging {
                    let anchor = tab.selection_anchor_sample.or_else(|| {
                        resp.interact_pointer_pos()
                            .map(|pos| to_display_sample_snapped(pos.x))
                    });
                    if tab.selection_anchor_sample.is_none() {
                        tab.selection_anchor_sample = anchor;
                    }
                    if let (Some(anchor), Some(pos)) = (anchor, resp.interact_pointer_pos()) {
                        let samp = to_display_sample_snapped(pos.x);
                        Self::editor_set_selection_from_anchor(tab, anchor, samp);
                        suppress_seek = true;
                    }
                }
                let _ = drag_released;
            }
            // Selection vs Seek with primary button (Alt+LeftDrag = pan handled above)
            if !alt_now && !suppress_seek {
                if resp.clicked_by(egui::PointerButton::Primary) {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let x = pos.x.clamp(wave_left, wave_left + wave_w);
                        let spp = tab.samples_per_px.max(0.0001);
                        let vis = (wave_w * spp).ceil() as usize;
                        let pos_samp = tab
                            .view_offset
                            .saturating_add(
                                (((x - wave_left) / wave_w) * vis as f32) as usize,
                            )
                            .min(display_samples_len);
                        let shift_now = ui.input(|i| i.modifiers.shift);
                        if shift_now {
                            let anchor =
                                Self::editor_selection_anchor_or(tab, playhead_display_now);
                            Self::editor_set_selection_from_anchor(tab, anchor, pos_samp);
                        } else {
                            if tab.selection.is_some() {
                                tab.selection = None;
                            }
                            tab.selection_anchor_sample = None;
                            tab.right_drag_mode = None;
                            tab.selection = None;
                            request_seek = Some(map_display_to_audio(tab, pos_samp));
                        }
                    }
                }
            }

            let spp = tab.samples_per_px.max(0.0001);
            let vis = (wave_w * spp).ceil() as usize;
            let start = tab.view_offset.min(display_samples_len);
            let end = (start + vis).min(display_samples_len);
            let visible_len = end.saturating_sub(start);
            waveform_scratch = std::mem::take(&mut self.waveform_scratch);
            waveform_query_ms_total = 0.0;
            waveform_draw_ms_total = 0.0;

            let show_waveform = view_mode == ViewMode::Waveform || tab.show_waveform_overlay;

            // Draw per-channel lanes with dB grid and playhead
            waveform_render_started = Some(std::time::Instant::now());
            if show_waveform {
            for lane_idx in 0..lane_count {
                let channel_index = if use_mixdown {
                    None
                } else {
                    visible_channels.get(lane_idx).copied()
                };
                let lane_top = rect.top() + lane_h * lane_idx as f32;
                let lane_rect = egui::Rect::from_min_size(egui::pos2(wave_left, lane_top), egui::vec2(wave_w, lane_h));
                let waveform_columns =
                    ov::compute_waveform_device_columns(lane_rect, pixels_per_point);
                // dB lines: -6, -12 dBFS and center line (0 amp)
                let dbs = [-6.0f32, -12.0f32];
                // center
                let center_y =
                    Self::waveform_center_y(lane_rect, tab.vertical_zoom, tab.vertical_view_center);
                painter.line_segment([egui::pos2(lane_rect.left(), center_y), egui::pos2(lane_rect.right(), center_y)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50)));
                for &db in &dbs {
                    let a = db_to_amp(db).clamp(0.0, 1.0);
                    let y0 = Self::waveform_y_from_amp(
                        lane_rect,
                        tab.vertical_zoom,
                        tab.vertical_view_center,
                        a,
                    );
                    let y1 = Self::waveform_y_from_amp(
                        lane_rect,
                        tab.vertical_zoom,
                        tab.vertical_view_center,
                        -a,
                    );
                    painter.line_segment([egui::pos2(lane_rect.left(), y0), egui::pos2(lane_rect.right(), y0)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50)));
                    painter.line_segment([egui::pos2(lane_rect.left(), y1), egui::pos2(lane_rect.right(), y1)], egui::Stroke::new(1.0, Color32::from_rgb(45,45,50)));
                    // labels on the left gutter
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    painter.text(egui::pos2(rect.left() + 2.0, y0), egui::Align2::LEFT_CENTER, format!("{db:.0} dB"), fid, Color32::GRAY);
                }

                if visible_len > 0 {
                    let (wave_lod, lane_query_ms, lane_draw_ms) = if tab.loading
                        && !tab.loading_waveform_minmax.is_empty()
                    {
                        Self::render_loading_overview_waveform(
                            &tab.loading_waveform_minmax,
                            display_samples_len.max(1),
                            lane_rect,
                            &waveform_columns,
                            scale,
                            tab.vertical_zoom,
                            tab.vertical_view_center,
                            start,
                            end,
                            &painter,
                            &mut waveform_scratch,
                        )
                    } else {
                        Self::render_editor_lane_waveform(
                            &*tab,
                            use_mixdown,
                            channel_index,
                            lane_rect,
                            EditorDisplayGeometry::new(
                                wave_left,
                                wave_w,
                                tab.samples_per_px,
                                tab.view_offset,
                                tab.view_offset_exact,
                                display_samples_len,
                            ),
                            &waveform_columns,
                            scale,
                            tab.vertical_zoom,
                            tab.vertical_view_center,
                            start,
                            end,
                            spp,
                            &painter,
                            &mut waveform_scratch,
                        )
                    };
                    waveform_query_ms_total += lane_query_ms;
                    waveform_draw_ms_total += lane_draw_ms;
                    if use_mixdown && !matches!(wave_lod, WaveformRenderLod::Pyramid) {
                        let acc = perf_mixdown_ms.unwrap_or(0.0);
                        perf_mixdown_ms = Some(acc + lane_query_ms);
                    }
                    match wave_lod {
                        WaveformRenderLod::Raw => {
                            self.debug.waveform_lod_raw_count =
                                self.debug.waveform_lod_raw_count.saturating_add(1);
                        }
                        WaveformRenderLod::VisibleMinMax => {
                            self.debug.waveform_lod_visible_count =
                                self.debug.waveform_lod_visible_count.saturating_add(1);
                        }
                        WaveformRenderLod::Pyramid => {
                            self.debug.waveform_lod_pyramid_count =
                                self.debug.waveform_lod_pyramid_count.saturating_add(1);
                        }
                    }
                    if !matches!(wave_lod, WaveformRenderLod::Raw) {
                        // Aggregated mode: also draw overlay here so it shows at widest zoom
                        if tab.active_tool != ToolKind::Trim && tab.preview_overlay.is_some() {
                            if let Some(overlay) = &tab.preview_overlay {
                                let overlay_overview: Option<&[(f32, f32)]> = if use_mixdown {
                                    overlay
                                        .overview_mixdown
                                        .as_ref()
                                        .map(|v| v.as_slice())
                                        .or_else(|| overlay.overview_channels.get(0).map(|v| v.as_slice()))
                                } else {
                                    channel_index
                                        .and_then(|idx| overlay.overview_channels.get(idx).map(|v| v.as_slice()))
                                        .or_else(|| overlay.overview_channels.get(0).map(|v| v.as_slice()))
                                };
                                let overlay_samples: Option<&[f32]> = if use_mixdown {
                                    overlay
                                        .mixdown
                                        .as_ref()
                                        .map(|v| v.as_slice())
                                        .or_else(|| overlay.channels.get(0).map(|v| v.as_slice()))
                                } else {
                                    channel_index
                                        .and_then(|idx| overlay.channels.get(idx).map(|v| v.as_slice()))
                                        .or_else(|| overlay.channels.get(0).map(|v| v.as_slice()))
                                };
                                use crate::app::render::colors::{
                                    OVERLAY_COLOR, OVERLAY_STROKE_BASE, OVERLAY_STROKE_EMPH,
                                };
                                let base_total = tab.samples_len.max(1);
                                let overlay_total = overlay.timeline_len.max(1);
                                let is_time_stretch =
                                    matches!(overlay.source_tool, ToolKind::TimeStretch);
                                if overlay.is_overview_only() {
                                    if let Some(overview) = overlay_overview {
                                        let bins_values = Self::compute_overlay_bins_from_overview(
                                            overview,
                                            start,
                                            visible_len,
                                            base_total,
                                            overlay_total,
                                            waveform_columns.column_count(),
                                            is_time_stretch,
                                        );
                                        if !bins_values.is_empty() {
                                            ov::draw_bins_locked(
                                                &painter,
                                                lane_rect,
                                                &waveform_columns,
                                                &bins_values,
                                                scale,
                                                tab.vertical_zoom,
                                                tab.vertical_view_center,
                                                OVERLAY_COLOR,
                                                OVERLAY_STROKE_BASE,
                                            );
                                        }
                                    }
                                } else if let Some(buf) = overlay_samples {
                                    let base_total = tab.samples_len.max(1);
                                    let overlay_total = overlay.timeline_len.max(1);
                                    let is_time_stretch = matches!(overlay.source_tool, ToolKind::TimeStretch);
                                    let unwrap_preview = matches!(overlay.source_tool, ToolKind::LoopEdit)
                                        && overlay_total > base_total
                                        && tab.pending_loop_unwrap.is_some()
                                        && tab.loop_region.is_some();
                                    let ratio = if is_time_stretch {
                                        1.0
                                    } else if base_total == 0 {
                                        1.0
                                    } else {
                                        overlay_total as f32 / base_total as f32
                                    };
                                    let start_scaled = ((start as f32) * ratio).round() as usize;
                                    let mut vis_scaled = ((visible_len as f32) * ratio).ceil() as usize;
                                    if vis_scaled == 0 { vis_scaled = 1; }
                                    let (startb, _endb, over_vis) = ov::map_visible_overlay(start_scaled, vis_scaled, overlay_total, buf.len());
                                    if over_vis > 0 {
                                        let bins = waveform_columns.column_count();
                                        let bins_values = if unwrap_preview {
                                            let loop_start = tab.loop_region.map(|(a, _)| a).unwrap_or(0);
                                            ov::compute_overlay_bins_for_unwrap(
                                                start,
                                                visible_len,
                                                base_total,
                                                loop_start,
                                                buf,
                                                overlay_total,
                                                bins,
                                            )
                                        } else {
                                            ov::compute_overlay_bins_for_base_columns(start, visible_len, startb, over_vis, buf, bins)
                                        };
                                        // Draw full overlay
                                        ov::draw_bins_locked(
                                            &painter,
                                            lane_rect,
                                            &waveform_columns,
                                            &bins_values,
                                            scale,
                                            tab.vertical_zoom,
                                            tab.vertical_view_center,
                                            OVERLAY_COLOR,
                                            OVERLAY_STROKE_BASE,
                                        );
                                        // Emphasize LoopEdit boundary segments if applicable
                                        if tab.active_tool == ToolKind::LoopEdit && !unwrap_preview {
                                            if let Some((a, b)) = tab.loop_region {
                                                let cf = Self::effective_loop_xfade_samples(
                                                    a,
                                                    b,
                                                    tab.samples_len,
                                                    tab.loop_xfade_samples,
                                                );
                                                if cf > 0 {
                                                    // Map required pre/post segments into overlay domain using ratio
                                                    let ratio = if base_total > 0 { (overlay_total as f32) / (base_total as f32) } else { 1.0 };
                                                    let head0 = a;
                                                    let head1 = (a + cf).min(b);
                                                    let tail0 = b.saturating_sub(cf);
                                                    let tail1 = b;
                                                    let a0 = (((head0 as f32) * ratio).round() as usize).min(buf.len());
                                                    let a1 = (((head1 as f32) * ratio).round() as usize).min(buf.len());
                                                    let b0 = (((tail0 as f32) * ratio).round() as usize).min(buf.len());
                                                    let b1 = (((tail1 as f32) * ratio).round() as usize).min(buf.len());
                                                    let segs = [(a0, a1), (b0, b1)];
                                                    for (s, e) in segs {
                                                        if let Some((p0, p1)) = ov::overlay_px_range_for_segment(startb, over_vis, bins, s, e) {
                                                            if p1 > p0 && p1 <= bins {
                                                                ov::draw_aggregated_waveform_columns(
                                                                    &painter,
                                                                    lane_rect,
                                                                    &waveform_columns,
                                                                    p0,
                                                                    p1 - p0,
                                                                    tab.vertical_zoom,
                                                                    tab.vertical_view_center,
                                                                    |local_idx| {
                                                                        let &(mn0, mx0) =
                                                                            bins_values.get(p0 + local_idx)?;
                                                                        let mn = (mn0 * scale).clamp(-1.0, 1.0);
                                                                        let mx = (mx0 * scale).clamp(-1.0, 1.0);
                                                                        if !mn.is_finite() || !mx.is_finite() {
                                                                            return None;
                                                                        }
                                                                        Some(ov::AggregatedWaveColumn {
                                                                            min: mn,
                                                                            max: mx,
                                                                            color: OVERLAY_COLOR,
                                                                            stroke: OVERLAY_STROKE_EMPH,
                                                                        })
                                                                    },
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                // Overlay preview aligned to this lane (if any), per-channel.
                // Skip Trim tool (Trim does not show green overlay by spec).
                // Draw whenever overlay data is present to avoid relying on preview_audio_tool state.
                #[cfg(debug_assertions)]
                if self.debug.cfg.enabled && self.debug.overlay_trace {
                    let mode = if !matches!(wave_lod, WaveformRenderLod::Raw) {
                        "agg"
                    } else {
                        "line"
                    };
                    let has_ov = tab.preview_overlay.is_some();
                    eprintln!(
                            "OVERLAY gate: mode={} has_overlay={} active={:?} spp={:.5} vis_len={} start={} end={} view_off={} len={}",
                        mode, has_ov, tab.active_tool, spp, visible_len, start, end, tab.view_offset, display_samples_len
                    );
                }
                if tab.active_tool != ToolKind::Trim && tab.preview_overlay.is_some() {
                    if let Some(overlay) = &tab.preview_overlay {
                        let overlay_overview: Option<&[(f32, f32)]> = if use_mixdown {
                            overlay
                                .overview_mixdown
                                .as_ref()
                                .map(|v| v.as_slice())
                                .or_else(|| overlay.overview_channels.get(0).map(|v| v.as_slice()))
                        } else {
                            channel_index
                                .and_then(|idx| overlay.overview_channels.get(idx).map(|v| v.as_slice()))
                                .or_else(|| overlay.overview_channels.get(0).map(|v| v.as_slice()))
                        };
                        let overlay_samples: Option<&[f32]> = if use_mixdown {
                            overlay
                                .mixdown
                                .as_ref()
                                .map(|v| v.as_slice())
                                .or_else(|| overlay.channels.get(0).map(|v| v.as_slice()))
                        } else {
                            channel_index
                                .and_then(|idx| overlay.channels.get(idx).map(|v| v.as_slice()))
                                .or_else(|| overlay.channels.get(0).map(|v| v.as_slice()))
                        };
                        let base_total = tab.samples_len.max(1);
                        let overlay_total = overlay.timeline_len.max(1);
                        let is_time_stretch = matches!(overlay.source_tool, ToolKind::TimeStretch);
                        if overlay.is_overview_only() {
                            if let Some(overview) = overlay_overview {
                                let values = Self::compute_overlay_bins_from_overview(
                                    overview,
                                    start,
                                    visible_len.max(1),
                                    base_total,
                                    overlay_total,
                                    waveform_columns.column_count(),
                                    is_time_stretch,
                                );
                                if !values.is_empty() {
                                    ov::draw_bins_locked(
                                        &painter,
                                        lane_rect,
                                        &waveform_columns,
                                        &values,
                                        scale,
                                        tab.vertical_zoom,
                                        tab.vertical_view_center,
                                        egui::Color32::from_rgb(80, 240, 160),
                                        1.3,
                                    );
                                }
                            }
                        } else if let Some(buf) = overlay_samples {
                            let unwrap_preview = matches!(overlay.source_tool, ToolKind::LoopEdit)
                                && overlay_total > base_total
                                && tab.pending_loop_unwrap.is_some()
                                && tab.loop_region.is_some();
                            if unwrap_preview {
                                if let Some((loop_start, _)) = tab.loop_region {
                                    let bins = waveform_columns.column_count();
                                    if bins > 0 {
                                        let values = ov::compute_overlay_bins_for_unwrap(
                                            start,
                                            visible_len.max(1),
                                            base_total,
                                            loop_start,
                                            buf,
                                            overlay_total,
                                            bins,
                                        );
                                        ov::draw_bins_locked(
                                            &painter,
                                            lane_rect,
                                            &waveform_columns,
                                            &values,
                                            scale,
                                            tab.vertical_zoom,
                                            tab.vertical_view_center,
                                            egui::Color32::from_rgb(80, 240, 160),
                                            1.3,
                                        );
                                    }
                                }
                            } else {
                                // Map original-visible [start,end) to overlay domain using length ratio.
                                // This keeps overlays visible at any zoom, even when length differs (e.g. TimeStretch).
                                let lenb = buf.len();
                                let ratio = if is_time_stretch {
                                    1.0
                                } else if base_total > 0 {
                                    (overlay_total as f32) / (base_total as f32)
                                } else {
                                    1.0
                                };
                            let orig_vis = visible_len.max(1);
                            // Map visible window [start .. start+orig_vis) into overlay domain using total-length ratio
                            // Align overlay start to original start using nearest sample to minimize off-by-one drift
                            let startb = (((start as f32) * ratio).round() as usize).min(lenb);
                            let mut endb = startb + (((orig_vis as f32) * ratio).ceil() as usize);
                            if endb > lenb { endb = lenb; }
                            if startb >= endb { endb = (startb + 1).min(lenb); }
                            let over_vis = (endb.saturating_sub(startb)).max(1);
                            let r_w = if orig_vis > 0 { (over_vis as f32) / (orig_vis as f32) } else { 1.0 };
                            let ov_w = (wave_w * r_w).max(1.0);
                            #[cfg(debug_assertions)]
                            if self.debug.cfg.enabled && self.debug.overlay_trace {
                                let mode = if spp >= 2.0 { "agg" } else { "line" };
                                eprintln!(
                                    "OVERLAY map: mode={} lenb={} startb={} endb={} over_vis={} ov_w_px={:.1}",
                                    mode, lenb, startb, endb, over_vis, ov_w
                                );
                            }
                            if startb < endb {
                                // Pre-compute LoopEdit highlight segments (mapped to overlay domain)
                                let (seg1_opt, seg2_opt) = if tab.active_tool == ToolKind::LoopEdit {
                                    if let Some((a, b)) = tab.loop_region {
                                        let cf = Self::effective_loop_xfade_samples(
                                            a,
                                            b,
                                            tab.samples_len,
                                            tab.loop_xfade_samples,
                                        );
                                        if cf > 0 {
                                            let head0 = a;
                                            let head1 = (a + cf).min(b);
                                            let tail0 = b.saturating_sub(cf);
                                            let tail1 = b;
                                            let a0 = (((head0 as f32) * ratio).round() as usize).min(lenb);
                                            let a1 = (((head1 as f32) * ratio).round() as usize).min(lenb);
                                            let b0 = (((tail0 as f32) * ratio).round() as usize).min(lenb);
                                            let b1 = (((tail1 as f32) * ratio).round() as usize).min(lenb);
                                            let s1 = a0.max(startb); let e1 = a1.min(endb);
                                            let s2 = b0.max(startb); let e2 = b1.min(endb);
                                            (if s1 < e1 { Some((s1,e1)) } else { None }, if s2 < e2 { Some((s2,e2)) } else { None })
                                        } else { (None, None) }
                                    } else { (None, None) }
                                } else { (None, None) };

                                // helper: draw polyline for [p0,p1) within [startb,endb) mapped into [0..ov_w]
                                let _draw_segment_poly = |p0: usize, p1: usize| {
                                    let seg_len = p1.saturating_sub(p0);
                                    if seg_len == 0 { return; }
                                    let seg_ratio = (seg_len as f32) / (over_vis as f32);
                                    let seg_w = (ov_w * seg_ratio).max(1.0);
                                    let seg_x0 = lane_rect.left() + ((p0 - startb) as f32 / over_vis as f32) * ov_w;
                                    let count = seg_w.max(1.0) as usize; // ~1 point per px
                                    let denom = (count.saturating_sub(1)).max(1) as f32;
                                    #[cfg(debug_assertions)]
                                    if self.debug.cfg.enabled && self.debug.overlay_trace {
                                        let band = egui::Rect::from_min_max(egui::pos2(seg_x0, lane_rect.top()), egui::pos2(seg_x0 + seg_w, lane_rect.bottom()));
                                        painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(110, 255, 200, 20));
                                        eprintln!(
                                            "OVERLAY seg: p0={} p1={} seg_len={} seg_w_px={:.1} count={}",
                                            p0, p1, seg_len, seg_w, count
                                        );
                                    }
                                    // Widest zoom: a very short segment can quantize to <=1px. Ensure something is drawn.
                                    if count <= 2 {
                                        let idx = p0; // head of segment as representative
                                        let v = (buf[idx] * scale).clamp(-1.0, 1.0);
                                        let sx = seg_x0 + (seg_w * 0.5);
                                        let sy = Self::waveform_y_from_amp(
                                            lane_rect,
                                            tab.vertical_zoom,
                                            tab.vertical_view_center,
                                            v,
                                        );
                                        // Draw a short tick so it remains visible
                                        let tick_h = (lane_rect.height() * 0.10).max(2.0);
                                        painter.line_segment(
                                            [egui::pos2(sx, sy - tick_h*0.5), egui::pos2(sx, sy + tick_h*0.5)],
                                            egui::Stroke::new(1.8, Color32::from_rgb(80, 240, 160))
                                        );
                                    #[cfg(debug_assertions)]
                                    if self.debug.cfg.enabled && self.debug.overlay_trace {
                                        eprintln!("OVERLAY seg: fallback_tick used at x={:.1}", sx);
                                    }
                                        return;
                                    }
                                    let mut last: Option<egui::Pos2> = None;
                                    for i in 0..count {
                                        let t = (i as f32) / denom;
                                        let idx = p0 + ((t * (seg_len as f32 - 1.0)).round() as usize).min(seg_len - 1);
                                        let v = (buf[idx] * scale).clamp(-1.0, 1.0);
                                        let sx = seg_x0 + t * seg_w;
                                        let sy = Self::waveform_y_from_amp(
                                            lane_rect,
                                            tab.vertical_zoom,
                                            tab.vertical_view_center,
                                            v,
                                        );
                                        let p = egui::pos2(sx, sy);
                                        if let Some(lp) = last { painter.line_segment([lp, p], egui::Stroke::new(1.8, Color32::from_rgb(80, 240, 160))); }
                                        last = Some(p);
                                    }
                                };

                                if spp >= 2.0 {
                                    // Aggregated: compute bins via helper and draw pixel-locked bars
                                    let bins = waveform_columns.column_count();
                                    if bins > 0 {
                                        let ratio_approx_1 = (over_vis as i64 - orig_vis as i64).abs() <= 1;
                                            let values = if ratio_approx_1 {
                                                let mut tmp = Vec::new();
                                                let s = start.min(lenb);
                                                let e = end.min(lenb);
                                                if s < e {
                                                    build_minmax(&mut tmp, &buf[s..e], bins);
                                                }
                                                tmp
                                            } else {
                                                crate::app::render::overlay::compute_overlay_bins_for_base_columns(
                                                    start, orig_vis, startb, over_vis, buf, bins
                                                )
                                        };
                                        crate::app::render::overlay::draw_bins_locked(
                                            &painter,
                                            lane_rect,
                                            &waveform_columns,
                                            &values,
                                            scale,
                                            tab.vertical_zoom,
                                            tab.vertical_view_center,
                                            egui::Color32::from_rgb(80, 240, 160),
                                            1.3,
                                        );
                                    }
                                    // Emphasize LoopEdit boundary subranges if present (thicker over the same px columns)
                                    if let Some((s1,e1)) = seg1_opt {
                                        let bins = waveform_columns.column_count();
                                        if bins > 0 {
                                            let step_b = (orig_vis as f32) / (bins as f32);
                                            let mut pos_b = 0.0f32;
                                            let px_end = ((over_vis as f32 / orig_vis as f32) * bins as f32).round().clamp(1.0, bins as f32) as usize;
                                            for px in 0..px_end {
                                                let i0 = start + pos_b.floor() as usize;
                                                pos_b += step_b;
                                                let mut i1 = start + pos_b.floor() as usize;
                                                if i1 <= i0 { i1 = i0 + 1; }
                                                let mut o0 = startb + (((i0 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                let mut o1 = startb + (((i1 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                if o1 <= o0 { o1 = o0 + 1; }
                                                o0 = o0.max(s1); o1 = o1.min(e1);
                                                if o1 <= o0 { continue; }
                                                let mut mn = f32::INFINITY;
                                                let mut mx = f32::NEG_INFINITY;
                                                for &v in &buf[o0..o1] {
                                                    if v < mn {
                                                        mn = v;
                                                    }
                                                    if v > mx {
                                                        mx = v;
                                                    }
                                                }
                                                if !mn.is_finite() || !mx.is_finite() {
                                                    continue;
                                                }
                                                let mn = (mn * scale).clamp(-1.0, 1.0);
                                                let mx = (mx * scale).clamp(-1.0, 1.0);
                                                let x = waveform_columns.x_center_pt(px);
                                                if !x.is_finite() {
                                                    continue;
                                                }
                                                let y0 = Self::waveform_y_from_amp(
                                                    lane_rect,
                                                    tab.vertical_zoom,
                                                    tab.vertical_view_center,
                                                    mx,
                                                );
                                                let y1 = Self::waveform_y_from_amp(
                                                    lane_rect,
                                                    tab.vertical_zoom,
                                                    tab.vertical_view_center,
                                                    mn,
                                                );
                                                painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.6, Color32::from_rgb(80, 240, 160)));
                                            }
                                        }
                                    }
                                    if let Some((s2,e2)) = seg2_opt {
                                        let bins = waveform_columns.column_count();
                                        if bins > 0 {
                                            let step_b = (orig_vis as f32) / (bins as f32);
                                            let mut pos_b = 0.0f32;
                                            let px_end = ((over_vis as f32 / orig_vis as f32) * bins as f32).round().clamp(1.0, bins as f32) as usize;
                                            for px in 0..px_end {
                                                let i0 = start + pos_b.floor() as usize;
                                                pos_b += step_b;
                                                let mut i1 = start + pos_b.floor() as usize;
                                                if i1 <= i0 { i1 = i0 + 1; }
                                                let mut o0 = startb + (((i0 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                let mut o1 = startb + (((i1 - start) as f32 * over_vis as f32 / orig_vis as f32).round() as usize);
                                                if o1 <= o0 { o1 = o0 + 1; }
                                                o0 = o0.max(s2); o1 = o1.min(e2);
                                                if o1 <= o0 { continue; }
                                                let mut mn = f32::INFINITY;
                                                let mut mx = f32::NEG_INFINITY;
                                                for &v in &buf[o0..o1] {
                                                    if v < mn {
                                                        mn = v;
                                                    }
                                                    if v > mx {
                                                        mx = v;
                                                    }
                                                }
                                                if !mn.is_finite() || !mx.is_finite() {
                                                    continue;
                                                }
                                                let mn = (mn * scale).clamp(-1.0, 1.0);
                                                let mx = (mx * scale).clamp(-1.0, 1.0);
                                                let x = waveform_columns.x_center_pt(px);
                                                if !x.is_finite() {
                                                    continue;
                                                }
                                                let y0 = Self::waveform_y_from_amp(
                                                    lane_rect,
                                                    tab.vertical_zoom,
                                                    tab.vertical_view_center,
                                                    mx,
                                                );
                                                let y1 = Self::waveform_y_from_amp(
                                                    lane_rect,
                                                    tab.vertical_zoom,
                                                    tab.vertical_view_center,
                                                    mn,
                                                );
                                                painter.line_segment([egui::pos2(x, y0.min(y1)), egui::pos2(x, y0.max(y1))], egui::Stroke::new(1.6, Color32::from_rgb(80, 240, 160)));
                                            }
                                        }
                                    }
                                } else {
                                    let denom = (endb - startb - 1).max(1) as f32;
                                    let base_y = Self::waveform_center_y(
                                        lane_rect,
                                        tab.vertical_zoom,
                                        tab.vertical_view_center,
                                    );
                                    #[cfg(debug_assertions)]
                                    {
                                        let x0 = lane_rect.left();
                                        let x1 = x0 + ov_w;
                                        let band = egui::Rect::from_min_max(egui::pos2(x0, lane_rect.top()), egui::pos2(x1, lane_rect.bottom()));
                                        painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(80, 240, 160, 20));
                                    }
                                    let mut last: Option<egui::Pos2> = None;
                                    for i in startb..endb {
                                        let v = (buf[i] * scale).clamp(-1.0, 1.0);
                                        let t = (i - startb) as f32 / denom;
                                        let sx = lane_rect.left() + t * ov_w;
                                        let sy = Self::waveform_y_from_amp(
                                            lane_rect,
                                            tab.vertical_zoom,
                                            tab.vertical_view_center,
                                            v,
                                        );
                                        let p = egui::pos2(sx, sy);
                                        if let Some(lp) = last { painter.line_segment([lp, p], egui::Stroke::new(1.5, Color32::from_rgb(80, 240, 160))); }
                                        last = Some(p);
                                    }
                                    // Add stems like the base waveform when zoomed in enough
                                    let pps = 1.0 / spp; // pixels per sample
                                    if pps >= 6.0 {
                                        for i in startb..endb {
                                            let v = (buf[i] * scale).clamp(-1.0, 1.0);
                                            let t = (i - startb) as f32 / denom;
                                            let sx = lane_rect.left() + t * ov_w;
                                            let sy = Self::waveform_y_from_amp(
                                                lane_rect,
                                                tab.vertical_zoom,
                                                tab.vertical_view_center,
                                                v,
                                            );
                                            painter.line_segment([egui::pos2(sx, base_y), egui::pos2(sx, sy)], egui::Stroke::new(1.0, Color32::from_rgb(80, 240, 160)));
                                        }
                                    }
                                }
                            }
                            }
                        }
                    }
                }
            }
            }
                }
            }

            // (Removed) global mono overlay to avoid double/triple drawing.

            // Overlay regions (loop/trim/fade) on top of waveform
            if tab.samples_len > 0 {
                let to_x = |samp: usize| geom.sample_boundary_x(samp);
                let marker_x = |samp: usize| geom.sample_center_x(samp);
                let draw_handle = |x: f32, col: Color32| {
                    let handle_w = 6.0;
                    let handle_h = 16.0;
                    let r = egui::Rect::from_min_max(
                        egui::pos2(x - handle_w * 0.5, rect.top()),
                        egui::pos2(x + handle_w * 0.5, rect.top() + handle_h),
                    );
                    painter.rect_filled(r, 2.0, col);
                };
                let sr = sr_ctx.max(1.0);

                let mut fade_in_handle: Option<f32> = None;
                let mut fade_out_handle: Option<f32> = None;

                // Selection overlay (tool-independent)
                if let Some((a0, b0)) = tab.selection {
                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                    if b >= tab.view_offset {
                        let vis = (wave_w * spp).ceil() as usize;
                        let end = tab.view_offset.saturating_add(vis).min(tab.samples_len);
                        if a <= end {
                            let ax = to_x(a);
                            let bx = to_x(b);
                            let sel_rect = egui::Rect::from_min_max(
                                egui::pos2(ax, rect.top()),
                                egui::pos2(bx, rect.bottom()),
                            );
                            let fill = Color32::from_rgba_unmultiplied(70, 140, 255, 28);
                            let stroke = Color32::from_rgba_unmultiplied(70, 140, 255, 160);
                            painter.rect_filled(sel_rect, 0.0, fill);
                            painter.rect_stroke(
                                sel_rect,
                                0.0,
                                egui::Stroke::new(1.0, stroke),
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                }

                // Trim overlay (set range): orange to distinguish from generic blue selection.
                if let Some((a0, b0)) = tab.trim_range {
                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                    if b >= tab.view_offset {
                        let vis = (wave_w * spp).ceil() as usize;
                        let end = tab.view_offset.saturating_add(vis).min(tab.samples_len);
                        if a <= end {
                            let ax = to_x(a);
                            let bx = to_x(b);
                            let trim_rect = egui::Rect::from_min_max(
                                egui::pos2(ax, rect.top()),
                                egui::pos2(bx, rect.bottom()),
                            );
                            let fill = Color32::from_rgba_unmultiplied(255, 140, 0, 34);
                            let stroke = Color32::from_rgba_unmultiplied(255, 140, 0, 190);
                            painter.rect_filled(trim_rect, 0.0, fill);
                            painter.rect_stroke(
                                trim_rect,
                                0.0,
                                egui::Stroke::new(1.0, stroke),
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                }

                // Marker overlay
                if !tab.markers.is_empty() {
                    let vis = (wave_w * spp).ceil() as usize;
                    let start = tab.view_offset.min(tab.samples_len);
                    let end = (start + vis).min(tab.samples_len);
                    let pending = tab.markers != tab.markers_committed;
                    let mut provisional_set = std::collections::HashSet::<(usize, String)>::new();
                    for m in tab.music_analysis_draft.provisional_markers.iter() {
                        provisional_set.insert((m.sample, m.label.clone()));
                    }
                    let base_col = if pending {
                        Color32::from_rgb(235, 210, 130)
                    } else {
                        Color32::from_rgb(255, 200, 80)
                    };
                    let provisional_col = Color32::from_rgb(120, 220, 120);
                    for m in tab.markers.iter() {
                        if m.sample < start || m.sample > end {
                            continue;
                        }
                        let x = marker_x(m.sample);
                        let is_provisional =
                            provisional_set.contains(&(m.sample, m.label.clone()));
                        let col = if is_provisional {
                            provisional_col
                        } else {
                            base_col
                        };
                        painter.line_segment(
                            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                            egui::Stroke::new(1.0, col),
                        );
                    }
                }

                // Loop overlay
                let (applied_loop, editing_loop) = Self::resolve_editor_loop_visual_ranges(tab);
                if let Some((a, b)) = applied_loop {
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    let ax = to_x(a);
                    let bx = to_x(b);
                    let line =
                        Color32::from_rgba_unmultiplied(110, 190, 200, 120);
                    let shade =
                        Color32::from_rgba_unmultiplied(110, 190, 200, 18);
                    if a == b {
                        painter.line_segment(
                            [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                            egui::Stroke::new(1.5, line),
                        );
                        painter.text(
                            egui::pos2(ax + 6.0, rect.top() + 2.0),
                            egui::Align2::LEFT_TOP,
                            "Applied",
                            fid,
                            Color32::from_rgb(150, 210, 216),
                        );
                    } else {
                        let applied_rect = egui::Rect::from_min_max(
                            egui::pos2(ax, rect.top()),
                            egui::pos2(bx, rect.bottom()),
                        );
                        painter.rect_filled(applied_rect, 0.0, shade);
                        painter.line_segment(
                            [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                            egui::Stroke::new(1.5, line),
                        );
                        painter.line_segment(
                            [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
                            egui::Stroke::new(1.5, line),
                        );
                        painter.text(
                            egui::pos2(ax + 6.0, rect.top() + 2.0),
                            egui::Align2::LEFT_TOP,
                            "Applied",
                            fid,
                            Color32::from_rgb(150, 210, 216),
                        );
                    }
                }
                if let Some((a, b)) = editing_loop {
                    let active = tab.active_tool == ToolKind::LoopEdit;
                    let line_alpha = if active { 220 } else { 160 };
                    let line = Color32::from_rgba_unmultiplied(60, 160, 255, line_alpha);
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    let ax = to_x(a);
                    if b == a {
                        painter.line_segment(
                            [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                            egui::Stroke::new(2.0, line),
                        );
                        draw_handle(ax, line);
                        painter.text(
                            egui::pos2(ax + 6.0, rect.top() + 2.0),
                            egui::Align2::LEFT_TOP,
                            "S",
                            fid,
                            Color32::from_rgb(170, 200, 255),
                        );
                    } else {
                        let bx = to_x(b);
                        let shade_alpha = if active { 40 } else { 22 };
                        let shade = Color32::from_rgba_unmultiplied(60, 160, 255, shade_alpha);
                        let r = egui::Rect::from_min_max(
                            egui::pos2(ax, rect.top()),
                            egui::pos2(bx, rect.bottom()),
                        );
                        painter.rect_filled(r, 0.0, shade);
                        painter.line_segment(
                            [egui::pos2(ax, rect.top()), egui::pos2(ax, rect.bottom())],
                            egui::Stroke::new(2.0, line),
                        );
                        painter.line_segment(
                            [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
                            egui::Stroke::new(2.0, line),
                        );
                        draw_handle(ax, line);
                        draw_handle(bx, line);
                        painter.text(
                            egui::pos2(ax + 6.0, rect.top() + 2.0),
                            egui::Align2::LEFT_TOP,
                            "S",
                            fid.clone(),
                            Color32::from_rgb(170, 200, 255),
                        );
                        painter.text(
                            egui::pos2(bx + 6.0, rect.top() + 2.0),
                            egui::Align2::LEFT_TOP,
                            "E",
                            fid.clone(),
                            Color32::from_rgb(170, 200, 255),
                        );
                        let dur = (b.saturating_sub(a)) as f32 / sr;
                        let label = crate::app::helpers::format_time_s(dur);
                        painter.text(
                            egui::pos2(ax + 6.0, rect.top() + 18.0),
                            egui::Align2::LEFT_TOP,
                            format!("Loop {label}"),
                            fid,
                            Color32::from_rgb(160, 190, 230),
                        );

                        // Crossfade bands and shape
                        let cf = Self::effective_loop_xfade_samples(
                            a,
                            b,
                            tab.samples_len,
                            tab.loop_xfade_samples,
                        );
                        if cf > 0 {
                            let head0 = a;
                            let head1 = (a + cf).min(b);
                            let tail0 = b.saturating_sub(cf);
                            let tail1 = b;
                            let xs0 = to_x(head0);
                            let xs1 = to_x(head1);
                            let xe0 = to_x(tail0);
                            let xe1 = to_x(tail1);
                            let band_alpha = if active { 50 } else { 28 };
                            let col_in = Color32::from_rgba_unmultiplied(255, 180, 60, band_alpha);
                            let col_out = Color32::from_rgba_unmultiplied(60, 180, 255, band_alpha);
                            let r_in = egui::Rect::from_min_max(
                                egui::pos2(xs0, rect.top()),
                                egui::pos2(xs1, rect.bottom()),
                            );
                            let r_out = egui::Rect::from_min_max(
                                egui::pos2(xe0, rect.top()),
                                egui::pos2(xe1, rect.bottom()),
                            );
                            painter.rect_filled(r_in, 0.0, col_in);
                            painter.rect_filled(r_out, 0.0, col_out);

                            let curve_alpha = if active { 220 } else { 140 };
                            let curve_col =
                                Color32::from_rgba_unmultiplied(255, 170, 60, curve_alpha);
                            let steps = 36;
                            let uses_dip =
                                Self::loop_xfade_uses_through_zero(tab.loop_xfade_shape);
                            let mut last_in_up: Option<egui::Pos2> = None;
                            let mut last_in_down: Option<egui::Pos2> = None;
                            let mut last_out_up: Option<egui::Pos2> = None;
                            let mut last_out_down: Option<egui::Pos2> = None;
                            let h = rect.height();
                            let y_of = |w: f32| rect.bottom() - w * h;
                            for i in 0..=steps {
                                let t = (i as f32) / (steps as f32);
                                let (w_out, w_in) =
                                    Self::loop_xfade_weights(tab.loop_xfade_shape, t);
                                let x_in = egui::lerp(xs0..=xs1, t);
                                let p_in_up = egui::pos2(x_in, y_of(w_in));
                                let p_in_down = egui::pos2(x_in, y_of(w_out));
                                if let Some(lp) = last_in_up {
                                    painter.line_segment(
                                        [lp, p_in_up],
                                        egui::Stroke::new(2.0, curve_col),
                                    );
                                }
                                if !uses_dip {
                                    if let Some(lp) = last_in_down {
                                        painter.line_segment(
                                            [lp, p_in_down],
                                            egui::Stroke::new(2.0, curve_col),
                                        );
                                    }
                                }
                                last_in_up = Some(p_in_up);
                                last_in_down = if uses_dip { None } else { Some(p_in_down) };

                                let x_out = egui::lerp(xe0..=xe1, t);
                                let p_out_up = egui::pos2(x_out, y_of(w_in));
                                let p_out_down = egui::pos2(x_out, y_of(w_out));
                                if let Some(lp) = last_out_down {
                                    painter.line_segment(
                                        [lp, p_out_down],
                                        egui::Stroke::new(2.0, curve_col),
                                    );
                                }
                                if !uses_dip {
                                    if let Some(lp) = last_out_up {
                                        painter.line_segment(
                                            [lp, p_out_up],
                                            egui::Stroke::new(2.0, curve_col),
                                        );
                                    }
                                }
                                last_out_up = if uses_dip { None } else { Some(p_out_up) };
                                last_out_down = Some(p_out_down);
                            }
                        }
                    }
                }

                // Fade overlays
                let draw_fade = |x0: f32, x1: f32, shape: crate::app::types::FadeShape, is_in: bool, base_col: Color32| {
                    let steps = 28;
                    let max_alpha = 80.0;
                    for i in 0..steps {
                        let t0 = i as f32 / steps as f32;
                        let t1 = (i + 1) as f32 / steps as f32;
                        let w0 = if is_in { Self::fade_weight(shape, t0) } else { Self::fade_weight_out(shape, t0) };
                        let w1 = if is_in { Self::fade_weight(shape, t1) } else { Self::fade_weight_out(shape, t1) };
                        let vol0 = w0;
                        let vol1 = w1;
                        let vol = (vol0 + vol1) * 0.5;
                        let alpha = ((1.0 - vol) * max_alpha).clamp(0.0, 255.0) as u8;
                        if alpha == 0 { continue; }
                        let rx0 = egui::lerp(x0..=x1, t0);
                        let rx1 = egui::lerp(x0..=x1, t1);
                        let r = egui::Rect::from_min_max(
                            egui::pos2(rx0, rect.top()),
                            egui::pos2(rx1, rect.bottom()),
                        );
                        painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(base_col.r(), base_col.g(), base_col.b(), alpha));
                    }
                    let curve_col = Color32::from_rgba_unmultiplied(base_col.r(), base_col.g(), base_col.b(), 200);
                    let mut last: Option<egui::Pos2> = None;
                    for i in 0..=steps {
                        let t = i as f32 / steps as f32;
                        let w = if is_in { Self::fade_weight(shape, t) } else { Self::fade_weight_out(shape, t) };
                        let vol = w;
                        let x = egui::lerp(x0..=x1, t);
                        let y = rect.bottom() - vol * rect.height();
                        let p = egui::pos2(x, y);
                        if let Some(lp) = last {
                            painter.line_segment([lp, p], egui::Stroke::new(2.0, curve_col));
                        }
                        last = Some(p);
                    }
                };
                if tab.active_tool == ToolKind::Fade {
                    let n_in = ((tab.tool_state.fade_in_ms / 1000.0) * sr).round() as usize;
                    if n_in > 0 {
                        let end = n_in.min(tab.samples_len);
                        let x0 = to_x(0);
                        let x1 = to_x(end);
                        if x1 > x0 + 1.0 {
                            draw_fade(
                                x0,
                                x1,
                                tab.fade_in_shape,
                                true,
                                Color32::from_rgb(80, 180, 255),
                            );
                            fade_in_handle = Some(x1);
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let secs = (end as f32) / sr;
                            painter.text(
                                egui::pos2(x0 + 6.0, rect.bottom() - 18.0),
                                egui::Align2::LEFT_BOTTOM,
                                format!(
                                    "Fade In {}",
                                    crate::app::helpers::format_time_s(secs)
                                ),
                                fid,
                                Color32::from_rgb(150, 190, 230),
                            );
                        }
                    }
                    let n_out = ((tab.tool_state.fade_out_ms / 1000.0) * sr).round() as usize;
                    if n_out > 0 {
                        let start_out = tab.samples_len.saturating_sub(n_out);
                        let x0 = to_x(start_out);
                        let x1 = to_x(tab.samples_len);
                        if x1 > x0 + 1.0 {
                            draw_fade(
                                x0,
                                x1,
                                tab.fade_out_shape,
                                false,
                                Color32::from_rgb(255, 160, 90),
                            );
                            fade_out_handle = Some(x0);
                            let fid = TextStyle::Monospace.resolve(ui.style());
                            let secs = (n_out as f32) / sr;
                            painter.text(
                                egui::pos2(x0 + 6.0, rect.bottom() - 18.0),
                                egui::Align2::LEFT_BOTTOM,
                                format!(
                                    "Fade Out {}",
                                    crate::app::helpers::format_time_s(secs)
                                ),
                                fid,
                                Color32::from_rgb(230, 190, 150),
                            );
                        }
                    }
                }

                // Cursor feedback for editor handles
                if pointer_over_waveform {
                    let handle_radius = 7.0;
                    if tab.dragging_marker.is_some() {
                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                    } else if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                        let x = pos.x;
                        let near = |hx: f32| (x - hx).abs() <= handle_radius;
                        match tab.active_tool {
                            ToolKind::LoopEdit => {
                                if let Some((a0, b0)) = tab.loop_region {
                                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                    let ax = to_x(a);
                                    let bx = to_x(b);
                                    if near(ax) || near(bx) {
                                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                    }
                                }
                            }
                            ToolKind::Trim => {
                                if let Some((a0, b0)) = tab.trim_range {
                                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                    let ax = to_x(a);
                                    let bx = to_x(b);
                                    if near(ax) || near(bx) {
                                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                    }
                                }
                            }
                            ToolKind::Fade => {
                                if let Some(xh) = fade_in_handle {
                                    if near(xh) {
                                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                    }
                                }
                                if let Some(xh) = fade_out_handle {
                                    if near(xh) {
                                        hover_cursor = Some(egui::CursorIcon::ResizeHorizontal);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(icon) = hover_cursor {
                    ui.output_mut(|o| o.cursor_icon = icon);
                }
            }

            // Shared playhead across lanes
            if tab_samples_len > 0 {
                let len = self.audio.current_source_len();
                if len > 0 {
                    let pos_audio = self
                        .audio
                        .shared
                        .play_pos
                        .load(std::sync::atomic::Ordering::Relaxed)
                        .min(len);
                    let pos = map_audio_to_display(tab, pos_audio);
                    let x = geom.sample_center_x(pos.min(display_samples_len.saturating_sub(1)));
                    painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(2.0, Color32::from_rgb(70,140,255)));
                    // Playhead time label
                    let sr_f = sr_ctx.max(1.0);
                    let pos_time = (pos as f32) / sr_f;
                    let label = crate::app::helpers::format_time_s(pos_time);
                    let fid = TextStyle::Monospace.resolve(ui.style());
                    let text_pos = egui::pos2(x + 6.0, rect.top() + 2.0);
                    painter.text(text_pos, egui::Align2::LEFT_TOP, label, fid, Color32::from_rgb(180, 200, 220));
                }
            }

            if let Some(amp_rect) = amplitude_nav_rect {
                if let Some((next_zoom, next_center)) =
                    Self::draw_editor_amplitude_navigator(ui, amp_rect, tab)
                {
                    tab.vertical_zoom = next_zoom
                        .clamp(crate::app::EDITOR_MIN_VERTICAL_ZOOM, crate::app::EDITOR_MAX_VERTICAL_ZOOM);
                    tab.vertical_view_center = next_center;
                    Self::editor_clamp_vertical_view(tab);
                    tab.last_amplitude_viewport_rect = Some(Self::amplitude_nav_viewport_rect(
                        amp_rect,
                        tab.vertical_zoom,
                        tab.vertical_view_center,
                    ));
                }
            }

            if tab_samples_len > 0 {
                let spp = tab.samples_per_px.max(0.0001);
                let vis = (wave_w * spp).ceil() as usize;
                let max_left = tab_samples_len.saturating_sub(vis);
                if tab.view_offset > max_left {
                    Self::editor_set_view_offset(tab, max_left);
                }
                let overview = if tab.loading && !tab.loading_waveform_minmax.is_empty() {
                    tab.loading_waveform_minmax.as_slice()
                } else {
                    tab.waveform_minmax.as_slice()
                };
                if let Some(next_view) = Self::draw_editor_time_navigator(
                    ui,
                    overview,
                    tab_samples_len,
                    sr_ctx.max(1.0) as u32,
                    tab.view_offset,
                    vis,
                    gutter_w,
                    wave_w,
                ) {
                    Self::editor_set_view_offset(tab, next_view);
                }
            }

            if tab.loading {
                let (msg, progress) = decode_status
                    .as_ref()
                    .map(|status| (status.message.as_str(), status.progress))
                    .unwrap_or(("Loading audio", 0.0));
                let overlay_rect = egui::Rect::from_min_size(
                    egui::pos2(wave_left, rect.top()),
                    egui::vec2(wave_w, rect.height()),
                )
                .shrink(10.0);
                painter.rect_filled(
                    overlay_rect,
                    6.0,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 150),
                );
                let fid = TextStyle::Monospace.resolve(ui.style());
                let label = if tab.samples_len == 0 {
                    format!("{msg}...")
                } else {
                    format!("Waveform preview only. {msg}... Playback locked.")
                };
                painter.text(
                    overlay_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    fid,
                    Color32::from_rgb(220, 220, 230),
                );
                let bar_w = overlay_rect.width().min(240.0);
                let bar_h = 6.0;
                let bar_left = overlay_rect.center().x - (bar_w * 0.5);
                let bar_top = overlay_rect.center().y + 18.0;
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(bar_left, bar_top),
                    egui::vec2(bar_w, bar_h),
                );
                painter.rect_filled(bar_rect, 3.0, Color32::from_rgb(40, 40, 45));
                let fill_w = (bar_w * progress.clamp(0.0, 1.0)).max(2.0);
                let fill_rect = egui::Rect::from_min_size(
                    egui::pos2(bar_left, bar_top),
                    egui::vec2(fill_w, bar_h),
                );
                painter.rect_filled(fill_rect, 3.0, Color32::from_rgb(90, 160, 240));
            }
                },
                ); // end canvas UI

                // Inspector area (right)
                ui.allocate_ui_with_layout(
                    egui::vec2(inspector_w, avail.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                    ui.set_width(inspector_w);
                    ui.heading("Inspector");
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt(("editor_inspector_scroll", tab_idx))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                    if let Some(status) = decode_status.as_ref() {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(status.message.as_str()).strong());
                            let mut bar =
                                egui::ProgressBar::new(status.progress).desired_width(120.0);
                            if status.show_percentage {
                                bar = bar.show_percentage();
                            }
                            ui.add(bar);
                            if ui.button("Cancel").clicked() {
                                cancel_decode = true;
                            }
                        });
                        ui.separator();
                    }
                    if let Some(apply_msg) = apply_msg.as_ref() {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(apply_msg.as_str()).strong());
                            if ui.button("Cancel").clicked() {
                                cancel_apply = true;
                            }
                        });
                        ui.separator();
                    }
                    if let Some((msg, started_at)) = processing_msg {
                        let elapsed = started_at.elapsed().as_secs_f32();
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(format!(
                                "{} ({:.1}s)",
                                msg,
                                elapsed
                            )).weak());
                            if ui.button("Cancel").clicked() {
                                cancel_processing = true;
                            }
                        });
                        ui.separator();
                    }
                    if let Some(msg) = preview_msg.as_ref() {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(msg.as_str()).weak());
                            if ui.button("Cancel").clicked() {
                                cancel_preview = true;
                            }
                        });
                        ui.separator();
                    }
                    if analysis_loading {
                        let (done, total, started_at) =
                            analysis_progress.unwrap_or((0, 0, std::time::Instant::now()));
                        let pct = if total > 0 {
                            (done as f32 / total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let elapsed = started_at.elapsed().as_secs_f32();
                        ui.horizontal_wrapped(|ui| {
                            ui.add(egui::Spinner::new());
                            ui.label(RichText::new(format!(
                                "{}... ({:.1}s)",
                                analysis_label,
                                elapsed
                            )).weak());
                            if total > 0 {
                                ui.add(
                                    egui::ProgressBar::new(pct)
                                        .desired_width(120.0)
                                        .show_percentage(),
                                );
                            }
                            if ui.button("Cancel").clicked() {
                                match current_view {
                                    ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => {
                                        cancel_spectro = true;
                                    }
                                    ViewMode::Tempogram | ViewMode::Chromagram => {
                                        cancel_feature_analysis = true;
                                    }
                                    ViewMode::Waveform => {}
                                }
                            }
                        });
                        ui.separator();
                    }
                    let can_undo = !tab.undo_stack.is_empty();
                    let can_redo = !tab.redo_stack.is_empty();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(can_undo, egui::Button::new("Undo"))
                            .clicked()
                        {
                            request_undo = true;
                        }
                        if ui
                            .add_enabled(can_redo, egui::Button::new("Redo"))
                            .clicked()
                        {
                            request_redo = true;
                        }
                    });
                    let sr = sr_ctx.max(1.0);
                    let range_info = tab
                        .selection
                        .map(|r| ("Selection", r))
                        .or_else(|| tab.trim_range.map(|r| ("Trim", r)))
                        .or_else(|| tab.loop_region.map(|r| ("Loop", r)));
                    if let Some((kind, (a0, b0))) = range_info {
                        let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                        let len = b.saturating_sub(a);
                        let start_sec = (a as f32 / sr).max(0.0);
                        let end_sec = (b as f32 / sr).max(0.0);
                        let len_sec = (len as f32 / sr).max(0.0);
                        ui.label(
                            RichText::new(format!(
                                "{kind}: {a}..{b} ({len} smp) / {}..{} ({})",
                                crate::app::helpers::format_time_s(start_sec),
                                crate::app::helpers::format_time_s(end_sec),
                                crate::app::helpers::format_time_s(len_sec)
                            ))
                            .monospace(),
                        );
                    } else {
                        ui.label(RichText::new("Range: -").monospace().weak());
                    }
                    ui.separator();
                    let leaf_view = tab.leaf_view_mode();
                    match leaf_view {
                        ViewMode::Waveform => {
                            // Tool selector
                            let mut tool = tab.active_tool;
                            egui::ComboBox::new("tool_selector", "Tool")
                                .selected_text(format!("{:?}", tool))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut tool, ToolKind::LoopEdit, "Loop Edit");
                                    ui.selectable_value(&mut tool, ToolKind::Markers, "Markers");
                                    ui.selectable_value(&mut tool, ToolKind::Trim, "Trim");
                                    ui.selectable_value(&mut tool, ToolKind::Fade, "Fade");
                                    ui.selectable_value(&mut tool, ToolKind::PitchShift, "PitchShift");
                                    ui.selectable_value(&mut tool, ToolKind::TimeStretch, "TimeStretch");
                                    ui.selectable_value(&mut tool, ToolKind::Gain, "Gain");
                                    ui.selectable_value(&mut tool, ToolKind::Normalize, "Normalize");
                                    ui.selectable_value(&mut tool, ToolKind::Loudness, "LoudNorm");
                                    ui.selectable_value(&mut tool, ToolKind::Reverse, "Reverse");
                                    ui.selectable_value(
                                        &mut tool,
                                        ToolKind::MusicAnalyze,
                                        "Music Analyze",
                                    );
                                    ui.selectable_value(&mut tool, ToolKind::PluginFx, "Plugin FX");
                                });
                            if tool != tab.active_tool {
                                tab.active_tool_last = Some(tab.active_tool);
                                // Leaving Markers/LoopEdit: discard un-applied preview markers/loops
                                if matches!(tab.active_tool, ToolKind::Markers) {
                                    if tab.markers != tab.markers_committed {
                                        tab.markers = tab.markers_committed.clone();
                                        Self::update_markers_dirty(tab);
                                    }
                                }
                                if matches!(tab.active_tool, ToolKind::LoopEdit) {
                                    if tab.loop_region != tab.loop_region_applied {
                                        tab.loop_region =
                                            tab.loop_region_applied.or(tab.loop_region_committed);
                                    }
                                    tab.pending_loop_unwrap = None;
                                    if tab.markers != tab.markers_committed {
                                        tab.markers = tab.markers_committed.clone();
                                        Self::update_markers_dirty(tab);
                                    }
                                    Self::update_loop_markers_dirty(tab);
                                }
                                if matches!(tab.active_tool, ToolKind::Trim) {
                                    // Trim-specific range display should not persist after leaving Trim.
                                    tab.trim_range = None;
                                }
                                if matches!(tab.active_tool, ToolKind::MusicAnalyze) {
                                    tab.music_analysis_draft.provisional_markers.clear();
                                    tab.markers = tab.markers_committed.clone();
                                    Self::update_markers_dirty(tab);
                                    tab.music_analysis_draft.stems_audio = None;
                                    tab.music_analysis_draft.preview_inflight = false;
                                    tab.music_analysis_draft.preview_active = false;
                                    pending_music_preview_cancel = true;
                                }
                                // Leaving a tool: discard any preview overlay/audio
                                if tab.preview_audio_tool.is_some() || tab.preview_overlay.is_some() {
                                    need_restore_preview = true;
                                }
                                stop_playback = true;
                                tab.active_tool = tool;
                            }
                            ui.separator();
                            ui.label(RichText::new(format!("Tool: {:?}", tab.active_tool)).strong());
                            match tab.active_tool {
                                // Seek/Select removed: seeking is always available on the canvas
                                ToolKind::LoopEdit => {
                                    // compact spacing for inspector controls
                                    ui.scope(|ui| {
                                        let s = ui.style_mut();
                                        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                        s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                        ui.label("Loop marker range");
                                        if let Some((a0, b0)) =
                                            Self::normalized_loop_range(tab.loop_region_applied)
                                        {
                                            let (a, b) = (a0, b0);
                                            let len = b.saturating_sub(a);
                                            ui.label(
                                                RichText::new(format!(
                                                    "Applied Loop: {a}..{b} ({len} smp)"
                                                ))
                                                .monospace()
                                                .weak(),
                                            );
                                        } else {
                                            ui.label(
                                                RichText::new("Applied Loop: -")
                                                    .monospace()
                                                    .weak(),
                                            );
                                        }
                                        if let Some((a0, b0)) =
                                            Self::normalized_loop_range(tab.loop_region)
                                        {
                                            let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                            let len = b.saturating_sub(a);
                                            ui.label(
                                                RichText::new(format!(
                                                    "Editing Loop: {a}..{b} ({len} smp)"
                                                ))
                                                    .monospace(),
                                            );
                                        } else {
                                            ui.label(
                                                RichText::new("Editing Loop: -")
                                                    .monospace()
                                                    .weak(),
                                            );
                                        }
                                        if let Some((a0, b0)) = tab.selection {
                                            let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                            let len = b.saturating_sub(a);
                                            ui.label(
                                                RichText::new(format!("Range: {a}..{b} ({len} smp)"))
                                                    .monospace()
                                                    .weak(),
                                            );
                                        } else {
                                            ui.label(RichText::new("Range: -").monospace().weak());
                                        }
                                        let effective_cf = tab
                                            .loop_region
                                            .map(|(a, b)| {
                                                Self::effective_loop_xfade_samples(
                                                    a,
                                                    b,
                                                    tab.samples_len,
                                                    tab.loop_xfade_samples,
                                                )
                                            })
                                            .unwrap_or(0);
                                        let editing_loop =
                                            Self::normalized_loop_range(tab.loop_region);
                                        let applied_loop = Self::normalized_loop_range(
                                            tab.loop_region_applied.or(tab.loop_region_committed),
                                        );
                                        let saved_loop =
                                            Self::normalized_loop_range(tab.loop_markers_saved);
                                        let loop_preview_pending = editing_loop != applied_loop
                                            || tab.pending_loop_unwrap.is_some()
                                            || effective_cf > 0;
                                        let loop_saved_pending = applied_loop != saved_loop;
                                        let (loop_status_color, loop_status_text, loop_status_hint) =
                                            if !loop_preview_pending && !loop_saved_pending {
                                                (
                                                    Color32::from_rgb(160, 200, 160),
                                                    "Saved",
                                                    "Loop markers match the file/session baseline",
                                                )
                                            } else if !loop_preview_pending {
                                                (
                                                    Color32::from_rgb(255, 180, 60),
                                                    "Applied (pending save)",
                                                    "Loop markers are applied in the editor but not saved yet",
                                                )
                                            } else {
                                                (
                                                    Color32::from_rgb(120, 220, 120),
                                                    "Preview (not applied)",
                                                    "Loop markers or seam fade are staged but not applied yet",
                                                )
                                            };
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new("\u{25CF}")
                                                    .color(loop_status_color)
                                                    .strong(),
                                            )
                                            .on_hover_text(loop_status_hint);
                                            ui.label(loop_status_text);
                                        });
                                        // Crossfade controls (duration in ms + shape)
                                        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                        let mut x_ms = (tab.loop_xfade_samples as f32 / sr) * 1000.0;
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label("Xfade (ms):");
                                            let resp_x = ui.add(egui::DragValue::new(&mut x_ms).range(0.0..=5000.0).speed(5.0).fixed_decimals(1));
                                            if (resp_x.gained_focus() || resp_x.drag_started()) && pending_edit_undo.is_none() {
                                                pending_edit_undo = Some(Self::capture_undo_state(tab));
                                            }
                                            if resp_x.changed() {
                                                let samp = ((x_ms / 1000.0) * sr).round().clamp(0.0, tab.samples_len as f32) as usize;
                                                tab.loop_xfade_samples = samp;
                                                apply_pending_loop = true;
                                            }
                                            let mut use_dip = Self::loop_xfade_uses_through_zero(tab.loop_xfade_shape);
                                            ui.label("Mode:");
                                            egui::ComboBox::from_id_salt("xfade_mode")
                                                .selected_text(if use_dip { "Fade to 0" } else { "Crossfade" })
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(&mut use_dip, false, "Crossfade");
                                                    ui.selectable_value(&mut use_dip, true, "Fade to 0");
                                                });
                                            ui.label("Shape:");
                                            let mut shp = tab.loop_xfade_shape;
                                            let mut use_equal = matches!(
                                                shp,
                                                crate::app::types::LoopXfadeShape::EqualPower
                                                    | crate::app::types::LoopXfadeShape::EqualPowerDip
                                            );
                                            egui::ComboBox::from_id_salt("xfade_shape")
                                                .selected_text(if use_equal { "Equal" } else { "Linear" })
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(&mut use_equal, false, "Linear");
                                                    ui.selectable_value(&mut use_equal, true, "Equal");
                                                });
                                            shp = match (use_dip, use_equal) {
                                                (false, false) => crate::app::types::LoopXfadeShape::Linear,
                                                (false, true) => crate::app::types::LoopXfadeShape::EqualPower,
                                                (true, false) => crate::app::types::LoopXfadeShape::LinearDip,
                                                (true, true) => crate::app::types::LoopXfadeShape::EqualPowerDip,
                                            };
                                            if shp != tab.loop_xfade_shape {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                tab.loop_xfade_shape = shp;
                                                apply_pending_loop = true;
                                            }
                                        });
                                        ui.horizontal_wrapped(|ui| {
                                            let can_set = tab
                                                .selection
                                                .map(|(a0, b0)| {
                                                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                                    b > a
                                                })
                                                .unwrap_or(false);
                                            let set_resp = ui
                                                .add_enabled(can_set, egui::Button::new("Set"))
                                                .on_hover_text("Use current range as loop markers");
                                            if set_resp.clicked()
                                            {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                if let Some((a0, b0)) = tab.selection {
                                                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                                    tab.loop_region = Some((a, b));
                                                    tab.pending_loop_unwrap = None;
                                                    tab.preview_audio_tool = None;
                                                    tab.preview_overlay = None;
                                                    Self::update_loop_markers_dirty(tab);
                                                    apply_pending_loop = true;
                                                }
                                            }
                                            if ui.button("Clear").clicked() {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                do_set_loop_from = Some((0,0));
                                            }
                                        });

                                        // Crossfade controls already above; add Apply button to destructively bake Xfade
                                        ui.horizontal_wrapped(|ui| {
                                            let mut repeat = tab.tool_state.loop_repeat.max(2);
                                            ui.label("Repeat:");
                                            if ui
                                                .add(
                                                    egui::DragValue::new(&mut repeat)
                                                        .range(2..=128)
                                                        .speed(1),
                                                )
                                                .changed()
                                            {
                                                tab.tool_state =
                                                    ToolState { loop_repeat: repeat, ..tab.tool_state };
                                            }
                                            let has_loop = tab
                                                .loop_region
                                                .map(|(a, b)| b > a)
                                                .unwrap_or(false);
                                            if ui
                                                .add_enabled(
                                                    has_loop && !apply_busy,
                                                    egui::Button::new(format!("Unwrap x{}", repeat)),
                                                )
                                                .on_hover_text("Preview loop unwrap (non-destructive until Apply)")
                                                .clicked()
                                            {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                do_preview_unwrap = Some(repeat);
                                                stop_playback = true;
                                                tab.pending_loop_unwrap = Some(repeat);
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                            }                                        ui.horizontal_wrapped(|ui| {
                                            let effective_cf = tab
                                                .loop_region
                                                .map(|(a, b)| {
                                                    Self::effective_loop_xfade_samples(
                                                        a,
                                                        b,
                                                        tab.samples_len,
                                                        tab.loop_xfade_samples,
                                                    )
                                                })
                                                .unwrap_or(0);
                                            let is_loop_dirty = Self::normalized_loop_range(
                                                tab.loop_region,
                                            ) != Self::normalized_loop_range(
                                                tab.loop_region_applied,
                                            );
                                            let unwrap_pending = tab.pending_loop_unwrap.is_some();
                                            let can_apply = (is_loop_dirty || effective_cf > 0 || unwrap_pending) && !apply_busy;
                                            if ui
                                                .add_enabled(
                                                    can_apply,
                                                    egui::Button::new("Apply"),
                                                )
                                                .on_hover_text(
                                                    "Commit loop changes and bake crossfade",
                                                )
                                                .clicked()
                                            {
                                                do_commit_loop = true;
                                            }
                                        });


                                        });

                                        if let Some(seam_preview) = Self::build_loop_seam_preview(
                                            tab,
                                            self.audio.shared.out_sample_rate,
                                        ) {
                                            ui.separator();
                                            ui.label("Loop inspector");
                                            ui.columns(3, |cols| {
                                                cols[0].label("Pre-Loop window");
                                                Self::draw_loop_window_preview(
                                                    &mut cols[0],
                                                    &seam_preview.raw_left,
                                                    seam_preview.sample_rate,
                                                    Color32::from_rgb(120, 176, 255),
                                                    tab.vertical_zoom,
                                                    tab.vertical_view_center,
                                                );
                                                cols[1].label("Seam preview");
                                                Self::draw_loop_seam_preview(
                                                    &mut cols[1],
                                                    &seam_preview,
                                                    tab.vertical_zoom,
                                                    tab.vertical_view_center,
                                                );
                                                cols[2].label("Post-Loop window");
                                                Self::draw_loop_window_preview(
                                                    &mut cols[2],
                                                    &seam_preview.raw_right,
                                                    seam_preview.sample_rate,
                                                    Color32::from_rgb(92, 255, 224),
                                                    tab.vertical_zoom,
                                                    tab.vertical_view_center,
                                                );
                                            });
                                        } else {
                                            ui.separator();
                                            ui.label(RichText::new("Loop inspector: -").weak());
                                        }

                                        // Dynamic preview overlay for LoopEdit (non-destructive):
                                        // Build a mono preview applying the current loop crossfade to the mixdown.
                                        if let Some(reason) = preview_disabled_reason {
                                            ui.label(RichText::new(reason).weak());
                                        } else if let Some((a,b)) = tab.loop_region {
                                            let cf = Self::effective_loop_xfade_samples(
                                                a,
                                                b,
                                                tab.samples_len,
                                                tab.loop_xfade_samples,
                                            );
                                            if cf > 0 {
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                Self::apply_loop_xfade_to_channels(
                                                    &mut overlay,
                                                    a,
                                                    b,
                                                    cf,
                                                    tab.loop_xfade_shape,
                                                );
                                                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                    overlay,
                                                    ToolKind::LoopEdit,
                                                    timeline_len,
                                                ));
                                                tab.preview_audio_tool = Some(ToolKind::LoopEdit);
                                            }
                                        }
                                    });
                                }

                                                                    ToolKind::Markers => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut();
                                        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                        s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                        let out_sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("Add at Playhead").clicked() {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                let pos = playhead_display_now;
                                                let label = Self::next_marker_label(&tab.markers);
                                                let entry = crate::markers::MarkerEntry {
                                                    sample: pos,
                                                    label,
                                                };
                                                match tab.markers.binary_search_by_key(&pos, |m| m.sample) {
                                                    Ok(idx) => {
                                                        tab.markers[idx] = entry;
                                                    }
                                                    Err(idx) => {
                                                        tab.markers.insert(idx, entry);
                                                    }
                                                }
                                                Self::update_markers_dirty(tab);
                                            }
                                            if ui
                                                .add_enabled(
                                                    !tab.markers.is_empty(),
                                                    egui::Button::new("Clear"),
                                                )
                                                .clicked()
                                            {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                tab.markers.clear();
                                                Self::update_markers_dirty(tab);
                                            }
                                        });
                                        ui.horizontal_wrapped(|ui| {
                                            let can_apply = tab.markers != tab.markers_committed && !apply_busy;
                                            if ui
                                                .add_enabled(can_apply, egui::Button::new("Apply"))
                                                .on_hover_text("Commit markers (written on Save Selected)")
                                                .clicked()
                                            {
                                                do_commit_markers = true;
                                            }
                                        });
                                        let markers_preview_pending = tab.markers != tab.markers_committed;
                                        let markers_saved_pending = tab.markers != tab.markers_saved;
                                        let (dot_color, dot_hint, marker_status) =
                                            if !markers_preview_pending && !markers_saved_pending {
                                                (
                                                    Color32::from_rgb(160, 200, 160),
                                                    "Markers match the file/session baseline",
                                                    "Saved",
                                                )
                                            } else if !markers_preview_pending {
                                                (
                                                    Color32::from_rgb(255, 180, 60),
                                                    "Markers are applied in the editor but not saved yet",
                                                    "Applied (pending save)",
                                                )
                                            } else {
                                                (
                                                    Color32::from_rgb(120, 220, 120),
                                                    "Marker edits are staged but not applied yet",
                                                    "Preview (not applied)",
                                                )
                                            };
                                        ui.label(format!("Count: {}", tab.markers.len()));
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new("\u{25CF}")
                                                    .color(dot_color)
                                                    .strong(),
                                            )
                                            .on_hover_text(dot_hint);
                                            ui.label(marker_status);
                                        });
                                        if !tab.markers.is_empty() {
                                            ui.horizontal(|ui| {
                                                ui.label(
                                                    RichText::new("\u{25CF}")
                                                        .color(dot_color)
                                                        .strong(),
                                                );
                                                ui.label("Label");
                                                ui.label("Sec");
                                                ui.label("Time");
                                                ui.label("");
                                            });
                                            let samples_len = tab.samples_len;
                                            let mut len_sec = (samples_len as f32 / out_sr).max(0.0);
                                            if !len_sec.is_finite() { len_sec = 0.0; }
                                            let mut markers_local = tab.markers.clone();
                                            let mut remove_idx: Option<usize> = None;
                                            let mut resort = false;
                                            let mut dirty = false;
                                            egui::ScrollArea::vertical()
                                                .max_height(160.0)
                                                .show(ui, |ui| {
                                                    for (idx, m) in markers_local.iter_mut().enumerate() {
                                                        let mut secs = (m.sample as f32) / out_sr;
                                                        if !secs.is_finite() { secs = 0.0; }
                                                        if secs > len_sec { secs = len_sec; }
                                                        ui.horizontal(|ui| {
                                                            ui.label(
                                                                RichText::new("\u{25CF}")
                                                                    .color(dot_color),
                                                            )
                                                            .on_hover_text(dot_hint);
                                                            let resp = ui.add(
                                                                egui::TextEdit::singleline(&mut m.label)
                                                                    .desired_width(80.0),
                                                            );
                                                            if resp.changed() {
                                                                dirty = true;
                                                            }
                                                            let resp_time = ui.add(
                                                                egui::DragValue::new(&mut secs)
                                                                    .range(0.0..=len_sec)
                                                                    .speed(0.01)
                                                                    .fixed_decimals(3),
                                                            );
                                                            let time_changed = resp_time.changed();
                                                            if time_changed {
                                                                let sample = ((secs.max(0.0)) * out_sr)
                                                                    .round() as usize;
                                                                m.sample = sample.min(samples_len);
                                                                dirty = true;
                                                                resort = true;
                                                            }
                                                            ui.label(crate::app::helpers::format_time_s(secs));
                                                            if ui.button("Delete").clicked() {
                                                                remove_idx = Some(idx);
                                                            }
                                                        });
                                                    }
                                                });
                                            if let Some(idx) = remove_idx {
                                                if idx < markers_local.len() {
                                                    markers_local.remove(idx);
                                                }
                                                dirty = true;
                                            }
                                            if resort {
                                                markers_local.sort_by_key(|m| m.sample);
                                            }
                                            if dirty {
                                                if pending_edit_undo.is_none() {
                                                    pending_edit_undo = Some(Self::capture_undo_state(tab));
                                                }
                                                tab.markers = markers_local;
                                                Self::update_markers_dirty(tab);
                                            }
                                        }
                                    });
                                }
    ToolKind::Trim => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut();
                                        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                        s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                        if let Some(reason) = preview_disabled_reason {
                                            ui.label(RichText::new(reason).weak());
                                        }
                                        // Trim range is separated from loop markers and set from generic selection.
                                        let mut range_opt = tab.trim_range;
                                        if let Some((smp, emp)) = range_opt {
                                            let (s, e) = if smp <= emp { (smp, emp) } else { (emp, smp) };
                                            ui.label(
                                                RichText::new(format!("Trim: {s}..{e} ({} smp)", e.saturating_sub(s)))
                                                    .monospace(),
                                            );
                                        } else {
                                            ui.label(RichText::new("Trim: -").monospace().weak());
                                        }

                                        ui.horizontal_wrapped(|ui| {
                                            let can_set = tab
                                                .selection
                                                .map(|(a0, b0)| {
                                                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                                    b > a
                                                })
                                                .unwrap_or(false);
                                            let set_resp = ui
                                                .add_enabled(can_set, egui::Button::new("Set"))
                                                .on_hover_text("Use current range as trim range");
                                            if set_resp.clicked() {
                                                if let Some((a0, b0)) = tab.selection {
                                                    let (a, b) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
                                                    tab.trim_range = Some((a, b));
                                                    range_opt = tab.trim_range;
                                                    if preview_ok && b > a {
                                                        let mut mono = Self::editor_mixdown_mono(tab);
                                                        mono = mono[a..b].to_vec();
                                                        pending_preview = Some((ToolKind::Trim, mono));
                                                        stop_playback = true;
                                                        tab.preview_audio_tool = Some(ToolKind::Trim);
                                                    } else {
                                                        tab.preview_audio_tool = None;
                                                        tab.preview_overlay = None;
                                                    }
                                                }
                                            }
                                        });

                                        ui.horizontal_wrapped(|ui| {
                                            let dis = !range_opt.map(|(s, e)| e > s).unwrap_or(false);
                                            let range = range_opt.unwrap_or((0, 0));
                                            if ui.add_enabled(!dis, egui::Button::new("Apply cut")).clicked() {
                                                do_cutjoin = Some(range);
                                            }
                                            if ui.add_enabled(!dis, egui::Button::new("Apply mute")).clicked() {
                                                do_mute = Some(range);
                                            }
                                            if ui.add_enabled(!dis, egui::Button::new("Apply trim")).clicked() {
                                                do_trim = Some(range);
                                                tab.preview_audio_tool = None;
                                            }
                                            if ui
                                                .add_enabled(!dis, egui::Button::new("Add Trim As Virtual"))
                                                .clicked()
                                            {
                                                do_trim_virtual = Some(range);
                                            }
                                        });
                                    });
                                }
                                ToolKind::Fade => {
                                    // Simplified: duration (seconds) from start/end + Apply
                                    ui.scope(|ui| {
                                        let s = ui.style_mut();
                                        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                        s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                        if let Some(reason) = preview_disabled_reason {
                                            ui.label(RichText::new(reason).weak());
                                        }
                                        if let Some(note) = simplified_preview_note {
                                            ui.label(RichText::new(note).weak());
                                        }
                                        let sr = self.audio.shared.out_sample_rate.max(1) as f32;
                                        let shape_label = |shape: crate::app::types::FadeShape| match shape {
                                            crate::app::types::FadeShape::Linear => "Linear",
                                            crate::app::types::FadeShape::EqualPower => "Equal",
                                            crate::app::types::FadeShape::Cosine => "Cosine",
                                            crate::app::types::FadeShape::SCurve => "S-Curve",
                                            crate::app::types::FadeShape::Quadratic => "Quadratic",
                                            crate::app::types::FadeShape::Cubic => "Cubic",
                                        };
                                        // Fade In
                                        ui.label("Fade In");
                                        ui.horizontal_wrapped(|ui| {
                                            let mut secs = tab.tool_state.fade_in_ms / 1000.0;
                                            if !secs.is_finite() { secs = 0.0; }
                                            ui.label("duration (s)");
                                            let mut changed = ui
                                                .add(
                                                    egui::DragValue::new(&mut secs)
                                                        .range(0.0..=600.0)
                                                        .speed(0.05)
                                                        .fixed_decimals(2),
                                                )
                                                .changed();
                                            ui.label("shape");
                                            let mut shape = tab.fade_in_shape;
                                            egui::ComboBox::from_id_salt("fade_in_shape")
                                                .selected_text(shape_label(shape))
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Linear,
                                                        "Linear",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::EqualPower,
                                                        "Equal",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Cosine,
                                                        "Cosine",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::SCurve,
                                                        "S-Curve",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Quadratic,
                                                        "Quadratic",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Cubic,
                                                        "Cubic",
                                                    );
                                                });
                                            if shape != tab.fade_in_shape {
                                                tab.fade_in_shape = shape;
                                                changed = true;
                                            }
                                            if changed {
                                                tab.tool_state = ToolState{ fade_in_ms: (secs*1000.0).max(0.0), ..tab.tool_state };
                                                if preview_ok {
                                                    // Live preview (per-channel overlay) + mono audition
                                                    let n = ((secs) * sr).round() as usize;
                                                    // Build overlay per channel
                                                    let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                    for ch in overlay.iter_mut() {
                                                        let nn = n.min(ch.len());
                                                        for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight(tab.fade_in_shape, t); ch[i] *= w; }
                                                    }
                                                    let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                        overlay.clone(),
                                                        ToolKind::Fade,
                                                        timeline_len,
                                                    ));
                                                    // Mono audition
                                                    let mut mono = Self::editor_mixdown_mono(tab);
                                                    let nn = n.min(mono.len());
                                                    for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight(tab.fade_in_shape, t); mono[i] *= w; }
                                                    pending_preview = Some((ToolKind::Fade, mono));
                                                    stop_playback = true;
                                                    tab.preview_audio_tool = Some(ToolKind::Fade);
                                                } else {
                                                    if tab.tool_state.fade_in_ms > 0.0
                                                        || tab.tool_state.fade_out_ms > 0.0
                                                    {
                                                        request_preview_refresh = true;
                                                    } else {
                                                        tab.preview_audio_tool = None;
                                                        tab.preview_overlay = None;
                                                    }
                                                }
                                            }
                                            if ui.add_enabled(secs>0.0, egui::Button::new("Apply")).clicked() {
                                                let n = ((secs) * sr).round() as usize;
                                                do_fade_in = Some(((0, n.min(tab.samples_len)), tab.fade_in_shape));
                                                tab.preview_audio_tool = None; // will be rebuilt from destructive result below
                                                tab.preview_overlay = None;
                                                tab.tool_state = ToolState { fade_in_ms: 0.0, ..tab.tool_state };
                                            }
                                        });
                                        ui.separator();
                                        // Fade Out
                                        ui.label("Fade Out");
                                        ui.horizontal_wrapped(|ui| {
                                            let mut secs = tab.tool_state.fade_out_ms / 1000.0;
                                            if !secs.is_finite() { secs = 0.0; }
                                            ui.label("duration (s)");
                                            let mut changed = ui
                                                .add(
                                                    egui::DragValue::new(&mut secs)
                                                        .range(0.0..=600.0)
                                                        .speed(0.05)
                                                        .fixed_decimals(2),
                                                )
                                                .changed();
                                            ui.label("shape");
                                            let mut shape = tab.fade_out_shape;
                                            egui::ComboBox::from_id_salt("fade_out_shape")
                                                .selected_text(shape_label(shape))
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Linear,
                                                        "Linear",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::EqualPower,
                                                        "Equal",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Cosine,
                                                        "Cosine",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::SCurve,
                                                        "S-Curve",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Quadratic,
                                                        "Quadratic",
                                                    );
                                                    ui.selectable_value(
                                                        &mut shape,
                                                        crate::app::types::FadeShape::Cubic,
                                                        "Cubic",
                                                    );
                                                });
                                            if shape != tab.fade_out_shape {
                                                tab.fade_out_shape = shape;
                                                changed = true;
                                            }
                                            if changed {
                                                tab.tool_state = ToolState{ fade_out_ms: (secs*1000.0).max(0.0), ..tab.tool_state };
                                                if preview_ok {
                                                    let n = ((secs) * sr).round() as usize;
                                                    // per-channel overlay
                                                    let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                    for ch in overlay.iter_mut() {
                                                        let len = ch.len(); let nn = n.min(len);
                                                        for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight_out(tab.fade_out_shape, t); let idx = len - nn + i; ch[idx] *= w; }
                                                    }
                                                    let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                        overlay.clone(),
                                                        ToolKind::Fade,
                                                        timeline_len,
                                                    ));
                                                    // mono audition
                                                    let mut mono = Self::editor_mixdown_mono(tab);
                                                    let len = mono.len(); let nn = n.min(len);
                                                    for i in 0..nn { let t = i as f32 / nn.max(1) as f32; let w = Self::fade_weight_out(tab.fade_out_shape, t); let idx = len - nn + i; mono[idx] *= w; }
                                                    pending_preview = Some((ToolKind::Fade, mono));
                                                    stop_playback = true;
                                                    tab.preview_audio_tool = Some(ToolKind::Fade);
                                                } else {
                                                    if tab.tool_state.fade_in_ms > 0.0
                                                        || tab.tool_state.fade_out_ms > 0.0
                                                    {
                                                        request_preview_refresh = true;
                                                    } else {
                                                        tab.preview_audio_tool = None;
                                                        tab.preview_overlay = None;
                                                    }
                                                }
                                            }
                                            if ui.add_enabled(secs>0.0, egui::Button::new("Apply")).clicked() {
                                                let n = ((secs) * sr).round() as usize;
                                                do_fade_out = Some(((0, n.min(tab.samples_len)), tab.fade_out_shape));
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                                tab.tool_state = ToolState { fade_out_ms: 0.0, ..tab.tool_state };
                                            }
                                        });
                                    });
                                }
                                ToolKind::PitchShift => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut(); s.spacing.item_spacing = egui::vec2(6.0,6.0); s.spacing.button_padding = egui::vec2(6.0,3.0);
                                        if !preview_ok {
                                            ui.label(RichText::new("Long clip: simplified waveform preview, full preview runs in background").weak());
                                        }
                                        let mut semi = tab.tool_state.pitch_semitones;
                                        if !semi.is_finite() { semi = 0.0; }
                                        ui.label("Semitones");
                                        let changed = ui.add(egui::DragValue::new(&mut semi).range(-12.0..=12.0).speed(0.1).fixed_decimals(2)).changed();
                                        if changed {
                                            tab.tool_state = ToolState{ pitch_semitones: semi, ..tab.tool_state };
                                            stop_playback = true;
                                            tab.preview_audio_tool = Some(ToolKind::PitchShift);
                                            if preview_ok || tab.dirty {
                                                let mono = Self::editor_mixdown_mono(tab);
                                                pending_heavy_preview = Some((ToolKind::PitchShift, mono, semi));
                                                // Defer overlay spawn to avoid nested &mut borrow
                                                pending_overlay_job = Some((ToolKind::PitchShift, semi));
                                            } else {
                                                let path = tab.path.clone();
                                                pending_heavy_preview_path = Some((ToolKind::PitchShift, path.clone(), semi));
                                                pending_overlay_path = Some((ToolKind::PitchShift, path, semi));
                                            }
                                        }
                                        if overlay_busy || apply_busy { ui.add(egui::Spinner::new()); }
                                        if ui.add_enabled(!apply_busy, egui::Button::new("Apply")).clicked() {
                                            pending_pitch_apply = Some(tab.tool_state.pitch_semitones);
                                            tab.tool_state = ToolState { pitch_semitones: 0.0, ..tab.tool_state };
                                            tab.preview_audio_tool = None;
                                            tab.preview_overlay = None;
                                        }
                                    });
                                }
                                ToolKind::TimeStretch => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut(); s.spacing.item_spacing = egui::vec2(6.0,6.0); s.spacing.button_padding = egui::vec2(6.0,3.0);
                                        if !preview_ok {
                                            ui.label(RichText::new("Long clip: simplified waveform preview, full preview runs in background").weak());
                                        }
                                        let mut rate = tab.tool_state.stretch_rate;
                                        if !rate.is_finite() { rate = 1.0; }
                                        ui.label("Rate");
                                        let changed = ui.add(egui::DragValue::new(&mut rate).range(0.25..=4.0).speed(0.02).fixed_decimals(2)).changed();
                                        if changed {
                                            tab.tool_state = ToolState{ stretch_rate: rate, ..tab.tool_state };
                                            stop_playback = true;
                                            tab.preview_audio_tool = Some(ToolKind::TimeStretch);
                                            if preview_ok || tab.dirty {
                                                let mono = Self::editor_mixdown_mono(tab);
                                                pending_heavy_preview = Some((ToolKind::TimeStretch, mono, rate));
                                                // Defer overlay spawn to avoid nested &mut borrow
                                                pending_overlay_job = Some((ToolKind::TimeStretch, rate));
                                            } else {
                                                let path = tab.path.clone();
                                                pending_heavy_preview_path = Some((ToolKind::TimeStretch, path.clone(), rate));
                                                pending_overlay_path = Some((ToolKind::TimeStretch, path, rate));
                                            }
                                        }
                                        if overlay_busy || apply_busy { ui.add(egui::Spinner::new()); }
                                        if ui.add_enabled(!apply_busy, egui::Button::new("Apply")).clicked() {
                                            pending_stretch_apply = Some(tab.tool_state.stretch_rate);
                                            tab.tool_state = ToolState { stretch_rate: 1.0, ..tab.tool_state };
                                            tab.preview_audio_tool = None;
                                            tab.preview_overlay = None;
                                        }
                                    });
                                }
                                ToolKind::Gain => {
                                    if let Some(reason) = preview_disabled_reason {
                                        ui.label(RichText::new(reason).weak());
                                    }
                                    if let Some(note) = simplified_preview_note {
                                        ui.label(RichText::new(note).weak());
                                    }
                                    let st = tab.tool_state;
                                    let mut gain_db = st.gain_db;
                                    if !gain_db.is_finite() { gain_db = 0.0; }
                                    ui.label("Gain (dB)"); ui.add(egui::DragValue::new(&mut gain_db).range(-24.0..=24.0).speed(0.1));
                                    tab.tool_state = ToolState{ gain_db, ..tab.tool_state };
                                    // live preview on change
                                    if (gain_db - st.gain_db).abs() > 1e-6 {
                                        if preview_ok {
                                            let g = db_to_amp(gain_db);
                                            // per-channel overlay
                                            let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                            for ch in overlay.iter_mut() { for v in ch.iter_mut() { *v *= g; } }
                                            let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                            tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                overlay,
                                                ToolKind::Gain,
                                                timeline_len,
                                            ));
                                            // mono audition
                                            let mut mono = Self::editor_mixdown_mono(tab);
                                            for v in &mut mono { *v *= g; }
                                            pending_preview = Some((ToolKind::Gain, mono));
                                            stop_playback = true;
                                            tab.preview_audio_tool = Some(ToolKind::Gain);
                                        } else {
                                            if gain_db.abs() > 1e-6 {
                                                request_preview_refresh = true;
                                            } else {
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                            }
                                        }
                                    }
                                    if ui.button("Apply").clicked() {
                                        do_gain = Some(((0, tab.samples_len), gain_db));
                                        tab.preview_audio_tool = None;
                                        tab.preview_overlay = None;
                                        tab.tool_state = ToolState { gain_db: 0.0, ..tab.tool_state };
                                    }
                                }
                                ToolKind::Normalize => {
                                    if let Some(reason) = preview_disabled_reason {
                                        ui.label(RichText::new(reason).weak());
                                    }
                                    if let Some(note) = simplified_preview_note {
                                        ui.label(RichText::new(note).weak());
                                    }
                                    let st = tab.tool_state;
                                    let mut target_db = st.normalize_target_db;
                                    if !target_db.is_finite() { target_db = -6.0; }
                                    ui.label("Target dBFS"); ui.add(egui::DragValue::new(&mut target_db).range(-24.0..=0.0).speed(0.1));
                                    tab.tool_state = ToolState{ normalize_target_db: target_db, ..tab.tool_state };
                                    let mut preview_normalize = |target_db: f32, tab: &mut EditorTab| {
                                        let mut mono = Self::editor_mixdown_mono(tab);
                                        if !mono.is_empty() {
                                            let mut peak = 0.0f32;
                                            for &v in &mono { peak = peak.max(v.abs()); }
                                            if peak > 0.0 {
                                                let g = db_to_amp(target_db) / peak.max(1e-12);
                                                // per-channel overlay
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                for ch in overlay.iter_mut() { for v in ch.iter_mut() { *v *= g; } }
                                                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                    overlay,
                                                    ToolKind::Normalize,
                                                    timeline_len,
                                                ));
                                                // mono audition
                                                for v in &mut mono { *v *= g; }
                                                pending_preview = Some((ToolKind::Normalize, mono));
                                                stop_playback = true;
                                                tab.preview_audio_tool = Some(ToolKind::Normalize);
                                            }
                                        }
                                    };
                                    if preview_button_enabled {
                                        let changed = (target_db - st.normalize_target_db).abs() > 1e-6;
                                        if changed {
                                            if preview_ok {
                                                preview_normalize(target_db, tab);
                                            } else if (target_db + 6.0).abs() > 1e-6 {
                                                request_preview_refresh = true;
                                            } else {
                                                tab.preview_audio_tool = None;
                                                tab.preview_overlay = None;
                                            }
                                        }
                                    } else {
                                        tab.preview_audio_tool = None;
                                        tab.preview_overlay = None;
                                    }
                                    if ui
                                        .add_enabled(preview_button_enabled, egui::Button::new("Preview"))
                                        .clicked()
                                    {
                                        if preview_ok {
                                            preview_normalize(target_db, tab);
                                        } else if (target_db + 6.0).abs() > 1e-6 {
                                            request_preview_refresh = true;
                                        } else {
                                            tab.preview_audio_tool = None;
                                            tab.preview_overlay = None;
                                        }
                                    }
                                    if ui.button("Apply").clicked() {
                                        do_normalize = Some(((0, tab.samples_len), target_db));
                                        tab.preview_audio_tool = None;
                                        tab.preview_overlay = None;
                                        tab.tool_state =
                                            ToolState { normalize_target_db: -6.0, ..tab.tool_state };
                                    }
                                }
                                ToolKind::Loudness => {
                                    if let Some(reason) = preview_disabled_reason {
                                        ui.label(RichText::new(reason).weak());
                                    }
                                    if let Some(note) = simplified_preview_note {
                                        ui.label(RichText::new(note).weak());
                                    }
                                    let st = tab.tool_state;
                                    let mut target_lufs = st.loudness_target_lufs;
                                    if !target_lufs.is_finite() { target_lufs = -14.0; }
                                    ui.label("Target LUFS (I)");
                                    ui.add(
                                        egui::DragValue::new(&mut target_lufs)
                                            .range(-36.0..=0.0)
                                            .speed(0.1),
                                    );
                                    tab.tool_state = ToolState {
                                        loudness_target_lufs: target_lufs,
                                        ..tab.tool_state
                                    };
                                    if ui
                                        .add_enabled(preview_button_enabled, egui::Button::new("Preview"))
                                        .clicked()
                                    {
                                        if preview_ok {
                                            if let Ok(lufs) = crate::wave::lufs_integrated_from_multi(
                                                &tab.ch_samples,
                                                self.audio.shared.out_sample_rate,
                                            ) {
                                                if lufs.is_finite() {
                                                    let gain_db = target_lufs - lufs;
                                                    let gain = db_to_amp(gain_db);
                                                    let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                    for ch in overlay.iter_mut() {
                                                        for v in ch.iter_mut() {
                                                            *v = (*v * gain).clamp(-1.0, 1.0);
                                                        }
                                                    }
                                                    let timeline_len = overlay
                                                        .get(0)
                                                        .map(|c| c.len())
                                                        .unwrap_or(tab.samples_len);
                                                    tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                        overlay,
                                                        ToolKind::Loudness,
                                                        timeline_len,
                                                    ));
                                                    let mut mono = Self::editor_mixdown_mono(tab);
                                                    for v in &mut mono {
                                                        *v = (*v * gain).clamp(-1.0, 1.0);
                                                    }
                                                    pending_preview = Some((ToolKind::Loudness, mono));
                                                    stop_playback = true;
                                                    tab.preview_audio_tool = Some(ToolKind::Loudness);
                                                }
                                            }
                                        } else if (target_lufs + 14.0).abs() > 1e-6 {
                                            request_preview_refresh = true;
                                        } else {
                                            tab.preview_audio_tool = None;
                                            tab.preview_overlay = None;
                                        }
                                    }
                                    if ui.button("Apply").clicked() {
                                        pending_loudness_apply = Some(target_lufs);
                                        tab.preview_audio_tool = None;
                                        tab.preview_overlay = None;
                                        tab.tool_state = ToolState {
                                            loudness_target_lufs: -14.0,
                                            ..tab.tool_state
                                        };
                                    }
                                }
                                ToolKind::MusicAnalyze => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut();
                                        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                        s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                        if music_model_ready {
                                            ui.label(
                                                RichText::new("Analyze model: ready")
                                                    .color(egui::Color32::from_rgb(120, 220, 140)),
                                            );
                                        } else if music_model_downloading {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.add(egui::Spinner::new());
                                                ui.label(
                                                    RichText::new("Analyze model: downloading...")
                                                        .weak(),
                                                );
                                            });
                                        } else {
                                            ui.label(
                                                RichText::new("Analyze model: not installed")
                                                    .weak(),
                                            );
                                        }
                                        if music_model_ready {
                                            if music_demucs_ready {
                                                ui.label(
                                                    RichText::new("Auto Demucs: ready").color(
                                                        egui::Color32::from_rgb(120, 220, 140),
                                                    ),
                                                );
                                            } else {
                                                ui.label(
                                                    RichText::new("Auto Demucs: missing").color(
                                                        egui::Color32::from_rgb(220, 170, 120),
                                                    ),
                                                );
                                            }
                                        }
                                        if let Some(model_dir) = music_model_dir_text.as_ref() {
                                            ui.label(
                                                RichText::new(format!("Model dir: {model_dir}"))
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                        ui.horizontal_wrapped(|ui| {
                                            if !music_model_ready {
                                                if ui
                                                    .add_enabled(
                                                        !music_model_downloading,
                                                        egui::Button::new("Download Model..."),
                                                    )
                                                    .clicked()
                                                {
                                                    pending_music_model_download = true;
                                                }
                                            } else {
                                                if !music_demucs_ready
                                                    && ui
                                                        .add_enabled(
                                                            music_can_uninstall,
                                                            egui::Button::new("Repair Model Files..."),
                                                        )
                                                        .clicked()
                                                {
                                                    pending_music_model_download = true;
                                                }
                                                if ui
                                                    .add_enabled(
                                                        music_can_uninstall,
                                                        egui::Button::new("Uninstall Model..."),
                                                    )
                                                    .clicked()
                                                {
                                                    pending_music_model_uninstall = true;
                                                }
                                            }
                                        });
                                        if let Some(err) = tab.music_analysis_draft.last_error.as_ref() {
                                            ui.label(
                                                RichText::new(err)
                                                    .color(egui::Color32::LIGHT_RED),
                                            );
                                        } else if let Some(err) = self.music_ai_last_error.as_ref() {
                                            ui.label(
                                                RichText::new(err)
                                                    .color(egui::Color32::LIGHT_RED),
                                            );
                                        }
                                        ui.separator();

                                        let stems = music_onnx::resolve_stem_paths(
                                            tab.path.as_path(),
                                            tab.music_analysis_draft.stems_dir_override.as_deref(),
                                        );
                                        let searched_dirs = stems
                                            .searched_roots
                                            .iter()
                                            .map(|path| path.display().to_string())
                                            .collect::<Vec<_>>()
                                            .join(" | ");
                                        if stems.is_ready() {
                                            ui.label(
                                                RichText::new("Input: stems ready")
                                                    .color(egui::Color32::from_rgb(120, 220, 140)),
                                            );
                                            ui.label(
                                                RichText::new(format!(
                                                    "Stem dir: {}",
                                                    stems.root_dir.display()
                                                ))
                                                .small()
                                                .weak(),
                                            );
                                        } else if music_demucs_ready {
                                            ui.label(
                                                RichText::new("Input: source audio (auto Demucs)")
                                                .color(egui::Color32::from_rgb(120, 200, 220)),
                                            );
                                        } else {
                                            ui.label(
                                                RichText::new(
                                                    "Input unavailable: stems not found and auto-Demucs is unavailable",
                                                )
                                                .color(egui::Color32::from_rgb(220, 170, 120)),
                                            );
                                            ui.label(
                                                RichText::new(format!(
                                                    "Missing: {}",
                                                    stems.missing.join(", ")
                                                ))
                                                .small()
                                                .weak(),
                                            );
                                        }
                                        ui.label(
                                            RichText::new(format!("Searched: {searched_dirs}"))
                                                .small()
                                                .weak(),
                                        );
                                        if music_onnx::source_audio_has_timing_risk(
                                            tab.path.as_path(),
                                        ) {
                                            ui.label(
                                                RichText::new(
                                                    "Timing note: compressed input can shift beat markers. WAV is recommended.",
                                                )
                                                .small()
                                                .color(egui::Color32::from_rgb(220, 180, 110)),
                                            );
                                        }
                                        let can_analyze = music_model_ready
                                            && (stems.is_ready() || music_demucs_ready)
                                            && !tab.music_analysis_draft.analysis_inflight
                                            && !music_analyze_running;
                                        ui.horizontal_wrapped(|ui| {
                                            if ui
                                                .add_enabled(can_analyze, egui::Button::new("Analyze"))
                                                .clicked()
                                            {
                                                pending_music_analyze_start = true;
                                            }
                                            if ui
                                                .add_enabled(
                                                    tab.music_analysis_draft.analysis_inflight
                                                        || music_analyze_running,
                                                    egui::Button::new("Cancel"),
                                                )
                                                .clicked()
                                            {
                                                pending_music_analyze_cancel = true;
                                            }
                                            if let Some(status) = music_run_status.as_ref() {
                                                ui.label(RichText::new(status).weak());
                                            }
                                        });
                                        if let Some(process) = music_run_process.as_ref() {
                                            ui.horizontal_wrapped(|ui| {
                                                if music_analyze_running {
                                                    ui.add(egui::Spinner::new());
                                                }
                                                ui.label(
                                                    RichText::new(format!("Process: {process}"))
                                                        .small()
                                                        .weak(),
                                                );
                                            });
                                        } else if !tab.music_analysis_draft.analysis_process_message.is_empty() {
                                            ui.label(
                                                RichText::new(format!(
                                                    "Process: {}",
                                                    tab.music_analysis_draft.analysis_process_message
                                                ))
                                                .small()
                                                .weak(),
                                            );
                                        }

                                        if tab.music_analysis_draft.result.is_some() {
                                            let (beat_count, downbeat_count, section_count, estimated_bpm) = tab
                                                .music_analysis_draft
                                                .result
                                                .as_ref()
                                                .map(|r| {
                                                    (
                                                        r.beats.len(),
                                                        r.downbeats.len(),
                                                        r.sections.len(),
                                                        r.estimated_bpm,
                                                    )
                                                })
                                                .unwrap_or((0, 0, 0, None));
                                            let source_text = match tab.music_analysis_draft.analysis_source_kind {
                                                MusicAnalysisSourceKind::StemsDir => "stems",
                                                MusicAnalysisSourceKind::AutoDemucs => "auto-demucs",
                                            };
                                            ui.separator();
                                            ui.label(
                                                RichText::new(format!(
                                                    "Result: beats={beat_count}, downbeats={downbeat_count}, sections={section_count}{}",
                                                    estimated_bpm
                                                        .map(|v| format!(", bpm={v:.2}"))
                                                        .unwrap_or_default()
                                                ))
                                                .small()
                                                .weak(),
                                            );
                                            ui.label(
                                                RichText::new(format!("Source: {source_text}"))
                                                    .small()
                                                    .weak(),
                                            );
                                            ui.label(RichText::new("Markers").strong());
                                            if ui
                                                .checkbox(&mut tab.music_analysis_draft.show_beat, "Beat")
                                                .changed()
                                            {
                                                pending_music_rebuild_markers = true;
                                            }
                                            if ui
                                                .checkbox(
                                                    &mut tab.music_analysis_draft.show_downbeat,
                                                    "DownBeat",
                                                )
                                                .changed()
                                            {
                                                pending_music_rebuild_markers = true;
                                            }
                                            if ui
                                                .checkbox(
                                                    &mut tab.music_analysis_draft.show_section,
                                                    "Section",
                                                )
                                                .changed()
                                            {
                                                pending_music_rebuild_markers = true;
                                            }
                                            ui.label(
                                                RichText::new(format!(
                                                    "Provisional markers: {}",
                                                    tab.music_analysis_draft.provisional_markers.len()
                                                ))
                                                .small()
                                                .weak(),
                                            );
                                            ui.horizontal_wrapped(|ui| {
                                                if ui
                                                    .add_enabled(
                                                        !apply_busy
                                                            && tab.music_analysis_draft.result.is_some(),
                                                        egui::Button::new("Apply Markers"),
                                                    )
                                                    .clicked()
                                                {
                                                    if pending_edit_undo.is_none() {
                                                        pending_edit_undo =
                                                            Some(Self::capture_undo_state(tab));
                                                    }
                                                    pending_music_apply_markers = true;
                                                }
                                            });

                                            ui.separator();
                                            ui.label(RichText::new("Sonify").strong());
                                            let mut sonify_changed = false;
                                            sonify_changed |= ui
                                                .checkbox(
                                                    &mut tab.music_analysis_draft.preview_click_beat,
                                                    "Beat Click",
                                                )
                                                .changed();
                                            sonify_changed |= ui
                                                .checkbox(
                                                    &mut tab.music_analysis_draft.preview_click_downbeat,
                                                    "DownBeat Accent",
                                                )
                                                .changed();
                                            sonify_changed |= ui
                                                .checkbox(
                                                    &mut tab.music_analysis_draft.preview_cue_section,
                                                    "Section Cue",
                                                )
                                                .changed();
                                            if sonify_changed {
                                                tab.music_analysis_draft.preview_active = false;
                                                pending_music_preview_refresh = true;
                                                stop_playback = true;
                                            }

                                            ui.separator();
                                            ui.label(RichText::new("Stem Preview (dB)").strong());
                                            ui.label(
                                                RichText::new(format!(
                                                    "Peak: {:.4}{}",
                                                    tab.music_analysis_draft.preview_peak_abs,
                                                    if tab.music_analysis_draft.preview_clip_applied {
                                                        " (clip applied)"
                                                    } else {
                                                        ""
                                                    }
                                                ))
                                                .small()
                                                .weak(),
                                            );
                                            if tab.music_analysis_draft.preview_inflight {
                                                ui.horizontal_wrapped(|ui| {
                                                    ui.add(egui::Spinner::new());
                                                    ui.label(
                                                        RichText::new("Preview: updating...")
                                                            .small()
                                                            .weak(),
                                                    );
                                                });
                                            }
                                            if let Some(err) =
                                                tab.music_analysis_draft.preview_error.as_ref()
                                            {
                                                ui.label(
                                                    RichText::new(err)
                                                        .small()
                                                        .color(egui::Color32::LIGHT_RED),
                                                );
                                            }
                                            let mut slider_changed = false;
                                            slider_changed |= ui
                                                .add(
                                                    egui::Slider::new(
                                                        &mut tab.music_analysis_draft.preview_gains_db.bass,
                                                        crate::app::helpers::GAIN_DB_MIN
                                                            ..=crate::app::helpers::GAIN_DB_MAX,
                                                    )
                                                    .text("bass"),
                                                )
                                                .changed();
                                            slider_changed |= ui
                                                .add(
                                                    egui::Slider::new(
                                                        &mut tab.music_analysis_draft.preview_gains_db.drums,
                                                        crate::app::helpers::GAIN_DB_MIN
                                                            ..=crate::app::helpers::GAIN_DB_MAX,
                                                    )
                                                    .text("drums"),
                                                )
                                                .changed();
                                            slider_changed |= ui
                                                .add(
                                                    egui::Slider::new(
                                                        &mut tab.music_analysis_draft.preview_gains_db.other,
                                                        crate::app::helpers::GAIN_DB_MIN
                                                            ..=crate::app::helpers::GAIN_DB_MAX,
                                                    )
                                                    .text("other"),
                                                )
                                                .changed();
                                            slider_changed |= ui
                                                .add(
                                                    egui::Slider::new(
                                                        &mut tab.music_analysis_draft.preview_gains_db.vocals,
                                                        crate::app::helpers::GAIN_DB_MIN
                                                            ..=crate::app::helpers::GAIN_DB_MAX,
                                                    )
                                                    .text("vocals"),
                                                )
                                                .changed();
                                            tab.music_analysis_draft.preview_gains_db.bass = tab
                                                .music_analysis_draft
                                                .preview_gains_db
                                                .bass
                                                .clamp(
                                                    crate::app::helpers::GAIN_DB_MIN,
                                                    crate::app::helpers::GAIN_DB_MAX,
                                                );
                                            tab.music_analysis_draft.preview_gains_db.drums = tab
                                                .music_analysis_draft
                                                .preview_gains_db
                                                .drums
                                                .clamp(
                                                    crate::app::helpers::GAIN_DB_MIN,
                                                    crate::app::helpers::GAIN_DB_MAX,
                                                );
                                            tab.music_analysis_draft.preview_gains_db.other = tab
                                                .music_analysis_draft
                                                .preview_gains_db
                                                .other
                                                .clamp(
                                                    crate::app::helpers::GAIN_DB_MIN,
                                                    crate::app::helpers::GAIN_DB_MAX,
                                                );
                                            tab.music_analysis_draft.preview_gains_db.vocals = tab
                                                .music_analysis_draft
                                                .preview_gains_db
                                                .vocals
                                                .clamp(
                                                    crate::app::helpers::GAIN_DB_MIN,
                                                    crate::app::helpers::GAIN_DB_MAX,
                                                );
                                            if slider_changed {
                                                tab.music_analysis_draft.preview_active = false;
                                                pending_music_preview_refresh = true;
                                                stop_playback = true;
                                            }
                                            if ui
                                                .checkbox(
                                                    &mut tab.music_analysis_draft.preview_selection_only,
                                                    "Selection only",
                                                )
                                                .changed()
                                            {
                                                tab.music_analysis_draft.preview_active = false;
                                                pending_music_preview_refresh = true;
                                                stop_playback = true;
                                            }
                                            ui.horizontal_wrapped(|ui| {
                                                if ui
                                                    .add_enabled(
                                                        !apply_busy
                                                            && !tab.music_analysis_draft.preview_inflight
                                                            && tab.music_analysis_draft.preview_active,
                                                        egui::Button::new("Apply"),
                                                    )
                                                    .clicked()
                                                {
                                                    pending_music_apply_preview = true;
                                                }
                                            });
                                            ui.label(
                                                RichText::new("Preview updates live (async).")
                                                .small()
                                                .weak(),
                                            );
                                            ui.label(
                                                RichText::new(
                                                    "Apply writes the current stem mix and enabled cue sounds.",
                                                )
                                                .small()
                                                .weak(),
                                            );
                                        } else {
                                            if music_analyze_running {
                                                ui.label(
                                                    RichText::new(
                                                        "Analyze is running. Apply becomes available after completion.",
                                                    )
                                                    .weak(),
                                                );
                                            } else {
                                                ui.label(
                                                    RichText::new(
                                                        "Run Analyze to enable marker toggles and stem preview sliders.",
                                                    )
                                                    .weak(),
                                                );
                                            }
                                        }
                                    });
                                }
                                ToolKind::PluginFx => {
                                    ui.scope(|ui| {
                                        let s = ui.style_mut();
                                        s.spacing.item_spacing = egui::vec2(6.0, 6.0);
                                        s.spacing.button_padding = egui::vec2(6.0, 3.0);
                                        if plugin_scan_busy {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.add(egui::Spinner::new());
                                                ui.label(RichText::new("Scanning plugins...").weak());
                                            });
                                        } else if let Some(err) = plugin_scan_error.as_ref() {
                                            ui.label(
                                                RichText::new(format!("Plugin scan failed: {err}"))
                                                    .color(egui::Color32::LIGHT_RED),
                                            );
                                        }
                                        let draft = &mut tab.plugin_fx_draft;
                                        let mut selected_changed = false;
                                        let selected_text = draft
                                            .plugin_key
                                            .as_deref()
                                            .and_then(|key| {
                                                plugin_catalog
                                                    .iter()
                                                    .find(|entry| entry.key == key)
                                                    .map(|entry| {
                                                        format!(
                                                            "{:?}: {}",
                                                            entry.format,
                                                            Self::plugin_path_label(&entry.path)
                                                        )
                                                    })
                                            })
                                            .or_else(|| {
                                                if draft.plugin_name.is_empty() {
                                                    None
                                                } else {
                                                    Some(draft.plugin_name.clone())
                                                }
                                            })
                                            .unwrap_or_else(|| "(Select plugin)".to_string());
                                        egui::ComboBox::from_id_salt("plugin_fx_select")
                                            .selected_text(selected_text)
                                            .show_ui(ui, |ui| {
                                                for entry in plugin_catalog.iter() {
                                                    let selected = draft
                                                        .plugin_key
                                                        .as_deref()
                                                        .map(|v| v == entry.key)
                                                        .unwrap_or(false);
                                                    let label = format!(
                                                        "{:?}: {}",
                                                        entry.format,
                                                        Self::plugin_path_label(&entry.path)
                                                    );
                                                    if ui.selectable_label(selected, label).clicked()
                                                    {
                                                        draft.plugin_key = Some(entry.key.clone());
                                                        draft.plugin_name = entry.name.clone();
                                                        draft.params.clear();
                                                        draft.last_error = None;
                                                        draft.last_backend_log = None;
                                                        pending_plugin_probe =
                                                            Some(entry.key.clone());
                                                        selected_changed = true;
                                                    }
                                                }
                                            });
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("Rescan").clicked() {
                                                pending_plugin_scan = true;
                                            }
                                            let gui_live = draft.gui_status
                                                == crate::plugin::GuiSessionStatus::Live;
                                            let can_reload = draft.plugin_key.is_some()
                                                && !plugin_probe_busy
                                                && !plugin_scan_busy;
                                            if ui
                                                .add_enabled(
                                                    can_reload,
                                                    egui::Button::new(if gui_live {
                                                        "Sync Now"
                                                    } else {
                                                        "Reload Params"
                                                    }),
                                                )
                                                .clicked()
                                            {
                                                if gui_live {
                                                    pending_plugin_gui_sync = true;
                                                } else {
                                                    pending_plugin_probe = draft.plugin_key.clone();
                                                    draft.last_backend_log = None;
                                                }
                                            }
                                            let can_open_gui = can_reload
                                                && draft.gui_capabilities.supports_native_gui;
                                            if ui
                                                .add_enabled(can_open_gui, egui::Button::new("Open Native GUI"))
                                                .clicked()
                                            {
                                                pending_plugin_gui_open = true;
                                            }
                                            if ui
                                                .add_enabled(gui_live, egui::Button::new("Close GUI"))
                                                .clicked()
                                            {
                                                pending_plugin_gui_close = true;
                                            }
                                        });
                                        ui.collapsing("Search Paths", |ui| {
                                            ui.horizontal_wrapped(|ui| {
                                                if ui.button("Add Folder...").clicked() {
                                                    pending_plugin_pick_folder = true;
                                                }
                                                if ui.button("Reset Defaults").clicked() {
                                                    pending_plugin_reset_paths = true;
                                                }
                                                if ui.button("Rescan Paths").clicked() {
                                                    pending_plugin_scan = true;
                                                }
                                            });
                                            ui.horizontal_wrapped(|ui| {
                                                let edit = ui.text_edit_singleline(&mut plugin_search_path_input);
                                                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                                {
                                                    let raw = plugin_search_path_input.trim();
                                                    if !raw.is_empty() {
                                                        pending_plugin_add_path = Some(PathBuf::from(raw));
                                                        plugin_search_path_input.clear();
                                                    }
                                                }
                                                if ui.button("Add Path").clicked() {
                                                    let raw = plugin_search_path_input.trim();
                                                    if !raw.is_empty() {
                                                        pending_plugin_add_path = Some(PathBuf::from(raw));
                                                        plugin_search_path_input.clear();
                                                    }
                                                }
                                            });
                                            egui::ScrollArea::vertical()
                                                .id_salt("plugin_search_paths_scroll")
                                                .max_height(120.0)
                                                .show(ui, |ui| {
                                                    if plugin_search_paths.is_empty() {
                                                        ui.label(RichText::new("(No search paths)").weak());
                                                    } else {
                                                        for (idx, path) in plugin_search_paths.iter().enumerate() {
                                                            ui.horizontal_wrapped(|ui| {
                                                                ui.label(
                                                                    RichText::new(path.display().to_string())
                                                                        .small()
                                                                        .monospace(),
                                                                );
                                                                if ui.small_button("Remove").clicked() {
                                                                    pending_plugin_remove_index = Some(idx);
                                                                }
                                                            });
                                                        }
                                                    }
                                                });
                                        });
                                        if selected_changed {
                                            stop_playback = true;
                                            need_restore_preview = true;
                                            pending_plugin_gui_close = true;
                                        }
                                        ui.horizontal_wrapped(|ui| {
                                            ui.checkbox(&mut draft.enabled, "Enable");
                                            ui.checkbox(&mut draft.bypass, "Bypass");
                                            if let Some(backend) = draft.backend {
                                                ui.label(
                                                    RichText::new(format!("Backend: {:?}", backend))
                                                        .small()
                                                        .weak(),
                                                );
                                            }
                                            ui.label(
                                                RichText::new(format!("GUI: {:?}", draft.gui_status))
                                                    .small()
                                                    .weak(),
                                            );
                                        });
                                        if !draft.gui_capabilities.supports_native_gui {
                                            ui.label(
                                                RichText::new("Native GUI unsupported for current plugin/backend")
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label("Param Filter");
                                            ui.text_edit_singleline(&mut draft.filter);
                                        });
                                        if plugin_probe_busy {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.add(egui::Spinner::new());
                                                ui.label(RichText::new("Loading params...").weak());
                                            });
                                        }
                                        let filter = draft.filter.trim().to_ascii_lowercase();
                                        egui::ScrollArea::vertical()
                                            .id_salt("plugin_param_scroll")
                                            .max_height(320.0)
                                            .show(ui, |ui| {
                                                if draft.params.is_empty() {
                                                    ui.label(RichText::new("No parameters").weak());
                                                } else {
                                                    for param in draft.params.iter_mut() {
                                                        if !filter.is_empty()
                                                            && !param
                                                                .name
                                                                .to_ascii_lowercase()
                                                                .contains(&filter)
                                                            && !param
                                                                .id
                                                                .to_ascii_lowercase()
                                                                .contains(&filter)
                                                        {
                                                            continue;
                                                        }
                                                        ui.horizontal(|ui| {
                                                            ui.label(
                                                                RichText::new(param.name.as_str())
                                                                    .monospace(),
                                                            );
                                                            let mut norm = param.normalized;
                                                            if ui
                                                                .add(
                                                                    egui::Slider::new(
                                                                        &mut norm,
                                                                        0.0..=1.0,
                                                                    )
                                                                    .show_value(false),
                                                                )
                                                                .changed()
                                                            {
                                                                param.normalized =
                                                                    norm.clamp(0.0, 1.0);
                                                            }
                                                            let actual = param.min
                                                                + (param.max - param.min)
                                                                    * param.normalized;
                                                            let val = if param.unit.is_empty() {
                                                                format!(
                                                                    "{actual:.3} (n={:.3})",
                                                                    param.normalized
                                                                )
                                                            } else {
                                                                format!(
                                                                    "{actual:.3}{} (n={:.3})",
                                                                    param.unit,
                                                                    param.normalized
                                                                )
                                                            };
                                                            ui.label(RichText::new(val).small());
                                                            if ui.small_button("Reset").clicked() {
                                                                param.normalized = param
                                                                    .default_normalized
                                                                    .clamp(0.0, 1.0);
                                                            }
                                                        });
                                                    }
                                                }
                                            });
                                        if let Some(err) = draft.last_error.as_ref() {
                                            ui.label(RichText::new(err).color(Color32::LIGHT_RED));
                                        }
                                        if let Some(log) = draft.last_backend_log.as_ref() {
                                            ui.label(
                                                RichText::new(log.as_str())
                                                    .small()
                                                    .monospace()
                                                    .weak(),
                                            );
                                        }
                                        if plugin_preview_busy || plugin_apply_busy {
                                            ui.horizontal_wrapped(|ui| {
                                                ui.add(egui::Spinner::new());
                                                if plugin_apply_busy {
                                                    ui.label(RichText::new("Applying Plugin FX...").weak());
                                                } else {
                                                    ui.label(RichText::new("Previewing Plugin FX...").weak());
                                                }
                                            });
                                        }
                                        let can_run = draft.plugin_key.is_some()
                                            && !plugin_scan_busy
                                            && !plugin_probe_busy
                                            && !plugin_preview_busy
                                            && !plugin_apply_busy
                                            && !apply_busy;
                                        ui.horizontal_wrapped(|ui| {
                                            if ui
                                                .add_enabled(can_run, egui::Button::new("Preview"))
                                                .clicked()
                                            {
                                                pending_plugin_preview = true;
                                                stop_playback = true;
                                            }
                                            if ui
                                                .add_enabled(can_run, egui::Button::new("Apply"))
                                                .clicked()
                                            {
                                                pending_plugin_apply = true;
                                                stop_playback = true;
                                            }
                                            if ui.button("Cancel").clicked() {
                                                need_restore_preview = true;
                                            }
                                        });
                                    });
                                }
                                ToolKind::Reverse => {
                                    if let Some(reason) = preview_disabled_reason {
                                        ui.label(RichText::new(reason).weak());
                                    }
                                    if let Some(note) = simplified_preview_note {
                                        ui.label(RichText::new(note).weak());
                                    }
                                    ui.horizontal_wrapped(|ui| {
                                        if ui
                                            .add_enabled(preview_button_enabled, egui::Button::new("Preview"))
                                            .clicked()
                                        {
                                            if preview_ok {
                                                let mut overlay: Vec<Vec<f32>> = tab.ch_samples.clone();
                                                for ch in overlay.iter_mut() { ch.reverse(); }
                                                let timeline_len = overlay.get(0).map(|c| c.len()).unwrap_or(tab.samples_len);
                                                tab.preview_overlay = Some(Self::preview_overlay_from_channels(
                                                    overlay,
                                                    ToolKind::Reverse,
                                                    timeline_len,
                                                ));
                                                let mut mono = Self::editor_mixdown_mono(tab);
                                                mono.reverse();
                                                pending_preview = Some((ToolKind::Reverse, mono));
                                                stop_playback = true;
                                                tab.preview_audio_tool = Some(ToolKind::Reverse);
                                            } else {
                                                request_preview_refresh = true;
                                            }
                                        }
                                        if ui.button("Apply").clicked() { do_reverse = Some((0, tab.samples_len)); tab.preview_audio_tool=None; tab.preview_overlay=None; }
                                        if ui.button("Cancel").clicked() { need_restore_preview = true; }
                                    });
                                }
                            }
                        }
                        ViewMode::Spectrogram | ViewMode::Log | ViewMode::Mel => {
                            ui.label(RichText::new("Display").strong());
                            ui.label(
                                RichText::new("Values: dB (log magnitude)").monospace().weak(),
                            );
                            let overlay_resp =
                                ui.checkbox(&mut tab.show_waveform_overlay, "Waveform overlay");
                            if overlay_resp.changed() {
                                if tab.show_waveform_overlay {
                                    if Self::tool_supports_preview(tab.active_tool) {
                                        request_preview_refresh = true;
                                    }
                                } else {
                                    tab.preview_overlay = None;
                                }
                            }
                        }
                        ViewMode::Tempogram => {
                            ui.label(RichText::new("Tempogram").strong());
                            ui.checkbox(&mut tab.show_waveform_overlay, "Waveform overlay");
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("Analyze BPM").clicked() {
                                    pending_tempogram_refresh = true;
                                }
                                if analysis_loading && ui.button("Cancel").clicked() {
                                    cancel_feature_analysis = true;
                                }
                                if let Some(data) = tempogram_data.as_ref() {
                                    if let Some(bpm) = data.estimated_bpm {
                                        if ui.button("Apply BPM").clicked() {
                                            apply_estimated_bpm = Some(bpm);
                                        }
                                    }
                                }
                            });
                            if let Some(data) = tempogram_data.as_ref() {
                                let bpm_text = data
                                    .estimated_bpm
                                    .map(|bpm| format!("{bpm:.2}"))
                                    .unwrap_or_else(|| "-".to_string());
                                ui.label(
                                    RichText::new(format!("Estimated BPM: {bpm_text}")).monospace(),
                                );
                                ui.label(
                                    RichText::new(format!("Confidence: {:.2}", data.confidence))
                                        .monospace(),
                                );
                            } else if analysis_loading {
                                ui.label(RichText::new("Analyzing mono mixdown...").weak());
                            } else {
                                ui.label(RichText::new("Tempogram not ready").weak());
                            }
                        }
                        ViewMode::Chromagram => {
                            ui.label(RichText::new("Chromagram").strong());
                            ui.checkbox(&mut tab.show_waveform_overlay, "Waveform overlay");
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("Analyze Key").clicked() {
                                    pending_chromagram_refresh = true;
                                }
                                if analysis_loading && ui.button("Cancel").clicked() {
                                    cancel_feature_analysis = true;
                                }
                            });
                            if let Some(data) = chromagram_data.as_ref() {
                                let key = data
                                    .estimated_key
                                    .clone()
                                    .unwrap_or_else(|| "-".to_string());
                                let mode = data
                                    .estimated_mode
                                    .clone()
                                    .unwrap_or_else(|| "-".to_string());
                                ui.label(RichText::new(format!("Key: {key}")).monospace());
                                ui.label(RichText::new(format!("Mode: {mode}")).monospace());
                                ui.label(
                                    RichText::new(format!("Confidence: {:.2}", data.confidence))
                                        .monospace(),
                                );
                            } else if analysis_loading {
                                ui.label(RichText::new("Analyzing mono mixdown...").weak());
                            } else {
                                ui.label(RichText::new("Chromagram not ready").weak());
                            }
                        }
                    }
                });
                },
                ); // end inspector
                if need_restore_preview {
                    pending_preview = None;
                    pending_heavy_preview = None;
                    pending_heavy_preview_path = None;
                    pending_overlay_job = None;
                    pending_overlay_path = None;
                }
                if let Some((tool, mono, p)) = pending_heavy_preview {
                    if self.is_decode_failed_path(&self.tabs[tab_idx].path) {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.preview_audio_tool = None;
                            tab.preview_overlay = None;
                        }
                    } else {
                        self.spawn_heavy_preview_owned(mono, tool, p);
                    }
                }
                if let Some((tool, path, p)) = pending_heavy_preview_path {
                    if self.is_decode_failed_path(&path) {
                        if let Some(tab) = self.tabs.get_mut(tab_idx) {
                            tab.preview_audio_tool = None;
                            tab.preview_overlay = None;
                        }
                    } else {
                        self.spawn_heavy_preview_from_path(path, tool, p);
                    }
                }
                if let Some(semi) = pending_pitch_apply {
                    self.spawn_editor_apply_for_tab(tab_idx, ToolKind::PitchShift, semi);
                }
                if let Some(rate) = pending_stretch_apply {
                    self.spawn_editor_apply_for_tab(tab_idx, ToolKind::TimeStretch, rate);
                }
                if let Some(target) = pending_loudness_apply {
                    self.spawn_editor_apply_for_tab(tab_idx, ToolKind::Loudness, target);
                }
                if pending_plugin_scan {
                    self.spawn_plugin_scan();
                }
                if pending_plugin_pick_folder {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        pending_plugin_add_path = Some(folder);
                    }
                }
                if pending_plugin_reset_paths {
                    self.reset_plugin_search_paths_to_default();
                    self.save_prefs();
                    self.plugin_catalog.clear();
                    self.spawn_plugin_scan();
                }
                if let Some(index) = pending_plugin_remove_index {
                    if self.remove_plugin_search_path_at(index) {
                        self.save_prefs();
                        self.plugin_catalog.clear();
                        self.spawn_plugin_scan();
                    }
                }
                if let Some(path) = pending_plugin_add_path {
                    if self.add_plugin_search_path(path) {
                        self.save_prefs();
                        self.plugin_catalog.clear();
                        self.spawn_plugin_scan();
                    }
                }
                if let Some(plugin_key) = pending_plugin_probe {
                    self.spawn_plugin_probe_for_tab(tab_idx, plugin_key);
                }
                if pending_plugin_gui_open {
                    self.open_plugin_gui_for_tab(tab_idx);
                }
                if pending_plugin_gui_sync {
                    self.sync_plugin_gui_for_tab(tab_idx);
                }
                if pending_plugin_gui_close {
                    self.close_plugin_gui_for_tab(tab_idx);
                }
                if pending_plugin_preview {
                    self.spawn_plugin_preview_for_tab(tab_idx);
                }
                if pending_plugin_apply {
                    self.spawn_plugin_apply_for_tab(tab_idx);
                }
                if pending_music_model_download {
                    self.queue_music_model_download();
                }
                if pending_music_model_uninstall {
                    self.uninstall_music_model_cache();
                }
                if pending_music_analyze_start {
                    self.start_music_analysis_for_tab(tab_idx);
                }
                if pending_music_analyze_cancel {
                    self.cancel_music_analysis_run();
                }
                if pending_music_preview_cancel {
                    self.cancel_music_preview_run();
                }
                if pending_music_rebuild_markers {
                    self.rebuild_music_provisional_markers_for_tab(tab_idx);
                }
                if pending_music_preview_refresh {
                    self.apply_music_preview_mix_for_tab(tab_idx);
                }
                if pending_music_apply_markers {
                    self.apply_music_analysis_markers_to_tab(tab_idx);
                }
                if pending_music_apply_preview {
                    self.apply_music_preview_to_tab(tab_idx);
                }
                if stop_playback { self.audio.stop(); }
                if need_restore_preview { self.clear_preview_if_any(tab_idx); }
                if request_preview_refresh {
                    self.clear_heavy_preview_state();
                    self.clear_heavy_overlay_state();
                    self.refresh_tool_preview_for_tab(tab_idx);
                }
                if let Some(s) = request_seek {
                    self.audio.seek_to_sample(s);
                    let seek_display = if let Some(tab) = self.tabs.get(tab_idx) {
                        map_audio_to_display(tab, s)
                    } else {
                        0
                    };
                    if let Some(tab) = self.tabs.get_mut(tab_idx) {
                        let seek_geom = EditorDisplayGeometry::new(
                            0.0,
                            tab.last_wave_w.max(1.0),
                            tab.samples_per_px,
                            tab.view_offset,
                            tab.view_offset_exact,
                            tab_samples_len,
                        );
                        let clamped_seek =
                            seek_geom.clamp_display_sample(seek_display);
                        if !seek_geom.contains_sample_center(clamped_seek) {
                            let centered = clamped_seek.saturating_sub(seek_geom.visible_count / 2);
                            Self::editor_set_view_offset_exact(
                                tab,
                                centered as f64,
                                seek_geom.max_left(),
                            );
                        }
                    }
                }
                if let Some((tool_kind, mono)) = pending_preview { self.set_preview_mono(tab_idx, tool_kind, mono); }
            }); // end horizontal split
        self.plugin_search_path_input = plugin_search_path_input;
        if touch_spectro_cache {
            self.touch_spectro_cache(&spec_path);
        }
        if let Some(hint) = pending_viewport_hint {
            self.ensure_editor_viewport_for_tab(tab_idx, hint);
        }
        if waveform_query_ms_total > 0.0 {
            self.debug_push_waveform_query_sample(waveform_query_ms_total);
        }
        if waveform_draw_ms_total > 0.0 {
            self.debug_push_waveform_draw_sample(waveform_draw_ms_total);
            self.debug_push_waveform_render_sample(
                waveform_query_ms_total + waveform_draw_ms_total,
            );
        }
        self.waveform_scratch = waveform_scratch;
        if let Some(ms) = perf_mixdown_ms {
            self.debug_push_editor_mixdown_build_sample(ms);
        }
        if let Some(ms) = perf_wave_render_ms {
            self.debug_push_editor_wave_render_sample(ms);
        }

        let painted_path = self.tabs[tab_idx].path.clone();
        let painted_samples_len = self.tabs[tab_idx].samples_len;
        self.debug_mark_editor_open_shell_paint(&painted_path);
        if painted_samples_len > 0 {
            self.debug_mark_editor_open_first_paint(&painted_path, painted_samples_len);
        }

        if cancel_apply {
            self.cancel_editor_apply();
            self.cancel_plugin_process();
        }
        if cancel_decode {
            self.cancel_editor_decode();
        }
        if cancel_processing {
            self.cancel_processing();
        }
        if cancel_preview {
            self.cancel_heavy_preview();
        }
        if cancel_spectro {
            self.cancel_spectrogram_for_path(&tab_path);
        }
        if cancel_feature_analysis {
            if let Some(kind) = feature_kind {
                self.cancel_feature_analysis_for_key(&EditorAnalysisKey {
                    path: tab_path.clone(),
                    kind,
                });
            }
        }
        if pending_tempogram_refresh {
            let key = EditorAnalysisKey {
                path: tab_path.clone(),
                kind: EditorAnalysisKind::Tempogram,
            };
            self.cancel_feature_analysis_for_key(&key);
        }
        if pending_chromagram_refresh {
            let key = EditorAnalysisKey {
                path: tab_path.clone(),
                kind: EditorAnalysisKind::Chromagram,
            };
            self.cancel_feature_analysis_for_key(&key);
        }
        if let Some(bpm) = apply_estimated_bpm {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.bpm_value = bpm;
                tab.bpm_user_set = true;
            }
        }

        // perform pending actions after borrows end
        // Defer starting heavy overlay until after UI to avoid nested &mut self borrow (E0499)
        if let Some((tool, p)) = pending_overlay_job {
            if !self.is_decode_failed_path(&self.tabs[tab_idx].path) {
                self.spawn_heavy_overlay_for_tab(tab_idx, tool, p);
            }
        }
        if let Some((tool, path, p)) = pending_overlay_path {
            if !self.is_decode_failed_path(&path) {
                self.spawn_heavy_overlay_from_path(path, tool, p);
            }
        }
        if request_undo {
            self.clear_preview_if_any(tab_idx);
            self.editor_apply_state = None;
            self.undo_in_tab(tab_idx);
        }
        if request_redo {
            self.clear_preview_if_any(tab_idx);
            self.editor_apply_state = None;
            self.redo_in_tab(tab_idx);
        }
        if let Some((s, e)) = do_set_loop_from {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                if s == 0 && e == 0 {
                    tab.loop_region = None;
                } else {
                    tab.loop_region = Some((s, e));
                    apply_pending_loop = true;
                }
                Self::update_loop_markers_dirty(tab);
            }
        }
        if let Some(state) = pending_edit_undo.take() {
            self.push_editor_undo_state(tab_idx, state, true);
        }
        if let Some((s, e)) = do_trim {
            self.editor_apply_trim_range(tab_idx, (s, e));
        }
        if let Some((s, e)) = do_trim_virtual {
            self.add_trim_range_as_virtual(tab_idx, (s, e));
        }
        if let Some((s, e)) = do_mute {
            self.editor_apply_mute_range(tab_idx, (s, e));
        }
        if let Some(((s, e), in_ms, out_ms)) = do_fade {
            self.editor_apply_fade_range(tab_idx, (s, e), in_ms, out_ms);
        }
        if let Some(((s, e), shp)) = do_fade_in {
            self.editor_apply_fade_in_explicit(tab_idx, (s, e), shp);
        }
        if let Some(((mut s, mut e), shp)) = do_fade_out {
            // If range provided is (0, n) as length, anchor to end
            if let Some(tab) = self.tabs.get(tab_idx) {
                let len = tab.samples_len;
                if s == 0 {
                    s = len.saturating_sub(e);
                    e = len;
                }
            }
            self.editor_apply_fade_out_explicit(tab_idx, (s, e), shp);
        }
        if let Some(((s, e), gdb)) = do_gain {
            self.editor_apply_gain_range(tab_idx, (s, e), gdb);
        }
        if let Some(((s, e), tdb)) = do_normalize {
            self.editor_apply_normalize_range(tab_idx, (s, e), tdb);
        }
        if let Some((s, e)) = do_reverse {
            self.editor_apply_reverse_range(tab_idx, (s, e));
        }
        if let Some((_, _)) = do_cutjoin {
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.trim_range = None;
            }
        }
        if let Some((s, e)) = do_cutjoin {
            self.editor_delete_range_and_join(tab_idx, (s, e));
        }
        if do_commit_loop {
            let mut apply_xfade = false;
            let mut do_unwrap: Option<u32> = None;
            let mut undo_state = None;
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                if let Some(repeat) = tab.pending_loop_unwrap {
                    do_unwrap = Some(repeat);
                } else {
                    let will_change = tab.loop_region_committed != tab.loop_region
                        || tab.loop_region_applied != tab.loop_region
                        || tab.loop_xfade_samples > 0;
                    if will_change {
                        undo_state = Some(Self::capture_undo_state(tab));
                    }
                    tab.loop_region_committed = tab.loop_region;
                    tab.loop_region_applied = tab.loop_region_committed;
                    apply_xfade = tab.loop_xfade_samples > 0;
                }
                tab.pending_loop_unwrap = None;
                tab.preview_audio_tool = None;
                tab.preview_overlay = None;
            }
            if let Some(state) = undo_state {
                self.push_editor_undo_state(tab_idx, state, true);
            }
            if let Some(repeat) = do_unwrap {
                self.editor_apply_loop_unwrap(tab_idx, repeat);
            } else {
                if apply_xfade {
                    self.editor_apply_loop_xfade(tab_idx);
                }
                if let Some(tab) = self.tabs.get_mut(tab_idx) {
                    Self::update_loop_markers_dirty(tab);
                }
            }
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                tab.loop_xfade_samples = 0;
                tab.tool_state = ToolState {
                    loop_repeat: 2,
                    ..tab.tool_state
                };
            }
        }
        if let Some(repeat) = do_preview_unwrap {
            let preview_ok = self
                .tabs
                .get(tab_idx)
                .map(|t| t.samples_len <= LIVE_PREVIEW_SAMPLE_LIMIT)
                .unwrap_or(false);
            if preview_ok {
                if let Some(tab) = self.tabs.get(tab_idx) {
                    if let Some(chans) = self.editor_preview_loop_unwrap(tab, repeat) {
                        let timeline_len = chans.get(0).map(|c| c.len()).unwrap_or(0);
                        let mono = Self::mixdown_channels(&chans, timeline_len);
                        let markers = Self::build_loop_unwrap_markers(
                            &tab.markers,
                            tab.loop_region.map(|v| v.0).unwrap_or(0),
                            tab.loop_region.map(|v| v.1).unwrap_or(0),
                            tab.samples_len,
                            repeat as usize,
                        );
                        if let Some(tab_mut) = self.tabs.get_mut(tab_idx) {
                            tab_mut.markers = markers;
                            Self::update_markers_dirty(tab_mut);
                            tab_mut.preview_overlay = Some(Self::preview_overlay_from_channels(
                                chans,
                                ToolKind::LoopEdit,
                                timeline_len,
                            ));
                        }
                        if !mono.is_empty() {
                            self.set_preview_mono(tab_idx, ToolKind::LoopEdit, mono);
                        }
                    }
                }
            }
        }
        if do_commit_markers {
            let mut undo_state = None;
            if let Some(tab) = self.tabs.get_mut(tab_idx) {
                if tab.markers != tab.markers_committed {
                    undo_state = Some(Self::capture_undo_state(tab));
                }
                tab.markers_committed = tab.markers.clone();
                tab.markers_applied = tab.markers_committed.clone();
                Self::update_markers_dirty(tab);
            }
            if let Some(state) = undo_state {
                self.push_editor_undo_state(tab_idx, state, true);
            }
        }
        if apply_pending_loop {
            if let Some(tab_ro) = self.tabs.get(tab_idx) {
                self.apply_loop_mode_for_tab(tab_ro);
            }
        }
        self.ui_editor_zoo_overlay(ctx, Some(tab_idx), editor_panel_rect);
    }
}
